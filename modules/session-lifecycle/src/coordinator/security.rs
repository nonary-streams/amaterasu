use super::*;
use crate::input_authority::{ActiveBinding, Outcome, ReservationResult};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

pub(super) struct ValidationJob {
    key: String,
    target: LifecycleTarget,
    binding: CaptureBinding,
    observation: Value,
    consumer: Option<String>,
    provider: String,
    request: Option<u64>,
    result: Option<Value>,
    operation_id: String,
    deadline: Instant,
    durable: bool,
    input_token: Option<String>,
}
pub(super) struct RevocationJob {
    request: Option<u64>,
    result: Option<Value>,
    operation_id: String,
}

fn fingerprint(request: &amaterasu_extension_api::ProviderRequest) -> ModuleResult<String> {
    let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn target_for(binding: &ActiveBinding) -> LifecycleTarget {
    LifecycleTarget::Stream {
        host_epoch: binding.host_epoch.clone(),
        session_id: binding.session_id.clone(),
        stream_id: binding.stream_id.clone(),
        generation: binding.generation,
    }
}
fn normalized_observation(value: &Value) -> ModuleResult<Value> {
    if !value.is_object()
        || serde_json::to_vec(value)
            .map_err(|_| "invalid_native_observation")?
            .len()
            > 8192
    {
        return Err("invalid_native_observation".into());
    }
    // Platform selectors and native evidence belong to the selected provider.
    // Preserve its complete observation; the coordinator only compares the
    // common negotiated mode and enforces the exact binding and caller.
    Ok(value.clone())
}

impl Coordinator {
    pub(super) fn verify_provider(&self, caller: &str, payload: &Value) -> Value {
        let journal = &self.shared.borrow().value;
        for receipt in journal.receipts.values() {
            if receipt.response["status"] != "pending" {
                continue;
            }
            let Some(work) = &receipt.work else {
                continue;
            };
            if payload["host_epoch"] != journal.host_epoch
                && work.recovery_authorized_epoch.as_deref() != Some(journal.host_epoch.as_str())
            {
                continue;
            }
            if work.waiting_retirement {
                continue;
            }
            let Some(state) = work.states.get(caller) else {
                continue;
            };
            let request = &state.request;
            let Ok((target_epoch, session_id, stream_id, _)) =
                super::operations::target_parts(&request.target)
            else {
                continue;
            };
            if payload["host_epoch"].as_str() != Some(target_epoch) {
                continue;
            }
            let Some(session) = journal.sessions.get(session_id) else {
                continue;
            };
            let phase_allowed = match request.phase {
                LifecyclePhase::SessionPrepare => session.phase == SessionPhase::Preparing,
                LifecyclePhase::StreamPrepare => stream_id
                    .and_then(|id| session.streams.get(id))
                    .is_some_and(|s| s.phase == StreamPhase::Preparing),
                LifecyclePhase::StreamRelease => stream_id
                    .and_then(|id| session.streams.get(id))
                    .is_some_and(|s| {
                        s.phase == StreamPhase::Quiescing && s.retirements == s.required
                    }),
                LifecyclePhase::SessionRelease => {
                    session.phase == SessionPhase::Ending
                        && session.application_retired
                        && session
                            .streams
                            .values()
                            .all(|s| s.phase == StreamPhase::Retired)
                }
            };
            if !phase_allowed {
                continue;
            }
            if payload["provider_sequence"] != request.provider_sequence
                || payload["operation_id"] != request.operation_id
                || payload["transition_id"] != request.transition_id
                || payload["target"] != json!(request.target)
                || fingerprint(request).ok().as_deref() != payload["fingerprint"].as_str()
            {
                continue;
            }
            if request.operation == amaterasu_extension_api::ProviderOperation::LifecycleCallback
                && request
                    .admission_id
                    .as_ref()
                    .is_none_or(|id| !journal.authority.admissions.contains_key(id))
            {
                continue;
            }
            let mut result = payload.clone();
            result["status"] = json!("succeeded");
            result["session_target"] = json!({"kind":"session","host_epoch":session.host_epoch,"session_id":session.id,"generation":session.generation});
            result["consumers_retired"] = json!(session.streams.iter().all(|(id, stream)| {
                (stream_id == Some(id.as_str()) && stream.phase == StreamPhase::Preparing)
                    || stream.phase == StreamPhase::Retired
                    || (stream.phase == StreamPhase::Quiescing
                        && stream.retirements == stream.required)
            }));
            return result;
        }
        rejection("unknown_provider_admission")
    }

    pub(super) fn admit_binding_validation(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: Value,
        key: String,
        target: LifecycleTarget,
        ack: bool,
    ) -> ModuleResult<Value> {
        let mut next = self.shared.borrow().value.clone();
        let (session, stream_id) =
            super::operations::checked_target(&next, &target, caller, initiator)?;
        let stream = session
            .streams
            .get(stream_id.as_deref().ok_or("stream_target_required")?)
            .unwrap();
        if (ack && stream.phase != StreamPhase::Ready)
            || (!ack && stream.phase != StreamPhase::Active)
        {
            return Err("stream_not_ready".into());
        }
        let binding = stream
            .candidate
            .as_ref()
            .ok_or("candidate_missing")?
            .clone();
        let candidate_field = if ack { "candidate" } else { "binding" };
        if payload.get(candidate_field) != Some(&json!(binding))
            || payload["plan_revision"] != binding.plan_revision
        {
            return Err("candidate_mismatch".into());
        }
        let consumer = if ack {
            Some(
                payload["consumer"]
                    .as_str()
                    .ok_or("consumer_required")?
                    .to_owned(),
            )
        } else {
            None
        };
        let input_ack = consumer.as_deref() == Some("input");
        if consumer
            .as_deref()
            .is_some_and(|kind| !matches!(kind, "capture" | "input"))
        {
            return Err("unknown_consumer".into());
        }
        if if input_ack {
            !self.settings.consumers.input.contains(caller)
        } else {
            !self.settings.consumers.capture.contains(caller)
        } {
            return Err("permission_denied".into());
        }
        let input_token = if input_ack {
            Some(
                next.input_bindings
                    .iter()
                    .find(|(_, b)| target_for(b) == target)
                    .map(|(token, _)| token.clone())
                    .ok_or("input_binding_not_issued")?,
            )
        } else {
            None
        };
        let observation = if input_ack {
            Value::Null
        } else {
            let raw = if ack {
                &payload["observed_capture"]
            } else {
                &payload["observation"]
            };
            let observed = normalized_observation(raw)?;
            if observed["width"] != binding.mode.width
                || observed["height"] != binding.mode.height
                || observed["refresh_millihz"] != binding.mode.refresh_millihz
            {
                return Err("observed_mode_mismatch".into());
            }
            observed
        };
        let provider = stream.binding_owner.clone();
        let response = json!({"status":"pending","operation_id":payload["operation_id"],"target":target,"plan_revision":binding.plan_revision});
        if self.validations.len() >= 128 {
            return Err("validation_capacity_exhausted".into());
        }
        let receipt = Receipt {
            caller: caller.into(),
            initiator: initiator.into(),
            request: payload,
            response: response.clone(),
            work: None,
            wait_target: None,
        };
        if ack {
            next.receipts.insert(key.clone(), receipt);
            save(&self.shared, next)?;
        } else {
            if self.ephemeral.len() >= 512 {
                let oldest = self
                    .ephemeral
                    .iter()
                    .filter(|(_, (r, _))| r.response["status"] != "pending")
                    .min_by_key(|(_, (_, at))| *at)
                    .map(|(k, _)| k.clone());
                if let Some(oldest) = oldest {
                    self.ephemeral.remove(&oldest);
                } else {
                    return Err("validation_capacity_exhausted".into());
                }
            }
            self.ephemeral
                .insert(key.clone(), (receipt, Instant::now()));
        }
        self.validations.insert(
            key.clone(),
            ValidationJob {
                key,
                target,
                binding,
                observation,
                consumer,
                provider,
                request: None,
                result: None,
                operation_id: identifier()?,
                deadline: Instant::now() + Duration::from_secs(5),
                durable: ack,
                input_token,
            },
        );
        Ok(response)
    }

    pub(super) fn advance_validations(
        &mut self,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        self.advance_revocations(send)?;
        let keys: Vec<_> = self.validations.keys().take(8).cloned().collect();
        for key in keys {
            let mut job = self.validations.remove(&key).unwrap();
            if Instant::now() >= job.deadline {
                if let Some(id) = job.request {
                    let _ = send(Command::Cancel { request: id });
                    let _ = send(Command::ReleaseResult { request: id });
                }
                self.finish_validation(&job, None)?;
                continue;
            }
            if let Some(result) = job.result.take() {
                if let Some(id) = job.request
                    && !matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    )
                {
                    job.result = Some(result);
                    self.validations.insert(key, job);
                    continue;
                }
                self.finish_validation(&job, Some(result))?;
                continue;
            }
            if let Some(id) = job.request {
                match send(Command::TakeResult { request: id }) {
                    Ok(Response::Pending) => {}
                    Ok(Response::Result { payload }) => job.result = Some(payload),
                    _ => {
                        let _ = send(Command::ReleaseResult { request: id });
                        self.finish_validation(&job, None)?;
                        continue;
                    }
                }
            } else {
                if let Some(token) = &job.input_token {
                    match send(Command::Request {
                        service: "amaterasu.input/1".into(),
                        payload: json!({"action":"binding_status","operation_id":job.operation_id,"token":token,"target":job.target}),
                        timeout_ms: 5000,
                        operation_id: format!("input-status-{}", job.operation_id),
                        parent: None,
                    }) {
                        Ok(Response::Accepted { id }) => job.request = Some(id),
                        _ => {
                            self.finish_validation(&job, None)?;
                            continue;
                        }
                    }
                    self.validations.insert(key, job);
                    continue;
                }
                let Some(selection) = self.selections.get(&job.provider) else {
                    self.validations.insert(key, job);
                    continue;
                };
                let Some(method) = &selection.participant.binding_validate_method else {
                    self.finish_validation(&job, None)?;
                    continue;
                };
                let (_, session, stream, _) = super::operations::target_parts(&job.target)?;
                let transition = self.shared.borrow().value.sessions[session].streams
                    [stream.unwrap()]
                .preparation_transition
                .clone();
                let payload = json!({"operation_id":job.operation_id,"transition_id":transition,"target":job.target,"plan_revision":job.binding.plan_revision,"borrow_id":job.binding.resource.incarnation,"binding":job.binding,"observation":job.observation});
                match send(Command::CallApi {
                    service: method.service.clone(),
                    method: method.method.clone(),
                    interface_major: method.interface_major,
                    interface_minor: method.interface_minor,
                    payload,
                    timeout_ms: 5000,
                    operation_id: format!("binding-{}", job.operation_id),
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => job.request = Some(id),
                    _ => {
                        self.finish_validation(&job, None)?;
                        continue;
                    }
                }
            }
            self.validations.insert(key, job);
        }
        Ok(())
    }

    fn finish_validation(
        &mut self,
        job: &ValidationJob,
        result: Option<Value>,
    ) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let (_, session_id, stream_id, generation) = super::operations::target_parts(&job.target)?;
        let stream = next
            .sessions
            .get_mut(session_id)
            .and_then(|s| s.streams.get_mut(stream_id.unwrap()))
            .ok_or("unknown_stream")?;
        let valid = result.as_ref().is_some_and(|r| {
            if let Some(token) = &job.input_token {
                r["ok"] == true
                    && r["status"] == "bound"
                    && r["token"] == *token
                    && r["target"] == json!(job.target)
                    && r["geometry_revision"] == job.binding.geometry_revision
                    && r["lease"].as_str().is_some_and(|s| !s.is_empty())
            } else {
                r["status"] == "succeeded"
                    && r["target"] == json!(job.target)
                    && r["binding"] == json!(job.binding)
                    && r["observation"] == job.observation
                    && r["plan_revision"] == job.binding.plan_revision
            }
        }) && stream.generation == generation
            && stream.candidate.as_ref() == Some(&job.binding)
            && if job.consumer.is_some() {
                stream.phase == StreamPhase::Ready
            } else {
                stream.phase == StreamPhase::Active
            };
        if valid && let Some(consumer) = &job.consumer {
            stream.acknowledgements.insert(consumer.clone());
        }
        let receipt = if job.durable {
            next.receipts
                .get_mut(&job.key)
                .ok_or("unknown_validation")?
        } else {
            &mut self
                .ephemeral
                .get_mut(&job.key)
                .ok_or("unknown_validation")?
                .0
        };
        receipt.response = if valid {
            json!({"status":"succeeded","operation_id":receipt.request["operation_id"],"target":job.target,"plan_revision":job.binding.plan_revision,"binding":job.binding,"observation":job.observation})
        } else {
            json!({"status":"rejected","operation_id":receipt.request["operation_id"],"reason":"binding_validation_failed"})
        };
        if job.durable {
            save(&self.shared, next)
        } else {
            Ok(())
        }
    }

    pub(super) fn fence_stream(
        &mut self,
        next: &mut Journal,
        target: &LifecycleTarget,
    ) -> ModuleResult<()> {
        self.ephemeral.retain(|_, (receipt, _)| {
            receipt.request["target"] != json!(target) || receipt.response["status"] == "pending"
        });
        let bindings: Vec<_> = next
            .input_bindings
            .iter()
            .filter(|(_, b)| target_for(b) == *target)
            .map(|(t, b)| (t.clone(), b.clone()))
            .collect();
        let no_input = bindings.is_empty();
        for (token, binding) in bindings {
            next.inputs.fence(&binding);
            next.revoked_inputs.insert(token);
        }
        let (epoch, session, stream, generation) = super::operations::target_parts(target)?;
        let action_target = crate::action_authority::ActionTarget::Stream {
            host_epoch: epoch.into(),
            session_id: session.into(),
            stream_id: stream.ok_or("stream_target_required")?.into(),
            expected_generation: generation,
        };
        next.actions
            .fence_target(&action_target)
            .map_err(|_| "action_fence_failed")?;
        if no_input
            && let Some(record) = next
                .sessions
                .get_mut(session)
                .and_then(|s| s.streams.get_mut(stream.unwrap()))
            && record.required.contains("input")
        {
            record.retirements.insert("input".into());
        }
        Ok(())
    }

    fn advance_revocations(
        &mut self,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        let tokens: Vec<_> = self
            .shared
            .borrow()
            .value
            .revoked_inputs
            .iter()
            .take(8)
            .cloned()
            .collect();
        for token in tokens {
            let binding = self
                .shared
                .borrow()
                .value
                .input_bindings
                .get(&token)
                .cloned();
            let Some(binding) = binding else {
                continue;
            };
            let target = target_for(&binding);
            let transfer = {
                let shared = self.shared.borrow();
                shared.value.receipts.values().find_map(|receipt| {
                    let work = receipt.work.as_ref()?;
                    let transfer = work.action_transfer.as_ref()?;
                    if work.target != target || receipt.response["status"] != "pending" {
                        return None;
                    }
                    let reservation_id = transfer.input_reservation_id.as_ref()?;
                    let reservation = shared.value.inputs.resolve_running_reservation(reservation_id).ok()?;
                    (reservation.token == token).then(|| json!({"operation_id":transfer.binding.operation_id,"reservation_id":reservation_id}))
                })
            };
            let job = self
                .revocations
                .entry(token.clone())
                .or_insert(RevocationJob {
                    request: None,
                    result: None,
                    operation_id: identifier()?,
                });
            if let Some(result) = job.result.take() {
                if let Some(id) = job.request
                    && !matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    )
                {
                    job.result = Some(result);
                    continue;
                }
                if result["status"] == "retired"
                    && result["token"] == token
                    && result["target"] == json!(target)
                {
                    let mut next = self.shared.borrow().value.clone();
                    if let Some(stream) = next
                        .sessions
                        .get_mut(&binding.session_id)
                        .and_then(|s| s.streams.get_mut(&binding.stream_id))
                    {
                        stream.retirements.insert("input".into());
                    }
                    next.input_bindings.remove(&token);
                    next.input_boot_epochs.remove(&token);
                    next.revoked_inputs.remove(&token);
                    save(&self.shared, next)?;
                    self.revocations.remove(&token);
                } else {
                    self.fail_revocation(&token)?;
                }
            } else if let Some(id) = job.request {
                match send(Command::TakeResult { request: id }) {
                    Ok(Response::Pending) => {}
                    Ok(Response::Result { payload }) => job.result = Some(payload),
                    _ => self.fail_revocation(&token)?,
                }
            } else {
                let response = send(Command::Request {
                    service: "amaterasu.input/1".into(),
                    payload: json!({"action":"revoke_binding","operation_id":job.operation_id,"token":token,"target":target,"transfer":transfer}),
                    timeout_ms: 5000,
                    operation_id: format!("revoke-{}", job.operation_id),
                    parent: None,
                });
                if let Ok(Response::Accepted { id }) = response {
                    job.request = Some(id);
                } else {
                    self.fail_revocation(&token)?;
                }
            }
        }
        Ok(())
    }

    fn fail_revocation(&mut self, token: &str) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        if let Some(binding) = next.input_bindings.get(token) {
            if let Some(stream) = next
                .sessions
                .get_mut(&binding.session_id)
                .and_then(|session| session.streams.get_mut(&binding.stream_id))
            {
                stream.phase = StreamPhase::RecoveryRequired;
            }
            let target = target_for(binding);
            for receipt in next.receipts.values_mut() {
                if receipt.request["target"] == json!(target) {
                    receipt.response["status"] = json!("recovery_required");
                    receipt.response["reason"] = json!("input_retirement_unconfirmed");
                }
            }
        }
        next.revoked_inputs.remove(token);
        save(&self.shared, next)?;
        self.revocations.remove(token);
        Ok(())
    }

    pub(super) fn security_operation(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: &Value,
    ) -> Value {
        if matches!(
            payload["operation"].as_str(),
            Some(
                "authorize_action"
                    | "consume_action_permit"
                    | "complete_action"
                    | "begin_action_transition"
                    | "consume_input_reservation"
                    | "verify_action_effect"
                    | "check_action_effect"
            )
        ) {
            return self.action_operation(caller, initiator, payload);
        }
        match self.input_operation(caller, initiator, payload) {
            Ok(value) => value,
            Err(reason) => rejection(&reason),
        }
    }

    fn input_operation(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: &Value,
    ) -> ModuleResult<Value> {
        let mut next = self.shared.borrow().value.clone();
        let operation = payload["operation"].as_str().ok_or("invalid_request")?;
        // Existing reservations must be able to drain after their generation is
        // fenced. Revocation closes new admission, not completion of owned work.
        if operation == "finish_input_action" || operation == "lookup_input_action" {
            let id = payload["operation_id"]
                .as_str()
                .ok_or("operation_id_required")?;
            if operation == "lookup_input_action" {
                return serde_json::to_value(
                    next.inputs
                        .lookup_input_action(caller, initiator, id)
                        .map_err(|_| "unknown_reservation")?,
                )
                .map_err(|e| e.to_string());
            }
            let outcome = if payload["outcome"] == "completed" {
                Outcome::Completed
            } else if payload["outcome"] == "cancelled" {
                Outcome::Cancelled
            } else {
                Outcome::RecoveryRequired
            };
            let reservation_id = payload["reservation_id"]
                .as_str()
                .ok_or("reservation_required")?;
            let stored = next
                .inputs
                .resolve_reservation(reservation_id)
                .map_err(|_| "unknown_reservation")?;
            if stored.caller != caller || stored.operation_id != id {
                return Err("permission_denied".into());
            }
            let result = next
                .inputs
                .finish_input_action(caller, &stored.initiator, id, reservation_id, outcome)
                .map_err(|_| "input_reservation_rejected")?;
            save(&self.shared, next)?;
            return Ok(json!({"status":"succeeded","reservation_id":result.reservation_id}));
        }
        if operation == "issue_input_binding" {
            let id = payload["operation_id"]
                .as_str()
                .filter(|id| valid_id(id))
                .ok_or("operation_id_required")?;
            let key = receipt_key(caller, initiator, id);
            if let Some(old) = next.receipts.get(&key) {
                return if old.request == *payload {
                    Ok(old.response.clone())
                } else {
                    Err("operation_id_conflict".into())
                };
            }
            let target: LifecycleTarget =
                serde_json::from_value(payload["target"].clone()).map_err(|_| "invalid_target")?;
            let (session, stream_id) =
                super::operations::checked_target(&next, &target, caller, initiator)?;
            let stream_id = stream_id.ok_or("stream_target_required")?;
            let stream = &session.streams[&stream_id];
            if !matches!(stream.phase, StreamPhase::Ready | StreamPhase::Active)
                || !stream.acknowledgements.contains("capture")
            {
                return Err("capture_not_acknowledged".into());
            }
            let binding = stream.candidate.as_ref().ok_or("candidate_missing")?;
            let plan_revision = binding.plan_revision;
            let active = ActiveBinding {
                host_epoch: session.host_epoch.clone(),
                session_id: session.id.clone(),
                stream_id,
                generation: stream.generation,
                geometry_revision: binding.geometry_revision,
                input_consumer: "amaterasu.input".into(),
                allowed_actions: self.settings.input_actions.clone(),
            };
            let token = next
                .inputs
                .issue_input_binding(&active)
                .map_err(|_| "input_binding_unavailable")?;
            next.input_bindings.insert(token.clone(), active.clone());
            let result = json!({"status":"succeeded","operation_id":id,"token":token,"target":target,"geometry_revision":active.geometry_revision,"plan_revision":plan_revision});
            next.receipts.insert(
                key,
                Receipt {
                    caller: caller.into(),
                    initiator: initiator.into(),
                    request: payload.clone(),
                    response: result.clone(),
                    work: None,
                    wait_target: None,
                },
            );
            save(&self.shared, next)?;
            return Ok(result);
        }
        let token = payload["token"].as_str().ok_or("binding_token_required")?;
        if next.revoked_inputs.contains(token) {
            return Err("binding_revoked".into());
        }
        let binding = next
            .input_bindings
            .get(token)
            .cloned()
            .ok_or("unknown_input_binding")?;
        let session = next
            .sessions
            .get(&binding.session_id)
            .ok_or("unknown_session")?;
        let stream = session
            .streams
            .get(&binding.stream_id)
            .ok_or("unknown_stream")?;
        if binding.host_epoch != next.host_epoch
            || binding.generation != stream.generation
            || !matches!(stream.phase, StreamPhase::Ready | StreamPhase::Active)
        {
            return Err("stale_input_binding".into());
        }
        if operation == "validate_input_binding" {
            next.inputs
                .validate_input_binding(caller, token, &binding)
                .map_err(|_| "permission_denied")?;
            let response = json!({"status":"succeeded","active":stream.phase==StreamPhase::Active,"target":target_for(&binding),"action_target":{"kind":"session","host_epoch":session.host_epoch,"session_id":session.id,"expected_generation":session.generation},"context":"active","application":session.application_ref["app_id"],"paired_client":null,"geometry_revision":binding.geometry_revision,"batch_epoch":format!("{}.{}.{}",binding.host_epoch,binding.stream_id,binding.generation),"allowed_actions":binding.allowed_actions});
            let boot_epoch = payload["input_boot_epoch"]
                .as_str()
                .filter(|epoch| valid_id(epoch))
                .or_else(|| (!self.configured).then_some("unit-test-input"))
                .ok_or("input_boot_epoch_required")?;
            if let Some(previous) = next.input_boot_epochs.get(token) {
                if previous != boot_epoch {
                    return Err("input_provider_restarted".into());
                }
            } else {
                next.input_boot_epochs
                    .insert(token.into(), boot_epoch.into());
                save(&self.shared, next)?;
            }
            return Ok(response);
        }
        if stream.phase != StreamPhase::Active {
            return Err("stream_not_active".into());
        }
        let id = payload["operation_id"]
            .as_str()
            .ok_or("operation_id_required")?;
        let result = match operation {
            "reserve_input_action" => {
                let action = payload["action_id"].as_str().ok_or("action_required")?;
                let reservation = next
                    .inputs
                    .reserve_input_action(caller, initiator, id, token, action, &binding)
                    .map_err(|_| "input_reservation_rejected")?;
                let (ReservationResult::Admitted(r) | ReservationResult::Existing(r)) = reservation;
                json!({"status":"reserved","operation_id":id,"reservation_id":r.reservation_id,"target":{"kind":"session","host_epoch":session.host_epoch,"session_id":session.id,"expected_generation":session.generation},"action_id":action})
            }
            "lookup_input_action" => serde_json::to_value(
                next.inputs
                    .lookup_input_action(caller, initiator, id)
                    .map_err(|_| "unknown_reservation")?,
            )
            .map_err(|e| e.to_string())?,
            "finish_input_action" => {
                let outcome = if payload["outcome"] == "completed" {
                    Outcome::Completed
                } else if payload["outcome"] == "cancelled" {
                    Outcome::Cancelled
                } else {
                    Outcome::RecoveryRequired
                };
                let r = next
                    .inputs
                    .finish_input_action(
                        caller,
                        initiator,
                        id,
                        payload["reservation_id"]
                            .as_str()
                            .ok_or("reservation_required")?,
                        outcome,
                    )
                    .map_err(|_| "input_reservation_rejected")?;
                json!({"status":"succeeded","reservation_id":r.reservation_id})
            }
            _ => return Err("unsupported_operation".into()),
        };
        save(&self.shared, next)?;
        Ok(result)
    }
}
