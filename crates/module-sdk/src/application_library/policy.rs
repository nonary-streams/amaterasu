//! Versioned private policy resolution for selected application libraries.
//!
//! This service returns administrator-owned command policy only to explicitly
//! authorized coordinators. The broker supplies the caller and initiator; a
//! request cannot choose its provider or establish user/device authority. A
//! response is a snapshot, not proof that it remains current indefinitely.
pub use super::provider_adapter::Identity;
use super::v2::{ApplicationReference, token};
use crate::requests::IncomingRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_POLICY_BYTES: usize = 12_000;
pub const MAX_PREPARATION_STEPS: usize = 8;
pub const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 10_000;

fn default_command_timeout_ms() -> u64 {
    DEFAULT_COMMAND_TIMEOUT_MS
}
fn default_exit_timeout_seconds() -> u64 {
    10
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_command_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PreparationStep {
    pub run: CommandSpec,
    #[serde(default)]
    pub undo: Option<CommandSpec>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub action: ResolveAction,
    pub application: ApplicationReference,
    pub entry_revision: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ResolveAction {
    Resolve,
}

impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.application.validate()?;
        if token(&self.entry_revision) {
            Ok(())
        } else {
            Err("invalid_request")
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub application: ApplicationReference,
    pub entry_revision: String,
    /// Opaque equality token owned by the selected policy provider.
    pub policy_revision: String,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default = "default_exit_timeout_seconds")]
    pub exit_timeout_seconds: u64,
    #[serde(default)]
    pub preparation: Vec<PreparationStep>,
}

impl Snapshot {
    /// Check identity and wire bounds. The coordinator must additionally
    /// validate platform paths, command semantics, policy trust and authority.
    pub fn validate_for(&self, request: &Request) -> Result<(), &'static str> {
        request.validate()?;
        if self.application != request.application
            || self.entry_revision != request.entry_revision
            || !token(&self.policy_revision)
            || self.preparation.len() > MAX_PREPARATION_STEPS
            || self.exit_timeout_seconds == 0
            || self.exit_timeout_seconds > 300
            || !serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= MAX_POLICY_BYTES)
        {
            return Err("invalid_policy_snapshot");
        }
        Ok(())
    }
}

pub trait PolicyProvider {
    fn resolve(
        &mut self,
        identity: &Identity<'_>,
        request: &Request,
    ) -> Result<Snapshot, &'static str>;
}

/// The trusted startup configuration fixes the service and the broker callers
/// and initiators. Implementing this adapter does not grant any authority.
pub struct PolicyAdapter<P> {
    provider: P,
    service: String,
    callers: BTreeMap<String, BTreeSet<String>>,
}

impl<P: PolicyProvider> PolicyAdapter<P> {
    pub fn new(
        provider: P,
        service: String,
        callers: BTreeMap<String, BTreeSet<String>>,
    ) -> Result<Self, &'static str> {
        if service
            .strip_suffix("/1")
            .is_none_or(|name| !super::v2::key(name))
            || callers.is_empty()
            || callers.len() > 32
            || callers.iter().any(|(caller, initiators)| {
                !super::v2::key(caller)
                    || initiators.is_empty()
                    || initiators.len() > 32
                    || initiators
                        .iter()
                        .any(|initiator| !super::v2::key(initiator))
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
                .is_some_and(|set| set.contains(&incoming.initiator))
        {
            return json!({"ok":false,"error":{"code":"unauthorized"}});
        }
        if incoming.remaining_ms == 0
            || !serde_json::to_vec(&incoming.payload).is_ok_and(|b| b.len() <= MAX_POLICY_BYTES)
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
        let response = match self.provider.resolve(&identity, &request) {
            Ok(snapshot) if snapshot.validate_for(&request).is_ok() => {
                json!({"ok":true,"policy":snapshot})
            }
            _ => return json!({"ok":false,"error":{"code":"policy_unavailable"}}),
        };
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

    struct Provider;
    impl PolicyProvider for Provider {
        fn resolve(
            &mut self,
            _: &Identity<'_>,
            request: &Request,
        ) -> Result<Snapshot, &'static str> {
            Ok(Snapshot {
                application: request.application.clone(),
                entry_revision: request.entry_revision.clone(),
                policy_revision: "policy-v1".into(),
                working_directory: None,
                exit_timeout_seconds: 10,
                preparation: vec![],
            })
        }
    }

    fn incoming(caller: &str, initiator: &str) -> IncomingRequest<Value> {
        IncomingRequest {
            id: 1,
            caller: caller.into(),
            initiator: initiator.into(),
            service: "policy/1".into(),
            remaining_ms: 1000,
            payload: json!({"action":"resolve","application":{"library":"games","id":"opaque"},"entry_revision":"entry-v1"}),
        }
    }

    #[test]
    fn resolver_requires_exact_broker_identities_and_preserves_opaque_revisions() {
        let callers = BTreeMap::from([("launch".into(), BTreeSet::from(["consumer".into()]))]);
        let mut adapter = PolicyAdapter::new(Provider, "policy/1".into(), callers).unwrap();
        let response = adapter.dispatch(&incoming("launch", "consumer"));
        assert_eq!(response["policy"]["entry_revision"], "entry-v1");
        assert_eq!(response["policy"]["policy_revision"], "policy-v1");
        assert_eq!(
            adapter.dispatch(&incoming("foreign", "consumer"))["error"]["code"],
            "unauthorized"
        );
        assert_eq!(
            adapter.dispatch(&incoming("launch", "foreign"))["error"]["code"],
            "unauthorized"
        );
        let mut invalid = incoming("launch", "consumer");
        invalid.payload["entry_revision"] = json!("");
        assert_eq!(
            adapter.dispatch(&invalid)["error"]["code"],
            "invalid_request"
        );
    }

    #[test]
    fn snapshot_rejects_changed_identity_and_unbounded_steps() {
        let request: Request =
            serde_json::from_value(incoming("launch", "consumer").payload).unwrap();
        let mut snapshot = Provider
            .resolve(
                &Identity {
                    caller: "launch",
                    initiator: "consumer",
                },
                &request,
            )
            .unwrap();
        assert!(snapshot.validate_for(&request).is_ok());
        snapshot.entry_revision = "different".into();
        assert!(snapshot.validate_for(&request).is_err());
        snapshot.entry_revision = request.entry_revision.clone();
        snapshot.policy_revision = String::new();
        assert!(snapshot.validate_for(&request).is_err());
        snapshot.policy_revision = "policy-v1".into();
        let command = CommandSpec {
            executable: "/fixture".into(),
            args: vec![],
            environment: BTreeMap::new(),
            cwd: None,
            timeout_ms: 1,
        };
        snapshot.preparation = vec![
            PreparationStep {
                run: command,
                undo: None
            };
            MAX_PREPARATION_STEPS + 1
        ];
        assert!(snapshot.validate_for(&request).is_err());
    }
}
