//! Concrete control verification backed by coordinator-owned durable state.
use super::control_endpoints::{ControlEndpointBackend, ResolvedControl, control_endpoint};
use super::*;
use crate::prepare_authority::{ControlContext, ControlPermitUse, CoordinatorPrepareAuthority};
use amaterasu_extension_api::{
    ResourceControlRequest, ResourceControlSettlementRequest, SchemaRef,
};

struct Backend<'a> {
    coordinator: &'a mut Coordinator,
}

fn target_phase_allowed(
    phase: &LifecyclePhase,
    stream: &StreamRecord,
    session: &SessionRecord,
) -> bool {
    match phase {
        LifecyclePhase::StreamPrepare => {
            stream.phase == StreamPhase::Preparing
                && matches!(session.phase, SessionPhase::Ready | SessionPhase::Paused)
        }
        LifecyclePhase::StreamRelease => {
            stream.phase == StreamPhase::Quiescing
                && stream.retirements == stream.required
                && matches!(
                    session.phase,
                    SessionPhase::Ready | SessionPhase::Paused | SessionPhase::Ending
                )
        }
        _ => false,
    }
}

impl Backend<'_> {
    fn authority(
        &self,
        request: &ResourceControlRequest,
    ) -> ModuleResult<CoordinatorPrepareAuthority<super::providers::AuthorityWriter>> {
        let work = {
            let shared = self.coordinator.shared.borrow();
            let mut matches = shared.value.receipts.values().filter_map(|receipt| {
                if receipt.response["status"] != "pending" {
                    return None;
                }
                let work = receipt.work.as_ref()?;
                let progress = work.composition_progress.as_ref()?;
                (work.target == request.target
                    && work.transition_id == request.transition_id
                    && progress
                        .controls
                        .iter()
                        .any(|control| control.dispatch.request == *request)
                    && work.states.values().any(|state| {
                        state.admission_id.as_deref() == Some(request.admission_id.as_str())
                    }))
                .then_some(work)
            });
            let work = matches.next().ok_or("active_control_work_unavailable")?;
            if matches.next().is_some() {
                return Err("ambiguous_control_work".into());
            }
            work.clone()
        };
        self.coordinator.authority_for_work(&work)
    }
}

impl Backend<'_> {
    fn resolve_control(
        &mut self,
        request: &ResourceControlRequest,
        settlement: Option<&ResourceControlSettlementRequest>,
    ) -> ModuleResult<ResolvedControl> {
        let journal = &self.coordinator.shared.borrow().value;
        let admission = journal
            .authority
            .admissions
            .get(&request.admission_id)
            .ok_or("control_admission_unavailable")?;
        let dependent = &admission.provider;
        let mut works = journal.receipts.values().filter_map(|receipt| {
            if receipt.response["status"] != "pending" {
                return None;
            }
            let work = receipt.work.as_ref()?;
            let state = work.states.get(dependent)?;
            (state.admission_id.as_deref() == Some(request.admission_id.as_str())
                && state.plan.as_ref() == Some(&admission.plan)
                && matches!(
                    work.phase,
                    LifecyclePhase::StreamPrepare | LifecyclePhase::StreamRelease
                )
                && state.request.phase == work.phase
                && work.target == request.target
                && work.transition_id == request.transition_id
                && !work.waiting_retirement
                && work.recovery_authorized_epoch.is_none())
            .then_some((work, state))
        });
        let (work, state) = works.next().ok_or("active_control_work_unavailable")?;
        if works.next().is_some() {
            return Err("ambiguous_control_work".into());
        }
        let owner = &request.proposal.owner_provider;
        let graph = work
            .provider_graph
            .as_ref()
            .ok_or("control_graph_unavailable")?;
        if !graph
            .dependencies
            .get(dependent)
            .is_some_and(|deps| deps.contains(owner))
            || !admission.plan.control_proposals.contains(&request.proposal)
            || (work.phase == LifecyclePhase::StreamRelease && work.cleanup_blocked(dependent))
        {
            return Err("control_not_in_frozen_graph_or_plan".into());
        }
        let progress = work
            .composition_progress
            .as_ref()
            .ok_or("control_stage_unavailable")?;
        let mut selections = progress.selections.iter().filter(|selection| {
            selection.target == work.target
                && selection.control_policy.dependent_provider == *dependent
                && selection.control_policy.owner_provider == *owner
                && selection.input.output.output_id == request.proposal.output_id
                && (work.phase == LifecyclePhase::StreamRelease
                    || selection.transition_id == work.transition_id)
        });
        let selection = selections.next().ok_or("control_selection_unavailable")?;
        if selections.next().is_some()
            || journal.composition.selection(
                &selection.target,
                &selection.transition_id,
                dependent,
                owner,
                &selection.input.output.output_id,
            ) != Some(selection)
        {
            return Err("control_selection_mismatch".into());
        }
        let input = &selection.input;
        let stage = if matches!(input.output.origin, LifecycleTarget::Session { .. }) {
            if !journal.composition.validate_structure() {
                return Err("control_stage_proof_invalid".into());
            }
            let stage =
                journal
                    .composition
                    .staged_for(&work.target, owner, &input.output.output_id)?;
            if stage.before != input.output
                || (work.phase == LifecyclePhase::StreamPrepare
                    && stage.transition_id != work.transition_id)
            {
                return Err("control_stage_provenance_mismatch".into());
            }
            Some(stage)
        } else {
            None
        };
        let output = stage.map_or(&input.output, |stage| &stage.after);
        let mut controls = progress.controls.iter().filter(|control| {
            control.dispatch.request == *request
                && control.dispatch.dependency == *output
                && match (&control.dispatch.phase, settlement) {
                    (super::control_dispatch::ControlPhase::Effect, None) => true,
                    (super::control_dispatch::ControlPhase::Applied { result }, Some(tuple)) => {
                        result.output == tuple.output
                            && control.settlement.as_ref().is_some_and(|stage| {
                                stage.request == *tuple
                                    && stage.policy == selection.control_policy
                                    && matches!(stage.phase, super::settlement_dispatch::SettlementPhase::Ready)
                                    && stage.owner_ack.is_none()
                                    && stage.dispatch.as_ref().is_some_and(|dispatch| dispatch.state == crate::provider_dispatch::DispatchState::OutcomeUnknown)
                            })
                    }
                    _ => false,
                }
        });
        if controls.next().is_none()
            || controls.next().is_some()
            || !progress.selections.contains(selection)
            || !progress.inputs.contains(&selection.input)
        {
            return Err("control_stage_mismatch".into());
        }
        if !state.request.dependency_inputs.contains(output)
            || journal
                .composition
                .get(owner, &input.output.output_id, &input.output.origin)
                != Some(input)
            || !selection.validate_structure()
            || !self
                .coordinator
                .settings
                .control_policies
                .contains(&selection.control_policy)
            || !self
                .coordinator
                .settings
                .finalize_policies
                .contains(&selection.finalize_policy)
            || selection
                .schema_documents
                .iter()
                .any(|doc| !self.coordinator.settings.schema_documents.contains(doc))
        {
            return Err("control_frozen_selection_mismatch".into());
        }
        let source = journal
            .authority
            .admissions
            .get(&input.admission_id)
            .or_else(|| journal.authority.settled.get(&input.admission_id))
            .ok_or("producer_admission_unavailable")?;
        if source.provider != *owner
            || source.plan != input.plan
            || journal.admission_transitions.get(&input.admission_id) != Some(&input.transition_id)
        {
            return Err("control_output_provenance_mismatch".into());
        }
        // Every method used for effects or polling must have a current broker
        // approval equal to its independently frozen selection.
        let approved = self
            .coordinator
            .selections
            .get(owner)
            .ok_or("owner_approval_unavailable")?;
        if [
            &selection.control_approval,
            &selection.control_status_approval,
            &selection.finalize_approval,
            &selection.finalize_status_approval,
        ]
        .iter()
        .any(|approval| !approved.approvals.contains(approval))
        {
            return Err("owner_approval_changed".into());
        }
        if settlement.is_some() {
            let stage = progress
                .controls
                .iter()
                .find(|control| control.dispatch.request == *request)
                .and_then(|control| control.settlement.as_ref())
                .ok_or("settlement_stage_unavailable")?;
            if !approved.approvals.iter().any(|approval| {
                approval.method.service == selection.control_policy.settlement_method.service
                    && approval.method.method == selection.control_policy.settlement_method.method
                    && approval.method.interface_major
                        == selection.control_policy.settlement_method.interface_major
                    && approval.method.interface_minor
                        == selection.control_policy.settlement_method.interface_minor
                    && approval.method.schema_revision
                        == selection.control_policy.settlement_method.schema_revision
                    && approval.broker_approval == stage.call.approval
            }) {
                return Err("settlement_approval_unavailable".into());
            }
        }
        let (epoch, session_id, stream_id, generation) =
            super::operations::target_parts(&work.target)?;
        let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
        let stream = stream_id
            .and_then(|id| session.streams.get(id))
            .ok_or("unknown_stream")?;
        if epoch != journal.host_epoch
            || session.host_epoch != epoch
            || stream.generation != generation
            || !target_phase_allowed(&work.phase, stream, session)
            || session.admission_targets.get(&request.admission_id) != Some(&work.target)
            || session.admission_phases.get(&request.admission_id) != Some(&work.phase)
            || journal.admission_transitions.get(&request.admission_id) != Some(&work.transition_id)
            || session.streams.iter().any(|(id, other)| {
                Some(id.as_str()) != stream_id
                    && other.phase != StreamPhase::Retired
                    && !(other.phase == StreamPhase::Quiescing
                        && other.retirements == other.required)
            })
        {
            return Err("control_target_or_consumers_stale".into());
        }
        if let Some(stage) = stage {
            let stage_admission = journal
                .authority
                .admissions
                .get(&stage.admission_id)
                .or_else(|| journal.authority.settled.get(&stage.admission_id))
                .ok_or("control_stage_admission_unavailable")?;
            if stage.parent_generation != session.generation
                || !session.admission_ids.contains(&stage.admission_id)
                || session.admission_targets.get(&stage.admission_id) != Some(&stage.target)
                || session.admission_phases.get(&stage.admission_id)
                    != Some(&LifecyclePhase::StreamPrepare)
                || journal.admission_transitions.get(&stage.admission_id)
                    != Some(&stage.transition_id)
                || stage_admission.provider != *owner
                || stage_admission.plan != stage.plan
                || stage_admission.selection.participant != stage.callback_proof.participant
                || !stage_admission.selection.approvals.iter().any(|approval| {
                    approval.broker_approval == stage.callback_proof.terminal.call.approval
                        && super::composition::approval_matches_call(
                            approval,
                            &stage.callback_proof.terminal.call,
                        )
                })
            {
                return Err("control_stage_provenance_mismatch".into());
            }
        }
        if let LifecycleTarget::Session {
            generation: parent, ..
        } = output.origin
            && parent != session.generation
        {
            return Err("control_parent_stale".into());
        }
        self.coordinator
            .settings
            .schema_catalog()?
            .validate(&output.schema, &output.data)?;
        Ok(ResolvedControl {
            participant: admission.selection.participant.clone(),
            consumers_retired: work.phase == LifecyclePhase::StreamRelease
                && stream.retirements == stream.required,
            session_target: LifecycleTarget::Session {
                host_epoch: session.host_epoch.clone(),
                session_id: session.id.clone(),
                generation: session.generation,
            },
            context: ControlContext {
                policy: selection.control_policy.clone(),
                output: output.clone(),
                target: work.target.clone(),
                transition_id: work.transition_id.clone(),
                parent_generation: Some(session.generation),
            },
        })
    }
}

impl ControlEndpointBackend for Backend<'_> {
    fn resolve(&mut self, request: &ResourceControlRequest) -> ModuleResult<ResolvedControl> {
        self.resolve_control(request, None)
    }
    fn validate_arguments(&mut self, schema: &SchemaRef, arguments: &Value) -> ModuleResult<()> {
        self.coordinator
            .settings
            .schema_catalog()?
            .validate(schema, arguments)
    }
    fn describe(
        &mut self,
        owner: &str,
        resolved: &ResolvedControl,
        request: &ResourceControlRequest,
    ) -> ModuleResult<()> {
        let catalog = self.coordinator.settings.schema_catalog()?;
        self.authority(request)?.describe_control_permit(
            owner,
            &resolved.participant,
            &resolved.context,
            request,
            |schema, value| catalog.validate(schema, value),
        )
    }
    fn consume(
        &mut self,
        owner: &str,
        resolved: &ResolvedControl,
        request: &ResourceControlRequest,
    ) -> ModuleResult<ControlPermitUse> {
        let catalog = self.coordinator.settings.schema_catalog()?;
        self.authority(request)?.consume_control_permit(
            owner,
            &resolved.participant,
            &resolved.context,
            request,
            |schema, value| catalog.validate(schema, value),
        )
    }
    fn validate(
        &mut self,
        owner: &str,
        resolved: &ResolvedControl,
        request: &ResourceControlRequest,
    ) -> ModuleResult<()> {
        let catalog = self.coordinator.settings.schema_catalog()?;
        self.authority(request)?.validate_control_permit(
            owner,
            &resolved.participant,
            &resolved.context,
            request,
            |schema, value| catalog.validate(schema, value),
        )
    }
}

impl Coordinator {
    /// Called only with the broker-authenticated immediate owner principal.
    pub(super) fn control_operation(&mut self, caller: &str, payload: Value) -> Value {
        control_endpoint(caller, payload, &mut Backend { coordinator: self })
    }

    /// Owner CAS verification. The tuple must already be durably recorded in
    /// the selected pending work and a matching authenticated refinement.
    pub(super) fn verify_control_settlement(&mut self, caller: &str, mut payload: Value) -> Value {
        let result = (|| -> ModuleResult<Value> {
            let object = payload
                .as_object_mut()
                .ok_or("invalid_settlement_envelope")?;
            if object.remove("operation") != Some(json!("verify_control_settlement")) {
                return Err("invalid_settlement_operation".into());
            }
            let tuple: ResourceControlSettlementRequest =
                serde_json::from_value(payload).map_err(|_| "invalid_settlement_request")?;
            amaterasu_extension_api::validate_resource_control_settlement_request(&tuple)?;
            if caller.is_empty() || caller != tuple.request.proposal.owner_provider {
                return Err("settlement_owner_mismatch".into());
            }
            if let Some(result) = self.verify_historical_settlement(caller, &tuple)? {
                return Ok(result);
            }
            let mut backend = Backend { coordinator: self };
            let resolved = backend.resolve_control(&tuple.request, Some(&tuple))?;
            {
                let shared = backend.coordinator.shared.borrow();
                let record = shared
                    .value
                    .control_settlements
                    .get(&tuple.settlement_id)
                    .ok_or("settlement_record_unavailable")?;
                if record.settlement != tuple
                    || record.owner_ack.is_some()
                    || record.owner_dispatch.is_some()
                {
                    return Err("settlement_record_mismatch".into());
                }
                let mut stages = shared.value.receipts.values().filter_map(|receipt| {
                    if receipt.response["status"] != "pending" {
                        return None;
                    }
                    let work = receipt.work.as_ref()?;
                    let progress = work.composition_progress.as_ref()?;
                    progress
                        .controls
                        .iter()
                        .find(|control| control.dispatch.request == tuple.request)
                });
                let stage = stages.next().ok_or("settlement_control_unavailable")?;
                if stages.next().is_some()
                    || stage.dispatch.dispatch.as_ref() != Some(&record.effect_dispatch)
                    || !matches!(&stage.dispatch.phase, super::control_dispatch::ControlPhase::Applied { result } if **result == record.confirmed)
                {
                    return Err("settlement_effect_proof_mismatch".into());
                }
            }
            backend.validate(caller, &resolved, &tuple.request)?;
            backend
                .coordinator
                .settings
                .schema_catalog()?
                .validate(&tuple.output.schema, &tuple.output.data)?;
            Ok(
                json!({"status":"valid", "session_target":resolved.session_target, "target":resolved.context.target, "output":tuple.output, "consumers_retired":resolved.consumers_retired}),
            )
        })();
        result.unwrap_or_else(
            |_| json!({"status":"rejected","reason":"resource_control_not_authorized"}),
        )
    }

    /// Read-only proof of an already acknowledged exact tuple. It grants no
    /// mutation authority and does not require the original Work to be pending.
    fn verify_historical_settlement(
        &self,
        caller: &str,
        tuple: &ResourceControlSettlementRequest,
    ) -> ModuleResult<Option<Value>> {
        let shared = self.shared.borrow();
        let journal = &shared.value;
        let Some(record) = journal.control_settlements.get(&tuple.settlement_id) else {
            return Ok(None);
        };
        if record.owner_ack.is_none() {
            return Ok(None);
        }
        if record.settlement != *tuple || !journal.control_settlements.validate_structure() {
            return Err("settlement_record_mismatch".into());
        }
        let permit = journal
            .authority
            .control_permits
            .get(&tuple.request.permit_id)
            .ok_or("settlement_permit_unavailable")?;
        if !permit.consumed
            || !permit.settled
            || permit.request != tuple.request
            || caller != permit.context.policy.owner_provider
            || !record.valid_under_permit(permit)
        {
            return Err("settlement_permit_mismatch".into());
        }
        let admission = journal
            .authority
            .admissions
            .get(&tuple.request.admission_id)
            .or_else(|| journal.authority.settled.get(&tuple.request.admission_id))
            .ok_or("settlement_admission_unavailable")?;
        if admission.provider != permit.context.policy.dependent_provider
            || !admission
                .plan
                .control_proposals
                .contains(&tuple.request.proposal)
        {
            return Err("settlement_admission_mismatch".into());
        }
        let selection = journal
            .composition
            .selection(
                &tuple.request.target,
                &record.selection_transition_id,
                &admission.provider,
                caller,
                &tuple.request.proposal.output_id,
            )
            .ok_or("settlement_selection_unavailable")?;
        if !selection.validate_structure()
            || selection.control_policy != permit.context.policy
            || journal.composition.get(
                caller,
                &selection.input.output.output_id,
                &selection.input.output.origin,
            ) != Some(&selection.input)
            || !self
                .settings
                .control_policies
                .contains(&selection.control_policy)
            || selection
                .schema_documents
                .iter()
                .any(|doc| !self.settings.schema_documents.contains(doc))
        {
            return Err("settlement_selection_mismatch".into());
        }
        if selection.input.output != permit.context.output {
            let stage = journal.composition.staged_for(
                &tuple.request.target,
                caller,
                &selection.input.output.output_id,
            )?;
            let stage_admission = journal
                .authority
                .admissions
                .get(&stage.admission_id)
                .or_else(|| journal.authority.settled.get(&stage.admission_id))
                .ok_or("settlement_stage_admission_unavailable")?;
            let (_, session_id, _, _) = super::operations::target_parts(&stage.target)?;
            let stage_session = journal.sessions.get(session_id).ok_or("unknown_session")?;
            if !journal.composition.validate_structure()
                || stage.before != selection.input.output
                || stage.after != permit.context.output
                || stage.transition_id != record.selection_transition_id
                || stage.parent_generation != stage_session.generation
                || !stage_session.admission_ids.contains(&stage.admission_id)
                || stage_session.admission_targets.get(&stage.admission_id) != Some(&stage.target)
                || stage_session.admission_phases.get(&stage.admission_id)
                    != Some(&LifecyclePhase::StreamPrepare)
                || journal.admission_transitions.get(&stage.admission_id)
                    != Some(&stage.transition_id)
                || stage_admission.provider != caller
                || stage_admission.plan != stage.plan
                || stage_admission.selection.participant != stage.callback_proof.participant
                || !stage_admission.selection.approvals.iter().any(|approval| {
                    approval.broker_approval == stage.callback_proof.terminal.call.approval
                        && super::composition::approval_matches_call(
                            approval,
                            &stage.callback_proof.terminal.call,
                        )
                })
            {
                return Err("settlement_stage_provenance_mismatch".into());
            }
        }
        let source = journal
            .authority
            .admissions
            .get(&selection.input.admission_id)
            .or_else(|| journal.authority.settled.get(&selection.input.admission_id))
            .ok_or("settlement_source_unavailable")?;
        if source.provider != caller
            || source.plan != selection.input.plan
            || journal
                .admission_transitions
                .get(&selection.input.admission_id)
                != Some(&selection.input.transition_id)
        {
            return Err("settlement_source_mismatch".into());
        }
        let effect = &record.effect_dispatch.call;
        let effect_approved = [
            &selection.control_approval,
            &selection.control_status_approval,
        ]
        .iter()
        .any(|approval| super::composition::approval_matches_call(approval, effect));
        if record.effect_pending_proof.as_ref().is_some_and(|proof| {
            !super::composition::approval_matches_call(&selection.control_approval, &proof.call)
        }) {
            return Err("settlement_pending_approval_mismatch".into());
        }
        let settled = record
            .owner_dispatch
            .as_ref()
            .ok_or("settlement_ack_missing")?;
        if !effect_approved
            || !super::composition::approval_matches_call(
                &selection.settlement_approval,
                &settled.call,
            )
        {
            return Err("settlement_proof_approval_mismatch".into());
        }
        let (epoch, session_id, stream_id, generation) =
            super::operations::target_parts(&tuple.request.target)?;
        let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
        if epoch != journal.host_epoch
            || session.host_epoch != epoch
            || stream_id
                .and_then(|id| session.streams.get(id))
                .is_none_or(|stream| stream.generation != generation)
        {
            return Err("settlement_target_stale".into());
        }
        if let LifecycleTarget::Session {
            generation: parent, ..
        } = selection.input.output.origin
            && parent != session.generation
        {
            return Err("settlement_parent_stale".into());
        }
        self.settings
            .schema_catalog()?
            .validate(&tuple.output.schema, &tuple.output.data)?;
        Ok(Some(
            json!({"status":"valid", "session_target":{"kind":"session","host_epoch":session.host_epoch,"session_id":session.id,"generation":session.generation}, "target":tuple.request.target,"output":tuple.output,"root_output":selection.input.output,"consumers_retired":false}),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_requires_exact_quiescence_and_consumer_retirement() {
        let mut stream = StreamRecord {
            provider_graph: None,
            generation: 1,
            phase: StreamPhase::Preparing,
            candidate: None,
            required: ["capture".into()].into(),
            acknowledgements: BTreeSet::new(),
            retirements: BTreeSet::new(),
            binding_owner: String::new(),
            preparation_transition: String::new(),
            provider_settings: BTreeMap::new(),
        };
        let mut session: SessionRecord = serde_json::from_value(json!({"host_epoch":"epoch","id":"session","generation":1,"phase":"ready","owner":{"caller":"c","initiator":"i"},"delegates":[],"application_ref":{},"settings_revision":"one","application_retired":false,"streams":{}})).unwrap();
        assert!(target_phase_allowed(
            &LifecyclePhase::StreamPrepare,
            &stream,
            &session
        ));
        assert!(!target_phase_allowed(
            &LifecyclePhase::StreamRelease,
            &stream,
            &session
        ));
        stream.phase = StreamPhase::Quiescing;
        assert!(!target_phase_allowed(
            &LifecyclePhase::StreamRelease,
            &stream,
            &session
        ));
        stream.retirements.insert("capture".into());
        assert!(target_phase_allowed(
            &LifecyclePhase::StreamRelease,
            &stream,
            &session
        ));
        session.phase = SessionPhase::Ending;
        assert!(target_phase_allowed(
            &LifecyclePhase::StreamRelease,
            &stream,
            &session
        ));
        session.phase = SessionPhase::RecoveryRequired;
        assert!(!target_phase_allowed(
            &LifecyclePhase::StreamRelease,
            &stream,
            &session
        ));
    }
    #[test]
    fn request_without_durable_admission_cannot_create_authority() {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-control-backend-{}-{}",
            std::process::id(),
            identifier().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&path.join("journal.json")).unwrap();
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        let request = serde_json::from_value(json!({"version":2,"transition_id":"transition","operation_id":"effect","provider_sequence":1,"target":target,"proposal":{"proposal_id":"proposal","output_id":"out","owner_provider":"owner","owner_service":"owner/2","target":target,"resource":{"provider":"owner","resource_id":"out","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"method":{"service":"owner/2","method":"control","interface_major":2,"interface_minor":0,"schema_revision":1},"argument_schema":{"id":"schema","major":1,"minor":0,"digest":"a".repeat(64)},"arguments":{},"access":"shared","mutation_domains":[]},"admission_id":"invented","permit_id":"invented","remaining_ms":1000})).unwrap();
        assert!(
            Backend {
                coordinator: &mut coordinator
            }
            .resolve(&request)
            .is_err()
        );
        let output = json!({"output_id":"out","capability":"display","schema":request.proposal.argument_schema,"producer":"owner","producer_service":"owner/2","origin":target,"resource":request.proposal.resource,"data":{},"domain_observations":[]});
        let envelope = json!({"operation":"verify_control_settlement","request":request,"output":output,"settlement_id":"invented"});
        assert_eq!(
            coordinator.verify_control_settlement("owner", envelope)["status"],
            "rejected"
        );
        assert!(
            coordinator
                .shared
                .borrow()
                .value
                .authority
                .control_permits
                .is_empty()
        );
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn work_authority_reconstructs_nonempty_borrower_scope() {
        use crate::prepare_driver::PrepareAuthority;
        let path =
            std::env::temp_dir().join(format!("amaterasu-control-scope-{}", identifier().unwrap()));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&path.join("journal.json")).unwrap();
        let epoch = coordinator.shared.borrow().value.host_epoch.clone();
        let target =
            json!({"kind":"session","host_epoch":epoch,"session_id":"session","generation":1});
        let method = json!({"service":"owner/1","method":"plan","interface_major":1,"interface_minor":0,"schema_revision":1});
        let participant = json!({"provider":"owner","plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"one","settings":{}});
        let selection: crate::prepare_authority::ProviderSelection = serde_json::from_value(json!({"participant":participant,"approvals":[{"method":method,"broker_approval":{"provider":"owner","method":{"service":"owner/1","method":"plan","interface_major":1,"interface_minor":0}}}],"capabilities":[],"cleanup_methods":[],"authorized_borrowers":[]})).unwrap();
        let plan = serde_json::from_value(json!({"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        let mut admitted_selection = selection.clone();
        admitted_selection
            .authorized_borrowers
            .insert("session".into());
        let admission = crate::prepare_authority::Admission {
            provider: "owner".into(),
            selection: admitted_selection,
            plan,
            rights: vec![],
        };
        let mut session: SessionRecord = serde_json::from_value(json!({"host_epoch":epoch,"id":"session","generation":1,"phase":"ready","owner":{"caller":"c","initiator":"i"},"delegates":[],"application_ref":{},"settings_revision":"one","application_retired":false,"streams":{},"obligations":[{"obligation_id":"root","provider":"owner","resource_id":"output","mutation_domain":"domain","depends_on":[],"cleanup_method":method,"payload":{}}]})).unwrap();
        session.provider_settings.clear();
        let work: Work = serde_json::from_value(json!({"phase":"session.prepare","target":target,"intent":null,"reason":null,"video":null,"transition_id":"transition","providers":[],"index":0,"states":{},"candidate":null,"waiting_retirement":false})).unwrap();
        coordinator
            .selections
            .insert("owner".into(), selection.clone());
        {
            let mut shared = coordinator.shared.borrow_mut();
            shared.value.sessions.insert("session".into(), session);
            shared
                .value
                .authority
                .admissions
                .insert("admission".into(), admission.clone());
        }
        assert!(
            coordinator
                .authority_for_work(&work)
                .unwrap()
                .validate_rights(&selection.participant, &admission.plan, "admission")
                .is_ok()
        );
        coordinator
            .shared
            .borrow_mut()
            .value
            .sessions
            .get_mut("session")
            .unwrap()
            .obligations
            .clear();
        assert_eq!(
            coordinator
                .authority_for_work(&work)
                .unwrap()
                .validate_rights(&selection.participant, &admission.plan, "admission")
                .unwrap_err(),
            "admission changed or rights lost"
        );
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn historical_owner_ack_requires_exact_durable_tuple() {
        use sha2::{Digest, Sha256};
        let path = std::env::temp_dir().join(format!(
            "amaterasu-historical-settlement-{}-{}",
            std::process::id(),
            identifier().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&path.join("journal.json")).unwrap();
        let epoch = coordinator.shared.borrow().value.host_epoch.clone();
        let session =
            json!({"kind":"session","host_epoch":epoch,"session_id":"session","generation":1});
        let target = json!({"kind":"stream","host_epoch":epoch,"session_id":"session","stream_id":"stream","generation":1});
        let document = r#"{"$id":"example.schema/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false}"#;
        let schema = json!({"id":"example.schema/1","major":1,"minor":0,"digest":format!("{:x}", Sha256::digest(document.as_bytes()))});
        let resource = json!({"provider":"owner","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":"domain","mutation_domains":["domain"]});
        let output = json!({"output_id":"out","capability":"display","schema":schema,"producer":"owner","producer_service":"owner/2","origin":session,"resource":resource,"domain_observations":[{"domain":"domain","revision":1,"borrowers":{},"complete":true}],"data":{}});
        let method = |name: &str| json!({"service":"owner/2","method":name,"interface_major":2,"interface_minor":0,"schema_revision":1});
        let approval = |name: &str| json!({"method":method(name),"broker_approval":{"provider":"owner","method":{"service":"owner/2","method":name,"interface_major":2,"interface_minor":0}}});
        let proposal = json!({"proposal_id":"proposal","output_id":"out","owner_provider":"owner","owner_service":"owner/2","target":target,"resource":resource,"method":method("control"),"argument_schema":schema,"arguments":{},"access":"exclusive","mutation_domains":["domain"]});
        let source_plan = json!({"plan_id":"source","revision":1,"resources":[{"provider":"owner","resource_id":"output","incarnation":"native","expected_revision":1,"execution_domain":"seat","mutation_domains":["domain"],"access":"read"}],"mutation_domains":[{"domain":"domain","expected_revision":1,"borrowers":{},"access":"read"}],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]});
        let participant = |name: &str| json!({"provider":name,"plan_method":method("plan"),"callback_method":method("callback"),"status_method":method("status"),"capabilities":[],"cleanup_methods":[],"settings_revision":"one","settings":{}});
        let source_request = json!({"version":2,"provider_sequence":1,"operation":"lifecycle_callback","transition_id":"source-transition","operation_id":"source-op","target":session,"phase":"session.prepare","settings_revision":"one","settings":{},"intent":null,"reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector"]},"remaining_ms":1000,"plan_revision":1,"admission_id":"source","approved_plan":source_plan});
        let prepared =
            json!({"status":"prepared","candidate":null,"obligations":[],"outputs":[output]});
        let callback_call = json!({"service":"owner/2","method":"callback","interface_major":2,"interface_minor":0,"approval":approval("callback")["broker_approval"],"arguments":source_request,"timeout_ms":1000,"operation_id":"source-op"});
        let proof = json!({"participant":participant("owner"),"request":source_request,"terminal":{"call":callback_call,"state":{"state":"completed","payload":prepared}},"pending_receipt":null});
        let stored = json!({"output":output,"admission_id":"source","transition_id":"source-transition","plan":source_plan,"state":"retained","callback_proof":proof});
        let policy = json!({"dependent_provider":"dependent","owner_provider":"owner","owner_service":"owner/2","owner_method":method("control"),"status_method":method("status"),"settlement_method":method("settle"),"argument_schema":schema,"output_capability":"display","output_schema":schema,"max_access":"exclusive","domain_scope":"resource","mutation_domains":[]});
        let final_policy = json!({"owner_provider":"owner","owner_service":"owner/2","method":method("finalize"),"status_method":method("status"),"output_capability":"display","output_schema":schema,"capture_provider":"capture","selector_schema":"selector","required_access":"exclusive","domain_scope":"resource","mutation_domains":[]});
        let frozen = json!({"target":target,"transition_id":"selection-transition","input":stored,"control_policy":policy,"finalize_policy":final_policy,"control_approval":approval("control"),"control_status_approval":approval("status"),"settlement_approval":approval("settle"),"finalize_approval":approval("finalize"),"finalize_status_approval":approval("status"),"schema_documents":[{"reference":schema,"document":document}]});
        let request = json!({"version":2,"transition_id":"control-transition","operation_id":"effect","provider_sequence":1,"target":target,"proposal":proposal,"admission_id":"dependent-admission","permit_id":"permit","remaining_ms":1000});
        let tuple = json!({"request":request,"output":output,"settlement_id":"settlement"});
        let applied = json!({"status":"applied","output":output,"candidate":null});
        let call = |name: &str, args: Value, id: &str| json!({"service":"owner/2","method":name,"interface_major":2,"interface_minor":0,"approval":approval(name)["broker_approval"],"arguments":args,"timeout_ms":1000,"operation_id":id});
        let record = json!({"settlement":tuple,"selection_transition_id":"selection-transition","confirmed":{"proposal_id":"proposal","output":output,"candidate":null},"applied":applied,"effect_dispatch":{"call":call("control",request.clone(),"effect"),"state":{"state":"completed","payload":applied}},"owner_receipt_id":null,"owner_ack":applied,"owner_dispatch":{"call":call("settle",tuple.clone(),"effect.settle"),"state":{"state":"completed","payload":applied}}});
        let dependent_plan = json!({"plan_id":"dependent","revision":1,"resources":[],"mutation_domains":[{"domain":"domain","expected_revision":1,"borrowers":{},"access":"exclusive"}],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[],"control_proposals":[proposal]});
        let selection = |name: &str| json!({"participant":participant(name),"approvals":[approval("plan"),approval("callback"),approval("status")],"capabilities":[],"cleanup_methods":[],"authorized_borrowers":[]});
        let admission = |provider: &str, plan: Value| json!({"provider":provider,"selection":selection(provider),"plan":plan,"rights":[]});
        let permit = json!({"context":{"policy":policy,"output":output,"target":target,"transition_id":"control-transition","parent_generation":1},"request":request,"consumed":true,"settled":true});
        let session_record = json!({"host_epoch":epoch,"id":"session","generation":1,"phase":"ready","owner":{"caller":"transport","initiator":"transport"},"delegates":[],"application_ref":{},"settings_revision":"one","application_retired":false,"streams":{"stream":{"generation":1,"phase":"active","candidate":null,"required":[],"acknowledgements":[],"retirements":[],"provider_graph":{"order":["owner","dependent"],"dependencies":{"owner":[],"dependent":["owner"]}}}},"admission_ids":["source"],"admission_targets":{"source":session},"admission_phases":{"source":"session.prepare"}});
        let mut journal = coordinator.shared.borrow().value.clone();
        journal.sessions.insert(
            "session".into(),
            serde_json::from_value(session_record).unwrap(),
        );
        journal.composition = serde_json::from_value(json!({"outputs":[stored],"selections":[frozen],"finalize_selections":[],"refinements":[]})).unwrap();
        journal.control_settlements = serde_json::from_value(json!({"records":[record]})).unwrap();
        journal.authority.settled.insert(
            "source".into(),
            serde_json::from_value(admission("owner", source_plan)).unwrap(),
        );
        journal.authority.settled.insert(
            "dependent-admission".into(),
            serde_json::from_value(admission("dependent", dependent_plan)).unwrap(),
        );
        journal
            .authority
            .control_permits
            .insert("permit".into(), serde_json::from_value(permit).unwrap());
        journal
            .admission_transitions
            .insert("source".into(), "source-transition".into());
        coordinator
            .settings
            .control_policies
            .push(serde_json::from_value(policy).unwrap());
        coordinator
            .settings
            .schema_documents
            .push(serde_json::from_value(json!({"reference":schema,"document":document})).unwrap());
        let key = receipt_key("transport", "transport", "selection-op");
        journal.receipts.insert(key, serde_json::from_value(json!({"caller":"transport","initiator":"transport","request":{"operation_id":"selection-op"},"response":{"status":"succeeded","operation_id":"selection-op","transition_id":"selection-transition","target":target},"work":null})).unwrap());
        assert!(
            journal
                .receipts
                .values()
                .all(|receipt| receipt.work.is_none()),
            "the original Work has completed"
        );
        assert!(journal.control_settlements.validate_structure());
        validate_journal(&journal).expect("historical graph must survive journal validation");
        save(&coordinator.shared, journal).unwrap();
        let persisted: Journal =
            serde_json::from_slice(&std::fs::read(path.join("journal.json")).unwrap()).unwrap();
        validate_journal(&persisted).expect("persisted graph must retain valid provenance");
        let mut envelope = tuple.clone();
        envelope["operation"] = json!("verify_control_settlement");
        let valid = coordinator.verify_control_settlement("owner", envelope.clone());
        assert_eq!(valid["status"], "valid", "{valid}");
        assert_eq!(valid["root_output"], output);
        assert_eq!(
            coordinator.verify_control_settlement("foreign", envelope.clone())["status"],
            "rejected"
        );
        // A different permit input cannot claim a staged lineage without a
        // retained authenticated stage linking it to the frozen root.
        coordinator
            .shared
            .borrow_mut()
            .value
            .authority
            .control_permits
            .get_mut("permit")
            .unwrap()
            .context
            .output
            .resource
            .revision += 1;
        assert_eq!(
            coordinator.verify_control_settlement("owner", envelope.clone())["status"],
            "rejected"
        );
        coordinator
            .shared
            .borrow_mut()
            .value
            .authority
            .control_permits
            .get_mut("permit")
            .unwrap()
            .context
            .output
            .resource
            .revision -= 1;
        let mut changed = envelope;
        changed["output"]["resource"]["revision"] = json!(2);
        assert_eq!(
            coordinator.verify_control_settlement("owner", changed)["status"],
            "rejected"
        );
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }
}
