//! Coordinator-owned dependent input selection before any v2 provider call.
//!
//! A selected output is data, not authority. Mutations still require the
//! separately admitted plan, owner permit, live retirement and settlement.
use super::composition::{CompositionSelectionSnapshot, OutputSelection};
use super::composition_progress::CompositionProgress;
use super::*;
use amaterasu_extension_api::{MethodRef, ResourceOutput};

fn ensure_control_scope_unfenced(
    book: &super::closed_stream::ClosedStreamBook,
    target: &LifecycleTarget,
) -> ModuleResult<()> {
    if book.scope_fenced(target) {
        return Err("control_scope_retired".into());
    }
    Ok(())
}

fn ensure_control_operation_unfenced(
    book: &super::control_settlement::ControlSettlementBook,
    owner: &str,
    operation_id: &str,
) -> ModuleResult<()> {
    if book.operation_fenced(owner, operation_id) {
        return Err("control_operation_retired".into());
    }
    Ok(())
}

fn approval_for(
    selected: &ProviderSelection,
    method: &MethodRef,
) -> ModuleResult<crate::prepare_authority::MethodApproval> {
    let mut found = selected.approvals.iter().filter(|entry| {
        entry.method.service == method.service
            && entry.method.method == method.method
            && entry.method.interface_major == method.interface_major
            && entry.method.interface_minor == method.interface_minor
            && entry.method.schema_revision == method.schema_revision
    });
    let approval = found.next().ok_or("control_method_not_approved")?;
    if found.next().is_some() {
        return Err("ambiguous_control_approval".into());
    }
    Ok(approval.clone())
}

impl Coordinator {
    /// Freeze policy-selected direct dependency inputs and their exact method
    /// approvals before asking the dependent to plan. No native effects occur.
    pub(super) fn freeze_dependent_inputs(
        &mut self,
        key: &str,
        work: &Work,
        dependent: &str,
    ) -> ModuleResult<Vec<ResourceOutput>> {
        if work.phase != LifecyclePhase::StreamPrepare || work.waiting_retirement {
            return Err("dependent_target_not_ready".into());
        }
        let graph = work
            .provider_graph
            .as_ref()
            .ok_or("provider_graph_missing")?;
        let dependencies = graph
            .dependencies
            .get(dependent)
            .ok_or("dependent_not_in_graph")?;
        if dependencies.is_empty() {
            return Err("dependent_has_no_inputs".into());
        }
        let journal = self.shared.borrow().value.clone();
        // The canonical journal checks connect producer callbacks and stages to
        // their admitted targets before any selection becomes an input.
        validate_journal(&journal).map_err(|_| "invalid_composition_journal")?;
        let (_, session_id, stream_id, generation) = super::operations::target_parts(&work.target)?;
        let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
        let stream = stream_id
            .and_then(|id| session.streams.get(id))
            .ok_or("unknown_stream")?;
        if journal.host_epoch != session.host_epoch
            || stream.generation != generation
            || stream.phase != StreamPhase::Preparing
            || !matches!(session.phase, SessionPhase::Ready | SessionPhase::Paused)
        {
            return Err("dependent_target_not_ready".into());
        }
        let policies: Vec<_> = self
            .settings
            .control_policies
            .iter()
            .filter(|policy| policy.dependent_provider == dependent)
            .collect();
        if policies.is_empty() || policies.len() > 32 {
            return Err("dependent_policy_unavailable".into());
        }
        let mut frozen = Vec::new();
        let mut inputs = Vec::new();
        let mut selected_outputs = Vec::new();
        for policy in policies {
            if !dependencies.contains(&policy.owner_provider) {
                return Err("control_owner_not_direct_dependency".into());
            }
            let owner = self
                .selections
                .get(&policy.owner_provider)
                .ok_or("control_owner_unavailable")?;
            let mut finalize = self.settings.finalize_policies.iter().filter(|item| {
                item.owner_provider == policy.owner_provider
                    && item.owner_service == policy.owner_service
                    && item.output_capability == policy.output_capability
                    && item.output_schema == policy.output_schema
            });
            let finalize_policy = finalize.next().ok_or("finalize_policy_missing")?;
            if finalize.next().is_some() {
                return Err("ambiguous_finalize_policy".into());
            }
            let wanted = OutputSelection {
                producer: policy.owner_provider.clone(),
                service: policy.owner_service.clone(),
                capability: policy.output_capability.clone(),
                schema: policy.output_schema.clone(),
            };
            let input = journal
                .composition
                .inputs(&work.target, &[wanted])
                .map_err(str::to_owned)?
                .pop()
                .ok_or("dependency_input_missing")?;
            if inputs.iter().any(|old: &super::composition::StoredOutput| {
                old.output.producer == input.output.producer
                    && old.output.output_id == input.output.output_id
            }) {
                return Err("ambiguous_control_policy".into());
            }
            let current = match &input.output.origin {
                LifecycleTarget::Session {
                    generation: parent, ..
                } => {
                    super::resource_generation::current_root_generation(&journal, &input)?;
                    if *parent != session.generation {
                        return Err("dependency_parent_stale".into());
                    }
                    let stage = journal
                        .composition
                        .staged_for(
                            &work.target,
                            &input.output.producer,
                            &input.output.output_id,
                        )
                        .map_err(str::to_owned)?;
                    if stage.parent_generation != session.generation {
                        return Err("dependency_stage_stale".into());
                    }
                    stage.after.clone()
                }
                _ => input.output.clone(),
            };
            let selected = CompositionSelectionSnapshot {
                target: work.target.clone(),
                transition_id: work.transition_id.clone(),
                input: input.clone(),
                control_policy: policy.clone(),
                finalize_policy: finalize_policy.clone(),
                control_approval: approval_for(owner, &policy.owner_method)?,
                control_status_approval: approval_for(owner, &policy.status_method)?,
                settlement_approval: approval_for(owner, &policy.settlement_method)?,
                finalize_approval: approval_for(owner, &finalize_policy.method)?,
                finalize_status_approval: approval_for(owner, &finalize_policy.status_method)?,
                schema_documents: self.settings.schema_documents.clone(),
            };
            if !selected.validate_structure() {
                return Err("invalid_control_selection".into());
            }
            self.settings
                .schema_catalog()?
                .validate(&current.schema, &current.data)?;
            inputs.push(input);
            selected_outputs.push(current);
            frozen.push(selected);
        }
        let mut next = journal;
        for selection in &frozen {
            next.composition
                .record_selection(selection.clone(), &|candidate| candidate == selection)
                .map_err(str::to_owned)?;
        }
        let current = next
            .receipts
            .get_mut(key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        if current.target != work.target
            || current.transition_id != work.transition_id
            || current.states.contains_key(dependent)
        {
            return Err("dependent_selection_changed".into());
        }
        let progress = current
            .composition_progress
            .get_or_insert_with(|| CompositionProgress {
                inputs: Vec::new(),
                selections: Vec::new(),
                controls: Vec::new(),
                finalize: None,
            });
        if progress.finalize.is_some() {
            return Err("finalization_already_started".into());
        }
        for selection in frozen {
            if let Some(previous) = progress.selections.iter().find(|item| {
                item.control_policy.dependent_provider
                    == selection.control_policy.dependent_provider
                    && item.control_policy.owner_provider == selection.control_policy.owner_provider
                    && item.input.output.output_id == selection.input.output.output_id
            }) {
                if previous != &selection {
                    return Err("dependent_selection_changed".into());
                }
            } else {
                progress.selections.push(selection);
            }
        }
        for input in inputs {
            if let Some(previous) = progress.inputs.iter().find(|item| {
                item.output.producer == input.output.producer
                    && item.output.output_id == input.output.output_id
                    && item.output.origin == input.output.origin
            }) {
                if previous != &input {
                    return Err("dependent_input_changed".into());
                }
            } else {
                progress.inputs.push(input);
            }
        }
        save(&self.shared, next)?;
        Ok(selected_outputs)
    }
}

impl Coordinator {
    /// Reserve the next effect from the already admitted dependent plan. The
    /// permit and complete dispatch are persisted before any owner call.
    pub(super) fn reserve_next_dependent_control(
        &mut self,
        key: &str,
        work: &Work,
        dependent: &str,
    ) -> ModuleResult<bool> {
        use crate::prepare_driver::PrepareAuthority;
        use amaterasu_extension_api::ResourceControlRequest;
        let state = work.states.get(dependent).ok_or("dependent_plan_missing")?;
        let plan = state.plan.as_ref().ok_or("dependent_plan_missing")?;
        let admission = state
            .admission_id
            .as_ref()
            .ok_or("dependent_admission_missing")?;
        if state.provider != dependent
            || state.request.target != work.target
            || state.request.transition_id != work.transition_id
            || state.request.version != amaterasu_extension_api::COMPOSITION_VERSION
        {
            return Err("dependent_plan_identity_mismatch".into());
        }
        let progress = work
            .composition_progress
            .as_ref()
            .ok_or("dependent_selection_missing")?;
        let completed = progress
            .controls
            .iter()
            .filter(|control| {
                control.dispatch.request.admission_id == *admission
                    && control.settlement.as_ref().is_some_and(|settlement| {
                        matches!(
                            settlement.phase,
                            super::settlement_dispatch::SettlementPhase::Acknowledged
                        )
                    })
                    && plan
                        .control_proposals
                        .contains(&control.dispatch.request.proposal)
            })
            .count();
        if progress.controls.iter().any(|control| {
            control.dispatch.request.admission_id == *admission
                && plan
                    .control_proposals
                    .contains(&control.dispatch.request.proposal)
                && !control.settlement.as_ref().is_some_and(|settlement| {
                    matches!(
                        settlement.phase,
                        super::settlement_dispatch::SettlementPhase::Acknowledged
                    )
                })
        }) {
            return Ok(false);
        }
        if completed == plan.control_proposals.len() {
            return Ok(true);
        }
        let proposal = plan
            .control_proposals
            .get(completed)
            .ok_or("control_order_invalid")?;
        if plan.control_proposals[..completed].iter().any(|old| {
            old.owner_provider == proposal.owner_provider && old.output_id == proposal.output_id
        }) {
            return Err("multiple_controls_require_replanning".into());
        }
        let mut selected = progress.selections.iter().filter(|entry| {
            entry.control_policy.dependent_provider == dependent
                && entry.control_policy.owner_provider == proposal.owner_provider
                && entry.input.output.output_id == proposal.output_id
        });
        let selection = selected.next().ok_or("control_selection_missing")?;
        if selected.next().is_some() || !selection.validate_structure() {
            return Err("ambiguous_control_selection".into());
        }
        let mut inputs = state.request.dependency_inputs.iter().filter(|output| {
            output.producer == proposal.owner_provider && output.output_id == proposal.output_id
        });
        let input = inputs.next().ok_or("control_input_missing")?;
        if inputs.next().is_some() || input.resource != proposal.resource {
            return Err("control_input_changed".into());
        }
        let (_, session_id, _, _) = super::operations::target_parts(&work.target)?;
        let parent_generation = self
            .shared
            .borrow()
            .value
            .sessions
            .get(session_id)
            .ok_or("unknown_session")?
            .generation;
        let context = crate::prepare_authority::ControlContext {
            policy: selection.control_policy.clone(),
            output: input.clone(),
            target: work.target.clone(),
            transition_id: work.transition_id.clone(),
            parent_generation: Some(parent_generation),
        };
        ensure_control_scope_unfenced(&self.shared.borrow().value.closed_streams, &work.target)?;
        let mut authority = self.authority_for_work(work)?;
        let selected_providers = self.work_selections(work)?;
        let owner = selected_providers
            .iter()
            .find(|entry| entry.participant.provider == proposal.owner_provider)
            .ok_or("owner_unavailable")?;
        let (sequence, operation_id) = authority.allocate(&proposal.owner_provider)?;
        ensure_control_operation_unfenced(
            &self.shared.borrow().value.control_settlements,
            &proposal.owner_provider,
            &operation_id,
        )?;
        let mut request = ResourceControlRequest {
            version: amaterasu_extension_api::COMPOSITION_VERSION,
            transition_id: work.transition_id.clone(),
            operation_id: operation_id.clone(),
            provider_sequence: sequence,
            target: work.target.clone(),
            proposal: proposal.clone(),
            admission_id: admission.clone(),
            permit_id: String::new(),
            remaining_ms: 30_000,
        };
        let catalog = self.settings.schema_catalog()?;
        // Permit issuance and its frozen work record must share one durable
        // journal write. A sequence allocation may safely precede this write:
        // an unused sequence cannot authorize a provider effect.
        let base_authority = self.shared.borrow().value.authority.clone();
        let mut staged_authority = crate::prepare_authority::CoordinatorPrepareAuthority::new(
            base_authority.clone(),
            selected_providers.clone(),
            StagedAuthorityWriter,
        )?;
        staged_authority.issue_control_permit(
            &state.participant,
            &context,
            &mut request,
            |schema, value| catalog.validate(schema, value),
        )?;
        let convert = |reference: &MethodRef| crate::adapter::MethodSettings {
            service: reference.service.clone(),
            method: reference.method.clone(),
            interface_major: reference.interface_major,
            interface_minor: reference.interface_minor,
            schema_revision: reference.schema_revision,
        };
        let effect = staged_authority.approved_call(
            &owner.participant,
            &convert(&selection.control_policy.owner_method),
            serde_json::to_value(&request).map_err(|_| "invalid_control_request")?,
            operation_id.clone(),
        )?;
        let status = staged_authority.approved_call(
            &owner.participant,
            &convert(&selection.control_policy.status_method),
            json!({}),
            operation_id,
        )?;
        let dispatch = super::control_dispatch::ControlDispatchState::new(
            request,
            input.clone(),
            effect,
            status,
        )?;
        let mut next = self.shared.borrow().value.clone();
        if serde_json::to_value(&next.authority).map_err(|_| "invalid_authority")?
            != serde_json::to_value(&base_authority).map_err(|_| "invalid_authority")?
        {
            return Err("control_authority_changed".into());
        }
        next.authority = staged_authority.state().clone();
        let current = next
            .receipts
            .get_mut(key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        if current.target != work.target
            || current.transition_id != work.transition_id
            || current
                .states
                .get(dependent)
                .and_then(|item| item.admission_id.as_ref())
                != Some(admission)
        {
            return Err("dependent_plan_changed".into());
        }
        let progress = current
            .composition_progress
            .as_mut()
            .ok_or("dependent_selection_missing")?;
        if progress.controls.iter().any(|entry| {
            entry.dispatch.request.admission_id == *admission
                && entry.dispatch.request.proposal == *proposal
        }) {
            return Err("control_already_reserved".into());
        }
        progress
            .controls
            .push(super::composition_progress::ControlProgress {
                dispatch,
                settlement: None,
            });
        save(&self.shared, next)?;
        Ok(false)
    }
}

/// Holds authorization changes in memory until the matching dispatch is saved.
struct StagedAuthorityWriter;
impl crate::prepare_authority::AuthorityJournal for StagedAuthorityWriter {
    fn persist(&mut self, _: &crate::prepare_authority::AuthorityState) -> ModuleResult<()> {
        Ok(())
    }
}

struct ControlStateWriter {
    shared: Shared,
    key: String,
    permit: String,
}
impl super::control_dispatch::ControlJournal for ControlStateWriter {
    fn persist(
        &mut self,
        state: &super::control_dispatch::ControlDispatchState,
    ) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get_mut(&self.key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        let control = work
            .composition_progress
            .as_mut()
            .ok_or("dependent_progress_missing")?
            .controls
            .iter_mut()
            .find(|entry| entry.dispatch.request.permit_id == self.permit)
            .ok_or("control_not_reserved")?;
        let previous = &control.dispatch;
        if state.request != previous.request
            || state.dependency != previous.dependency
            || state.effect_call != previous.effect_call
            || state.status_selection != previous.status_selection
        {
            return Err("control_dispatch_identity_changed".into());
        }
        control.dispatch = state.clone();
        save(&self.shared, next)
    }
}

impl Coordinator {
    /// Advance one owner effect while retaining transient broker handles in the
    /// coordinator. An unknown outcome is fenced for recovery, never replayed.
    pub(super) fn advance_dependent_control_effect(
        &mut self,
        key: &str,
        work: &Work,
        dependent: &str,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<bool> {
        use super::control_dispatch::{ControlDispatch, ControlPhase};
        use crate::prepare_driver::PrepareAuthority;
        let state = work.states.get(dependent).ok_or("dependent_plan_missing")?;
        let plan = state.plan.as_ref().ok_or("dependent_plan_missing")?;
        let progress = work
            .composition_progress
            .as_ref()
            .ok_or("dependent_progress_missing")?;
        let control = progress
            .controls
            .iter()
            .find(|entry| {
                state.admission_id.as_ref() == Some(&entry.dispatch.request.admission_id)
                    && plan
                        .control_proposals
                        .contains(&entry.dispatch.request.proposal)
                    && !entry.settlement.as_ref().is_some_and(|settlement| {
                        matches!(
                            settlement.phase,
                            super::settlement_dispatch::SettlementPhase::Acknowledged
                        )
                    })
            })
            .ok_or("control_not_reserved")?;
        let permit = control.dispatch.request.permit_id.clone();
        let runtime_key = format!("{key}/control/{permit}");
        if matches!(control.dispatch.phase, ControlPhase::Effect)
            && control
                .dispatch
                .dispatch
                .as_ref()
                .is_none_or(|record| record.state == crate::provider_dispatch::DispatchState::Ready)
        {
            ensure_control_scope_unfenced(
                &self.shared.borrow().value.closed_streams,
                &control.dispatch.request.target,
            )?;
            ensure_control_operation_unfenced(
                &self.shared.borrow().value.control_settlements,
                &control.dispatch.request.proposal.owner_provider,
                &control.dispatch.request.operation_id,
            )?;
        }
        match &control.dispatch.phase {
            ControlPhase::Applied { .. } => {
                self.control_drivers.remove(&runtime_key);
                return Ok(true);
            }
            ControlPhase::Rejected { .. } | ControlPhase::RecoveryRequired { .. } => {
                return Err("control_requires_recovery".into());
            }
            _ => {}
        }
        let context = self
            .shared
            .borrow()
            .value
            .authority
            .control_permits
            .get(&permit)
            .ok_or("control_permit_missing")?
            .context
            .clone();
        let owner = context.policy.owner_provider.clone();
        let catalog = self.settings.schema_catalog()?;
        let authority = std::cell::RefCell::new(self.authority_for_work(work)?);
        let mut driver = match self.control_drivers.remove(&runtime_key) {
            Some(driver) => driver,
            None => ControlDispatch::new(control.dispatch.clone())?,
        };
        let participant = state.participant.clone();
        let status_template = control.dispatch.status_selection.clone();
        let mut status_call = |request: &amaterasu_extension_api::ResourceControlRequest,
                               receipt: &str|
         -> ModuleResult<crate::provider_dispatch::ProviderCall> {
            let mut call = status_template.clone();
            call.operation_id = authority.borrow_mut().allocate_poll_id(&owner)?;
            call.arguments = serde_json::to_value(amaterasu_extension_api::ProviderStatusRequest {
                provider_sequence: request.provider_sequence,
                operation_id: request.operation_id.clone(),
                transition_id: request.transition_id.clone(),
                receipt_id: receipt.to_owned(),
            })
            .map_err(|_| "invalid_control_status")?;
            Ok(call)
        };
        let mut validate_rights = |request: &amaterasu_extension_api::ResourceControlRequest,
                                   input: &ResourceOutput|
         -> ModuleResult<()> {
            if input != &context.output {
                return Err("control_input_changed".into());
            }
            authority.borrow_mut().describe_control_permit(
                &owner,
                &participant,
                &context,
                request,
                |schema, value| catalog.validate(schema, value),
            )
        };
        let mut validate_applied = |request: &amaterasu_extension_api::ResourceControlRequest,
                                    input: &ResourceOutput,
                                    result: &amaterasu_extension_api::ConfirmedControl|
         -> ModuleResult<()> {
            if input != &context.output {
                return Err("control_input_changed".into());
            }
            authority.borrow_mut().validate_control_permit(
                &owner,
                &participant,
                &context,
                request,
                |schema, value| catalog.validate(schema, value),
            )?;
            let response = amaterasu_extension_api::ResourceControlResponse::Applied {
                output: Box::new(result.output.clone()),
                candidate: result.candidate.clone().map(Box::new),
            };
            amaterasu_extension_api::validate_resource_control_response(request, &response)?;
            catalog.validate(&result.output.schema, &result.output.data)
        };
        let mut writer = ControlStateWriter {
            shared: self.shared.clone(),
            key: key.into(),
            permit,
        };
        let outcome = driver.advance(
            &mut writer,
            &mut status_call,
            &mut validate_rights,
            &mut validate_applied,
            send,
        );
        // Applied is durably persisted by the driver before advance succeeds.
        // Settlement reconstructs from that state and does not need the
        // in-memory effect driver, which must not pin a completed transition.
        if outcome.is_ok() && !matches!(driver.state().phase, ControlPhase::Applied { .. }) {
            self.control_drivers.insert(runtime_key, driver);
        }
        outcome?;
        Ok(false)
    }
}

impl Coordinator {
    /// Persist the authenticated owner effect and settlement request together
    /// before sending the owner's idempotent acknowledgement operation.
    pub(super) fn prepare_dependent_settlement(
        &mut self,
        key: &str,
        work: &Work,
        permit_id: &str,
    ) -> ModuleResult<()> {
        use crate::prepare_driver::PrepareAuthority;
        let progress = work
            .composition_progress
            .as_ref()
            .ok_or("dependent_progress_missing")?;
        let control = progress
            .controls
            .iter()
            .find(|entry| entry.dispatch.request.permit_id == permit_id)
            .ok_or("control_not_reserved")?;
        if control.settlement.is_some() {
            return Ok(());
        }
        let result = match &control.dispatch.phase {
            super::control_dispatch::ControlPhase::Applied { result } => result.as_ref().clone(),
            _ => return Err("control_not_applied".into()),
        };
        let request = &control.dispatch.request;
        let mut matches = progress.selections.iter().filter(|entry| {
            entry.control_policy.owner_provider == request.proposal.owner_provider
                && entry.control_policy.dependent_provider
                    == work
                        .states
                        .values()
                        .find(|state| state.admission_id.as_deref() == Some(&request.admission_id))
                        .map_or("", |state| &state.provider)
                && entry.input.output.output_id == request.proposal.output_id
        });
        let selection = matches.next().ok_or("control_selection_missing")?;
        if matches.next().is_some() {
            return Err("ambiguous_control_selection".into());
        }
        let mut authority = self.authority_for_work(work)?;
        let owners = self.work_selections(work)?;
        let owner = owners
            .iter()
            .find(|entry| entry.participant.provider == request.proposal.owner_provider)
            .ok_or("control_owner_unavailable")?;
        let tuple = amaterasu_extension_api::ResourceControlSettlementRequest {
            request: request.clone(),
            output: result.output.clone(),
            settlement_id: format!("settlement.{}", request.permit_id),
        };
        amaterasu_extension_api::validate_resource_control_settlement_request(&tuple)?;
        let method = &selection.control_policy.settlement_method;
        let settings = crate::adapter::MethodSettings {
            service: method.service.clone(),
            method: method.method.clone(),
            interface_major: method.interface_major,
            interface_minor: method.interface_minor,
            schema_revision: method.schema_revision,
        };
        let call = authority.approved_call(
            &owner.participant,
            &settings,
            serde_json::to_value(&tuple).map_err(|_| "invalid_settlement")?,
            format!("{}.settle", request.operation_id),
        )?;
        if call.approval != selection.settlement_approval.broker_approval {
            return Err("settlement_approval_changed".into());
        }
        let settlement = super::settlement_dispatch::SettlementState::new(
            tuple.clone(),
            selection.control_policy.clone(),
            call,
        )?;
        let effect = control
            .dispatch
            .dispatch
            .clone()
            .ok_or("control_effect_proof_missing")?;
        let applied = amaterasu_extension_api::ResourceControlResponse::Applied {
            output: Box::new(result.output.clone()),
            candidate: result.candidate.clone().map(Box::new),
        };
        let owner_receipt_id =
            control
                .dispatch
                .pending_proof
                .as_ref()
                .and_then(|proof| match &proof.state {
                    crate::provider_dispatch::DispatchState::Completed { payload } => payload
                        .get("receipt_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    _ => None,
                });
        let record = super::control_settlement::ControlSettlementRecord {
            settlement: tuple,
            selection_transition_id: selection.transition_id.clone(),
            confirmed: result,
            applied,
            effect_dispatch: effect,
            effect_pending_proof: control.dispatch.pending_proof.clone(),
            owner_receipt_id,
            owner_ack: None,
            owner_dispatch: None,
        };
        let mut next = self.shared.borrow().value.clone();
        let permit = next
            .authority
            .control_permits
            .get(permit_id)
            .ok_or("control_permit_missing")?
            .clone();
        let admission = next
            .authority
            .admissions
            .get(&request.admission_id)
            .ok_or("dependent_admission_missing")?
            .clone();
        let catalog = self.settings.schema_catalog()?;
        next.control_settlements
            .record_provisional(record, &permit, &admission, &|item| {
                catalog
                    .validate(&item.confirmed.output.schema, &item.confirmed.output.data)
                    .is_ok()
            })
            .map_err(str::to_owned)?;
        let current = next
            .receipts
            .get_mut(key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        let entry = current
            .composition_progress
            .as_mut()
            .ok_or("dependent_progress_missing")?
            .controls
            .iter_mut()
            .find(|entry| entry.dispatch.request == *request)
            .ok_or("control_not_reserved")?;
        if entry.settlement.is_some() {
            return Err("settlement_already_recorded".into());
        }
        entry.settlement = Some(settlement);
        save(&self.shared, next)
    }
}

struct SettlementStateWriter {
    shared: Shared,
    key: String,
    permit: String,
}
impl super::settlement_dispatch::SettlementJournal for SettlementStateWriter {
    fn persist(&mut self, state: &super::settlement_dispatch::SettlementState) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get_mut(&self.key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        let entry = work
            .composition_progress
            .as_mut()
            .ok_or("dependent_progress_missing")?
            .controls
            .iter_mut()
            .find(|entry| entry.dispatch.request.permit_id == self.permit)
            .ok_or("control_not_reserved")?;
        let original = entry.settlement.as_ref().ok_or("settlement_missing")?;
        if state.request != original.request
            || state.policy != original.policy
            || state.call != original.call
        {
            return Err("settlement_identity_changed".into());
        }
        entry.settlement = Some(state.clone());
        if matches!(
            state.phase,
            super::settlement_dispatch::SettlementPhase::Acknowledged
        ) {
            let ack = state.owner_ack.clone().ok_or("settlement_ack_missing")?;
            let dispatch = state
                .dispatch
                .clone()
                .ok_or("settlement_dispatch_missing")?;
            next.control_settlements
                .acknowledge(
                    &state.request.settlement_id,
                    ack,
                    dispatch,
                    &|_, _, completed| completed.call == state.call,
                )
                .map_err(str::to_owned)?;
            let permit = next
                .authority
                .control_permits
                .get_mut(&self.permit)
                .ok_or("control_permit_missing")?;
            if !permit.consumed || permit.request != state.request.request {
                return Err("control_permit_not_consumed".into());
            }
            permit.settled = true;
        }
        save(&self.shared, next)
    }
}

impl Coordinator {
    /// Drive the owner's separate settlement CAS. The acknowledged tuple,
    /// consumed permit and work progress are committed in one journal write.
    pub(super) fn advance_dependent_settlement(
        &mut self,
        key: &str,
        work: &Work,
        permit_id: &str,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<bool> {
        use super::settlement_dispatch::{SettlementDispatch, SettlementPhase};
        let entry = work
            .composition_progress
            .as_ref()
            .ok_or("dependent_progress_missing")?
            .controls
            .iter()
            .find(|entry| entry.dispatch.request.permit_id == permit_id)
            .ok_or("control_not_reserved")?;
        let Some(state) = &entry.settlement else {
            self.prepare_dependent_settlement(key, work, permit_id)?;
            return Ok(false);
        };
        if matches!(state.phase, SettlementPhase::RecoveryRequired) {
            return Err("settlement_requires_recovery".into());
        }
        if matches!(state.phase, SettlementPhase::Acknowledged) {
            self.settlement_drivers
                .remove(&format!("{key}/settlement/{permit_id}"));
            let shared = self.shared.borrow();
            let record = shared
                .value
                .control_settlements
                .get(&state.request.settlement_id)
                .ok_or("settlement_record_missing")?;
            let permit = shared
                .value
                .authority
                .control_permits
                .get(permit_id)
                .ok_or("control_permit_missing")?;
            if !permit.consumed
                || !permit.settled
                || permit.request != state.request.request
                || record.owner_ack != state.owner_ack
                || record.settlement != state.request
            {
                return Err("settlement_not_durable".into());
            }
            return Ok(true);
        }
        if state
            .dispatch
            .as_ref()
            .is_none_or(|record| record.state == crate::provider_dispatch::DispatchState::Ready)
        {
            let mut envelope =
                serde_json::to_value(&state.request).map_err(|_| "invalid_settlement")?;
            envelope["operation"] = json!("verify_control_settlement");
            let verified =
                self.preflight_control_settlement(&state.policy.owner_provider, envelope);
            if verified["status"] != "valid" {
                return Err("settlement_not_authorized".into());
            }
        }
        let runtime_key = format!("{key}/settlement/{permit_id}");
        let mut driver = match self.settlement_drivers.remove(&runtime_key) {
            Some(driver) => driver,
            None => SettlementDispatch::new(state.clone())?,
        };
        let expected = state.request.clone();
        let catalog = self.settings.schema_catalog()?;
        let mut validate_tuple = |tuple: &amaterasu_extension_api::ResourceControlSettlementRequest| -> ModuleResult<()> {
            if tuple != &expected { return Err("settlement_changed".into()); }
            amaterasu_extension_api::validate_resource_control_settlement_request(tuple)
        };
        let mut validate_ack =
            |tuple: &amaterasu_extension_api::ResourceControlSettlementRequest,
             ack: &amaterasu_extension_api::ResourceControlResponse|
             -> ModuleResult<()> {
                if tuple != &expected {
                    return Err("settlement_changed".into());
                }
                amaterasu_extension_api::validate_resource_control_settlement_response(tuple, ack)?;
                catalog.validate(&tuple.output.schema, &tuple.output.data)
            };
        let mut writer = SettlementStateWriter {
            shared: self.shared.clone(),
            key: key.into(),
            permit: permit_id.into(),
        };
        let outcome = driver.advance(&mut writer, &mut validate_tuple, &mut validate_ack, send);
        if outcome.is_ok() {
            self.settlement_drivers.insert(runtime_key, driver);
        }
        outcome?;
        Ok(false)
    }
}

impl Coordinator {
    /// Advance one serialized control or settlement per tick. Results become
    /// eligible for the dependent callback only after all acknowledgements and
    /// permit settlement are durable.
    pub(super) fn advance_dependent_controls(
        &mut self,
        key: &str,
        work: &Work,
        dependent: &str,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<Option<Vec<amaterasu_extension_api::ConfirmedControl>>> {
        use super::control_dispatch::ControlPhase;
        let state = work.states.get(dependent).ok_or("dependent_plan_missing")?;
        let plan = state.plan.as_ref().ok_or("dependent_plan_missing")?;
        let admission = state
            .admission_id
            .as_ref()
            .ok_or("dependent_admission_missing")?;
        let progress = work
            .composition_progress
            .as_ref()
            .ok_or("dependent_progress_missing")?;
        let mut results = Vec::new();
        for proposal in &plan.control_proposals {
            let mut matching = progress.controls.iter().filter(|entry| {
                entry.dispatch.request.admission_id == *admission
                    && entry.dispatch.request.proposal == *proposal
            });
            let Some(control) = matching.next() else {
                self.reserve_next_dependent_control(key, work, dependent)?;
                return Ok(None);
            };
            if matching.next().is_some() {
                return Err("ambiguous_control_progress".into());
            }
            let result = match &control.dispatch.phase {
                ControlPhase::Applied { result } => result.as_ref().clone(),
                ControlPhase::Rejected { .. } | ControlPhase::RecoveryRequired { .. } => {
                    return Err("control_requires_recovery".into());
                }
                _ => {
                    self.advance_dependent_control_effect(key, work, dependent, send)?;
                    return Ok(None);
                }
            };
            if !self.advance_dependent_settlement(
                key,
                work,
                &control.dispatch.request.permit_id,
                send,
            )? {
                return Ok(None);
            }
            results.push(result);
        }
        Ok(Some(results))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovered_ready_control_never_dispatches_after_scope_retirement() {
        use crate::provider_dispatch::{DispatchRecord, DispatchState};
        use sha2::{Digest, Sha256};
        let root = std::env::temp_dir().join(format!(
            "amaterasu-ready-retired-{}-{}",
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
        let mut dispatch = super::super::control_dispatch::tests::fixture();
        let target = dispatch.request.target.clone();
        dispatch.dispatch = Some(DispatchRecord {
            call: dispatch.effect_call.clone(),
            state: DispatchState::Ready,
        });
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&json!(["closed-stream-scope-v1", target])).unwrap())
        );
        let mut journal = coordinator.shared.borrow().value.clone();
        journal.closed_streams = serde_json::from_value(json!({
            "records":[], "expired_scopes":[digest]
        }))
        .unwrap();
        save(&coordinator.shared, journal).unwrap();
        drop(coordinator);

        let (_, mut work, mut state) = super::super::session_release::tests::complete_fixture();
        let mut plan = state.plan.clone().unwrap();
        plan.control_proposals = vec![dispatch.request.proposal.clone()];
        state.plan = Some(plan);
        state.admission_id = Some(dispatch.request.admission_id.clone());
        work.target = target;
        work.states.insert("dependent".into(), state);
        work.composition_progress = Some(super::super::composition_progress::CompositionProgress {
            inputs: vec![],
            selections: vec![],
            controls: vec![super::super::composition_progress::ControlProgress {
                dispatch,
                settlement: None,
            }],
            finalize: None,
        });
        let recovered: Work = serde_json::from_slice(&serde_json::to_vec(&work).unwrap()).unwrap();
        let mut coordinator = Coordinator::open(&path).unwrap();
        let mut calls = 0;
        let result = coordinator.advance_dependent_control_effect(
            "old-receipt",
            &recovered,
            "dependent",
            &mut |_| {
                calls += 1;
                Err("unexpected broker call".into())
            },
        );
        assert_eq!(result.unwrap_err(), "control_scope_retired");
        assert_eq!(calls, 0);
        drop(coordinator);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn closed_stream_scope_cannot_issue_or_replay_control() {
        use sha2::{Digest, Sha256};
        let target: LifecycleTarget = serde_json::from_value(json!({
            "kind":"stream", "host_epoch":"epoch", "session_id":"session",
            "stream_id":"stream", "generation":1
        }))
        .unwrap();
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&json!(["closed-stream-scope-v1", target])).unwrap())
        );
        let book: super::super::closed_stream::ClosedStreamBook =
            serde_json::from_value(json!({"records":[],"expired_scopes":[digest]})).unwrap();
        assert!(ensure_control_scope_unfenced(&book, &target).is_err());
        let mut other = target.clone();
        if let LifecycleTarget::Stream { generation, .. } = &mut other {
            *generation += 1;
        }
        assert!(ensure_control_scope_unfenced(&book, &other).is_ok());
    }

    #[test]
    fn retired_control_operation_cannot_be_dispatched_again() {
        use sha2::{Digest, Sha256};
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&["operation", "owner", "old-op"]).unwrap())
        );
        let book: super::super::control_settlement::ControlSettlementBook =
            serde_json::from_value(json!({"records":[],"replay_fences":[digest]})).unwrap();
        assert!(ensure_control_operation_unfenced(&book, "owner", "old-op").is_err());
        assert!(ensure_control_operation_unfenced(&book, "owner", "new-op").is_ok());
        assert!(ensure_control_operation_unfenced(&book, "other", "old-op").is_ok());
    }

    #[test]
    fn selection_never_starts_for_unretired_or_wrong_phase_work() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-dependent-select-{}-{}",
            std::process::id(),
            identifier().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&root.join("journal.json")).unwrap();
        let mut work: Work = serde_json::from_value(json!({"phase":"session.prepare","target":{"kind":"session","host_epoch":"epoch","session_id":"session","generation":1},"intent":null,"reason":null,"video":null,"transition_id":"transition","providers":[],"index":0,"states":{},"candidate":null,"waiting_retirement":false})).unwrap();
        assert!(
            coordinator
                .freeze_dependent_inputs("missing", &work, "dependent")
                .is_err()
        );
        work.phase = LifecyclePhase::StreamPrepare;
        work.waiting_retirement = true;
        assert!(
            coordinator
                .freeze_dependent_inputs("missing", &work, "dependent")
                .is_err()
        );
        assert!(
            coordinator
                .shared
                .borrow()
                .value
                .composition
                .selections()
                .next()
                .is_none()
        );
        drop(coordinator);
        std::fs::remove_dir_all(root).unwrap();
    }
}
