//! Durable owner finalization evidence. Live authorization is checked separately
//! against the same journal snapshot before this record is admitted.
use super::composition::approval_matches_call;
use super::finalize_selection::FinalizeSelectionSnapshot;
use crate::provider_dispatch::{DispatchRecord, DispatchState};
use amaterasu_extension_api::{
    self as api, ProviderOperation, ProviderRequest, ProviderResponse, ProviderStatusRequest,
    ResourceControlResponse, ResourceFinalizeRequest, ResourcePlan,
};
use serde::{Deserialize, Serialize};

const MAX_RESULTS: usize = 128;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FinalizeResultRecord {
    pub selection: FinalizeSelectionSnapshot,
    /// Frozen completed participant state needed to authenticate the control chain after restart.
    #[serde(default)]
    pub chain_work: Option<Box<super::Work>>,
    pub plan_request: ProviderRequest,
    pub plan: ResourcePlan,
    #[serde(default)]
    pub plan_approval: Option<crate::prepare_authority::MethodApproval>,
    #[serde(default)]
    pub plan_terminal: Option<DispatchRecord>,
    pub request: ResourceFinalizeRequest,
    pub applied: ResourceControlResponse,
    pub terminal: DispatchRecord,
    pub pending_receipt: Option<String>,
    #[serde(default)]
    pub pending_proof: Option<DispatchRecord>,
}

impl FinalizeResultRecord {
    pub fn validate_control_chain(&self, journal: &super::Journal) -> bool {
        let Some(work) = &self.chain_work else {
            if !self.request.control_results.is_empty() {
                return false;
            }
            return match super::operations::target_parts(&self.request.target) {
                Ok((_, session_id, Some(stream_id), _)) => journal
                    .sessions
                    .get(session_id)
                    .and_then(|session| session.streams.get(stream_id))
                    .and_then(|stream| stream.provider_graph.as_ref())
                    .is_some_and(|graph| graph.order.len() == 1),
                Ok((_, _, None, _)) => true,
                Err(_) => false,
            };
        };
        let Ok((_, session_id, Some(stream_id), generation)) =
            super::operations::target_parts(&self.request.target)
        else {
            return false;
        };
        let Some(stream) = journal
            .sessions
            .get(session_id)
            .and_then(|session| session.streams.get(stream_id))
        else {
            return false;
        };
        stream.generation == generation
            && serde_json::to_value(&stream.provider_graph).ok()
                == serde_json::to_value(&work.provider_graph).ok()
            && super::composition_finalize::resolve_historical_control_chain(
                journal,
                work,
                &self.selection,
            )
            .is_ok_and(|(output, controls)| {
                output == self.request.output && controls == self.request.control_results
            })
    }

    pub fn validate_structure(&self) -> bool {
        let selection = &self.selection;
        let request = &self.request;
        let ResourceControlResponse::Applied {
            output,
            candidate: Some(candidate),
        } = &self.applied
        else {
            return false;
        };
        let Some(plan_approval) = &self.plan_approval else {
            return false;
        };
        let Some(plan_terminal) = &self.plan_terminal else {
            return false;
        };
        let DispatchState::Completed {
            payload: plan_payload,
        } = &plan_terminal.state
        else {
            return false;
        };
        let plan_matches = approval_matches_call(plan_approval, &plan_terminal.call)
            && plan_terminal.call.approval == plan_approval.broker_approval
            && plan_terminal.call.approval["provider"].as_str()
                == Some(selection.policy.owner_provider.as_str())
            && plan_terminal.call.operation_id == self.plan_request.operation_id
            && serde_json::to_value(&self.plan_request).ok().as_ref()
                == Some(&plan_terminal.call.arguments)
            && matches!(serde_json::from_value::<ProviderResponse>(plan_payload.clone()), Ok(ProviderResponse::Planned { plan }) if plan == self.plan);
        let Some(constraint) = &self.plan.candidate_constraint else {
            return false;
        };
        let Some(geometry) = constraint.geometry_revision.filter(|value| *value > 0) else {
            return false;
        };
        let DispatchState::Completed { payload } = &self.terminal.state else {
            return false;
        };
        let call = &self.terminal.call;
        let effect = &selection.finalize_approval;
        let effect_match = approval_matches_call(effect, call)
            && call.approval == effect.broker_approval
            && call.operation_id == request.operation_id
            && serde_json::to_value(request).ok().as_ref() == Some(&call.arguments);
        let status_match = self.pending_receipt.as_ref().is_some_and(|receipt| {
            let status = &selection.finalize_status_approval;
            approval_matches_call(status, call)
                && call.approval == status.broker_approval
                && call.operation_id != request.operation_id
                && serde_json::from_value::<ProviderStatusRequest>(call.arguments.clone()).is_ok_and(|query| {
                    query.provider_sequence == request.provider_sequence
                        && query.operation_id == request.operation_id
                        && query.transition_id == request.transition_id
                        && query.receipt_id == *receipt
                })
                && self.pending_proof.as_ref().is_some_and(|proof| {
                    approval_matches_call(effect, &proof.call)
                        && proof.call.approval == effect.broker_approval
                        && proof.call.operation_id == request.operation_id
                        && serde_json::to_value(request).ok().as_ref() == Some(&proof.call.arguments)
                        && matches!(&proof.state, DispatchState::Completed { payload } if matches!(serde_json::from_value::<ResourceControlResponse>(payload.clone()), Ok(ResourceControlResponse::Pending { receipt_id }) if receipt_id == *receipt))
                })
        });
        let planned = ProviderResponse::Planned {
            plan: self.plan.clone(),
        };
        let schema = api::schemas::SchemaCatalog::compile(
            selection
                .schema_documents
                .iter()
                .map(|doc| (doc.reference.clone(), doc.document.as_str())),
        );
        selection.validate_structure()
            && plan_matches
            && self.plan_request.operation == ProviderOperation::ResourceFinalizePlan
            && self.plan_request.target == selection.target
            && self.plan_request.transition_id == selection.transition_id
            && self.plan_request.dependency_inputs.as_slice() == [request.output.clone()]
            && self.plan_request.control_results == request.control_results
            && api::validate_provider_response(&self.plan_request, &planned).is_ok()
            && request.target == selection.target
            && request.transition_id == selection.transition_id
            && if request.control_results.is_empty() {
                request.output == *selection.current_output()
            } else {
                self.chain_work.is_some()
            }
            && api::validate_resource_finalize_response(request, &self.applied).is_ok()
            && serde_json::from_value::<ResourceControlResponse>(payload.clone())
                .ok()
                .as_ref()
                == Some(&self.applied)
            && ((effect_match && self.pending_receipt.is_none() && self.pending_proof.is_none())
                || status_match)
            && candidate.geometry_revision == geometry
            && candidate.plan_revision == constraint.plan_revision
            && candidate.capture_provider == constraint.capture_provider
            && candidate.selector_schema == constraint.selector_schema
            && candidate.resource.provider == constraint.resource_provider
            && candidate.resource.resource_id == constraint.resource_id
            && candidate.resource.execution_domain == constraint.execution_domain
            && constraint
                .incarnation
                .as_ref()
                .is_none_or(|id| id == &candidate.resource.incarnation)
            && constraint
                .mode
                .as_ref()
                .is_some_and(|mode| mode == &candidate.mode)
            && schema.is_ok_and(|catalog| catalog.validate(&output.schema, &output.data).is_ok())
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FinalizeResultBook {
    records: Vec<FinalizeResultRecord>,
}
impl FinalizeResultBook {
    pub fn records(&self) -> impl Iterator<Item = &FinalizeResultRecord> {
        self.records.iter()
    }
    pub fn validate_structure(&self) -> bool {
        self.records.len() <= MAX_RESULTS
            && self.records.iter().enumerate().all(|(index, record)| {
                record.validate_structure()
                    && !self.records[..index].iter().any(|old| {
                        old.request.permit_id == record.request.permit_id
                            || old.request.operation_id == record.request.operation_id
                    })
            })
    }
    pub fn record(&mut self, record: FinalizeResultRecord) -> Result<(), &'static str> {
        if !record.validate_structure() {
            return Err("invalid_finalize_result");
        }
        if let Some(old) = self
            .records
            .iter()
            .find(|old| old.request.permit_id == record.request.permit_id)
        {
            return if serde_json::to_value(old).ok() == serde_json::to_value(&record).ok() {
                Ok(())
            } else {
                Err("finalize_result_conflict")
            };
        }
        if self.records.len() >= MAX_RESULTS
            || self
                .records
                .iter()
                .any(|old| old.request.operation_id == record.request.operation_id)
        {
            return Err("finalize_result_capacity_or_conflict");
        }
        self.records.push(record);
        Ok(())
    }
}

/// Called on a private clone of the locked journal. The caller atomically saves
/// that clone together with settled authority and the candidate; this function
/// never publishes it. Control lineage is reconstructed from durable owner acknowledgements.
pub(super) fn record_finalize_result(
    journal: &mut super::Journal,
    settings: &crate::adapter::LifecycleSettings,
    selections: &std::collections::BTreeMap<String, crate::prepare_authority::ProviderSelection>,
    work: &super::Work,
    dispatch: &super::finalize_dispatch::FinalizeDispatchState,
) -> amaterasu_module_sdk::ModuleResult<api::CaptureBinding> {
    use crate::prepare_authority::{AuthorityJournal, AuthorityState, CoordinatorPrepareAuthority};
    struct ReadOnly;
    impl AuthorityJournal for ReadOnly {
        fn persist(&mut self, _: &AuthorityState) -> amaterasu_module_sdk::ModuleResult<()> {
            Err("unexpected_authority_write".into())
        }
    }
    let progress = work
        .composition_progress
        .as_ref()
        .and_then(|progress| progress.finalize.as_ref())
        .ok_or("finalize_progress_missing")?;
    let selected = &progress.selection;
    if serde_json::to_value(progress.dispatch.as_ref()).ok()
        != serde_json::to_value(Some(dispatch)).ok()
        || !journal.receipts.values().any(|receipt| {
            receipt.work.as_ref().is_some_and(|saved| {
                serde_json::to_value(saved).ok() == serde_json::to_value(work).ok()
            })
        })
    {
        return Err("finalize_context_mismatch".into());
    }
    let composed_graph = work
        .provider_graph
        .as_ref()
        .is_some_and(|graph| graph.order.len() > 1);
    let chain_work = if dispatch.request.control_results.is_empty() && !composed_graph {
        if dispatch.request.output != *selected.current_output() {
            return Err("finalize_output_mismatch".into());
        }
        None
    } else {
        let (output, controls) =
            super::composition_finalize::resolve_control_chain(journal, work, selected)?;
        if output != dispatch.request.output || controls != dispatch.request.control_results {
            return Err("finalize_control_chain_mismatch".into());
        }
        Some(Box::new(work.clone()))
    };
    let installed = selections
        .get(&selected.policy.owner_provider)
        .ok_or("finalize_owner_not_selected")?;
    if installed.participant != progress.owner_plan.participant
        || !settings.finalize_policies.contains(&selected.policy)
        || !installed.approvals.contains(&selected.finalize_approval)
        || !installed
            .approvals
            .contains(&selected.finalize_status_approval)
        || settings
            .schema_catalog()?
            .validate(
                &dispatch.request.output.schema,
                &dispatch.request.output.data,
            )
            .is_err()
        || progress.owner_plan.request.phase != work.phase
        || journal.composition.finalize_selection(
            &work.target,
            &work.transition_id,
            &selected.policy.owner_provider,
            &selected.input.output.output_id,
        ) != Some(selected)
    {
        return Err("finalize_selection_mismatch".into());
    }
    let mut authority = CoordinatorPrepareAuthority::new(
        journal.authority.clone(),
        selections.values().cloned().collect(),
        ReadOnly,
    )?;
    authority.validate_finalize_permit(
        &selected.policy.owner_provider,
        &installed.participant,
        &dispatch.context,
        &dispatch.request,
    )?;
    let plan = progress
        .owner_plan
        .plan
        .clone()
        .ok_or("finalize_plan_missing")?;
    if progress.owner_plan.admission_id.as_deref() != Some(dispatch.request.admission_id.as_str())
        || journal
            .authority
            .admissions
            .get(&dispatch.request.admission_id)
            .is_none_or(|admission| admission.plan != plan)
    {
        return Err("finalize_admission_mismatch".into());
    }
    let super::finalize_dispatch::FinalizePhase::Confirmed { result } = &dispatch.phase else {
        return Err("finalize_not_confirmed".into());
    };
    let plan_terminal = progress
        .owner_plan
        .terminal_dispatch
        .clone()
        .ok_or("finalize_plan_proof_missing")?;
    let plan_approval = installed
        .approvals
        .iter()
        .find(|approval| {
            approval.method == installed.participant.plan_method
                && approval.broker_approval == plan_terminal.call.approval
                && approval_matches_call(approval, &plan_terminal.call)
        })
        .cloned()
        .ok_or("finalize_plan_approval_missing")?;
    let pending_receipt = dispatch
        .pending_proof
        .as_ref()
        .and_then(|proof| match &proof.state {
            DispatchState::Completed { payload } => {
                match serde_json::from_value::<ResourceControlResponse>(payload.clone()) {
                    Ok(ResourceControlResponse::Pending { receipt_id }) => Some(receipt_id),
                    _ => None,
                }
            }
            _ => None,
        });
    let record = FinalizeResultRecord {
        selection: selected.clone(),
        chain_work,
        plan_request: progress.owner_plan.request.clone(),
        plan,
        plan_approval: Some(plan_approval),
        plan_terminal: Some(plan_terminal),
        request: dispatch.request.clone(),
        applied: (**result).clone(),
        terminal: dispatch
            .dispatch
            .clone()
            .ok_or("finalize_terminal_proof_missing")?,
        pending_receipt,
        pending_proof: dispatch.pending_proof.clone(),
    };
    let ResourceControlResponse::Applied {
        candidate: Some(candidate),
        ..
    } = &record.applied
    else {
        return Err("finalize_candidate_missing".into());
    };
    let candidate = (**candidate).clone();
    journal
        .finalize_results
        .record(record)
        .map_err(str::to_owned)?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> FinalizeResultRecord {
        let mut selection = super::super::finalize_selection::tests::fixture();
        selection.input.output.origin = selection.target.clone();
        let output = &selection.input.output;
        let mode = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
        let plan = json!({"plan_id":"finalize-plan","revision":2,"resources":[{"provider":"owner","resource_id":"display","incarnation":"native","expected_revision":2,"execution_domain":"seat","mutation_domains":["topology"],"access":"exclusive"}],"mutation_domains":[{"domain":"topology","expected_revision":1,"borrowers":{},"access":"exclusive"}],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":{"capture_provider":"capture","selector_schema":"selector/1","resource_provider":"owner","resource_id":"display","incarnation":"native","execution_domain":"seat","mode":mode,"plan_revision":2,"geometry_revision":4},"allowed_degradation":[]});
        let plan_request = json!({"provider_sequence":1,"operation":"resource_finalize_plan","version":2,"transition_id":"finalize-transition","operation_id":"plan-operation","target":selection.target,"phase":"stream.prepare","settings_revision":"revision","settings":{},"intent":"initial","reason":null,"requested_video":mode,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"remaining_ms":1000,"plan_revision":null,"admission_id":null,"approved_plan":null,"dependency_inputs":[output],"control_results":[]});
        let request = json!({"version":2,"transition_id":"finalize-transition","operation_id":"finalize-operation","provider_sequence":2,"target":selection.target,"output":output,"control_results":[],"admission_id":"finalize-admission","permit_id":"permit","remaining_ms":1000});
        let candidate = json!({"capture_provider":"capture","selector_schema":"selector/1","selector":{},"resource":output.resource,"mode":mode,"plan_revision":2,"geometry_revision":4,"generation":1});
        let applied = json!({"status":"applied","output":output,"candidate":candidate});
        let terminal = json!({"call":{"service":"owner/2","method":"finalize","interface_major":2,"interface_minor":0,"approval":selection.finalize_approval.broker_approval,"arguments":request,"timeout_ms":1000,"operation_id":"finalize-operation"},"state":{"state":"completed","payload":applied}});
        let plan_approval = selection.finalize_approval.clone();
        let plan_request: ProviderRequest = serde_json::from_value(plan_request).unwrap();
        let plan_terminal = serde_json::from_value(json!({"call":{"service":"owner/2","method":"finalize","interface_major":2,"interface_minor":0,"approval":plan_approval.broker_approval,"arguments":plan_request,"timeout_ms":1000,"operation_id":"plan-operation"},"state":{"state":"completed","payload":{"status":"planned","plan":plan}}})).unwrap();
        FinalizeResultRecord {
            selection,
            chain_work: None,
            plan_request,
            plan: serde_json::from_value(plan).unwrap(),
            plan_approval: Some(plan_approval),
            plan_terminal: Some(plan_terminal),
            request: serde_json::from_value(request).unwrap(),
            applied: serde_json::from_value(applied).unwrap(),
            terminal: serde_json::from_value(terminal).unwrap(),
            pending_receipt: None,
            pending_proof: None,
        }
    }
    #[test]
    fn zero_control_graph_snapshot_roundtrips_but_needs_canonical_evidence() {
        let mut record = fixture();
        let work: super::super::Work = serde_json::from_value(json!({
            "phase":"stream.prepare", "target":record.request.target,
            "intent":null, "reason":null, "video":null,
            "transition_id":record.request.transition_id,
            "providers":["owner","dependent"],
            "provider_graph":{"order":["owner","dependent"],"dependencies":{"owner":[],"dependent":["owner"]}},
            "index":2, "states":{}, "candidate":null, "waiting_retirement":false
        })).unwrap();
        record.chain_work = Some(Box::new(work));
        assert!(record.validate_structure());
        let recovered: FinalizeResultRecord =
            serde_json::from_value(serde_json::to_value(record).unwrap()).unwrap();
        assert!(recovered.chain_work.is_some());
        let journal: super::super::Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap();
        assert!(!recovered.validate_control_chain(&journal));
    }

    #[test]
    fn control_result_requires_durable_chain_evidence() {
        let mut record = fixture();
        let control = api::ConfirmedControl {
            proposal_id: "proposal".into(),
            output: record.request.output.clone(),
            candidate: None,
        };
        record.request.control_results.push(control.clone());
        record.plan_request.control_results.push(control);
        record.terminal.call.arguments = serde_json::to_value(&record.request).unwrap();
        record.plan_terminal.as_mut().unwrap().call.arguments =
            serde_json::to_value(&record.plan_request).unwrap();
        assert!(!record.validate_structure());
    }

    #[test]
    fn pending_status_requires_exact_effect_receipt_and_selected_status() {
        let mut record = fixture();
        let mut pending = record.terminal.clone();
        pending.state = DispatchState::Completed {
            payload: json!({"status":"pending","receipt_id":"receipt"}),
        };
        let status = &record.selection.finalize_status_approval;
        record.terminal.call.service = status.method.service.clone();
        record.terminal.call.method = status.method.method.clone();
        record.terminal.call.interface_major = status.method.interface_major;
        record.terminal.call.interface_minor = status.method.interface_minor;
        record.terminal.call.approval = status.broker_approval.clone();
        record.terminal.call.operation_id = "status-poll".into();
        record.terminal.call.arguments = json!({"provider_sequence":record.request.provider_sequence,"operation_id":record.request.operation_id,"transition_id":record.request.transition_id,"receipt_id":"receipt"});
        record.pending_receipt = Some("receipt".into());
        record.pending_proof = Some(pending);
        assert!(record.validate_structure());
        let mut changed = record.clone();
        changed.pending_receipt = Some("foreign".into());
        assert!(!changed.validate_structure());
        let mut changed = record;
        changed.terminal.call.approval["provider"] = json!("foreign");
        assert!(!changed.validate_structure());
    }

    #[test]
    fn pins_geometry_and_completed_owner_response() {
        let record = fixture();
        assert!(record.selection.validate_structure());
        assert_eq!(
            api::validate_provider_response(
                &record.plan_request,
                &ProviderResponse::Planned {
                    plan: record.plan.clone()
                }
            ),
            Ok(())
        );
        assert_eq!(
            api::validate_resource_finalize_response(&record.request, &record.applied),
            Ok(())
        );
        assert_eq!(
            serde_json::to_value(&record.request).unwrap(),
            record.terminal.call.arguments
        );
        assert!(record.validate_structure());
        let mut book = FinalizeResultBook::default();
        book.record(record.clone()).unwrap();
        let restored: FinalizeResultBook =
            serde_json::from_value(serde_json::to_value(&book).unwrap()).unwrap();
        assert!(restored.validate_structure());
        let mut changed = record.clone();
        changed
            .plan
            .candidate_constraint
            .as_mut()
            .unwrap()
            .geometry_revision = Some(5);
        assert!(!changed.validate_structure());
        let mut changed = record.clone();
        changed.plan_terminal.as_mut().unwrap().call.operation_id = "foreign-plan".into();
        assert!(!changed.validate_structure());
        let mut changed = record;
        changed.terminal.call.approval["provider"] = json!("other");
        assert!(!changed.validate_structure());
    }
}
