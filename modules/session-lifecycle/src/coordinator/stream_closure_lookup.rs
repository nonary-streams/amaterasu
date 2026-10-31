//! Read-only proof that one exact owner control belongs to a closed stream.
//! The result is historical evidence, never permission to perform an effect.
use super::*;
use amaterasu_extension_api::LifecycleTarget;
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    operation: String,
    target: LifecycleTarget,
    resource_generation: u64,
    resource_id: String,
    incarnation: String,
    operation_id: String,
    request_digest: String,
}

impl Coordinator {
    pub(super) fn lookup_stream_closure(&self, caller: &str, payload: Value) -> Value {
        self.checked_stream_closure(caller, payload)
            .unwrap_or_else(|_| json!({"status":"rejected","reason":"stream_closure_unavailable"}))
    }

    fn checked_stream_closure(&self, caller: &str, payload: Value) -> ModuleResult<Value> {
        let query: Query = serde_json::from_value(payload).map_err(|_| "invalid_closure_query")?;
        if query.operation != "lookup_stream_closure"
            || caller.is_empty()
            || !matches!(query.target, LifecycleTarget::Stream { .. })
            || query.resource_generation == 0
            || [&query.resource_id, &query.incarnation, &query.operation_id]
                .iter()
                .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
            || query.request_digest.len() != 64
            || !query
                .request_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid_closure_query".into());
        }
        let mut participants = self
            .settings
            .participants
            .iter()
            .filter(|p| p.provider == caller);
        let participant = participants.next().ok_or("owner_unavailable")?;
        let selected = self.selections.get(caller).ok_or("owner_unselected")?;
        if participants.next().is_some() || &selected.participant != participant {
            return Err("owner_mismatch".into());
        }
        let live_tuple = {
            let shared = self.shared.borrow();
            let journal = &shared.value;
            if !journal.validate_composition() || !journal.closed_streams.validate(journal) {
                return Err("invalid_closure_journal".into());
            }
            let checkpoint = journal.closed_streams.contains_target(&query.target);
            let live = journal.composed_releases.contains_target(&query.target);
            if checkpoint && live {
                return Err("ambiguous_closure_source".into());
            }
            if checkpoint {
                if super::checkpoint_queries::has_original_result(journal, &query.target) {
                    return Err("ambiguous_closure_source".into());
                }
                if !journal.closed_streams.verify_closed_control(
                    journal,
                    &self.settings,
                    caller,
                    &query.target,
                    query.resource_generation,
                    &query.resource_id,
                    &query.incarnation,
                    &query.operation_id,
                    &query.request_digest,
                ) {
                    return Err("checkpoint_proof_unavailable".into());
                }
                None
            } else {
                let evidence = journal
                    .composed_releases
                    .verified_evidence_for_target(journal, &query.target)
                    .ok_or("release_proof_unavailable")?;
                let selection = &evidence.finalized.selection;
                let root = &selection.input;
                if selection.policy.owner_provider != caller
                    || root.output.producer != caller
                    || root.output.resource.resource_id != query.resource_id
                    || root.output.resource.incarnation != query.incarnation
                    || super::resource_generation::historical_generation(journal, root)?
                        != query.resource_generation
                    || !self.settings.finalize_policies.contains(&selection.policy)
                    || selection
                        .schema_documents
                        .iter()
                        .any(|doc| !self.settings.schema_documents.contains(doc))
                    || !selected.approvals.contains(&selection.finalize_approval)
                    || !selected
                        .approvals
                        .contains(&selection.finalize_status_approval)
                {
                    return Err("release_root_mismatch".into());
                }
                let mut records = journal.control_settlements.records().filter(|record| {
                    let request = &record.settlement.request;
                    request.target == query.target
                        && request.proposal.owner_provider == caller
                        && request.proposal.resource.resource_id == query.resource_id
                        && request.proposal.resource.incarnation == query.incarnation
                        && request.operation_id == query.operation_id
                        && record.owner_ack.is_some()
                        && record.owner_dispatch.is_some()
                        && serde_json::to_vec(request).ok().is_some_and(|bytes| {
                            format!("{:x}", Sha256::digest(bytes)) == query.request_digest
                        })
                });
                let record = records.next().ok_or("control_proof_unavailable")?;
                if records.next().is_some() {
                    return Err("control_proof_ambiguous".into());
                }
                Some(record.settlement.clone())
            }
        };
        if let Some(tuple) = live_tuple {
            let proof = self
                .verify_historical_settlement(caller, &tuple)?
                .ok_or("control_proof_unavailable")?;
            if proof["status"] != "valid"
                || proof["target"] != json!(query.target)
                || proof["resource_generation"] != json!(query.resource_generation)
                || proof["root_output"]["resource"]["resource_id"] != query.resource_id
                || proof["root_output"]["resource"]["incarnation"] != query.incarnation
            {
                return Err("control_proof_mismatch".into());
            }
        }
        Ok(
            json!({"status":"closed", "target":query.target, "resource_generation":query.resource_generation,
            "resource_id":query.resource_id, "incarnation":query.incarnation,
            "operation_id":query.operation_id, "request_digest":query.request_digest}),
        )
    }
}
