//! Source-qualified policy identities for a preflight handoff.
//!
//! This identity is evidence to compare, not an admission token. A verifier
//! must still check the trusted coordinator, owner, operation and display/RTSP
//! admission. Opaque library revisions are never numeric catalog revisions.
use super::v2::{ApplicationReference, token};
use serde::{Deserialize, Serialize};

/// Versioned preview and owner-bound selected streaming admission endpoint.
pub const SERVICE: &str = "amaterasu.launch/3";

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Prepare {
        operation_id: String,
        application: ApplicationReference,
        entry_revision: String,
    },
    /// Paired-client admission preflight. Only the trusted protocol coordinator
    /// may assert a certificate obtained from its authenticated TLS connection.
    PrepareStreaming {
        operation_id: String,
        application: ApplicationReference,
        entry_revision: String,
        certificate_id: String,
        #[serde(default)]
        global_preparation: Vec<super::policy::PreparationStep>,
        #[serde(default)]
        legacy_global_preparation: Vec<LegacyPreparationCommand>,
    },
    AcceptStreaming {
        operation_id: String,
        application: ApplicationReference,
        entry_revision: String,
        certificate_id: String,
        preflight_token: String,
        policy_identity: PolicyIdentity,
        frame_limiter_settings: std::collections::BTreeMap<String, serde_json::Value>,
        client_frame_limiter_overrides: std::collections::BTreeMap<String, serde_json::Value>,
    },
    AdmitStreaming {
        operation_id: String,
        application: ApplicationReference,
        entry_revision: String,
        certificate_id: String,
        preflight_token: String,
        policy_identity: PolicyIdentity,
        display_admission_token: Option<String>,
        application_display_overrides: std::collections::BTreeMap<String, serde_json::Value>,
        application_display_settings: Option<serde_json::Value>,
        virtual_display_intent: Option<serde_json::Value>,
        frame_limiter_settings: std::collections::BTreeMap<String, serde_json::Value>,
        client_frame_limiter_overrides: std::collections::BTreeMap<String, serde_json::Value>,
        proton_dualsense_compatibility: bool,
        return_on_admission: bool,
    },
}
impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Prepare {
                operation_id,
                application,
                entry_revision,
            } if token(operation_id) && application.validate().is_ok() && token(entry_revision) => {
                Ok(())
            }
            Self::PrepareStreaming {
                operation_id,
                application,
                entry_revision,
                certificate_id,
                global_preparation,
                legacy_global_preparation,
            } if token(operation_id)
                && application.validate().is_ok()
                && token(entry_revision)
                && global_preparation.len() + legacy_global_preparation.len() <= 8
                && legacy_global_preparation.iter().all(|step| {
                    [&step.do_command, &step.undo]
                        .iter()
                        .all(|command| command.len() <= 2048 && !command.contains('\0'))
                })
                && certificate_id.len() == 64
                && certificate_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
            {
                Ok(())
            }
            Self::AcceptStreaming {
                operation_id,
                application,
                entry_revision,
                certificate_id,
                preflight_token,
                policy_identity,
                ..
            }
            | Self::AdmitStreaming {
                operation_id,
                application,
                entry_revision,
                certificate_id,
                preflight_token,
                policy_identity,
                ..
            } if token(operation_id)
                && application.validate().is_ok()
                && token(entry_revision)
                && certificate_id.len() == 64
                && certificate_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                && preflight_token.len() == 64
                && preflight_token
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                && policy_identity.validate().is_ok() =>
            {
                Ok(())
            }
            _ => Err("invalid_request"),
        }
    }
}

/// Operator-owned compatibility commands supplied by the trusted coordinator.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LegacyPreparationCommand {
    #[serde(default, rename = "do")]
    pub do_command: String,
    #[serde(default)]
    pub undo: String,
    #[serde(default)]
    pub elevated: bool,
}

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
        /// Logical startup-selected authority name, never a service address.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy_source: Option<String>,
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
                policy_source,
                snapshot_id,
            } if application.validate().is_ok()
                && token(entry_revision)
                && token(policy_revision)
                && policy_source
                    .as_ref()
                    .is_none_or(|source| super::v2::key(source))
                && token(snapshot_id) =>
            {
                Ok(())
            }
            _ => Err("invalid_policy_identity"),
        }
    }
}

/// Stable, source-qualified settings scope for a selected application. This is
/// a lookup key, not an authorization token; lifecycle still checks the owner.
pub fn application_scope_id(application: &ApplicationReference) -> Result<String, &'static str> {
    use sha2::{Digest, Sha256};
    application.validate()?;
    let bytes = serde_json::to_vec(application).map_err(|_| "invalid_application_reference")?;
    Ok(format!("library-{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preview_request_rejects_payload_policy_or_provider_injection() {
        let request = json!({"action":"prepare","operation_id":"op", "application":{"library":"games","id":"app"},"entry_revision":"opaque"});
        let parsed: Request = serde_json::from_value(request.clone()).unwrap();
        assert!(parsed.validate().is_ok());
        for field in ["policy", "service", "command", "client_id"] {
            let mut forged = request.clone();
            forged[field] = json!("injected");
            assert!(serde_json::from_value::<Request>(forged).is_err());
        }
    }

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
