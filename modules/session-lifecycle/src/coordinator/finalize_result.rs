//! Durable owner finalization evidence. Live authorization is checked separately
//! against the same journal snapshot before this record is admitted.
use super::composition::approval_matches_call;
use super::finalize_selection::FinalizeSelectionSnapshot;
use crate::provider_dispatch::{DispatchRecord, DispatchState};
use amaterasu_extension_api::{
    self as api, ProviderOperation, ProviderRequest, ProviderResponse, ResourceControlResponse,
    ResourceFinalizeRequest, ResourcePlan,
};
use serde::{Deserialize, Serialize};

const MAX_RESULTS: usize = 128;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FinalizeResultRecord {
    pub selection: FinalizeSelectionSnapshot,
    pub plan_request: ProviderRequest,
    pub plan: ResourcePlan,
    pub request: ResourceFinalizeRequest,
    pub applied: ResourceControlResponse,
    pub terminal: DispatchRecord,
    pub pending_receipt: Option<String>,
}

impl FinalizeResultRecord {
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
            && self.plan_request.operation == ProviderOperation::ResourceFinalizePlan
            && self.plan_request.target == selection.target
            && self.plan_request.transition_id == selection.transition_id
            && self.plan_request.dependency_inputs.as_slice() == [request.output.clone()]
            && self.plan_request.control_results == request.control_results
            && api::validate_provider_response(&self.plan_request, &planned).is_ok()
            && request.target == selection.target
            && request.transition_id == selection.transition_id
            && request.output == *selection.current_output()
            && api::validate_resource_finalize_response(request, &self.applied).is_ok()
            && serde_json::from_value::<ResourceControlResponse>(payload.clone())
                .ok()
                .as_ref()
                == Some(&self.applied)
            && effect_match
            && self.pending_receipt.is_none()
            && request.control_results.is_empty()
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
                .is_none_or(|mode| mode == &candidate.mode)
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
/// never publishes it. Only standalone finalization is admitted at present.
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
    if !dispatch.request.control_results.is_empty()
        || serde_json::to_value(progress.dispatch.as_ref()).ok()
            != serde_json::to_value(Some(dispatch)).ok()
        || !journal.receipts.values().any(|receipt| {
            receipt.work.as_ref().is_some_and(|saved| {
                serde_json::to_value(saved).ok() == serde_json::to_value(work).ok()
            })
        })
    {
        return Err("finalize_context_mismatch".into());
    }
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
    let record = FinalizeResultRecord {
        selection: selected.clone(),
        plan_request: progress.owner_plan.request.clone(),
        plan,
        request: dispatch.request.clone(),
        applied: (**result).clone(),
        terminal: dispatch
            .dispatch
            .clone()
            .ok_or("finalize_terminal_proof_missing")?,
        pending_receipt: None,
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
        FinalizeResultRecord {
            selection,
            plan_request: serde_json::from_value(plan_request).unwrap(),
            plan: serde_json::from_value(plan).unwrap(),
            request: serde_json::from_value(request).unwrap(),
            applied: serde_json::from_value(applied).unwrap(),
            terminal: serde_json::from_value(terminal).unwrap(),
            pending_receipt: None,
        }
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
        let mut changed = record;
        changed.terminal.call.approval["provider"] = json!("other");
        assert!(!changed.validate_structure());
    }
}
