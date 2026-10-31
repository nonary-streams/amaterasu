//! Bounded commitments to broker calls observed by the trusted action service.
use amaterasu_extension_api::MethodRef;
use amaterasu_module_sdk::Command;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const MAX_STATUS_STEPS: usize = 8;

pub(crate) fn digest(value: &Value) -> Result<String, &'static str> {
    let bytes = serde_json::to_vec(value).map_err(|_| "invalid_commitment")?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovedCall {
    pub call_digest: String,
    pub input_receipt_digest: Option<String>,
    pub result_digest: Option<String>,
    pub pending_receipt_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchProof {
    pub effect: ApprovedCall,
    pub status: Vec<ApprovedCall>,
    pub completed: Option<Value>,
}

impl ApprovedCall {
    pub fn new(
        approval: Value,
        call: &Command,
        provider: &str,
        method: &MethodRef,
    ) -> Result<Self, &'static str> {
        let Command::CallApi {
            service,
            method: name,
            interface_major,
            interface_minor,
            payload,
            ..
        } = call
        else {
            return Err("invalid_approved_call");
        };
        if approval["provider"].as_str() != Some(provider)
            || approval["method"]["service"].as_str() != Some(method.service.as_str())
            || approval["method"]["method"].as_str() != Some(method.method.as_str())
            || approval["method"]["interface_major"].as_u64()
                != Some(u64::from(method.interface_major))
            || approval["method"]["interface_minor"].as_u64()
                != Some(u64::from(method.interface_minor))
            || service != &method.service
            || name != &method.method
            || *interface_major != method.interface_major
            || *interface_minor != method.interface_minor
        {
            return Err("approval_mismatch");
        }
        Ok(Self {
            call_digest: digest(payload)?,
            input_receipt_digest: payload.get("provider_receipt").map(digest).transpose()?,
            result_digest: None,
            pending_receipt_digest: None,
        })
    }
}
impl DispatchProof {
    pub fn arm_status(&mut self, call: ApprovedCall) -> Result<(), &'static str> {
        if self.status.len() >= MAX_STATUS_STEPS
            || self.last().result_digest.is_none()
            || call.input_receipt_digest != self.last().pending_receipt_digest
            || call.result_digest.is_some()
        {
            return Err("status_proof_unavailable");
        }
        self.status.push(call);
        Ok(())
    }
    pub fn last(&self) -> &ApprovedCall {
        self.status.last().unwrap_or(&self.effect)
    }
    pub fn complete(&mut self, result: Value) -> Result<(), &'static str> {
        if self.last().result_digest.is_some()
            || serde_json::to_vec(&result).map_or(true, |v| v.len() > 4_000)
        {
            return Err("dispatch_result_invalid");
        }
        let pending = result["status"] == "pending";
        let result_digest = digest(&result)?;
        let receipt_digest = if pending {
            Some(digest(result.get("receipt").ok_or("invalid_reply")?)?)
        } else {
            None
        };
        let call = self.status.last_mut().unwrap_or(&mut self.effect);
        call.result_digest = Some(result_digest);
        call.pending_receipt_digest = receipt_digest;
        if !pending {
            self.completed = Some(result);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maximum_pending_chain_keeps_only_linked_commitments() {
        let method: MethodRef = serde_json::from_value(json!({"service":"owner/1","method":"effect","interface_major":1,"interface_minor":0,"schema_revision":1})).unwrap();
        let approval = json!({"provider":"owner","method":{"service":"owner/1","method":"effect","interface_major":1,"interface_minor":0}});
        let call = |receipt: Option<Value>, index: usize| Command::CallApi {
            service: "owner/1".into(),
            method: "effect".into(),
            interface_major: 1,
            interface_minor: 0,
            payload: if let Some(receipt) = receipt {
                json!({"provider_receipt":receipt})
            } else {
                json!({"permit":"p"})
            },
            timeout_ms: 5000,
            operation_id: format!("op-{index}"),
            parent: None,
        };
        let effect = ApprovedCall::new(approval.clone(), &call(None, 0), "owner", &method).unwrap();
        let mut proof = DispatchProof {
            effect,
            status: vec![],
            completed: None,
        };
        let mut previous = None;
        for index in 0..=8 {
            if index > 0 {
                let step = ApprovedCall::new(
                    approval.clone(),
                    &call(previous.clone(), index),
                    "owner",
                    &method,
                )
                .unwrap();
                assert_eq!(
                    step.input_receipt_digest,
                    proof.last().pending_receipt_digest
                );
                proof.arm_status(step).unwrap();
            }
            if index == 8 {
                let receipt = Value::String("x".repeat(1_998));
                let mut result = json!({"status":"succeeded","receipt":receipt,"padding":""});
                let size = serde_json::to_vec(&result).unwrap().len();
                result["padding"] = Value::String("p".repeat(4_000 - size));
                proof.complete(result).unwrap();
            } else {
                let receipt = Value::String(format!("{}{}", index, "x".repeat(1_997)));
                proof
                    .complete(
                        json!({"status":"pending","receipt":receipt,"extra":"z".repeat(1_800)}),
                    )
                    .unwrap();
                previous = Some(receipt);
            }
        }
        assert_eq!(proof.status.len(), 8);
        assert!(serde_json::to_vec(&proof).unwrap().len() < 8_000);
        assert_eq!(proof.last().pending_receipt_digest, None);
        assert_eq!(
            proof.last().result_digest.as_deref(),
            Some(digest(proof.completed.as_ref().unwrap()).unwrap().as_str())
        );
    }
}
