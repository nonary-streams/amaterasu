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
        let mut current_output =
            authenticated_current_output(&journal.composition, selection)?.clone();
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
/// Resolve only the stage recorded by the coordinator, never a caller-provided output.
pub(super) fn authenticated_current_output<'a>(
    book: &super::composition::CompositionBook,
    selection: &'a super::finalize_selection::FinalizeSelectionSnapshot,
) -> ModuleResult<&'a ResourceOutput> {
    if let Some(stage) = &selection.stage {
        if book.staged_for(
            &selection.target,
            &selection.input.output.producer,
            &selection.input.output.output_id,
        )? != stage
        {
            return Err("finalize_stage_mismatch".into());
        }
    } else if matches!(
        (&selection.input.output.origin, &selection.target),
        (
            LifecycleTarget::Session { .. },
            LifecycleTarget::Stream { .. }
        )
    ) {
        return Err("finalize_stage_missing".into());
    }
    Ok(selection.current_output())
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
    #[test]
    fn durable_graph_consumes_once_and_rejects_forged_or_stale_requests() {
        use sha2::{Digest, Sha256};
        let path = std::env::temp_dir().join(format!(
            "amaterasu-finalize-positive-{}-{}",
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
        let stream = json!({"kind":"stream","host_epoch":epoch,"session_id":"session","stream_id":"stream","generation":1});
        let document = r#"{"$id":"example.schema/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false}"#;
        let schema = json!({"id":"example.schema/1","major":1,"minor":0,"digest":format!("{:x}", Sha256::digest(document.as_bytes()))});
        let resource = json!({"provider":"owner","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":"domain","mutation_domains":["domain"]});
        let output = json!({"output_id":"out","capability":"display","schema":schema,"producer":"owner","producer_service":"owner/2","origin":session,"resource":resource,"domain_observations":[{"domain":"domain","revision":1,"borrowers":{},"complete":true}],"data":{}});
        let method = |name: &str| json!({"service":"owner/2","method":name,"interface_major":2,"interface_minor":0,"schema_revision":1});
        let approval = |name: &str| json!({"method":method(name),"broker_approval":{"provider":"owner","method":{"service":"owner/2","method":name,"interface_major":2,"interface_minor":0}}});
        let participant = json!({"provider":"owner","contract_version":2,"plan_method":method("plan"),"callback_method":method("callback"),"status_method":method("status"),"capabilities":[],"cleanup_methods":[method("callback")],"settings_revision":"one","settings":{}});
        let selection = json!({"participant":participant,"approvals":[approval("plan"),approval("callback"),approval("status"),approval("finalize")],"capabilities":[],"cleanup_methods":[method("callback")],"authorized_borrowers":[]});
        let obligation = json!({"obligation_id":"restore","provider":"owner","resource_id":"output","mutation_domain":"domain","depends_on":[],"cleanup_method":method("callback"),"payload":{}});
        let source_plan = json!({"plan_id":"source","revision":1,"resources":[{"provider":"owner","resource_id":"output","incarnation":"native","expected_revision":1,"execution_domain":"seat","mutation_domains":["domain"],"access":"exclusive"}],"mutation_domains":[{"domain":"domain","expected_revision":1,"borrowers":{},"access":"exclusive"}],"required_capabilities":[],"constraints":{},"obligations":[obligation],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]});
        let mut final_plan = source_plan.clone();
        final_plan["plan_id"] = json!("final");
        final_plan["candidate_constraint"] = json!({"capture_provider":"capture","selector_schema":"selector","resource_provider":"owner","resource_id":"output","incarnation":"native","execution_domain":"seat","mode":null,"plan_revision":1,"geometry_revision":2});
        let requirements = json!({"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector"]});
        let callback = json!({"version":2,"provider_sequence":1,"operation":"lifecycle_callback","transition_id":"source-transition","operation_id":"source-op","target":session,"phase":"session.prepare","settings_revision":"one","settings":{},"intent":null,"reason":null,"requested_video":null,"consumer_requirements":requirements,"remaining_ms":5000,"plan_revision":1,"admission_id":"source","approved_plan":source_plan});
        let prepared = json!({"status":"prepared","candidate":null,"obligations":[obligation],"outputs":[output]});
        let call = |name: &str, args: Value, operation: &str| json!({"service":"owner/2","method":name,"interface_major":2,"interface_minor":0,"approval":approval(name)["broker_approval"],"arguments":args,"timeout_ms":5000,"operation_id":operation});
        let proof = json!({"participant":participant,"request":callback,"terminal":{"call":call("callback",callback.clone(),"source-op"),"state":{"state":"completed","payload":prepared}},"pending_receipt":null});
        let stored = json!({"output":output,"admission_id":"source","transition_id":"source-transition","plan":source_plan,"state":"retained","callback_proof":proof});
        let policy = json!({"owner_provider":"owner","owner_service":"owner/2","method":method("finalize"),"status_method":method("status"),"output_capability":"display","output_schema":schema,"capture_provider":"capture","selector_schema":"selector","required_access":"exclusive","domain_scope":"resource","mutation_domains":[]});
        let stage_request = json!({"version":2,"provider_sequence":2,"operation":"lifecycle_callback","transition_id":"transition","operation_id":"stage-op","target":stream,"phase":"stream.prepare","settings_revision":"one","settings":{},"intent":"initial","reason":null,"requested_video":null,"consumer_requirements":requirements,"remaining_ms":5000,"plan_revision":1,"admission_id":"stage","approved_plan":source_plan});
        let stage_proof = json!({"participant":participant,"request":stage_request,"terminal":{"call":call("callback",stage_request.clone(),"stage-op"),"state":{"state":"completed","payload":prepared}},"pending_receipt":null});
        let stage = json!({"target":stream,"parent_generation":1,"admission_id":"stage","transition_id":"transition","plan":source_plan,"before":output,"after":output,"callback_proof":stage_proof});
        let frozen = json!({"target":stream,"parent_generation":1,"transition_id":"transition","input":stored,"stage":stage,"policy":policy,"finalize_approval":approval("finalize"),"finalize_status_approval":approval("status"),"schema_documents":[{"reference":schema,"document":document}]});
        let plan_request = json!({"version":2,"provider_sequence":3,"operation":"resource_finalize_plan","transition_id":"transition","operation_id":"plan-op","target":stream,"phase":"stream.prepare","settings_revision":"one","settings":{},"intent":"initial","reason":null,"requested_video":null,"consumer_requirements":requirements,"remaining_ms":5000,"plan_revision":null,"admission_id":null,"approved_plan":null,"dependency_inputs":[output],"control_results":[]});
        let request: ResourceFinalizeRequest = serde_json::from_value(json!({"version":2,"transition_id":"transition","operation_id":"effect","provider_sequence":4,"target":stream,"output":output,"control_results":[],"admission_id":"fresh","permit_id":"permit","remaining_ms":5000})).unwrap();
        let context = json!({"policy":policy,"output":output,"control_results":[],"target":stream,"transition_id":"transition","parent_generation":1});
        let effect_call = call(
            "finalize",
            serde_json::to_value(&request).unwrap(),
            "effect",
        );
        let status_call = call("status", json!({}), "status-op");
        let dispatch = json!({"request":request,"context":context,"effect_call":effect_call,"status_selection":status_call,"phase":{"effect":{}},"status_polls":0,"dispatch":null});
        // Serialize the enum using its actual wire tag instead of assuming a unit representation.
        let mut dispatch = dispatch;
        dispatch["phase"] = json!("effect");
        let owner_plan = json!({"provider":"owner","participant":participant,"request":plan_request,"phase":"finalize_plan_admitted","plan":final_plan,"admission_id":"fresh","pending_receipt":null,"status_polls":0,"last_sequence":3,"dispatch":null,"terminal_dispatch":null,"response":null});
        let progress = json!({"inputs":[stored],"selections":[],"controls":[],"finalize":{"selection":frozen,"owner_plan":owner_plan,"dispatch":dispatch}});
        let work = json!({"composition_progress":progress,"phase":"stream.prepare","target":stream,"intent":"initial","reason":null,"video":null,"transition_id":"transition","providers":[],"index":0,"states":{},"candidate":null,"waiting_retirement":false});
        let session_record = json!({"host_epoch":epoch,"id":"session","generation":1,"phase":"ready","owner":{"caller":"transport","initiator":"transport"},"delegates":[],"application_ref":{},"settings_revision":"one","application_retired":false,"streams":{"stream":{"generation":1,"phase":"preparing","candidate":null,"required":[],"acknowledgements":[],"retirements":[]}},"admission_ids":["source","stage","fresh"],"admission_targets":{"source":session,"stage":stream,"fresh":stream},"admission_phases":{"source":"session.prepare","stage":"stream.prepare","fresh":"stream.prepare"}});
        let admission = |plan: Value| json!({"provider":"owner","selection":selection,"plan":plan,"rights":[["domain","Exclusive"]]});
        let mut journal = coordinator.shared.borrow().value.clone();
        journal.sessions.insert(
            "session".into(),
            serde_json::from_value(session_record).unwrap(),
        );
        journal.receipts.insert("work".into(), serde_json::from_value(json!({"caller":"transport","initiator":"transport","request":{},"response":{"status":"pending"},"work":work})).unwrap());
        journal.composition = serde_json::from_value(json!({"outputs":[stored],"selections":[],"finalize_selections":[frozen],"staged_outputs":[stage],"refinements":[]})).unwrap();
        journal
            .admission_transitions
            .insert("source".into(), "source-transition".into());
        journal
            .admission_transitions
            .insert("fresh".into(), "transition".into());
        journal
            .admission_transitions
            .insert("stage".into(), "transition".into());
        journal.authority = serde_json::from_value(json!({"sequences":{"owner":4},"snapshot":{"resources":{"owner":{"output":{"provider":"owner","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domains":["domain"]}}},"domains":{"domain":{"revision":1,"borrowers":{}}}},"admissions":{"fresh":admission(final_plan.clone())},"domain_locks":{"domain":"fresh"},"domain_owners":{"domain":"owner"},"refresh_required":[],"settled":{"source":admission(source_plan.clone()),"stage":admission(source_plan)},"finalize_permits":{"permit":{"context":context,"request":request,"consumed":false,"settled":false}}})).unwrap();
        coordinator
            .settings
            .finalize_policies
            .push(serde_json::from_value(policy).unwrap());
        coordinator
            .settings
            .schema_documents
            .push(serde_json::from_value(json!({"reference":schema,"document":document})).unwrap());
        coordinator
            .selections
            .insert("owner".into(), serde_json::from_value(selection).unwrap());
        save(&coordinator.shared, journal).unwrap();
        let envelope = json!({"operation":"verify_resource_finalize","request":request});
        let described = coordinator.finalize_operation(
            "owner",
            json!({"operation":"describe_resource_finalize","request":request}),
        );
        assert_eq!(described["status"], "described", "{described}");
        assert_eq!(
            coordinator.finalize_operation("other", envelope.clone())["status"],
            "rejected"
        );
        let mut forged = envelope.clone();
        forged["request"]["output"]["resource"]["revision"] = json!(2);
        assert_eq!(
            coordinator.finalize_operation("owner", forged)["status"],
            "rejected"
        );
        assert_eq!(
            coordinator.finalize_operation("owner", envelope.clone())["status"],
            "execute"
        );
        assert!(coordinator.shared.borrow().value.authority.finalize_permits["permit"].consumed);
        let persisted: Value =
            serde_json::from_slice(&std::fs::read(path.join("journal.json")).unwrap()).unwrap();
        assert_eq!(
            persisted["authority"]["finalize_permits"]["permit"]["consumed"],
            true
        );
        assert_eq!(
            coordinator.finalize_operation("owner", envelope)["status"],
            "reattach_only"
        );
        // Preserve an exact completed result after the original work disappears.
        let candidate = json!({"capture_provider":"capture","selector_schema":"selector","selector":{},"resource":resource,"mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":2,"generation":1});
        let applied = json!({"status":"applied","output":output,"candidate":candidate});
        let record = json!({"selection":frozen,"plan_request":plan_request,"plan":final_plan,"plan_approval":approval("plan"),"plan_terminal":{"call":call("plan",serde_json::to_value(serde_json::from_value::<amaterasu_extension_api::ProviderRequest>(plan_request.clone()).unwrap()).unwrap(),"plan-op"),"state":{"state":"completed","payload":{"status":"planned","plan":final_plan}}},"request":request,"applied":applied,"terminal":{"call":effect_call,"state":{"state":"completed","payload":applied}},"pending_receipt":null});
        let record: super::super::finalize_result::FinalizeResultRecord =
            serde_json::from_value(record).unwrap();
        assert!(
            amaterasu_extension_api::validate_provider_response(
                &record.plan_request,
                &amaterasu_extension_api::ProviderResponse::Planned {
                    plan: record.plan.clone()
                }
            )
            .is_ok(),
            "plan: {:?}",
            amaterasu_extension_api::validate_provider_response(
                &record.plan_request,
                &amaterasu_extension_api::ProviderResponse::Planned {
                    plan: record.plan.clone()
                }
            )
        );
        assert!(
            amaterasu_extension_api::validate_resource_finalize_response(
                &record.request,
                &record.applied
            )
            .is_ok(),
            "final response: {:?}",
            amaterasu_extension_api::validate_resource_finalize_response(
                &record.request,
                &record.applied
            )
        );
        assert_eq!(
            record.plan_terminal.as_ref().unwrap().call.arguments,
            serde_json::to_value(&record.plan_request).unwrap()
        );
        assert_eq!(
            record.terminal.call.arguments,
            serde_json::to_value(&record.request).unwrap()
        );
        assert!(record.validate_structure());
        let mut journal = coordinator.shared.borrow().value.clone();
        journal.finalize_results.record(record).unwrap();
        journal
            .authority
            .finalize_permits
            .get_mut("permit")
            .unwrap()
            .settled = true;
        journal.authority.output_admissions.insert("fresh".into());
        journal.receipts.clear();
        save(&coordinator.shared, journal).unwrap();
        let proof =
            json!({"operation":"verify_finalize_result","request":request,"response":applied});
        let result = coordinator.verify_finalize_result("owner", proof.clone());
        assert_eq!(result["status"], "valid", "{result}");
        assert_eq!(result["result_id"], "permit");
        assert_eq!(
            coordinator.verify_finalize_result("other", proof.clone())["status"],
            "rejected"
        );
        let mut forged = proof;
        forged["response"]["candidate"]["geometry_revision"] = json!(3);
        assert_eq!(
            coordinator.verify_finalize_result("owner", forged)["status"],
            "rejected"
        );
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }
}
