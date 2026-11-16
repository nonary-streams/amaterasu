//! Virtual display failures this provider reports first-hand to the launch
//! owner's lifecycle store.
//!
//! Mode changes carry the caller's operation ID. Creation and removal carry no
//! operation ID, so they are reported under the monitor identity the caller
//! chose for its stream; the caller binds that identity to its stream.
//! Admission replies are reported from the tick that sends them; a worker's
//! later creation, removal or mode change failure is reported when the tick
//! receives its event.
use amaterasu_module_sdk::stream_lifecycle::{
    LifecycleLayer, StreamIssue, StreamIssueOutbox, StreamReport,
};
use amaterasu_module_sdk::virtual_display::Operation;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::Value;

/// Code used when a failed reply carries no identifier-shaped code.
const UNNAMED_FAILURE: &str = "virtual_display_failed";
/// What a stream keeps when a mode change is refused.
const MODE_KEPT: &str = "virtual_display_mode_kept";

/// The stream operation a request serves and what it was doing.
pub(crate) struct StreamOperation {
    phase: &'static str,
    operation: String,
}

impl StreamOperation {
    /// Requests that create, change or remove a stream's monitor. Status,
    /// verification and borrows are reads whose callers report the outcome.
    pub(crate) fn of(operation: &Operation) -> Option<Self> {
        let (phase, operation) = match operation {
            Operation::Create { identity, .. } => ("create", identity),
            Operation::Remove { identity, .. } => ("remove", identity),
            Operation::Reconfigure { operation_id, .. } => ("reconfigure", operation_id),
            Operation::ReconfigureModeOnly { operation_id, .. } => {
                ("reconfigure_mode_only", operation_id)
            }
            Operation::PrepareRetainedMode { operation_id, .. } => {
                ("prepare_retained_mode", operation_id)
            }
            Operation::CommitRetainedMode { operation_id, .. } => {
                ("commit_retained_mode", operation_id)
            }
            Operation::AbortRetainedMode { operation_id, .. } => {
                ("abort_retained_mode", operation_id)
            }
            _ => return None,
        };
        Some(Self {
            phase,
            operation: operation.clone(),
        })
    }
}

#[derive(Default)]
pub(crate) struct StreamIssues {
    enabled: bool,
    outbox: StreamIssueOutbox,
}

impl StreamIssues {
    pub(crate) fn enable(&mut self) {
        self.enabled = true;
    }

    /// Record the admission reply to a stream request if it was refused. A
    /// refused mode change leaves the monitor in its current mode; any other
    /// refusal leaves the stream without its monitor or its cleanup.
    pub(crate) fn admitted(&mut self, stream: Option<StreamOperation>, reply: &Value) {
        let Some(stream) = stream else {
            return;
        };
        // A request its caller ended is the caller's to report.
        if reply["ok"] == true || reply["code"] == "request_ended" {
            return;
        }
        let code = reply["code"]
            .as_str()
            .filter(|code| identifier(code))
            .unwrap_or(UNNAMED_FAILURE);
        let issue = match stream.phase {
            "reconfigure"
            | "reconfigure_mode_only"
            | "prepare_retained_mode"
            | "commit_retained_mode" => StreamIssue::demoted(
                stream.operation,
                LifecycleLayer::VirtualDisplay,
                stream.phase,
                code,
                MODE_KEPT,
            ),
            _ => StreamIssue::critical(
                stream.operation,
                LifecycleLayer::VirtualDisplay,
                stream.phase,
                code,
            ),
        };
        self.push(issue);
    }

    /// Take the issues a manager recorded from its worker's events.
    #[cfg_attr(not(amaterasu_libvirtualdisplay), allow(dead_code))]
    pub(crate) fn adopt(&mut self, reports: impl Iterator<Item = StreamReport>) {
        for report in reports {
            match report {
                StreamReport::Issue(issue) => self.push(issue),
                StreamReport::Binding(binding) => self.outbox.push_binding(binding),
            }
        }
    }

    fn push(&mut self, issue: StreamIssue) {
        eprintln!(
            "virtual display stream issue operation={} phase={} code={}",
            issue.operation, issue.phase, issue.code
        );
        self.outbox.push(issue);
    }

    /// Publish queued issues from the tick; discard them when not enabled.
    pub(crate) fn publish(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) {
        if !self.enabled {
            self.outbox.drain().for_each(drop);
            return;
        }
        if self.outbox.is_empty() {
            return;
        }
        if let Err(error) = self.outbox.flush_with(send) {
            eprintln!("virtual display stream issue report failed error={error}");
        }
    }
}

/// A worker failed to finish a monitor change after its admission. Native
/// error text is not an identifier, so it stays in the module log.
#[cfg_attr(not(amaterasu_libvirtualdisplay), allow(dead_code))]
#[track_caller]
pub(crate) fn worker_failure(operation: &str, phase: &str, code: &str) -> StreamIssue {
    StreamIssue::critical(operation, LifecycleLayer::VirtualDisplay, phase, code)
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::stream_lifecycle::IssueDisposition;
    use serde_json::json;

    fn create(identity: &str) -> Operation {
        serde_json::from_value(json!({"action":"create","identity":identity,"width":1920,
            "height":1080,"refresh_millihz":60000,"name":"Amaterasu","hdr_enabled":false}))
        .unwrap()
    }

    fn publish(issues: &mut StreamIssues) -> Vec<StreamIssue> {
        let mut published = Vec::new();
        issues.publish(&mut |command| {
            let Command::Publish { topic, payload } = command else {
                panic!("only stream reports are sent")
            };
            assert_eq!(topic, "amaterasu.stream.report/1");
            assert_eq!(payload["kind"], "issue");
            if let StreamReport::Issue(issue) = serde_json::from_value(payload).unwrap() {
                published.push(issue);
            }
            Ok(Response::Ok)
        });
        published
    }

    #[test]
    fn a_refused_creation_is_published_under_the_monitor_identity() {
        let mut issues = StreamIssues::default();
        issues.enable();
        let identity = "4f6c2a10-0b1e-4c3d-8e2f-1234567890ab";
        issues.admitted(
            StreamOperation::of(&create(identity)),
            &json!({"ok":false,"code":"monitor_limit"}),
        );
        let published = publish(&mut issues);
        assert_eq!(published.len(), 1);
        let issue = &published[0];
        assert_eq!(issue.operation, identity);
        assert_eq!(issue.layer, LifecycleLayer::VirtualDisplay);
        assert_eq!(issue.phase, "create");
        assert_eq!(issue.code, "monitor_limit");
        assert_eq!(issue.disposition, IssueDisposition::CriticalFailure);
        assert!(issue.caused_by.is_none());
        assert!(issue.site.contains("stream_issues.rs:"), "{}", issue.site);
    }

    #[test]
    fn a_worker_failure_is_published_in_order_after_admission_failures() {
        let mut issues = StreamIssues::default();
        issues.enable();
        let mut manager = StreamIssueOutbox::default();
        manager.push(worker_failure(
            "monitor-1",
            "create",
            "virtual_display_create_failed",
        ));
        issues.adopt(manager.drain());
        let published = publish(&mut issues);
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].code, "virtual_display_create_failed");
        assert!(published[0].site.contains("stream_issues.rs:"));
    }

    #[test]
    fn success_reads_and_disabled_reports_publish_nothing() {
        let status: Operation =
            serde_json::from_value(json!({"action":"status","identity":"monitor-1"})).unwrap();
        assert!(StreamOperation::of(&status).is_none());
        let mut issues = StreamIssues::default();
        issues.enable();
        issues.admitted(
            StreamOperation::of(&create("monitor-2")),
            &json!({"ok":true}),
        );
        assert!(publish(&mut issues).is_empty());

        let mut disabled = StreamIssues::default();
        disabled.admitted(
            StreamOperation::of(&create("monitor-3")),
            &json!({"ok":false,"code":"disabled"}),
        );
        assert!(publish(&mut disabled).is_empty());
        disabled.enable();
        assert!(publish(&mut disabled).is_empty(), "discarded, not kept");
    }
}
