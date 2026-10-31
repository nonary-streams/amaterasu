use super::*;
use crate::prepare_authority::{AuthorityJournal, CoordinatorPrepareAuthority, MethodApproval};
use crate::prepare_driver::{PrepareAuthority, PrepareJournal, PreparePhase};
use amaterasu_extension_api::{ProviderOperation, ProviderRequest, ProviderResponse};

struct AuthorityWriter(Shared);
impl AuthorityJournal for AuthorityWriter {
    fn persist(&mut self, state: &AuthorityState) -> ModuleResult<()> {
        let mut next = self.0.borrow().value.clone();
        next.authority = state.clone();
        save(&self.0, next)
    }
}
struct ActionAdmissionWriter {
    shared: Shared,
    key: String,
}
impl AuthorityJournal for ActionAdmissionWriter {
    fn persist(&mut self, state: &AuthorityState) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let fresh: Vec<_> = state
            .admissions
            .keys()
            .filter(|id| !next.authority.admissions.contains_key(*id))
            .cloned()
            .collect();
        if fresh.len() != 1 {
            return Err("ambiguous action admission".into());
        }
        let transfer = next
            .receipts
            .get_mut(&self.key)
            .and_then(|receipt| receipt.work.as_mut())
            .and_then(|work| work.action_transfer.as_mut())
            .ok_or("action_transition_missing")?;
        if transfer.admission_id.is_some() {
            return Err("action_already_admitted".into());
        }
        transfer.admission_id = Some(fresh[0].clone());
        next.authority = state.clone();
        save(&self.shared, next)
    }
}

struct WorkWriter {
    shared: Shared,
    key: String,
    provider: String,
}
impl PrepareJournal for WorkWriter {
    fn persist(&mut self, state: &PrepareState) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get_mut(&self.key)
            .and_then(|r| r.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        work.states.insert(self.provider.clone(), state.clone());
        save(&self.shared, next)
    }
}

impl Coordinator {
    pub(super) fn refresh_approvals(
        &mut self,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        let methods: Vec<_> = self
            .settings
            .participants
            .iter()
            .flat_map(|p| {
                let mut methods = vec![
                    p.plan_method.clone(),
                    p.callback_method.clone(),
                    p.status_method.clone(),
                ];
                if let Some(method) = &p.binding_validate_method {
                    methods.push(method.clone());
                }
                methods.into_iter().map(move |m| (p.clone(), m))
            })
            .collect();
        if self.approval_cursor >= methods.len() {
            return Ok(());
        }
        let (participant, method) = &methods[self.approval_cursor];
        let response = send(Command::QueryApiApproval {
            service: method.service.clone(),
            method: method.method.clone(),
            interface_major: method.interface_major,
            min_minor: method.interface_minor,
        })?;
        if let Response::Result { payload } = response {
            if payload["provider"].as_str() != Some(participant.provider.as_str())
                || payload["method"]["interface_minor"].as_u64()
                    != Some(u64::from(method.interface_minor))
            {
                return Err("selected_provider_approval_mismatch".into());
            }
            let selection = self
                .selections
                .entry(participant.provider.clone())
                .or_insert_with(|| ProviderSelection {
                    participant: participant.clone(),
                    approvals: vec![],
                    capabilities: participant.capabilities.clone(),
                    cleanup_methods: participant.cleanup_methods.clone(),
                    authorized_borrowers: BTreeSet::new(),
                });
            selection.approvals.push(MethodApproval {
                method: method.clone(),
                broker_approval: payload,
            });
            self.approval_cursor += 1;
        }
        Ok(())
    }

    fn authority(&self) -> ModuleResult<CoordinatorPrepareAuthority<AuthorityWriter>> {
        CoordinatorPrepareAuthority::new(
            self.shared.borrow().value.authority.clone(),
            self.selections.values().cloned().collect(),
            AuthorityWriter(self.shared.clone()),
        )
    }

    pub(super) fn advance_work(
        &mut self,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        self.advance_validations(send)?;
        self.complete_retirement_waiters()?;
        if self.selections.len() != self.settings.participants.len() {
            return Ok(());
        }
        let mut keys: Vec<_> = self
            .shared
            .borrow()
            .value
            .receipts
            .iter()
            .filter(|(_, r)| r.response["status"] == "pending" && r.work.is_some())
            .map(|(k, _)| k.clone())
            .collect();
        if !keys.is_empty() {
            let offset = self.work_cursor % keys.len();
            keys.rotate_left(offset);
            self.work_cursor = (offset + 4.min(keys.len())) % keys.len();
        }
        for key in keys.into_iter().take(4) {
            let Some(mut work) = self
                .shared
                .borrow()
                .value
                .receipts
                .get(&key)
                .and_then(|r| r.work.clone())
            else {
                continue;
            };
            if work.waiting_retirement {
                if !self.retirement_ready(&work)? {
                    continue;
                }
                work.waiting_retirement = false;
                let mut next = self.shared.borrow().value.clone();
                next.receipts.get_mut(&key).unwrap().work = Some(work.clone());
                save(&self.shared, next)?;
            }
            if work.action_transfer.is_some() {
                if let Err(reason) = self.complete_action_barrier(&key, &work) {
                    self.fail_work(&key, &reason)?;
                }
                continue;
            }
            if work.index >= work.providers.len() {
                if work.recovery_authorized_epoch.is_some()
                    && matches!(
                        work.phase,
                        LifecyclePhase::SessionPrepare | LifecyclePhase::StreamPrepare
                    )
                {
                    self.fail_work(&key, "restart_requires_cleanup")?;
                    continue;
                }
                if let Err(reason) = self.complete_work(&key, &work) {
                    self.fail_work(&key, &reason)?;
                }
                continue;
            }
            let provider = &work.providers[work.index];
            if work.recovery_authorized_epoch.is_some()
                && matches!(
                    work.phase,
                    LifecyclePhase::SessionPrepare | LifecyclePhase::StreamPrepare
                )
                && !work.states.contains_key(provider)
            {
                self.fail_work(&key, "restart_requires_cleanup")?;
                continue;
            }
            let Some(participant) = self
                .settings
                .participants
                .iter()
                .find(|p| &p.provider == provider)
                .cloned()
            else {
                self.fail_work(&key, "provider_unavailable")?;
                continue;
            };
            let runtime_key = format!("{key}/{}", work.index);
            let mut authority = match self.authority() {
                Ok(a) => a,
                Err(_) => continue,
            };
            if !work.states.contains_key(provider) {
                let (sequence, operation_id) = authority.allocate(provider)?;
                let request = ProviderRequest {
                    provider_sequence: sequence,
                    operation: ProviderOperation::ResourcePlan,
                    version: 1,
                    transition_id: work.transition_id.clone(),
                    operation_id,
                    target: work.target.clone(),
                    phase: work.phase.clone(),
                    settings_revision: participant.settings_revision.clone(),
                    settings: serde_json::from_value(participant.settings.clone())
                        .map_err(|_| "invalid_participant_settings")?,
                    intent: work.intent.clone(),
                    reason: work.reason.clone(),
                    requested_video: work.video.clone(),
                    consumer_requirements: self.settings.consumer_requirements.clone(),
                    remaining_ms: 30_000,
                    plan_revision: None,
                    admission_id: None,
                    approved_plan: None,
                };
                let state = PrepareState {
                    provider: provider.clone(),
                    participant: participant.clone(),
                    request,
                    phase: PreparePhase::Plan,
                    plan: None,
                    admission_id: None,
                    pending_receipt: None,
                    status_polls: 0,
                    last_sequence: sequence,
                    dispatch: None,
                    response: None,
                };
                WorkWriter {
                    shared: self.shared.clone(),
                    key: key.clone(),
                    provider: provider.clone(),
                }
                .persist(&state)?;
                work.states.insert(provider.clone(), state);
            }
            if !self.drivers.contains_key(&runtime_key) {
                self.drivers.insert(
                    runtime_key.clone(),
                    PrepareDriver::new(work.states[provider].clone(), &self.settings)?,
                );
            }
            let mut writer = WorkWriter {
                shared: self.shared.clone(),
                key: key.clone(),
                provider: provider.clone(),
            };
            let result = self.drivers.get_mut(&runtime_key).unwrap().advance(
                &self.settings,
                &mut authority,
                &mut writer,
                send,
            );
            if result.is_err() {
                // The dispatcher records whether submission crossed its durable
                // effect boundary. An uncertain call is fenced, never replayed.
                if matches!(
                    work.phase,
                    LifecyclePhase::StreamRelease | LifecyclePhase::SessionRelease
                ) {
                    self.cleanup_failure(&key, "provider_outcome_unknown")?;
                } else {
                    self.fail_work(&key, "provider_outcome_unknown")?;
                }
                self.drivers.remove(&runtime_key);
                continue;
            }
            let state = self.drivers[&runtime_key].state().clone();
            if matches!(
                state.phase,
                PreparePhase::Prepared | PreparePhase::CleanupSettled
            ) {
                let fully_released = match &state.response {
                    Some(ProviderResponse::Cleanup { outcomes }) => outcomes
                        .iter()
                        .all(|o| o.state == amaterasu_extension_api::CleanupState::Released),
                    _ => true,
                };
                if fully_released && let Some(admission) = &state.admission_id {
                    authority.finish_admission(provider, admission)?;
                }
                if let Some(ProviderResponse::Cleanup { outcomes }) = &state.response {
                    let settled = self.shared.borrow().value.authority.settled.clone();
                    for (id, admission) in settled {
                        if admission.provider != *provider {
                            continue;
                        }
                        let matched: Vec<_> = outcomes
                            .iter()
                            .filter(|outcome| {
                                admission.plan.obligations.iter().any(|obligation| {
                                    obligation.obligation_id == outcome.obligation_id
                                })
                            })
                            .cloned()
                            .collect();
                        if matched.len() == admission.plan.obligations.len()
                            && matched.iter().all(|outcome| {
                                outcome.state == amaterasu_extension_api::CleanupState::Released
                            })
                        {
                            authority.retire_settled(provider, &id, &matched)?;
                        }
                    }
                }
                if let Err(reason) = self.finish_participant(&key, provider, &state) {
                    if matches!(
                        work.phase,
                        LifecyclePhase::StreamRelease | LifecyclePhase::SessionRelease
                    ) {
                        self.cleanup_failure(&key, &reason)?;
                    } else {
                        self.fail_work(&key, &reason)?;
                    }
                }
                self.drivers.remove(&runtime_key);
            } else if matches!(
                state.phase,
                PreparePhase::Rejected | PreparePhase::RecoveryRequired
            ) {
                if matches!(
                    work.phase,
                    LifecyclePhase::StreamRelease | LifecyclePhase::SessionRelease
                ) {
                    self.cleanup_failure(&key, "provider_rejected_or_requires_recovery")?;
                } else {
                    self.fail_work(&key, "provider_rejected_or_requires_recovery")?;
                }
                self.drivers.remove(&runtime_key);
            }
        }
        Ok(())
    }

    fn complete_action_barrier(&self, key: &str, work: &Work) -> ModuleResult<()> {
        let transfer = work
            .action_transfer
            .as_ref()
            .ok_or("action_transition_missing")?;
        let journal = self.shared.borrow().value.clone();
        let (provider, _, plan) = journal
            .actions
            .execution_for_permit(&transfer.service, &transfer.permit)
            .map_err(|_| "action_execution_missing")?;
        let mut selections: Vec<_> = self.selections.values().cloned().collect();
        let selected = selections
            .iter_mut()
            .find(|selection| selection.participant.provider == provider)
            .ok_or("provider_approval_unavailable")?;
        selected.authorized_borrowers.clear();
        if let amaterasu_extension_api::LifecycleTarget::Stream { stream_id, .. } = &work.target {
            selected.authorized_borrowers.insert(stream_id.clone());
        }
        let participant = selected.participant.clone();
        let mut authority = CoordinatorPrepareAuthority::new(
            journal.authority.clone(),
            selections,
            ActionAdmissionWriter {
                shared: self.shared.clone(),
                key: key.into(),
            },
        )?;
        let id = if let Some(id) = &transfer.admission_id {
            id.clone()
        } else {
            authority.admit_plan(&participant, &plan)?
        };
        authority.validate_rights(&participant, &plan, &id)?;
        let mut next = self.shared.borrow().value.clone();
        next.actions
            .mark_transition_ready(
                &transfer.service,
                &transfer.permit,
                &transfer.binding,
                &work.transition_id,
            )
            .map_err(|_| "action_transition_invalid")?;
        let receipt = next
            .receipts
            .get_mut(key)
            .ok_or("action_transition_missing")?;
        receipt.response["status"] = json!("succeeded");
        receipt.response["reconnect_required"] = json!(true);
        save(&self.shared, next)
    }

    fn retirement_ready(&self, work: &Work) -> ModuleResult<bool> {
        let (_, id, stream_id, _) = super::operations::target_parts(&work.target)?;
        let journal = &self.shared.borrow().value;
        let session = journal.sessions.get(id).ok_or("unknown_session")?;
        if journal.receipts.values().any(|receipt| {
            receipt.work.as_ref().is_some_and(|other| {
                other.transition_id != work.transition_id
                    && matches!(
                        other.phase,
                        LifecyclePhase::SessionPrepare | LifecyclePhase::StreamPrepare
                    )
                    && super::operations::target_parts(&other.target).is_ok_and(
                        |(_, other_session, other_stream, _)| {
                            other_session == id
                                && (stream_id.is_none() || other_stream == stream_id)
                        },
                    )
                    && matches!(
                        receipt.response["status"].as_str(),
                        Some("pending" | "recovery_required")
                    )
            })
        }) {
            return Ok(false);
        }
        if let Some(stream_id) = stream_id {
            let stream = session.streams.get(stream_id).ok_or("unknown_stream")?;
            if stream.retirements != stream.required {
                return Ok(false);
            }
            let mut actions = journal.actions.clone();
            let (epoch, session, _, generation) = super::operations::target_parts(&work.target)?;
            let action_target = crate::action_authority::ActionTarget::Stream {
                host_epoch: epoch.into(),
                session_id: session.into(),
                stream_id: stream_id.into(),
                expected_generation: generation,
            };
            let blocked = if let Some(transfer) = &work.action_transfer {
                actions.fence_target_except_transfer(
                    &action_target,
                    &transfer.service,
                    &transfer.permit,
                    &transfer.binding,
                    &work.transition_id,
                )
            } else {
                actions.fence_target(&action_target)
            }
            .map_err(|_| "action_fence_failed")?;
            if blocked {
                return Ok(false);
            }
            let mut inputs = journal.inputs.clone();
            if let Some(transfer) = &work.action_transfer
                && let Some(reservation_id) = &transfer.input_reservation_id
            {
                let reservation = inputs
                    .resolve_running_reservation(reservation_id)
                    .map_err(|_| "input_reservation_unavailable")?;
                if reservation.binding.session_id != id
                    || reservation.binding.stream_id != stream_id
                    || reservation.binding.generation != generation
                {
                    return Err("input_reservation_mismatch".into());
                }
                return Ok(inputs
                    .fence_except(&reservation.binding, reservation_id, &transfer.service)
                    .map_err(|_| "input_fence_failed")?
                    == 0);
            }
            Ok(!inputs.has_unresolved_stream(epoch, id, stream_id))
        } else {
            Ok(session.application_retired
                && session
                    .streams
                    .values()
                    .all(|s| s.phase == StreamPhase::Retired))
        }
    }

    fn complete_retirement_waiters(&self) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let mut changed = false;
        let keys: Vec<_> = next
            .receipts
            .iter()
            .filter_map(|(key, r)| {
                r.wait_target
                    .as_ref()
                    .map(|target| (key.clone(), target.clone()))
            })
            .collect();
        for (key, target) in keys {
            let (_, session, stream, _) = super::operations::target_parts(&target)?;
            if next
                .sessions
                .get(session)
                .and_then(|s| s.streams.get(stream?))
                .is_some_and(|s| s.phase == StreamPhase::Retired)
            {
                let receipt = next.receipts.get_mut(&key).unwrap();
                receipt.wait_target = None;
                receipt.response["status"] = json!("succeeded");
                changed = true;
            }
        }
        if changed {
            save(&self.shared, next)?;
        }
        Ok(())
    }

    fn finish_participant(
        &self,
        key: &str,
        provider: &str,
        state: &PrepareState,
    ) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get_mut(key)
            .and_then(|r| r.work.as_mut())
            .ok_or("unknown_transition")?;
        if let Some(ProviderResponse::Prepared {
            candidate,
            obligations,
        }) = &state.response
        {
            if let Some(candidate) = candidate {
                if work.candidate.as_ref().is_some_and(|old| old != candidate) {
                    return Err("participants_disagree_on_capture".into());
                }
                work.candidate = Some(candidate.clone());
            }
            let (_, id, _, _) = super::operations::target_parts(&work.target)?;
            let session = next.sessions.get_mut(id).ok_or("unknown_session")?;
            for obligation in obligations {
                if let Some(old) = session
                    .obligations
                    .iter()
                    .find(|o| o.obligation_id == obligation.obligation_id)
                {
                    if old != obligation {
                        return Err("obligation_identity_conflict".into());
                    }
                } else {
                    session.obligations.push(obligation.clone());
                }
            }
        }
        work.states.insert(provider.into(), state.clone());
        work.index += 1;
        save(&self.shared, next)
    }

    fn complete_work(&self, key: &str, work: &Work) -> ModuleResult<()> {
        if let Some(reason) = &work.failure {
            return self.fail_work(key, reason);
        }
        let mut next = self.shared.borrow().value.clone();
        let (_, id, stream_id, _) = super::operations::target_parts(&work.target)?;
        let session = next.sessions.get_mut(id).ok_or("unknown_session")?;
        let mut response = json!({"status":"succeeded","target":work.target,"transition_id":work.transition_id,"host_epoch":session.host_epoch,"session_id":session.id,"generation":session.generation});
        if let Some(stream) = stream_id.and_then(|id| session.streams.get(id)) {
            response["plan_revision"] = json!(
                stream
                    .candidate
                    .as_ref()
                    .map(|binding| binding.plan_revision)
            );
        }
        let deferred=work.states.values().any(|state|matches!(&state.response,Some(ProviderResponse::Cleanup{outcomes}) if outcomes.iter().any(|o|o.state==amaterasu_extension_api::CleanupState::DeferredRestoration)));
        match work.phase {
            LifecyclePhase::SessionPrepare => {
                if session.phase == SessionPhase::Preparing {
                    session.phase = SessionPhase::Ready;
                } else {
                    response["status"] = json!("cancelled");
                }
                response["execution_domain"] =
                    json!(self.settings.consumer_requirements.execution_domain);
            }
            LifecyclePhase::StreamPrepare => {
                let candidate = work.candidate.clone().ok_or("capture_candidate_missing")?;
                let stream = session
                    .streams
                    .get_mut(stream_id.ok_or("stream_target_required")?)
                    .ok_or("unknown_stream")?;
                if candidate.generation != stream.generation
                    || work.video.as_ref() != Some(&candidate.mode)
                {
                    return Err("capture_candidate_mismatch".into());
                }
                stream.candidate = Some(candidate.clone());
                if stream.phase == StreamPhase::Preparing {
                    stream.phase = StreamPhase::Ready;
                } else {
                    response["status"] = json!("cancelled");
                }
                stream.binding_owner = work
                    .states
                    .iter()
                    .find_map(|(provider, state)| {
                        matches!(
                            &state.response,
                            Some(ProviderResponse::Prepared {
                                candidate: Some(_),
                                ..
                            })
                        )
                        .then(|| provider.clone())
                    })
                    .ok_or("capture_owner_missing")?;
                response["candidate"] =
                    serde_json::to_value(candidate).map_err(|e| e.to_string())?;
                response["required_consumers"] = json!(stream.required);
            }
            LifecyclePhase::StreamRelease => {
                session
                    .streams
                    .get_mut(stream_id.ok_or("stream_target_required")?)
                    .ok_or("unknown_stream")?
                    .phase = StreamPhase::Retired;
                if session.phase != SessionPhase::Ending
                    && session
                        .streams
                        .values()
                        .all(|s| s.phase == StreamPhase::Retired)
                {
                    session.phase = SessionPhase::Paused;
                }
            }
            LifecyclePhase::SessionRelease => {
                session.phase = if deferred {
                    SessionPhase::Retired
                } else {
                    SessionPhase::Ended
                };
                if !deferred
                    && !work
                        .failed_preparation
                        .as_ref()
                        .is_some_and(|failed| failed.uncertain)
                {
                    session.obligations.clear();
                }
            }
        }
        if deferred {
            response["status"] = json!("deferred_restoration");
        }
        if let Some(failed) = &work.failed_preparation {
            response["status"] = json!(if failed.uncertain {
                "recovery_required"
            } else {
                "failed"
            });
            response["reason"] = json!(failed.reason);
            response["cleanup_completed"] = json!(!deferred && !failed.uncertain);
            if failed.uncertain {
                session.phase = SessionPhase::RecoveryRequired;
                if let Some(stream) = stream_id {
                    session.streams.get_mut(stream).unwrap().phase = StreamPhase::RecoveryRequired;
                } else {
                    session.phase = SessionPhase::RecoveryRequired;
                }
            }
        }
        let receipt = next.receipts.get_mut(key).ok_or("unknown_transition")?;
        response["operation_id"] = receipt.request["operation_id"].clone();
        receipt.response = response;
        receipt.work = work
            .failed_preparation
            .as_ref()
            .is_some_and(|failed| failed.uncertain)
            .then(|| work.clone());
        save(&self.shared, next)
    }

    fn cleanup_failure(&self, key: &str, reason: &str) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get_mut(key)
            .and_then(|r| r.work.as_mut())
            .ok_or("unknown_transition")?;
        work.failure.get_or_insert_with(|| reason.into());
        work.index += 1;
        save(&self.shared, next)
    }

    fn fail_work(&self, key: &str, reason: &str) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get(key)
            .and_then(|receipt| receipt.work.as_ref())
            .cloned();
        if let Some(work) = &work
            && matches!(
                work.phase,
                LifecyclePhase::SessionPrepare | LifecyclePhase::StreamPrepare
            )
        {
            let uncertain = work.states.values().any(|state| {
                !matches!(state.phase, PreparePhase::Prepared | PreparePhase::Rejected)
            });
            let mut providers: Vec<_> = work
                .providers
                .iter()
                .filter(|provider| {
                    work.states
                        .get(*provider)
                        .is_some_and(|state| state.phase == PreparePhase::Prepared)
                })
                .cloned()
                .collect();
            providers.reverse();
            let phase = if work.phase == LifecyclePhase::SessionPrepare {
                LifecyclePhase::SessionRelease
            } else {
                LifecyclePhase::StreamRelease
            };
            let another_release = next.receipts.iter().any(|(other_key, receipt)| {
                other_key != key
                    && receipt.response["status"] == "pending"
                    && receipt
                        .work
                        .as_ref()
                        .is_some_and(|other| other.phase == phase && other.target == work.target)
            });
            let (_, id, stream, _) = super::operations::target_parts(&work.target)?;
            let session = next.sessions.get_mut(id).ok_or("unknown_session")?;
            if let Some(stream) = stream {
                let stream = session.streams.get_mut(stream).ok_or("unknown_stream")?;
                // Preparation has never published a usable binding, so no native
                // consumer can have acquired it. Completed participants still
                // need reverse-order cleanup before this operation is terminal.
                stream.phase = StreamPhase::Quiescing;
                stream.retirements = stream.required.clone();
            } else {
                session.phase = SessionPhase::Ending;
                session.application_retired = session.application.is_none();
            }
            let receipt = next.receipts.get_mut(key).unwrap();
            receipt.response["reason"] = json!(reason);
            if another_release {
                // An admitted end operation already owns cleanup. Do not issue
                // a second set of restoration callbacks for the same target.
                receipt.response["status"] = json!(if uncertain {
                    "recovery_required"
                } else {
                    "failed"
                });
            } else {
                receipt.work = Some(Work {
                    phase,
                    target: work.target.clone(),
                    intent: None,
                    reason: Some(ReleaseReason::FailedStart),
                    video: work.video.clone(),
                    transition_id: identifier()?,
                    providers,
                    index: 0,
                    states: BTreeMap::new(),
                    candidate: None,
                    waiting_retirement: stream.is_none() && !session.application_retired,
                    failure: None,
                    action_transfer: None,
                    recovery_authorized_epoch: work.recovery_authorized_epoch.clone(),
                    failed_preparation: Some(FailedPreparation {
                        reason: reason.into(),
                        uncertain,
                        states: work.states.clone(),
                    }),
                });
                receipt.response["status"] = json!("pending");
                receipt.response["cleanup_pending"] = json!(true);
            }
            return save(&self.shared, next);
        }
        let receipt = next.receipts.get_mut(key).ok_or("unknown_transition")?;
        if let Some(work) = &receipt.work {
            let (_, id, stream, _) = super::operations::target_parts(&work.target)?;
            if let Some(session) = next.sessions.get_mut(id) {
                if let Some(stream) = stream {
                    if let Some(stream) = session.streams.get_mut(stream) {
                        stream.phase = StreamPhase::RecoveryRequired;
                    }
                } else {
                    session.phase = SessionPhase::RecoveryRequired;
                }
            }
        }
        receipt.response["status"] = json!("recovery_required");
        receipt.response["reason"] = json!(reason);
        // Keep the work and obligations for explicit reconciliation, but never
        // submit it again from the background pending-work loop.
        save(&self.shared, next)
    }
}
