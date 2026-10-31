//! Read-only owner observation before dependent planning.
use super::plan_observation::{
    PlanObservationDriver, PlanObservationJournal, PlanObservationPhase, PlanObservationState,
};
use super::*;
use crate::prepare_driver::{PrepareAuthority, PreparePhase};

fn observation_key(dependent: &str, owner: &str) -> String {
    serde_json::to_string(&(dependent, owner)).expect("string tuple serializes")
}

fn selected_stage<'a>(
    journal: &'a Journal,
    work: &Work,
    owner: &str,
) -> ModuleResult<&'a composition::StagedOutputRefinement> {
    let mut stages = journal
        .composition
        .staged_outputs()
        .filter(|stage| stage.target == work.target && stage.after.producer == owner);
    let stage = stages.next().ok_or("owner_stage_missing")?;
    if stages.next().is_some()
        || journal
            .composition
            .staged_for(&work.target, owner, &stage.after.output_id)
            .ok()
            != Some(stage)
    {
        return Err("owner_stage_ambiguous".into());
    }
    Ok(stage)
}

struct ObservationWriter {
    shared: Shared,
    key: String,
    observation_key: String,
}
impl PlanObservationJournal for ObservationWriter {
    fn persist(&mut self, state: &PlanObservationState) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get_mut(&self.key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        let original = work
            .owner_observations
            .get(&self.observation_key)
            .ok_or("owner_observation_missing")?;
        if original.request != state.request || original.participant != state.participant {
            return Err("owner_observation_identity_changed".into());
        }
        work.owner_observations
            .insert(self.observation_key.clone(), state.clone());
        save(&self.shared, next)
    }
}

fn verify_current(
    journal: &Journal,
    work: &Work,
    owner: &str,
    state: &PlanObservationState,
) -> ModuleResult<()> {
    let stage = selected_stage(journal, work, owner)?;
    if state.participant.provider != owner
        || state.request.target != work.target
        || state.request.transition_id != work.transition_id
        || state.request.dependency_inputs != [stage.after.clone()]
        || work.states.get(owner).is_none_or(|producer| {
            producer.phase != PreparePhase::Prepared || producer.participant != state.participant
        })
    {
        return Err("owner_observation_context_changed".into());
    }
    verify_output_current(journal, owner, &stage.after, state)
}

fn verify_output_current(
    journal: &Journal,
    owner: &str,
    output: &amaterasu_extension_api::ResourceOutput,
    state: &PlanObservationState,
) -> ModuleResult<()> {
    let Some(plan) = state.plan.as_ref() else {
        return Err("owner_observation_plan_missing".into());
    };
    if !plan.control_proposals.is_empty() {
        return Err("owner_observation_has_controls".into());
    }
    let resource = &output.resource;
    if !plan.resources.iter().any(|entry| {
        entry.provider == owner
            && entry.resource_id == resource.resource_id
            && entry.incarnation.as_deref() == Some(&resource.incarnation)
            && entry.expected_revision == Some(resource.revision)
            && entry.execution_domain == resource.execution_domain
    }) {
        return Err("owner_observation_resource_mismatch".into());
    }
    let observed = journal
        .authority
        .snapshot
        .resources
        .get(owner)
        .and_then(|items| items.get(&resource.resource_id))
        .ok_or("owner_resource_observation_missing")?;
    let resource_domains: BTreeSet<_> = resource
        .mutation_domains
        .iter()
        .cloned()
        .chain(resource.mutation_domain.iter().cloned())
        .collect();
    if observed.provider != *owner
        || observed.incarnation != resource.incarnation
        || observed.revision != resource.revision
        || observed.execution_domain != resource.execution_domain
        || observed.mutation_domains != resource_domains
    {
        return Err("owner_resource_observation_changed".into());
    }
    let expected_domains: BTreeSet<_> = resource
        .mutation_domains
        .iter()
        .cloned()
        .chain(resource.mutation_domain.iter().cloned())
        .collect();
    let observed_domains: BTreeSet<_> = output
        .domain_observations
        .iter()
        .map(|observation| observation.domain.clone())
        .collect();
    if expected_domains.is_empty() || expected_domains != observed_domains {
        return Err("owner_domain_coverage_incomplete".into());
    }
    for domain in &output.domain_observations {
        let current = journal
            .authority
            .snapshot
            .domains
            .get(&domain.domain)
            .ok_or("owner_domain_observation_missing")?;
        if !domain.complete
            || journal.authority.refresh_required.contains(&domain.domain)
            || current.revision != domain.revision
            || current.borrowers != domain.borrowers
            || !plan.mutation_domains.iter().any(|entry| {
                entry.domain == domain.domain
                    && entry.expected_revision == domain.revision
                    && entry.borrowers == domain.borrowers
            })
        {
            return Err("owner_domain_observation_changed".into());
        }
    }
    Ok(())
}

impl Coordinator {
    /// Refresh the selected owner's latest release lineage before each reverse-graph step.
    pub(super) fn advance_release_observation(
        &mut self,
        key: &str,
        work: &Work,
        step: &str,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<bool> {
        let graph = work
            .provider_graph
            .as_ref()
            .ok_or("provider_graph_missing")?;
        let owner = graph.order.first().ok_or("provider_graph_missing")?;
        let (_, output) = self.resolve_release_observation_binding(work, step)?;
        let id = observation_key(step, owner);
        let Some(state) = work.owner_observations.get(&id) else {
            let (participant, mut request) = {
                let shared = self.shared.borrow();
                let mut results = shared.value.finalize_results.records().filter(|result| {
                    result.request.target == work.target
                        && result.selection.policy.owner_provider == *owner
                });
                let result = results.next().ok_or("release_finalize_missing")?;
                if results.next().is_some() {
                    return Err("release_finalize_ambiguous".into());
                }
                let stage = result
                    .selection
                    .stage
                    .as_ref()
                    .ok_or("release_stage_missing")?;
                (
                    stage.callback_proof.participant.clone(),
                    stage.callback_proof.request.clone(),
                )
            };
            let mut selections: Vec<_> = self.selections.values().cloned().collect();
            let selected = selections
                .iter_mut()
                .find(|selected| selected.participant.provider == *owner)
                .ok_or("owner_selection_missing")?;
            selected.participant = participant.clone();
            let mut authority = crate::prepare_authority::CoordinatorPrepareAuthority::new(
                self.shared.borrow().value.authority.clone(),
                selections,
                super::providers::AuthorityWriter(self.shared.clone()),
            )?;
            let (sequence, operation_id) = authority.allocate(owner)?;
            request.provider_sequence = sequence;
            request.operation_id = operation_id;
            request.operation = amaterasu_extension_api::ProviderOperation::ResourcePlan;
            request.phase = LifecyclePhase::StreamRelease;
            request.target = work.target.clone();
            request.transition_id = work.transition_id.clone();
            request.reason = work.reason.clone();
            request.intent = None;
            request.requested_video = None;
            request.plan_revision = None;
            request.admission_id = None;
            request.approved_plan = None;
            request.dependency_inputs = vec![output];
            request.control_results.clear();
            let state = PlanObservationState {
                participant,
                request,
                phase: PlanObservationPhase::Ready,
                dispatch: None,
                plan: None,
            };
            let _ = PlanObservationDriver::new(state.clone())?;
            let mut next = self.shared.borrow().value.clone();
            let saved = next
                .receipts
                .get_mut(key)
                .and_then(|receipt| receipt.work.as_mut())
                .ok_or("transition_no_longer_active")?;
            if serde_json::to_value(&*saved).ok() != serde_json::to_value(work).ok()
                || saved.owner_observations.contains_key(&id)
            {
                return Err("owner_observation_changed".into());
            }
            saved.owner_observations.insert(id, state);
            save(&self.shared, next)?;
            return Ok(false);
        };
        if state.request.dependency_inputs != [output.clone()]
            || state.request.target != work.target
            || state.request.transition_id != work.transition_id
            || state.participant.provider != *owner
        {
            return Err("owner_observation_context_changed".into());
        }
        let _ = PlanObservationDriver::new(state.clone())?;
        match state.phase {
            PlanObservationPhase::Planned => {
                let selected = self
                    .selections
                    .get(owner)
                    .ok_or("owner_selection_missing")?;
                let dispatch = state.dispatch.as_ref().ok_or("owner_plan_proof_missing")?;
                if !selected.approvals.iter().any(|approval| {
                    approval.method == state.participant.plan_method
                        && approval.broker_approval == dispatch.call.approval
                        && composition::approval_matches_call(approval, &dispatch.call)
                }) {
                    return Err("owner_plan_approval_changed".into());
                }
                verify_output_current(&self.shared.borrow().value, owner, &output, state)?;
                Ok(true)
            }
            PlanObservationPhase::RecoveryRequired => {
                Err("owner_observation_requires_recovery".into())
            }
            PlanObservationPhase::Ready => {
                let runtime_key = format!("{key}/owner-observation/{id}");
                if !self.observation_drivers.contains_key(&runtime_key) {
                    self.observation_drivers.insert(
                        runtime_key.clone(),
                        PlanObservationDriver::new(state.clone())?,
                    );
                }
                let mut selections: Vec<_> = self.selections.values().cloned().collect();
                let selected = selections
                    .iter_mut()
                    .find(|selected| selected.participant.provider == *owner)
                    .ok_or("owner_selection_missing")?;
                selected.participant = state.participant.clone();
                let mut authority = crate::prepare_authority::CoordinatorPrepareAuthority::new(
                    self.shared.borrow().value.authority.clone(),
                    selections,
                    super::providers::AuthorityWriter(self.shared.clone()),
                )?;
                let mut writer = ObservationWriter {
                    shared: self.shared.clone(),
                    key: key.into(),
                    observation_key: id,
                };
                let result = self
                    .observation_drivers
                    .get_mut(&runtime_key)
                    .expect("driver exists")
                    .advance(&mut authority, &mut writer, send);
                if result.is_err() {
                    self.observation_drivers.remove(&runtime_key);
                }
                result?;
                if self.observation_drivers[&runtime_key].state().phase
                    != PlanObservationPhase::Ready
                {
                    self.observation_drivers.remove(&runtime_key);
                }
                Ok(false)
            }
        }
    }

    /// A fresh authenticated owner plan is read-only and never admitted. The
    /// dependent may freeze inputs only after every direct owner re-observes.
    pub(super) fn advance_owner_observations(
        &mut self,
        key: &str,
        work: &Work,
        dependent: &str,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<bool> {
        let graph = work
            .provider_graph
            .as_ref()
            .ok_or("provider_graph_missing")?;
        let dependencies = graph
            .dependencies
            .get(dependent)
            .ok_or("dependent_not_in_graph")?;
        for owner in dependencies {
            let id = observation_key(dependent, owner);
            let stage = {
                let shared = self.shared.borrow();
                selected_stage(&shared.value, work, owner)?.clone()
            };
            let Some(state) = work.owner_observations.get(&id) else {
                let producer = work.states.get(owner).ok_or("owner_prepare_missing")?;
                if producer.phase != PreparePhase::Prepared
                    || producer.participant.provider != *owner
                {
                    return Err("owner_prepare_incomplete".into());
                }
                let mut authority = self.authority_for_work(work)?;
                let (sequence, operation_id) = authority.allocate(owner)?;
                let mut request = producer.request.clone();
                request.provider_sequence = sequence;
                request.operation_id = operation_id;
                request.operation = amaterasu_extension_api::ProviderOperation::ResourcePlan;
                request.target = work.target.clone();
                request.transition_id = work.transition_id.clone();
                request.plan_revision = None;
                request.admission_id = None;
                request.approved_plan = None;
                request.dependency_inputs = vec![stage.after.clone()];
                request.control_results.clear();
                let state = PlanObservationState {
                    participant: producer.participant.clone(),
                    request,
                    phase: PlanObservationPhase::Ready,
                    dispatch: None,
                    plan: None,
                };
                let _ = PlanObservationDriver::new(state.clone())?;
                let mut next = self.shared.borrow().value.clone();
                let saved = next
                    .receipts
                    .get_mut(key)
                    .and_then(|receipt| receipt.work.as_mut())
                    .ok_or("transition_no_longer_active")?;
                if saved.target != work.target
                    || saved.transition_id != work.transition_id
                    || saved.owner_observations.contains_key(&id)
                {
                    return Err("owner_observation_changed".into());
                }
                saved.owner_observations.insert(id, state);
                save(&self.shared, next)?;
                return Ok(false);
            };
            let producer = work.states.get(owner).ok_or("owner_prepare_missing")?;
            if state.request.dependency_inputs != [stage.after.clone()]
                || state.request.target != work.target
                || state.request.transition_id != work.transition_id
                || state.request.resource_generation != producer.request.resource_generation
                || state.participant != producer.participant
                || producer.phase != PreparePhase::Prepared
            {
                return Err("owner_observation_context_changed".into());
            }
            let _ = PlanObservationDriver::new(state.clone())?;
            match state.phase {
                PlanObservationPhase::Planned => {
                    let selections = self.work_selections(work)?;
                    let selected = selections
                        .iter()
                        .find(|selection| selection.participant.provider == *owner)
                        .ok_or("owner_selection_missing")?;
                    let dispatch = state.dispatch.as_ref().ok_or("owner_plan_proof_missing")?;
                    if selected.participant != state.participant
                        || !selected.approvals.iter().any(|approval| {
                            approval.method == state.participant.plan_method
                                && approval.broker_approval == dispatch.call.approval
                                && composition::approval_matches_call(approval, &dispatch.call)
                        })
                    {
                        return Err("owner_plan_approval_changed".into());
                    }
                    let shared = self.shared.borrow();
                    verify_current(&shared.value, work, owner, state)?;
                }
                PlanObservationPhase::RecoveryRequired => {
                    return Err("owner_observation_requires_recovery".into());
                }
                PlanObservationPhase::Ready => {
                    let runtime_key = format!("{key}/owner-observation/{id}");
                    if !self.observation_drivers.contains_key(&runtime_key) {
                        self.observation_drivers.insert(
                            runtime_key.clone(),
                            PlanObservationDriver::new(state.clone())?,
                        );
                    }
                    let mut authority = self.authority_for_work(work)?;
                    let mut writer = ObservationWriter {
                        shared: self.shared.clone(),
                        key: key.into(),
                        observation_key: id,
                    };
                    let result = self
                        .observation_drivers
                        .get_mut(&runtime_key)
                        .expect("driver exists")
                        .advance(&mut authority, &mut writer, send);
                    if result.is_err() {
                        self.observation_drivers.remove(&runtime_key);
                    }
                    result?;
                    if self.observation_drivers[&runtime_key].state().phase
                        != PlanObservationPhase::Ready
                    {
                        self.observation_drivers.remove(&runtime_key);
                    }
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}

/// Recovery checks identity and completed broker proof only. Native freshness is
/// checked again immediately before any dependent plan or callback.
pub(super) fn validate_work_observations(journal: &Journal, work: &Work) -> bool {
    if work.owner_observations.len() > 32 {
        return false;
    }
    work.owner_observations.iter().all(|(key, state)| {
        let Ok((dependent, owner)) = serde_json::from_str::<(String, String)>(key) else {
            return false;
        };
        if work.phase == LifecyclePhase::StreamRelease {
            let Some(graph) = &work.provider_graph else {
                return false;
            };
            if observation_key(&dependent, &owner) != *key
                || graph.order.first() != Some(&owner)
                || !graph.order.contains(&dependent)
                || (dependent != owner
                    && graph
                        .dependencies
                        .get(&dependent)
                        .is_none_or(|deps| !deps.contains(&owner)))
                || PlanObservationDriver::new(state.clone()).is_err()
                || state.participant.provider != owner
                || state.request.target != work.target
                || state.request.transition_id != work.transition_id
            {
                return false;
            }
            let mut stages = journal
                .composition
                .staged_outputs()
                .filter(|stage| stage.target == work.target && stage.after.producer == owner);
            let Some(stage) = stages.next() else {
                return false;
            };
            if stages.next().is_some()
                || stage.callback_proof.participant != state.participant
                || state.request.resource_generation
                    != stage.callback_proof.request.resource_generation
                || state.request.dependency_inputs.len() != 1
                || amaterasu_extension_api::validate_output_refinement(
                    &stage.after,
                    &state.request.dependency_inputs[0],
                )
                .is_err()
            {
                return false;
            }
            let admission = journal
                .authority
                .settled
                .get(&stage.admission_id)
                .or_else(|| journal.authority.admissions.get(&stage.admission_id));
            return state.dispatch.as_ref().is_none_or(|dispatch| {
                admission.is_some_and(|admission| {
                    admission.provider == owner
                        && admission.selection.participant == state.participant
                        && admission.selection.approvals.iter().any(|approval| {
                            approval.method == state.participant.plan_method
                                && approval.broker_approval == dispatch.call.approval
                                && composition::approval_matches_call(approval, &dispatch.call)
                        })
                })
            });
        }
        if observation_key(&dependent, &owner) != *key
            || work
                .provider_graph
                .as_ref()
                .and_then(|graph| graph.dependencies.get(&dependent))
                .is_none_or(|dependencies| !dependencies.contains(&owner))
            || PlanObservationDriver::new(state.clone()).is_err()
        {
            return false;
        }
        // Recovery may fence the retained root after restart; validate the
        // immutable stage history without treating it as currently usable.
        let mut stages = journal
            .composition
            .staged_outputs()
            .filter(|stage| stage.target == work.target && stage.after.producer == owner);
        let Some(stage) = stages.next() else {
            return false;
        };
        if stages.next().is_some() {
            return false;
        }
        let Some(producer) = work.states.get(&owner) else {
            return false;
        };
        let selected = journal
            .authority
            .settled
            .get(&stage.admission_id)
            .or_else(|| journal.authority.admissions.get(&stage.admission_id));
        let approval_valid = state.dispatch.as_ref().is_none_or(|dispatch| {
            selected.is_some_and(|admission| {
                admission.provider == owner
                    && admission.selection.participant == state.participant
                    && admission.selection.approvals.iter().any(|approval| {
                        approval.method == state.participant.plan_method
                            && approval.broker_approval == dispatch.call.approval
                            && composition::approval_matches_call(approval, &dispatch.call)
                    })
            })
        });
        producer.phase == PreparePhase::Prepared
            && approval_valid
            && producer.participant == state.participant
            && state.participant.provider == owner
            && state.request.target == work.target
            && state.request.transition_id == work.transition_id
            && state.request.resource_generation == producer.request.resource_generation
            && state.request.dependency_inputs == [stage.after.clone()]
    })
}
