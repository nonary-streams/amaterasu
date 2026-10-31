//! Owner-authenticated read-only lookup of an active configure settlement.
//!
//! This returns existing evidence only; the lookup never grants an effect or
//! substitutes for the caller's separate settlement verification.
use super::*;
use amaterasu_extension_api::{LifecycleTarget, ResourceOutput};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const MAX_RESPONSE_BYTES: usize = 16 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    operation: String,
    session_target: LifecycleTarget,
    resource_generation: u64,
    root_output: ResourceOutput,
    operation_id: String,
    request_digest: String,
}

impl Coordinator {
    pub(super) fn lookup_control_baseline(&self, caller: &str, payload: Value) -> Value {
        self.checked_control_baseline(caller, payload)
            .unwrap_or_else(
                |_| json!({"status":"rejected","reason":"control_baseline_unavailable"}),
            )
    }

    fn checked_control_baseline(&self, caller: &str, payload: Value) -> ModuleResult<Value> {
        let query: Lookup =
            serde_json::from_value(payload).map_err(|_| "invalid_baseline_lookup")?;
        if query.operation != "lookup_control_baseline"
            || caller.is_empty()
            || query.resource_generation == 0
            || query.root_output.producer != caller
            || query.root_output.origin != query.session_target
            || !matches!(query.session_target, LifecycleTarget::Session { .. })
            || query.operation_id.is_empty()
            || query.operation_id.len() > 256
            || query.request_digest.len() != 64
            || !query
                .request_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid_baseline_lookup".into());
        }
        let mut installed = self
            .settings
            .participants
            .iter()
            .filter(|participant| participant.provider == caller);
        let participant = installed.next().ok_or("baseline_owner_unavailable")?;
        let selected = self
            .selections
            .get(caller)
            .ok_or("baseline_owner_unselected")?;
        if installed.next().is_some() || &selected.participant != participant {
            return Err("baseline_owner_mismatch".into());
        }
        // Only one exact durable tuple may satisfy the requested identity. Do
        // not treat an unacknowledged native effect as an accepted baseline.
        let tuple = {
            let shared = self.shared.borrow();
            let journal = &shared.value;
            if !journal.control_settlements.validate_structure() {
                return Err("invalid_settlement_book".into());
            }
            let mut roots = journal
                .composition
                .records()
                .filter(|root| root.output == query.root_output);
            let root = roots.next().ok_or("baseline_root_unavailable")?;
            if roots.next().is_some()
                || super::resource_generation::current_root_generation(journal, root)?
                    != query.resource_generation
            {
                return Err("baseline_root_not_current".into());
            }
            let mut candidates = journal.control_settlements.records().filter(|record| {
                let request = &record.settlement.request;
                record.owner_ack.is_some()
                    && record.owner_dispatch.is_some()
                    && request.proposal.owner_provider == caller
                    && request.operation_id == query.operation_id
                    && request
                        .proposal
                        .arguments
                        .get("operation")
                        .and_then(Value::as_str)
                        == Some("configure")
                    && serde_json::to_vec(request).ok().is_some_and(|bytes| {
                        format!("{:x}", Sha256::digest(bytes)) == query.request_digest
                    })
            });
            let record = candidates.next().ok_or("baseline_unavailable")?;
            if candidates.next().is_some() {
                return Err("baseline_ambiguous".into());
            }
            let permit = journal
                .authority
                .control_permits
                .get(&record.settlement.request.permit_id)
                .ok_or("baseline_permit_missing")?;
            let selection = journal
                .composition
                .selection(
                    &record.settlement.request.target,
                    &record.selection_transition_id,
                    &permit.context.policy.dependent_provider,
                    caller,
                    &record.settlement.request.proposal.output_id,
                )
                .ok_or("baseline_selection_missing")?;
            if !selected.approvals.contains(&selection.control_approval)
                || !selected
                    .approvals
                    .contains(&selection.control_status_approval)
                || !selected.approvals.contains(&selection.settlement_approval)
            {
                return Err("baseline_owner_approval_changed".into());
            }
            record.settlement.clone()
        };
        // The historical verifier rechecks the selected installed owner,
        // signed policy, exact durable acknowledgement, session and provenance.
        let proof = self
            .verify_historical_settlement(caller, &tuple)?
            .ok_or("baseline_unavailable")?;
        if proof["status"] != "valid"
            || proof["session_target"]
                != serde_json::to_value(&query.session_target).map_err(|error| error.to_string())?
            || proof["resource_generation"] != json!(query.resource_generation)
            || proof["root_output"]
                != serde_json::to_value(&query.root_output).map_err(|error| error.to_string())?
            || proof["target"]
                != serde_json::to_value(&tuple.request.target).map_err(|error| error.to_string())?
            || proof["output"]
                != serde_json::to_value(&tuple.output).map_err(|error| error.to_string())?
        {
            return Err("baseline_identity_mismatch".into());
        }
        let result = json!({"status":"valid", "settlement":tuple,
            "session_target":query.session_target, "target":proof["target"],
            "root_output":query.root_output, "resource_generation":query.resource_generation});
        if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > MAX_RESPONSE_BYTES) {
            return Err("baseline_response_too_large".into());
        }
        Ok(result)
    }
}
