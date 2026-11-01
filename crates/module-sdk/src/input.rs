//! Versioned broker contract for stream input lease operations.
//!
//! Startup composition chooses the provider of this service and grants calls;
//! the runtime supplies caller identity. A lease is opaque and owner scoped.
//! Reconcile ambiguous acquire results with the same operation ID before retrying.
//! Batches are bounded by the provider and successful admission is not proof of
//! operating-system delivery. Native providers remain trusted code.

pub mod outbound;

use serde::Serialize;
use serde_json::Value;

pub const INPUT_SERVICE: &str = "amaterasu.input/1";
pub const MAX_BATCH_EVENTS: usize = 64;

/// Common stream lease operations. Acquisition policy is provider-independent
/// but remains encoded separately until its complete schema is shared.
#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum InputRequest<'a> {
    Lookup {
        operation_id: &'a str,
    },
    Renew {
        lease: Option<&'a str>,
        ttl_ms: u64,
    },
    Release {
        lease: Option<&'a str>,
    },
    Batch {
        lease: Option<&'a str>,
        events: Vec<&'a Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        input_binding: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        operation_id: Option<String>,
    },
    FeedbackPeek {
        lease: Option<&'a str>,
        limit: u8,
    },
    FeedbackAck {
        lease: Option<&'a str>,
        through_sequence: Option<u64>,
    },
    PollGamepadFeedback {
        lease: Option<&'a str>,
    },
}

/// Versioned optional extension to the legacy peek request. The provider binds
/// session to the broker-attested lease owner and filters stale resources.
#[derive(Debug, Serialize)]
pub struct ScopedFeedbackPeek<'a> {
    pub lease: Option<&'a str>,
    pub limit: u8,
    pub session: u64,
    pub contribution_version: u16,
}
impl ScopedFeedbackPeek<'_> {
    pub fn payload(&self) -> Value {
        let mut payload =
            serde_json::to_value(self).expect("feedback peek contains only JSON values");
        payload["action"] = Value::String("feedback_peek".into());
        payload
    }
}

impl InputRequest<'_> {
    /// Serialize the contract payload for the broker request adapter.
    pub fn payload(&self) -> Value {
        serde_json::to_value(self).expect("input request contains only JSON values")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // This exhaustive match intentionally fails to compile if the legacy enum grows.
    fn original_request_shape(request: &InputRequest<'_>) -> &'static str {
        match request {
            InputRequest::Lookup { .. } => "lookup",
            InputRequest::Renew { .. } => "renew",
            InputRequest::Release { .. } => "release",
            InputRequest::Batch { .. } => "batch",
            InputRequest::FeedbackPeek { .. } => "peek",
            InputRequest::FeedbackAck { .. } => "ack",
            InputRequest::PollGamepadFeedback { .. } => "poll",
        }
    }

    #[test]
    fn legacy_and_scoped_peek_keep_distinct_source_and_wire_contracts() {
        assert_eq!(
            original_request_shape(&InputRequest::FeedbackPeek {
                lease: None,
                limit: 1
            }),
            "peek"
        );
        assert_eq!(
            InputRequest::FeedbackPeek {
                lease: Some("opaque"),
                limit: 2
            }
            .payload(),
            json!({"action":"feedback_peek","lease":"opaque","limit":2})
        );
        assert_eq!(
            ScopedFeedbackPeek {
                lease: Some("opaque"),
                limit: 2,
                session: 19,
                contribution_version: 1
            }
            .payload(),
            json!({"action":"feedback_peek","lease":"opaque","limit":2,"session":19,"contribution_version":1})
        );
    }

    #[test]
    fn bound_batch_preserves_wire_contract() {
        let event = json!({"kind":"key","state":"down"});
        let request = InputRequest::Batch {
            lease: Some("opaque"),
            events: vec![&event],
            input_binding: Some("binding"),
            operation_id: Some("epoch:1".into()),
        };
        assert_eq!(
            request.payload(),
            json!({"action":"batch","lease":"opaque","events":[event],"input_binding":"binding","operation_id":"epoch:1"})
        );
        let unbound = InputRequest::Batch {
            lease: None,
            events: vec![],
            input_binding: None,
            operation_id: None,
        };
        assert_eq!(
            unbound.payload(),
            json!({"action":"batch","lease":null,"events":[]})
        );
    }
}
