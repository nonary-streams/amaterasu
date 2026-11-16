//! Stream failures this capture provider reports first-hand.
//!
//! The video service opens a capture stream with an `operation_id` it derived
//! from its own stream operation and bound to the launch. A failed open, and
//! the loss of a source a stream is reading, are reported under that
//! operation with this module as the publisher.
use crate::model::SourceState;
use amaterasu_module_sdk::stream_lifecycle::{
    LifecycleLayer, StreamIssue, StreamIssueOutbox, StreamReport,
};

/// A capture failure in `phase` with this provider's `code`.
#[track_caller]
pub(crate) fn capture_failure(operation: &str, phase: &str, code: &str) -> StreamIssue {
    StreamIssue::critical(operation, LifecycleLayer::Capture, phase, code)
}

/// The failure code of a source that can no longer deliver frames.
pub(crate) fn lost_source_code(state: SourceState) -> Option<&'static str> {
    match state {
        SourceState::Failed(code) => Some(code),
        SourceState::Revoked => Some("source_revoked"),
        SourceState::Denied => Some("source_denied"),
        SourceState::Selecting | SourceState::Ready { .. } => None,
    }
}

/// Move queued reports between outboxes, keeping their order.
pub(crate) fn transfer(from: &mut StreamIssueOutbox, to: &mut StreamIssueOutbox) {
    for report in from.drain() {
        match report {
            StreamReport::Issue(issue) => to.push(issue),
            StreamReport::Binding(binding) => to.push_binding(binding),
        }
    }
}
