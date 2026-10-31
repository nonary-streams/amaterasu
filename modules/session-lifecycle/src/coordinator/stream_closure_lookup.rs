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
    #[serde(default)]
    require_retirement_safe: bool,
}

fn contains_target(value: &Value, target: &Value) -> bool {
    value == target
        || value.as_object().is_some_and(|object| {
            ["host_epoch", "session_id", "stream_id"].iter().all(|key| {
                target
                    .get(*key)
                    .is_some_and(|expected| object.get(*key) == Some(expected))
            })
        })
        || match value {
            Value::Array(items) => items.iter().any(|item| contains_target(item, target)),
            Value::Object(items) => items.values().any(|item| contains_target(item, target)),
            _ => false,
        }
}

fn receipt_is_pending(receipt: &Receipt) -> bool {
    receipt.work.is_some()
        || receipt.wait_target.is_some()
        || !matches!(
            receipt.response["status"].as_str(),
            Some("succeeded" | "rejected" | "cancelled" | "failed")
        )
}

fn receipt_references(receipt: &Receipt, target: &Value) -> bool {
    contains_target(&receipt.request, target)
        || contains_target(&receipt.response, target)
        || receipt
            .wait_target
            .as_ref()
            .is_some_and(|wait| serde_json::to_value(wait).ok().as_ref() == Some(target))
        || receipt.work.as_ref().is_some_and(|work| {
            serde_json::to_value(work).map_or(true, |value| contains_target(&value, target))
        })
}

fn pending_receipt_references(receipt: &Receipt, target: &Value) -> bool {
    receipt_is_pending(receipt)
        && (receipt_references(receipt, target)
            || (receipt.work.is_none()
                && receipt.wait_target.is_none()
                && receipt.request.get("target").is_none_or(Value::is_null)))
}

impl Coordinator {
    /// Conservative old-scope proof only. It does not attest that an owner
    /// has drained its own local state, and it never grants effect authority.
    fn retirement_safe(&self, journal: &Journal, target: &LifecycleTarget) -> bool {
        let Ok((epoch, session, Some(stream), _)) = operations::target_parts(target) else {
            return false;
        };
        if !(journal.closed_streams.scope_fenced(target)
            || retention::session_expired(journal, target))
            || journal
                .actions
                .has_unresolved_stream(epoch, session, stream)
            || journal.inputs.has_unresolved_stream(epoch, session, stream)
            || journal
                .input_bindings
                .values()
                .any(|binding| binding.session_id == session && binding.stream_id == stream)
            || journal
                .authority
                .control_permits
                .values()
                .any(|permit| &permit.request.target == target && !permit.settled)
            || journal
                .authority
                .finalize_permits
                .values()
                .any(|permit| &permit.request.target == target && !permit.settled)
            || journal.sessions.values().any(|record| {
                record.host_epoch == epoch
                    && record.id == session
                    && record.admission_ids.iter().any(|id| {
                        journal.authority.admissions.contains_key(id)
                            && record
                                .admission_targets
                                .get(id)
                                .is_none_or(|admitted_target| admitted_target == target)
                    })
            })
            || journal.control_settlements.records().any(|record| {
                &record.settlement.request.target == target && record.owner_ack.is_none()
            })
            || !self.validations.is_empty()
            || !self.revocations.is_empty()
            || !self.application_jobs.is_empty()
            || !self.settings_jobs.is_empty()
        {
            return false;
        }
        let Ok(target_value) = serde_json::to_value(target) else {
            return false;
        };
        if journal
            .receipts
            .values()
            .any(|receipt| pending_receipt_references(receipt, &target_value))
            || self
                .ephemeral
                .values()
                .any(|(receipt, _)| pending_receipt_references(receipt, &target_value))
            || journal.recovery_sources.iter().any(|(alias, original)| {
                let Some(alias) = journal.receipts.get(alias) else {
                    return true;
                };
                let Some(original) = journal.receipts.get(original) else {
                    return true;
                };
                receipt_is_pending(alias) && receipt_references(original, &target_value)
            })
        {
            return false;
        }
        // A driver without one exact durable active receipt is ambiguous.
        // A different in-flight session callback may continue if its frozen
        // Work does not reference this old stream target.
        let driver_safe = |driver_key: &str| {
            let mut owners = journal
                .receipts
                .iter()
                .filter(|(key, _)| driver_key.starts_with(&format!("{key}/")));
            let Some((_, receipt)) = owners.next() else {
                return false;
            };
            owners.next().is_none()
                && receipt.work.is_some()
                && !pending_receipt_references(receipt, &target_value)
        };
        self.drivers.keys().all(|key| driver_safe(key))
            && self.finalize_drivers.keys().all(|key| driver_safe(key))
            && self.observation_drivers.keys().all(|key| driver_safe(key))
            && self.control_drivers.keys().all(|key| driver_safe(key))
            && self.settlement_drivers.keys().all(|key| driver_safe(key))
    }
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
            if let Some((history, _)) =
                journal
                    .ended_session_history
                    .historical_children(journal, &query.target, caller)
            {
                if checkpoint
                    || live
                    || !history.installed(&self.settings)
                    || !history.contains_closure_query(
                        caller,
                        &query.target,
                        query.resource_generation,
                        (&query.resource_id, &query.incarnation),
                        &query.operation_id,
                        &query.request_digest,
                    )
                {
                    return Err("ended_session_closure_unavailable".into());
                }
                if query.require_retirement_safe && !self.retirement_safe(journal, &query.target) {
                    return Err("retirement_not_safe".into());
                }
                let mut response = json!({"status":"closed","target":query.target,"resource_generation":query.resource_generation,
                    "resource_id":query.resource_id,"incarnation":query.incarnation,"operation_id":query.operation_id,
                    "request_digest":query.request_digest});
                if query.require_retirement_safe {
                    response["retirement_safe"] = json!(true);
                }
                return Ok(response);
            }
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
        if query.require_retirement_safe
            && !self.retirement_safe(&self.shared.borrow().value, &query.target)
        {
            return Err("retirement_not_safe".into());
        }
        let mut response = json!({"status":"closed", "target":query.target, "resource_generation":query.resource_generation,
            "resource_id":query.resource_id, "incarnation":query.incarnation,
            "operation_id":query.operation_id, "request_digest":query.request_digest});
        if query.require_retirement_safe {
            response["retirement_safe"] = json!(true);
        }
        Ok(response)
    }
}

#[cfg(test)]
mod retirement_safe_tests {
    use super::*;

    #[test]
    fn strong_query_requires_fence_and_no_pending_scope_or_alias() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-retirement-safe-{}-{}",
            std::process::id(),
            identifier().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = root.join("journal.json");
        let coordinator = Coordinator::open(&path).unwrap();
        let mut journal = coordinator.shared.borrow().value.clone();
        let target: LifecycleTarget = serde_json::from_value(json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1})).unwrap();
        assert!(!coordinator.retirement_safe(&journal, &target));
        let session: LifecycleTarget = serde_json::from_value(
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":7}),
        )
        .unwrap();
        retention::fence_closed_session(&mut journal, &session).unwrap();
        assert!(coordinator.retirement_safe(&journal, &target));
        let unrelated = Receipt {
            caller: "caller".into(),
            initiator: "client".into(),
            request: json!({"target":session}),
            response: json!({"status":"pending"}),
            work: None,
            wait_target: None,
        };
        journal.receipts.insert("unrelated".into(), unrelated);
        assert!(coordinator.retirement_safe(&journal, &target));
        journal.receipts.get_mut("unrelated").unwrap().request = json!({});
        assert!(!coordinator.retirement_safe(&journal, &target));
        journal.receipts.get_mut("unrelated").unwrap().request = json!({"target":session});
        let original = Receipt {
            caller: "caller".into(),
            initiator: "client".into(),
            request: json!({"target":target}),
            response: json!({"status":"succeeded"}),
            work: None,
            wait_target: None,
        };
        journal.receipts.insert("original".into(), original);
        journal.receipts.insert(
            "alias".into(),
            Receipt {
                caller: "caller".into(),
                initiator: "client".into(),
                request: json!({}),
                response: json!({"status":"pending"}),
                work: None,
                wait_target: None,
            },
        );
        journal
            .recovery_sources
            .insert("alias".into(), "original".into());
        assert!(!coordinator.retirement_safe(&journal, &target));
        journal.receipts.get_mut("alias").unwrap().response["status"] = json!("succeeded");
        assert!(coordinator.retirement_safe(&journal, &target));
        journal.receipts.get_mut("original").unwrap().response["status"] = json!("pending");
        assert!(!coordinator.retirement_safe(&journal, &target));
        journal.receipts.get_mut("original").unwrap().response["status"] = json!("succeeded");
        journal.input_bindings.insert(
            "token".into(),
            crate::input_authority::ActiveBinding {
                source_id: None,
                host_epoch: "epoch".into(),
                session_id: "session".into(),
                stream_id: "stream".into(),
                generation: 1,
                geometry_revision: 1,
                input_consumer: "input".into(),
                allowed_actions: Default::default(),
            },
        );
        assert!(!coordinator.retirement_safe(&journal, &target));
        journal.input_bindings.clear();
        journal.receipts.clear();
        journal.recovery_sources.clear();
        save(&coordinator.shared, journal).unwrap();
        drop(coordinator);
        let reopened = Coordinator::open(&path).unwrap();
        assert!(reopened.retirement_safe(&reopened.shared.borrow().value, &target));
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_query_defaults_to_non_strong_mode() {
        let query: Query = serde_json::from_value(json!({"operation":"lookup_stream_closure","target":{"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1},"resource_generation":1,"resource_id":"resource","incarnation":"incarnation","operation_id":"operation","request_digest":"a".repeat(64)})).unwrap();
        assert!(!query.require_retirement_safe);
    }
}
