//! Read-only preflight for compacting completed action history.
//!
//! Certificates are private-journal evidence, not authority to execute an
//! action. Their insertion and source retirement remain disabled until one
//! atomic transaction can fence and retire every inbound reference.
use super::*;
use crate::action_authority::ActionBinding;
use sha2::{Digest, Sha256};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClosedActionCertificate {
    version: u32,
    session_target: LifecycleTarget,
    owner: String,
    resource_generation: u64,
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
        self.version == 1
            && valid_id(&self.owner)
            && valid_id(&self.caller)
            && self.resource_generation > 0
            && self.replacement_generation > self.resource_generation
            && valid_id(&self.replacement_admission)
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
    fn absent_marker_cannot_create_certificate() {
        let journal: Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap();
        assert!(ClosedActionCertificate::from_verified(&journal, "session", "owner", 1).is_err());
    }
}
