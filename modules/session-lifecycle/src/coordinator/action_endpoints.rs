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
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    time::{SystemTime, UNIX_EPOCH},
};

// An action transition currently retires consumers but does not run dependent
// provider cleanup. Until it does, a persisted dependency graph must prove that
// no provider depends on the provider whose resource the action will mutate.
pub(super) fn graph_proves_no_dependents(
    order: &[String],
    dependencies: &BTreeMap<String, Vec<String>>,
    provider: &str,
) -> bool {
    let positions: BTreeMap<_, _> = order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect();
    if positions.len() != order.len()
        || dependencies.len() != order.len()
        || !positions.contains_key(provider)
        || dependencies.iter().any(|(id, parents)| {
            let Some(position) = positions.get(id.as_str()) else {
                return true;
            };
            let unique: BTreeSet<_> = parents.iter().collect();
            unique.len() != parents.len()
                || parents.iter().any(|parent| {
                    positions
                        .get(parent.as_str())
                        .is_none_or(|parent_position| parent_position >= position)
                })
        })
    {
        return false;
    }
    // Any transitive dependent implies an immediate dependent, so checking
    // every edge is sufficient and also avoids trusting admission provenance.
    !dependencies
        .values()
        .any(|parents| parents.iter().any(|parent| parent == provider))
}

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
        if matches!(
            operation,
            "inspect_action_effect" | "verify_action_effect" | "check_action_effect"
        ) {
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
            if operation == "inspect_action_effect" {
                if journal
                    .actions
                    .inspect_effect(caller, token, &binding, &method)
                    .is_err()
                {
                    return rejection("action_effect_not_authorized");
                }
                return json!({"status":"succeeded", "operation_id":binding.operation_id, "action_id":binding.action_id, "target":binding.target, "plan_digest":binding.plan_digest, "provider":caller, "execute_method":method});
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
                if action_transfer_in_progress(&journal) {
                    return rejection("action_transition_in_progress");
                }
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
                let borrowers = if let Some(selected) = selection {
                    let Some(stream_target) = action_stream_target(&journal, &target) else {
                        return rejection("action_target_ambiguous");
                    };
                    if !action_dependencies_clear(
                        &journal,
                        &stream_target,
                        &selected.participant.provider,
                    ) {
                        return rejection("action_dependencies_unresolved");
                    }
                    match action_borrower_scope(
                        &journal,
                        &stream_target,
                        &selected.participant.provider,
                        &plan,
                    ) {
                        Ok(borrowers) => borrowers,
                        Err(reason) => return rejection(reason),
                    }
                } else {
                    BTreeSet::new()
                };
                let context = AdmissionContext {
                    provider: selection
                        .map(|selected| selected.participant.provider.as_str())
                        .unwrap_or(""),
                    approved_control_bindings: &[],
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
                if action_transfer_in_progress(&journal) {
                    return rejection("action_transition_in_progress");
                }
                let Ok((provider, _method, _plan)) =
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
                if !action_dependencies_clear(&journal, &target, &provider) {
                    return rejection("action_dependencies_unresolved");
                }
                let Some(provider_graph) = stream.provider_graph.clone() else {
                    return rejection("action_provider_graph_missing");
                };
                let providers = provider_graph.order.iter().rev().cloned().collect();
                let Ok(snapshot) = capture_action_snapshot(&journal, &target) else {
                    return rejection("action_snapshot_unavailable");
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
                    composition_progress: None,
                    phase: amaterasu_extension_api::LifecyclePhase::StreamRelease,
                    target,
                    intent: None,
                    reason: Some(amaterasu_extension_api::ReleaseReason::Reconfigure),
                    video: None,
                    transition_id,
                    providers,
                    provider_graph: Some(provider_graph),
                    cleanup_blockers: BTreeSet::new(),
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
                        snapshot: Some(snapshot),
                        stage: super::ActionTransferStage::CleanupPending,
                        release_proof: None,
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
                    if action_transfer_in_progress(&journal) {
                        return rejection("action_transition_in_progress");
                    }
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

fn action_transfer_in_progress(journal: &super::Journal) -> bool {
    journal.receipts.values().any(|receipt| {
        receipt
            .work
            .as_ref()
            .is_some_and(|work| work.action_transfer.is_some())
    })
}

pub(super) fn capture_action_snapshot(
    journal: &super::Journal,
    target: &amaterasu_extension_api::LifecycleTarget,
) -> Result<super::ActionReleaseSnapshot, &'static str> {
    use amaterasu_extension_api::{LifecyclePhase, LifecycleTarget};
    let LifecycleTarget::Stream {
        host_epoch,
        session_id,
        stream_id,
        generation,
    } = target
    else {
        return Err("stream_target_required");
    };
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    let stream = session.streams.get(stream_id).ok_or("unknown_stream")?;
    if host_epoch != &session.host_epoch || stream.generation != *generation {
        return Err("stale_target");
    }
    let session_target = LifecycleTarget::Session {
        host_epoch: host_epoch.clone(),
        session_id: session_id.clone(),
        generation: session.generation,
    };
    let mut session_admissions = BTreeMap::new();
    let mut stream_admissions = BTreeMap::new();
    for id in &session.admission_ids {
        let original_target = session
            .admission_targets
            .get(id)
            .ok_or("admission_target_missing")?;
        let phase = session
            .admission_phases
            .get(id)
            .ok_or("admission_phase_missing")?;
        let destination =
            if original_target == &session_target && *phase == LifecyclePhase::SessionPrepare {
                &mut session_admissions
            } else if original_target == target && *phase == LifecyclePhase::StreamPrepare {
                &mut stream_admissions
            } else {
                continue;
            };
        let admission = journal
            .authority
            .settled
            .get(id)
            .ok_or("admission_not_settled")?;
        let transition_id = journal
            .admission_transitions
            .get(id)
            .filter(|id| super::valid_id(id))
            .ok_or("admission_transition_missing")?;
        destination.insert(
            id.clone(),
            super::ActionSnapshotAdmission {
                admission: admission.clone(),
                transition_id: transition_id.clone(),
            },
        );
    }
    let snapshot = super::ActionReleaseSnapshot {
        session_target,
        stream_target: target.clone(),
        session_graph: session
            .provider_graph
            .clone()
            .ok_or("session_graph_missing")?,
        stream_graph: stream
            .provider_graph
            .clone()
            .ok_or("stream_graph_missing")?,
        session_admissions,
        stream_admissions,
    };
    if serde_json::to_vec(&snapshot).map_or(true, |bytes| bytes.len() > 64 * 1024) {
        return Err("action_snapshot_too_large");
    }
    Ok(snapshot)
}

pub(super) fn action_snapshot_matches(
    journal: &super::Journal,
    receipt: &super::Receipt,
    work: &super::Work,
    transfer: &super::ActionTransfer,
    snapshot: &super::ActionReleaseSnapshot,
) -> bool {
    let amaterasu_extension_api::LifecycleTarget::Stream {
        host_epoch,
        session_id,
        stream_id,
        generation,
    } = &work.target
    else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    let target_matches = match &transfer.binding.target {
        ActionTarget::Session {
            host_epoch: epoch,
            session_id: id,
            expected_generation,
        } => epoch == host_epoch && id == session_id && *expected_generation == session.generation,
        ActionTarget::Stream {
            host_epoch: epoch,
            session_id: id,
            stream_id: stream,
            expected_generation,
        } => {
            epoch == host_epoch
                && id == session_id
                && stream == stream_id
                && expected_generation == generation
        }
        ActionTarget::Host { .. } => false,
    };
    target_matches
        && transfer.binding.source == session.owner
        && transfer.service == receipt.caller
        && work.phase == amaterasu_extension_api::LifecyclePhase::StreamRelease
        && work.provider_graph.as_ref() == Some(&snapshot.stream_graph)
        && work
            .providers
            .iter()
            .eq(snapshot.stream_graph.order.iter().rev())
        && receipt.request["operation_id"].as_str() == Some(transfer.binding.operation_id.as_str())
        && journal
            .actions
            .binding_for_permit(&transfer.service, &transfer.permit)
            .ok()
            .as_ref()
            == Some(&transfer.binding)
        && snapshot.stream_target == work.target
        && capture_action_snapshot(journal, &work.target).ok().as_ref() == Some(snapshot)
}

/// Resolve the single stream that the action will retire; preparing or
/// quiescing siblings keep the action fenced until independently retired.
fn action_stream_target(
    journal: &super::Journal,
    target: &ActionTarget,
) -> Option<amaterasu_extension_api::LifecycleTarget> {
    let session_id = match target {
        ActionTarget::Session { session_id, .. } | ActionTarget::Stream { session_id, .. } => {
            session_id
        }
        ActionTarget::Host { .. } => return None,
    };
    let session = journal.sessions.get(session_id)?;
    let active: Vec<_> = session
        .streams
        .iter()
        .filter(|(_, stream)| stream.phase == StreamPhase::Active)
        .collect();
    if active.len() != 1
        || session
            .streams
            .iter()
            .any(|(id, stream)| id != active[0].0 && stream.phase != StreamPhase::Retired)
    {
        return None;
    }
    let (stream_id, stream) = active[0];
    if let ActionTarget::Stream {
        stream_id: requested,
        ..
    } = target
        && requested != stream_id
    {
        return None;
    }
    Some(amaterasu_extension_api::LifecycleTarget::Stream {
        host_epoch: session.host_epoch.clone(),
        session_id: session.id.clone(),
        stream_id: stream_id.clone(),
        generation: stream.generation,
    })
}

/// Only exact retained preparation admissions can authorize the owning session
/// and retiring stream as cooperative borrowers of these resources.
pub(super) fn action_borrower_scope(
    journal: &super::Journal,
    stream_target: &amaterasu_extension_api::LifecycleTarget,
    provider: &str,
    plan: &ResourcePlan,
) -> Result<BTreeSet<String>, &'static str> {
    use amaterasu_extension_api::{LifecyclePhase, LifecycleTarget};
    let LifecycleTarget::Stream {
        host_epoch,
        session_id,
        stream_id,
        generation,
    } = stream_target
    else {
        return Err("stream_target_required");
    };
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    let stream = session.streams.get(stream_id).ok_or("unknown_stream")?;
    if session.host_epoch != *host_epoch
        || journal.host_epoch != *host_epoch
        || stream.generation != *generation
        || stream.binding_owner != provider
        || !matches!(stream.phase, StreamPhase::Active | StreamPhase::Quiescing)
        || session
            .streams
            .iter()
            .any(|(id, other)| id != stream_id && other.phase != StreamPhase::Retired)
    {
        return Err("action_target_ambiguous");
    }
    if !plan.resources.is_empty() {
        let candidate = stream.candidate.as_ref().ok_or("candidate_missing")?;
        if plan.resources.iter().any(|resource| {
            resource.provider != provider
                || resource.resource_id != candidate.resource.resource_id
                || resource.incarnation.as_deref() != Some(candidate.resource.incarnation.as_str())
                || resource.execution_domain != candidate.resource.execution_domain
        }) {
            return Err("action_resource_mismatch");
        }
    }
    let shares_resource = |old: &ResourcePlan| {
        old.resources.iter().any(|owned| {
            plan.resources.iter().any(|requested| {
                owned.provider == provider
                    && requested.provider == provider
                    && owned.resource_id == requested.resource_id
                    && owned.incarnation.as_ref().is_none_or(|incarnation| {
                        requested.incarnation.as_ref() == Some(incarnation)
                    })
                    && owned.execution_domain == requested.execution_domain
            })
        })
    };
    let owns = |expected_target: &LifecycleTarget, expected_phase: LifecyclePhase| {
        session.admission_ids.iter().any(|id| {
            session.admission_targets.get(id) == Some(expected_target)
                && session.admission_phases.get(id) == Some(&expected_phase)
                && journal
                    .authority
                    .admissions
                    .get(id)
                    .or_else(|| journal.authority.settled.get(id))
                    .is_some_and(|admission| {
                        admission.provider == provider && shares_resource(&admission.plan)
                    })
        })
    };
    let session_target = LifecycleTarget::Session {
        host_epoch: host_epoch.clone(),
        session_id: session_id.clone(),
        generation: session.generation,
    };
    let mut result = BTreeSet::new();
    for domain in &plan.mutation_domains {
        if journal
            .authority
            .domain_owners
            .get(&domain.domain)
            .map(String::as_str)
            != Some(provider)
            || journal.authority.refresh_required.contains(&domain.domain)
        {
            return Err("action_domain_unavailable");
        }
        for (borrower, revision) in &domain.borrowers {
            if borrower == stream_id && *revision == stream.generation {
                if !owns(stream_target, LifecyclePhase::StreamPrepare) {
                    return Err("borrower_provenance_missing");
                }
                result.insert(stream_id.clone());
            } else if borrower == session_id && *revision == session.generation {
                if !owns(&session_target, LifecyclePhase::SessionPrepare) {
                    return Err("borrower_provenance_missing");
                }
                result.insert(session_id.clone());
            } else {
                return Err("foreign_borrower");
            }
        }
    }
    Ok(result)
}

fn action_dependencies_clear(
    journal: &super::Journal,
    target: &amaterasu_extension_api::LifecycleTarget,
    provider: &str,
) -> bool {
    let amaterasu_extension_api::LifecycleTarget::Stream {
        host_epoch,
        session_id,
        stream_id,
        generation,
    } = target
    else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    if host_epoch != &journal.host_epoch
        || host_epoch != &session.host_epoch
        || session
            .streams
            .get(stream_id)
            .is_none_or(|stream| stream.generation != *generation)
    {
        return false;
    }
    let independent = |graph: &Option<super::ProviderGraph>| {
        graph.as_ref().is_some_and(|graph| {
            graph_proves_no_dependents(&graph.order, &graph.dependencies, provider)
        })
    };
    independent(&session.provider_graph)
        && session
            .streams
            .values()
            .filter(|stream| stream.phase != StreamPhase::Retired)
            .all(|stream| independent(&stream.provider_graph))
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
    if !action_dependencies_clear(journal, &work.target, &provider) {
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

    #[test]
    fn action_graph_requires_proven_independence() {
        let order = vec!["display".into(), "control".into(), "other".into()];
        let mut graph = BTreeMap::from([
            ("display".into(), vec![]),
            ("control".into(), vec!["display".into()]),
            ("other".into(), vec!["control".into()]),
        ]);
        assert!(!graph_proves_no_dependents(&order, &graph, "display"));
        assert!(!graph_proves_no_dependents(&order, &graph, "control"));
        assert!(graph_proves_no_dependents(&order, &graph, "other"));
        graph.insert("control".into(), vec![]);
        assert!(graph_proves_no_dependents(&order, &graph, "display"));
        graph.insert("display".into(), vec!["other".into()]);
        assert!(!graph_proves_no_dependents(&order, &graph, "display"));
        assert!(!graph_proves_no_dependents(&order, &graph, "missing"));
    }
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
                provider_graph: None,
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
                resource_generations: Default::default(),
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
    fn mutating_action_fences_unimplemented_provider_cleanup() {
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
            contract_version: 1,
            depends_on: vec![],
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
                provider_graph: None,
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
                provider_graph: None,
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
                resource_generations: Default::default(),
                provider_settings: Default::default(),
                required_consumers: None,
                settings_revision: "revision".into(),
                application_retired: false,
                streams,
                obligations: vec![],
            },
        );
        let graph = super::super::ProviderGraph {
            order: vec!["display".into()],
            dependencies: [("display".into(), vec![])].into(),
        };
        let session = state.sessions.get_mut("session").unwrap();
        session.provider_graph = Some(graph.clone());
        session.streams.get_mut("stream").unwrap().provider_graph = Some(graph);
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
        let mut blocked = coordinator.shared.borrow().value.clone();
        let graph = blocked
            .sessions
            .get_mut("session")
            .unwrap()
            .provider_graph
            .as_mut()
            .unwrap();
        graph.order.push("control".into());
        graph
            .dependencies
            .insert("control".into(), vec!["display".into()]);
        save(&coordinator.shared, blocked).unwrap();
        assert_eq!(
            coordinator.action_operation(&service, "service", &auth)["reason"],
            "action_dependencies_unresolved"
        );
        let mut restored = coordinator.shared.borrow().value.clone();
        let graph = restored
            .sessions
            .get_mut("session")
            .unwrap()
            .provider_graph
            .as_mut()
            .unwrap();
        graph.order.pop();
        graph.dependencies.remove("control");
        save(&coordinator.shared, restored).unwrap();
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
        {
            let journal = &coordinator.shared.borrow().value;
            let key = super::super::receipt_key(&service, "action-transition", "op-transition");
            let work = journal.receipts[&key].work.as_ref().unwrap();
            assert_eq!(work.providers, vec!["display"]);
            assert_eq!(work.provider_graph.as_ref().unwrap().order, vec!["display"]);
            assert!(super::super::validate_journal(journal).is_ok());
            let mut tampered = journal.clone();
            tampered
                .receipts
                .get_mut(&key)
                .unwrap()
                .work
                .as_mut()
                .unwrap()
                .action_transfer
                .as_mut()
                .unwrap()
                .snapshot
                .as_mut()
                .unwrap()
                .stream_graph
                .order
                .clear();
            assert!(super::super::validate_journal(&tampered).is_err());
            let mut premature = journal.clone();
            premature
                .receipts
                .get_mut(&key)
                .unwrap()
                .work
                .as_mut()
                .unwrap()
                .action_transfer
                .as_mut()
                .unwrap()
                .stage = super::super::ActionTransferStage::ReplanRequired;
            assert!(super::super::validate_journal(&premature).is_err());
            let mut legacy = serde_json::to_value(journal).unwrap();
            legacy["receipts"][&key]["work"]["action_transfer"]
                .as_object_mut()
                .unwrap()
                .remove("stage");
            let restored: super::super::Journal = serde_json::from_value(legacy).unwrap();
            assert!(super::super::validate_journal(&restored).is_ok());
        }
        let mut competing = auth.clone();
        competing["operation_id"] = json!("competing");
        assert_eq!(
            coordinator.action_operation(&service, "service", &competing)["reason"],
            "action_transition_in_progress"
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
            .advance_work(&mut |_| panic!("cleanup must remain fenced"))
            .unwrap();
        let stopped = coordinator.action_operation(&service, "service", &request);
        assert_eq!(stopped["status"], "recovery_required");
        assert_eq!(stopped["reason"], "action_provider_cleanup_not_implemented");
        let mut verify = request.clone();
        verify["operation"] = json!("verify_action_effect");
        verify["provider"] = json!("display");
        verify["execute_method"] = execute;
        assert_eq!(
            coordinator.action_operation("display", "display", &verify)["status"],
            "rejected"
        );
        {
            let journal = &coordinator.shared.borrow().value;
            let key = super::super::receipt_key(&service, "action-transition", "op-transition");
            assert!(
                journal.receipts[&key]
                    .work
                    .as_ref()
                    .unwrap()
                    .action_transfer
                    .is_some()
            );
            assert!(
                journal.sessions["session"].streams["stream"].phase
                    == StreamPhase::RecoveryRequired
            );
            assert!(journal.sessions["session"].phase == SessionPhase::Ready);
        }
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
            contract_version: 1,
            depends_on: vec![],
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
                provider_graph: None,
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
                provider_graph: None,
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
                resource_generations: Default::default(),
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
        // Prove that the parent session borrower belongs to the same provider,
        // exact target generation and logical resource as the retiring stream.
        let mut scoped = state.clone();
        let plan: ResourcePlan = serde_json::from_value(json!({"plan_id":"plan","revision":1,"resources":[{"provider":"display","resource_id":"output","incarnation":"native","expected_revision":1,"execution_domain":"actual-seat","mutation_domains":["domain"],"access":"exclusive"}],"mutation_domains":[{"domain":"domain","expected_revision":6,"borrowers":{"session":3,"stream":4},"access":"exclusive"}],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"reconnect_required","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        let selection = crate::prepare_authority::ProviderSelection {
            participant: coordinator.settings.participants[0].clone(),
            approvals: vec![],
            capabilities: Default::default(),
            cleanup_methods: vec![],
            authorized_borrowers: Default::default(),
        };
        let session_target = amaterasu_extension_api::LifecycleTarget::Session {
            host_epoch: epoch.clone(),
            session_id: "session".into(),
            generation: 3,
        };
        let stream_target = amaterasu_extension_api::LifecycleTarget::Stream {
            host_epoch: epoch.clone(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 4,
        };
        for (id, target, phase) in [
            (
                "session-admission",
                session_target,
                amaterasu_extension_api::LifecyclePhase::SessionPrepare,
            ),
            (
                "stream-admission",
                stream_target.clone(),
                amaterasu_extension_api::LifecyclePhase::StreamPrepare,
            ),
        ] {
            let session = scoped.sessions.get_mut("session").unwrap();
            session.admission_ids.insert(id.into());
            session.admission_targets.insert(id.into(), target);
            session.admission_phases.insert(id.into(), phase);
            scoped.authority.settled.insert(
                id.into(),
                crate::prepare_authority::Admission {
                    provider: "display".into(),
                    selection: selection.clone(),
                    plan: plan.clone(),
                    rights: vec![],
                },
            );
        }
        let frozen_graph = super::super::ProviderGraph {
            order: vec!["display".into()],
            dependencies: [("display".into(), vec![])].into(),
        };
        let session = scoped.sessions.get_mut("session").unwrap();
        session.provider_graph = Some(frozen_graph.clone());
        session.streams.get_mut("stream").unwrap().provider_graph = Some(frozen_graph);
        scoped
            .admission_transitions
            .insert("session-admission".into(), "session-transition".into());
        scoped
            .admission_transitions
            .insert("stream-admission".into(), "stream-transition".into());
        let captured = capture_action_snapshot(&scoped, &stream_target).unwrap();
        assert_eq!(
            captured.session_admissions.keys().collect::<Vec<_>>(),
            vec!["session-admission"]
        );
        assert_eq!(
            captured.stream_admissions.keys().collect::<Vec<_>>(),
            vec!["stream-admission"]
        );
        let mut missing_source = scoped.clone();
        missing_source.authority.settled.remove("stream-admission");
        assert!(capture_action_snapshot(&missing_source, &stream_target).is_err());
        assert_eq!(
            action_borrower_scope(&scoped, &stream_target, "display", &plan).unwrap(),
            ["session".into(), "stream".into()].into()
        );
        let mut foreign = plan.clone();
        foreign.mutation_domains[0]
            .borrowers
            .insert("other-session".into(), 1);
        assert_eq!(
            action_borrower_scope(&scoped, &stream_target, "display", &foreign),
            Err("foreign_borrower")
        );
        let mut stale = plan.clone();
        stale.mutation_domains[0]
            .borrowers
            .insert("session".into(), 2);
        assert_eq!(
            action_borrower_scope(&scoped, &stream_target, "display", &stale),
            Err("foreign_borrower")
        );
        let mut paused = scoped.clone();
        let session = paused.sessions.get_mut("session").unwrap();
        session.phase = SessionPhase::Paused;
        session.streams.clear();
        session.admission_ids.remove("stream-admission");
        session.admission_targets.remove("stream-admission");
        session.admission_phases.remove("stream-admission");
        paused.authority.settled.remove("stream-admission");
        paused
            .authority
            .snapshot
            .domains
            .get_mut("domain")
            .unwrap()
            .borrowers = [("session".into(), 3)].into();
        paused.authority.snapshot.resources.insert(
            "display".into(),
            [(
                "output".into(),
                crate::planning::ResourceRef {
                    provider: "display".into(),
                    resource_id: "output".into(),
                    incarnation: "native".into(),
                    revision: 1,
                    execution_domain: "actual-seat".into(),
                    mutation_domains: ["domain".into()].into(),
                },
            )]
            .into(),
        );
        paused
            .admission_transitions
            .insert("session-admission".into(), "transition".into());
        paused.composition = serde_json::from_value(json!({"outputs":[{"output":{"output_id":"owned","capability":"screen","schema":{"id":"screen","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"display","producer_service":"display/2","origin":{"kind":"session","host_epoch":epoch,"session_id":"session","generation":3},"resource":{"provider":"display","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"actual-seat","mutation_domain":"domain"},"domain_observations":[{"domain":"domain","revision":6,"borrowers":{"session":3},"complete":true}],"data":{}},"admission_id":"session-admission","transition_id":"transition","plan":plan,"state":"retained"}]})).unwrap();
        save(&coordinator.shared, paused.clone()).unwrap();
        let v2 = json!({"operation":"describe_action_target","version":2,"target":{"kind":"session","host_epoch":epoch,"session_id":"session","expected_generation":3}});
        let described = coordinator.describe_action_target("display", &v2);
        assert_eq!(described["status"], "succeeded", "{described}");
        let typed: amaterasu_extension_api::ActionTargetDescription =
            serde_json::from_value(described.clone()).unwrap();
        amaterasu_extension_api::validate_action_target_description(&typed).unwrap();
        assert_eq!(described["owner_output"]["output_id"], "owned");
        assert_eq!(described["settings_revision"], "session-rev");
        assert_eq!(described["streams"], json!([]));
        let mut unavailable = paused.clone();
        let mut book = serde_json::to_value(&unavailable.composition).unwrap();
        book["outputs"][0]["state"] = json!("recovery_required");
        unavailable.composition = serde_json::from_value(book).unwrap();
        save(&coordinator.shared, unavailable).unwrap();
        assert_eq!(
            coordinator.describe_action_target("display", &v2)["reason"],
            "owner_output_ambiguous_or_unavailable"
        );
        let mut ambiguous = paused.clone();
        let mut book = serde_json::to_value(&ambiguous.composition).unwrap();
        let mut second = book["outputs"][0].clone();
        second["output"]["output_id"] = json!("other-output");
        book["outputs"].as_array_mut().unwrap().push(second);
        ambiguous.composition = serde_json::from_value(book).unwrap();
        save(&coordinator.shared, ambiguous).unwrap();
        assert_eq!(
            coordinator.describe_action_target("display", &v2)["reason"],
            "owner_output_ambiguous_or_unavailable"
        );
        paused
            .authority
            .snapshot
            .domains
            .get_mut("domain")
            .unwrap()
            .borrowers
            .insert("foreign".into(), 1);
        save(&coordinator.shared, paused).unwrap();
        assert_eq!(
            coordinator.describe_action_target("display", &v2)["reason"],
            "foreign_borrower"
        );
        scoped
            .sessions
            .get_mut("session")
            .unwrap()
            .admission_phases
            .remove("session-admission");
        assert_eq!(
            action_borrower_scope(&scoped, &stream_target, "display", &plan),
            Err("borrower_provenance_missing")
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
