//! Concrete control verification backed by coordinator-owned durable state.
use super::control_endpoints::{ControlEndpointBackend, ResolvedControl, control_endpoint};
use super::*;
use crate::prepare_authority::{
    AuthorityJournal, ControlContext, ControlPermitUse, CoordinatorPrepareAuthority,
};
use amaterasu_extension_api::{ResourceControlRequest, SchemaRef};

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
    fn authority(&self) -> ModuleResult<CoordinatorPrepareAuthority<Writer>> {
        CoordinatorPrepareAuthority::new(
            self.coordinator.shared.borrow().value.authority.clone(),
            self.coordinator.selections.values().cloned().collect(),
            Writer(self.coordinator.shared.clone()),
        )
    }
}

impl ControlEndpointBackend for Backend<'_> {
    fn resolve(&mut self, request: &ResourceControlRequest) -> ModuleResult<ResolvedControl> {
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
        let mut controls = progress.controls.iter().filter(|control| {
            control.request == *request
                && control.dependency == selection.input.output
                && matches!(control.phase, super::control_dispatch::ControlPhase::Effect)
        });
        if controls.next().is_none()
            || controls.next().is_some()
            || !progress.selections.contains(selection)
            || !progress.inputs.contains(&selection.input)
        {
            return Err("control_stage_mismatch".into());
        }
        let input = &selection.input;
        let output = &input.output;
        if !state.request.dependency_inputs.contains(output)
            || journal
                .composition
                .get(owner, &output.output_id, &output.origin)
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
        self.authority()?.describe_control_permit(
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
        self.authority()?.consume_control_permit(
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
        self.authority()?.validate_control_permit(
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
}
