//! Display settings failures this provider reports first-hand to the launch
//! owner's lifecycle store.
//!
//! A source registrar opens and closes a scope, and a consumer acquires an
//! adjustment in it, under operation IDs the caller binds to its stream. A
//! failed reply to one of those requests becomes one issue under that ID; the
//! provider tick publishes it.
use amaterasu_module_sdk::display_settings::Operation;
use amaterasu_module_sdk::stream_lifecycle::{LifecycleLayer, StreamIssue, StreamIssueOutbox};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::Value;

/// Code used when a failed reply carries no identifier-shaped code.
const UNNAMED_FAILURE: &str = "display_settings_failed";

/// The stream operation a request serves and what it was doing.
pub(crate) struct StreamOperation {
    phase: &'static str,
    operation: String,
}

impl StreamOperation {
    /// Scope and adjustment changes are made for a stream. Reads, status and
    /// lease release carry no operation ID and are not reported.
    pub(crate) fn of(operation: &Operation) -> Option<Self> {
        let (phase, operation) = match operation {
            Operation::OpenScope { operation_id, .. } => ("open_scope", operation_id),
            Operation::Acquire { operation_id, .. } => ("acquire", operation_id),
            Operation::CloseScope { operation_id } => ("close_scope", operation_id),
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

    /// Record `reply` to a stream request if it failed. Without a scope the
    /// source cannot be prepared and without its close the adjustment stays,
    /// so each failure is critical to the stream.
    pub(crate) fn completed(&mut self, stream: StreamOperation, reply: &Value) {
        if reply["ok"] == true {
            return;
        }
        let code = reply["code"]
            .as_str()
            .filter(|code| identifier(code))
            .unwrap_or(UNNAMED_FAILURE);
        self.outbox.push(StreamIssue::critical(
            stream.operation,
            LifecycleLayer::DisplaySettings,
            stream.phase,
            code,
        ));
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
            eprintln!("display settings stream issue report failed error={error}");
        }
    }
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
    use amaterasu_module_sdk::stream_lifecycle::{IssueDisposition, StreamReport};
    use serde_json::json;

    fn open_scope(operation_id: &str) -> Operation {
        Operation::OpenScope {
            operation_id: operation_id.into(),
            source: "DP-1".into(),
            consumers: vec!["rtx-hdr".into()],
        }
    }

    fn publish(issues: &mut StreamIssues) -> Vec<Value> {
        let mut published = Vec::new();
        issues.publish(&mut |command| {
            if let Command::Publish { topic, payload } = command {
                assert_eq!(topic, "amaterasu.stream.report/1");
                published.push(payload);
            }
            Ok(Response::Ok)
        });
        published
    }

    #[test]
    fn a_failed_scope_is_published_under_the_registrars_operation() {
        let mut issues = StreamIssues::default();
        issues.enable();
        let stream = StreamOperation::of(&open_scope("source-1f")).unwrap();
        issues.completed(
            stream,
            &json!({"ok":false,"code":"display_source_unavailable"}),
        );
        let published = publish(&mut issues);
        assert_eq!(published.len(), 1);
        assert_eq!(published[0]["kind"], "issue");
        let StreamReport::Issue(issue) = serde_json::from_value(published[0].clone()).unwrap()
        else {
            panic!("an issue")
        };
        assert_eq!(issue.operation, "source-1f");
        assert_eq!(issue.layer, LifecycleLayer::DisplaySettings);
        assert_eq!(issue.phase, "open_scope");
        assert_eq!(issue.code, "display_source_unavailable");
        assert_eq!(issue.disposition, IssueDisposition::CriticalFailure);
        assert!(issue.caused_by.is_none());
        assert!(issue.site.contains("stream_issues.rs:"), "{}", issue.site);
    }

    #[test]
    fn success_reads_and_disabled_reports_publish_nothing() {
        assert!(StreamOperation::of(&Operation::Enumerate).is_none());
        let mut issues = StreamIssues::default();
        issues.enable();
        let stream = StreamOperation::of(&open_scope("source-2a")).unwrap();
        issues.completed(stream, &json!({"ok":true}));
        assert!(publish(&mut issues).is_empty());

        let mut disabled = StreamIssues::default();
        let stream = StreamOperation::of(&open_scope("source-2b")).unwrap();
        disabled.completed(stream, &json!({"ok":false,"code":"display_scope_denied"}));
        assert!(publish(&mut disabled).is_empty());
        disabled.enable();
        assert!(publish(&mut disabled).is_empty(), "discarded, not kept");
    }
}
