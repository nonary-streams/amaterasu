//! Coordinator-owned selection and planning for v2 finalization.
//!
//! A selected policy or serialized output never grants authority. The scheduler
//! must persist this snapshot and acquire fresh finalization rights before any
//! owner effect or capture binding can be dispatched.
use super::composition::OutputSelection;
use super::finalize_selection::FinalizeSelectionSnapshot;
use super::*;

fn approval_for(
    selection: &ProviderSelection,
    method: &amaterasu_extension_api::MethodRef,
) -> ModuleResult<crate::prepare_authority::MethodApproval> {
    let matches: Vec<_> = selection
        .approvals
        .iter()
        .filter(|entry| {
            entry.method.service == method.service
                && entry.method.method == method.method
                && entry.method.interface_major == method.interface_major
                && entry.method.interface_minor == method.interface_minor
                && entry.method.schema_revision == method.schema_revision
        })
        .collect();
    if matches.len() != 1 {
        return Err("finalize_method_not_approved".into());
    }
    Ok(matches[0].clone())
}

/// Select one owner from the frozen graph. Composed graphs additionally require
/// a completed, authenticated control and owner-acknowledgement chain.
pub(super) fn standalone_selection(
    journal: &Journal,
    settings: &LifecycleSettings,
    selections: &BTreeMap<String, ProviderSelection>,
    work: &Work,
    require_current: bool,
) -> ModuleResult<FinalizeSelectionSnapshot> {
    let (_, session_id, stream_id, generation) = super::operations::target_parts(&work.target)?;
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    let stream = session
        .streams
        .get(stream_id.ok_or("stream_target_required")?)
        .ok_or("unknown_stream")?;
    if session.host_epoch != journal.host_epoch
        || stream.generation != generation
        || stream.phase != StreamPhase::Preparing
        || work.phase != LifecyclePhase::StreamPrepare
    {
        return Err("finalize_target_not_preparing".into());
    }
    let graph = work
        .provider_graph
        .as_ref()
        .ok_or("provider_dependency_graph_missing")?;
    if graph.order.is_empty() || graph.order != work.providers {
        return Err("finalize_graph_incomplete".into());
    }
    if graph.order.len() > 1
        && (work.index < work.providers.len()
            || graph.order.iter().any(|provider| {
                work.states.get(provider).is_none_or(|state| {
                    state.phase != crate::prepare_driver::PreparePhase::Prepared
                })
            }))
    {
        return Err("finalize_graph_incomplete".into());
    }
    let policies: Vec<_> = settings
        .finalize_policies
        .iter()
        .filter(|policy| graph.order.contains(&policy.owner_provider))
        .collect();
    if policies.len() != 1 {
        return Err("finalize_policy_ambiguous".into());
    }
    let policy = policies[0];
    let owner = &policy.owner_provider;
    if graph.order.len() == 1
        && settings
            .control_policies
            .iter()
            .any(|policy| policy.owner_provider == *owner)
    {
        return Err("dependent_controls_require_sequencing".into());
    }
    let selection = selections.get(owner).ok_or("finalize_owner_unavailable")?;
    let selected = OutputSelection {
        producer: owner.clone(),
        service: policy.owner_service.clone(),
        capability: policy.output_capability.clone(),
        schema: policy.output_schema.clone(),
    };
    let mut inputs = journal
        .composition
        .inputs(&work.target, &[selected])
        .map_err(str::to_owned)?;
    let input = inputs.pop().ok_or("finalize_input_missing")?;
    let admission = journal
        .authority
        .settled
        .get(&input.admission_id)
        .ok_or("finalize_admission_missing")?;
    let original_phase = match &input.output.origin {
        LifecycleTarget::Session { .. } => LifecyclePhase::SessionPrepare,
        LifecycleTarget::Stream { .. } => LifecyclePhase::StreamPrepare,
        LifecycleTarget::Validation { .. } => return Err("invalid_finalize_origin".into()),
    };
    if admission.provider != *owner
        || admission.plan != input.plan
        || session.admission_targets.get(&input.admission_id) != Some(&input.output.origin)
        || session.admission_phases.get(&input.admission_id) != Some(&original_phase)
        || journal.admission_transitions.get(&input.admission_id) != Some(&input.transition_id)
    {
        return Err("finalize_provenance_mismatch".into());
    }
    let stage = if matches!(input.output.origin, LifecycleTarget::Session { .. }) {
        Some(
            journal
                .composition
                .staged_for(&work.target, owner, &input.output.output_id)
                .map_err(str::to_owned)?
                .clone(),
        )
    } else {
        None
    };
    let snapshot = FinalizeSelectionSnapshot {
        target: work.target.clone(),
        parent_generation: Some(session.generation),
        transition_id: work.transition_id.clone(),
        input,
        stage,
        policy: policy.clone(),
        finalize_approval: approval_for(selection, &policy.method)?,
        finalize_status_approval: approval_for(selection, &policy.status_method)?,
        schema_documents: settings.schema_documents.clone(),
    };
    if !snapshot.validate_structure() {
        return Err("invalid_finalize_selection".into());
    }
    let output = if graph.order.len() > 1 {
        super::composition_finalize::resolve_control_chain(journal, work, &snapshot)?.0
    } else {
        snapshot.current_output().clone()
    };
    if require_current {
        let observed = journal
            .authority
            .snapshot
            .resources
            .get(owner)
            .and_then(|resources| resources.get(&output.resource.resource_id))
            .ok_or("finalize_observation_missing")?;
        let domains: BTreeSet<_> = output
            .resource
            .mutation_domains
            .iter()
            .cloned()
            .chain(output.resource.mutation_domain.iter().cloned())
            .collect();
        if observed.provider != output.resource.provider
            || observed.incarnation != output.resource.incarnation
            || observed.revision != output.resource.revision
            || observed.execution_domain != output.resource.execution_domain
            || observed.mutation_domains != domains
        {
            return Err("finalize_observation_stale".into());
        }
        for observation in &output.domain_observations {
            let current = journal
                .authority
                .snapshot
                .domains
                .get(&observation.domain)
                .ok_or("finalize_domain_missing")?;
            if !observation.complete
                || journal
                    .authority
                    .refresh_required
                    .contains(&observation.domain)
                || current.revision != observation.revision
                || current.borrowers != observation.borrowers
            {
                return Err("finalize_domain_stale".into());
            }
        }
    }
    Ok(snapshot)
}

struct FinalizePlanWriter {
    shared: Shared,
    key: String,
}
impl crate::prepare_driver::PrepareJournal for FinalizePlanWriter {
    fn persist(&mut self, state: &crate::prepare_driver::PrepareState) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get_mut(&self.key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        let finalize = work
            .composition_progress
            .as_mut()
            .and_then(|progress| progress.finalize.as_mut())
            .ok_or("finalize_progress_missing")?;
        if state.provider != finalize.selection.policy.owner_provider
            || state.request.target != work.target
            || state.request.transition_id != work.transition_id
            || state.request.phase != work.phase
            || state.request.operation
                != amaterasu_extension_api::ProviderOperation::ResourceFinalizePlan
        {
            return Err("finalize_plan_identity_mismatch".into());
        }
        finalize.owner_plan = state.clone();
        let target = work.target.clone();
        let phase = work.phase.clone();
        let transition_id = work.transition_id.clone();
        let (_, session_id, _, _) = super::operations::target_parts(&target)?;
        if let Some(admission_id) = &state.admission_id {
            super::providers::record_admission(
                &mut next,
                session_id,
                admission_id,
                &target,
                &phase,
                &transition_id,
            )?;
        }
        save(&self.shared, next)
    }
}

impl Coordinator {
    /// Persist a separately selected owner finalization plan request before any
    /// broker call. This remains unreachable until the complete v2 path is wired.
    pub(super) fn start_standalone_finalize(&mut self, key: &str, work: &Work) -> ModuleResult<()> {
        use crate::prepare_driver::PrepareAuthority;
        use amaterasu_extension_api::{ProviderOperation, ProviderRequest};
        if work
            .composition_progress
            .as_ref()
            .is_some_and(|progress| progress.finalize.is_some())
        {
            return Err("finalization_already_started".into());
        }
        let snapshot = standalone_selection(
            &self.shared.borrow().value,
            &self.settings,
            &self.selections,
            work,
            false,
        )?;
        let (output, controls) = if work.providers.len() > 1 {
            super::composition_finalize::resolve_control_chain(
                &self.shared.borrow().value,
                work,
                &snapshot,
            )?
        } else {
            (snapshot.current_output().clone(), vec![])
        };
        let settings = self.effective_settings(work)?;
        let participant = settings
            .participants
            .iter()
            .find(|participant| participant.provider == snapshot.policy.owner_provider)
            .ok_or("finalize_owner_unavailable")?
            .clone();
        let mut authority = self.authority_for_work(work)?;
        let (sequence, operation_id) = authority.allocate(&participant.provider)?;
        let request = ProviderRequest {
            provider_sequence: sequence,
            operation: ProviderOperation::ResourceFinalizePlan,
            version: amaterasu_extension_api::COMPOSITION_VERSION,
            transition_id: work.transition_id.clone(),
            operation_id,
            target: work.target.clone(),
            resource_generation: {
                let shared = self.shared.borrow();
                let (_, session_id, _, _) = operations::target_parts(&work.target)?;
                Some(
                    *shared
                        .value
                        .sessions
                        .get(session_id)
                        .and_then(|session| session.resource_generations.get(&participant.provider))
                        .filter(|generation| **generation > 0)
                        .ok_or("resource_generation_unavailable")?,
                )
            },
            phase: work.phase.clone(),
            settings_revision: participant.settings_revision.clone(),
            settings: serde_json::from_value(participant.settings.clone())
                .map_err(|_| "invalid_participant_settings")?,
            intent: work.intent.clone(),
            reason: work.reason.clone(),
            requested_video: work.video.clone(),
            source_video_mode: work
                .processing_decision
                .as_ref()
                .and_then(|decision| decision.source_video_mode.clone()),
            consumer_requirements: self.settings.consumer_requirements.clone(),
            remaining_ms: 30_000,
            plan_revision: None,
            admission_id: None,
            approved_plan: None,
            dependency_inputs: vec![output],
            control_results: controls,
        };
        amaterasu_extension_api::validate_provider_request(&request)?;
        let owner_plan = crate::prepare_driver::PrepareState {
            provider: participant.provider.clone(),
            participant,
            request,
            phase: crate::prepare_driver::PreparePhase::Plan,
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
        let mut next = self.shared.borrow().value.clone();
        next.composition
            .record_finalize_selection(snapshot.clone(), &|candidate| candidate == &snapshot)
            .map_err(str::to_owned)?;
        let current = next
            .receipts
            .get_mut(key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        if current.transition_id != work.transition_id
            || current.target != work.target
            || current
                .composition_progress
                .as_ref()
                .is_some_and(|progress| progress.finalize.is_some())
        {
            return Err("finalization_changed".into());
        }
        let progress = current.composition_progress.get_or_insert_with(|| {
            super::composition_progress::CompositionProgress {
                inputs: vec![snapshot.input.clone()],
                selections: vec![],
                controls: vec![],
                finalize: None,
            }
        });
        progress.finalize = Some(super::composition_progress::FinalizeProgress {
            selection: snapshot,
            owner_plan,
            dispatch: None,
        });
        save(&self.shared, next)
    }
}

impl Coordinator {
    /// Drive the read-only finalize-plan broker call and persist its fresh
    /// admission. A true result only means that planning settled; the caller
    /// must still authorize and execute finalization before publishing capture.
    pub(super) fn advance_standalone_finalize_plan(
        &mut self,
        key: &str,
        work: &Work,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<bool> {
        use crate::prepare_driver::{PrepareDriver, PreparePhase};
        let Some(finalize) = work
            .composition_progress
            .as_ref()
            .and_then(|progress| progress.finalize.as_ref())
        else {
            self.start_standalone_finalize(key, work)?;
            return Ok(false);
        };
        let current = standalone_selection(
            &self.shared.borrow().value,
            &self.settings,
            &self.selections,
            work,
            false,
        )?;
        let (output, controls) = if work.providers.len() > 1 {
            super::composition_finalize::resolve_control_chain(
                &self.shared.borrow().value,
                work,
                &current,
            )?
        } else {
            (current.current_output().clone(), vec![])
        };
        if current != finalize.selection
            || finalize.owner_plan.request.dependency_inputs != [output]
            || finalize.owner_plan.request.control_results != controls
        {
            return Err("finalize_selection_changed".into());
        }
        let state = &finalize.owner_plan;
        if state.phase == PreparePhase::FinalizePlanAdmitted {
            return Ok(true);
        }
        if matches!(
            state.phase,
            PreparePhase::Rejected | PreparePhase::RecoveryRequired
        ) {
            return Err("finalize_plan_requires_recovery".into());
        }
        let settings = self.effective_settings(work)?;
        let runtime_key = format!("{key}/finalize-plan");
        if !self.drivers.contains_key(&runtime_key) {
            self.drivers.insert(
                runtime_key.clone(),
                PrepareDriver::new_finalize_plan(state.clone(), &settings)?,
            );
        }
        let mut authority = self.authority_for_work(work)?;
        let mut writer = FinalizePlanWriter {
            shared: self.shared.clone(),
            key: key.into(),
        };
        let result = self
            .drivers
            .get_mut(&runtime_key)
            .expect("finalize driver exists")
            .advance(&settings, &mut authority, &mut writer, send);
        if result.is_err() {
            self.drivers.remove(&runtime_key);
        }
        result?;
        if self.drivers[&runtime_key].state().phase == PreparePhase::FinalizePlanAdmitted {
            self.drivers.remove(&runtime_key);
            return Ok(true);
        }
        Ok(false)
    }
}

struct StagedFinalizeAuthority;
impl crate::prepare_authority::AuthorityJournal for StagedFinalizeAuthority {
    fn persist(&mut self, _: &crate::prepare_authority::AuthorityState) -> ModuleResult<()> {
        Ok(())
    }
}

impl Coordinator {
    /// Reserve a fresh owner finalization permit and freeze the exact approved
    /// dispatch. No owner effect is sent by this method.
    pub(super) fn reserve_standalone_finalize(
        &mut self,
        key: &str,
        work: &Work,
    ) -> ModuleResult<()> {
        use crate::prepare_driver::{PrepareAuthority, PreparePhase};
        use amaterasu_extension_api::ResourceFinalizeRequest;
        super::checkpoint_queries::ensure_effect_scope_open(
            &self.shared.borrow().value,
            &work.target,
        )?;
        let finalize = work
            .composition_progress
            .as_ref()
            .and_then(|progress| progress.finalize.as_ref())
            .ok_or("finalize_progress_missing")?;
        if finalize.dispatch.is_some()
            || finalize.owner_plan.phase != PreparePhase::FinalizePlanAdmitted
        {
            return Err("finalize_plan_not_ready".into());
        }
        let selection = standalone_selection(
            &self.shared.borrow().value,
            &self.settings,
            &self.selections,
            work,
            true,
        )?;
        if selection != finalize.selection {
            return Err("finalize_selection_changed".into());
        }
        let (output, controls) = if work.providers.len() > 1 {
            super::composition_finalize::resolve_control_chain(
                &self.shared.borrow().value,
                work,
                &selection,
            )?
        } else {
            (selection.current_output().clone(), vec![])
        };
        if finalize.owner_plan.request.dependency_inputs != [output.clone()]
            || finalize.owner_plan.request.control_results != controls
        {
            return Err("finalize_inputs_changed".into());
        }
        let plan = finalize
            .owner_plan
            .plan
            .as_ref()
            .ok_or("finalize_plan_missing")?;
        let constraint = plan
            .candidate_constraint
            .as_ref()
            .ok_or("finalize_capture_constraint_missing")?;
        if constraint
            .geometry_revision
            .is_none_or(|revision| revision == 0)
            || constraint.mode.as_ref()
                != work
                    .processing_decision
                    .as_ref()
                    .and_then(|decision| decision.source_video_mode.as_ref())
                    .or(work.video.as_ref())
        {
            return Err("finalize_geometry_or_mode_unpinned".into());
        }
        let participant = finalize.owner_plan.participant.clone();
        let admission_id = finalize
            .owner_plan
            .admission_id
            .clone()
            .ok_or("finalize_admission_missing")?;
        let context = crate::prepare_authority::FinalizeContext {
            policy: selection.policy.clone(),
            output: output.clone(),
            control_results: controls.clone(),
            target: work.target.clone(),
            transition_id: work.transition_id.clone(),
            parent_generation: selection.parent_generation,
        };
        let mut allocator = self.authority_for_work(work)?;
        let (sequence, operation_id) = allocator.allocate(&participant.provider)?;
        let authority_before = self.shared.borrow().value.authority.clone();
        let mut authority = crate::prepare_authority::CoordinatorPrepareAuthority::new(
            authority_before.clone(),
            self.work_selections(work)?,
            StagedFinalizeAuthority,
        )?;
        let mut request = ResourceFinalizeRequest {
            version: amaterasu_extension_api::COMPOSITION_VERSION,
            transition_id: work.transition_id.clone(),
            operation_id: operation_id.clone(),
            provider_sequence: sequence,
            target: work.target.clone(),
            output,
            control_results: controls,
            admission_id,
            permit_id: String::new(),
            remaining_ms: 30_000,
        };
        authority.issue_finalize_permit(&participant, &context, &mut request)?;
        let method =
            |reference: &amaterasu_extension_api::MethodRef| crate::adapter::MethodSettings {
                service: reference.service.clone(),
                method: reference.method.clone(),
                interface_major: reference.interface_major,
                interface_minor: reference.interface_minor,
                schema_revision: reference.schema_revision,
            };
        let effect_call = authority.approved_call(
            &participant,
            &method(&selection.policy.method),
            serde_json::to_value(&request).map_err(|_| "invalid_finalize_request")?,
            operation_id.clone(),
        )?;
        let status_selection = authority.approved_call(
            &participant,
            &method(&selection.policy.status_method),
            json!({}),
            operation_id,
        )?;
        let dispatch = super::finalize_dispatch::FinalizeDispatchState::new(
            request,
            context,
            effect_call,
            status_selection,
        )?;
        let mut next = self.shared.borrow().value.clone();
        let current = next
            .receipts
            .get_mut(key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        let current_finalize = current
            .composition_progress
            .as_mut()
            .and_then(|progress| progress.finalize.as_mut())
            .ok_or("finalize_progress_missing")?;
        if current.transition_id != work.transition_id
            || current.target != work.target
            || current_finalize.selection != selection
            || current_finalize.owner_plan.admission_id != finalize.owner_plan.admission_id
            || current_finalize.dispatch.is_some()
        {
            return Err("finalization_changed".into());
        }
        current_finalize.dispatch = Some(dispatch);
        if serde_json::to_value(&next.authority).map_err(|_| "invalid_authority_state")?
            != serde_json::to_value(&authority_before).map_err(|_| "invalid_authority_state")?
        {
            return Err("finalize_authority_changed".into());
        }
        next.authority = authority.state().clone();
        validate_journal(&next).map_err(|_| "invalid_finalize_journal")?;
        save(&self.shared, next)
    }
}

struct FinalizeWriter {
    shared: Shared,
    key: String,
}
impl super::finalize_dispatch::FinalizeJournal for FinalizeWriter {
    fn persist(
        &mut self,
        state: &super::finalize_dispatch::FinalizeDispatchState,
    ) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get_mut(&self.key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        let finalize = work
            .composition_progress
            .as_mut()
            .and_then(|progress| progress.finalize.as_mut())
            .ok_or("finalize_progress_missing")?;
        let original = finalize
            .dispatch
            .as_ref()
            .ok_or("finalize_dispatch_missing")?;
        if state.request != original.request
            || state.context != original.context
            || state.effect_call != original.effect_call
            || state.status_selection != original.status_selection
        {
            return Err("finalize_dispatch_identity_changed".into());
        }
        finalize.dispatch = Some(state.clone());
        save(&self.shared, next)
    }
}

impl Coordinator {
    /// Advance the owner effect only under its separately admitted rights. A
    /// confirmed result is still not a published capture binding; recording
    /// owner evidence and settling the admission must happen atomically first.
    pub(super) fn advance_standalone_finalize_effect(
        &mut self,
        key: &str,
        work: &Work,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<bool> {
        use super::finalize_dispatch::{FinalizeDispatch, FinalizePhase};
        use crate::prepare_driver::PrepareAuthority;
        let finalize = work
            .composition_progress
            .as_ref()
            .and_then(|progress| progress.finalize.as_ref())
            .ok_or("finalize_progress_missing")?;
        let Some(state) = &finalize.dispatch else {
            self.reserve_standalone_finalize(key, work)?;
            return Ok(false);
        };
        if matches!(state.phase, FinalizePhase::Effect)
            && state
                .dispatch
                .as_ref()
                .is_none_or(|record| record.state == crate::provider_dispatch::DispatchState::Ready)
        {
            super::checkpoint_queries::ensure_effect_scope_open(
                &self.shared.borrow().value,
                &state.request.target,
            )?;
        }
        match &state.phase {
            FinalizePhase::Confirmed { .. } => return Ok(true),
            FinalizePhase::Rejected { .. } | FinalizePhase::RecoveryRequired { .. } => {
                return Err("finalize_requires_recovery".into());
            }
            _ => {}
        }
        let selection = standalone_selection(
            &self.shared.borrow().value,
            &self.settings,
            &self.selections,
            work,
            true,
        )?;
        if selection != finalize.selection {
            return Err("finalize_selection_changed".into());
        }
        let participant = finalize.owner_plan.participant.clone();
        let plan = finalize
            .owner_plan
            .plan
            .clone()
            .ok_or("finalize_plan_missing")?;
        let admission = finalize
            .owner_plan
            .admission_id
            .clone()
            .ok_or("finalize_admission_missing")?;
        let constraint = plan
            .candidate_constraint
            .as_ref()
            .ok_or("finalize_capture_constraint_missing")?;
        let geometry = constraint
            .geometry_revision
            .filter(|revision| *revision > 0)
            .ok_or("finalize_geometry_unpinned")?;
        let (_, _, _, generation) = super::operations::target_parts(&work.target)?;
        let requested_mode = work
            .processing_decision
            .as_ref()
            .and_then(|decision| decision.source_video_mode.clone())
            .or(work.video.clone())
            .ok_or("finalize_mode_missing")?;
        let catalog = amaterasu_extension_api::schemas::SchemaCatalog::compile(
            selection
                .schema_documents
                .iter()
                .map(|item| (item.reference.clone(), item.document.as_str())),
        )?;
        let mut writer = FinalizeWriter {
            shared: self.shared.clone(),
            key: key.into(),
        };
        let authority = std::cell::RefCell::new(self.authority_for_work(work)?);
        let status_method = crate::adapter::MethodSettings {
            service: selection.policy.status_method.service.clone(),
            method: selection.policy.status_method.method.clone(),
            interface_major: selection.policy.status_method.interface_major,
            interface_minor: selection.policy.status_method.interface_minor,
            schema_revision: selection.policy.status_method.schema_revision,
        };
        let mut status_call = |request: &amaterasu_extension_api::ResourceFinalizeRequest,
                               receipt: &str| {
            let mut authority = authority.borrow_mut();
            let transport_id = authority.allocate_poll_id(&participant.provider)?;
            let status = amaterasu_extension_api::ProviderStatusRequest {
                provider_sequence: request.provider_sequence,
                operation_id: request.operation_id.clone(),
                transition_id: request.transition_id.clone(),
                receipt_id: receipt.into(),
            };
            authority.approved_call(
                &participant,
                &status_method,
                serde_json::to_value(status).map_err(|_| "invalid_finalize_status")?,
                transport_id,
            )
        };
        let mut validate_rights =
            |_: &amaterasu_extension_api::ResourceFinalizeRequest,
             _: &crate::prepare_authority::FinalizeContext| {
                authority
                    .borrow_mut()
                    .validate_rights(&participant, &plan, &admission)
            };
        let mut validate_applied = |request: &amaterasu_extension_api::ResourceFinalizeRequest,
                                    _: &crate::prepare_authority::FinalizeContext,
                                    response: &amaterasu_extension_api::ResourceControlResponse|
         -> ModuleResult<()> {
            authority
                .borrow_mut()
                .validate_rights(&participant, &plan, &admission)?;
            amaterasu_extension_api::validate_resource_finalize_response(request, response)?;
            let amaterasu_extension_api::ResourceControlResponse::Applied {
                output,
                candidate: Some(candidate),
            } = response
            else {
                return Err("finalize_candidate_missing".into());
            };
            catalog
                .validate(&output.schema, &output.data)
                .map_err(|_| "finalize_output_schema_invalid")?;
            if candidate.mode != requested_mode || candidate.geometry_revision != geometry {
                return Err("finalize_candidate_mismatch".into());
            }
            crate::plan_validation::validate_candidate(&plan, candidate, generation, geometry)
        };
        let runtime_key = format!("{key}/finalize-effect");
        if !self.finalize_drivers.contains_key(&runtime_key) {
            self.finalize_drivers
                .insert(runtime_key.clone(), FinalizeDispatch::new(state.clone())?);
        }
        let result = self
            .finalize_drivers
            .get_mut(&runtime_key)
            .expect("finalize driver exists")
            .advance(
                &mut writer,
                &mut status_call,
                &mut validate_rights,
                &mut validate_applied,
                send,
            );
        if result.is_err() {
            self.finalize_drivers.remove(&runtime_key);
        }
        result?;
        if matches!(
            self.finalize_drivers[&runtime_key].state().phase,
            FinalizePhase::Confirmed { .. }
        ) {
            self.finalize_drivers.remove(&runtime_key);
            return Ok(true);
        }
        Ok(false)
    }
}

/// Persists the authenticated result against the old active authority and then
/// publishes its candidate together with the new retained authority state.
struct FinalizeResultWriter {
    shared: Shared,
    key: String,
    work: Work,
    dispatch: super::finalize_dispatch::FinalizeDispatchState,
    settings: LifecycleSettings,
    selections: BTreeMap<String, ProviderSelection>,
}
impl crate::prepare_authority::AuthorityJournal for FinalizeResultWriter {
    fn persist(
        &mut self,
        authority: &crate::prepare_authority::AuthorityState,
    ) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let candidate = super::finalize_result::record_finalize_result(
            &mut next,
            &self.settings,
            &self.selections,
            &self.work,
            &self.dispatch,
        )?;
        let saved = next
            .receipts
            .get_mut(&self.key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        if saved.candidate.is_some() {
            return Err("finalize_candidate_already_published".into());
        }
        saved.candidate = Some(candidate);
        next.authority = authority.clone();
        validate_journal(&next).map_err(|_| "finalize_journal_invalid")?;
        save(&self.shared, next)
    }
}

impl Coordinator {
    pub(super) fn commit_standalone_finalize(
        &mut self,
        key: &str,
        work: &Work,
    ) -> ModuleResult<()> {
        let finalize = work
            .composition_progress
            .as_ref()
            .and_then(|progress| progress.finalize.as_ref())
            .ok_or("finalize_progress_missing")?;
        let dispatch = finalize
            .dispatch
            .as_ref()
            .ok_or("finalize_dispatch_missing")?;
        let super::finalize_dispatch::FinalizePhase::Confirmed { result } = &dispatch.phase else {
            return Err("finalize_not_confirmed".into());
        };
        let amaterasu_extension_api::ResourceControlResponse::Applied { output, .. } =
            result.as_ref()
        else {
            return Err("finalize_output_missing".into());
        };
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
        let selections = self.work_selections(work)?;
        let writer = FinalizeResultWriter {
            shared: self.shared.clone(),
            key: key.into(),
            work: work.clone(),
            dispatch: dispatch.clone(),
            settings: self.settings.clone(),
            selections: selections
                .iter()
                .map(|selection| (selection.participant.provider.clone(), selection.clone()))
                .collect(),
        };
        let mut authority = crate::prepare_authority::CoordinatorPrepareAuthority::new(
            self.shared.borrow().value.authority.clone(),
            selections,
            writer,
        )?;
        authority.settle_finalize_and_finish_with_output(
            &finalize.owner_plan.provider,
            &dispatch.request,
            resource,
        )
    }
}

/// Resolve the single retained session producer for a child stream. The caller
/// persists the returned value as part of PrepareState before sending the plan.
pub(super) fn standalone_stream_input(
    journal: &Journal,
    settings: &LifecycleSettings,
    work: &Work,
    owner: &str,
) -> ModuleResult<amaterasu_extension_api::ResourceOutput> {
    let (epoch, session_id, stream_id, generation) = operations::target_parts(&work.target)?;
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    let stream = session
        .streams
        .get(stream_id.ok_or("stream_target_required")?)
        .ok_or("unknown_stream")?;
    if journal.host_epoch != epoch
        || session.host_epoch != epoch
        || stream.generation != generation
        || stream.phase != StreamPhase::Preparing
        || work.phase != LifecyclePhase::StreamPrepare
    {
        return Err("stream_target_not_preparing".into());
    }
    let mut policies = settings
        .finalize_policies
        .iter()
        .filter(|policy| policy.owner_provider == owner);
    let policy = policies.next().ok_or("finalize_policy_missing")?;
    if policies.next().is_some() {
        return Err("finalize_policy_ambiguous".into());
    }
    let selected = OutputSelection {
        producer: owner.into(),
        service: policy.owner_service.clone(),
        capability: policy.output_capability.clone(),
        schema: policy.output_schema.clone(),
    };
    let input = journal
        .composition
        .inputs(&work.target, &[selected])
        .map_err(str::to_owned)?
        .pop()
        .ok_or("session_output_missing")?;
    let origin = LifecycleTarget::Session {
        host_epoch: epoch.into(),
        session_id: session_id.into(),
        generation: session.generation,
    };
    let admission = journal
        .authority
        .settled
        .get(&input.admission_id)
        .ok_or("session_admission_missing")?;
    if input.output.origin != origin
        || admission.provider != owner
        || admission.plan != input.plan
        || session.admission_targets.get(&input.admission_id) != Some(&origin)
        || session.admission_phases.get(&input.admission_id)
            != Some(&LifecyclePhase::SessionPrepare)
        || journal.admission_transitions.get(&input.admission_id) != Some(&input.transition_id)
        || !journal
            .authority
            .output_admissions
            .contains(&input.admission_id)
    {
        return Err("session_output_provenance_mismatch".into());
    }
    super::resource_generation::current_root_generation(journal, &input)?;
    // Resume must consume the most recent authenticated release readback;
    // falling back to the original session snapshot would resurrect stale state.
    if !journal.validate_composition() {
        return Err("composition_history_invalid".into());
    }
    let current = if let Some(previous) = journal.composition.staged_outputs().rev().find(|stage| {
        stage.after.producer == input.output.producer
            && stage.after.output_id == input.output.output_id
            && stage.after.origin == input.output.origin
    }) {
        journal
            .verified_released_output(&previous.target)
            .map(|(_, output)| output.clone())
            .ok_or("prior_stream_release_readback_missing")?
    } else if let Some(anchor) = journal.composition.stage_anchor(&input.output) {
        let (admission, released) = journal
            .verified_released_output(&anchor.target)
            .ok_or("prior_stream_release_readback_missing")?;
        if admission != anchor.release_admission || released != &anchor.released {
            return Err("prior_stream_release_anchor_mismatch".into());
        }
        released.clone()
    } else {
        input.output
    };
    // The owner refreshes its native observation during the read-only plan.
    // Admission independently checks freshness before any callback effect.
    Ok(current)
}

#[cfg(test)]
mod expired_effect_tests {
    use super::*;
    use crate::provider_dispatch::{DispatchRecord, DispatchState};

    #[test]
    fn retired_session_blocks_finalize_issue_and_recovered_ready_before_broker() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-finalize-expired-{}-{}",
            std::process::id(),
            identifier().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = root.join("journal.json");
        let coordinator = Coordinator::open(&path).unwrap();
        let mut journal = coordinator.shared.borrow().value.clone();
        let ended: LifecycleTarget = serde_json::from_value(json!({
            "kind":"session", "host_epoch":"epoch", "session_id":"session", "generation":7
        }))
        .unwrap();
        super::super::retention::fence_closed_session(&mut journal, &ended).unwrap();
        save(&coordinator.shared, journal).unwrap();
        drop(coordinator);

        let (_, mut work, owner_plan) = super::super::session_release::tests::complete_fixture();
        let mut dispatch = super::super::finalize_dispatch::tests::fixture();
        dispatch.dispatch = Some(DispatchRecord {
            call: dispatch.effect_call.clone(),
            state: DispatchState::Ready,
        });
        work.target = dispatch.request.target.clone();
        work.composition_progress = Some(super::super::composition_progress::CompositionProgress {
            inputs: vec![],
            selections: vec![],
            controls: vec![],
            finalize: Some(super::super::composition_progress::FinalizeProgress {
                selection: super::super::finalize_selection::tests::fixture(),
                owner_plan,
                dispatch: None,
            }),
        });
        let mut coordinator = Coordinator::open(&path).unwrap();
        let before = serde_json::to_value(&coordinator.shared.borrow().value.authority).unwrap();
        assert_eq!(
            coordinator
                .reserve_standalone_finalize("old", &work)
                .unwrap_err(),
            "session_expired"
        );
        work.composition_progress
            .as_mut()
            .unwrap()
            .finalize
            .as_mut()
            .unwrap()
            .dispatch = Some(dispatch);
        let recovered: Work = serde_json::from_slice(&serde_json::to_vec(&work).unwrap()).unwrap();
        let mut calls = 0;
        let error = coordinator
            .advance_standalone_finalize_effect("old", &recovered, &mut |_| {
                calls += 1;
                Err("unexpected broker call".into())
            })
            .unwrap_err();
        assert_eq!(error, "session_expired");
        assert_eq!(calls, 0);
        assert_eq!(
            serde_json::to_value(&coordinator.shared.borrow().value.authority).unwrap(),
            before
        );
        drop(coordinator);
        std::fs::remove_dir_all(root).unwrap();
    }
}
