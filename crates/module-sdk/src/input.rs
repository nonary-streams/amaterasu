//! Versioned broker contract for stream input lease operations.
//!
//! Startup composition chooses the provider of this service and grants calls;
//! the runtime supplies caller identity. A lease is opaque and owner scoped.
//! Reconcile ambiguous acquire results with the same operation ID before retrying.
//! Batches are bounded by the provider and successful admission is not proof of
//! operating-system delivery. Native providers remain trusted code.

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

/// Optional, separately composed extension messages for input devices.
pub mod extensions;
