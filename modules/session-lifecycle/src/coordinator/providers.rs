use super::*;
use crate::prepare_authority::{AuthorityJournal, CoordinatorPrepareAuthority, MethodApproval};
use crate::prepare_driver::{PrepareAuthority, PrepareJournal, PreparePhase};
use amaterasu_extension_api::{ProviderOperation, ProviderRequest, ProviderResponse};

/// Grant only canonical borrows attested by the selected owner output.
fn authorize_output_borrowers(
    borrowers: &mut BTreeSet<String>,
    session: &SessionRecord,
    work: &Work,
    owner: &str,
    output: &amaterasu_extension_api::ResourceOutput,
) {
    if session.streams.values().all(|stream| {
        stream.binding_owner != owner
            || stream.phase == StreamPhase::Retired
            || (stream.phase == StreamPhase::Quiescing && stream.retirements == stream.required)
    }) {
        if matches!(&output.origin, LifecycleTarget::Session { host_epoch, session_id, generation }
        if host_epoch == &session.host_epoch && session_id == &session.id && *generation == session.generation)
            && output.domain_observations.iter().any(|observation| {
                observation.complete
                    && observation.borrowers.get(&session.id) == Some(&session.generation)
            })
        {
            borrowers.insert(session.id.clone());
        }
        if let LifecycleTarget::Stream {
            stream_id,
            generation,
            ..
        } = &work.target
            && session.streams.get(stream_id).is_some_and(|stream| {
                stream.generation == *generation && stream.phase == StreamPhase::Preparing
            })
            && output.domain_observations.iter().any(|observation| {
                observation.complete && observation.borrowers.get(stream_id) == Some(generation)
            })
        {
            borrowers.insert(stream_id.clone());
        }
    }
}

pub(super) struct AuthorityWriter(pub(super) Shared);
impl AuthorityJournal for AuthorityWriter {
    fn persist(&mut self, state: &AuthorityState) -> ModuleResult<()> {
        let mut next = self.0.borrow().value.clone();
        if next.admission_transitions.keys().any(|id| {
            !state.admissions.contains_key(id)
                && !state.settled.contains_key(id)
                && next.references_admission(id)
        }) {
            return Err("composition_admission_still_referenced".into());
        }
        next.admission_transitions
            .retain(|id, _| state.admissions.contains_key(id) || state.settled.contains_key(id));
        for session in next.sessions.values_mut() {
            session
                .admission_ids
                .retain(|id| state.admissions.contains_key(id) || state.settled.contains_key(id));
            session
                .admission_targets
                .retain(|id, _| session.admission_ids.contains(id));
            session
                .admission_phases
                .retain(|id, _| session.admission_ids.contains(id));
        }
        next.authority = state.clone();
        save(&self.0, next)
    }
}

/// Commits a prepared output, obligations, participant progress and authority
/// transition in one durable journal replacement.
struct WorkFinishWriter {
    shared: Shared,
    key: String,
    provider: String,
    state: PrepareState,
    settings: LifecycleSettings,
    selections: BTreeMap<String, ProviderSelection>,
}
impl AuthorityJournal for WorkFinishWriter {
    fn persist(&mut self, authority: &AuthorityState) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get(&self.key)
            .and_then(|receipt| receipt.work.clone())
            .ok_or("unknown_transition")?;
        if work.phase == LifecyclePhase::SessionPrepare {
            super::session_prepare_evidence::record(&mut next, &work, &self.provider, &self.state)?;
        }
        composition::collect_verified_outputs(
            &mut next,
            &self.settings,
            &self.selections,
            &work,
            &self.provider,
            &self.state,
        )?;
        apply_participant(&mut next, &self.key, &self.provider, &self.state, true)?;
        let admission_id = self
            .state
            .admission_id
            .as_deref()
            .ok_or("admission_missing")?;
        if !authority.settled.contains_key(admission_id) {
            return Err("output_admission_not_retained".into());
        }
        next.authority = authority.clone();
        validate_journal(&next).map_err(|_| "invalid_composition_journal".to_owned())?;
        save(&self.shared, next)
    }
}
/// Persist a direct session prepare callback even if it has no output.
struct SessionPrepareWriter {
    shared: Shared,
    key: String,
    provider: String,
    state: PrepareState,
}
impl AuthorityJournal for SessionPrepareWriter {
    fn persist(&mut self, authority: &AuthorityState) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get(&self.key)
            .and_then(|receipt| receipt.work.clone())
            .ok_or("unknown_transition")?;
        super::session_prepare_evidence::record(&mut next, &work, &self.provider, &self.state)?;
        let admission = self
            .state
            .admission_id
            .as_deref()
            .ok_or("admission_missing")?;
        if !authority.settled.contains_key(admission) {
            return Err("session_admission_not_retained".into());
        }
        next.authority = authority.clone();
        apply_participant(&mut next, &self.key, &self.provider, &self.state, true)?;
        validate_journal(&next).map_err(|_| "invalid_session_prepare_journal")?;
        save(&self.shared, next)
    }
}

/// Retain a dependent callback and its settled control permits together.
struct DependentFinishWriter {
    shared: Shared,
    key: String,
    provider: String,
    state: PrepareState,
}
impl AuthorityJournal for DependentFinishWriter {
    fn persist(&mut self, authority: &AuthorityState) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let admission = self
            .state
            .admission_id
            .as_deref()
            .ok_or("admission_missing")?;
        if !authority.settled.contains_key(admission) {
            return Err("dependent_admission_not_retained".into());
        }
        next.authority = authority.clone();
        apply_participant(&mut next, &self.key, &self.provider, &self.state, true)?;
        validate_journal(&next).map_err(|_| "invalid_dependent_journal")?;
        save(&self.shared, next)
    }
}

/// Atomically persist terminal cleanup evidence with participant progress.
struct StreamReleaseWriter {
    shared: Shared,
    key: String,
    provider: String,
    state: PrepareState,
}
impl AuthorityJournal for StreamReleaseWriter {
    fn persist(&mut self, authority: &AuthorityState) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get(&self.key)
            .and_then(|receipt| receipt.work.clone())
            .ok_or("unknown_transition")?;
        super::stream_release::record_release_proof(&mut next, &work, &self.provider, &self.state)?;
        let admission = self
            .state
            .admission_id
            .as_deref()
            .ok_or("admission_missing")?;
        if !authority.settled.contains_key(admission) {
            return Err("release_admission_not_retained".into());
        }
        next.authority = authority.clone();
        apply_participant(&mut next, &self.key, &self.provider, &self.state, false)?;
        validate_journal(&next).map_err(|_| "invalid_release_journal")?;
        save(&self.shared, next)
    }
}

fn apply_participant(
    next: &mut Journal,
    key: &str,
    provider: &str,
    state: &PrepareState,
    allow_composition: bool,
) -> ModuleResult<()> {
    let work = next
        .receipts
        .get_mut(key)
        .and_then(|r| r.work.as_mut())
        .ok_or("unknown_transition")?;
    if let Some(ProviderResponse::Prepared {
        candidate,
        obligations,
        outputs,
    }) = &state.response
    {
        if (!allow_composition && (state.request.version != 1 || !outputs.is_empty()))
            || (allow_composition
                && (state.request.version != 2
                    || (outputs.len() != 1
                        && !(outputs.is_empty()
                            && (work.phase == LifecyclePhase::SessionPrepare
                                || work
                                    .provider_graph
                                    .as_ref()
                                    .is_some_and(|graph| graph.order.len() > 1))))))
        {
            return Err("composition_response_not_supported".into());
        }
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
    if matches!(
        &state.response,
        Some(ProviderResponse::Cleanup { outcomes, .. }) if outcomes.iter().any(|outcome|
            outcome.state == amaterasu_extension_api::CleanupState::DeferredRestoration)
    ) {
        // Runtime retirement is complete, but restoration still depends on
        // this provider's prerequisites remaining available.
        work.cleanup_blockers.insert(provider.into());
    }
    work.states.insert(provider.into(), state.clone());
    work.index += 1;
    for session in next.sessions.values_mut() {
        session.admission_ids.retain(|id| {
            next.authority.admissions.contains_key(id) || next.authority.settled.contains_key(id)
        });
        session
            .admission_targets
            .retain(|id, _| session.admission_ids.contains(id));
        session
            .admission_phases
            .retain(|id, _| session.admission_ids.contains(id));
    }
    Ok(())
}

/// Atomically bind an admission to its exact target. Reusing an admission ID
/// for a different session or generation is never an ownership transfer.
pub(super) fn record_admission(
    next: &mut Journal,
    session_id: &str,
    admission_id: &str,
    target: &LifecycleTarget,
    phase: &LifecyclePhase,
    transition_id: &str,
) -> ModuleResult<()> {
    if !valid_id(transition_id) {
        return Err("invalid_admission_transition".into());
    }
    if next
        .admission_transitions
        .get(admission_id)
        .is_some_and(|old| old != transition_id)
    {
        return Err("admission_transition_conflict".into());
    }
    if next
        .sessions
        .get(session_id)
        .is_some_and(|session| session.admission_ids.contains(admission_id))
        && !next.admission_transitions.contains_key(admission_id)
    {
        return Err("admission_transition_missing".into());
    }
    if next
        .sessions
        .iter()
        .any(|(id, session)| id != session_id && session.admission_ids.contains(admission_id))
    {
        return Err("admission_owned_by_other_session".into());
    }
    let session = next.sessions.get_mut(session_id).ok_or("unknown_session")?;
    if session
        .admission_targets
        .get(admission_id)
        .is_some_and(|old| old != target)
    {
        return Err("admission_target_conflict".into());
    }
    if session
        .admission_phases
        .get(admission_id)
        .is_some_and(|old| old != phase)
    {
        return Err("admission_phase_conflict".into());
    }
    session.admission_ids.insert(admission_id.to_owned());
    session
        .admission_phases
        .insert(admission_id.to_owned(), phase.clone());
    session
        .admission_targets
        .insert(admission_id.to_owned(), target.clone());
    next.admission_transitions
        .insert(admission_id.to_owned(), transition_id.to_owned());
    Ok(())
}

fn cleanup_admission_matches(
    journal: &Journal,
    session: &SessionRecord,
    admission_id: &str,
    target: &LifecycleTarget,
    phase: &LifecyclePhase,
) -> ModuleResult<bool> {
    let old_target = session
        .admission_targets
        .get(admission_id)
        .ok_or("admission_target_missing")?;
    let old_phase = session
        .admission_phases
        .get(admission_id)
        .ok_or("admission_phase_missing")?;
    if !journal
        .admission_transitions
        .get(admission_id)
        .is_some_and(|id| valid_id(id))
    {
        return Err("admission_transition_missing".into());
    }
    if old_target != target {
        return Ok(false);
    }
    let expected = match phase {
        LifecyclePhase::StreamRelease => matches!(
            old_phase,
            LifecyclePhase::StreamPrepare | LifecyclePhase::StreamRelease
        ),
        LifecyclePhase::SessionRelease => matches!(
            old_phase,
            LifecyclePhase::SessionPrepare | LifecyclePhase::SessionRelease
        ),
        _ => false,
    };
    if !expected {
        return Err("admission_phase_mismatch".into());
    }
    Ok(true)
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
        let work = next.receipts[&self.key]
            .work
            .as_ref()
            .ok_or("action_transition_missing")?;
        let target = work.target.clone();
        let phase = work.phase.clone();
        let transition_id = work.transition_id.clone();
        let (_, session_id, _, _) = super::operations::target_parts(&target)?;
        record_admission(
            &mut next,
            session_id,
            &fresh[0],
            &target,
            &phase,
            &transition_id,
        )?;
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
        let target = work.target.clone();
        let phase = work.phase.clone();
        let transition_id = work.transition_id.clone();
        if state.request.target != target
            || state.request.phase != phase
            || state.request.transition_id != transition_id
        {
            return Err("admission_target_mismatch".into());
        }
        let (_, session_id, _, _) = super::operations::target_parts(&target)?;
        if let Some(generation) = state.request.resource_generation {
            let session = next.sessions.get_mut(session_id).ok_or("unknown_session")?;
            match session.resource_generations.get(&self.provider) {
                Some(current) if *current == generation => {}
                None if phase == LifecyclePhase::SessionPrepare
                    && session.phase == SessionPhase::Preparing
                    && generation == 1 =>
                {
                    session
                        .resource_generations
                        .insert(self.provider.clone(), generation);
                }
                _ => return Err("resource_generation_mismatch".into()),
            }
        }
        if let Some(id) = &state.admission_id {
            record_admission(&mut next, session_id, id, &target, &phase, &transition_id)?;
        }
        save(&self.shared, next)
    }
}

fn frozen_settings<'a>(
    journal: &'a Journal,
    target: &LifecycleTarget,
) -> ModuleResult<&'a BTreeMap<String, super::settings_snapshot::FrozenSettings>> {
    let (_, session_id, stream_id, _) = super::operations::target_parts(target)?;
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    match stream_id {
        Some(stream_id) => Ok(&session
            .streams
            .get(stream_id)
            .ok_or("unknown_stream")?
            .provider_settings),
        None => Ok(&session.provider_settings),
    }
}

fn frozen_settings_mut<'a>(
    journal: &'a mut Journal,
    target: &LifecycleTarget,
) -> ModuleResult<&'a mut BTreeMap<String, super::settings_snapshot::FrozenSettings>> {
    let (_, session_id, stream_id, _) = super::operations::target_parts(target)?;
    let session = journal
        .sessions
        .get_mut(session_id)
        .ok_or("unknown_session")?;
    match stream_id {
        Some(stream_id) => Ok(&mut session
            .streams
            .get_mut(stream_id)
            .ok_or("unknown_stream")?
            .provider_settings),
        None => Ok(&mut session.provider_settings),
    }
}

impl Coordinator {
    pub(super) fn effective_settings(&self, work: &Work) -> ModuleResult<LifecycleSettings> {
        let shared = self.shared.borrow();
        let snapshots = frozen_settings(&shared.value, &work.target)?;
        let mut settings = self.settings.clone();
        for participant in &mut settings.participants {
            let snapshot = snapshots
                .get(&participant.provider)
                .ok_or("settings_snapshot_missing")?;
            participant.settings = snapshot.settings.clone();
            participant.settings_revision = snapshot.revision.clone();
        }
        Ok(settings)
    }

    fn ensure_settings(
        &mut self,
        work: &Work,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<bool> {
        let job_key = serde_json::to_string(&work.target).map_err(|_| "invalid_target")?;
        if frozen_settings(&self.shared.borrow().value, &work.target)?.len()
            == self.settings.participants.len()
        {
            return Ok(true);
        }
        if work.recovery_authorized_epoch.is_some() {
            let mut next = self.shared.borrow().value.clone();
            let snapshots = frozen_settings_mut(&mut next, &work.target)?;
            for (provider, state) in &work.states {
                snapshots.insert(
                    provider.clone(),
                    super::settings_snapshot::FrozenSettings {
                        settings: state.participant.settings.clone(),
                        revision: state.participant.settings_revision.clone(),
                    },
                );
            }
            for participant in &self.settings.participants {
                snapshots
                    .entry(participant.provider.clone())
                    .or_insert_with(|| super::settings_snapshot::FrozenSettings {
                        settings: participant.settings.clone(),
                        revision: participant.settings_revision.clone(),
                    });
            }
            save(&self.shared, next)?;
            return Ok(true);
        }
        if !self.settings_jobs.contains_key(&job_key) {
            self.settings_jobs.insert(
                job_key.clone(),
                super::settings_snapshot::SettingsSnapshotJob::new(
                    self.settings.settings_registry_service.as_deref(),
                    &self.settings.participants,
                )?,
            );
        }
        let snapshots = self
            .settings_jobs
            .get_mut(&job_key)
            .unwrap()
            .advance(send)?;
        let Some(snapshots) = snapshots else {
            return Ok(false);
        };
        let mut next = self.shared.borrow().value.clone();
        *frozen_settings_mut(&mut next, &work.target)? = snapshots;
        save(&self.shared, next)?;
        self.settings_jobs.remove(&job_key);
        Ok(true)
    }

    pub(super) fn refresh_approvals(
        &mut self,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        let mut methods: Vec<_> = self
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
        // Control and finalization each have separately installed effect and
        // status methods. Never infer status authority from lifecycle callbacks.
        let mut add_policy_method =
            |owner: &str, method: &amaterasu_extension_api::MethodRef| -> ModuleResult<()> {
                let participant = self
                    .settings
                    .participants
                    .iter()
                    .find(|p| p.provider == owner)
                    .ok_or("policy_owner_unavailable")?;
                let selected = crate::adapter::MethodSettings {
                    service: method.service.clone(),
                    method: method.method.clone(),
                    interface_major: method.interface_major,
                    interface_minor: method.interface_minor,
                    schema_revision: method.schema_revision,
                };
                if let Some((_, old)) = methods.iter().find(|(p, old)| {
                    p.provider == owner
                        && old.service == selected.service
                        && old.method == selected.method
                        && old.interface_major == selected.interface_major
                        && old.interface_minor == selected.interface_minor
                }) {
                    if old != &selected {
                        return Err("policy_method_revision_conflict".into());
                    }
                } else {
                    methods.push((participant.clone(), selected));
                }
                Ok(())
            };
        for policy in &self.settings.control_policies {
            add_policy_method(&policy.owner_provider, &policy.owner_method)?;
            add_policy_method(&policy.owner_provider, &policy.status_method)?;
            add_policy_method(&policy.owner_provider, &policy.settlement_method)?;
        }
        for policy in &self.settings.finalize_policies {
            add_policy_method(&policy.owner_provider, &policy.method)?;
            add_policy_method(&policy.owner_provider, &policy.status_method)?;
        }
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
                || payload["method"]["service"].as_str() != Some(method.service.as_str())
                || payload["method"]["method"].as_str() != Some(method.method.as_str())
                || payload["method"]["interface_major"].as_u64()
                    != Some(u64::from(method.interface_major))
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
                    approved_control_bindings: Vec::new(),
                });
            selection.approvals.push(MethodApproval {
                method: method.clone(),
                broker_approval: payload,
            });
            self.approval_cursor += 1;
        }
        Ok(())
    }

    pub(super) fn authority_for_work(
        &self,
        work: &Work,
    ) -> ModuleResult<CoordinatorPrepareAuthority<AuthorityWriter>> {
        let selections = self.work_selections(work)?;
        CoordinatorPrepareAuthority::new(
            self.shared.borrow().value.authority.clone(),
            selections,
            AuthorityWriter(self.shared.clone()),
        )
    }

    pub(super) fn work_selections(&self, work: &Work) -> ModuleResult<Vec<ProviderSelection>> {
        let journal = &self.shared.borrow().value;
        let (_, session_id, _, _) = super::operations::target_parts(&work.target)?;
        let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
        let mut selections: Vec<_> = self.selections.values().cloned().collect();
        for selected in &mut selections {
            if let Some(snapshot) =
                frozen_settings(journal, &work.target)?.get(&selected.participant.provider)
            {
                selected.participant.settings = snapshot.settings.clone();
                selected.participant.settings_revision = snapshot.revision.clone();
            }
            let provider = &selected.participant.provider;
            selected.approved_control_bindings.clear();
            if let Some(progress) = &work.composition_progress {
                for frozen in progress
                    .selections
                    .iter()
                    .filter(|item| item.control_policy.dependent_provider == *provider)
                {
                    let policy = &frozen.control_policy;
                    let owner = &policy.owner_provider;
                    let graph = work
                        .provider_graph
                        .as_ref()
                        .ok_or("provider_graph_missing")?;
                    if frozen.target != work.target
                        || frozen.transition_id != work.transition_id
                        || !frozen.validate_structure()
                        || !graph
                            .dependencies
                            .get(provider)
                            .is_some_and(|deps| deps.contains(owner))
                        || !self.settings.control_policies.contains(policy)
                        || journal.composition.selection(
                            &work.target,
                            &work.transition_id,
                            provider,
                            owner,
                            &frozen.input.output.output_id,
                        ) != Some(frozen)
                    {
                        return Err("control_selection_changed".into());
                    }
                    let owner_selection = self
                        .selections
                        .get(owner)
                        .ok_or("control_owner_unavailable")?;
                    if !owner_selection.approvals.contains(&frozen.control_approval)
                        || !owner_selection
                            .approvals
                            .contains(&frozen.control_status_approval)
                        || !owner_selection
                            .approvals
                            .contains(&frozen.settlement_approval)
                    {
                        return Err("control_approval_changed".into());
                    }
                    let output = match &frozen.input.output.origin {
                        LifecycleTarget::Session { .. } => journal
                            .composition
                            .staged_for(&work.target, owner, &frozen.input.output.output_id)
                            .map_err(str::to_owned)?
                            .after
                            .clone(),
                        _ => frozen.input.output.clone(),
                    };
                    if let Some(state) = work.states.get(provider)
                        && !state.request.dependency_inputs.contains(&output)
                    {
                        return Err("control_input_changed".into());
                    }
                    authorize_output_borrowers(
                        &mut selected.authorized_borrowers,
                        session,
                        work,
                        owner,
                        &output,
                    );
                    selected.approved_control_bindings.push(
                        crate::plan_validation::ApprovedControlBinding {
                            policy: policy.clone(),
                            output,
                            target: work.target.clone(),
                        },
                    );
                }
            }
            if let Some(finalize) = work
                .composition_progress
                .as_ref()
                .and_then(|progress| progress.finalize.as_ref())
                && finalize.selection.policy.owner_provider == *provider
            {
                let frozen = &finalize.selection;
                if !self.settings.finalize_policies.contains(&frozen.policy)
                    || !selected.approvals.contains(&frozen.finalize_approval)
                    || !selected
                        .approvals
                        .contains(&frozen.finalize_status_approval)
                    || journal.composition.finalize_selection(
                        &work.target,
                        &work.transition_id,
                        provider,
                        &frozen.input.output.output_id,
                    ) != Some(frozen)
                {
                    return Err("finalize_selection_changed".into());
                }
                let (output, _) =
                    super::composition_finalize::resolve_control_chain(journal, work, frozen)?;
                authorize_output_borrowers(
                    &mut selected.authorized_borrowers,
                    session,
                    work,
                    provider,
                    &output,
                );
            }
            let consumers_retired = session.streams.values().all(|stream| {
                stream.binding_owner != *provider
                    || stream.phase == StreamPhase::Retired
                    || (stream.phase == StreamPhase::Quiescing
                        && stream.retirements == stream.required)
            });
            if consumers_retired && session.obligations.iter().any(|o| o.provider == *provider) {
                selected.authorized_borrowers.insert(session.id.clone());
            }
            if work.phase == LifecyclePhase::StreamPrepare
                && work.provider_graph.as_ref().is_some_and(|graph| {
                    graph.order.contains(provider) && graph.dependencies.contains_key(provider)
                })
                && let LifecycleTarget::Stream {
                    stream_id,
                    generation,
                    ..
                } = &work.target
                && session.streams.get(stream_id).is_some_and(|stream| {
                    stream.generation == *generation && stream.phase == StreamPhase::Preparing
                })
            {
                let mut stages = journal.composition.staged_outputs().filter(|stage| {
                    stage.target == work.target && stage.after.producer == *provider
                });
                if let Some(stage) = stages.next()
                    && stages.next().is_none()
                    && journal
                        .composition
                        .staged_for(&work.target, provider, &stage.after.output_id)
                        .ok()
                        == Some(stage)
                    && stage.after.domain_observations.iter().any(|observation| {
                        observation.complete
                            && observation.borrowers.get(stream_id) == Some(generation)
                    })
                {
                    selected.authorized_borrowers.insert(stream_id.clone());
                }
            }
            selected.authorized_borrowers.extend(
                session
                    .streams
                    .iter()
                    .filter(|(_, stream)| {
                        stream.binding_owner == *provider
                            && (stream.phase == StreamPhase::Retired
                                || (stream.phase == StreamPhase::Quiescing
                                    && stream.retirements == stream.required))
                    })
                    .map(|(id, _)| id.clone()),
            );
        }
        Ok(selections)
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
                if !work.providers.is_empty() {
                    self.fail_work(&key, "action_provider_cleanup_not_implemented")?;
                    continue;
                }
                if let Err(reason) = self.complete_action_barrier(&key, &work) {
                    self.fail_work(&key, &reason)?;
                }
                continue;
            }
            if matches!(
                work.phase,
                LifecyclePhase::StreamRelease | LifecyclePhase::SessionRelease
            ) && work.provider_graph.is_none()
            {
                self.fail_work(&key, "provider_dependency_graph_missing")?;
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
                let composition_stream = work.phase == LifecyclePhase::StreamPrepare
                    && work
                        .provider_graph
                        .as_ref()
                        .is_some_and(|graph| graph.order == work.providers)
                    && self.settings.finalize_policies.iter().any(|policy| {
                        work.providers.contains(&policy.owner_provider)
                            && self.settings.participants.iter().any(|participant| {
                                participant.provider == policy.owner_provider
                                    && participant.contract_version
                                        == amaterasu_extension_api::COMPOSITION_VERSION
                            })
                    });
                if composition_stream && work.candidate.is_none() {
                    let outcome = if work
                        .composition_progress
                        .as_ref()
                        .and_then(|progress| progress.finalize.as_ref())
                        .is_none_or(|finalize| {
                            finalize.owner_plan.phase != PreparePhase::FinalizePlanAdmitted
                        }) {
                        self.advance_standalone_finalize_plan(&key, &work, send)
                            .map(|_| ())
                    } else if work
                        .composition_progress
                        .as_ref()
                        .and_then(|progress| progress.finalize.as_ref())
                        .and_then(|finalize| finalize.dispatch.as_ref())
                        .is_some_and(|dispatch| {
                            matches!(
                                dispatch.phase,
                                super::finalize_dispatch::FinalizePhase::Confirmed { .. }
                            )
                        })
                    {
                        self.commit_standalone_finalize(&key, &work)
                    } else {
                        self.advance_standalone_finalize_effect(&key, &work, send)
                            .map(|_| ())
                    };
                    if let Err(reason) = outcome {
                        // Finalization may already have crossed its effect boundary.
                        // Preserve the exact work, permit and dispatch evidence for
                        // recovery instead of replacing it with a cleanup work item.
                        let mut next = self.shared.borrow().value.clone();
                        let receipt = next
                            .receipts
                            .get_mut(&key)
                            .ok_or("transition_no_longer_active")?;
                        receipt.response["status"] = json!("recovery_required");
                        receipt.response["reason"] = json!(reason);
                        save(&self.shared, next)?;
                    }
                    continue;
                }
                if let Err(reason) = self.complete_work(&key, &work) {
                    self.fail_work(&key, &reason)?;
                }
                continue;
            }
            let provider = &work.providers[work.index];
            if matches!(
                work.phase,
                LifecyclePhase::StreamRelease | LifecyclePhase::SessionRelease
            ) && work.cleanup_blocked(provider)
            {
                self.cleanup_failure(&key, "dependent_cleanup_unresolved")?;
                continue;
            }
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
            // Unsupported v2 phases must be rejected before settings discovery or
            // any provider call. A session output alone does not authorize a
            // stream attachment or cleanup callback.
            if self.settings.participants.iter().any(|participant| {
                &participant.provider == provider
                    && participant.contract_version == amaterasu_extension_api::COMPOSITION_VERSION
            }) && !matches!(
                work.phase,
                LifecyclePhase::SessionPrepare
                    | LifecyclePhase::StreamPrepare
                    | LifecyclePhase::StreamRelease
            ) {
                self.fail_work(&key, "composition_path_not_supported")?;
                continue;
            }
            match self.ensure_settings(&work, send) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => {
                    if let Ok(job_key) = serde_json::to_string(&work.target) {
                        self.settings_jobs.remove(&job_key);
                    }
                    self.fail_work(&key, "settings_snapshot_unavailable")?;
                    continue;
                }
            }
            let effective_settings = self.effective_settings(&work)?;
            let Some(participant) = effective_settings
                .participants
                .iter()
                .find(|p| &p.provider == provider)
                .cloned()
            else {
                if matches!(
                    work.phase,
                    LifecyclePhase::StreamRelease | LifecyclePhase::SessionRelease
                ) {
                    self.cleanup_failure(&key, "provider_unavailable")?;
                } else {
                    self.fail_work(&key, "provider_unavailable")?;
                }
                continue;
            };
            let standalone_v2 = participant.contract_version
                == amaterasu_extension_api::COMPOSITION_VERSION
                && matches!(
                    work.phase,
                    LifecyclePhase::SessionPrepare
                        | LifecyclePhase::StreamPrepare
                        | LifecyclePhase::StreamRelease
                )
                && work.provider_graph.as_ref().is_some_and(|graph| {
                    graph.order == [participant.provider.clone()]
                        && graph
                            .dependencies
                            .get(&participant.provider)
                            .is_some_and(Vec::is_empty)
                })
                && self.settings.control_policies.is_empty()
                && self
                    .settings
                    .finalize_policies
                    .iter()
                    .filter(|policy| policy.owner_provider == participant.provider)
                    .count()
                    == 1;
            // Qualification startup remains fenced for dependent graphs until
            // composition, cleanup and recovery pass end-to-end tests.
            let composed_v2 = participant.contract_version
                == amaterasu_extension_api::COMPOSITION_VERSION
                && matches!(
                    work.phase,
                    LifecyclePhase::SessionPrepare | LifecyclePhase::StreamPrepare
                )
                && work.provider_graph.as_ref().is_some_and(|graph| {
                    graph.order.len() > 1
                        && graph.order.contains(provider)
                        && graph.dependencies.contains_key(provider)
                });
            if participant.contract_version != 1 && !standalone_v2 && !composed_v2 {
                self.fail_work(&key, "composition_path_not_supported")?;
                continue;
            }
            if standalone_v2 && work.phase == LifecyclePhase::StreamRelease {
                let preflight = {
                    let shared = self.shared.borrow();
                    super::stream_release::validate_release_preflight(
                        &shared.value,
                        &work,
                        provider,
                    )
                };
                if let Err(reason) = preflight {
                    self.cleanup_failure(&key, &reason)?;
                    continue;
                }
            }
            if composed_v2
                && work.phase == LifecyclePhase::StreamPrepare
                && !work.states.contains_key(provider)
                && work
                    .provider_graph
                    .as_ref()
                    .and_then(|graph| graph.dependencies.get(provider))
                    .is_some_and(|dependencies| !dependencies.is_empty())
            {
                match self.advance_owner_observations(&key, &work, provider, send) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(reason) => {
                        let mut next = self.shared.borrow().value.clone();
                        let receipt = next
                            .receipts
                            .get_mut(&key)
                            .ok_or("transition_no_longer_active")?;
                        receipt.response["status"] = json!("recovery_required");
                        receipt.response["reason"] = json!(reason);
                        save(&self.shared, next)?;
                        continue;
                    }
                }
            }
            let runtime_key = format!("{key}/{}", work.index);
            let mut authority = match self.authority_for_work(&work) {
                Ok(a) => a,
                Err(_) => continue,
            };
            if !work.states.contains_key(provider) {
                let has_dependencies = work
                    .provider_graph
                    .as_ref()
                    .and_then(|graph| graph.dependencies.get(provider))
                    .is_some_and(|dependencies| !dependencies.is_empty());
                let dependency_inputs = if composed_v2
                    && work.phase == LifecyclePhase::StreamPrepare
                    && has_dependencies
                {
                    match self.freeze_dependent_inputs(&key, &work, provider) {
                        Ok(inputs) => inputs,
                        Err(reason) => {
                            self.fail_work(&key, &reason)?;
                            continue;
                        }
                    }
                } else if (standalone_v2 || composed_v2)
                    && work.phase == LifecyclePhase::StreamPrepare
                {
                    let resolved = {
                        let shared = self.shared.borrow();
                        super::composition_prepare::standalone_stream_input(
                            &shared.value,
                            &effective_settings,
                            &work,
                            provider,
                        )
                    };
                    match resolved {
                        Ok(input) => vec![input],
                        Err(reason) => {
                            self.fail_work(&key, &reason)?;
                            continue;
                        }
                    }
                } else if standalone_v2 && work.phase == LifecyclePhase::StreamRelease {
                    let shared = self.shared.borrow();
                    let mut results = shared.value.finalize_results.records().filter(|record| {
                        record.request.target == work.target
                            && record.selection.policy.owner_provider == *provider
                    });
                    let result = results.next().ok_or("finalize_result_missing")?;
                    if results.next().is_some() {
                        return Err("finalize_result_ambiguous".into());
                    }
                    let amaterasu_extension_api::ResourceControlResponse::Applied {
                        output, ..
                    } = &result.applied
                    else {
                        return Err("finalize_output_missing".into());
                    };
                    vec![(**output).clone()]
                } else {
                    vec![]
                };
                let (sequence, operation_id) = authority.allocate(provider)?;
                let request = ProviderRequest {
                    provider_sequence: sequence,
                    operation: ProviderOperation::ResourcePlan,
                    version: participant.contract_version,
                    transition_id: work.transition_id.clone(),
                    operation_id,
                    target: work.target.clone(),
                    resource_generation: if participant.contract_version
                        == amaterasu_extension_api::COMPOSITION_VERSION
                        && self
                            .settings
                            .finalize_policies
                            .iter()
                            .any(|policy| policy.owner_provider == *provider)
                    {
                        let shared = self.shared.borrow();
                        let (_, session_id, _, _) = super::operations::target_parts(&work.target)?;
                        let session = shared
                            .value
                            .sessions
                            .get(session_id)
                            .ok_or("unknown_session")?;
                        match session.resource_generations.get(provider) {
                            Some(generation) if *generation > 0 => Some(*generation),
                            None if work.phase == LifecyclePhase::SessionPrepare
                                && session.phase == SessionPhase::Preparing =>
                            {
                                Some(1)
                            }
                            _ => return Err("resource_generation_unavailable".into()),
                        }
                    } else {
                        None
                    },
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
                    dependency_inputs,
                    control_results: vec![],
                };
                let state = PrepareState {
                    provider: provider.clone(),
                    participant: participant.clone(),
                    request,
                    phase: PreparePhase::Plan,
                    plan: None,
                    admission_id: None,
                    pending_receipt: None,
                    pending_dispatch: None,
                    status_polls: 0,
                    last_sequence: sequence,
                    dispatch: None,
                    terminal_dispatch: None,
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
            if composed_v2 && work.states[provider].phase == PreparePhase::ControlsPending {
                match self.advance_dependent_controls(&key, &work, provider, send) {
                    Ok(Some(results)) => {
                        let mut state = work.states[provider].clone();
                        state.request.operation = ProviderOperation::LifecycleCallback;
                        state.request.approved_plan = state.plan.clone();
                        state.request.plan_revision = state.plan.as_ref().map(|plan| plan.revision);
                        state.request.admission_id = state.admission_id.clone();
                        state.request.control_results = results;
                        amaterasu_extension_api::validate_provider_request(&state.request)?;
                        state.phase = PreparePhase::Callback;
                        WorkWriter {
                            shared: self.shared.clone(),
                            key: key.clone(),
                            provider: provider.clone(),
                        }
                        .persist(&state)?;
                        self.drivers.remove(&runtime_key);
                    }
                    Ok(None) => {}
                    Err(reason) => {
                        let mut next = self.shared.borrow().value.clone();
                        let receipt = next
                            .receipts
                            .get_mut(&key)
                            .ok_or("transition_no_longer_active")?;
                        receipt.response["status"] = json!("recovery_required");
                        receipt.response["reason"] = json!(reason);
                        save(&self.shared, next)?;
                    }
                }
                continue;
            }
            if !self.drivers.contains_key(&runtime_key) {
                let driver = if composed_v2 {
                    PrepareDriver::new_composed(work.states[provider].clone(), &effective_settings)?
                } else if standalone_v2 && work.phase == LifecyclePhase::StreamRelease {
                    PrepareDriver::new_standalone_release(
                        work.states[provider].clone(),
                        &effective_settings,
                    )?
                } else if standalone_v2 {
                    PrepareDriver::new_standalone_prepare(
                        work.states[provider].clone(),
                        &effective_settings,
                    )?
                } else {
                    PrepareDriver::new(work.states[provider].clone(), &effective_settings)?
                };
                self.drivers.insert(runtime_key.clone(), driver);
            }
            let mut writer = WorkWriter {
                shared: self.shared.clone(),
                key: key.clone(),
                provider: provider.clone(),
            };
            let result = self.drivers.get_mut(&runtime_key).unwrap().advance(
                &effective_settings,
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
                if let Some(ProviderResponse::Prepared { outputs, .. }) = &state.response
                    && !outputs.is_empty()
                {
                    // The current authority supports one materialized output per
                    // admission. Never partially publish a multi-output result.
                    if outputs.len() != 1 {
                        self.fail_work(&key, "multiple_outputs_not_supported")?;
                        self.drivers.remove(&runtime_key);
                        continue;
                    }
                    let output = &outputs[0];
                    let resource = crate::planning::ResourceRef {
                        provider: output.resource.provider.clone(),
                        resource_id: output.resource.resource_id.clone(),
                        incarnation: output.resource.incarnation.clone(),
                        revision: output.resource.revision,
                        execution_domain: output.resource.execution_domain.clone(),
                        mutation_domains: output
                            .resource
                            .mutation_domains
                            .iter()
                            .cloned()
                            .chain(output.resource.mutation_domain.iter().cloned())
                            .collect(),
                    };
                    let writer = WorkFinishWriter {
                        shared: self.shared.clone(),
                        key: key.clone(),
                        provider: provider.clone(),
                        state: state.clone(),
                        settings: self.settings.clone(),
                        selections: self.selections.clone(),
                    };
                    let mut finish = CoordinatorPrepareAuthority::new(
                        self.shared.borrow().value.authority.clone(),
                        self.work_selections(&work)?,
                        writer,
                    )?;
                    let admission = state.admission_id.as_deref().ok_or("admission_missing")?;
                    if let Err(reason) =
                        finish.finish_admission_with_output(provider, admission, resource)
                    {
                        self.fail_work(&key, &reason)?;
                    }
                    self.drivers.remove(&runtime_key);
                    continue;
                }
                if state.request.version == amaterasu_extension_api::COMPOSITION_VERSION
                    && work.phase == LifecyclePhase::SessionPrepare
                    && work
                        .provider_graph
                        .as_ref()
                        .is_some_and(|graph| graph.order.len() > 1)
                    && matches!(&state.response, Some(ProviderResponse::Prepared { outputs, .. }) if outputs.is_empty())
                {
                    let admission = state.admission_id.as_deref().ok_or("admission_missing")?;
                    let writer = SessionPrepareWriter {
                        shared: self.shared.clone(),
                        key: key.clone(),
                        provider: provider.clone(),
                        state: state.clone(),
                    };
                    let mut finish = CoordinatorPrepareAuthority::new(
                        self.shared.borrow().value.authority.clone(),
                        self.work_selections(&work)?,
                        writer,
                    )?;
                    if let Err(reason) = finish.finish_admission_retained(provider, admission) {
                        self.fail_work(&key, &reason)?;
                    }
                    self.drivers.remove(&runtime_key);
                    continue;
                }
                if composed_v2
                    && work.phase == LifecyclePhase::StreamPrepare
                    && matches!(&state.response, Some(ProviderResponse::Prepared { outputs, .. }) if outputs.is_empty())
                {
                    let admission = state.admission_id.as_deref().ok_or("admission_missing")?;
                    let writer = DependentFinishWriter {
                        shared: self.shared.clone(),
                        key: key.clone(),
                        provider: provider.clone(),
                        state: state.clone(),
                    };
                    let mut finish = CoordinatorPrepareAuthority::new(
                        self.shared.borrow().value.authority.clone(),
                        self.work_selections(&work)?,
                        writer,
                    )?;
                    if let Err(reason) = finish.finish_admission_retained(provider, admission) {
                        self.fail_work(&key, &reason)?;
                    }
                    self.drivers.remove(&runtime_key);
                    continue;
                }
                if standalone_v2 && work.phase == LifecyclePhase::StreamRelease {
                    let admission = state.admission_id.as_deref().ok_or("admission_missing")?;
                    let writer = StreamReleaseWriter {
                        shared: self.shared.clone(),
                        key: key.clone(),
                        provider: provider.clone(),
                        state: state.clone(),
                    };
                    let mut finish = CoordinatorPrepareAuthority::new(
                        self.shared.borrow().value.authority.clone(),
                        self.work_selections(&work)?,
                        writer,
                    )?;
                    if let Err(reason) = finish.finish_admission_retained(provider, admission) {
                        self.cleanup_failure(&key, &reason)?;
                    }
                    self.drivers.remove(&runtime_key);
                    continue;
                }
                if standalone_v2 {
                    self.fail_work(&key, "composition_output_missing")?;
                    self.drivers.remove(&runtime_key);
                    continue;
                }
                let fully_released = match &state.response {
                    Some(ProviderResponse::Cleanup { outcomes, .. }) => outcomes
                        .iter()
                        .all(|o| o.state == amaterasu_extension_api::CleanupState::Released),
                    _ => true,
                };
                if fully_released && let Some(admission) = &state.admission_id {
                    let resource = match &state.response {
                        Some(ProviderResponse::Prepared {
                            candidate: Some(candidate),
                            ..
                        }) => {
                            let plan = state.plan.as_ref().ok_or("candidate_plan_missing")?;
                            let (_, _, _, generation) =
                                super::operations::target_parts(&work.target)?;
                            crate::plan_validation::validate_candidate(
                                plan,
                                candidate,
                                generation,
                                candidate.geometry_revision,
                            )?;
                            Some(crate::planning::ResourceRef {
                                provider: candidate.resource.provider.clone(),
                                resource_id: candidate.resource.resource_id.clone(),
                                incarnation: candidate.resource.incarnation.clone(),
                                revision: candidate.resource.revision,
                                execution_domain: candidate.resource.execution_domain.clone(),
                                mutation_domains: candidate
                                    .resource
                                    .mutation_domains
                                    .iter()
                                    .cloned()
                                    .chain(candidate.resource.mutation_domain.iter().cloned())
                                    .collect(),
                            })
                        }
                        _ => None,
                    };
                    authority.finish_admission_with_resource(provider, admission, resource)?;
                }
                let mut composition_cleanup_blocked = false;
                if let Some(ProviderResponse::Cleanup { outcomes, .. }) = &state.response {
                    let settled = self.shared.borrow().value.authority.settled.clone();
                    let owned = {
                        let (_, session_id, _, _) = super::operations::target_parts(&work.target)?;
                        self.shared.borrow().value.sessions[session_id]
                            .admission_ids
                            .clone()
                    };
                    for (id, admission) in settled {
                        if admission.provider != *provider || !owned.contains(&id) {
                            continue;
                        }
                        let eligible = {
                            let journal = self.shared.borrow();
                            let (_, session_id, _, _) =
                                super::operations::target_parts(&work.target)?;
                            cleanup_admission_matches(
                                &journal.value,
                                &journal.value.sessions[session_id],
                                &id,
                                &work.target,
                                &work.phase,
                            )
                        };
                        match eligible {
                            Ok(true) => {}
                            Ok(false) => continue,
                            Err(_) => {
                                composition_cleanup_blocked = true;
                                continue;
                            }
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
                            if self.shared.borrow().value.references_admission(&id) {
                                composition_cleanup_blocked = true;
                            } else {
                                authority.retire_settled(provider, &id, &matched)?;
                            }
                        }
                    }
                }
                if composition_cleanup_blocked {
                    self.cleanup_failure(&key, "composition_output_cleanup_unconfirmed")?;
                    self.drivers.remove(&runtime_key);
                    continue;
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
        selected.authorized_borrowers = super::action_endpoints::action_borrower_scope(
            &journal,
            &work.target,
            &provider,
            &plan,
        )
        .map_err(str::to_owned)?;
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
                    // A failed preparation may be retained for explicit recovery,
                    // while its unresolved effects are already fenced by this
                    // cleanup work. Independent branches can then proceed.
                    && !(receipt.response["status"] == "recovery_required"
                        && other.states.iter().any(|(_, state)| !matches!(state.phase, PreparePhase::Prepared | PreparePhase::Rejected))
                        && other.states.iter().filter(|(_, state)| !matches!(state.phase, PreparePhase::Prepared | PreparePhase::Rejected))
                            .all(|(provider, _)| work.cleanup_blockers.contains(provider)))
                    && matches!(
                        other.phase,
                        LifecyclePhase::SessionPrepare | LifecyclePhase::StreamPrepare
                    )
                    && super::operations::target_parts(&other.target).is_ok_and(
                        |(_, other_session, other_stream, _)| {
                            work.action_transfer.is_some()
                                || (other_session == id
                                    && (stream_id.is_none() || other_stream == stream_id))
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
        apply_participant(&mut next, key, provider, state, false)?;
        save(&self.shared, next)
    }

    fn complete_work(&self, key: &str, work: &Work) -> ModuleResult<()> {
        if let Some(reason) = &work.failure {
            return self.fail_work(key, reason);
        }
        let mut next = self.shared.borrow().value.clone();
        let (_, id, stream_id, _) = super::operations::target_parts(&work.target)?;
        let mut finalize_owners = next.finalize_results.records().filter_map(|record| {
            if record.request.target != work.target
                || record.request.transition_id != work.transition_id
            {
                return None;
            }
            match &record.applied {
                amaterasu_extension_api::ResourceControlResponse::Applied {
                    candidate: Some(candidate),
                    ..
                } if Some(candidate.as_ref()) == work.candidate.as_ref() => {
                    Some(record.selection.policy.owner_provider.clone())
                }
                _ => None,
            }
        });
        let finalize_owner = finalize_owners.next();
        if finalize_owners.next().is_some() {
            return Err("capture_owner_ambiguous".into());
        }
        drop(finalize_owners);
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
        let deferred=work.states.values().any(|state|matches!(&state.response,Some(ProviderResponse::Cleanup{outcomes,..}) if outcomes.iter().any(|o|o.state==amaterasu_extension_api::CleanupState::DeferredRestoration)));
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
                    .or(finalize_owner)
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
                if !deferred
                    && session.admission_ids.iter().any(|admission| {
                        next.authority.admissions.contains_key(admission)
                            || next.authority.settled.contains_key(admission)
                            || next.composition.references_admission(admission)
                            || next.session_releases.references_admission(admission)
                            || next.stream_releases.references_admission(admission)
                            || next
                                .finalize_results
                                .records()
                                .any(|record| &record.request.admission_id == admission)
                            || next
                                .control_settlements
                                .records()
                                .any(|record| &record.settlement.request.admission_id == admission)
                    })
                {
                    return Err("session_cleanup_obligations_unresolved".into());
                }
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
        if let Some(provider) = work.providers.get(work.index) {
            work.cleanup_blockers.insert(provider.clone());
        }
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
            next.receipts.get_mut(key).unwrap().response["reason"] = json!(reason);
            if another_release {
                // Transfer uncertain providers to the existing cleanup fence before
                // it can run another callback. Its graph may predate this work;
                // if graphs differ, discard proof of independence altogether.
                let blockers: BTreeSet<_> = work
                    .states
                    .iter()
                    .filter_map(|(provider, state)| {
                        (!matches!(state.phase, PreparePhase::Prepared | PreparePhase::Rejected))
                            .then_some(provider.clone())
                    })
                    .collect();
                for (other_key, other) in &mut next.receipts {
                    if other_key == key || other.response["status"] != "pending" {
                        continue;
                    }
                    if let Some(release) = other.work.as_mut()
                        && release.phase == phase
                        && release.target == work.target
                    {
                        if serde_json::to_value(&release.provider_graph).ok()
                            != serde_json::to_value(&work.provider_graph).ok()
                        {
                            release.provider_graph = None;
                        }
                        release.cleanup_blockers.extend(blockers.iter().cloned());
                    }
                }
                let receipt = next.receipts.get_mut(key).unwrap();
                // An admitted end operation already owns cleanup. Do not issue
                // a second set of restoration callbacks for the same target.
                receipt.response["status"] = json!(if uncertain {
                    "recovery_required"
                } else {
                    "failed"
                });
            } else {
                let receipt = next.receipts.get_mut(key).unwrap();
                receipt.work = Some(Work {
                    composition_progress: None,
                    owner_observations: Default::default(),
                    phase,
                    target: work.target.clone(),
                    intent: None,
                    reason: Some(ReleaseReason::FailedStart),
                    video: work.video.clone(),
                    transition_id: identifier()?,
                    providers,
                    provider_graph: work.provider_graph.clone(),
                    cleanup_blockers: work
                        .states
                        .iter()
                        .filter_map(|(provider, state)| {
                            (!matches!(
                                state.phase,
                                PreparePhase::Prepared | PreparePhase::Rejected
                            ))
                            .then_some(provider.clone())
                        })
                        .collect(),
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

#[cfg(test)]
mod admission_target_tests {
    use super::*;

    fn session(id: &str) -> SessionRecord {
        SessionRecord {
            provider_graph: None,
            host_epoch: "epoch".into(),
            id: id.into(),
            generation: 1,
            phase: SessionPhase::Ready,
            owner: crate::action_authority::Principal {
                caller: "caller".into(),
                initiator: "owner".into(),
            },
            delegates: Default::default(),
            application_ref: json!({}),
            settings_revision: "revision".into(),
            application_retired: false,
            streams: Default::default(),
            obligations: vec![],
            required_consumers: None,
            application: None,
            application_operation_id: None,
            admission_ids: Default::default(),
            admission_targets: Default::default(),
            admission_phases: Default::default(),
            resource_generations: Default::default(),
            provider_settings: Default::default(),
        }
    }

    #[test]
    fn admission_target_survives_roundtrip_and_rejects_rebinding() {
        let mut journal = Journal {
            version: 3,
            host_epoch: "epoch".into(),
            receipts: Default::default(),
            sessions: [
                ("first".into(), session("first")),
                ("second".into(), session("second")),
            ]
            .into(),
            authority: Default::default(),
            inputs: Default::default(),
            actions: Default::default(),
            input_bindings: Default::default(),
            revoked_inputs: Default::default(),
            recovery_sources: Default::default(),
            input_boot_epochs: Default::default(),
            retention: Default::default(),
            composition: Default::default(),
            control_settlements: Default::default(),
            finalize_results: Default::default(),
            stream_releases: Default::default(),
            session_releases: Default::default(),
            session_prepare_evidence: Default::default(),
            admission_transitions: Default::default(),
        };
        let target = LifecycleTarget::Stream {
            host_epoch: "epoch".into(),
            session_id: "first".into(),
            stream_id: "stream".into(),
            generation: 7,
        };
        record_admission(
            &mut journal,
            "first",
            "admission",
            &target,
            &LifecyclePhase::StreamPrepare,
            "transition",
        )
        .unwrap();
        record_admission(
            &mut journal,
            "first",
            "admission",
            &target,
            &LifecyclePhase::StreamPrepare,
            "transition",
        )
        .unwrap();
        assert!(
            record_admission(
                &mut journal,
                "first",
                "admission",
                &target,
                &LifecyclePhase::StreamPrepare,
                "other-transition",
            )
            .is_err()
        );
        let mut legacy = journal.clone();
        legacy.admission_transitions.clear();
        assert!(
            record_admission(
                &mut legacy,
                "first",
                "admission",
                &target,
                &LifecyclePhase::StreamPrepare,
                "transition",
            )
            .is_err()
        );
        let changed = LifecycleTarget::Stream {
            host_epoch: "epoch".into(),
            session_id: "first".into(),
            stream_id: "stream".into(),
            generation: 8,
        };
        assert!(
            cleanup_admission_matches(
                &journal,
                &journal.sessions["first"],
                "admission",
                &target,
                &LifecyclePhase::StreamRelease
            )
            .unwrap()
        );
        assert!(
            !cleanup_admission_matches(
                &journal,
                &journal.sessions["first"],
                "admission",
                &changed,
                &LifecyclePhase::StreamRelease
            )
            .unwrap()
        );
        assert!(
            cleanup_admission_matches(
                &legacy,
                &legacy.sessions["first"],
                "admission",
                &target,
                &LifecyclePhase::StreamRelease
            )
            .is_err()
        );
        assert!(
            record_admission(
                &mut journal,
                "first",
                "admission",
                &changed,
                &LifecyclePhase::StreamPrepare,
                "transition"
            )
            .is_err()
        );
        assert!(
            record_admission(
                &mut journal,
                "first",
                "admission",
                &target,
                &LifecyclePhase::StreamRelease,
                "transition"
            )
            .is_err()
        );
        let foreign = LifecycleTarget::Session {
            host_epoch: "epoch".into(),
            session_id: "second".into(),
            generation: 1,
        };
        assert!(
            record_admission(
                &mut journal,
                "second",
                "admission",
                &foreign,
                &LifecyclePhase::SessionPrepare,
                "transition"
            )
            .is_err()
        );
        let restored: Journal =
            serde_json::from_slice(&serde_json::to_vec(&journal).unwrap()).unwrap();
        assert_eq!(restored.admission_transitions["admission"], "transition");
        assert_eq!(
            restored.sessions["first"].admission_targets["admission"],
            target
        );
        assert_eq!(
            restored.sessions["first"].admission_phases["admission"],
            LifecyclePhase::StreamPrepare
        );
        assert!(restored.sessions["second"].admission_ids.is_empty());
    }
}

#[cfg(test)]
mod dependency_cleanup_tests {
    use super::*;

    fn work() -> Work {
        Work {
            composition_progress: None,
            owner_observations: Default::default(),
            phase: LifecyclePhase::SessionRelease,
            target: LifecycleTarget::Session {
                host_epoch: "epoch".into(),
                session_id: "session".into(),
                generation: 1,
            },
            intent: None,
            reason: Some(ReleaseReason::End),
            video: None,
            transition_id: "transition".into(),
            providers: vec![
                "independent".into(),
                "leaf".into(),
                "middle".into(),
                "root".into(),
            ],
            provider_graph: Some(ProviderGraph {
                order: vec![
                    "root".into(),
                    "middle".into(),
                    "leaf".into(),
                    "independent".into(),
                ],
                dependencies: [
                    ("root".into(), vec![]),
                    ("middle".into(), vec!["root".into()]),
                    ("leaf".into(), vec!["middle".into()]),
                    ("independent".into(), vec![]),
                ]
                .into(),
            }),
            cleanup_blockers: ["leaf".into()].into(),
            index: 0,
            states: Default::default(),
            candidate: None,
            waiting_retirement: false,
            failure: None,
            failed_preparation: None,
            action_transfer: None,
            recovery_authorized_epoch: None,
        }
    }

    #[test]
    fn unresolved_dependent_fences_all_prerequisites_after_restart() {
        let original = work();
        let recovered: Work =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        assert!(recovered.cleanup_blocked("leaf"));
        assert!(recovered.cleanup_blocked("middle"));
        assert!(recovered.cleanup_blocked("root"));
        assert!(!recovered.cleanup_blocked("independent"));
    }

    #[test]
    fn scheduler_persists_unknown_dependent_fence_before_skipping_prerequisite() {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-dependency-cleanup-{}",
            identifier().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let journal_path = path.join("journal.json");
        let mut coordinator = Coordinator::open(&journal_path).unwrap();
        coordinator.configured = true;
        let receipt = coordinator.handle(
            "caller",
            "initiator",
            json!({
                "operation": "prepare", "operation_id": "operation"
            }),
        );
        let key = receipt_key("caller", "initiator", "operation");
        let mut next = coordinator.shared.borrow().value.clone();
        let target: LifecycleTarget = serde_json::from_value(receipt["target"].clone()).unwrap();
        let session_id = super::super::operations::target_parts(&target)
            .unwrap()
            .1
            .to_owned();
        let mut release = work();
        release.target = target;
        release.providers = vec!["root".into()];
        next.sessions.get_mut(&session_id).unwrap().phase = SessionPhase::Ending;
        next.receipts.get_mut(&key).unwrap().work = Some(release);
        save(&coordinator.shared, next).unwrap();
        coordinator
            .advance_work(&mut |_| panic!("blocked prerequisite must not be called"))
            .unwrap();
        let saved = coordinator.shared.borrow().value.receipts[&key]
            .work
            .clone()
            .unwrap();
        assert_eq!(saved.index, 1);
        assert!(saved.cleanup_blockers.contains("root"));
        assert!(saved.cleanup_blockers.contains("leaf"));
        drop(coordinator);
        let recovered = Coordinator::open(&journal_path).unwrap();
        let saved = recovered.shared.borrow().value.receipts[&key]
            .work
            .clone()
            .unwrap();
        assert!(saved.cleanup_blocked("root"));
        drop(recovered);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn missing_or_invalid_graph_never_establishes_independence() {
        let mut pending = work();
        pending.provider_graph = None;
        assert!(pending.cleanup_blocked("independent"));
        pending.provider_graph = work().provider_graph;
        pending
            .provider_graph
            .as_mut()
            .unwrap()
            .dependencies
            .get_mut("root")
            .unwrap()
            .push("leaf".into());
        assert!(pending.cleanup_blocked("independent"));
        let mut legacy = serde_json::to_value(work()).unwrap();
        legacy.as_object_mut().unwrap().remove("provider_graph");
        legacy.as_object_mut().unwrap().remove("cleanup_blockers");
        let legacy: Work = serde_json::from_value(legacy).unwrap();
        assert!(legacy.cleanup_blocked("root"));
    }
}

#[cfg(test)]
#[path = "cleanup_dispatch_tests.rs"]
mod cleanup_dispatch_tests;

#[cfg(test)]
mod output_finish_tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn exercise_output_finish(mode: u8) {
        let dir =
            std::env::temp_dir().join(format!("lifecycle-output-finish-{}", identifier().unwrap()));
        std::fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let coordinator = Coordinator::open(&dir.join("journal.json")).unwrap();
        let epoch = coordinator.shared.borrow().value.host_epoch.clone();
        let target =
            json!({"kind":"session","host_epoch":epoch,"session_id":"session","generation":1});
        let method = json!({"service":"screen/2","method":"callback","interface_major":2,"interface_minor":1,"schema_revision":1});
        let participant: crate::adapter::ParticipantSettings = serde_json::from_value(json!({"provider":"screen","depends_on":[],"plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"revision","settings":{}})).unwrap();
        let approval: MethodApproval = serde_json::from_value(
            json!({"method":method,"broker_approval":{"provider":"screen","method":method}}),
        )
        .unwrap();
        let selected = ProviderSelection {
            participant: participant.clone(),
            approvals: vec![approval],
            capabilities: Default::default(),
            cleanup_methods: vec![],
            authorized_borrowers: Default::default(),
            approved_control_bindings: Vec::new(),
        };
        let document = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"output/1","type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"]}"#;
        let schema = json!({"id":"output/1","major":1,"minor":0,"digest":format!("{:x}",Sha256::digest(document.as_bytes()))});
        let plan = json!({"plan_id":"plan","revision":1,"resources":[{"provider":"screen","resource_id":"output","incarnation":"native","expected_revision":1,"execution_domain":"seat","mutation_domains":[],"access":"read"}],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]});
        let output = json!({"output_id":"screen-output","capability":"screen/output","schema":schema,"producer":"screen","producer_service":"screen/2","origin":target,"resource":{"provider":"screen","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"data":{"ok":if mode == 2 { json!("bad") } else { json!(true) }}});
        let request = json!({"provider_sequence":1,"operation":"lifecycle_callback","version":2,"transition_id":"transition","operation_id":"callback","target":target,"phase":"session.prepare","settings_revision":"revision","settings":{},"intent":null,"reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"remaining_ms":1000,"plan_revision":1,"admission_id":"admission","approved_plan":plan});
        let mut state: PrepareState = serde_json::from_value(json!({"provider":"screen","participant":participant,"request":request,"phase":"prepared","plan":plan,"admission_id":"admission","pending_receipt":null,"status_polls":0,"last_sequence":1,"dispatch":null,"terminal_dispatch":{"call":{"service":"screen/2","method":"callback","interface_major":2,"interface_minor":1,"approval":{"provider":"screen","method":method},"arguments":request,"timeout_ms":1000,"operation_id":"callback"},"state":{"state":"completed","payload":{"status":"prepared","candidate":null,"obligations":[],"outputs":[output]}}},"response":{"status":"prepared","candidate":null,"obligations":[],"outputs":[output]}})).unwrap();
        if mode == 3 {
            state.terminal_dispatch = None;
        }
        let mut settings = LifecycleSettings::default();
        settings
            .schema_documents
            .push(serde_json::from_value(json!({"reference":schema,"document":document})).unwrap());
        settings.finalize_policies.push(serde_json::from_value(json!({"owner_provider":"screen","owner_service":"screen/2","method":method,"status_method":method,"output_capability":"screen/output","output_schema":schema,"capture_provider":"capture","selector_schema":"selector/1","required_access":"read","mutation_domains":[]})).unwrap());
        let mut next = coordinator.shared.borrow().value.clone();
        let mut session: SessionRecord = serde_json::from_value(json!({"host_epoch":epoch,"id":"session","generation":1,"phase":"preparing","owner":{"caller":"transport","initiator":"client"},"delegates":[],"application_ref":null,"settings_revision":"revision","application_retired":false,"streams":{},"admission_ids":["admission"],"admission_targets":{"admission":target},"admission_phases":{"admission":"session.prepare"}})).unwrap();
        session.provider_graph = Some(
            serde_json::from_value(json!({"order":["screen"],"dependencies":{"screen":[]}}))
                .unwrap(),
        );
        next.sessions.insert("session".into(), session);
        next.admission_transitions
            .insert("admission".into(), "transition".into());
        let admission = crate::prepare_authority::Admission {
            provider: "screen".into(),
            selection: selected.clone(),
            plan: serde_json::from_value(plan).unwrap(),
            rights: vec![],
        };
        next.authority
            .admissions
            .insert("admission".into(), admission.clone());
        let work: Work = serde_json::from_value(json!({"phase":"session.prepare","target":target,"intent":null,"reason":null,"video":null,"transition_id":"transition","providers":["screen"],"provider_graph":{"order":["screen"],"dependencies":{"screen":[]}},"index":0,"states":{"screen":state},"candidate":null,"waiting_retirement":false})).unwrap();
        let key = receipt_key("transport", "client", "operation");
        next.receipts.insert(
            key.clone(),
            Receipt {
                caller: "transport".into(),
                initiator: "client".into(),
                request: json!({"operation_id":"operation"}),
                response: json!({"operation_id":"operation","status":"pending"}),
                work: Some(work),
                wait_target: None,
            },
        );
        save(&coordinator.shared, next.clone()).unwrap();
        let mut resulting_authority = next.authority.clone();
        resulting_authority.admissions.remove("admission");
        resulting_authority
            .settled
            .insert("admission".into(), admission);
        resulting_authority
            .output_admissions
            .insert("admission".into());
        let mut writer = WorkFinishWriter {
            shared: coordinator.shared.clone(),
            key: key.clone(),
            provider: "screen".into(),
            state,
            settings,
            selections: [("screen".into(), selected)].into(),
        };
        if mode == 1 {
            let before = std::fs::read(dir.join("journal.json")).unwrap();
            let moved = dir.with_extension("moved");
            std::fs::rename(&dir, &moved).unwrap();
            assert!(writer.persist(&resulting_authority).is_err());
            assert_eq!(std::fs::read(moved.join("journal.json")).unwrap(), before);
            assert_eq!(
                coordinator.shared.borrow().value.receipts[&key]
                    .work
                    .as_ref()
                    .unwrap()
                    .index,
                0
            );
            std::fs::rename(&moved, &dir).unwrap();
        } else if mode == 2 || mode == 3 {
            let before = std::fs::read(dir.join("journal.json")).unwrap();
            assert!(writer.persist(&resulting_authority).is_err());
            assert_eq!(std::fs::read(dir.join("journal.json")).unwrap(), before);
            assert_eq!(
                coordinator.shared.borrow().value.receipts[&key]
                    .work
                    .as_ref()
                    .unwrap()
                    .index,
                0
            );
        } else {
            writer.persist(&resulting_authority).unwrap();
            let journal = &coordinator.shared.borrow().value;
            assert_eq!(journal.receipts[&key].work.as_ref().unwrap().index, 1);
            assert!(journal.authority.settled.contains_key("admission"));
            assert!(journal.authority.output_admissions.contains("admission"));
            assert_eq!(journal.composition.records().count(), 1);
        }
        drop(writer);
        drop(coordinator);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn output_and_progress_commit_together() {
        exercise_output_finish(0);
    }
    #[test]
    fn failed_save_publishes_neither_output_nor_progress() {
        exercise_output_finish(1);
    }
    #[test]
    fn invalid_schema_publishes_neither_output_nor_progress() {
        exercise_output_finish(2);
    }
    #[test]
    fn missing_completed_callback_cannot_publish_output() {
        exercise_output_finish(3);
    }
}
