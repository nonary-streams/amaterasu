//! Stream failures RTSP reports to the launch owner's lifecycle store.
//!
//! The launch owner opens a ticket with its stream key as `operation_id`.
//! RTSP reports its own refusals and failures under that key, and binds its
//! ticket operation (`rtsp-<ticket>`), from which every stream-session and
//! lifecycle call it makes is derived, to the same stream. Record methods
//! queue reports on the record; the tick publishes them on the SDK's
//! stream-report topic, where the runtime stamps this module as the reporter.
use super::{Controller, Phase, Record, State, error};
use crate::rpc;
use amaterasu_module_sdk::stream_lifecycle::{
    LifecycleLayer, StreamBinding, StreamIssue, StreamReport,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};

const OPEN_PHASE: &str = "open";
const CLEANUP_PHASE: &str = "cleanup";

/// Close reasons that are the caller's own decision rather than a failure
/// RTSP observed. The caller records those itself.
const CALLER_DECISIONS: [&str; 1] = ["request_cancelled"];

/// The dependency whose failure made RTSP end or reject a stream. The
/// dependency reports its own failure; RTSP's report is its reaction.
#[derive(Clone, Copy, Debug)]
pub(super) struct Cause<'a> {
    pub layer: LifecycleLayer,
    /// The dependency's own code, when it gave one.
    pub code: Option<&'a str>,
}

impl<'a> Cause<'a> {
    pub(super) fn stream_session(code: Option<&'a str>) -> Self {
        Self {
            layer: LifecycleLayer::StreamSession,
            code,
        }
    }

    pub(super) fn lifecycle(code: Option<&'a str>) -> Self {
        Self {
            layer: LifecycleLayer::SessionLifecycle,
            code,
        }
    }
}

impl Cause<'static> {
    /// The dependency a job phase was calling.
    pub(super) fn of_phase(phase: Phase) -> Self {
        match phase {
            Phase::LifecyclePrepare | Phase::LifecycleLookup => Self::lifecycle(None),
            Phase::Prepare
            | Phase::Find
            | Phase::Arm
            | Phase::Stop
            | Phase::Observe
            | Phase::Status => Self::stream_session(None),
        }
    }

    /// The dependency a close reason names by itself. Reasons that depend on
    /// the call that failed are given their cause where they are raised.
    pub(super) fn of_close(reason: &str) -> Option<Self> {
        match reason {
            "dependency_unavailable"
            | "stream_failed"
            | "stream_status_unavailable"
            | "stream_observation_lost"
            | "invalid_prepare_response"
            | "display_lease_unverified"
            | "history_activity_unverified"
            | "lifecycle_identity_unverified"
            | "borrowed_capture_unverified"
            | "source_capture_context_unverified"
            | "lifecycle_mode_unverified"
            | "lifecycle_input_policy_unverified" => Some(Self::stream_session(None)),
            "lifecycle_prepare_rejected" => Some(Self::lifecycle(None)),
            _ => None,
        }
    }

    /// The dependency whose answer left cleanup unconfirmed. Broker
    /// settlement and RTSP's own deadlines have none.
    pub(super) fn of_cleanup(code: &str) -> Option<Self> {
        match code {
            "dependency_unavailable"
            | "invalid_lookup_response"
            | "invalid_stop_response"
            | "invalid_status_response"
            | "stream_cleanup_failed"
            | "shutdown_lookup_invalid"
            | "shutdown_lookup_not_found"
            | "shutdown_stop_rejected"
            | "shutdown_request_unavailable" => Some(Self::stream_session(None)),
            "lifecycle_prepare_lookup_unavailable"
            | "lifecycle_prepare_unconfirmed"
            | "lifecycle_retirement_unconfirmed"
            | "shutdown_lifecycle_unconfirmed" => Some(Self::lifecycle(None)),
            _ => None,
        }
    }
}

impl State {
    /// What the ticket was doing when it failed.
    fn phase(self) -> &'static str {
        match self {
            Self::Preparing => "prepare",
            Self::Ready => "await_play",
            Self::Arming => "arm",
            Self::Playing => "play",
            Self::Closing | Self::Closed | Self::Failed | Self::CleanupFailed => CLEANUP_PHASE,
        }
    }
}

impl Record {
    /// Bind this ticket's own operation to the launch owner's stream so
    /// providers RTSP calls under `rtsp-<ticket>-…` report to that stream.
    pub(super) fn bind_stream_operations(&mut self) {
        self.issues.push_binding(StreamBinding::new(
            self.config.operation_id.clone(),
            self.operation(),
        ));
    }

    /// Record the failure that ended this ticket, while it was in its
    /// current state.
    #[track_caller]
    pub(super) fn report_failure(&mut self, code: &'static str, cause: Option<Cause<'_>>) {
        if CALLER_DECISIONS.contains(&code) {
            return;
        }
        let issue = StreamIssue::critical(
            self.config.operation_id.clone(),
            LifecycleLayer::Rtsp,
            self.state.phase(),
            code,
        );
        self.issues.push(with_cause(issue, cause));
    }

    /// Mark cleanup of this ticket unconfirmed. A repeat of the same code
    /// for a ticket already in that state is not reported again.
    #[track_caller]
    pub(super) fn fail_cleanup(&mut self, code: &'static str) {
        let repeated = self.state == State::CleanupFailed && self.cleanup_error == Some(code);
        self.state = State::CleanupFailed;
        self.cleanup_error = Some(code);
        if !repeated {
            let issue = StreamIssue::critical(
                self.config.operation_id.clone(),
                LifecycleLayer::Rtsp,
                CLEANUP_PHASE,
                code,
            );
            self.issues.push(with_cause(issue, Cause::of_cleanup(code)));
        }
    }
}

fn with_cause(issue: StreamIssue, cause: Option<Cause<'_>>) -> StreamIssue {
    let Some(cause) = cause else {
        return issue;
    };
    let issue = issue.caused_by(cause.layer);
    match cause.code {
        Some(code) => issue.because(code),
        None => issue,
    }
}

impl Controller {
    #[track_caller]
    pub(super) fn report_open_warning(&mut self, caller: &str, operation: &str, code: &str) {
        if caller == self.settings.launch_owner {
            self.stream_issues.push(
                StreamIssue::advisory(operation, LifecycleLayer::Rtsp, OPEN_PHASE, code)
                    .because(code),
            );
        }
    }

    /// Report on the launch owner's topic. Off by default so a controller in
    /// a test sends only the commands that test scripts.
    pub fn enable_stream_issue_reports(&mut self) {
        self.report_stream_issues = true;
    }

    /// Refuse an open before a ticket exists. Only the launch owner names
    /// streams in the lifecycle store, so other callers' refusals are
    /// answered but not reported.
    #[track_caller]
    pub(super) fn refuse_open(
        &mut self,
        runtime: &RuntimeClient,
        request: u64,
        caller: &str,
        operation: &str,
        code: &str,
    ) {
        rpc::complete(runtime, request, error(code));
        if caller == self.settings.launch_owner {
            self.stream_issues.push(StreamIssue::critical(
                operation,
                LifecycleLayer::Rtsp,
                OPEN_PHASE,
                code,
            ));
        }
    }

    /// Refuse an open because the launch module's display admission did not
    /// verify. The launch module reports its own decision; this is RTSP's
    /// reaction to it.
    #[track_caller]
    pub(super) fn refuse_display_admission(
        &mut self,
        runtime: &RuntimeClient,
        admission: &super::DisplayAdmission,
        code: &str,
    ) {
        rpc::complete(runtime, admission.request, error(code));
        if admission.caller == self.settings.launch_owner {
            self.stream_issues.push(
                StreamIssue::critical(
                    admission.config.operation_id.as_str(),
                    LifecycleLayer::Rtsp,
                    "display_admission",
                    code,
                )
                .caused_by(LifecycleLayer::Application),
            );
        }
    }

    /// Keep what a ticket recorded when its record is dropped for capacity.
    pub(super) fn keep_dropped_record_issues(&mut self, record: &mut Record) {
        if record.owner != self.settings.launch_owner {
            return;
        }
        for report in record.issues.drain() {
            match report {
                StreamReport::Issue(issue) => self.stream_issues.push(issue),
                StreamReport::Binding(binding) => self.stream_issues.push_binding(binding),
            }
        }
    }

    /// Publish what the controller and each launch-owned ticket recorded. A
    /// refused publication keeps the rest for the next tick; every outbox is
    /// bounded either way.
    pub(super) fn publish_stream_issues(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) {
        let launch_owner = &self.settings.launch_owner;
        for record in self.records.values_mut() {
            if &record.owner != launch_owner {
                record.issues.drain().for_each(drop);
            }
        }
        let outboxes = std::iter::once(&mut self.stream_issues)
            .chain(self.records.values_mut().map(|record| &mut record.issues));
        for outbox in outboxes {
            if !self.report_stream_issues {
                outbox.drain().for_each(drop);
            } else if !outbox.is_empty()
                && let Err(error) = outbox.flush_with(send)
            {
                eprintln!("amaterasu rtsp stream issue report failed error={error}");
            }
        }
    }
}
