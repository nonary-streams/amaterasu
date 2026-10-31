//! Durable evidence of the broker-selected owner and its exact calls.
use amaterasu_extension_api::MethodRef;
use amaterasu_module_sdk::Command;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_STATUS_STEPS: usize = 8;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovedCall {
    pub approval: Value,
    pub call: Value,
    pub result: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchProof {
    pub effect: ApprovedCall,
    pub status: Vec<ApprovedCall>,
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
            approval,
            call: serde_json::to_value(call).map_err(|_| "invalid_approved_call")?,
            result: None,
        })
    }
}

impl DispatchProof {
    pub fn arm_status(&mut self, call: ApprovedCall) -> Result<(), &'static str> {
        if self.status.len() >= MAX_STATUS_STEPS
            || self.last().result.is_none()
            || call.result.is_some()
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
        let call = self.status.last_mut().unwrap_or(&mut self.effect);
        if call.result.is_some() || serde_json::to_vec(&result).map_or(true, |v| v.len() > 4_000) {
            return Err("dispatch_result_invalid");
        }
        call.result = Some(result);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selected_provider_and_exact_method_are_required() {
        let method: MethodRef = serde_json::from_value(json!({"service":"owner/1","method":"execute","interface_major":1,"interface_minor":0,"schema_revision":1})).unwrap();
        let call = Command::CallApi {
            service: "owner/1".into(),
            method: "execute".into(),
            interface_major: 1,
            interface_minor: 0,
            payload: json!({"permit":"p"}),
            timeout_ms: 5000,
            operation_id: "effect".into(),
            parent: None,
        };
        let approval = json!({"provider":"owner","method":{"service":"owner/1","method":"execute","interface_major":1,"interface_minor":0}});
        assert!(ApprovedCall::new(approval.clone(), &call, "owner", &method).is_ok());
        assert!(ApprovedCall::new(approval.clone(), &call, "foreign", &method).is_err());
        let mut forged = approval;
        forged["method"]["method"] = json!("other");
        assert!(ApprovedCall::new(forged, &call, "owner", &method).is_err());
    }
}
