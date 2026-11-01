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
pub const MAX_LEASE_BYTES: usize = 128;

/// Read a provider-owned lease without imposing an implementation-specific
/// identifier format. The value is opaque to the consumer.
pub fn lease_from_reply(response: &Value) -> Result<&str, &'static str> {
    response
        .get("lease")
        .and_then(Value::as_str)
        .filter(|lease| valid_lease(lease))
        .ok_or("invalid_input_lease")
}

fn valid_lease(lease: &str) -> bool {
    !lease.is_empty() && lease.len() <= MAX_LEASE_BYTES && !lease.chars().any(char::is_control)
}

/// Capabilities pinned by a provider's active lease response. Omission keeps
/// legacy homogeneous-batch behavior for independently built providers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputBatchCapabilities {
    pub mixed_events: bool,
}

impl InputBatchCapabilities {
    pub fn from_lease_response(response: &Value) -> Result<Self, &'static str> {
        let object = response
            .as_object()
            .ok_or("invalid_input_batch_capabilities")?;
        let mixed_events = match object.get("mixed_input_batches") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => return Err("invalid_input_batch_capabilities"),
        };
        Ok(Self { mixed_events })
    }

    pub fn write_lease_response(self, response: &mut Value) -> Result<(), &'static str> {
        let object = response
            .as_object_mut()
            .ok_or("invalid_input_batch_capabilities")?;
        object.insert("mixed_input_batches".into(), Value::Bool(self.mixed_events));
        Ok(())
    }
}

/// A provider's result for one ordered input batch.
#[derive(Debug, PartialEq, Eq)]
pub enum InputBatchOutcome<'a> {
    /// Every event was accepted by the provider, including suppressed events.
    Complete,
    /// The prefix was accepted. The next event may have had an effect; later
    /// events were not accepted and must not be treated as delivered.
    Partial {
        accepted: usize,
        partial_event_possible: bool,
        code: &'a str,
    },
}

/// Decode progress only when it is bounded by the submitted batch and refers
/// to the same lease. Other errors are not evidence of a retained lease.
pub fn decode_batch_outcome<'a>(
    payload: &'a Value,
    lease: Option<&str>,
    submitted: usize,
) -> Result<InputBatchOutcome<'a>, &'static str> {
    let lease = lease
        .filter(|lease| valid_lease(lease))
        .ok_or("invalid_input_batch_reply")?;
    if !(1..=MAX_BATCH_EVENTS).contains(&submitted)
        || payload.get("lease").and_then(Value::as_str) != Some(lease)
    {
        return Err("invalid_input_batch_reply");
    }
    let accepted = payload
        .get("accepted")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or("invalid_input_batch_reply")?;
    match payload.get("ok").and_then(Value::as_bool) {
        Some(true) if accepted == submitted => Ok(InputBatchOutcome::Complete),
        Some(false) if accepted < submitted => {
            let partial_event_possible = payload
                .get("partial_event_possible")
                .and_then(Value::as_bool)
                .ok_or("invalid_input_batch_reply")?;
            let code = payload
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
                .filter(|code| valid_batch_error_code(code))
                .ok_or("invalid_input_batch_reply")?;
            Ok(InputBatchOutcome::Partial {
                accepted,
                partial_event_possible,
                code,
            })
        }
        _ => Err("invalid_input_batch_reply"),
    }
}

/// Build the common success fields for an ordered batch. Providers may add
/// their own non-authoritative diagnostics, such as a suppressed-event count.
pub fn batch_success(lease: &str, submitted: usize) -> Result<Value, &'static str> {
    if !valid_lease(lease) || !(1..=MAX_BATCH_EVENTS).contains(&submitted) {
        return Err("invalid_input_batch_size");
    }
    Ok(serde_json::json!({"ok":true,"lease":lease,"accepted":submitted}))
}

/// Attach accepted-prefix evidence to a provider's existing error response.
/// The failed event is at `accepted`; its effect may be uncertain.
pub fn record_batch_failure(
    response: &mut Value,
    lease: &str,
    submitted: usize,
    accepted: usize,
    partial_event_possible: bool,
) -> Result<(), &'static str> {
    if !valid_lease(lease)
        || !(1..=MAX_BATCH_EVENTS).contains(&submitted)
        || accepted >= submitted
        || response.get("ok") != Some(&Value::Bool(false))
        || response
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)
            .is_none_or(|code| !valid_batch_error_code(code))
    {
        return Err("invalid_input_batch_reply");
    }
    let object = response
        .as_object_mut()
        .ok_or("invalid_input_batch_reply")?;
    object.insert("lease".into(), Value::String(lease.into()));
    object.insert("accepted".into(), serde_json::json!(accepted));
    object.insert(
        "partial_event_possible".into(),
        Value::Bool(partial_event_possible),
    );
    Ok(())
}

fn valid_batch_error_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 128 && !code.chars().any(char::is_control)
}

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

    #[test]
    fn batch_outcomes_validate_lease_prefix_and_uncertain_event() {
        let success = batch_success("lease", 3).unwrap();
        assert_eq!(
            decode_batch_outcome(&success, Some("lease"), 3),
            Ok(InputBatchOutcome::Complete)
        );
        assert!(decode_batch_outcome(&success, Some("other"), 3).is_err());
        assert!(decode_batch_outcome(&success, Some("lease"), 2).is_err());
        assert!(batch_success("lease", 0).is_err());
        assert!(batch_success("lease", MAX_BATCH_EVENTS + 1).is_err());

        let mut failure =
            json!({"ok":false,"error":{"code":"controller_unavailable","message":"unavailable"}});
        record_batch_failure(&mut failure, "lease", 3, 1, true).unwrap();
        assert_eq!(
            decode_batch_outcome(&failure, Some("lease"), 3),
            Ok(InputBatchOutcome::Partial {
                accepted: 1,
                partial_event_possible: true,
                code: "controller_unavailable",
            })
        );
        assert!(decode_batch_outcome(&failure, Some("lease"), 1).is_err());
        failure["partial_event_possible"] = json!("true");
        assert!(decode_batch_outcome(&failure, Some("lease"), 3).is_err());
        assert!(record_batch_failure(&mut failure, "lease", 3, 3, true).is_err());
    }

    #[test]
    fn mixed_batch_capability_requires_an_explicit_boolean() {
        assert!(
            !InputBatchCapabilities::from_lease_response(&json!({}))
                .unwrap()
                .mixed_events
        );
        let mut response = json!({"ok":true});
        InputBatchCapabilities { mixed_events: true }
            .write_lease_response(&mut response)
            .unwrap();
        assert!(
            InputBatchCapabilities::from_lease_response(&response)
                .unwrap()
                .mixed_events
        );
        response["mixed_input_batches"] = json!("true");
        assert!(InputBatchCapabilities::from_lease_response(&response).is_err());
        assert!(InputBatchCapabilities::from_lease_response(&json!("bad")).is_err());
    }

    #[test]
    fn lease_ids_are_bounded_and_provider_owned() {
        assert_eq!(
            lease_from_reply(&json!({"lease":"provider/lease-7"})),
            Ok("provider/lease-7")
        );
        assert!(lease_from_reply(&json!({"lease":""})).is_err());
        assert!(lease_from_reply(&json!({"lease":"bad\nlease"})).is_err());
        assert!(lease_from_reply(&json!({"lease":"a".repeat(MAX_LEASE_BYTES + 1)})).is_err());
    }
}
