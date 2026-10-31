//! Durable dispatch decisions. Callers persist each transition before acting on it.
use amaterasu_extension_api::{ActionDescriptor, ActionInvoke};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Dispatch {
    pub request: ActionInvoke,
    pub descriptor: ActionDescriptor,
    pub caller: String,
    pub initiator: String,
    pub stage: Stage,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum Stage {
    Planning,
    AwaitingPermit {
        plan: Value,
        digest: String,
    },
    AwaitingConsume {
        plan: Value,
        digest: String,
        permit: String,
    },
    ReadyToExecute {
        plan: Value,
        digest: String,
        permit: String,
    },
    Running {
        digest: String,
        permit: String,
    },
    Completing {
        digest: String,
        permit: String,
        outcome: Outcome,
        provider_receipt: Value,
    },
    Finished {
        outcome: Outcome,
        provider_receipt: Value,
    },
    RecoveryRequired,
    Cancelled,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
    RecoveryRequired,
}
impl Dispatch {
    pub fn new(
        request: ActionInvoke,
        descriptor: ActionDescriptor,
        caller: String,
        initiator: String,
    ) -> Result<Self, &'static str> {
        crate::validation::validate_request(&descriptor, &request)
            .map_err(|_| "invalid_request")?;
        if caller.is_empty() || initiator.is_empty() {
            return Err("invalid_authority");
        }
        Ok(Self {
            request,
            descriptor,
            caller,
            initiator,
            stage: Stage::Planning,
        })
    }
    pub fn accept_plan(&mut self, plan: Value) -> Result<(), &'static str> {
        if self.stage != Stage::Planning {
            return Err("invalid_phase");
        }
        let bytes = serde_json::to_vec(&plan).map_err(|_| "invalid_plan")?;
        if bytes.len() > 4_000 || !plan.is_object() {
            return Err("invalid_plan");
        }
        let digest = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.stage = Stage::AwaitingPermit { plan, digest };
        Ok(())
    }
    pub fn accept_permit(
        &mut self,
        operation_id: &str,
        target: &amaterasu_extension_api::ActionTarget,
        digest: &str,
        permit: String,
    ) -> Result<(), &'static str> {
        let Stage::AwaitingPermit {
            plan,
            digest: expected,
        } = &self.stage
        else {
            return Err("invalid_phase");
        };
        if operation_id != self.request.operation_id
            || target != &self.request.target
            || digest != expected
            || permit.is_empty()
            || permit.len() > 256
        {
            return Err("permit_mismatch");
        }
        self.stage = Stage::AwaitingConsume {
            plan: plan.clone(),
            digest: expected.clone(),
            permit,
        };
        Ok(())
    }
    pub fn confirm_consume(
        &mut self,
        operation_id: &str,
        target: &amaterasu_extension_api::ActionTarget,
        digest: &str,
        permit: &str,
    ) -> Result<(), &'static str> {
        let Stage::AwaitingConsume {
            plan,
            digest: expected,
            permit: selected,
        } = &self.stage
        else {
            return Err("invalid_phase");
        };
        if operation_id != self.request.operation_id
            || target != &self.request.target
            || digest != expected
            || permit != selected
        {
            return Err("permit_mismatch");
        }
        self.stage = Stage::ReadyToExecute {
            plan: plan.clone(),
            digest: expected.clone(),
            permit: selected.clone(),
        };
        Ok(())
    }
    /// Persist the returned Running stage before issuing the provider call.
    pub fn begin_effect(&mut self) -> Result<Value, &'static str> {
        let Stage::ReadyToExecute {
            plan,
            digest,
            permit,
        } = &self.stage
        else {
            return Err("invalid_phase");
        };
        let plan = plan.clone();
        self.stage = Stage::Running {
            digest: digest.clone(),
            permit: permit.clone(),
        };
        Ok(plan)
    }
    pub fn provider_result(
        &mut self,
        outcome: Outcome,
        receipt: Value,
    ) -> Result<(), &'static str> {
        let Stage::Running { digest, permit } = &self.stage else {
            return Err("invalid_phase");
        };
        if serde_json::to_vec(&receipt).map_or(true, |bytes| bytes.len() > 2_000) {
            self.stage = Stage::RecoveryRequired;
            return Err("receipt_too_large");
        }
        self.stage = Stage::Completing {
            digest: digest.clone(),
            permit: permit.clone(),
            outcome,
            provider_receipt: receipt,
        };
        Ok(())
    }
    pub fn confirm_completion(&mut self) -> Result<(), &'static str> {
        let Stage::Completing {
            outcome,
            provider_receipt,
            ..
        } = &self.stage
        else {
            return Err("invalid_phase");
        };
        if *outcome == Outcome::RecoveryRequired {
            self.stage = Stage::RecoveryRequired;
        } else {
            self.stage = Stage::Finished {
                outcome: outcome.clone(),
                provider_receipt: provider_receipt.clone(),
            };
        }
        Ok(())
    }
    /// A restart cannot prove whether an in-flight provider effect committed.
    pub fn fence_after_restart(&mut self) {
        if matches!(
            self.stage,
            Stage::AwaitingPermit { .. }
                | Stage::AwaitingConsume { .. }
                | Stage::ReadyToExecute { .. }
                | Stage::Running { .. }
                | Stage::Completing { .. }
        ) {
            self.stage = Stage::RecoveryRequired;
        }
    }
    pub fn cancel_before_effect(&mut self) -> Result<(), &'static str> {
        if self.stage != Stage::Planning {
            return Err("permit_or_effect_may_be_in_flight");
        }
        self.stage = Stage::Cancelled;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_extension_api::{ActionTarget, ExecutionClass, MethodRef, TargetKind};
    use serde_json::json;
    fn fixture() -> Dispatch {
        let method = MethodRef {
            service: "example/1".into(),
            method: "action_execute".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        };
        let descriptor = ActionDescriptor {
            id: "example/action".into(),
            label: "Action".into(),
            description: "Action".into(),
            execution: ExecutionClass::SessionMutation,
            arguments: Default::default(),
            allowed_targets: vec![TargetKind::Session],
            required_capabilities: vec![],
            plan: Some(method.clone()),
            execute: method,
            status: None,
        };
        let request = ActionInvoke {
            epoch: "e".into(),
            sequence: 1,
            operation_id: "e:1".into(),
            action_id: descriptor.id.clone(),
            target: ActionTarget::Session {
                host_epoch: "h".into(),
                session_id: "s".into(),
                expected_generation: 1,
            },
            arguments: json!({}),
            expected_settings_revision: None,
            input_reservation_id: None,
        };
        Dispatch::new(request, descriptor, "caller".into(), "initiator".into()).unwrap()
    }
    #[test]
    fn effect_requires_matching_authorization_and_consume() {
        let mut operation = fixture();
        assert!(operation.begin_effect().is_err());
        operation
            .accept_plan(json!({"mutation_domains":["desktop"]}))
            .unwrap();
        let Stage::AwaitingPermit { digest, .. } = &operation.stage else {
            panic!()
        };
        let digest = digest.clone();
        let target = operation.request.target.clone();
        assert_eq!(
            operation.accept_permit("other", &target, &digest, "permit".into()),
            Err("permit_mismatch")
        );
        operation
            .accept_permit("e:1", &target, &digest, "permit".into())
            .unwrap();
        assert!(operation.begin_effect().is_err());
        assert_eq!(
            operation.confirm_consume("e:1", &target, &digest, "revoked"),
            Err("permit_mismatch")
        );
        operation
            .confirm_consume("e:1", &target, &digest, "permit")
            .unwrap();
        assert_eq!(
            operation.begin_effect().unwrap(),
            json!({"mutation_domains":["desktop"]})
        );
        assert!(operation.cancel_before_effect().is_err());
        operation
            .provider_result(Outcome::Succeeded, json!({"receipt":"ok"}))
            .unwrap();
        operation.confirm_completion().unwrap();
        assert!(matches!(
            operation.stage,
            Stage::Finished {
                outcome: Outcome::Succeeded,
                ..
            }
        ));
    }
    #[test]
    fn restart_fences_an_effect_with_unknown_outcome() {
        let mut operation = fixture();
        operation.accept_plan(json!({})).unwrap();
        let Stage::AwaitingPermit { digest, .. } = &operation.stage else {
            panic!()
        };
        let digest = digest.clone();
        let target = operation.request.target.clone();
        operation
            .accept_permit("e:1", &target, &digest, "permit".into())
            .unwrap();
        operation
            .confirm_consume("e:1", &target, &digest, "permit")
            .unwrap();
        operation.begin_effect().unwrap();
        operation.fence_after_restart();
        assert_eq!(operation.stage, Stage::RecoveryRequired);
        assert!(operation.begin_effect().is_err());
    }
}
