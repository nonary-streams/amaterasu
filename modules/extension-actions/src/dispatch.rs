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
    pub selected_provider: String,
    pub registry_revision: u64,
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
        plan: Value,
        digest: String,
        permit: String,
    },
    ProviderPending {
        plan: Value,
        digest: String,
        permit: String,
        provider_receipt: Value,
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
        selected_provider: String,
        registry_revision: u64,
        caller: String,
        initiator: String,
    ) -> Result<Self, &'static str> {
        crate::validation::validate_request(&descriptor, &request)
            .map_err(|_| "invalid_request")?;
        if caller.is_empty()
            || initiator.is_empty()
            || selected_provider.is_empty()
            || registry_revision == 0
        {
            return Err("invalid_authority");
        }
        Ok(Self {
            request,
            descriptor,
            selected_provider,
            registry_revision,
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
        let typed: amaterasu_extension_api::ResourcePlan =
            serde_json::from_value(plan.clone()).map_err(|_| "invalid_plan")?;
        amaterasu_extension_api::validate_resource_plan(&typed).map_err(|_| "invalid_plan")?;
        if typed
            .required_capabilities
            .iter()
            .any(|capability| !self.descriptor.required_capabilities.contains(capability))
        {
            return Err("capability_expansion");
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
            plan: plan.clone(),
            digest: digest.clone(),
            permit: permit.clone(),
        };
        Ok(plan)
    }
    /// Build the next broker operation from a durably persisted phase. The
    /// caller must never reissue a Running effect after an uncertain send.
    pub fn broker_call(&self) -> Result<Option<amaterasu_module_sdk::Command>, &'static str> {
        use amaterasu_module_sdk::Command;
        use serde_json::json;
        let operation_id = &self.request.operation_id;
        let broker_id = operation_id.replace(':', ".");
        let action_id = &self.request.action_id;
        let target = &self.request.target;
        let request = |payload: Value, suffix: &str| Command::Request {
            service: amaterasu_extension_api::SESSION_LIFECYCLE_SERVICE.into(),
            payload,
            timeout_ms: 5_000,
            operation_id: format!("{broker_id}.{suffix}"),
            parent: None,
        };
        let api = |method: &amaterasu_extension_api::MethodRef, payload: Value, suffix: &str| {
            Command::CallApi {
                service: method.service.clone(),
                method: method.method.clone(),
                interface_major: method.interface_major,
                interface_minor: method.interface_minor,
                payload,
                timeout_ms: 5_000,
                operation_id: format!("{broker_id}.{suffix}"),
                parent: None,
            }
        };
        Ok(match &self.stage {
            Stage::Planning => {
                let method = self.descriptor.plan.as_ref().ok_or("plan_unavailable")?;
                Some(api(
                    method,
                    json!({"version":1,"operation_id":operation_id,"action_id":action_id,"target":target,"arguments":self.request.arguments,"expected_settings_revision":self.request.expected_settings_revision}),
                    "plan",
                ))
            }
            Stage::AwaitingPermit { plan, digest } => Some(request(
                json!({"operation":"authorize_action","operation_id":operation_id,"action_id":action_id,"target":target,"plan":plan,"plan_digest":digest,"source":{"caller":self.caller,"initiator":self.initiator}}),
                "authorize",
            )),
            Stage::AwaitingConsume { digest, permit, .. } => Some(request(
                json!({"operation":"consume_action_permit","operation_id":operation_id,"action_id":action_id,"target":target,"plan_digest":digest,"permit":permit}),
                "consume",
            )),
            Stage::Running {
                plan,
                digest,
                permit,
            } => Some(api(
                &self.descriptor.execute,
                json!({"version":1,"operation_id":operation_id,"action_id":action_id,"target":target,"arguments":self.request.arguments,"expected_settings_revision":self.request.expected_settings_revision,"plan":plan,"plan_digest":digest,"permit":permit}),
                "execute",
            )),
            Stage::ProviderPending {
                digest,
                provider_receipt,
                ..
            } => {
                let method = self
                    .descriptor
                    .status
                    .as_ref()
                    .ok_or("status_unavailable")?;
                Some(api(
                    method,
                    json!({"version":1,"operation_id":operation_id,"action_id":action_id,"target":target,"plan_digest":digest,"provider_receipt":provider_receipt}),
                    "status",
                ))
            }
            Stage::Completing {
                digest,
                permit,
                outcome,
                provider_receipt,
            } => Some(request(
                json!({"operation":"complete_action","operation_id":operation_id,"action_id":action_id,"target":target,"plan_digest":digest,"permit":permit,"outcome":outcome,"provider_receipt":provider_receipt}),
                "complete",
            )),
            Stage::ReadyToExecute { .. }
            | Stage::Finished { .. }
            | Stage::RecoveryRequired
            | Stage::Cancelled => None,
        })
    }

    /// Advance only on a matching, explicit result; transport failures and
    /// unknown native outcomes must be handled as recovery, never as replay.
    pub fn accept_reply(&mut self, reply: Value) -> Result<(), &'static str> {
        let status = reply
            .get("status")
            .and_then(Value::as_str)
            .ok_or("invalid_reply")?;
        match self.stage.clone() {
            Stage::Planning => {
                if status != "planned"
                    || reply.get("operation_id").and_then(Value::as_str)
                        != Some(self.request.operation_id.as_str())
                    || reply.get("action_id").and_then(Value::as_str)
                        != Some(self.request.action_id.as_str())
                    || reply.get("target")
                        != serde_json::to_value(&self.request.target).ok().as_ref()
                {
                    return Err("invalid_reply");
                }
                self.accept_plan(reply.get("plan").cloned().ok_or("invalid_reply")?)
            }
            Stage::AwaitingPermit { digest, .. } => {
                if status != "succeeded"
                    || reply.get("operation_id").and_then(Value::as_str)
                        != Some(self.request.operation_id.as_str())
                    || reply.get("target")
                        != serde_json::to_value(&self.request.target).ok().as_ref()
                {
                    return Err("permit_denied");
                }
                let permit = reply
                    .get("permit")
                    .and_then(Value::as_str)
                    .ok_or("invalid_reply")?
                    .to_owned();
                let target = self.request.target.clone();
                let id = self.request.operation_id.clone();
                self.accept_permit(&id, &target, &digest, permit)
            }
            Stage::AwaitingConsume { digest, permit, .. } => {
                if status != "succeeded"
                    || reply.get("operation_id").and_then(Value::as_str)
                        != Some(self.request.operation_id.as_str())
                    || reply.get("target")
                        != serde_json::to_value(&self.request.target).ok().as_ref()
                    || reply.get("plan_digest").and_then(Value::as_str) != Some(digest.as_str())
                {
                    return Err("permit_denied");
                }
                let target = self.request.target.clone();
                let id = self.request.operation_id.clone();
                self.confirm_consume(&id, &target, &digest, &permit)
            }
            Stage::Running { .. } | Stage::ProviderPending { .. } => {
                let receipt = reply.get("receipt").cloned().ok_or("invalid_reply")?;
                match status {
                    "pending" => self.provider_pending(receipt),
                    "succeeded" => self.provider_result(Outcome::Succeeded, receipt),
                    "failed" if reply.get("cleanup_complete") == Some(&Value::Bool(true)) => {
                        self.provider_result(Outcome::Failed, receipt)
                    }
                    "failed" | "recovery_required" => {
                        self.provider_result(Outcome::RecoveryRequired, receipt)
                    }
                    _ => Err("invalid_reply"),
                }
            }
            Stage::Completing { .. } => {
                if status != "succeeded"
                    || reply.get("operation_id").and_then(Value::as_str)
                        != Some(self.request.operation_id.as_str())
                {
                    return Err("completion_unknown");
                }
                self.confirm_completion()
            }
            _ => Err("invalid_phase"),
        }
    }

    pub fn provider_pending(&mut self, receipt: Value) -> Result<(), &'static str> {
        let (Stage::Running {
            plan,
            digest,
            permit,
        }
        | Stage::ProviderPending {
            plan,
            digest,
            permit,
            ..
        }) = &self.stage
        else {
            return Err("invalid_phase");
        };
        if serde_json::to_vec(&receipt).map_or(true, |bytes| bytes.len() > 2_000) {
            self.stage = Stage::RecoveryRequired;
            return Err("receipt_too_large");
        }
        self.stage = Stage::ProviderPending {
            plan: plan.clone(),
            digest: digest.clone(),
            permit: permit.clone(),
            provider_receipt: receipt,
        };
        Ok(())
    }
    pub fn provider_result(
        &mut self,
        outcome: Outcome,
        receipt: Value,
    ) -> Result<(), &'static str> {
        let (Stage::Running { digest, permit, .. } | Stage::ProviderPending { digest, permit, .. }) =
            &self.stage
        else {
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
                | Stage::ProviderPending { .. }
                | Stage::Completing { .. }
        ) {
            self.stage = Stage::RecoveryRequired;
        }
    }
    /// Only a coordinator-confirmed permit cancellation may use this before the
    /// provider request is sent. The caller must persist it before reporting it.
    pub fn confirm_cancelled_before_effect(&mut self) -> Result<(), &'static str> {
        if !matches!(self.stage, Stage::Running { .. }) {
            return Err("invalid_phase");
        }
        self.stage = Stage::Cancelled;
        Ok(())
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
    fn sample_plan() -> Value {
        json!({"plan_id":"p1","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})
    }
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
        Dispatch::new(
            request,
            descriptor,
            "example".into(),
            1,
            "caller".into(),
            "initiator".into(),
        )
        .unwrap()
    }
    #[test]
    fn effect_requires_matching_authorization_and_consume() {
        let mut operation = fixture();
        assert!(operation.begin_effect().is_err());
        operation.accept_plan(sample_plan()).unwrap();
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
        assert_eq!(operation.begin_effect().unwrap(), sample_plan());
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
        operation.accept_plan(sample_plan()).unwrap();
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
    #[test]
    fn broker_wire_requires_explicit_permit_and_completion() {
        let mut operation = fixture();
        let target = serde_json::to_value(&operation.request.target).unwrap();
        assert!(matches!(
            operation.broker_call().unwrap(),
            Some(amaterasu_module_sdk::Command::CallApi { .. })
        ));
        operation.accept_reply(json!({"status":"planned","operation_id":"e:1","action_id":"example/action","target":target,"plan":sample_plan()})).unwrap();
        let Stage::AwaitingPermit { digest, .. } = &operation.stage else {
            panic!()
        };
        let digest = digest.clone();
        operation.accept_reply(json!({"status":"succeeded","operation_id":"e:1","target":target,"permit":"opaque"})).unwrap();
        assert!(operation.accept_reply(json!({"status":"succeeded","operation_id":"e:1","target":target,"plan_digest":"wrong"})).is_err());
        operation.accept_reply(json!({"status":"succeeded","operation_id":"e:1","target":target,"plan_digest":digest})).unwrap();
        assert!(operation.broker_call().unwrap().is_none());
        operation.begin_effect().unwrap();
        assert!(matches!(
            operation.broker_call().unwrap(),
            Some(amaterasu_module_sdk::Command::CallApi { .. })
        ));
        operation
            .accept_reply(json!({"status":"succeeded","receipt":{"id":"r"}}))
            .unwrap();
        assert!(matches!(
            operation.broker_call().unwrap(),
            Some(amaterasu_module_sdk::Command::Request { .. })
        ));
        operation
            .accept_reply(json!({"status":"succeeded","operation_id":"e:1"}))
            .unwrap();
        assert!(matches!(
            operation.stage,
            Stage::Finished {
                outcome: Outcome::Succeeded,
                ..
            }
        ));
    }
    #[test]
    fn plan_cannot_expand_approved_capabilities() {
        let mut operation = fixture();
        let mut plan = sample_plan();
        plan["required_capabilities"] = json!(["example/capability"]);
        assert_eq!(operation.accept_plan(plan), Err("capability_expansion"));
        assert_eq!(operation.stage, Stage::Planning);
    }
}
