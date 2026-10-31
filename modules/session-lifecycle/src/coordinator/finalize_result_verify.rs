//! Read-only proof of one durable owner finalization. This grants no mutation.
use super::*;
use amaterasu_extension_api::{ResourceControlResponse, ResourceFinalizeRequest};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
    request: ResourceFinalizeRequest,
    response: ResourceControlResponse,
}

impl Coordinator {
    pub(super) fn verify_finalize_result(&self, caller: &str, payload: Value) -> Value {
        self.checked_finalize_result(caller, payload)
            .unwrap_or_else(
                |_| json!({"status":"rejected","reason":"finalize_result_not_authorized"}),
            )
    }
    fn checked_finalize_result(&self, caller: &str, payload: Value) -> ModuleResult<Value> {
        let query: Request =
            serde_json::from_value(payload).map_err(|_| "invalid_finalize_result_query")?;
        if query.operation != "verify_finalize_result" || caller.is_empty() {
            return Err("invalid_finalize_result_query".into());
        }
        let shared = self.shared.borrow();
        let journal = &shared.value;
        if !journal.finalize_results.validate_structure() {
            return Err("invalid_finalize_result_book".into());
        }
        let mut matches = journal
            .finalize_results
            .records()
            .filter(|record| record.request == query.request && record.applied == query.response);
        let record = matches.next().ok_or("finalize_result_unavailable")?;
        if matches.next().is_some()
            || !record.validate_structure()
            || record.selection.policy.owner_provider != caller
        {
            return Err("finalize_result_owner_mismatch".into());
        }
        let request = &record.request;
        let selection = &record.selection;
        let permit = journal
            .authority
            .finalize_permits
            .get(&request.permit_id)
            .ok_or("finalize_permit_unavailable")?;
        let admission = journal
            .authority
            .admissions
            .get(&request.admission_id)
            .or_else(|| journal.authority.settled.get(&request.admission_id))
            .ok_or("finalize_admission_unavailable")?;
        if !permit.consumed
            || !permit.settled
            || permit.request != *request
            || permit.context.policy != selection.policy
            || permit.context.target != request.target
            || permit.context.transition_id != request.transition_id
            || permit.context.output != request.output
            || permit.context.control_results != request.control_results
            || permit.context.parent_generation != selection.parent_generation
            || admission.provider != caller
            || admission.plan != record.plan
            || !journal
                .authority
                .output_admissions
                .contains(&request.admission_id)
            || journal.admission_transitions.get(&request.admission_id)
                != Some(&request.transition_id)
            || record.plan_approval.as_ref().is_none_or(|approval| {
                !admission.selection.approvals.contains(approval)
                    || approval.method != admission.selection.participant.plan_method
            })
        {
            return Err("finalize_result_authority_mismatch".into());
        }
        if journal.composition.finalize_selection(
            &selection.target,
            &selection.transition_id,
            caller,
            &selection.input.output.output_id,
        ) != Some(selection)
            || journal.composition.get(
                caller,
                &selection.input.output.output_id,
                &selection.input.output.origin,
            ) != Some(&selection.input)
            || !self.settings.finalize_policies.contains(&selection.policy)
            || selection
                .schema_documents
                .iter()
                .any(|doc| !self.settings.schema_documents.contains(doc))
        {
            return Err("finalize_result_selection_mismatch".into());
        }
        let source = journal
            .authority
            .admissions
            .get(&selection.input.admission_id)
            .or_else(|| journal.authority.settled.get(&selection.input.admission_id))
            .ok_or("finalize_source_unavailable")?;
        if source.provider != caller
            || source.plan != selection.input.plan
            || journal
                .admission_transitions
                .get(&selection.input.admission_id)
                != Some(&selection.input.transition_id)
        {
            return Err("finalize_source_mismatch".into());
        }
        let (epoch, session_id, stream_id, generation) =
            super::operations::target_parts(&request.target)?;
        let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
        if epoch != journal.host_epoch
            || epoch != session.host_epoch
            || stream_id
                .and_then(|id| session.streams.get(id))
                .is_none_or(|stream| stream.generation != generation)
            || selection.parent_generation != Some(session.generation)
            || !session.admission_ids.contains(&request.admission_id)
            || session.admission_targets.get(&request.admission_id) != Some(&request.target)
            || session.admission_phases.get(&request.admission_id)
                != Some(&record.plan_request.phase)
        {
            return Err("finalize_result_target_stale".into());
        }
        let ResourceControlResponse::Applied { output, .. } = &record.applied else {
            return Err("finalize_result_not_applied".into());
        };
        self.settings
            .schema_catalog()?
            .validate(&output.schema, &output.data)?;
        let resource_generation =
            super::resource_generation::current_root_generation(journal, &selection.input)?;
        if record.plan_request.resource_generation != Some(resource_generation)
            || selection.stage.as_ref().is_some_and(|stage| {
                stage.callback_proof.request.resource_generation != Some(resource_generation)
            })
        {
            return Err("finalize_result_generation_mismatch".into());
        }
        Ok(
            json!({"status":"valid","target":request.target,"result_id":request.permit_id,"resource_generation":resource_generation}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fabricated_result_and_wrong_identity_never_authorize() {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-finalize-result-{}-{}",
            std::process::id(),
            identifier().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let coordinator = Coordinator::open(&path.join("journal.json")).unwrap();
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        let output = json!({"output_id":"out","capability":"display","schema":{"id":"schema","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"owner","producer_service":"owner/2","origin":target,"resource":{"provider":"owner","resource_id":"out","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"domain_observations":[],"data":{}});
        let request = json!({"version":2,"transition_id":"transition","operation_id":"finalize","provider_sequence":1,"target":target,"output":output,"control_results":[],"admission_id":"invented","permit_id":"invented","remaining_ms":1000});
        let envelope = json!({"operation":"verify_finalize_result","request":request,"response":{"status":"applied","output":output,"candidate":null}});
        assert_eq!(
            coordinator.verify_finalize_result("owner", envelope.clone())["status"],
            "rejected"
        );
        assert_eq!(
            coordinator.verify_finalize_result("other", envelope)["status"],
            "rejected"
        );
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }
}
