//! Stream failures this capture provider reports first-hand.
//!
//! The video service opens a capture stream with an `operation_id` it derived
//! from its own stream operation and bound to the launch. A failed open, and
//! the first failed read of a stream it opened, are reported under that
//! operation with this module as the publisher.
use amaterasu_module_sdk::stream_lifecycle::{
    LifecycleLayer, StreamIssue, StreamIssueOutbox, StreamReport,
};

/// Failure codes that mean the display output went away or changed under
/// the capture rather than the capture itself failing.
const DISPLAY_CODES: &[&str] = &["output_unavailable", "access_lost"];

/// A capture failure in `phase` with this provider's `code`.
#[track_caller]
pub(crate) fn capture_failure(operation: &str, phase: &str, code: &str) -> StreamIssue {
    if DISPLAY_CODES.contains(&code) {
        StreamIssue::critical(
            operation,
            LifecycleLayer::Capture,
            phase,
            "capture_output_lost",
        )
        .caused_by(LifecycleLayer::Display)
        .because(code)
    } else {
        StreamIssue::critical(operation, LifecycleLayer::Capture, phase, code)
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
