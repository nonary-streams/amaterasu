//! Authenticated-client admission for a startup-selected streaming policy.
//!
//! Only a trusted protocol coordinator may assert a certificate ID. The broker
//! caller and initiator still need explicit grants; a payload is not proof of TLS
//! possession or current pairing permission. Security revocation is independent
//! of the ordinary policy snapshot returned here.
use super::policy::{Identity, MAX_POLICY_BYTES, Snapshot};
use super::v2::{ApplicationReference, key, token};
use crate::requests::IncomingRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub action: AdmitAction,
    pub application: ApplicationReference,
    pub entry_revision: String,
    /// SHA-256 fingerprint attested by the trusted paired-TLS coordinator.
    pub certificate_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AdmitAction {
    Admit,
}

impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.application.validate()?;
        if !token(&self.entry_revision)
            || self.certificate_id.len() != 64
            || !self
                .certificate_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid_request");
        }
        Ok(())
    }
}

/// The provider authorizes the certificate and returns the exact ordinary
/// execution and streaming settings for this operation. The coordinator validates
/// all settings and retains the accepted snapshot unchanged through admission.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Admission {
    pub certificate_id: String,
    pub policy: Snapshot,
    pub config_overrides: Value,
    pub application_display_settings: Option<Value>,
    /// Per-application opt-out from the operator's global preparation plan.
    #[serde(default)]
    pub exclude_global_preparation: bool,
}

impl Admission {
    pub fn validate_for(&self, request: &Request) -> Result<(), &'static str> {
        request.validate()?;
        let policy_request = super::policy::Request {
            action: super::policy::ResolveAction::Resolve,
            application: request.application.clone(),
            entry_revision: request.entry_revision.clone(),
        };
        if self.certificate_id != request.certificate_id
            || self.policy.validate_for(&policy_request).is_err()
            || !self.config_overrides.is_object()
            || self
                .application_display_settings
                .as_ref()
                .is_some_and(|value| !value.is_object())
            || !serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= MAX_POLICY_BYTES)
        {
            return Err("invalid_policy_admission");
        }
        Ok(())
    }
}

pub trait Provider {
    fn admit(
        &mut self,
        identity: &Identity<'_>,
        request: &Request,
    ) -> Result<Admission, &'static str>;
}

/// Routes and allowed broker identities are fixed by trusted startup settings.
pub struct Adapter<P> {
    provider: P,
    service: String,
    callers: BTreeMap<String, BTreeSet<String>>,
}

impl<P: Provider> Adapter<P> {
    pub fn new(
        provider: P,
        service: String,
        callers: BTreeMap<String, BTreeSet<String>>,
    ) -> Result<Self, &'static str> {
        if service.strip_suffix("/2").is_none_or(|name| !key(name))
            || callers.is_empty()
            || callers.len() > 32
            || callers.iter().any(|(caller, initiators)| {
                !key(caller)
                    || initiators.is_empty()
                    || initiators.len() > 32
                    || initiators.iter().any(|initiator| !key(initiator))
            })
        {
            return Err("invalid_policy_adapter");
        }
        Ok(Self {
            provider,
            service,
            callers,
        })
    }

    pub fn dispatch(&mut self, incoming: &IncomingRequest<Value>) -> Value {
        if incoming.service != self.service
            || !self
                .callers
                .get(&incoming.caller)
                .is_some_and(|initiators| initiators.contains(&incoming.initiator))
        {
            return json!({"ok":false,"error":{"code":"unauthorized"}});
        }
        if incoming.remaining_ms == 0
            || !serde_json::to_vec(&incoming.payload)
                .is_ok_and(|bytes| bytes.len() <= MAX_POLICY_BYTES)
        {
            return json!({"ok":false,"error":{"code":"invalid_request"}});
        }
        let Ok(request) = incoming.decode::<Request>() else {
            return json!({"ok":false,"error":{"code":"invalid_request"}});
        };
        if request.validate().is_err() {
            return json!({"ok":false,"error":{"code":"invalid_request"}});
        }
        let identity = Identity {
            caller: &incoming.caller,
            initiator: &incoming.initiator,
        };
        let Ok(admission) = self.provider.admit(&identity, &request) else {
            return json!({"ok":false,"error":{"code":"policy_unavailable"}});
        };
        if admission.validate_for(&request).is_err() {
            return json!({"ok":false,"error":{"code":"policy_unavailable"}});
        }
        let response = json!({"ok":true,"admission":admission});
        if serde_json::to_vec(&response).is_ok_and(|bytes| bytes.len() <= MAX_POLICY_BYTES) {
            response
        } else {
            json!({"ok":false,"error":{"code":"policy_unavailable"}})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application_library::policy::Snapshot;

    struct OwnerOnly;
    impl Provider for OwnerOnly {
        fn admit(
            &mut self,
            _: &Identity<'_>,
            request: &Request,
        ) -> Result<Admission, &'static str> {
            if request.certificate_id != "ab".repeat(32) {
                return Err("not_authorized");
            }
            Ok(Admission {
                certificate_id: request.certificate_id.clone(),
                policy: Snapshot {
                    application: request.application.clone(),
                    entry_revision: request.entry_revision.clone(),
                    policy_revision: "policy".into(),
                    working_directory: None,
                    exit_timeout_seconds: 10,
                    preparation: vec![],
                },
                config_overrides: json!({}),
                application_display_settings: None,
                exclude_global_preparation: false,
            })
        }
    }

    #[test]
    fn selected_policy_adapter_checks_broker_identities_and_certificate() {
        let callers = BTreeMap::from([(
            "amaterasu.launch".into(),
            BTreeSet::from(["amaterasu.nvhttp".into()]),
        )]);
        let mut adapter = Adapter::new(OwnerOnly, "selected.policy/2".into(), callers).unwrap();
        let mut incoming = IncomingRequest {
            id: 1,
            caller: "amaterasu.launch".into(),
            initiator: "amaterasu.nvhttp".into(),
            service: "selected.policy/2".into(),
            remaining_ms: 1000,
            payload: json!({"action":"admit","application":{"library":"games","id":"opaque"},
                "entry_revision":"entry","certificate_id":"ab".repeat(32)}),
        };
        assert_eq!(adapter.dispatch(&incoming)["ok"], true);
        incoming.initiator = "other".into();
        assert_eq!(adapter.dispatch(&incoming)["error"]["code"], "unauthorized");
        incoming.initiator = "amaterasu.nvhttp".into();
        incoming.payload["certificate_id"] = json!("cd".repeat(32));
        assert_eq!(adapter.dispatch(&incoming)["ok"], false);
    }
}
