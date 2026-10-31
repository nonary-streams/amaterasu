//! Bounded host-scope provider settings snapshot, read before session effects.
use crate::adapter::ParticipantSettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_PROVIDERS: usize = 32;
const MAX_SETTINGS_BYTES: usize = 8192;
const MAX_REGISTRY_RESPONSE_BYTES: usize = 12_000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FrozenSettings {
    pub settings: Value,
    pub revision: String,
}

#[derive(Debug)]
enum Stage {
    Submit,
    Poll {
        request: u64,
    },
    Release {
        request: u64,
        snapshot: FrozenSettings,
    },
    ReleaseFailure {
        request: u64,
        error: String,
    },
    Failed,
}

/// A separate job per logical session. Nothing is exposed until every selected
/// provider's settings are read and its broker result has been released.
pub struct SettingsSnapshotJob {
    service: Option<String>,
    participants: Vec<ParticipantSettings>,
    index: usize,
    snapshots: BTreeMap<String, FrozenSettings>,
    stage: Stage,
}

impl SettingsSnapshotJob {
    pub fn new(service: Option<&str>, participants: &[ParticipantSettings]) -> ModuleResult<Self> {
        if participants.len() > MAX_PROVIDERS || service.is_some_and(str::is_empty) {
            return Err("invalid settings snapshot configuration".into());
        }
        let mut seen = BTreeSet::new();
        for participant in participants {
            if participant.provider.is_empty()
                || !seen.insert(&participant.provider)
                || !valid_snapshot(&participant.settings, &participant.settings_revision)
            {
                return Err("invalid settings snapshot participant".into());
            }
        }
        let snapshots = if service.is_none() {
            participants
                .iter()
                .map(|p| {
                    (
                        p.provider.clone(),
                        FrozenSettings {
                            settings: p.settings.clone(),
                            revision: p.settings_revision.clone(),
                        },
                    )
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        let index = if service.is_none() {
            participants.len()
        } else {
            0
        };
        Ok(Self {
            service: service.map(str::to_owned),
            participants: participants.to_vec(),
            index,
            snapshots,
            stage: Stage::Submit,
        })
    }

    /// One broker command at most per tick. Any ambiguous/failed read or release
    /// fails this job closed; no participant data is returned on partial success.
    pub fn advance(
        &mut self,
        send: &mut (impl FnMut(Command) -> ModuleResult<Response> + ?Sized),
    ) -> ModuleResult<Option<BTreeMap<String, FrozenSettings>>> {
        if matches!(self.stage, Stage::Failed) {
            return Err("settings snapshot failed".into());
        }
        if self.index == self.participants.len() {
            return Ok(Some(self.snapshots.clone()));
        }
        match self.step(send) {
            Ok(()) if self.index == self.participants.len() => Ok(Some(self.snapshots.clone())),
            Ok(()) => Ok(None),
            Err(error) => {
                self.stage = Stage::Failed;
                Err(error)
            }
        }
    }

    fn step(
        &mut self,
        send: &mut (impl FnMut(Command) -> ModuleResult<Response> + ?Sized),
    ) -> ModuleResult<()> {
        match &self.stage {
            Stage::Submit => {
                let operation_id = amaterasu_storage::random_identifier()
                    .map_err(|_| "settings snapshot entropy unavailable")?;
                let command = Command::Request {
                    service: self.service.clone().ok_or("registry service unavailable")?,
                    payload: json!({"operation":"settings_get", "provider":self.participants[self.index].provider, "scope":{"kind":"host"}, "application":null, "paired_client":null, "session":null}),
                    timeout_ms: 5000,
                    operation_id,
                    parent: None,
                };
                match send(command)? {
                    Response::Accepted { id } => self.stage = Stage::Poll { request: id },
                    _ => return Err("settings registry request unavailable".into()),
                }
            }
            Stage::Poll { request } => match send(Command::TakeResult { request: *request })? {
                Response::Pending => {}
                Response::Result { payload } => {
                    self.stage = match parse_registry_response(payload) {
                        Ok(snapshot) => Stage::Release {
                            request: *request,
                            snapshot,
                        },
                        Err(error) => Stage::ReleaseFailure {
                            request: *request,
                            error,
                        },
                    };
                }
                Response::Error { code, .. }
                    if code == "deadline_exceeded" || code == "cancelled" =>
                {
                    self.stage = Stage::ReleaseFailure {
                        request: *request,
                        error: "settings registry result unavailable".into(),
                    };
                }
                _ => return Err("settings registry result unavailable".into()),
            },
            Stage::Release { request, snapshot } => {
                match send(Command::ReleaseResult { request: *request })? {
                    Response::Ok => {}
                    _ => return Err("settings registry result release failed".into()),
                }
                self.snapshots.insert(
                    self.participants[self.index].provider.clone(),
                    snapshot.clone(),
                );
                self.index += 1;
                self.stage = Stage::Submit;
            }
            Stage::ReleaseFailure { request, error } => {
                match send(Command::ReleaseResult { request: *request })? {
                    Response::Ok => return Err(error.clone()),
                    _ => return Err("settings registry result release failed".into()),
                }
            }
            Stage::Failed => return Err("settings snapshot failed".into()),
        }
        Ok(())
    }
}

fn valid_snapshot(settings: &Value, revision: &str) -> bool {
    settings.is_object()
        && !revision.is_empty()
        && revision.len() <= 128
        && !revision.chars().any(char::is_control)
        && serde_json::to_vec(settings).is_ok_and(|bytes| bytes.len() <= MAX_SETTINGS_BYTES)
}

fn parse_registry_response(payload: Value) -> ModuleResult<FrozenSettings> {
    if serde_json::to_vec(&payload)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_REGISTRY_RESPONSE_BYTES
        || payload.get("ok").and_then(Value::as_bool) != Some(true)
        || payload.get("status").and_then(Value::as_str) != Some("ok")
    {
        return Err("invalid settings registry response".into());
    }
    let revision = payload
        .get("revision")
        .and_then(Value::as_u64)
        .ok_or("missing registry revision")?;
    let effective = payload
        .get("effective")
        .and_then(Value::as_object)
        .ok_or("missing effective settings")?;
    let mut values = serde_json::Map::new();
    for (name, entry) in effective {
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return Err("invalid settings name".into());
        }
        values.insert(
            name.clone(),
            entry
                .as_object()
                .and_then(|entry| entry.get("value"))
                .ok_or("missing effective value")?
                .clone(),
        );
    }
    let snapshot = FrozenSettings {
        settings: Value::Object(values),
        revision: format!("registry-{revision}"),
    };
    if !valid_snapshot(&snapshot.settings, &snapshot.revision) {
        return Err("invalid effective settings".into());
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn participant(provider: &str) -> ParticipantSettings {
        let method = json!({"service":"provider/1","method":"prepare","interface_major":1,"interface_minor":0,"schema_revision":1});
        serde_json::from_value(json!({"provider":provider,"plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"installed","settings":{"x":1}})).unwrap()
    }
    fn registry_result(value: Value, revision: u64) -> Response {
        Response::Result {
            payload: json!({"ok":true,"status":"ok","revision":revision,"values":{},"effective":{"x":{"value":value,"source":{"kind":"host"}}}}),
        }
    }
    #[test]
    fn two_providers_are_frozen_only_after_every_result_is_released() {
        let mut job = SettingsSnapshotJob::new(
            Some("amaterasu.extension-registry/1"),
            &[participant("a"), participant("b")],
        )
        .unwrap();
        let mut replies = VecDeque::from([
            Response::Accepted { id: 1 },
            Response::Pending,
            registry_result(json!(7), 4),
            Response::Ok,
            Response::Accepted { id: 2 },
            registry_result(json!(9), 5),
            Response::Ok,
        ]);
        let mut commands = Vec::new();
        let mut send = |command| {
            commands.push(command);
            Ok(replies.pop_front().unwrap())
        };
        for _ in 0..6 {
            assert!(job.advance(&mut send).unwrap().is_none());
        }
        let map = job.advance(&mut send).unwrap().unwrap();
        assert_eq!(map["a"].revision, "registry-4");
        assert_eq!(map["a"].settings, json!({"x":7}));
        assert_eq!(map["b"].settings, json!({"x":9}));
        assert!(
            matches!(&commands[0], Command::Request { payload, parent:None, .. } if payload["scope"] == json!({"kind":"host"}) && payload["provider"] == "a")
        );
        assert!(
            matches!(&commands[4], Command::Request { payload, .. } if payload["provider"] == "b")
        );
    }
    #[test]
    fn failed_or_malformed_result_never_falls_back_to_installed_settings() {
        for result in [
            registry_result(json!("x".repeat(9000)), 0),
            Response::Result {
                payload: json!({"ok":true,"status":"ok","revision":1,"effective":{"x":{}}}),
            },
            Response::error("deadline_exceeded", "expired"),
        ] {
            let mut job =
                SettingsSnapshotJob::new(Some("registry/1"), &[participant("a")]).unwrap();
            let mut replies = VecDeque::from([Response::Accepted { id: 1 }, result, Response::Ok]);
            let mut send = |_: Command| Ok(replies.pop_front().unwrap());
            assert!(job.advance(&mut send).unwrap().is_none());
            assert!(job.advance(&mut send).unwrap().is_none());
            assert!(job.advance(&mut send).is_err());
            assert!(job.advance(&mut send).is_err());
        }
    }
    #[test]
    fn failed_release_fences_and_disabled_registry_uses_installed_baseline() {
        let mut job = SettingsSnapshotJob::new(Some("registry/1"), &[participant("a")]).unwrap();
        let mut replies = VecDeque::from([
            Response::Accepted { id: 1 },
            registry_result(json!(3), 1),
            Response::error("permission_denied", "failed"),
        ]);
        let mut send = |_: Command| Ok(replies.pop_front().unwrap());
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(job.advance(&mut send).is_err());
        assert!(job.advance(&mut send).is_err());
        let mut baseline = SettingsSnapshotJob::new(None, &[participant("a")]).unwrap();
        let result = baseline
            .advance(&mut |_| panic!("baseline should not call broker"))
            .unwrap()
            .unwrap();
        assert_eq!(result["a"].revision, "installed");
        assert_eq!(result["a"].settings, json!({"x":1}));
        assert!(SettingsSnapshotJob::new(None, &[participant("a"), participant("a")]).is_err());
    }
}
