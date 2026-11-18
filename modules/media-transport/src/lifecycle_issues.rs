//! Stream failures this transport reports to the launch owner's lifecycle
//! store.
//!
//! The stream session opens each media stream with an operation ID it has
//! bound to the launch. Open refusals, demoted options and the first
//! delivery failure of each stream are queued here and published from the
//! module's tick on the SDK's stream-report topic, where the runtime stamps
//! this module as the reporter.
use crate::controller::Controller;
use crate::stream::Stream;
use amaterasu_module_sdk::stream_lifecycle::{LifecycleLayer, StreamIssue, StreamIssueOutbox};
use amaterasu_module_sdk::stream_warnings::WarningValue;
use amaterasu_module_sdk::{Command, ModuleResult, Response};

const LAYER: LifecycleLayer = LifecycleLayer::MediaTransport;
const OPEN_PHASE: &str = "open";
const DELIVERY_PHASE: &str = "deliver";

/// Refusals that say the caller may not use this transport at all. They
/// name no stream of the caller's.
const UNREPORTED_REFUSALS: [&str; 1] = ["forbidden"];

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

    /// Record the options an opened stream could not honor as requested.
    #[track_caller]
    pub fn open_demoted(&mut self, operation: &str, requested_batch_size_kb: u8, stream: &Stream) {
        if stream.video_batch_size_fallback {
            let effective = stream.video_settings.max_batch_size_kb;
            self.outbox.push(
                StreamIssue::demoted(
                    operation,
                    LAYER,
                    OPEN_PHASE,
                    "video_batch_size_unsupported",
                    "maximum_video_batch_size",
                )
                .with_values(
                    Some(WarningValue::Integer(requested_batch_size_kb.into())),
                    Some(WarningValue::Integer(effective.into())),
                ),
            );
        }
    }

    /// Record the first delivery failure of `stream`, once, at the line that
    /// raised it.
    pub fn delivery_failed(&mut self, stream: &mut Stream) {
        if stream.failure_reported {
            return;
        }
        let Some(failure) = &stream.delivery.first_failure else {
            return;
        };
        stream.failure_reported = true;
        let Some(operation) = &stream.operation_id else {
            return;
        };
        let mut issue = StreamIssue::critical(operation, LAYER, DELIVERY_PHASE, failure.code)
            .because(&failure.reason)
            .with_values(
                failure.requested.map(WarningValue::Integer),
                failure.observed.map(WarningValue::Integer),
            );
        issue.os_error = failure.os_error;
        issue.site = format!("{}:{}", failure.site.file(), failure.site.line());
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
            eprintln!("amaterasu media transport stream issue report failed error={error}");
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
