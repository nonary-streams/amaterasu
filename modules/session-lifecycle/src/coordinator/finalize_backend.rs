//! Finalize authorization reconstructed from fresh coordinator-owned work.
use super::finalize_endpoints::{FinalizeEndpointBackend, ResolvedFinalize, finalize_endpoint};
use super::*;
use crate::prepare_authority::{
    AuthorityJournal, ControlPermitUse, CoordinatorPrepareAuthority, FinalizeContext,
};
use crate::prepare_driver::PreparePhase;
use amaterasu_extension_api::{ProviderOperation, ResourceFinalizeRequest, ResourceOutput};

struct Writer(Shared);
impl AuthorityJournal for Writer {
    fn persist(&mut self, state: &AuthorityState) -> ModuleResult<()> {
        let mut next = self.0.borrow().value.clone();
        next.authority = state.clone();
        save(&self.0, next)
    }
}
struct Backend<'a> {
    coordinator: &'a mut Coordinator,
}
impl Backend<'_> {
    fn authority(&self) -> ModuleResult<CoordinatorPrepareAuthority<Writer>> {
        CoordinatorPrepareAuthority::new(
            self.coordinator.shared.borrow().value.authority.clone(),
            self.coordinator.selections.values().cloned().collect(),
            Writer(self.coordinator.shared.clone()),
        )
    }
}
impl FinalizeEndpointBackend for Backend<'_> {
    fn resolve(&mut self, request: &ResourceFinalizeRequest) -> ModuleResult<ResolvedFinalize> {
        let shared = self.coordinator.shared.borrow();
        let journal = &shared.value;
        let admission = journal
            .authority
            .admissions
            .get(&request.admission_id)
            .ok_or("finalize_admission_unavailable")?;
        let owner = &admission.provider;
        let mut matches = journal.receipts.values().filter_map(|receipt| {
            if receipt.response["status"] != "pending" {
                return None;
            }
            let work = receipt.work.as_ref()?;
            let progress = work.composition_progress.as_ref()?;
            let finalize = progress.finalize.as_ref()?;
            let dispatch = finalize.dispatch.as_ref()?;
            let plan = &finalize.owner_plan;
            (work.phase == LifecyclePhase::StreamPrepare
                && !work.waiting_retirement
                && work.recovery_authorized_epoch.is_none()
                && work.target == request.target
                && work.transition_id == request.transition_id
                && dispatch.request == *request
                && matches!(
                    dispatch.phase,
                    super::finalize_dispatch::FinalizePhase::Effect
                )
                && plan.provider == *owner
                && plan.phase == PreparePhase::FinalizePlanAdmitted
                && plan.request.operation == ProviderOperation::ResourceFinalizePlan
                && plan.request.target == work.target
                && plan.request.transition_id == work.transition_id
                && plan.admission_id.as_deref() == Some(request.admission_id.as_str())
                && plan.plan.as_ref() == Some(&admission.plan))
            .then_some((work, progress, finalize))
        });
        let (work, progress, finalize) = matches.next().ok_or("finalize_work_unavailable")?;
        if matches.next().is_some() {
            return Err("ambiguous_finalize_work".into());
        }
        let selection = &finalize.selection;
        let input = &selection.input;
        if selection.target != work.target
            || selection.transition_id != work.transition_id
            || selection.policy.owner_provider != *owner
            || !selection.validate_structure()
            || journal.composition.finalize_selection(
                &selection.target,
                &selection.transition_id,
                owner,
                &input.output.output_id,
            ) != Some(selection)
            || journal
                .composition
                .get(owner, &input.output.output_id, &input.output.origin)
                != Some(input)
            || !self
                .coordinator
                .settings
                .finalize_policies
                .contains(&selection.policy)
            || selection
                .schema_documents
                .iter()
                .any(|doc| !self.coordinator.settings.schema_documents.contains(doc))
        {
            return Err("finalize_selection_mismatch".into());
        }
        let source = journal
            .authority
            .admissions
            .get(&input.admission_id)
            .or_else(|| journal.authority.settled.get(&input.admission_id))
            .ok_or("finalize_source_unavailable")?;
        if source.provider != *owner
            || source.plan != input.plan
            || !super::composition::validate_callback_provenance(input, source)
            || journal.admission_transitions.get(&input.admission_id) != Some(&input.transition_id)
        {
            return Err("finalize_source_mismatch".into());
        }
        let selected = self
            .coordinator
            .selections
            .get(owner)
            .ok_or("finalize_owner_unavailable")?;
        if !selected.approvals.contains(&selection.finalize_approval)
            || !selected
                .approvals
                .contains(&selection.finalize_status_approval)
        {
            return Err("finalize_approval_changed".into());
        }
        let mut controls = Vec::new();
        let mut current_output = input.output.clone();
        if !journal.control_settlements.validate_structure() {
            return Err("invalid_control_settlement_journal".into());
        }
        for control in &progress.controls {
            if control.dispatch.request.proposal.owner_provider != *owner
                || control.dispatch.request.proposal.output_id != input.output.output_id
            {
                continue;
            }
            let super::control_dispatch::ControlPhase::Applied { result } = &control.dispatch.phase
            else {
                return Err("control_not_applied".into());
            };
            let settled = control.settlement.as_ref().ok_or("control_not_settled")?;
            if !matches!(
                settled.phase,
                super::settlement_dispatch::SettlementPhase::Acknowledged
            ) || settled.owner_ack.is_none()
                || settled.request.request != control.dispatch.request
                || settled.request.output != result.output
            {
                return Err("control_ack_mismatch".into());
            }
            let record = journal
                .control_settlements
                .get(&settled.request.settlement_id)
                .ok_or("control_settlement_unavailable")?;
            let permit = journal
                .authority
                .control_permits
                .get(&control.dispatch.request.permit_id)
                .ok_or("control_permit_unavailable")?;
            if record.settlement != settled.request
                || record.confirmed != **result
                || record.owner_ack != settled.owner_ack
                || !permit.consumed
                || !permit.settled
                || permit.request != control.dispatch.request
                || permit.context.output != current_output
                || record.owner_dispatch.is_none()
            {
                return Err("control_settlement_mismatch".into());
            }
            amaterasu_extension_api::validate_resource_control_settlement_response(
                &record.settlement,
                record.owner_ack.as_ref().ok_or("control_ack_missing")?,
            )?;
            amaterasu_extension_api::validate_output_refinement(&current_output, &result.output)?;
            current_output = result.output.clone();
            controls.push((**result).clone());
        }
        if request.output != current_output
            || request.control_results != controls
            || finalize.owner_plan.request.dependency_inputs != vec![current_output.clone()]
            || finalize.owner_plan.request.control_results != controls
        {
            return Err("finalize_inputs_mismatch".into());
        }
        amaterasu_extension_api::validate_provider_response(
            &finalize.owner_plan.request,
            &amaterasu_extension_api::ProviderResponse::Planned {
                plan: admission.plan.clone(),
            },
        )?;
        let dispatch = finalize
            .dispatch
            .as_ref()
            .ok_or("finalize_dispatch_missing")?;
        let frozen_context = FinalizeContext {
            policy: selection.policy.clone(),
            output: current_output.clone(),
            control_results: controls.clone(),
            target: work.target.clone(),
            transition_id: work.transition_id.clone(),
            parent_generation: selection.parent_generation,
        };
        if dispatch.context != frozen_context
            || dispatch.effect_call.approval != selection.finalize_approval.broker_approval
            || dispatch.status_selection.approval
                != selection.finalize_status_approval.broker_approval
            || super::finalize_dispatch::FinalizeDispatchState::new(
                dispatch.request.clone(),
                dispatch.context.clone(),
                dispatch.effect_call.clone(),
                dispatch.status_selection.clone(),
            )
            .is_err()
        {
            return Err("finalize_dispatch_mismatch".into());
        }
        let (epoch, session_id, stream_id, generation) =
            super::operations::target_parts(&work.target)?;
        let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
        let stream = stream_id
            .and_then(|id| session.streams.get(id))
            .ok_or("unknown_stream")?;
        if epoch != journal.host_epoch
            || epoch != session.host_epoch
            || stream.generation != generation
            || stream.phase != StreamPhase::Preparing
            || stream.candidate.is_some()
            || !stream.acknowledgements.is_empty()
            || !matches!(session.phase, SessionPhase::Ready | SessionPhase::Paused)
            || selection.parent_generation != Some(session.generation)
            || !session.admission_ids.contains(&input.admission_id)
            || session.admission_targets.get(&input.admission_id) != Some(&input.output.origin)
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
            return Err("finalize_target_or_consumers_stale".into());
        }
        let catalog = self.coordinator.settings.schema_catalog()?;
        catalog.validate(&current_output.schema, &current_output.data)?;
        for result in &controls {
            catalog.validate(&result.output.schema, &result.output.data)?;
        }
        Ok(ResolvedFinalize {
            participant: admission.selection.participant.clone(),
            context: FinalizeContext {
                policy: selection.policy.clone(),
                output: current_output,
                control_results: controls,
                target: work.target.clone(),
                transition_id: work.transition_id.clone(),
                parent_generation: Some(session.generation),
            },
            session_target: LifecycleTarget::Session {
                host_epoch: session.host_epoch.clone(),
                session_id: session.id.clone(),
                generation: session.generation,
            },
            admitted_plan: admission.plan.clone(),
            consumers_retired: false,
        })
    }
    fn validate_output(&mut self, output: &ResourceOutput) -> ModuleResult<()> {
        self.coordinator
            .settings
            .schema_catalog()?
            .validate(&output.schema, &output.data)
    }
    fn describe(
        &mut self,
        owner: &str,
        resolved: &ResolvedFinalize,
        request: &ResourceFinalizeRequest,
    ) -> ModuleResult<()> {
        self.authority()?.describe_finalize_permit(
            owner,
            &resolved.participant,
            &resolved.context,
            request,
        )
    }
    fn consume(
        &mut self,
        owner: &str,
        resolved: &ResolvedFinalize,
        request: &ResourceFinalizeRequest,
    ) -> ModuleResult<ControlPermitUse> {
        self.authority()?.consume_finalize_permit(
            owner,
            &resolved.participant,
            &resolved.context,
            request,
        )
    }
    fn validate(
        &mut self,
        owner: &str,
        resolved: &ResolvedFinalize,
        request: &ResourceFinalizeRequest,
    ) -> ModuleResult<()> {
        self.authority()?.validate_finalize_permit(
            owner,
            &resolved.participant,
            &resolved.context,
            request,
        )
    }
}
impl Coordinator {
    pub(super) fn finalize_operation(&mut self, caller: &str, payload: Value) -> Value {
        finalize_endpoint(caller, payload, &mut Backend { coordinator: self })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fabricated_request_cannot_create_finalize_authority() {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-finalize-backend-{}-{}",
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
        let output = json!({"output_id":"output","capability":"example.display","schema":{"id":"example.schema","major":2,"minor":0,"digest":"a".repeat(64)},"producer":"example.owner","producer_service":"example.owner/2","origin":target,"resource":{"provider":"example.owner","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"data":{}});
        let request: ResourceFinalizeRequest = serde_json::from_value(json!({"version":2,"transition_id":"transition","operation_id":"finalize","provider_sequence":1,"target":target,"output":output,"control_results":[],"admission_id":"fabricated","permit_id":"fabricated","remaining_ms":1000})).unwrap();
        assert!(
            Backend {
                coordinator: &mut coordinator
            }
            .resolve(&request)
            .is_err()
        );
        let result = coordinator.finalize_operation(
            "example.owner",
            json!({"operation":"verify_resource_finalize","request":request}),
        );
        assert_eq!(result["status"], "rejected");
        assert!(
            coordinator
                .shared
                .borrow()
                .value
                .authority
                .finalize_permits
                .is_empty()
        );
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }
}
