//! Source-qualified policy identities for a preflight handoff.
//!
//! This identity is evidence to compare, not an admission token. A verifier
//! must still check the trusted coordinator, owner, operation and display/RTSP
//! admission. Opaque library revisions are never numeric catalog revisions.
use super::v2::{ApplicationReference, token};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum PolicyIdentity {
    Catalog {
        version: u8,
        revision: u64,
    },
    Library {
        version: u8,
        application: ApplicationReference,
        entry_revision: String,
        policy_revision: String,
        snapshot_id: String,
    },
}

impl PolicyIdentity {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Catalog { version: 1, .. } => Ok(()),
            Self::Library {
                version: 1,
                application,
                entry_revision,
                policy_revision,
                snapshot_id,
            } if application.validate().is_ok()
                && token(entry_revision)
                && token(policy_revision)
                && token(snapshot_id) =>
            {
                Ok(())
            }
            _ => Err("invalid_policy_identity"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn opaque_library_revisions_remain_distinct_from_catalog_revisions() {
        let library: PolicyIdentity =
            serde_json::from_value(json!({"source":"library","version":1,
            "application":{"library":"games","id":"app:1"},
            "entry_revision":"01:opaque", "policy_revision":"001",
            "snapshot_id":"opaque-snapshot"}))
            .unwrap();
        assert!(library.validate().is_ok());
        let catalog = PolicyIdentity::Catalog {
            version: 1,
            revision: 1,
        };
        assert_ne!(library, catalog);
        assert_eq!(
            serde_json::to_value(library).unwrap()["entry_revision"],
            "01:opaque"
        );
        assert!(
            serde_json::from_value::<PolicyIdentity>(json!({"source":"library","version":1,
            "application":{"library":"games","id":"app:1"},"entry_revision":1,
            "policy_revision":"001","snapshot_id":"opaque-snapshot"}))
            .is_err()
        );
    }

    #[test]
    fn library_identity_rejects_missing_or_changed_components() {
        let value = json!({"source":"library","version":1,"application":{"library":"games","id":"app"},
            "entry_revision":"entry","policy_revision":"policy","snapshot_id":"id"});
        for key in ["entry_revision", "policy_revision", "snapshot_id"] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(serde_json::from_value::<PolicyIdentity>(missing).is_err());
            let mut empty = value.clone();
            empty[key] = json!("");
            assert!(
                serde_json::from_value::<PolicyIdentity>(empty)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        let mut unsupported = value.clone();
        unsupported["version"] = json!(2);
        assert!(
            serde_json::from_value::<PolicyIdentity>(unsupported)
                .unwrap()
                .validate()
                .is_err()
        );
        let original: PolicyIdentity = serde_json::from_value(value.clone()).unwrap();
        let mut changed = value;
        changed["policy_revision"] = json!("other");
        assert_ne!(
            original,
            serde_json::from_value::<PolicyIdentity>(changed).unwrap()
        );
    }
}
