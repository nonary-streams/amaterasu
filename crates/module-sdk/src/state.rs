//! Caller-scoped key/value operations for `amaterasu.state/1`.
//!
//! The broker supplies the immediate caller; these requests cannot select a
//! namespace. A delegated child request remains subject to the initiator's
//! grants and deadline. Cancellation does not undo a completed write.
pub use amaterasu_contracts::FeedbackBootstrapClaim;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const STATE_SERVICE: &str = "amaterasu.state/1";
pub const FEEDBACK_BOOTSTRAP_KEY: &str = "host/feedback-bootstrap/v1";

/// The key/value subset of the State v1 contract. Collection and snapshot
/// operations remain available through the ordinary broker protocol.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum StateRequest {
    InitializeFeedbackHost {
        claim: FeedbackBootstrapClaim,
        /// Return the exact stored claim, or retry while the durable State
        /// history marker proves no prior State operation has been accepted.
        #[serde(default)]
        resume_existing: bool,
    },
    Get {
        key: String,
    },
    Set {
        key: String,
        value: Value,
        expected_revision: Option<u64>,
    },
    Delete {
        key: String,
        expected_revision: Option<u64>,
    },
}

impl StateRequest {
    /// Submit using the broker's authenticated caller and explicitly scoped parent.
    /// Admission only queues work; poll for the terminal result and release it.
    pub fn submit(
        &self,
        runtime: &crate::RuntimeClient,
        timeout_ms: u64,
        operation_id: &str,
        parent: Option<u64>,
    ) -> crate::ModuleResult<u64> {
        runtime.request_serialized(
            crate::requests::RequestOptions {
                service: STATE_SERVICE,
                timeout_ms,
                operation_id,
                parent,
            },
            self,
        )
    }

    pub fn payload(&self) -> Value {
        serde_json::to_value(self).expect("state request contains only JSON values")
    }
}

/// The service's structured failure, including its CAS revision when present.
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct StateError {
    pub code: String,
    pub message: String,
    pub current_revision: Option<u64>,
}

/// State key lookup; `value` may be JSON null even when the key exists.
#[derive(Debug, Deserialize)]
pub struct StateLookup {
    pub revision: u64,
    pub found: bool,
    pub value: Value,
}

#[derive(Debug, Deserialize)]
pub struct StateMutation {
    pub revision: u64,
}

#[derive(Debug, Deserialize)]
pub struct StateDeletion {
    pub revision: u64,
    pub deleted: bool,
}

/// Separates service rejections from malformed wire responses.
pub fn decode_response<T: serde::de::DeserializeOwned>(
    payload: Value,
) -> Result<Result<T, StateError>, String> {
    match payload.get("ok").and_then(Value::as_bool) {
        Some(true) => serde_json::from_value(payload)
            .map(Ok)
            .map_err(|error| error.to_string()),
        Some(false) => {
            serde_json::from_value(payload.get("error").cloned().ok_or("missing state error")?)
                .map(Err)
                .map_err(|error| error.to_string())
        }
        None => Err("missing state response status".into()),
    }
}
