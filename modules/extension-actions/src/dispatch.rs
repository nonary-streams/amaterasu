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
    #[serde(default)]
    pub source: Option<Source>,
    pub stage: Stage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) proof: Option<crate::dispatch_proof::DispatchProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approvals: Option<FrozenApprovals>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenApprovals {
    pub execute: Value,
    pub status: Option<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub caller: String,
    pub initiator: String,
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
    AwaitingTransition {
        plan: Value,
        digest: String,
        permit: String,
    },
    Replanning {
        transition_id: String,
        old_digest: String,
        old_permit: String,
    },
    AwaitingRebind {
        transition_id: String,
        old_digest: String,
        old_permit: String,
        plan: Value,
        digest: String,
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
            source: None,
            stage: Stage::Planning,
            proof: None,
            approvals: None,
        })
    }
    pub fn accept_plan(&mut self, plan: Value) -> Result<(), &'static str> {
        if self.stage != Stage::Planning {
            return Err("invalid_phase");
        }
        let (plan, digest) = self.canonical_plan(&plan)?;
        self.stage = Stage::AwaitingPermit { plan, digest };
        Ok(())
    }
    fn canonical_plan(&self, plan: &Value) -> Result<(Value, String), &'static str> {
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
        let canonical = serde_json::to_value(&typed).map_err(|_| "invalid_plan")?;
        let bytes = serde_json::to_vec(&canonical).map_err(|_| "invalid_plan")?;
        if bytes.len() > 4_000 {
            return Err("invalid_plan");
        }
        let digest = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok((canonical, digest))
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
        self.stage =
            if self.descriptor.execution == amaterasu_extension_api::ExecutionClass::ReadOnly {
                Stage::ReadyToExecute {
                    plan: plan.clone(),
                    digest: expected.clone(),
                    permit: selected.clone(),
                }
            } else {
                Stage::AwaitingTransition {
                    plan: plan.clone(),
                    digest: expected.clone(),
                    permit: selected.clone(),
                }
            };
        Ok(())
    }
    pub fn confirm_transition(
        &mut self,
        operation_id: &str,
        target: &amaterasu_extension_api::ActionTarget,
        digest: &str,
    ) -> Result<(), &'static str> {
        let Stage::AwaitingTransition {
            plan,
            digest: expected,
            permit,
        } = &self.stage
        else {
            return Err("invalid_phase");
        };
        if operation_id != self.request.operation_id
            || target != &self.request.target
            || digest != expected
        {
            return Err("transition_mismatch");
        }
        self.stage = Stage::ReadyToExecute {
            plan: plan.clone(),
            digest: expected.clone(),
            permit: permit.clone(),
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
    pub(crate) fn freeze_approvals(
        &mut self,
        execute: Value,
        status: Option<Value>,
    ) -> Result<(), &'static str> {
        if !matches!(self.stage, Stage::AwaitingPermit { .. }) || self.approvals.is_some() {
            return Err("invalid_phase");
        }
        let check = |approval: &Value, method: &amaterasu_extension_api::MethodRef| {
            let call = amaterasu_module_sdk::Command::CallApi {
                service: method.service.clone(),
                method: method.method.clone(),
                interface_major: method.interface_major,
                interface_minor: method.interface_minor,
                payload: Value::Null,
                timeout_ms: 1,
                operation_id: "approval-check".into(),
                parent: None,
            };
            crate::dispatch_proof::ApprovedCall::new(
                approval.clone(),
                &call,
                &self.selected_provider,
                method,
            )
            .map(|_| ())
        };
        check(&execute, &self.descriptor.execute)?;
        match (&self.descriptor.status, &status) {
            (Some(method), Some(approval)) => check(approval, method)?,
            (None, None) => (),
            _ => return Err("status_approval_mismatch"),
        }
        self.approvals = Some(FrozenApprovals { execute, status });
        Ok(())
    }

    pub(crate) fn arm_effect(&mut self, approval: Value) -> Result<(), &'static str> {
        if !matches!(self.stage, Stage::Running { .. }) || self.proof.is_some() {
            return Err("invalid_phase");
        }
        if self
            .approvals
            .as_ref()
            .is_none_or(|frozen| frozen.execute != approval)
        {
            return Err("execute_approval_changed");
        }
        let call = self.broker_call()?.ok_or("effect_call_missing")?;
        let effect = crate::dispatch_proof::ApprovedCall::new(
            approval,
            &call,
            &self.selected_provider,
            &self.descriptor.execute,
        )?;
        self.proof = Some(crate::dispatch_proof::DispatchProof {
            effect,
            status: Vec::new(),
        });
        Ok(())
    }

    pub(crate) fn arm_status(&mut self, approval: Value) -> Result<(), &'static str> {
        if !matches!(self.stage, Stage::ProviderPending { .. }) {
            return Err("invalid_phase");
        }
        if self
            .approvals
            .as_ref()
            .and_then(|frozen| frozen.status.as_ref())
            != Some(&approval)
        {
            return Err("status_approval_changed");
        }
        let method = self
            .descriptor
            .status
            .as_ref()
            .ok_or("status_unavailable")?;
        let mut call = self.broker_call()?.ok_or("status_call_missing")?;
        let proof = self.proof.as_ref().ok_or("effect_proof_missing")?;
        let amaterasu_module_sdk::Command::CallApi { operation_id, .. } = &mut call else {
            return Err("status_call_missing");
        };
        operation_id.push_str(&format!("-{}", proof.status.len() + 1));
        let call = crate::dispatch_proof::ApprovedCall::new(
            approval,
            &call,
            &self.selected_provider,
            method,
        )?;
        self.proof
            .as_mut()
            .ok_or("effect_proof_missing")?
            .arm_status(call)
    }

    /// Build the next broker operation from a durably persisted phase. The
    /// caller must never reissue a Running effect after an uncertain send.
    pub fn broker_call(&self) -> Result<Option<amaterasu_module_sdk::Command>, &'static str> {
        use amaterasu_module_sdk::Command;
        use serde_json::json;
        let operation_id = &self.request.operation_id;
        let broker_id = operation_id.replace(':', ".");
        let source = self.source.clone().unwrap_or_else(|| Source {
            caller: self.caller.clone(),
            initiator: self.initiator.clone(),
        });
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
            Stage::AwaitingPermit { plan, digest } => {
                let mut payload = json!({"operation":"authorize_action","operation_id":operation_id,"action_id":action_id,"target":target,"plan":plan,"plan_digest":digest,"provider":self.selected_provider,"execute_method":self.descriptor.execute,"descriptor":self.descriptor,"execute_approval":self.approvals.as_ref().map(|a| &a.execute),"status_approval":self.approvals.as_ref().and_then(|a| a.status.as_ref()),"source":source});
                if let Some(reservation) = &self.request.input_reservation_id {
                    payload["input_reservation_id"] = json!(reservation);
                }
                Some(request(payload, "authorize"))
            }
            Stage::AwaitingConsume { digest, permit, .. } => Some(request(
                json!({"operation":"consume_action_permit","operation_id":operation_id,"action_id":action_id,"target":target,"plan_digest":digest,"permit":permit}),
                "consume",
            )),
            Stage::AwaitingTransition { digest, permit, .. } => {
                let mut payload = json!({"operation":"begin_action_transition","operation_id":operation_id,"action_id":action_id,"target":target,"plan_digest":digest,"permit":permit});
                if let Some(reservation) = &self.request.input_reservation_id {
                    payload["input_reservation_id"] = json!(reservation);
                }
                Some(request(payload, "transition"))
            }
            Stage::Replanning { .. } => {
                let method = self.descriptor.plan.as_ref().ok_or("plan_unavailable")?;
                Some(api(
                    method,
                    json!({"version":1,"operation_id":operation_id,"action_id":action_id,"target":target,"arguments":self.request.arguments,"expected_settings_revision":self.request.expected_settings_revision}),
                    "replan",
                ))
            }
            Stage::AwaitingRebind {
                transition_id,
                old_digest,
                old_permit,
                plan,
                digest,
            } => Some(request(
                json!({"operation":"rebind_action_plan","operation_id":operation_id,"transition_id":transition_id,"target":target,"old_permit":old_permit,"old_plan_digest":old_digest,"new_plan":plan,"new_plan_digest":digest}),
                "rebind",
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
                json!({"operation":"complete_action","operation_id":operation_id,"action_id":action_id,"target":target,"plan_digest":digest,"permit":permit,"outcome":outcome,"provider_receipt":provider_receipt,"dispatch_proof":self.proof,"provider_completed":self.proof.as_ref().and_then(|proof| proof.last().result.as_ref())}),
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
        if matches!(
            self.stage,
            Stage::Running { .. } | Stage::ProviderPending { .. }
        ) {
            if let Some(proof) = self.proof.as_mut() {
                proof.complete(reply.clone())?;
            }
            if reply.get("status").and_then(Value::as_str) != Some("pending") {
                let completed: amaterasu_extension_api::ActionProviderCompleted =
                    serde_json::from_value(reply.clone())
                        .map_err(|_| "invalid_provider_completion")?;
                let plan = match &self.stage {
                    Stage::Running { plan, .. } | Stage::ProviderPending { plan, .. } => plan,
                    _ => unreachable!(),
                };
                let plan: amaterasu_extension_api::ResourcePlan =
                    serde_json::from_value(plan.clone()).map_err(|_| "invalid_plan")?;
                amaterasu_extension_api::validate_action_provider_completed(
                    plan.resource_disposition.as_deref(),
                    &completed,
                )
                .map_err(|_| "invalid_provider_completion")?;
            }
        }
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
            Stage::AwaitingTransition { digest, permit, .. } => {
                if !matches!(status, "pending" | "succeeded" | "replan_required")
                    || reply.get("operation_id").and_then(Value::as_str)
                        != Some(self.request.operation_id.as_str())
                    || reply.get("target")
                        != serde_json::to_value(&self.request.target).ok().as_ref()
                    || reply.get("plan_digest").and_then(Value::as_str) != Some(digest.as_str())
                {
                    return Err("transition_denied");
                }
                if status == "pending" {
                    return Ok(());
                }
                if status == "replan_required" {
                    let transition_id = reply
                        .get("transition_id")
                        .and_then(Value::as_str)
                        .filter(|id| {
                            !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
                        })
                        .ok_or("invalid_transition")?
                        .to_owned();
                    self.stage = Stage::Replanning {
                        transition_id,
                        old_digest: digest,
                        old_permit: permit,
                    };
                    return Ok(());
                }
                let target = self.request.target.clone();
                let id = self.request.operation_id.clone();
                self.confirm_transition(&id, &target, &digest)
            }
            Stage::Replanning {
                transition_id,
                old_digest,
                old_permit,
            } => {
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
                let plan = reply.get("plan").cloned().ok_or("invalid_reply")?;
                let (plan, digest) = self.canonical_plan(&plan)?;
                self.stage = Stage::AwaitingRebind {
                    transition_id,
                    old_digest,
                    old_permit,
                    plan,
                    digest,
                };
                Ok(())
            }
            Stage::AwaitingRebind {
                transition_id,
                plan,
                digest,
                old_permit,
                ..
            } => {
                if status != "succeeded"
                    || reply.get("operation_id").and_then(Value::as_str)
                        != Some(self.request.operation_id.as_str())
                    || reply.get("transition_id").and_then(Value::as_str)
                        != Some(transition_id.as_str())
                    || reply.get("target")
                        != serde_json::to_value(&self.request.target).ok().as_ref()
                    || reply.get("plan_digest").and_then(Value::as_str) != Some(digest.as_str())
                {
                    return Err("rebind_denied");
                }
                let permit = reply
                    .get("permit_id")
                    .and_then(Value::as_str)
                    .filter(|p| !p.is_empty() && p.len() <= 256 && *p != old_permit)
                    .ok_or("invalid_reply")?
                    .to_owned();
                self.stage = Stage::ReadyToExecute {
                    plan,
                    digest,
                    permit,
                };
                Ok(())
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
                | Stage::AwaitingTransition { .. }
                | Stage::Replanning { .. }
                | Stage::AwaitingRebind { .. }
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
        if !matches!(
            self.stage,
            Stage::AwaitingConsume { .. }
                | Stage::AwaitingTransition { .. }
                | Stage::ReadyToExecute { .. }
                | Stage::Running { .. }
        ) {
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
            execution: ExecutionClass::ReadOnly,
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
    #[test]
    fn plan_digest_uses_canonical_typed_representation() {
        let mut operation = fixture();
        let mut plan = sample_plan();
        plan["control_proposals"] = json!([]);
        operation.accept_plan(plan).unwrap();
        let Stage::AwaitingPermit { plan, digest } = &operation.stage else {
            panic!()
        };
        assert!(plan.get("control_proposals").is_none());
        let expected = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&sample_plan()).unwrap())
        );
        assert_eq!(*digest, expected);
    }
    #[test]
    fn mutation_waits_for_confirmed_transition_before_effect() {
        let mut operation = fixture();
        operation.descriptor.execution = ExecutionClass::SessionMutation;
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
        assert!(matches!(operation.stage, Stage::AwaitingTransition { .. }));
        assert!(operation.begin_effect().is_err());
        let target_json = serde_json::to_value(&target).unwrap();
        operation.accept_reply(json!({"status":"pending","operation_id":"e:1","target":target_json,"plan_digest":digest})).unwrap();
        assert!(operation.begin_effect().is_err());
        operation.accept_reply(json!({"status":"succeeded","operation_id":"e:1","target":target_json,"plan_digest":digest})).unwrap();
        operation.begin_effect().unwrap();
    }
    #[test]
    fn dependent_cleanup_requires_persisted_replan_and_exact_rebind() {
        let mut operation = fixture();
        operation.descriptor.execution = ExecutionClass::SessionMutation;
        operation.descriptor.allowed_targets = vec![TargetKind::Stream];
        operation.request.target = ActionTarget::Stream {
            host_epoch: "h".into(),
            session_id: "s".into(),
            stream_id: "stream".into(),
            expected_generation: 7,
        };
        operation.request.input_reservation_id = Some("keyboard-reservation".into());
        operation.accept_plan(sample_plan()).unwrap();
        let Stage::AwaitingPermit { digest, .. } = &operation.stage else {
            panic!()
        };
        let old_digest = digest.clone();
        let target = operation.request.target.clone();
        let target_json = serde_json::to_value(&target).unwrap();
        operation
            .accept_permit("e:1", &target, &old_digest, "old-permit".into())
            .unwrap();
        operation
            .confirm_consume("e:1", &target, &old_digest, "old-permit")
            .unwrap();
        let Some(amaterasu_module_sdk::Command::Request { payload, .. }) =
            operation.broker_call().unwrap()
        else {
            panic!()
        };
        assert_eq!(payload["target"], target_json);
        assert_eq!(payload["input_reservation_id"], "keyboard-reservation");
        operation.accept_reply(json!({"status":"replan_required","operation_id":"e:1","target":target_json,"plan_digest":old_digest,"transition_id":"transition"})).unwrap();
        assert!(operation.begin_effect().is_err());
        let Some(amaterasu_module_sdk::Command::CallApi { payload, .. }) =
            operation.broker_call().unwrap()
        else {
            panic!()
        };
        assert_eq!(payload["target"], target_json);
        assert_eq!(payload["operation_id"], "e:1");
        assert_eq!(payload["action_id"], "example/action");
        let mut new_plan = sample_plan();
        new_plan["revision"] = json!(2);
        new_plan["control_proposals"] = json!([]);
        operation.accept_reply(json!({"status":"planned","operation_id":"e:1","action_id":"example/action","target":target_json,"plan":new_plan})).unwrap();
        let Stage::AwaitingRebind { digest, plan, .. } = &operation.stage else {
            panic!()
        };
        assert!(plan.get("control_proposals").is_none());
        let digest = digest.clone();
        let serialized = serde_json::to_vec(&operation).unwrap();
        operation = serde_json::from_slice(&serialized).unwrap();
        assert!(operation.accept_reply(json!({"status":"succeeded","operation_id":"e:1","transition_id":"wrong","target":target_json,"plan_digest":digest,"permit_id":"new-permit"})).is_err());
        assert!(operation.accept_reply(json!({"status":"succeeded","operation_id":"e:1","transition_id":"transition","target":target_json,"plan_digest":digest,"permit_id":"old-permit"})).is_err());
        assert!(operation.begin_effect().is_err());
        operation.accept_reply(json!({"status":"succeeded","operation_id":"e:1","transition_id":"transition","target":target_json,"plan_digest":digest,"permit_id":"new-permit"})).unwrap();
        assert!(
            matches!(&operation.stage, Stage::ReadyToExecute { permit, .. } if permit == "new-permit")
        );
        operation.begin_effect().unwrap();
    }
    #[test]
    fn plan_digest_matches_cross_component_canonical_vector() {
        let mut operation = fixture();
        let mut plan = sample_plan();
        plan["plan_id"] = json!("p");
        plan["constraints"] = json!({"z":1,"a":2});
        plan["consumer_change"] = json!("reconnect_required");
        operation.accept_plan(plan).unwrap();
        let Stage::AwaitingPermit { digest, .. } = &operation.stage else {
            panic!()
        };
        assert_eq!(
            digest,
            "e45d56a17f9c2d75f55e5c408a1274f304a6437f3c01b5c785c619911f5ce73e"
        );
    }
}
