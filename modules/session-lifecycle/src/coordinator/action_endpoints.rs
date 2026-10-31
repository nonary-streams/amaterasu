//! Action service bridge. Only broker-authenticated installed service may attest
//! its verified registry descriptor and provider plan; payload principals do not
//! themselves establish ownership.
use super::{Coordinator, SessionPhase, StreamPhase, rejection, save};
use crate::action_authority::{
    ActionBinding, ActionOutcome, ActionTarget, CanonicalAction, Principal,
};
use crate::plan_validation::{AdmissionContext, validate_effect_plan};
use amaterasu_extension_api::{ConsumerChange, ResourcePlan};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fmt::Write,
    time::{SystemTime, UNIX_EPOCH},
};

struct ActionFinishWriter {
    shared: super::Shared,
    next: Option<super::Journal>,
}
impl crate::prepare_authority::AuthorityJournal for ActionFinishWriter {
    fn persist(
        &mut self,
        state: &crate::prepare_authority::AuthorityState,
    ) -> amaterasu_module_sdk::ModuleResult<()> {
        let mut next = self.next.take().ok_or("duplicate_finish")?;
        next.authority = state.clone();
        save(&self.shared, next)
    }
}

fn timestamp() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|v| v.as_secs())
}
fn digest(plan: &Value) -> Option<String> {
    let bytes = serde_json::to_vec(plan).ok()?;
    if bytes.len() > 4_000 {
        return None;
    }
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(&mut output, "{byte:02x}").ok()?;
    }
    Some(output)
}
fn binding_matches(payload: &Value, binding: &ActionBinding) -> bool {
    payload["operation_id"].as_str() == Some(&binding.operation_id)
        && payload["action_id"].as_str() == Some(&binding.action_id)
        && payload["plan_digest"].as_str() == Some(&binding.plan_digest)
        && serde_json::from_value::<ActionTarget>(payload["target"].clone())
            .ok()
            .as_ref()
            == Some(&binding.target)
}

impl Coordinator {
    /// Admit action operations under the configured service and exact target.
    pub(super) fn action_operation(
        &mut self,
        caller: &str,
        _initiator: &str,
        payload: &Value,
    ) -> Value {
        let operation = payload["operation"].as_str().unwrap_or("");
        if operation == "verify_action_effect" || operation == "check_action_effect" {
            let mut journal = self.shared.borrow().value.clone();
            let Some(token) = payload["permit"].as_str() else {
                return rejection("invalid_permit");
            };
            let Ok(binding) = journal.actions.binding_for_provider(caller, token) else {
                return rejection("permission_denied");
            };
            if !binding_matches(payload, &binding) || payload["provider"].as_str() != Some(caller) {
                return rejection("action_binding_mismatch");
            }
            let Ok(method) = serde_json::from_value::<amaterasu_extension_api::MethodRef>(
                payload["execute_method"].clone(),
            ) else {
                return rejection("invalid_method");
            };
            if !effect_rights_current(&journal, &self.settings.action_service, token, &binding) {
                return rejection("action_effect_not_authorized");
            }
            let claim = if operation == "verify_action_effect" {
                journal
                    .actions
                    .claim_effect(caller, token, &binding, &method)
            } else {
                let Some(nonce) = payload["guard_nonce"].as_str() else {
                    return rejection("guard_nonce_required");
                };
                journal
                    .actions
                    .check_effect(caller, token, &binding, &method, nonce)
            };
            if claim.is_err() {
                return rejection("action_effect_not_authorized");
            }
            if save(&self.shared, journal).is_err() {
                return json!({"status":"recovery_required", "reason":"journal_write_failed"});
            }
            let mut response = json!({"status":"succeeded", "operation_id":binding.operation_id, "action_id":binding.action_id, "target":binding.target, "plan_digest":binding.plan_digest, "provider":caller, "execute_method":method});
            if operation == "check_action_effect" {
                response["guard_nonce"] = payload["guard_nonce"].clone();
            }
            return response;
        }
        if caller != self.settings.action_service {
            return rejection("permission_denied");
        }
        let mut journal = self.shared.borrow().value.clone();
        match operation {
            "cancel_input_reservation" => {
                let (Some(operation_id), Some(action_id), Some(reservation_id)) = (
                    payload["operation_id"].as_str(),
                    payload["action_id"].as_str(),
                    payload["reservation_id"].as_str(),
                ) else {
                    return rejection("invalid_reservation");
                };
                if journal
                    .actions
                    .has_operation(caller, operation_id, action_id)
                {
                    return rejection("action_outcome_unresolved");
                }
                if journal
                    .inputs
                    .cancel_transferred_reservation(reservation_id, caller, operation_id, action_id)
                    .is_err()
                {
                    return rejection("reservation_not_cancelled");
                }
                if save(&self.shared, journal).is_err() {
                    return json!({"status":"recovery_required","reason":"journal_write_failed"});
                }
                json!({"status":"succeeded","operation_id":operation_id,"action_id":action_id,"reservation_id":reservation_id})
            }
            "consume_input_reservation" => {
                let Some(reservation_id) = payload["reservation_id"].as_str() else {
                    return rejection("invalid_reservation");
                };
                let (Some(operation_id), Some(action_id)) = (
                    payload["operation_id"].as_str(),
                    payload["action_id"].as_str(),
                ) else {
                    return rejection("invalid_action");
                };
                let Ok(target) = serde_json::from_value::<ActionTarget>(payload["target"].clone())
                else {
                    return rejection("invalid_target");
                };
                let Some((canonical, owner)) = canonical_target(&journal, &target) else {
                    return rejection("target_not_active");
                };
                let Ok(reservation) = journal.inputs.resolve_running_reservation(reservation_id)
                else {
                    return rejection("unknown_reservation");
                };
                let target_session = match &canonical {
                    ActionTarget::Session {
                        session_id,
                        host_epoch,
                        ..
                    }
                    | ActionTarget::Stream {
                        session_id,
                        host_epoch,
                        ..
                    } => (session_id, host_epoch),
                    ActionTarget::Host { .. } => return rejection("invalid_target"),
                };
                if reservation.binding.session_id != *target_session.0
                    || reservation.binding.host_epoch != *target_session.1
                    || reservation.operation_id != operation_id
                    || reservation.action_id != action_id
                {
                    return rejection("reservation_mismatch");
                }
                if let ActionTarget::Stream {
                    stream_id,
                    expected_generation,
                    ..
                } = &canonical
                    && (reservation.binding.stream_id != *stream_id
                        || reservation.binding.generation != *expected_generation)
                {
                    return rejection("reservation_mismatch");
                }
                if journal
                    .inputs
                    .transfer_reservation(reservation_id, caller, operation_id, action_id)
                    .is_err()
                {
                    return rejection("reservation_not_authorized");
                }
                if save(&self.shared, journal).is_err() {
                    return json!({"status":"recovery_required","reason":"journal_write_failed"});
                }
                json!({"status":"succeeded","operation_id":operation_id,"action_id":action_id,"target":canonical,"reservation_id":reservation_id,"source":owner})
            }
            "authorize_action" => {
                let Some(source) =
                    serde_json::from_value::<Principal>(payload["source"].clone()).ok()
                else {
                    return rejection("invalid_source");
                };
                let Some(target) =
                    serde_json::from_value::<ActionTarget>(payload["target"].clone()).ok()
                else {
                    return rejection("invalid_target");
                };
                let (Some(operation_id), Some(action_id), Some(plan_digest)) = (
                    payload["operation_id"].as_str(),
                    payload["action_id"].as_str(),
                    payload["plan_digest"].as_str(),
                ) else {
                    return rejection("invalid_action");
                };
                if digest(&payload["plan"]).as_deref() != Some(plan_digest) {
                    return rejection("plan_digest_mismatch");
                }
                let Ok(plan) = serde_json::from_value::<ResourcePlan>(payload["plan"].clone())
                else {
                    return rejection("invalid_plan");
                };
                let Some((canonical_target, owner)) = canonical_target(&journal, &target) else {
                    return rejection("target_not_active");
                };
                if source != owner {
                    return rejection("permission_denied");
                }
                if plan.candidate_constraint.is_some() || !plan.allowed_degradation.is_empty() {
                    return rejection("action_transition_unsupported");
                }
                let mutating = !plan.resources.is_empty()
                    || !plan.mutation_domains.is_empty()
                    || !plan.affected_consumers.is_empty()
                    || !plan.obligations.is_empty()
                    || plan.consumer_change != ConsumerChange::Live;
                let mut selection = None;
                let mut method = None;
                if mutating {
                    let Some(provider) = payload["provider"].as_str() else {
                        return rejection("provider_required");
                    };
                    let Some(installed) = self.selections.get(provider) else {
                        return rejection("provider_unavailable");
                    };
                    if plan
                        .resources
                        .iter()
                        .any(|resource| resource.provider != provider)
                        || plan.mutation_domains.iter().any(|domain| {
                            journal.authority.domain_locks.contains_key(&domain.domain)
                        })
                    {
                        return rejection("action_domain_unavailable");
                    }
                    let Ok(selected_method) =
                        serde_json::from_value::<amaterasu_extension_api::MethodRef>(
                            payload["execute_method"].clone(),
                        )
                    else {
                        return rejection("execute_method_required");
                    };
                    let Some(actual_revision) =
                        active_provider_revision(&journal, &target, provider)
                    else {
                        return rejection("provider_settings_snapshot_missing");
                    };
                    if plan
                        .constraints
                        .get("selected_settings_revision")
                        .and_then(Value::as_str)
                        != Some(actual_revision)
                    {
                        return rejection("provider_settings_revision_mismatch");
                    }
                    selection = Some(installed);
                    method = Some(selected_method);
                }
                let approved: BTreeSet<String> = selection
                    .map(|selected| selected.capabilities.clone())
                    .unwrap_or_else(|| {
                        self.selections
                            .values()
                            .flat_map(|selected| selected.capabilities.iter().cloned())
                            .collect()
                    });
                let cleanup = selection
                    .map(|selected| selected.cleanup_methods.as_slice())
                    .unwrap_or(&[]);
                let mut borrowers = BTreeSet::new();
                match &target {
                    ActionTarget::Session { session_id, .. }
                    | ActionTarget::Stream { session_id, .. } => {
                        if let Some(session) = journal.sessions.get(session_id)
                            && mutating
                        {
                            let active: Vec<_> = session
                                .streams
                                .iter()
                                .filter(|(_, stream)| stream.phase == StreamPhase::Active)
                                .map(|(id, _)| id.clone())
                                .collect();
                            if active.len() != 1
                                || session.streams.iter().any(|(id, stream)| {
                                    id != &active[0] && stream.phase != StreamPhase::Retired
                                })
                            {
                                return rejection("action_target_ambiguous");
                            }
                            if let ActionTarget::Stream { stream_id, .. } = &target
                                && stream_id != &active[0]
                            {
                                return rejection("action_target_ambiguous");
                            }
                            borrowers.insert(active[0].clone());
                        }
                    }
                    ActionTarget::Host { .. } => return rejection("target_not_active"),
                }
                let context = AdmissionContext {
                    snapshot: &journal.authority.snapshot,
                    complete: true,
                    approved_capabilities: &approved,
                    approved_cleanup_methods: cleanup,
                    authorized_borrowers: &borrowers,
                };
                if validate_effect_plan(&plan, &context).is_err() {
                    return rejection("plan_not_admitted");
                }
                let binding = ActionBinding {
                    operation_id: operation_id.into(),
                    target: target.clone(),
                    action_id: action_id.into(),
                    plan_digest: plan_digest.into(),
                    source,
                };
                let canonical = CanonicalAction {
                    action_service: &self.settings.action_service,
                    target: &canonical_target,
                    owner: &owner,
                    action_id,
                    plan_digest,
                    active: true,
                };
                let Some(now) = timestamp() else {
                    return rejection("clock_unavailable");
                };
                let Some(expiry) = now.checked_add(30) else {
                    return rejection("clock_unavailable");
                };
                let Ok(token) = journal
                    .actions
                    .authorize_action(caller, &binding, &canonical, now, expiry)
                else {
                    return rejection("action_not_authorized");
                };
                if let (Some(selected), Some(method)) = (selection, method)
                    && journal
                        .actions
                        .bind_execution(
                            caller,
                            &token,
                            &binding,
                            &selected.participant.provider,
                            &method,
                            &plan,
                        )
                        .is_err()
                {
                    return rejection("action_execution_invalid");
                }
                if save(&self.shared, journal).is_err() {
                    return json!({"status":"recovery_required","reason":"journal_write_failed"});
                }
                json!({"status":"succeeded","operation_id":operation_id,"target":target,"permit":token})
            }
            "begin_action_transition" => {
                let Some(token) = payload["permit"].as_str() else {
                    return rejection("invalid_permit");
                };
                let Ok(binding) = journal.actions.binding_for_permit(caller, token) else {
                    return rejection("invalid_permit");
                };
                if !binding_matches(payload, &binding) {
                    return rejection("action_binding_mismatch");
                }
                let key = super::receipt_key(caller, "action-transition", &binding.operation_id);
                if let Some(receipt) = journal.receipts.get(&key) {
                    return if receipt.request == *payload {
                        receipt.response.clone()
                    } else {
                        rejection("operation_id_conflict")
                    };
                }
                let Ok((_provider, _method, _plan)) =
                    journal.actions.execution_for_permit(caller, token)
                else {
                    return rejection("action_execution_unavailable");
                };
                let Some((_, owner)) = canonical_target(&journal, &binding.target) else {
                    return rejection("target_not_active");
                };
                if owner != binding.source {
                    return rejection("permission_denied");
                }
                let session_id = match &binding.target {
                    ActionTarget::Session { session_id, .. }
                    | ActionTarget::Stream { session_id, .. } => session_id.clone(),
                    ActionTarget::Host { .. } => return rejection("invalid_target"),
                };
                let Some(session) = journal.sessions.get(&session_id) else {
                    return rejection("unknown_session");
                };
                let stream_id = match &binding.target {
                    ActionTarget::Stream { stream_id, .. } => stream_id.clone(),
                    ActionTarget::Session { .. } => {
                        let active: Vec<_> = session
                            .streams
                            .iter()
                            .filter(|(_, stream)| stream.phase == StreamPhase::Active)
                            .map(|(id, _)| id.clone())
                            .collect();
                        if active.len() != 1 {
                            return rejection("action_target_ambiguous");
                        }
                        active[0].clone()
                    }
                    ActionTarget::Host { .. } => return rejection("invalid_target"),
                };
                let stream = &session.streams[&stream_id];
                if stream.phase != StreamPhase::Active {
                    return rejection("target_not_active");
                }
                let target = amaterasu_extension_api::LifecycleTarget::Stream {
                    host_epoch: session.host_epoch.clone(),
                    session_id: session_id.clone(),
                    stream_id: stream_id.clone(),
                    generation: stream.generation,
                };
                let input_id = payload["input_reservation_id"].as_str().map(str::to_owned);
                if let Some(input_id) = &input_id {
                    let Ok(reservation) = journal.inputs.resolve_running_reservation(input_id)
                    else {
                        return rejection("unknown_reservation");
                    };
                    if reservation.transferred_to.as_deref() != Some(caller)
                        || reservation.operation_id != binding.operation_id
                        || reservation.action_id != binding.action_id
                        || reservation.binding.session_id != session_id
                        || reservation.binding.stream_id != stream_id
                        || reservation.binding.generation != stream.generation
                    {
                        return rejection("reservation_mismatch");
                    }
                }
                let Ok(transition_id) = super::identifier() else {
                    return rejection("entropy_unavailable");
                };
                if journal
                    .actions
                    .transfer_to_transition(caller, token, &binding, &transition_id)
                    .is_err()
                {
                    return rejection("action_not_running");
                }
                journal
                    .sessions
                    .get_mut(&session_id)
                    .unwrap()
                    .streams
                    .get_mut(&stream_id)
                    .unwrap()
                    .phase = StreamPhase::Quiescing;
                if self.fence_stream(&mut journal, &target).is_err() {
                    return rejection("fence_failed");
                }
                let response = json!({"status":"pending","operation_id":binding.operation_id,"target":binding.target,"plan_digest":binding.plan_digest});
                let work = super::Work {
                    phase: amaterasu_extension_api::LifecyclePhase::StreamRelease,
                    target,
                    intent: None,
                    reason: Some(amaterasu_extension_api::ReleaseReason::Reconfigure),
                    video: None,
                    transition_id,
                    providers: vec![],
                    index: 0,
                    states: Default::default(),
                    candidate: None,
                    waiting_retirement: true,
                    failure: None,
                    failed_preparation: None,
                    recovery_authorized_epoch: None,
                    action_transfer: Some(super::ActionTransfer {
                        service: caller.into(),
                        permit: token.into(),
                        binding,
                        input_reservation_id: input_id,
                        admission_id: None,
                    }),
                };
                journal.receipts.insert(
                    key,
                    super::Receipt {
                        caller: caller.into(),
                        initiator: "action-transition".into(),
                        request: payload.clone(),
                        response: response.clone(),
                        work: Some(work),
                        wait_target: None,
                    },
                );
                if save(&self.shared, journal).is_err() {
                    return json!({"status":"recovery_required","reason":"journal_write_failed"});
                }
                response
            }
            "consume_action_permit" | "complete_action" => {
                let Some(token) = payload["permit"].as_str() else {
                    return rejection("invalid_permit");
                };
                let Ok(binding) = journal.actions.binding_for_permit(caller, token) else {
                    return rejection("invalid_permit");
                };
                if !binding_matches(payload, &binding) {
                    return rejection("action_binding_mismatch");
                }
                if operation == "consume_action_permit" {
                    let Some((target, owner)) = canonical_target(&journal, &binding.target) else {
                        return rejection("target_not_active");
                    };
                    let canonical = CanonicalAction {
                        action_service: &self.settings.action_service,
                        target: &target,
                        owner: &owner,
                        action_id: &binding.action_id,
                        plan_digest: &binding.plan_digest,
                        active: true,
                    };
                    let Some(now) = timestamp() else {
                        return rejection("clock_unavailable");
                    };
                    if journal
                        .actions
                        .consume_action_permit(caller, &binding, &canonical, token, now)
                        .is_err()
                    {
                        return rejection("action_not_authorized");
                    }
                } else {
                    let outcome = match payload["outcome"].as_str() {
                        Some("succeeded") => ActionOutcome::Succeeded,
                        Some("failed" | "recovery_required") => ActionOutcome::RecoveryRequired,
                        _ => return rejection("invalid_outcome"),
                    };
                    let receipt = payload["provider_receipt"].clone();
                    let succeeded = outcome == ActionOutcome::Succeeded;
                    if journal
                        .actions
                        .complete_action(caller, &binding, outcome, receipt, false)
                        .is_err()
                    {
                        return rejection("action_completion_invalid");
                    }
                    let key =
                        super::receipt_key(caller, "action-transition", &binding.operation_id);
                    if let Some(work) = journal
                        .receipts
                        .get(&key)
                        .and_then(|record| record.work.clone())
                        && let Some(transfer) = &work.action_transfer
                        && let amaterasu_extension_api::LifecycleTarget::Stream {
                            session_id,
                            stream_id,
                            ..
                        } = &work.target
                    {
                        let Some(session) = journal.sessions.get_mut(session_id) else {
                            return rejection("unknown_session");
                        };
                        let Some(stream) = session.streams.get_mut(stream_id) else {
                            return rejection("unknown_stream");
                        };
                        stream.phase = if succeeded {
                            StreamPhase::Retired
                        } else {
                            StreamPhase::RecoveryRequired
                        };
                        if !succeeded {
                            session.phase = SessionPhase::RecoveryRequired;
                        } else if session.phase != SessionPhase::Ending
                            && session
                                .streams
                                .values()
                                .all(|stream| stream.phase == StreamPhase::Retired)
                        {
                            session.phase = SessionPhase::Paused;
                        }
                        if succeeded {
                            let Some(admission_id) = &transfer.admission_id else {
                                return rejection("action_admission_missing");
                            };
                            let Some(admission) = journal.authority.admissions.get(admission_id)
                            else {
                                return rejection("action_admission_missing");
                            };
                            for obligation in &admission.plan.obligations {
                                if let Some(existing) = session
                                    .obligations
                                    .iter()
                                    .find(|entry| entry.obligation_id == obligation.obligation_id)
                                {
                                    if existing != obligation {
                                        return rejection("cleanup_obligation_conflict");
                                    }
                                } else {
                                    session.obligations.push(obligation.clone());
                                }
                            }
                            let provider = admission.provider.clone();
                            journal.receipts.get_mut(&key).expect("work exists").work = None;
                            let writer = ActionFinishWriter {
                                shared: self.shared.clone(),
                                next: Some(journal.clone()),
                            };
                            let Ok(mut authority) =
                                crate::prepare_authority::CoordinatorPrepareAuthority::new(
                                    journal.authority.clone(),
                                    self.selections.values().cloned().collect(),
                                    writer,
                                )
                            else {
                                return rejection("action_authority_unavailable");
                            };
                            if authority.finish_admission(&provider, admission_id).is_err() {
                                return json!({"status":"recovery_required","reason":"action_admission_finish_failed"});
                            }
                            return json!({"status":"succeeded","operation_id":binding.operation_id,"target":binding.target,"plan_digest":binding.plan_digest});
                        }
                    }
                }
                if save(&self.shared, journal).is_err() {
                    return json!({"status":"recovery_required","reason":"journal_write_failed"});
                }
                json!({"status":"succeeded","operation_id":binding.operation_id,"target":binding.target,"plan_digest":binding.plan_digest})
            }
            _ => rejection("unsupported_operation"),
        }
    }
}

fn active_provider_revision<'a>(
    journal: &'a super::Journal,
    target: &ActionTarget,
    provider: &str,
) -> Option<&'a str> {
    let session_id = match target {
        ActionTarget::Session { session_id, .. } | ActionTarget::Stream { session_id, .. } => {
            session_id
        }
        ActionTarget::Host { .. } => return None,
    };
    let session = journal.sessions.get(session_id)?;
    let stream = match target {
        ActionTarget::Stream { stream_id, .. } => session.streams.get(stream_id)?,
        ActionTarget::Session { .. } => {
            let mut streams = session.streams.values().filter(|stream| {
                stream.phase == StreamPhase::Active && stream.binding_owner == provider
            });
            let stream = streams.next()?;
            if streams.next().is_some() {
                return None;
            }
            stream
        }
        ActionTarget::Host { .. } => return None,
    };
    if stream.phase != StreamPhase::Active || stream.binding_owner != provider {
        return None;
    }
    let revision = &stream.provider_settings.get(provider)?.revision;
    if !super::valid_id(revision) {
        return None;
    }
    Some(revision)
}

fn effect_rights_current(
    journal: &super::Journal,
    service: &str,
    token: &str,
    binding: &ActionBinding,
) -> bool {
    let key = super::receipt_key(service, "action-transition", &binding.operation_id);
    let Some(receipt) = journal.receipts.get(&key) else {
        return false;
    };
    if receipt.response["status"] != "succeeded" {
        return false;
    }
    let Some(work) = &receipt.work else {
        return false;
    };
    let Some(transfer) = &work.action_transfer else {
        return false;
    };
    if transfer.service != service || transfer.permit != token || transfer.binding != *binding {
        return false;
    }
    let Some(admission_id) = &transfer.admission_id else {
        return false;
    };
    let Some(admission) = journal.authority.admissions.get(admission_id) else {
        return false;
    };
    let Ok((provider, _, plan)) = journal.actions.execution_for_permit(service, token) else {
        return false;
    };
    if admission.provider != provider || admission.plan != plan {
        return false;
    }
    if admission
        .rights
        .iter()
        .any(|(domain, _)| journal.authority.domain_locks.get(domain) != Some(admission_id))
    {
        return false;
    }
    let amaterasu_extension_api::LifecycleTarget::Stream {
        host_epoch,
        session_id,
        stream_id,
        generation,
    } = &work.target
    else {
        return false;
    };
    if host_epoch != &journal.host_epoch {
        return false;
    }
    journal
        .sessions
        .get(session_id)
        .and_then(|session| session.streams.get(stream_id))
        .is_some_and(|stream| {
            stream.generation == *generation
                && stream.phase == StreamPhase::Quiescing
                && stream.retirements == stream.required
                && stream.binding_owner == provider
                && stream
                    .provider_settings
                    .get(&provider)
                    .is_some_and(|snapshot| {
                        plan.constraints
                            .get("selected_settings_revision")
                            .and_then(Value::as_str)
                            == Some(snapshot.revision.as_str())
                    })
        })
}

fn canonical_target(
    journal: &super::Journal,
    target: &ActionTarget,
) -> Option<(ActionTarget, Principal)> {
    match target {
        ActionTarget::Session {
            host_epoch,
            session_id,
            expected_generation,
        } => {
            let session = journal.sessions.get(session_id)?;
            if host_epoch != &journal.host_epoch
                || host_epoch != &session.host_epoch
                || session.phase != SessionPhase::Ready
                || *expected_generation != session.generation
            {
                return None;
            }
            Some((
                ActionTarget::Session {
                    host_epoch: session.host_epoch.clone(),
                    session_id: session.id.clone(),
                    expected_generation: session.generation,
                },
                session.owner.clone(),
            ))
        }
        ActionTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            expected_generation,
        } => {
            let session = journal.sessions.get(session_id)?;
            let stream = session.streams.get(stream_id)?;
            if host_epoch != &journal.host_epoch
                || host_epoch != &session.host_epoch
                || session.phase != SessionPhase::Ready
                || stream.phase != StreamPhase::Active
                || *expected_generation != stream.generation
            {
                return None;
            }
            Some((
                ActionTarget::Stream {
                    host_epoch: session.host_epoch.clone(),
                    session_id: session.id.clone(),
                    stream_id: stream_id.clone(),
                    expected_generation: stream.generation,
                },
                session.owner.clone(),
            ))
        }
        ActionTarget::Host { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::{SessionPhase, SessionRecord};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn action_plan_digest_matches_sorted_json_contract() {
        let plan = json!({"plan_id":"p","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{"z":1,"a":2},"obligations":[],"affected_consumers":[],"consumer_change":"reconnect_required","candidate_constraint":null,"allowed_degradation":[]});
        assert_eq!(
            digest(&plan).as_deref(),
            Some("e45d56a17f9c2d75f55e5c408a1274f304a6437f3c01b5c785c619911f5ce73e")
        );
    }

    #[test]
    fn trusted_service_must_bind_current_owner_plan_and_one_use_permit() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-action-wire-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&root.join("journal")).unwrap();
        let mut state = coordinator.shared.borrow().value.clone();
        let epoch = state.host_epoch.clone();
        state.sessions.insert(
            "session".into(),
            SessionRecord {
                host_epoch: epoch.clone(),
                id: "session".into(),
                generation: 3,
                phase: SessionPhase::Ready,
                owner: Principal {
                    caller: "transport".into(),
                    initiator: "user".into(),
                },
                delegates: Default::default(),
                application_ref: json!({}),
                application: None,
                application_operation_id: None,
                admission_ids: Default::default(),
                admission_targets: Default::default(),
                admission_phases: Default::default(),
                provider_settings: Default::default(),
                required_consumers: None,
                settings_revision: "revision".into(),
                application_retired: false,
                streams: Default::default(),
                obligations: vec![],
            },
        );
        save(&coordinator.shared, state).unwrap();
        let plan = json!({"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]});
        let target = json!({"kind":"session","host_epoch":epoch,"session_id":"session","expected_generation":3});
        let request = json!({"operation":"authorize_action","operation_id":"op","action_id":"example/read","target":target,"plan_digest":digest(&plan).unwrap(),"plan":plan,"source":{"caller":"transport","initiator":"user"}});
        let service = coordinator.settings.action_service.clone();
        assert_eq!(
            coordinator.action_operation("evil", "evil", &request)["reason"],
            "permission_denied"
        );
        let mut wrong = request.clone();
        wrong["source"]["initiator"] = json!("other");
        assert_eq!(
            coordinator.action_operation(&service, "service", &wrong)["status"],
            "rejected"
        );
        let mut stale = request.clone();
        stale["target"]["expected_generation"] = json!(2);
        assert_eq!(
            coordinator.action_operation(&service, "service", &stale)["status"],
            "rejected"
        );
        let reply = coordinator.action_operation(&service, "service", &request);
        assert_eq!(reply["status"], "succeeded");
        let consume = json!({"operation":"consume_action_permit","operation_id":"op","action_id":"example/read","target":request["target"],"plan_digest":request["plan_digest"],"permit":reply["permit"]});
        assert_eq!(
            coordinator.action_operation(&service, "service", &consume)["status"],
            "succeeded"
        );
        assert_eq!(
            coordinator.action_operation(&service, "service", &consume)["status"],
            "rejected"
        );
        let complete = json!({"operation":"complete_action","operation_id":"op","action_id":"example/read","target":request["target"],"plan_digest":request["plan_digest"],"permit":reply["permit"],"outcome":"succeeded","provider_receipt":{}});
        assert_eq!(
            coordinator.action_operation(&service, "service", &complete)["status"],
            "succeeded"
        );
        drop(coordinator);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn mutating_action_waits_for_retirement_then_guards_and_finalizes_once() {
        use crate::adapter::{MethodSettings, ParticipantSettings};
        use crate::prepare_authority::{MethodApproval, ProviderSelection};
        let root = std::env::temp_dir().join(format!(
            "amaterasu-action-transition-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&root.join("journal")).unwrap();
        let method = |name: &str| MethodSettings {
            service: "display/1".into(),
            method: name.into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        };
        let participant = ParticipantSettings {
            provider: "display".into(),
            plan_method: method("plan"),
            callback_method: method("callback"),
            status_method: method("status"),
            binding_validate_method: None,
            capabilities: Default::default(),
            cleanup_methods: vec![],
            settings_revision: "rev".into(),
            settings: json!({}),
        };
        let approvals = [&participant.plan_method,&participant.callback_method,&participant.status_method].iter().map(|m| MethodApproval { method:(*m).clone(), broker_approval:json!({"provider":"display","method":{"service":m.service,"method":m.method,"interface_major":m.interface_major,"interface_minor":m.interface_minor}})}).collect();
        let selected = ProviderSelection {
            participant: participant.clone(),
            approvals,
            capabilities: Default::default(),
            cleanup_methods: vec![],
            authorized_borrowers: Default::default(),
        };
        coordinator.settings.participants.push(participant);
        coordinator.selections.insert("display".into(), selected);
        coordinator.approval_cursor = 3;
        let mut state = coordinator.shared.borrow().value.clone();
        let epoch = state.host_epoch.clone();
        let mut streams = std::collections::BTreeMap::new();
        streams.insert(
            "stream".into(),
            super::super::StreamRecord {
                generation: 4,
                phase: StreamPhase::Active,
                candidate: None,
                required: ["capture".into()].into(),
                acknowledgements: ["capture".into()].into(),
                retirements: Default::default(),
                binding_owner: "display".into(),
                preparation_transition: String::new(),
                provider_settings: [(
                    "display".into(),
                    super::super::settings_snapshot::FrozenSettings {
                        settings: json!({}),
                        revision: "rev".into(),
                    },
                )]
                .into(),
            },
        );
        state.sessions.insert(
            "session".into(),
            SessionRecord {
                host_epoch: epoch.clone(),
                id: "session".into(),
                generation: 3,
                phase: SessionPhase::Ready,
                owner: Principal {
                    caller: "transport".into(),
                    initiator: "user".into(),
                },
                delegates: Default::default(),
                application_ref: json!({}),
                application: None,
                application_operation_id: None,
                admission_ids: Default::default(),
                admission_targets: Default::default(),
                admission_phases: Default::default(),
                provider_settings: Default::default(),
                required_consumers: None,
                settings_revision: "revision".into(),
                application_retired: false,
                streams,
                obligations: vec![],
            },
        );
        save(&coordinator.shared, state).unwrap();
        let plan = json!({"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{"selected_settings_revision":"rev"},"obligations":[],"affected_consumers":[],"consumer_change":"reconnect_required","candidate_constraint":null,"allowed_degradation":[]});
        let target = json!({"kind":"session","host_epoch":epoch,"session_id":"session","expected_generation":3});
        let execute = json!({"service":"display/1","method":"execute","interface_major":1,"interface_minor":0,"schema_revision":1});
        let auth = json!({"operation":"authorize_action","operation_id":"op-transition","action_id":"display/remove","target":target,"plan_digest":digest(&plan).unwrap(),"plan":plan,"source":{"caller":"transport","initiator":"user"},"provider":"display","execute_method":execute});
        let service = coordinator.settings.action_service.clone();
        let mut stale_plan = auth.clone();
        stale_plan["plan"]["constraints"]["selected_settings_revision"] = json!("other");
        stale_plan["plan_digest"] = json!(digest(&stale_plan["plan"]).unwrap());
        assert_eq!(
            coordinator.action_operation(&service, "service", &stale_plan)["reason"],
            "provider_settings_revision_mismatch"
        );
        let issued = coordinator.action_operation(&service, "service", &auth);
        assert_eq!(issued["status"], "succeeded");
        let mut request = json!({"operation":"consume_action_permit","operation_id":"op-transition","action_id":"display/remove","target":target,"plan_digest":auth["plan_digest"],"permit":issued["permit"]});
        assert_eq!(
            coordinator.action_operation(&service, "service", &request)["status"],
            "succeeded"
        );
        request["operation"] = json!("begin_action_transition");
        assert_eq!(
            coordinator.action_operation(&service, "service", &request)["status"],
            "pending"
        );
        coordinator
            .advance_work(&mut |_| panic!("no provider call before retirement"))
            .unwrap();
        assert_eq!(
            coordinator.action_operation(&service, "service", &request)["status"],
            "pending"
        );
        let mut state = coordinator.shared.borrow().value.clone();
        state
            .sessions
            .get_mut("session")
            .unwrap()
            .streams
            .get_mut("stream")
            .unwrap()
            .retirements
            .insert("capture".into());
        save(&coordinator.shared, state).unwrap();
        coordinator
            .advance_work(&mut |_| panic!("action barrier is coordinator-only"))
            .unwrap();
        assert_eq!(
            coordinator.action_operation(&service, "service", &request)["status"],
            "succeeded"
        );
        let mut verify = request.clone();
        verify["operation"] = json!("verify_action_effect");
        verify["provider"] = json!("display");
        verify["execute_method"] = execute;
        let mut changed = coordinator.shared.borrow().value.clone();
        changed
            .sessions
            .get_mut("session")
            .unwrap()
            .streams
            .get_mut("stream")
            .unwrap()
            .provider_settings
            .get_mut("display")
            .unwrap()
            .revision = "changed".into();
        save(&coordinator.shared, changed).unwrap();
        assert_eq!(
            coordinator.action_operation("display", "display", &verify)["status"],
            "rejected"
        );
        let mut restored = coordinator.shared.borrow().value.clone();
        restored
            .sessions
            .get_mut("session")
            .unwrap()
            .streams
            .get_mut("stream")
            .unwrap()
            .provider_settings
            .get_mut("display")
            .unwrap()
            .revision = "rev".into();
        save(&coordinator.shared, restored).unwrap();
        assert_eq!(
            coordinator.action_operation("display", "display", &verify)["status"],
            "succeeded"
        );
        assert_eq!(
            coordinator.action_operation("display", "display", &verify)["status"],
            "rejected"
        );
        verify["operation"] = json!("check_action_effect");
        verify["guard_nonce"] = json!("step-1");
        assert_eq!(
            coordinator.action_operation("display", "display", &verify)["status"],
            "succeeded"
        );
        assert_eq!(
            coordinator.action_operation("display", "display", &verify)["status"],
            "rejected"
        );
        let ending = coordinator.handle("transport", "user", json!({"operation":"end_session", "operation_id":"end-while-action", "target":{"kind":"session", "host_epoch":auth["target"]["host_epoch"], "session_id":"session", "generation":3}}));
        assert_eq!(ending["status"], "pending");
        assert!(
            coordinator.shared.borrow().value.sessions["session"].phase == SessionPhase::Ending
        );
        let mut complete = request;
        complete["operation"] = json!("complete_action");
        complete["outcome"] = json!("succeeded");
        complete["provider_receipt"] = json!({});
        assert_eq!(
            coordinator.action_operation(&service, "service", &complete)["status"],
            "succeeded"
        );
        assert!(
            coordinator.shared.borrow().value.sessions["session"].phase == SessionPhase::Ending
        );
        let mut state = coordinator.shared.borrow().value.clone();
        state.sessions.get_mut("session").unwrap().phase = SessionPhase::Ready;
        state.sessions.get_mut("session").unwrap().streams.insert(
            "resumed".into(),
            super::super::StreamRecord {
                generation: 1,
                phase: StreamPhase::Active,
                candidate: None,
                required: Default::default(),
                acknowledgements: Default::default(),
                retirements: Default::default(),
                binding_owner: String::new(),
                preparation_transition: String::new(),
                provider_settings: Default::default(),
            },
        );
        save(&coordinator.shared, state).unwrap();
        assert_eq!(
            coordinator.action_operation(&service, "service", &complete)["status"],
            "succeeded"
        );
        assert!(
            coordinator.shared.borrow().value.sessions["session"].streams["resumed"].phase
                == StreamPhase::Active
        );
        drop(coordinator);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn target_metadata_is_provider_scoped_and_uses_stream_snapshot() {
        use crate::adapter::{MethodSettings, ParticipantSettings};
        let root = std::env::temp_dir().join(format!(
            "amaterasu-action-metadata-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&root.join("journal")).unwrap();
        let method = |name: &str| MethodSettings {
            service: "display/1".into(),
            method: name.into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        };
        coordinator.settings.participants.push(ParticipantSettings {
            provider: "display".into(),
            plan_method: method("plan"),
            callback_method: method("callback"),
            status_method: method("status"),
            binding_validate_method: None,
            capabilities: Default::default(),
            cleanup_methods: vec![],
            settings_revision: "installed".into(),
            settings: json!({}),
        });
        let mut state = coordinator.shared.borrow().value.clone();
        let epoch = state.host_epoch.clone();
        let candidate: amaterasu_extension_api::CaptureBinding=serde_json::from_value(json!({"capture_provider":"capture", "selector_schema":"selector/1", "selector":{}, "resource":{"provider":"display","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"actual-seat","mutation_domain":"domain","mutation_domains":["domain"]},"mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":1,"generation":4})).unwrap();
        let mut streams = std::collections::BTreeMap::new();
        streams.insert(
            "stream".into(),
            super::super::StreamRecord {
                generation: 4,
                phase: StreamPhase::Active,
                candidate: Some(candidate),
                required: Default::default(),
                acknowledgements: Default::default(),
                retirements: Default::default(),
                binding_owner: "display".into(),
                preparation_transition: String::new(),
                provider_settings: [(
                    "display".into(),
                    super::super::settings_snapshot::FrozenSettings {
                        settings: json!({}),
                        revision: "stream-rev".into(),
                    },
                )]
                .into(),
            },
        );
        state.authority.snapshot.domains.insert(
            "domain".into(),
            crate::planning::DomainState {
                revision: 6,
                borrowers: [("stream".into(), 4)].into(),
            },
        );
        state
            .authority
            .domain_owners
            .insert("domain".into(), "display".into());
        state.sessions.insert(
            "session".into(),
            SessionRecord {
                host_epoch: epoch.clone(),
                id: "session".into(),
                generation: 3,
                phase: SessionPhase::Ready,
                owner: Principal {
                    caller: "transport".into(),
                    initiator: "user".into(),
                },
                delegates: Default::default(),
                application_ref: json!({}),
                application: None,
                application_operation_id: None,
                admission_ids: Default::default(),
                admission_targets: Default::default(),
                admission_phases: Default::default(),
                provider_settings: [(
                    "display".into(),
                    super::super::settings_snapshot::FrozenSettings {
                        settings: json!({}),
                        revision: "session-rev".into(),
                    },
                )]
                .into(),
                required_consumers: None,
                settings_revision: "request-rev".into(),
                application_retired: false,
                streams,
                obligations: vec![],
            },
        );
        save(&coordinator.shared, state).unwrap();
        let request = json!({"operation":"describe_action_target","target":{"kind":"session","host_epoch":epoch,"session_id":"session","expected_generation":3},"operation_id":"op","action_id":"display/remove"});
        let described = coordinator.describe_action_target("display", &request);
        assert_eq!(described["status"], "succeeded");
        assert_eq!(described["execution_domain"], "actual-seat");
        assert_eq!(described["settings_revision"], "stream-rev");
        assert_eq!(
            described["mutation_domains"]["domain"]["borrowers"]["stream"],
            4
        );
        assert_eq!(
            coordinator.describe_action_target("foreign", &request)["status"],
            "rejected"
        );
        let mut stale = request.clone();
        stale["target"]["expected_generation"] = json!(2);
        assert_eq!(
            coordinator.describe_action_target("display", &stale)["status"],
            "rejected"
        );
        let mut state = coordinator.shared.borrow().value.clone();
        state
            .sessions
            .get_mut("session")
            .unwrap()
            .streams
            .get_mut("stream")
            .unwrap()
            .provider_settings
            .clear();
        save(&coordinator.shared, state).unwrap();
        assert_eq!(
            coordinator.describe_action_target("display", &request)["reason"],
            "settings_snapshot_missing"
        );
        drop(coordinator);
        std::fs::remove_dir_all(root).unwrap();
    }
}
