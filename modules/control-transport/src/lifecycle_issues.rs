//! Stream failures this transport reports to the launch owner's lifecycle
//! store.
//!
//! The stream session opens each control stream with an operation ID it has
//! bound to the launch. Open refusals and the failure that ended each stream
//! are queued here and published from the module's tick on the SDK's
//! stream-report topic, where the runtime stamps this module as the reporter.
use crate::controller::Controller;
use crate::stream::Stream;
use amaterasu_module_sdk::stream_lifecycle::{LifecycleLayer, StreamIssue, StreamIssueOutbox};
use amaterasu_module_sdk::{Command, ModuleResult, Response};

const LAYER: LifecycleLayer = LifecycleLayer::ControlTransport;
const OPEN_PHASE: &str = "open";
const CONTROL_PHASE: &str = "control";

/// Refusals that say the caller may not use this transport at all. They
/// name no stream of the caller's.
const UNREPORTED_REFUSALS: [&str; 1] = ["forbidden"];

/// Ends that are not failures: the client closing its control connection
/// is how a stream normally ends.
const ORDERLY_ENDS: [&str; 1] = ["peer_disconnected"];

/// What the transport reports and whether it publishes it.
#[derive(Default)]
pub struct StreamIssueReports {
    outbox: StreamIssueOutbox,
    enabled: bool,
}

impl StreamIssueReports {
    /// Record an open the transport refused.
    #[track_caller]
    pub fn open_refused(&mut self, operation: &str, code: &str) {
        if !UNREPORTED_REFUSALS.contains(&code) {
            self.outbox
                .push(StreamIssue::critical(operation, LAYER, OPEN_PHASE, code));
        }
    }

    /// Record the failure that ended `stream`, once, at the line that raised
    /// it.
    pub fn stream_failed(&mut self, stream: &mut Stream) {
        if stream.failure_reported {
            return;
        }
        let Some((code, site)) = stream.failure_origin() else {
            return;
        };
        stream.failure_reported = true;
        let Some(operation) = &stream.operation_id else {
            return;
        };
        if ORDERLY_ENDS.contains(&code) {
            return;
        }
        let mut issue = StreamIssue::critical(operation, LAYER, CONTROL_PHASE, code);
        issue.site = format!("{}:{}", site.file(), site.line());
        self.outbox.push(issue);
    }

    /// Publish what was recorded. Off by default so a transport in a test
    /// sends only the commands that test scripts; when off, reports are
    /// discarded. A refused publication keeps the rest for the next tick.
    pub fn publish(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) {
        if !self.enabled {
            self.outbox.drain().for_each(drop);
        } else if !self.outbox.is_empty()
            && let Err(error) = self.outbox.flush_with(send)
        {
            eprintln!("amaterasu control transport stream issue report failed error={error}");
        }
    }
}

impl Controller {
    /// Report on the launch owner's topic.
    pub fn enable_stream_issue_reports(&mut self) {
        self.issue_reports.enabled = true;
    }

    /// Publish what this transport recorded through `send`.
    pub fn publish_stream_issues(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) {
        self.issue_reports.publish(send);
    }
}

#[cfg(test)]
mod tests;
