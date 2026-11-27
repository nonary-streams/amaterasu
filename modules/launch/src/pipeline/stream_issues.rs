//! Stream failures and warnings this module reports to the launch owner's
//! lifecycle store.
//!
//! A launch reports under the operation ID its owner launched it with; NVHTTP
//! aliases that application operation to its stream. Failure funnels queue
//! issues on the launch record, or on the pipeline for a refusal that leaves
//! no record. The tick publishes them on the SDK stream report topic, where
//! the runtime stamps this module as the reporter.
use super::{Pipeline, Transport, complete};
use crate::{protocol::State, protocol::error, records::Record, workflow::Step};
use amaterasu_module_sdk::{
    Command, ModuleResult,
    stream_lifecycle::{LifecycleLayer, StreamIssue},
    stream_warnings::StreamWarning,
};
use serde_json::Value;

const MAX_REASON: usize = 64;

/// `AwaitApplication` becomes `await_application`.
pub(crate) fn step_phase(step: Step) -> String {
    let name = format!("{step:?}");
    let mut phase = String::with_capacity(name.len() + 4);
    for (index, character) in name.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                phase.push('_');
            }
            phase.push(character.to_ascii_lowercase());
        } else {
            phase.push(character);
        }
    }
    phase
}

/// The kind of failure an application provider gave for a failed launch,
/// when it is a short identifier the store accepts as a reason.
pub(crate) fn application_failure_kind(process: &Value) -> Option<&str> {
    process["preparation"]["failure"]["kind"]
        .as_str()
        .or_else(|| process["failure"]["kind"].as_str())
        .filter(|kind| reason_identifier(kind))
}

fn reason_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REASON
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

impl Record {
    /// A failure that ends this launch, seen while the workflow was at `step`.
    #[track_caller]
    pub(crate) fn report_launch_failure(&mut self, step: Step, code: &str, reason: Option<&str>) {
        let mut issue = StreamIssue::critical(
            self.operation_id.clone(),
            LifecycleLayer::Application,
            step_phase(step),
            code,
        );
        if let Some(reason) = reason {
            issue = issue.because(reason);
        }
        self.issues.push(issue);
    }

    /// The application ended on its own. Before it was running that is a
    /// failed launch; once streaming, it is how a stream normally ends (the
    /// user quit the game), so it is recorded without ranking as a failure.
    /// The process provider's exit code, when it gave one, is the reason.
    #[track_caller]
    pub(crate) fn report_application_exit(&mut self, step: Step, process: Option<&Value>) {
        let operation = self.operation_id.clone();
        let phase = step_phase(step);
        let mut issue = if self.state == State::Starting {
            StreamIssue::critical(
                operation,
                LifecycleLayer::Application,
                phase,
                "application_exited_during_launch",
            )
        } else {
            StreamIssue::advisory(
                operation,
                LifecycleLayer::Application,
                phase,
                "application_exited",
            )
        };
        if let Some(exit_code) = process.and_then(|process| process["exit_code"].as_i64()) {
            issue = issue.because(format!("exit_code_{exit_code}"));
        }
        self.issues.push(issue);
    }

    /// Cleanup could not confirm that this launch released what it owned.
    #[track_caller]
    pub(crate) fn report_cleanup_failure(&mut self, step: Step, code: &str) {
        self.issues.push(StreamIssue::critical(
            self.operation_id.clone(),
            LifecycleLayer::Application,
            step_phase(step),
            code,
        ));
    }

    /// A cleanup step failed and cleanup continued at `fallback`.
    #[track_caller]
    pub(crate) fn report_cleanup_fallback(&mut self, step: Step, code: &str, fallback: Step) {
        self.issues.push(StreamIssue::demoted(
            self.operation_id.clone(),
            LifecycleLayer::Application,
            step_phase(step),
            code,
            step_phase(fallback),
        ));
    }

    /// A configured log path could not be resolved before a process was started.
    #[track_caller]
    pub(crate) fn discard_unresolved_output(&mut self, step: Step) {
        use amaterasu_module_sdk::stream_warnings::{MAX_WARNINGS, WarningValue};
        let issue = StreamIssue::demoted(
            self.operation_id.clone(),
            LifecycleLayer::Application,
            step_phase(step),
            "application_output_unavailable",
            "discard_output",
        )
        .because("output_path_resolution_failed")
        .with_values(
            Some(WarningValue::Label("configured_output".into())),
            Some(WarningValue::Label("discarded".into())),
        );
        let warning = issue.to_stream_warning(crate::SERVICE);
        if self.warnings.len() < MAX_WARNINGS && !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
        self.issues.push(issue);
    }

    /// A launch warning, reported when this launch first records it.
    #[track_caller]
    pub(crate) fn report_warning(&mut self, step: Step, warning: &StreamWarning) {
        let mut issue = StreamIssue::advisory(
            self.operation_id.clone(),
            LifecycleLayer::Application,
            step_phase(step),
            warning.code.clone(),
        )
        .because(warning.reason.clone())
        .with_values(warning.requested.clone(), warning.effective.clone());
        if let Some(os_error) = warning.os_error {
            issue = issue.with_os_error(os_error);
        }
        self.issues.push(issue);
    }

    /// The fallback a failed session stop continues with.
    pub(crate) fn session_stop_fallback(&self) -> Step {
        if self.process_id.is_some() {
            Step::Terminate
        } else {
            Step::Finish
        }
    }
}

impl Pipeline {
    /// Report on the stream report topic. Off by default so a pipeline in a
    /// test sends only the commands that test scripts.
    pub fn enable_stream_issue_reports(&mut self) {
        self.report_stream_issues = true;
    }

    /// Refuse a launch or preflight request and report the refusal under the
    /// operation the owner asked for.
    #[track_caller]
    pub(super) fn refuse_launch(
        &mut self,
        send: &mut impl FnMut(Command) -> Transport,
        request: u64,
        operation_id: &str,
        phase: &'static str,
        code: &'static str,
    ) -> ModuleResult<()> {
        self.refusals.push(StreamIssue::critical(
            operation_id,
            LifecycleLayer::Application,
            phase,
            code,
        ));
        complete(send, request, error(code))
    }

    /// Refuse a request the launch table did not admit. A conflicting replay
    /// of an admitted operation leaves that launch running, so only the
    /// other refusals are reported.
    #[track_caller]
    pub(super) fn refuse_admission(
        &mut self,
        send: &mut impl FnMut(Command) -> Transport,
        request: u64,
        operation_id: &str,
        phase: &'static str,
        code: &'static str,
    ) -> ModuleResult<()> {
        if code == "operation_conflict" {
            return complete(send, request, error(code));
        }
        self.refuse_launch(send, request, operation_id, phase, code)
    }

    /// Publish what the launches and refusals recorded. A refused
    /// publication keeps the rest for the next tick; each outbox is bounded.
    pub(super) fn publish_stream_issues(&mut self, send: &mut impl FnMut(Command) -> Transport) {
        let enabled = self.report_stream_issues;
        let outboxes = std::iter::once(&mut self.refusals).chain(
            self.records
                .values
                .values_mut()
                .map(|record| &mut record.issues),
        );
        for outbox in outboxes {
            if !enabled {
                outbox.drain().for_each(drop);
                continue;
            }
            if outbox.is_empty() {
                continue;
            }
            if let Err(error) = outbox.flush_with(&mut |command| send(command)) {
                eprintln!("amaterasu launch stream issue report failed error={error}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_are_snake_case_step_names() {
        assert_eq!(step_phase(Step::AwaitApplication), "await_application");
        assert_eq!(step_phase(Step::Spawn), "spawn");
    }
}
