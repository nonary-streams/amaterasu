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
    retired_root: amaterasu_extension_api::ResourceOutput,
    replacement_generation: u64,
    replacement_admission: String,
    replacement_commitment: String,
    caller: String,
    binding: ActionBinding,
    permit_commitment: String,
    dispatch_commitment: String,
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
            && valid_id(&self.replacement_admission)
            && valid_id(&self.retired_admission)
            && valid_id(&self.retired_transition)
            && self.retired_root.producer == self.owner
            && self.retired_root.origin == self.session_target
            && amaterasu_extension_api::validate_resource_output(&self.retired_root).is_ok()
            && valid_id(&self.binding.operation_id)
            && valid_id(&self.binding.action_id)
            && valid_id(&self.binding.source.caller)
            && valid_id(&self.binding.source.initiator)
            && digest(&self.binding.plan_digest)
            && digest(&self.replacement_commitment)
            && digest(&self.permit_commitment)
            && digest(&self.dispatch_commitment)
            && self.resource_generation > 0
            && self.replacement_generation > self.resource_generation
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
        if current <= generation {
            return Err("replacement_generation_missing".into());
        }
        let mut roots = journal.composition.records().filter(|root| {
            root.output.origin == marker.session_target
                && root.output.producer == owner
                && root.state == composition::OutputState::Retained
        });
        let replacement = roots.next().ok_or("replacement_root_missing")?;
        if roots.next().is_some()
            || resource_generation::current_root_generation(journal, replacement).ok()
                != Some(current)
        {
            return Err("replacement_root_unproven".into());
        }
        let transfer = marker
            .cleanup_work
            .action_transfer
            .as_ref()
            .ok_or("action_closure_transfer_missing")?;
        Ok(Self {
            version: 1,
            session_target: marker.session_target.clone(),
            owner: owner.into(),
            resource_generation: generation,
            retired_admission: old_root.admission_id.clone(),
            retired_transition: old_root.transition_id.clone(),
            retired_root: old_root.output.clone(),
            replacement_generation: current,
            replacement_admission: replacement.admission_id.clone(),
            replacement_commitment: commitment(replacement)
                .ok_or("replacement_commitment_invalid")?,
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
    fn absent_marker_cannot_create_certificate() {
        let journal: Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap();
        assert!(ClosedActionCertificate::from_verified(&journal, "session", "owner", 1).is_err());
    }
}
