//! The launch controller's stream lifecycle store.
//!
//! NVHTTP owns every stream launch, so it keeps the store. Its own failure
//! paths queue issues on the launch record; other layers publish theirs on the
//! SDK topic. Both reach one [`StreamLifecycleStore`] per controller, which
//! management reads with the launch status.
use super::{
    Controller,
    record::{Record, Stage},
    workflow::Phase,
};
use crate::wire::{Reply, StreamQueryError};
use amaterasu_module_sdk::stream_lifecycle::{
    LifecycleLayer, StreamIssue, StreamIssueFeed, StreamLifecycleSnapshot, StreamReport,
};
use std::fmt::Debug;

/// The identity NVHTTP's own commits carry, matching its module principal.
pub(super) const NVHTTP_PUBLISHER: &str = "amaterasu.nvhttp";
/// Reported issues read per poll; the rest wait for the next tick.
const FEED_READS_PER_POLL: usize = 32;
const PROVIDER_CODE_LIMIT: usize = 64;
const RECENT_REFUSALS_LIMIT: usize = 16;

/// The job a failure recorded now belongs to: its phase, and the code the
/// provider replied with when it gave one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FailureContext {
    pub phase: Phase,
    pub provider_code: Option<String>,
}

impl FailureContext {
    pub(super) fn new(phase: Phase, payload: Option<&serde_json::Value>) -> Self {
        let provider_code = payload
            .and_then(|payload| payload.get("code"))
            .and_then(serde_json::Value::as_str)
            .filter(|code| plain_code(code))
            .map(str::to_owned);
        Self {
            phase,
            provider_code,
        }
    }
}

/// The layer a job phase depends on.
pub(super) fn phase_layer(phase: Phase) -> LifecycleLayer {
    use Phase::*;
    match phase {
        BindPresentationApplication | BindPresentationTransport => LifecycleLayer::Presentation,
        ReadRtspRuntimeEpoch | OpenRtsp | LookupRtsp | MonitorRtsp | StopRtsp
        | VerifyRtspStopped => LifecycleLayer::Rtsp,
        VerifyBorrowedCapture => LifecycleLayer::Capture,
        PrepareLifecycle
        | LookupLifecycle
        | BindApplication
        | LookupApplicationBinding
        | EndLifecycle
        | ReportApplicationRetired
        | LookupApplicationRetirement
        | AwaitLifecycleEnd
        | VerifyPaused
        | VerifyResumeSession => LifecycleLayer::SessionLifecycle,
        ReadLaunchEpoch
        | VerifyResumeApplication
        | PreflightApplication
        | LaunchApplication
        | LookupApplication
        | AwaitApplication
        | MonitorApplication
        | StopApplication
        | VerifyApplicationStopped => LifecycleLayer::Application,
        RefreshResumePolicy | ReadProtonPolicy => LifecycleLayer::Input,
        ReserveHistory | ReadLaunchWarnings | RecordHistoryWarnings | ReleaseHistory => {
            LifecycleLayer::History
        }
        SnapshotDisplay | EnumerateDisplay | ConfirmDisplay | PrepareDisplayAdmission => {
            LifecycleLayer::Display
        }
    }
}

/// The layer a record's stage waits on, for failures recorded outside a job.
pub(super) fn stage_layer(stage: Stage) -> LifecycleLayer {
    use Stage::*;
    match stage {
        ReadRtspRuntimeEpoch | OpenRtsp | VerifyRtspCleanup => LifecycleLayer::Rtsp,
        BindPresentationTransport => LifecycleLayer::Presentation,
        VerifyBorrowedCapture => LifecycleLayer::Capture,
        ReserveHistory | ReleasePausedHistory | ReleaseRejectedResumeHistory | ReleaseHistory => {
            LifecycleLayer::History
        }
        ReadProtonPolicy | RefreshResumePolicy => LifecycleLayer::Input,
        PreflightApplication
        | LaunchApplication
        | AwaitApplicationReady
        | VerifyResumeApplication
        | VerifyApplicationCleanup => LifecycleLayer::Application,
        SnapshotDisplay | EnumerateDisplay | ConfirmDisplay | PrepareDisplayAdmission => {
            LifecycleLayer::Display
        }
        AwaitingDelivery => LifecycleLayer::StreamSession,
        VerifyPaused | VerifyResumeSession => LifecycleLayer::SessionLifecycle,
        Live | Cleanup | Terminal => LifecycleLayer::Nvhttp,
    }
}

/// `OpenRtsp` becomes `open_rtsp`.
pub(super) fn label(value: impl Debug) -> String {
    let name = format!("{value:?}");
    let mut label = String::with_capacity(name.len() + 4);
    for (index, character) in name.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                label.push('_');
            }
            label.push(character.to_ascii_lowercase());
        } else if character.is_ascii_alphanumeric() {
            label.push(character);
        }
    }
    label
}

pub(super) fn plain_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= PROVIDER_CODE_LIMIT
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

impl Record {
    /// Record a failure that ends this launch. The first one recorded for a
    /// stream stays its root cause.
    #[allow(dead_code)]
    #[track_caller]
    pub(super) fn record_failure(&mut self, code: &str) {
        let issue = self.attributed(StreamIssue::critical(
            self.rtsp_operation.clone(),
            LifecycleLayer::Nvhttp,
            "unattributed",
            code,
        ));
        self.issues.push(issue);
    }

    /// Record a failure with an explicit reason, such as the code a provider
    /// reported in a status rather than as its reply's `code`.
    #[track_caller]
    pub(super) fn record_failure_because(&mut self, code: &str, reason: &str) {
        let mut issue = self.attributed(StreamIssue::critical(
            self.rtsp_operation.clone(),
            LifecycleLayer::Nvhttp,
            "unattributed",
            code,
        ));
        if plain_code(reason) {
            reason.clone_into(&mut issue.reason);
        }
        self.issues.push(issue);
    }

    /// Record a failure this launch survived by using `fallback`.
    #[track_caller]
    pub(super) fn record_demotion(&mut self, code: &str, reason: &str, fallback: &str) {
        let issue = self.attributed(StreamIssue::demoted(
            self.rtsp_operation.clone(),
            LifecycleLayer::Nvhttp,
            "unattributed",
            code,
            fallback,
        ));
        let issue = if reason.is_empty() {
            issue
        } else {
            issue.because(reason)
        };
        self.issues.push(issue);
    }

    /// Record a stream warning a provider or this module raised. The stream
    /// continued, so it is an advisory issue.
    #[track_caller]
    pub(super) fn record_warning(
        &mut self,
        warning: &amaterasu_module_sdk::stream_warnings::StreamWarning,
    ) {
        let mut issue = self.attributed(StreamIssue::advisory(
            self.rtsp_operation.clone(),
            LifecycleLayer::Nvhttp,
            "unattributed",
            warning.code.clone(),
        ));
        issue.reason.clone_from(&warning.reason);
        let issue = issue.with_values(warning.requested.clone(), warning.effective.clone());
        self.issues.push(issue);
    }

    /// NVHTTP reports only under its own layer. When the failure is its
    /// reaction to the job it was handling, or to the stage it was waiting
    /// in, it names that dependency in `caused_by`; the dependency reports
    /// its own failure itself.
    pub(super) fn attributed(&self, mut issue: StreamIssue) -> StreamIssue {
        issue.layer = LifecycleLayer::Nvhttp;
        let (phase, dependency, reason) = self.failure_attribution(&issue.code);
        issue.phase = phase;
        if let Some(reason) = reason {
            issue.reason = reason.into();
        }
        if dependency != LifecycleLayer::Nvhttp {
            issue = issue.caused_by(dependency);
        }
        issue
    }

    pub(super) fn failure_attribution(&self, code: &str) -> (String, LifecycleLayer, Option<&str>) {
        match &self.failure_context {
            Some(context) => (
                label(context.phase),
                phase_layer(context.phase),
                context
                    .provider_code
                    .as_deref()
                    .filter(|reason| *reason != code),
            ),
            None => (label(self.stage), stage_layer(self.stage), None),
        }
    }
}

impl Controller {
    #[cfg(test)]
    pub(crate) fn refusal_snapshot(&self, key: &str) -> Option<StreamLifecycleSnapshot> {
        self.stream_lifecycle.snapshot(key)
    }

    /// Read issues other layers publish. Off by default so a controller in a
    /// test speaks only the commands that test scripts.
    pub fn enable_stream_issue_feed(&mut self) {
        self.stream_issue_feed.get_or_insert_with(|| {
            let mut feed = StreamIssueFeed::new();
            for coordinator in [
                "amaterasu.rtsp",
                "amaterasu.stream-session",
                "amaterasu.session-lifecycle",
                "amaterasu.video",
            ] {
                feed.allow_bindings_from(coordinator);
            }
            feed
        });
    }

    pub(super) fn lifecycle_snapshot(&self, record: &Record) -> Option<StreamLifecycleSnapshot> {
        self.stream_lifecycle.snapshot(&record.rtsp_operation)
    }

    /// Commit what each launch recorded since the last poll, then what other
    /// layers reported. Records go first so a report can find its stream.
    pub(super) fn commit_stream_lifecycle(&mut self, send: &mut super::Send<'_>) {
        for record in self.records.values_mut() {
            let key = record.rtsp_operation.as_str();
            self.stream_lifecycle.open(key);
            self.stream_lifecycle
                .alias(key, &record.application_operation);
            // The same display identity can legitimately outlive a stream.
            // Its launch-owner snapshot authorizes a new stream's association.
            if !record.state.terminal()
                && let Some(intent) = &record.virtual_display_intent
                && intent.reference_id == key
            {
                self.stream_lifecycle.bind(key, &intent.identity);
            }
            self.stream_lifecycle.enter_phase(key, &label(record.stage));
            for report in record.issues.drain() {
                match report {
                    StreamReport::Issue(issue) => {
                        self.stream_lifecycle.commit_logged(
                            self.logger.as_ref(),
                            NVHTTP_PUBLISHER,
                            issue,
                        );
                    }
                    StreamReport::Binding(binding) => {
                        self.stream_lifecycle.bind(&binding.stream, &binding.alias);
                    }
                }
            }
            if record.state.terminal() {
                self.stream_lifecycle.end(key);
            }
        }
        if let Some(feed) = self.stream_issue_feed.as_mut() {
            feed.poll_with(
                &mut |command| send(command),
                &mut self.stream_lifecycle,
                self.logger.as_ref(),
                FEED_READS_PER_POLL,
            );
        }
    }

    /// A refusal sent before any launch record existed is a stream of its
    /// own in the store: one entry naming the line that refused.
    pub(crate) fn record_refusal(&mut self, reply: &mut Reply, phase: &str) {
        self.record_refusal_with_status_kind(reply, phase, "http", None);
    }

    pub(crate) fn refuse_stream_query(&mut self, error: StreamQueryError) -> Reply {
        let mut reply = super::private_error_with_input(400, error.message, Some(&error));
        self.record_refusal_with_status_kind(&mut reply, "request_parsing", "http", Some(&error));
        reply
    }

    fn record_refusal_with_status_kind(
        &mut self,
        reply: &mut Reply,
        phase: &str,
        status_kind: &str,
        input: Option<&StreamQueryError>,
    ) {
        let Some(rejection) = reply.rejection.take() else {
            return;
        };
        self.sequence = self.sequence.saturating_add(1);
        let key = format!("refused.{}", self.sequence);
        let tracked = self.stream_lifecycle.open_transient(&key);
        let layer = if phase.starts_with("presentation_") {
            LifecycleLayer::Presentation
        } else {
            LifecycleLayer::Nvhttp
        };
        let reason = if status_kind == "transport" {
            rejection.code.clone()
        } else {
            format!("{status_kind}_{}", rejection.status)
        };
        let mut issue =
            StreamIssue::critical(key.clone(), layer, phase, rejection.code).because(reason);
        let site = if let Some(input) = input {
            if let Some(field) = input.field {
                issue.reason = format!("{}.{field}", issue.reason);
            }
            issue.requested = input
                .requested
                .map(amaterasu_module_sdk::stream_warnings::WarningValue::Integer);
            input.site
        } else {
            rejection.site
        };
        issue.site = format!("{}:{}", site.file(), site.line());
        self.stream_lifecycle
            .commit_logged(self.logger.as_ref(), NVHTTP_PUBLISHER, issue);
        self.stream_lifecycle.end(&key);
        if tracked {
            self.recent_refusals.push_back(key);
            if self.recent_refusals.len() > RECENT_REFUSALS_LIMIT {
                self.recent_refusals.pop_front();
            }
        }
    }

    /// Record a failed request while preserving its original protocol reply.
    #[track_caller]
    pub(crate) fn record_request_failure(
        &mut self,
        reply: &mut Reply,
        status: u16,
        code: &str,
        phase: &str,
    ) {
        reply.rejection = Some(crate::wire::Rejection {
            status,
            code: super::rejection_code(code),
            site: std::panic::Location::caller(),
        });
        self.record_refusal(reply, phase);
    }

    #[track_caller]
    pub(crate) fn record_semantic_request_failure(
        &mut self,
        reply: &mut Reply,
        status: u16,
        code: &str,
        phase: &str,
    ) {
        reply.rejection = Some(crate::wire::Rejection {
            status,
            code: super::rejection_code(code),
            site: std::panic::Location::caller(),
        });
        self.record_refusal_with_status_kind(reply, phase, "protocol", None);
    }

    #[track_caller]
    pub(crate) fn record_transport_failure(&mut self, code: &str) {
        let mut reply = crate::wire::xml::error(400, "request transport failed");
        reply.rejection = Some(crate::wire::Rejection {
            status: 0,
            code: super::rejection_code(code),
            site: std::panic::Location::caller(),
        });
        self.record_refusal_with_status_kind(&mut reply, "request_transport", "transport", None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_snake_case_identifiers() {
        assert_eq!(label(Phase::OpenRtsp), "open_rtsp");
        assert_eq!(
            label(Stage::ReleaseRejectedResumeHistory),
            "release_rejected_resume_history"
        );
    }

    #[test]
    fn provider_codes_are_kept_only_when_plain() {
        let payload = serde_json::json!({"code":"encoder_unavailable"});
        assert_eq!(
            FailureContext::new(Phase::OpenRtsp, Some(&payload))
                .provider_code
                .as_deref(),
            Some("encoder_unavailable")
        );
        let payload = serde_json::json!({"code":"has spaces / and more"});
        assert_eq!(
            FailureContext::new(Phase::OpenRtsp, Some(&payload)).provider_code,
            None
        );
    }
}
