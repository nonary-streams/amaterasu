//! Optional two-phase retirement bookkeeping for lifecycle-bound input leases.
use amaterasu_extension_api::LifecycleTarget;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const INPUT_BINDING_RETIREMENT_VERSION: u32 = 1;
const MAX_ID_BYTES: usize = 256;

/// Produced only after the exact native input context has retired. The
/// coordinator persists it with native retirement before acknowledging it.
/// Acknowledgement permits forgetting this receipt; it grants no input work
/// and must never be used as independent evidence of native cleanup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputBindingRetirement {
    pub version: u32,
    pub token: String,
    pub target: LifecycleTarget,
    pub revoke_operation_id: String,
    pub input_boot_epoch: String,
}

impl InputBindingRetirement {
    pub fn validate(&self) -> Result<(), &'static str> {
        let LifecycleTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            generation,
        } = &self.target
        else {
            return Err("invalid_input_binding_retirement");
        };
        if self.version != INPUT_BINDING_RETIREMENT_VERSION
            || *generation == 0
            || [
                &self.token,
                &self.revoke_operation_id,
                &self.input_boot_epoch,
                host_epoch,
                session_id,
                stream_id,
            ]
            .into_iter()
            .any(|value| {
                value.is_empty()
                    || value.len() > MAX_ID_BYTES
                    || value.chars().any(char::is_control)
            })
        {
            return Err("invalid_input_binding_retirement");
        }
        Ok(())
    }
}

/// Legacy providers omit `retirement_receipt` and retain their original
/// retirement behavior. An advertised v1 receipt must match every expected
/// identity; an unknown receipt version is not authority to acknowledge it.
pub fn binding_retirement_from_reply(
    payload: &Value,
    expected: &InputBindingRetirement,
) -> Result<Option<InputBindingRetirement>, &'static str> {
    expected.validate()?;
    if payload["status"] != "retired"
        || payload["token"] != expected.token
        || payload["target"] != serde_json::json!(expected.target)
    {
        return Err("invalid_input_binding_retirement");
    }
    let Some(receipt) = payload.get("retirement_receipt") else {
        return Ok(None);
    };
    let version = receipt
        .get("version")
        .and_then(Value::as_u64)
        .ok_or("invalid_input_binding_retirement")?;
    if version != u64::from(INPUT_BINDING_RETIREMENT_VERSION) {
        return Ok(None);
    }
    let receipt: InputBindingRetirement =
        serde_json::from_value(receipt.clone()).map_err(|_| "invalid_input_binding_retirement")?;
    receipt.validate()?;
    if &receipt != expected {
        return Err("invalid_input_binding_retirement");
    }
    Ok(Some(receipt))
}

/// An exact metadata acknowledgement retires only the stored receipt. Native
/// cleanup must already be proved and persisted before this call is issued.
pub fn binding_retirement_acknowledged(payload: &Value, expected: &InputBindingRetirement) -> bool {
    expected.validate().is_ok()
        && payload["ok"] == true
        && payload["status"] == "acknowledged"
        && payload["receipt"] == serde_json::json!(expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::InputRequest;
    use serde_json::json;

    fn receipt() -> InputBindingRetirement {
        InputBindingRetirement {
            version: INPUT_BINDING_RETIREMENT_VERSION,
            token: "binding".into(),
            target: LifecycleTarget::Stream {
                host_epoch: "host".into(),
                session_id: "session".into(),
                stream_id: "stream".into(),
                generation: 1,
            },
            revoke_operation_id: "revoke".into(),
            input_boot_epoch: "input-boot".into(),
        }
    }

    #[test]
    fn legacy_provider_retirement_does_not_require_optional_acknowledgement() {
        let expected = receipt();
        let legacy = json!({"status":"retired","token":expected.token,"target":expected.target});
        assert_eq!(binding_retirement_from_reply(&legacy, &expected), Ok(None));
        let mut newer = legacy;
        newer["retirement_receipt"] = json!({"version":2});
        assert_eq!(binding_retirement_from_reply(&newer, &expected), Ok(None));
    }

    #[test]
    fn optional_receipt_requires_the_exact_native_retirement_identity() {
        let expected = receipt();
        let payload = json!({"status":"retired","token":expected.token,"target":expected.target,"retirement_receipt":expected});
        assert_eq!(
            binding_retirement_from_reply(&payload, &expected),
            Ok(Some(expected.clone()))
        );
        for field in ["token", "revoke_operation_id", "input_boot_epoch"] {
            let mut wrong = payload.clone();
            wrong["retirement_receipt"][field] = json!("another");
            assert!(binding_retirement_from_reply(&wrong, &expected).is_err());
        }
        let mut wrong = payload;
        wrong["retirement_receipt"]["target"]["generation"] = json!(2);
        assert!(binding_retirement_from_reply(&wrong, &expected).is_err());
    }

    #[test]
    fn acknowledgement_is_a_versioned_metadata_request_with_an_exact_reply() {
        let expected = receipt();
        assert_eq!(
            InputRequest::AcknowledgeBindingRetirement { receipt: &expected }.payload(),
            json!({"action":"acknowledge_binding_retirement","receipt":expected})
        );
        let payload = json!({"ok":true,"status":"acknowledged","receipt":expected});
        assert!(binding_retirement_acknowledged(&payload, &expected));
        let mut wrong = payload;
        wrong["receipt"]["input_boot_epoch"] = json!("other");
        assert!(!binding_retirement_acknowledged(&wrong, &expected));
        assert!(!binding_retirement_acknowledged(
            &json!({"status":"retired"}),
            &expected
        ));
    }
}
