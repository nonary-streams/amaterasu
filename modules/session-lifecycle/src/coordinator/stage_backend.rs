//! Read-only authentication of the exact persisted stream-stage callback.
use super::*;
use amaterasu_extension_api::{ProviderRequest, ResourceFinalizeRequest};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StageEnvelope {
    operation: StageOperation,
    stage_request: ProviderRequest,
    finalize_request: ResourceFinalizeRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum StageOperation {
    VerifyFinalizeStage,
}

impl Coordinator {
    /// Reconstruct authority from the durable finalize selection and staged
    /// callback. This proves provenance only and never consumes an effect permit.
    pub(super) fn verify_finalize_stage_operation(
        &mut self,
        caller: &str,
        payload: &Value,
    ) -> Value {
        match self.verify_finalize_stage(caller, payload.clone()) {
            Ok((target, session_target, output)) => json!({
                "status": "valid", "target": target,
                "session_target": session_target, "output": output
            }),
            Err(_) => json!({"status":"rejected", "reason":"finalize_stage_not_authorized"}),
        }
    }

    fn verify_finalize_stage(
        &mut self,
        caller: &str,
        payload: Value,
    ) -> ModuleResult<(
        LifecycleTarget,
        LifecycleTarget,
        amaterasu_extension_api::ResourceOutput,
    )> {
        let envelope: StageEnvelope =
            serde_json::from_value(payload).map_err(|_| "invalid_stage_envelope")?;
        let _ = envelope.operation;
        let stage_request = &envelope.stage_request;
        let finalize = &envelope.finalize_request;
        amaterasu_extension_api::validate_provider_request(stage_request)?;
        amaterasu_extension_api::validate_resource_finalize_request(finalize)?;
        // Reuse the current finalize resolver and preflight permit check. It
        // verifies the exact work, owner, selection, rights and live generation.
        let described = self.finalize_operation(
            caller,
            json!({"operation":"describe_resource_finalize", "request":finalize}),
        );
        if described["status"] != "described" {
            return Err("finalize_preflight_failed".into());
        }
        let session_target: LifecycleTarget =
            serde_json::from_value(described["session_target"].clone())
                .map_err(|_| "invalid_session_target")?;
        let shared = self.shared.borrow();
        let journal = &shared.value;
        if !journal.composition.validate_structure() {
            return Err("invalid_composition_journal".into());
        }
        let selection = journal
            .composition
            .finalize_selection(
                &finalize.target,
                &finalize.transition_id,
                caller,
                &finalize.output.output_id,
            )
            .ok_or("finalize_selection_missing")?;
        let stage = journal
            .composition
            .staged_for(&finalize.target, caller, &finalize.output.output_id)
            .map_err(str::to_owned)?;
        let admission = journal
            .authority
            .admissions
            .get(&stage.admission_id)
            .or_else(|| journal.authority.settled.get(&stage.admission_id))
            .ok_or("stage_admission_missing")?;
        let (epoch, session_id, _, _) = super::operations::target_parts(&stage.target)?;
        let session = journal.sessions.get(session_id).ok_or("session_missing")?;
        if selection.stage.as_ref() != Some(stage)
            || stage.callback_proof.request != *stage_request
            || stage.target != finalize.target
            || stage.transition_id != finalize.transition_id
            || stage.before.origin != session_target
            || epoch != journal.host_epoch
            || stage.parent_generation != session.generation
            || admission.provider != caller
            || admission.plan != stage.plan
            || admission.selection.participant != stage.callback_proof.participant
            || !super::composition::callback_approvals_valid(
                &stage.callback_proof,
                &admission.selection.approvals,
                &admission.provider,
            )
            || !session.admission_ids.contains(&stage.admission_id)
            || session.admission_targets.get(&stage.admission_id) != Some(&stage.target)
            || session.admission_phases.get(&stage.admission_id)
                != Some(&LifecyclePhase::StreamPrepare)
            || journal.admission_transitions.get(&stage.admission_id) != Some(&stage.transition_id)
            || described["target"]
                != serde_json::to_value(&finalize.target).map_err(|error| error.to_string())?
            || described["output"]
                != serde_json::to_value(&finalize.output).map_err(|error| error.to_string())?
        {
            return Err("stage_provenance_mismatch".into());
        }
        Ok((
            finalize.target.clone(),
            session_target,
            finalize.output.clone(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_or_fabricated_stage_never_confers_authority() {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-stage-backend-{}-{}",
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
        for payload in [
            json!({"operation":"verify_finalize_stage"}),
            json!({"operation":"verify_finalize_stage","stage_request":{},"finalize_request":{},"authority":true}),
        ] {
            let result = coordinator.verify_finalize_stage_operation("example.owner", &payload);
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
        }
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }
}
