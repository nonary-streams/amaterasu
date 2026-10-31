//! Bounded retention for independently confirmed retired lifecycle targets.
//!
//! Digests fence operation keys after their full receipts are removed. They are
//! never deleted: when the budget is exhausted, compaction stops and admission
//! eventually fails closed at the normal journal limits.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, VecDeque};

const KEEP_ENDED_SESSIONS: usize = 32;
const KEEP_RECEIPTS: usize = 128;
const MAX_TOMBSTONES: usize = 8192;
const TARGET_JOURNAL_BYTES: usize = 384 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Retention {
    #[serde(default)]
    order: VecDeque<String>,
    #[serde(default)]
    tombstones: BTreeSet<String>,
}

fn digest(key: &str) -> String {
    Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(super) fn expired(journal: &Journal, key: &str) -> bool {
    journal.retention.tombstones.contains(&digest(key))
}

pub(super) fn at_capacity(journal: &Journal) -> bool {
    journal.retention.tombstones.len() >= MAX_TOMBSTONES
}

pub(super) fn validate(journal: &Journal) -> bool {
    let retention = &journal.retention;
    retention.tombstones.len() <= MAX_TOMBSTONES
        && retention.tombstones.iter().all(|d| {
            d.len() == 64
                && d.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        && retention.order.len() <= MAX_RECEIPTS
        && retention.order.iter().collect::<BTreeSet<_>>().len() == retention.order.len()
        && retention
            .order
            .iter()
            .all(|key| journal.receipts.contains_key(key))
        && journal
            .receipts
            .keys()
            .all(|key| !retention.tombstones.contains(&digest(key)))
}

fn target(value: &Value) -> Option<LifecycleTarget> {
    value
        .get("target")
        .and_then(|target| serde_json::from_value(target.clone()).ok())
}
fn matches_target(target: &LifecycleTarget, session_id: &str, stream_id: Option<&str>) -> bool {
    match target {
        LifecycleTarget::Session { session_id: id, .. } => id == session_id && stream_id.is_none(),
        LifecycleTarget::Stream {
            session_id: id,
            stream_id: stream,
            ..
        } => id == session_id && stream_id.is_none_or(|wanted| stream == wanted),
        LifecycleTarget::Validation { .. } => false,
    }
}
fn has_composition_output(journal: &Journal, session_id: &str, stream_id: Option<&str>) -> bool {
    journal
        .composition
        .records()
        .any(|record| matches_target(&record.output.origin, session_id, stream_id))
        || journal
            .control_settlements
            .records()
            .any(|record| matches_target(&record.settlement.request.target, session_id, stream_id))
}
fn unresolved_receipt(receipt: &Receipt, session_id: &str, stream_id: Option<&str>) -> bool {
    let matches = receipt
        .work
        .as_ref()
        .is_some_and(|work| matches_target(&work.target, session_id, stream_id))
        || receipt
            .wait_target
            .as_ref()
            .is_some_and(|target| matches_target(target, session_id, stream_id))
        || target(&receipt.request)
            .is_some_and(|target| matches_target(&target, session_id, stream_id))
        || target(&receipt.response)
            .is_some_and(|target| matches_target(&target, session_id, stream_id));
    matches
        && (receipt.work.is_some()
            || receipt.wait_target.is_some()
            || !matches!(
                receipt.response["status"].as_str(),
                Some("succeeded" | "rejected" | "cancelled")
            ))
}
fn target_is_absent(journal: &Journal, target: &LifecycleTarget) -> bool {
    match target {
        LifecycleTarget::Session { session_id, .. } => !journal.sessions.contains_key(session_id),
        LifecycleTarget::Stream {
            session_id,
            stream_id,
            ..
        } => journal
            .sessions
            .get(session_id)
            .is_none_or(|session| !session.streams.contains_key(stream_id)),
        LifecycleTarget::Validation { .. } => false,
    }
}

/// Compact only closed records. The caller persists this journal atomically
/// before admitting further work; any write failure fences later admission.
pub(super) fn compact(journal: &mut Journal) {
    // An admission without a journaled owning work item is ambiguous and must
    // fence all pruning. Once ownership is known, unrelated sessions may retire.
    let authority_quiet = journal
        .authority
        .domain_locks
        .values()
        .all(|id| journal.authority.admissions.contains_key(id))
        && journal.authority.admissions.keys().all(|id| {
            journal.receipts.values().any(|receipt| {
                receipt.work.as_ref().is_some_and(|work| {
                    work.states
                        .values()
                        .any(|state| state.admission_id.as_deref() == Some(id.as_str()))
                })
            })
        })
        && journal.authority.settled.values().all(|admission| {
            !admission.plan.obligations.is_empty()
                && admission.plan.obligations.iter().all(|obligation| {
                    journal
                        .sessions
                        .values()
                        .any(|session| session.obligations.contains(obligation))
                })
        });
    if authority_quiet {
        let sessions: Vec<_> = journal
            .sessions
            .iter()
            .map(|(id, session)| (id.clone(), session.host_epoch.clone()))
            .collect();
        for (id, epoch) in &sessions {
            if !journal.sessions[id].obligations.is_empty()
                || has_composition_output(journal, id, None)
                || !matches!(
                    journal.sessions[id].phase,
                    SessionPhase::Paused | SessionPhase::Ended
                )
                || journal.receipts.values().any(|receipt| {
                    receipt
                        .work
                        .as_ref()
                        .is_some_and(|work| matches_target(&work.target, id, None))
                })
            {
                continue;
            }
            let removable: Vec<_> = journal.sessions[id]
                .streams
                .iter()
                .filter(|(stream_id, stream)| {
                    stream.phase == StreamPhase::Retired
                        && !journal.inputs.has_unresolved_stream(epoch, id, stream_id)
                        && !journal.actions.has_unresolved_stream(epoch, id, stream_id)
                        && !journal.input_bindings.values().any(|binding| {
                            binding.session_id == *id && binding.stream_id == **stream_id
                        })
                        && !journal
                            .receipts
                            .values()
                            .any(|receipt| unresolved_receipt(receipt, id, Some(stream_id)))
                })
                .map(|(stream_id, _)| stream_id.clone())
                .collect();
            for stream_id in removable {
                journal
                    .sessions
                    .get_mut(id)
                    .unwrap()
                    .streams
                    .remove(&stream_id);
            }
        }
        let mut ended: Vec<_> = sessions
            .into_iter()
            .filter(|(id, epoch)| {
                let session = &journal.sessions[id];
                session.phase == SessionPhase::Ended
                    && session.application_retired
                    && session.obligations.is_empty()
                    && session.streams.is_empty()
                    && !journal.inputs.has_unresolved_session(epoch, id)
                    && !has_composition_output(journal, id, None)
                    && !journal.actions.has_unresolved_session(epoch, id)
                    && !journal
                        .input_bindings
                        .values()
                        .any(|binding| binding.session_id == *id)
                    && !journal
                        .receipts
                        .values()
                        .any(|receipt| unresolved_receipt(receipt, id, None))
            })
            .collect();
        // Prefer removing sessions whose creation receipt appears first.
        let order: BTreeMap<_, _> = journal
            .retention
            .order
            .iter()
            .enumerate()
            .map(|(i, key)| (key, i))
            .collect();
        ended.sort_by_key(|(id, _)| {
            journal
                .receipts
                .iter()
                .filter(|(_, receipt)| {
                    receipt.request["operation"] == "prepare"
                        && receipt.response["session_id"] == *id
                })
                .filter_map(|(key, _)| order.get(key).copied())
                .min()
                .unwrap_or(0)
        });
        let remove = ended.len().saturating_sub(KEEP_ENDED_SESSIONS);
        for (id, _) in ended.into_iter().take(remove) {
            journal.sessions.remove(&id);
        }
    }
    journal
        .retention
        .order
        .retain(|key| journal.receipts.contains_key(key));
    let known: BTreeSet<_> = journal.retention.order.iter().cloned().collect();
    journal.retention.order.extend(
        journal
            .receipts
            .keys()
            .filter(|key| !known.contains(*key))
            .cloned(),
    );
    let mut remaining = journal.retention.order.len().saturating_sub(KEEP_RECEIPTS);
    let mut estimated_bytes = serde_json::to_vec(journal).map_or(usize::MAX, |bytes| bytes.len());
    let mut kept = VecDeque::new();
    while let Some(key) = journal.retention.order.pop_front() {
        let receipt = &journal.receipts[&key];
        let referenced_by_alias = journal
            .recovery_sources
            .iter()
            .any(|(alias, original)| original == &key && journal.receipts.contains_key(alias));
        let closed = !referenced_by_alias
            && receipt.work.is_none()
            && receipt.wait_target.is_none()
            && matches!(
                receipt.response["status"].as_str(),
                Some("succeeded" | "rejected" | "cancelled")
            )
            && target(&receipt.response)
                .or_else(|| target(&receipt.request))
                .is_some_and(|target| target_is_absent(journal, &target));
        if (remaining > 0 || estimated_bytes > TARGET_JOURNAL_BYTES)
            && closed
            && journal.retention.tombstones.len() < MAX_TOMBSTONES
        {
            let receipt_bytes = serde_json::to_vec(receipt).map_or(0, |bytes| bytes.len());
            // The tombstone adds 67 bytes; account conservatively for the removed
            // receipt, map key, and receipt-order entry.
            estimated_bytes = estimated_bytes
                .saturating_sub(receipt_bytes + 2 * key.len())
                .saturating_add(67);
            journal.retention.tombstones.insert(digest(&key));
            journal.receipts.remove(&key);
            remaining = remaining.saturating_sub(1);
        } else {
            kept.push_back(key);
        }
    }
    journal.retention.order = kept;
    journal
        .recovery_sources
        .retain(|alias, _| journal.receipts.contains_key(alias));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_authority::ActiveBinding;

    fn journal() -> Journal {
        serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap()
    }

    fn insert_ended(journal: &mut Journal, id: usize, status: &str) -> String {
        let session = format!("session-{id}");
        let operation = format!("create-{id}");
        let target =
            json!({"kind":"session","host_epoch":"epoch","session_id":session,"generation":1});
        let record: SessionRecord = serde_json::from_value(json!({"host_epoch":"epoch","id":session,"generation":1,"phase":"ended","owner":{"caller":"transport","initiator":"client"},"delegates":[],"application_ref":{"kind":"application"},"settings_revision":"one","application_retired":true,"streams":{},"obligations":[],"required_consumers":null,"application":null})).unwrap();
        journal.sessions.insert(session, record);
        let key = receipt_key("transport", "client", &operation);
        journal.receipts.insert(
            key.clone(),
            Receipt {
                caller: "transport".into(),
                initiator: "client".into(),
                request: json!({"operation":"prepare","operation_id":operation}),
                response: json!({"operation_id":operation,"status":status,"target":target}),
                work: None,
                wait_target: None,
            },
        );
        key
    }

    #[test]
    fn retained_output_fences_session_and_stream_compaction() {
        let mut journal = journal();
        for id in 0..40 {
            insert_ended(&mut journal, id, "succeeded");
        }
        journal.composition = serde_json::from_value(json!({"outputs":[{
            "output":{"output_id":"output","capability":"screen","schema":{"id":"schema","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"provider","producer_service":"provider/1","origin":{"kind":"session","host_epoch":"epoch","session_id":"session-0","generation":1},"resource":{"provider":"provider","resource_id":"display","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null},"data":{}},
            "admission_id":"admission","transition_id":"transition","state":"retained",
            "plan":{"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]}
        }]})).unwrap();
        compact(&mut journal);
        assert!(journal.sessions.contains_key("session-0"));
        assert!(journal.sessions.len() <= KEEP_ENDED_SESSIONS + 1);
    }

    #[test]
    fn rolls_over_completed_sessions_and_fences_expired_creation_after_reload() {
        let mut journal = journal();
        let mut first = String::new();
        for id in 0..200 {
            let key = insert_ended(&mut journal, id, "succeeded");
            if id == 0 {
                first = key;
            }
            compact(&mut journal);
            assert!(journal.sessions.len() < MAX_SESSIONS);
        }
        assert!(expired(&journal, &first));
        assert!(!journal.receipts.contains_key(&first));
        assert!(validate(&journal));
        let restored: Journal =
            serde_json::from_slice(&serde_json::to_vec(&journal).unwrap()).unwrap();
        assert!(expired(&restored, &first));
        assert!(validate(&restored));
        let root =
            std::env::temp_dir().join(format!("amaterasu-retention-{}", identifier().unwrap()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&root.join("journal.json")).unwrap();
        save(&coordinator.shared, restored).unwrap();
        assert_eq!(
            coordinator.handle(
                "transport",
                "client",
                json!({"operation":"prepare","operation_id":"create-0"})
            )["reason"],
            "receipt_expired"
        );
        assert_eq!(
            coordinator.handle(
                "transport",
                "client",
                json!({"operation":"lookup","operation_id":"create-0"})
            )["reason"],
            "receipt_expired"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unresolved_input_and_failed_or_deferred_receipts_block_eviction() {
        let mut journal = journal();
        insert_ended(&mut journal, 0, "succeeded");
        insert_ended(&mut journal, 1, "failed");
        insert_ended(&mut journal, 2, "deferred_restoration");
        let binding = ActiveBinding {
            host_epoch: "epoch".into(),
            session_id: "session-0".into(),
            stream_id: "stream".into(),
            generation: 1,
            geometry_revision: 1,
            input_consumer: "input".into(),
            allowed_actions: Default::default(),
        };
        let token = journal.inputs.issue_input_binding(&binding).unwrap();
        journal.input_bindings.insert(token, binding);
        for id in 3..90 {
            insert_ended(&mut journal, id, "succeeded");
            compact(&mut journal);
        }
        for id in 0..=2 {
            assert!(journal.sessions.contains_key(&format!("session-{id}")));
        }
    }

    #[test]
    fn retired_streams_are_removed_but_resume_identity_remains_fenced() {
        let mut journal = journal();
        insert_ended(&mut journal, 0, "succeeded");
        journal.sessions.get_mut("session-0").unwrap().phase = SessionPhase::Paused;
        let mut first = String::new();
        for id in 0..150 {
            let stream_id = format!("stream-{id}");
            let op = format!("resume-{id}");
            let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session-0","stream_id":stream_id,"generation":1});
            let stream: StreamRecord = serde_json::from_value(json!({"generation":1,"phase":"retired","candidate":null,"required":["capture"],"acknowledgements":["capture"],"retirements":["capture"]})).unwrap();
            journal
                .sessions
                .get_mut("session-0")
                .unwrap()
                .streams
                .insert(stream_id, stream);
            let key = receipt_key("transport", "client", &op);
            journal.receipts.insert(key.clone(), Receipt { caller:"transport".into(), initiator:"client".into(), request:json!({"operation":"resume_stream","operation_id":op,"target":{"kind":"session","host_epoch":"epoch","session_id":"session-0","generation":1}}), response:json!({"status":"succeeded","operation_id":op,"target":target}), work:None, wait_target:None });
            if id == 0 {
                first = key;
            }
            compact(&mut journal);
        }
        assert!(journal.sessions["session-0"].streams.is_empty());
        assert!(expired(&journal, &first));
        assert!(validate(&journal));
    }

    #[test]
    fn byte_pressure_expires_safe_receipts_before_store_limit() {
        let mut journal = journal();
        for id in 0..90 {
            let key = insert_ended(&mut journal, id, "succeeded");
            journal.receipts.get_mut(&key).unwrap().response["detail"] = json!("x".repeat(7000));
            compact(&mut journal);
        }
        assert!(serde_json::to_vec(&journal).unwrap().len() < 600 * 1024);
        assert!(expired(
            &journal,
            &receipt_key("transport", "client", "create-0")
        ));
        assert!(validate(&journal));
    }

    #[test]
    fn recovery_alias_never_outlives_its_original_receipt() {
        let mut journal = journal();
        let original = insert_ended(&mut journal, 0, "succeeded");
        let alias = receipt_key("transport", "client", "recovery-0");
        let target = journal.receipts[&original].response["target"].clone();
        journal.receipts.insert(alias.clone(), Receipt { caller:"transport".into(), initiator:"client".into(), request:json!({"operation":"recover_session","operation_id":"recovery-0","target":target}), response:json!({"operation_id":"recovery-0","status":"succeeded","target":target}), work:None, wait_target:None });
        journal
            .recovery_sources
            .insert(alias.clone(), original.clone());
        for id in 1..200 {
            insert_ended(&mut journal, id, "succeeded");
            compact(&mut journal);
        }
        assert!(!journal.receipts.contains_key(&alias) || journal.receipts.contains_key(&original));
        assert!(
            !journal.recovery_sources.contains_key(&alias) || journal.receipts.contains_key(&alias)
        );
    }

    #[test]
    fn attributed_obligation_does_not_pin_unrelated_ended_sessions() {
        let mut journal = journal();
        insert_ended(&mut journal, 0, "succeeded");
        journal.sessions.get_mut("session-0").unwrap().phase = SessionPhase::Ready;
        let method = json!({"service":"example.display/1","method":"cleanup","interface_major":1,"interface_minor":0,"schema_revision":1});
        let obligation = json!({"obligation_id":"restore","provider":"display","resource_id":"output","mutation_domain":null,"depends_on":[],"cleanup_method":method,"payload":{}});
        journal
            .sessions
            .get_mut("session-0")
            .unwrap()
            .obligations
            .push(serde_json::from_value(obligation.clone()).unwrap());
        let selection = json!({"participant":{"provider":"display","plan_method":method,"callback_method":method,"status_method":method,"binding_validate_method":null,"capabilities":[],"cleanup_methods":[],"settings_revision":"one","settings":{}},"approvals":[],"capabilities":[],"cleanup_methods":[],"authorized_borrowers":[]});
        let plan = json!({"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[obligation],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]});
        journal.authority.settled.insert(
            "admission".into(),
            serde_json::from_value(
                json!({"provider":"display","selection":selection,"plan":plan,"rights":[]}),
            )
            .unwrap(),
        );
        for id in 1..100 {
            insert_ended(&mut journal, id, "succeeded");
            compact(&mut journal);
        }
        assert!(journal.sessions.contains_key("session-0"));
        assert!(journal.authority.settled.contains_key("admission"));
        assert!(journal.sessions.len() < MAX_SESSIONS);
        assert!(
            expired(&journal, &receipt_key("transport", "client", "create-1"))
                || journal
                    .receipts
                    .contains_key(&receipt_key("transport", "client", "create-1"))
        );
    }

    #[test]
    fn unattributed_native_admission_fences_all_target_pruning() {
        let mut journal = journal();
        journal
            .authority
            .domain_locks
            .insert("topology".into(), "unknown-admission".into());
        for id in 0..80 {
            insert_ended(&mut journal, id, "succeeded");
            compact(&mut journal);
        }
        assert_eq!(journal.sessions.len(), 80);
        assert!(!expired(
            &journal,
            &receipt_key("transport", "client", "create-0")
        ));
    }

    #[test]
    fn exhausted_budget_rejects_new_creation_explicitly() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-retention-capacity-{}",
            identifier().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&root.join("journal.json")).unwrap();
        let mut journal = coordinator.shared.borrow().value.clone();
        journal.retention.tombstones = (0..MAX_TOMBSTONES)
            .map(|id| digest(&format!("old-{id}")))
            .collect();
        save(&coordinator.shared, journal).unwrap();
        assert_eq!(
            coordinator.handle(
                "transport",
                "client",
                json!({"operation":"prepare","operation_id":"new"})
            )["reason"],
            "retention_capacity_exhausted"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exhausted_tombstones_never_drop_unfenced_receipts() {
        let mut journal = journal();
        journal.retention.tombstones = (0..MAX_TOMBSTONES)
            .map(|id| digest(&format!("old-{id}")))
            .collect();
        for id in 0..200 {
            insert_ended(&mut journal, id, "succeeded");
            compact(&mut journal);
        }
        let first = receipt_key("transport", "client", "create-0");
        assert!(journal.receipts.contains_key(&first));
        assert!(!expired(&journal, &first));
        assert!(validate(&journal));
    }
}
