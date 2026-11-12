//! Downstream source compatibility for the original exhaustive input request API.
use amaterasu_module_sdk::input::{InputRequest, ScopedFeedbackPeek};
use serde_json::json;

fn classify(request: &InputRequest<'_>) -> &'static str {
    match request {
        InputRequest::Lookup { .. } => "lookup",
        InputRequest::Renew { .. } => "renew",
        InputRequest::Release { .. } => "release",
        InputRequest::AcknowledgeBindingRetirement { .. } => "acknowledge_binding_retirement",
        InputRequest::Batch { .. } => "batch",
        InputRequest::FeedbackPeek { .. } => "peek",
        InputRequest::FeedbackAck { .. } => "ack",
        InputRequest::PollGamepadFeedback { .. } => "poll",
    }
}

#[test]
fn original_exhaustive_match_and_additive_scoped_wire_format() {
    let original = InputRequest::FeedbackPeek {
        lease: Some("lease"),
        limit: 4,
    };
    assert_eq!(classify(&original), "peek");
    assert_eq!(
        original.payload(),
        json!({"action":"feedback_peek","lease":"lease","limit":4})
    );
    let scoped = ScopedFeedbackPeek {
        lease: Some("lease"),
        limit: 4,
        session: 19,
        contribution_version: 1,
    };
    assert_eq!(
        scoped.payload(),
        json!({"action":"feedback_peek","lease":"lease","limit":4,"session":19,"contribution_version":1})
    );
}
