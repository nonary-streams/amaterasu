//! Read-only preflight for compacting completed action history.
//!
//! Certificates are private-journal evidence, not authority to execute an
//! action. Their insertion and source retirement remain disabled until one
//! atomic transaction can fence and retire every inbound reference.
use super::*;
use crate::action_authority::{ActionBinding, ActionTarget};
use sha2::{Digest, Sha256};

const MAX_CLOSED_ACTIONS: usize = 1024;

/// Compact private-journal completion receipts. Insertion is only permitted by
/// an atomic retirement transaction after all source references are fenced.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClosedActionBook {
    records: Vec<ClosedActionCertificate>,
}

impl ClosedActionBook {
    pub(super) fn validate(&self) -> bool {
        self.records.len() <= MAX_CLOSED_ACTIONS
            && self.records.iter().enumerate().all(|(i, record)| {
                record.valid_shape()
                    && !self.records[..i].iter().any(|old| {
                        old.caller == record.caller && old.binding == record.binding
                            || old.permit_commitment == record.permit_commitment
                            || old.retired_admission == record.retired_admission
                    })
            })
    }

    /// Resolve a certified released root for exact coordinator reprepare.
    /// The retained root and cursor are independently rechecked on each call.
    pub(super) fn released_disposition(
        &self,
        journal: &Journal,
        session_id: &str,
        owner: &str,
        generation: u64,
    ) -> Option<action_disposition::ResolvedDisposition> {
        if !self.validate() {
            return None;
        }
        let mut records = self.records.iter().filter(|record| {
            record.owner == owner && record.resource_generation == generation
                && matches!(&record.session_target, LifecycleTarget::Session { session_id: id, .. } if id == session_id)
        });
        let record = records.next()?;
        if records.next().is_some() {
            return None;
        }
        let session = journal.sessions.get(session_id)?;
        if session
            .resource_generations
            .get(owner)
            .is_none_or(|current| *current < generation)
        {
            return None;
        }
        let mut roots = journal.composition.records().filter(|root| {
            root.admission_id == record.retired_admission
                && root.transition_id == record.retired_transition
                && root.output == record.retired_root
                && root.state == composition::OutputState::Released
        });
        if let Some(root) = roots.next() {
            if roots.next().is_some()
                || resource_generation::historical_generation(journal, root).ok()
                    != Some(generation)
            {
                return None;
            }
        } else {
            // The compact certificate is historical proof only. Reprepare still
            // requires the separately authenticated current replacement root.
            let current = *session.resource_generations.get(owner)?;
            if current <= generation {
                return None;
            }
            let mut replacements = journal.composition.records().filter(|root| {
                root.output.origin == record.session_target
                    && root.output.producer == owner
                    && resource_generation::current_root_generation(journal, root).ok()
                        == Some(current)
            });
            let replacement = replacements.next()?;
            if replacements.next().is_some()
                || replacement.output.resource.incarnation
                    == record.retired_root.resource.incarnation
            {
                return None;
            }
        }
        Some(action_disposition::ResolvedDisposition {
            session_target: record.session_target.clone(),
            owner: owner.into(),
            resource_generation: generation,
            resource: record.retired_root.resource.clone(),
            root_admission_id: record.retired_admission.clone(),
        })
    }

    /// Recover the immutable old root only as read-only historical evidence.
    pub(super) fn historical_root(
        &self,
        admission: &str,
        transition: &str,
        owner: &str,
        generation: u64,
    ) -> Option<&amaterasu_extension_api::ResourceOutput> {
        if !self.validate() {
            return None;
        }
        let mut records = self.records.iter().filter(|record| {
            record.retired_admission == admission
                && record.retired_transition == transition
                && record.owner == owner
                && record.resource_generation == generation
        });
        let record = records.next()?;
        records.next().is_none().then_some(&record.retired_root)
    }

    /// Read-only root lookup for historical checkpoints after source retirement.
    pub(super) fn historical_root_for_admission(
        &self,
        admission: &str,
        owner: &str,
        generation: u64,
    ) -> Option<&amaterasu_extension_api::ResourceOutput> {
        if !self.validate() {
            return None;
        }
        let mut records = self.records.iter().filter(|record| {
            record.retired_admission == admission
                && record.owner == owner
                && record.resource_generation == generation
        });
        let record = records.next()?;
        records.next().is_none().then_some(&record.retired_root)
    }

    pub(super) fn retired_roots(&self) -> Vec<(String, String, String, u64)> {
        if !self.validate() {
            return Vec::new();
        }
        self.records
            .iter()
            .map(|record| {
                (
                    record.retired_admission.clone(),
                    record.retired_transition.clone(),
                    record.owner.clone(),
                    record.resource_generation,
                )
            })
            .collect()
    }

    /// Exact admission IDs covered by fully verified action cleanup.
    pub(super) fn closed_admissions(&self, target: &LifecycleTarget) -> Option<BTreeSet<String>> {
        if !self.validate() {
            return None;
        }
        Some(
            self.records
                .iter()
                .filter(|record| &record.session_target == target)
                .flat_map(|record| record.closed_admissions.iter().cloned())
                .collect(),
        )
    }

    /// A retired root may be used only as historical evidence, never as a live effect grant.
    pub(super) fn certifies_root(&self, root: &composition::StoredOutput, generation: u64) -> bool {
        self.historical_root(
            &root.admission_id,
            &root.transition_id,
            &root.output.producer,
            generation,
        ) == Some(&root.output)
            && root.state == composition::OutputState::Released
    }

    /// Read-only lookup; this never recovers permission to execute an effect.
    pub(super) fn matching_retry(
        &self,
        caller: &str,
        permit: &str,
        proof: &Value,
    ) -> Option<&ActionBinding> {
        if !self.validate() {
            return None;
        }
        let mut matches = self
            .records
            .iter()
            .filter(|record| record.matches_retry(caller, &record.binding, permit, proof));
        let record = matches.next()?;
        matches.next().is_none().then_some(&record.binding)
    }

    /// Called only on a private journal clone after the complete preflight.
    fn insert_verified(
        &mut self,
        journal: &Journal,
        session: &str,
        owner: &str,
        generation: u64,
    ) -> ModuleResult<()> {
        if !self.validate() || self.records.len() >= MAX_CLOSED_ACTIONS {
            return Err("closed_action_capacity".into());
        }
        let record = ClosedActionCertificate::from_verified(journal, session, owner, generation)?;
        if !record.valid_shape()
            || self.records.iter().any(|old| {
                old.caller == record.caller && old.binding == record.binding
                    || old.permit_commitment == record.permit_commitment
                    || old.retired_admission == record.retired_admission
            })
        {
            return Err("closed_action_conflict".into());
        }
        self.records.push(record);
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClosedActionCertificate {
    version: u32,
    session_target: LifecycleTarget,
    owner: String,
    resource_generation: u64,
    retired_admission: String,
    retired_transition: String,
    closed_admissions: BTreeSet<String>,
    retired_root: amaterasu_extension_api::ResourceOutput,
    replacement_generation: u64,
    replacement_admission: String,
    replacement_commitment: String,
    caller: String,
    binding: ActionBinding,
    permit_commitment: String,
    dispatch_commitment: String,
}

/// Atomically checkpoint each fully completed action and its stream proof on
/// a private clone. Any missing proof, live reference, or full fence budget
/// leaves the original marker and every source book unchanged.
pub(super) fn compact_released_actions(journal: &mut Journal) {
    let candidates: Vec<_> = journal
        .action_dispositions
        .values()
        .filter_map(|marker| {
            let LifecycleTarget::Session { session_id, .. } = &marker.session_target else {
                return None;
            };
            Some((
                session_id.clone(),
                marker.owner.clone(),
                marker.resource_generation,
            ))
        })
        .collect();
    for (session, owner, generation) in candidates {
        let _ = compact_one_released_action(journal, &session, &owner, generation);
    }
}

fn compact_one_released_action(
    journal: &mut Journal,
    session_id: &str,
    owner: &str,
    generation: u64,
) -> ModuleResult<()> {
    let marker = action_disposition::verified_marker_at(journal, session_id, owner, generation)
        .ok_or("action_marker_unproven")?
        .clone();
    let transfer = marker
        .cleanup_work
        .action_transfer
        .as_ref()
        .ok_or("action_transfer_missing")?;
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    if session.phase != SessionPhase::Paused
        || session.application_retired
        || session
            .streams
            .values()
            .any(|stream| stream.phase != StreamPhase::Retired)
    {
        return Err("action_checkpoint_not_quiescent".into());
    }
    let mut next = journal.clone();
    let mut book = next.closed_actions.clone();
    book.insert_verified(&next, session_id, owner, generation)?;
    next.closed_actions = book;
    next.actions
        .retire_certified_success(&transfer.service, &marker.permit, &marker.binding)
        .map_err(|_| "action_retirement_unavailable")?;
    let key = action_disposition::marker_key(session_id, owner, generation);
    if next.action_dispositions.remove(&key).is_none() {
        return Err("action_marker_changed".into());
    }
    let input = transfer
        .input_reservation_id
        .as_deref()
        .map(|reservation_id| closed_stream::ActionInputExemption {
            reservation_id,
            service: &transfer.service,
            binding: &marker.binding,
        });
    match closed_stream::retire_closed_stream_for_action(
        &mut next,
        &marker.cleanup_work.target,
        &marker.cleanup_work.transition_id,
        input.as_ref(),
    )? {
        closed_stream::RetirementOutcome::Retired => {}
        closed_stream::RetirementOutcome::NotReady => return Err("action_stream_not_ready".into()),
    }
    validate_journal(&next).map_err(|_| "action_checkpoint_invalid")?;
    *journal = next;
    Ok(())
}

fn commitment(value: &impl Serialize) -> Option<String> {
    Some(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).ok()?)
    ))
}

impl ClosedActionCertificate {
    fn valid_shape(&self) -> bool {
        let LifecycleTarget::Session {
            host_epoch,
            session_id,
            generation,
        } = &self.session_target
        else {
            return false;
        };
        let scope = match &self.binding.target {
            ActionTarget::Session {
                host_epoch: epoch,
                session_id: id,
                expected_generation,
            } => epoch == host_epoch && id == session_id && expected_generation == generation,
            ActionTarget::Stream {
                host_epoch: epoch,
                session_id: id,
                stream_id,
                expected_generation,
            } => {
                epoch == host_epoch
                    && id == session_id
                    && valid_id(stream_id)
                    && *expected_generation > 0
            }
            ActionTarget::Host { .. } => false,
        };
        let digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        self.version == 1
            && valid_id(host_epoch)
            && valid_id(session_id)
            && *generation > 0
            && scope
            && valid_id(&self.owner)
            && valid_id(&self.caller)
            && valid_id(&self.retired_admission)
            && valid_id(&self.retired_transition)
            && self.closed_admissions.len() <= 128
            && self.closed_admissions.contains(&self.retired_admission)
            && self.closed_admissions.iter().all(|id| valid_id(id))
            && self.retired_root.producer == self.owner
            && self.retired_root.origin == self.session_target
            && amaterasu_extension_api::validate_resource_output(&self.retired_root).is_ok()
            && valid_id(&self.binding.operation_id)
            && valid_id(&self.binding.action_id)
            && valid_id(&self.binding.source.caller)
            && valid_id(&self.binding.source.initiator)
            && digest(&self.binding.plan_digest)
            && digest(&self.permit_commitment)
            && digest(&self.dispatch_commitment)
            && self.resource_generation > 0
            && ((self.replacement_generation == 0
                && self.replacement_admission.is_empty()
                && self.replacement_commitment.is_empty())
                || (self.replacement_generation > self.resource_generation
                    && valid_id(&self.replacement_admission)
                    && digest(&self.replacement_commitment)))
    }
    /// Construct only while the full source marker and replacement root are
    /// present and independently authenticated. This does not modify either.
    pub(super) fn from_verified(
        journal: &Journal,
        session_id: &str,
        owner: &str,
        generation: u64,
    ) -> ModuleResult<Self> {
        let marker = action_disposition::verified_marker_at(journal, session_id, owner, generation)
            .ok_or("action_closure_marker_unproven")?;
        if marker.resource_generation != generation {
            return Err("action_closure_generation_mismatch".into());
        }
        let mut old_roots = journal.composition.records().filter(|root| {
            root.admission_id == marker.root_admission_id
                && root.output.origin == marker.session_target
                && root.output.producer == owner
        });
        let old_root = old_roots.next().ok_or("retired_root_missing")?;
        if old_roots.next().is_some()
            || old_root.state != composition::OutputState::Released
            || resource_generation::historical_generation(journal, old_root).ok()
                != Some(generation)
        {
            return Err("retired_root_unproven".into());
        }
        let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
        let current = *session
            .resource_generations
            .get(owner)
            .ok_or("replacement_generation_missing")?;
        if current < generation {
            return Err("replacement_generation_invalid".into());
        }
        let mut roots = journal.composition.records().filter(|root| {
            root.output.origin == marker.session_target
                && root.output.producer == owner
                && root.state == composition::OutputState::Retained
        });
        let replacement = roots.next();
        if roots.next().is_some() {
            return Err("replacement_root_ambiguous".into());
        }
        let (replacement_generation, replacement_admission, replacement_commitment) =
            if current == generation {
                if replacement.is_some() {
                    return Err("replacement_root_unexpected".into());
                }
                (0, String::new(), String::new())
            } else {
                let replacement = replacement.ok_or("replacement_root_missing")?;
                if resource_generation::current_root_generation(journal, replacement).ok()
                    != Some(current)
                {
                    return Err("replacement_root_unproven".into());
                }
                (
                    current,
                    replacement.admission_id.clone(),
                    commitment(replacement).ok_or("replacement_commitment_invalid")?,
                )
            };
        let transfer = marker
            .cleanup_work
            .action_transfer
            .as_ref()
            .ok_or("action_closure_transfer_missing")?;
        let snapshot = transfer
            .snapshot
            .as_ref()
            .ok_or("action_closure_snapshot_missing")?;
        let proof = transfer
            .release_proof
            .as_ref()
            .ok_or("action_closure_proof_missing")?;
        let mut closed_admissions = BTreeSet::from([marker.root_admission_id.clone()]);
        closed_admissions.extend(snapshot.stream_admissions.keys().cloned());
        closed_admissions.extend(
            proof
                .callbacks
                .iter()
                .filter_map(|callback| callback.request.admission_id.clone()),
        );
        closed_admissions.insert(
            transfer
                .admission_id
                .clone()
                .ok_or("action_closure_admission_missing")?,
        );
        if closed_admissions.len() > 128
            || closed_admissions.iter().any(|id| {
                !journal.authority.settled.contains_key(id)
                    || journal.authority.admissions.contains_key(id)
            })
        {
            return Err("action_closure_admission_unsettled".into());
        }
        Ok(Self {
            version: 1,
            session_target: marker.session_target.clone(),
            owner: owner.into(),
            resource_generation: generation,
            retired_admission: old_root.admission_id.clone(),
            retired_transition: old_root.transition_id.clone(),
            closed_admissions,
            retired_root: old_root.output.clone(),
            replacement_generation,
            replacement_admission,
            replacement_commitment,
            caller: transfer.service.clone(),
            binding: marker.binding.clone(),
            permit_commitment: commitment(&marker.permit).ok_or("permit_commitment_invalid")?,
            dispatch_commitment: commitment(&marker.dispatch_proof)
                .ok_or("dispatch_commitment_invalid")?,
        })
    }

    /// This proves only an exact historical completion retry; it never permits
    /// a provider call, admission, mutation, or recovery effect.
    pub(super) fn matches_retry(
        &self,
        caller: &str,
        binding: &ActionBinding,
        permit: &str,
        proof: &Value,
    ) -> bool {
        self.valid_shape()
            && caller == self.caller
            && binding == &self.binding
            && commitment(&permit).as_ref() == Some(&self.permit_commitment)
            && commitment(proof).as_ref() == Some(&self.dispatch_commitment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_authority::{ActionTarget, Principal};

    fn certificate() -> ClosedActionCertificate {
        ClosedActionCertificate {
            version: 1,
            session_target: LifecycleTarget::Session {
                host_epoch: "epoch".into(),
                session_id: "session".into(),
                generation: 1,
            },
            owner: "owner".into(),
            resource_generation: 1,
            retired_admission: "retired".into(),
            retired_transition: "prepare".into(),
            closed_admissions: BTreeSet::from(["retired".into()]),
            retired_root: serde_json::from_value(json!({"output_id":"output","capability":"screen","schema":{"id":"screen","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"owner","producer_service":"owner/2","origin":{"kind":"session","host_epoch":"epoch","session_id":"session","generation":1},"resource":{"provider":"owner","resource_id":"display","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"data":{}})).unwrap(),
            replacement_generation: 2,
            replacement_admission: "replacement".into(),
            replacement_commitment: "a".repeat(64),
            caller: "actions".into(),
            binding: ActionBinding {
                operation_id: "operation".into(),
                action_id: "owner/remove".into(),
                target: ActionTarget::Session {
                    host_epoch: "epoch".into(),
                    session_id: "session".into(),
                    expected_generation: 1,
                },
                plan_digest: "b".repeat(64),
                source: Principal {
                    caller: "transport".into(),
                    initiator: "user".into(),
                },
            },
            permit_commitment: commitment(&"permit").unwrap(),
            dispatch_commitment: commitment(&json!({"receipt":"done"})).unwrap(),
        }
    }

    #[test]
    fn certified_historical_root_is_scoped_and_read_only() {
        let cert = certificate();
        let mut journal: Journal = serde_json::from_value(json!({
            "version":3,"host_epoch":"epoch","receipts":{},
            "sessions":{"session":{"host_epoch":"epoch","id":"session","generation":1,
                "phase":"ready","owner":{"caller":"caller","initiator":"client"},
                "delegates":[],"application_ref":null,"settings_revision":"revision",
                "application_retired":false,"streams":{}}}
        }))
        .unwrap();
        journal.closed_actions.records.push(cert.clone());
        let target: LifecycleTarget = serde_json::from_value(json!({"kind":"stream", "host_epoch":"epoch", "session_id":"session", "stream_id":"stream", "generation":2})).unwrap();
        let digest = closed_stream::commitment(&cert.retired_root).unwrap();
        let resolve = |owner: &str, generation, hash: &str| {
            closed_stream::historical_root(&journal, &target, 1, "retired", hash, owner, generation)
        };
        assert_eq!(resolve("owner", 1, &digest), Some(&cert.retired_root));
        assert!(resolve("foreign", 1, &digest).is_none());
        assert!(resolve("owner", 2, &digest).is_none());
        assert!(resolve("owner", 1, &"0".repeat(64)).is_none());
        assert!(journal.composition.records().next().is_none());
    }

    #[test]
    fn historical_retry_is_exact_and_survives_serialization() {
        let cert = certificate();
        let restored: ClosedActionCertificate =
            serde_json::from_slice(&serde_json::to_vec(&cert).unwrap()).unwrap();
        let proof = json!({"receipt":"done"});
        assert!(restored.matches_retry("actions", &cert.binding, "permit", &proof));
        assert!(!restored.matches_retry("foreign", &cert.binding, "permit", &proof));
        assert!(!restored.matches_retry("actions", &cert.binding, "other", &proof));
        assert!(!restored.matches_retry(
            "actions",
            &cert.binding,
            "permit",
            &json!({"receipt":"other"})
        ));
        let mut binding = cert.binding.clone();
        binding.operation_id = "other".into();
        assert!(!restored.matches_retry("actions", &binding, "permit", &proof));
        let mut wrong_generation = cert.binding.clone();
        if let ActionTarget::Session {
            expected_generation,
            ..
        } = &mut wrong_generation.target
        {
            *expected_generation += 1;
        }
        assert!(!restored.matches_retry("actions", &wrong_generation, "permit", &proof));
        let mut invalid = restored.clone();
        invalid.replacement_generation = invalid.resource_generation;
        assert!(!invalid.matches_retry("actions", &cert.binding, "permit", &proof));
    }

    #[test]
    fn closed_book_only_reattaches_exact_historical_tuple() {
        let certificate = certificate();
        let book = ClosedActionBook {
            records: vec![certificate.clone()],
        };
        let restored: ClosedActionBook =
            serde_json::from_slice(&serde_json::to_vec(&book).unwrap()).unwrap();
        assert!(restored.validate());
        assert_eq!(
            restored.historical_root("retired", "prepare", "owner", 1),
            Some(&certificate.retired_root)
        );
        assert!(
            restored
                .historical_root("retired", "wrong", "owner", 1)
                .is_none()
        );
        assert!(
            restored
                .historical_root("retired", "prepare", "foreign", 1)
                .is_none()
        );
        assert!(
            restored
                .historical_root("retired", "prepare", "owner", 2)
                .is_none()
        );
        assert_eq!(
            restored.matching_retry("actions", "permit", &json!({"receipt":"done"})),
            Some(&certificate.binding)
        );
        assert!(
            restored
                .matching_retry("foreign", "permit", &json!({"receipt":"done"}))
                .is_none()
        );
        assert!(
            restored
                .matching_retry("actions", "other", &json!({"receipt":"done"}))
                .is_none()
        );
        assert!(
            restored
                .matching_retry("actions", "permit", &json!({"receipt":"other"}))
                .is_none()
        );
        let duplicate = ClosedActionBook {
            records: vec![certificate.clone(), certificate],
        };
        assert!(!duplicate.validate());
        assert!(
            duplicate
                .matching_retry("actions", "permit", &json!({"receipt":"done"}))
                .is_none()
        );
    }

    #[test]
    fn endpoint_retry_survives_restart_and_never_grants_effects() {
        let path =
            std::env::temp_dir().join(format!("closed-action-retry-{}", identifier().unwrap()));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let coordinator = Coordinator::open(&path.join("journal")).unwrap();
        let mut cert = certificate();
        cert.caller = coordinator.settings.action_service.clone();
        let mut journal = coordinator.shared.borrow().value.clone();
        journal.closed_actions.records.push(cert.clone());
        save(&coordinator.shared, journal).unwrap();
        drop(coordinator);
        let mut coordinator = Coordinator::open(&path.join("journal")).unwrap();
        let payload = json!({"operation":"complete_action", "operation_id":cert.binding.operation_id,
            "action_id":cert.binding.action_id, "target":cert.binding.target, "plan_digest":cert.binding.plan_digest,
            "permit":"permit", "outcome":"succeeded", "dispatch_proof":{"receipt":"done"}});
        let response = coordinator.action_operation(&cert.caller, "ignored", &payload);
        assert_eq!(response["status"], "succeeded", "{response}");
        let mut changed = payload.clone();
        changed["operation_id"] = json!("other");
        assert_eq!(
            coordinator.action_operation(&cert.caller, "ignored", &changed)["status"],
            "rejected"
        );
        changed = payload.clone();
        changed["dispatch_proof"]["receipt"] = json!("other");
        assert_eq!(
            coordinator.action_operation(&cert.caller, "ignored", &changed)["status"],
            "rejected"
        );
        changed = payload.clone();
        changed["operation"] = json!("consume_action_permit");
        assert_eq!(
            coordinator.action_operation(&cert.caller, "ignored", &changed)["status"],
            "rejected"
        );
        assert_eq!(
            coordinator.action_operation("foreign", "ignored", &payload)["status"],
            "rejected"
        );
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn pending_replacement_is_distinct_from_a_partially_claimed_replacement() {
        let mut cert = certificate();
        cert.replacement_generation = 0;
        cert.replacement_admission.clear();
        cert.replacement_commitment.clear();
        assert!(cert.valid_shape());
        cert.replacement_admission = "unproven".into();
        assert!(!cert.valid_shape());
        cert.replacement_admission.clear();
        cert.replacement_generation = cert.resource_generation;
        assert!(!cert.valid_shape());
    }

    #[test]
    fn absent_marker_leaves_every_book_unchanged() {
        let mut journal: Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap();
        let before = serde_json::to_value(&journal).unwrap();
        assert!(compact_one_released_action(&mut journal, "session", "owner", 1).is_err());
        assert_eq!(serde_json::to_value(&journal).unwrap(), before);
        compact_released_actions(&mut journal);
        assert_eq!(serde_json::to_value(&journal).unwrap(), before);
    }

    #[test]
    fn absent_marker_cannot_create_certificate() {
        let journal: Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap();
        assert!(ClosedActionCertificate::from_verified(&journal, "session", "owner", 1).is_err());
    }
}
