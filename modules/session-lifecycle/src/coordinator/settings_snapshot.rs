//! Bounded provider settings snapshots, read before session and stream effects.
use crate::adapter::ParticipantSettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const MAX_PROVIDERS: usize = 32;
const MAX_SETTINGS_BYTES: usize = 8192;
const MAX_REGISTRY_RESPONSE_BYTES: usize = 12_000;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct SnapshotScopes {
    application: Option<String>,
    paired_client: Option<String>,
    session: Option<String>,
}

impl SnapshotScopes {
    /// Shared resource policy may come from the source snapshot, while identity
    /// scopes must continue to identify this newly authenticated client.
    pub(super) fn for_joined_client(&self, value: Option<&Value>) -> ModuleResult<Self> {
        let mut scopes = self.clone();
        scopes.paired_client = match value.filter(|value| !value.is_null()) {
            None => None,
            Some(value) => {
                let client: amaterasu_extension_api::ClientContext =
                    serde_json::from_value(value.clone()).map_err(|_| "invalid_client_context")?;
                if !client.validate() {
                    return Err("invalid_client_context".into());
                }
                client.client_ref
            }
        };
        Ok(scopes)
    }
    /// Select from the admitted transition, never from provider-supplied data.
    pub(super) fn from_work(journal: &super::Journal, work: &super::Work) -> ModuleResult<Self> {
        let (_, session_id, _, _) = super::operations::target_parts(&work.target)?;
        let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
        super::operations::checked_target(
            journal,
            &work.target,
            &session.owner.caller,
            &session.owner.initiator,
        )?;
        let application = (session.application_ref["kind"] == "application")
            .then(|| {
                session.application_ref["app_id"]
                    .as_str()
                    .map(str::to_owned)
            })
            .flatten();
        let mut receipts = journal.receipts.values().filter(|receipt| {
            receipt.work.as_ref().is_some_and(|pending| {
                pending.target == work.target && pending.transition_id == work.transition_id
            })
        });
        let client = receipts
            .next()
            .and_then(|receipt| receipt.request.get("client_context"));
        if receipts.next().is_some() {
            return Err("ambiguous_settings_context".into());
        }
        let paired_client = match client {
            None | Some(Value::Null) => None,
            Some(value) => {
                let client: amaterasu_extension_api::ClientContext =
                    serde_json::from_value(value.clone()).map_err(|_| "invalid_client_context")?;
                if !client.validate() {
                    return Err("invalid_client_context".into());
                }
                client.client_ref
            }
        };
        Ok(Self {
            application,
            paired_client,
            session: Some(session_id.into()),
        })
    }

    pub(super) fn valid_for_session(&self, session: &str) -> bool {
        self.valid() && self.session.as_deref() == Some(session)
    }

    fn valid(&self) -> bool {
        [&self.application, &self.paired_client, &self.session]
            .into_iter()
            .all(|id| {
                id.as_deref().is_none_or(|id| {
                    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
                })
            })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FrozenSettings {
    pub settings: Value,
    pub revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) contract: Option<FrozenParticipantContract>,
}

/// Version 1 binds the complete installed participant contract. Settings values
/// and their revision are frozen separately, so normalize only those two fields.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct FrozenParticipantContract {
    pub(super) version: u32,
    pub(super) digest: String,
}

impl FrozenParticipantContract {
    pub(super) fn from_participant(participant: &ParticipantSettings) -> ModuleResult<Self> {
        let mut normalized = participant.clone();
        normalized.settings = json!({});
        normalized.settings_revision.clear();
        let bytes = serde_json::to_vec(&normalized).map_err(|_| "invalid_provider_contract")?;
        Ok(Self {
            version: 1,
            digest: format!("{:x}", Sha256::digest(bytes)),
        })
    }

    pub(super) fn matches_participant(&self, participant: &ParticipantSettings) -> bool {
        self.version == 1
            && self.digest.len() == 64
            && self
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && Self::from_participant(participant).is_ok_and(|current| current == *self)
    }
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
        live_options: Vec<String>,
        descriptor_revision: Option<u64>,
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
    scopes: SnapshotScopes,
    live_options: BTreeMap<String, Vec<String>>,
    descriptor_revisions: BTreeMap<String, u64>,
}

impl SettingsSnapshotJob {
    pub(super) fn new(
        service: Option<&str>,
        participants: &[ParticipantSettings],
        scopes: SnapshotScopes,
    ) -> ModuleResult<Self> {
        if participants.len() > MAX_PROVIDERS
            || service.is_some_and(str::is_empty)
            || !scopes.valid()
        {
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
                    Ok((
                        p.provider.clone(),
                        FrozenSettings {
                            settings: p.settings.clone(),
                            revision: p.settings_revision.clone(),
                            contract: Some(FrozenParticipantContract::from_participant(p)?),
                        },
                    ))
                })
                .collect::<ModuleResult<BTreeMap<_, _>>>()?
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
            scopes,
            live_options: BTreeMap::new(),
            descriptor_revisions: BTreeMap::new(),
        })
    }

    pub(super) fn all_live_options(&self) -> BTreeMap<String, Vec<String>> {
        self.live_options.clone()
    }

    pub(super) fn descriptor_revision(&self, provider: &str) -> Option<u64> {
        self.descriptor_revisions.get(provider).copied()
    }

    pub(super) fn live_options(&self, provider: &str) -> Option<&[String]> {
        self.live_options.get(provider).map(Vec::as_slice)
    }

    pub(super) fn cancel(&self, send: &mut dyn FnMut(Command) -> ModuleResult<Response>) {
        if let Stage::Poll { request }
        | Stage::Release { request, .. }
        | Stage::ReleaseFailure { request, .. } = self.stage
        {
            let _ = send(Command::Cancel { request });
            let _ = send(Command::ReleaseResult { request });
        }
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
                if let Stage::Poll { request }
                | Stage::Release { request, .. }
                | Stage::ReleaseFailure { request, .. } = self.stage
                {
                    let _ = send(Command::Cancel { request });
                    let _ = send(Command::ReleaseResult { request });
                }
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
                    payload: json!({"operation":"settings_get", "provider":self.participants[self.index].provider, "scope":{"kind":"host"}, "application":self.scopes.application, "paired_client":self.scopes.paired_client, "session":self.scopes.session}),
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
                        Ok((snapshot, live_options, descriptor_revision)) => Stage::Release {
                            request: *request,
                            snapshot,
                            live_options,
                            descriptor_revision,
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
            Stage::Release {
                request,
                snapshot,
                live_options,
                descriptor_revision,
            } => {
                // The registry supplies values, never the installed method or
                // capability identity that will govern later native cleanup.
                let mut snapshot = snapshot.clone();
                snapshot.contract = Some(FrozenParticipantContract::from_participant(
                    &self.participants[self.index],
                )?);
                match send(Command::ReleaseResult { request: *request })? {
                    Response::Ok => {}
                    _ => return Err("settings registry result release failed".into()),
                }
                if let Some(revision) = descriptor_revision {
                    self.descriptor_revisions
                        .insert(self.participants[self.index].provider.clone(), *revision);
                }
                self.live_options.insert(
                    self.participants[self.index].provider.clone(),
                    live_options.clone(),
                );
                self.snapshots
                    .insert(self.participants[self.index].provider.clone(), snapshot);
                self.index += 1;
                self.stage = Stage::Submit;
            }
            Stage::ReleaseFailure { request, error } => {
                match send(Command::ReleaseResult { request: *request })? {
                    Response::Ok => {
                        let error = error.clone();
                        self.stage = Stage::Failed;
                        return Err(error);
                    }
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

fn parse_registry_response(
    payload: Value,
) -> ModuleResult<(FrozenSettings, Vec<String>, Option<u64>)> {
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
        contract: None,
    };
    if !valid_snapshot(&snapshot.settings, &snapshot.revision) {
        return Err("invalid effective settings".into());
    }
    let live_options = match payload.get("live_options") {
        None => Vec::new(),
        Some(value) => serde_json::from_value::<Vec<String>>(value.clone())
            .map_err(|_| "invalid live options")?,
    };
    let mut seen = BTreeSet::new();
    if live_options.len() > 128
        || live_options
            .iter()
            .any(|name| !effective.contains_key(name) || !seen.insert(name))
    {
        return Err("invalid live options".into());
    }
    let descriptor_revision = payload.get("descriptor_revision").and_then(Value::as_u64);
    Ok((snapshot, live_options, descriptor_revision))
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
    fn direct_settings_freeze_selected_participant_contract_identity() {
        let participant = participant("provider");
        let mut job =
            SettingsSnapshotJob::new(None, &[participant], SnapshotScopes::default()).unwrap();
        let snapshots = job
            .advance(&mut |_| panic!("direct snapshot needs no broker"))
            .unwrap()
            .unwrap();
        let snapshot = serde_json::to_value(&snapshots["provider"]).unwrap();
        assert_eq!(snapshot["contract"]["version"], 1);
        assert_eq!(snapshot["contract"]["digest"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn registry_settings_cannot_supply_the_selected_contract_identity() {
        let participant = participant("provider");
        let mut direct = SettingsSnapshotJob::new(
            None,
            std::slice::from_ref(&participant),
            SnapshotScopes::default(),
        )
        .unwrap();
        let expected =
            serde_json::to_value(direct.advance(&mut |_| unreachable!()).unwrap().unwrap())
                .unwrap();
        let mut registry = SettingsSnapshotJob::new(
            Some("registry/1"),
            &[participant],
            SnapshotScopes::default(),
        )
        .unwrap();
        let mut send = |command| match command {
            Command::Request { .. } => Ok(Response::Accepted { id: 8 }),
            Command::TakeResult { request: 8 } => Ok(Response::Result {
                payload: json!({"ok":true,"status":"ok","revision":9,
                    "effective":{"x":{"value":2}},
                    "contract":{"version":1,"digest":"a".repeat(64)}}),
            }),
            Command::ReleaseResult { request: 8 } => Ok(Response::Ok),
            _ => panic!("unexpected registry command"),
        };
        assert!(registry.advance(&mut send).unwrap().is_none());
        assert!(registry.advance(&mut send).unwrap().is_none());
        let snapshots = registry.advance(&mut send).unwrap().unwrap();
        let snapshot = serde_json::to_value(&snapshots["provider"]).unwrap();
        assert_eq!(snapshot["contract"]["version"], 1);
        assert_eq!(snapshot["contract"], expected["provider"]["contract"]);
        assert_ne!(snapshot["contract"]["digest"], "a".repeat(64));
        assert_eq!(snapshot["settings"]["x"], 2);
    }

    #[test]
    fn contract_identity_covers_all_participant_fields_except_frozen_values() {
        let original = participant("provider");
        let identity = FrozenParticipantContract::from_participant(&original).unwrap();
        assert!(serde_json::to_vec(&identity).unwrap().len() <= 100);
        let mut values = original.clone();
        values.settings = json!({"different":"values"});
        values.settings_revision = "different-revision".into();
        assert!(identity.matches_participant(&values));
        for field in [
            "provider",
            "version",
            "dependencies",
            "plan_service",
            "plan_method",
            "interface_major",
            "interface_minor",
            "schema_revision",
            "callback",
            "status",
            "binding_validation",
            "capabilities",
            "cleanup_methods",
        ] {
            let mut changed = original.clone();
            match field {
                "provider" => changed.provider = "replacement".into(),
                "version" => changed.contract_version += 1,
                "dependencies" => changed.depends_on.push("dependency".into()),
                "plan_service" => changed.plan_method.service = "replacement/1".into(),
                "plan_method" => changed.plan_method.method = "replacement".into(),
                "interface_major" => changed.plan_method.interface_major += 1,
                "interface_minor" => changed.plan_method.interface_minor += 1,
                "schema_revision" => changed.plan_method.schema_revision += 1,
                "callback" => changed.callback_method.method = "replacement".into(),
                "status" => changed.status_method.method = "replacement".into(),
                "binding_validation" => {
                    changed.binding_validate_method = Some(changed.plan_method.clone())
                }
                "capabilities" => {
                    changed.capabilities.insert("new-capability".into());
                }
                "cleanup_methods" => changed.cleanup_methods.push(
                    serde_json::from_value(serde_json::to_value(&changed.callback_method).unwrap())
                        .unwrap(),
                ),
                _ => unreachable!(),
            }
            assert!(!identity.matches_participant(&changed), "changed {field}");
        }
    }

    #[test]
    fn malformed_or_unknown_contract_identity_never_matches_a_participant() {
        let participant = participant("provider");
        let identity = FrozenParticipantContract::from_participant(&participant).unwrap();
        assert!(identity.matches_participant(&participant));
        for version in [0, 2, u32::MAX] {
            let mut unknown = identity.clone();
            unknown.version = version;
            assert!(!unknown.matches_participant(&participant));
        }
        for digest in [
            String::new(),
            "a".repeat(63),
            "a".repeat(65),
            "g".repeat(64),
            "A".repeat(64),
        ] {
            let mut malformed = identity.clone();
            malformed.digest = digest;
            assert!(!malformed.matches_participant(&participant));
        }
        let legacy: FrozenSettings = serde_json::from_value(json!({
            "settings":{"x":1}, "revision":"legacy"
        }))
        .unwrap();
        assert!(legacy.contract.is_none());
        assert!(
            serde_json::from_value::<FrozenParticipantContract>(json!({
                "version":1,"digest":identity.digest,"replacement":true
            }))
            .is_err()
        );
    }

    #[test]
    fn admitted_context_selects_scopes_and_request_waits_for_result_release() {
        let root = std::env::temp_dir().join(format!(
            "limiter-scopes-{}-{}",
            std::process::id(),
            amaterasu_storage::random_identifier().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = super::super::Coordinator::open(&root.join("journal.json")).unwrap();
        let admitted = coordinator.handle(
            "transport",
            "client",
            json!({
                "operation":"prepare","operation_id":"prepare",
                "application_ref":{"kind":"application","app_id":"game"},
                "client_context":{"client_ref":"paired-client","display_mode":null}
            }),
        );
        assert_eq!(admitted["status"], "pending");
        let journal = coordinator.shared.borrow();
        let work = journal
            .value
            .receipts
            .values()
            .find_map(|receipt| receipt.work.as_ref())
            .unwrap();
        let scopes = SnapshotScopes::from_work(&journal.value, work).unwrap();
        assert_eq!(scopes.application.as_deref(), Some("game"));
        assert_eq!(scopes.paired_client.as_deref(), Some("paired-client"));
        assert_eq!(scopes.session.as_deref(), admitted["session_id"].as_str());
        let mut invalid = work.clone();
        invalid.target = serde_json::from_value(json!({"kind":"session","host_epoch":"foreign","session_id":admitted["session_id"],"generation":1})).unwrap();
        assert!(SnapshotScopes::from_work(&journal.value, &invalid).is_err());
        let mut resumed = journal.value.clone();
        let mut receipt = resumed
            .receipts
            .values()
            .find(|receipt| receipt.work.is_some())
            .unwrap()
            .clone();
        let session_id = admitted["session_id"].as_str().unwrap();
        resumed.sessions.get_mut(session_id).unwrap().streams.insert("stream".into(), serde_json::from_value(json!({
            "generation":2,"phase":"preparing","candidate":null,"required":[],"acknowledgements":[],"retirements":[]
        })).unwrap());
        receipt.request["client_context"]["client_ref"] = json!("resumed-client");
        let work = receipt.work.as_mut().unwrap();
        work.transition_id = "resume".into();
        work.target =
            serde_json::from_value(json!({"kind":"stream","host_epoch":admitted["host_epoch"],
            "session_id":session_id,"stream_id":"stream","generation":2}))
            .unwrap();
        let stream_work = work.clone();
        resumed.receipts.insert("resumed".into(), receipt);
        let stream_scopes = SnapshotScopes::from_work(&resumed, &stream_work).unwrap();
        assert_eq!(
            stream_scopes.paired_client.as_deref(),
            Some("resumed-client")
        );
        assert_eq!(stream_scopes.application, scopes.application);
        assert_eq!(stream_scopes.session, scopes.session);
        assert_eq!(scopes.paired_client.as_deref(), Some("paired-client"));
        drop(journal);
        let mut job =
            SettingsSnapshotJob::new(Some("registry/1"), &[participant("limiter")], scopes)
                .unwrap();
        let mut commands = Vec::new();
        let mut replies = VecDeque::from([
            Response::Accepted { id: 1 },
            registry_result(json!(false), 9),
            Response::Ok,
        ]);
        let mut send = |command| {
            commands.push(command);
            Ok(replies.pop_front().unwrap())
        };
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(job.advance(&mut send).unwrap().is_none());
        let result = job.advance(&mut send).unwrap().unwrap();
        assert_eq!(result["limiter"].settings, json!({"x":false}));
        assert!(
            matches!(&commands[0], Command::Request{payload,parent:None,..}
            if payload["application"] == "game" && payload["paired_client"] == "paired-client"
                && payload["session"] == admitted["session_id"])
        );
        assert!(matches!(commands[2], Command::ReleaseResult { request: 1 }));
        drop(coordinator);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn two_providers_are_frozen_only_after_every_result_is_released() {
        let mut job = SettingsSnapshotJob::new(
            Some("amaterasu.extension-registry/1"),
            &[participant("a"), participant("b")],
            SnapshotScopes::default(),
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
            let mut job = SettingsSnapshotJob::new(
                Some("registry/1"),
                &[participant("a")],
                SnapshotScopes::default(),
            )
            .unwrap();
            let mut replies = VecDeque::from([Response::Accepted { id: 1 }, result, Response::Ok]);
            let mut send = |_: Command| Ok(replies.pop_front().unwrap_or(Response::Ok));
            assert!(job.advance(&mut send).unwrap().is_none());
            assert!(job.advance(&mut send).unwrap().is_none());
            assert!(job.advance(&mut send).is_err());
            assert!(job.advance(&mut send).is_err());
        }
    }
    #[test]
    fn failed_release_fences_and_disabled_registry_uses_installed_baseline() {
        let mut job = SettingsSnapshotJob::new(
            Some("registry/1"),
            &[participant("a")],
            SnapshotScopes::default(),
        )
        .unwrap();
        let mut replies = VecDeque::from([
            Response::Accepted { id: 1 },
            registry_result(json!(3), 1),
            Response::error("permission_denied", "failed"),
        ]);
        let mut send = |_: Command| Ok(replies.pop_front().unwrap_or(Response::Ok));
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(job.advance(&mut send).is_err());
        assert!(job.advance(&mut send).is_err());
        let mut baseline =
            SettingsSnapshotJob::new(None, &[participant("a")], SnapshotScopes::default()).unwrap();
        let result = baseline
            .advance(&mut |_| panic!("baseline should not call broker"))
            .unwrap()
            .unwrap();
        assert_eq!(result["a"].revision, "installed");
        assert_eq!(result["a"].settings, json!({"x":1}));
        assert!(
            SettingsSnapshotJob::new(
                None,
                &[participant("a"), participant("a")],
                SnapshotScopes::default()
            )
            .is_err()
        );
    }
}
