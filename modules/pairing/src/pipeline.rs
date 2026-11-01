mod configuration_hold;

use crate::effect_gate::{EffectGate, Request as EffectRequest, Source as EffectSource};
use crate::engine::{Change, Engine};
use crate::identity::{
    ConfiguredIdentity, IMPORTED_IDENTITY_MIGRATION_KEY, ImportedIdentityReconciliation, Shared,
    Snapshot,
};
use crate::identity_generation::{Generator, IdentityWorker};
use crate::protocol::{Document, EffectAuthoritySettings, Identity, Operation, Settings, error};
use crate::{EFFECT_SERVICE, SERVICE, STATE_SERVICE};
use amaterasu_module_sdk::effect_authority::{ConfigurationChange, ConfigurationChangeOutcome};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use x509_cert::der::Encode;

mod owner_restart;
mod source_retirement;

const STATE_KEY: &str = "pairing/v1";
const SYSTEM_TRAY_SERVICE: &str = "amaterasu.system-tray/1";
const HOST_CONFIGURATION_SERVICE: &str = "amaterasu.host-configuration/1";
const HOST_CONFIGURATION_TIMEOUT_MS: u64 = 30_000;
const HOST_CONFIGURATION_REFRESH_SECONDS: u64 = 2;
const HOST_CONFIGURATION_MAX_RETRY_SECONDS: u64 = 8;

type Transport = ModuleResult<Response>;

struct Request {
    id: u64,
    caller: String,
    deadline: Instant,
}
struct EffectCaller<'a> {
    caller: &'a str,
    initiator: &'a str,
}
struct Pending {
    request: Request,
    state_request: u64,
    stage: Stage,
}
struct DeferredImport {
    response: Response,
    deadline: Instant,
}
struct EffectiveIdentityPaths {
    provider_revision: u64,
    certificate_path: String,
    private_key_path: String,
}
struct PairingStateSnapshot {
    paths: EffectiveIdentityPaths,
    revision: Option<u64>,
    cursor: Option<Value>,
    values: Map<String, Value>,
}
enum Stage {
    Loading(Box<Operation>),
    Saving {
        response: Value,
        snapshot: Option<Arc<Snapshot>>,
        expected_ack_revision: u64,
        preserve_base_revision: Option<u64>,
    },
}

struct Initialization {
    request: Request,
    document: Document,
    revision: u64,
    requested_at: u64,
}

struct IdentityGeneration {
    worker: IdentityWorker<Identity>,
    initialization: Option<Initialization>,
}

struct ConfigurationHold {
    owner: String,
    change: ConfigurationChange,
}

struct HoldConfirmation {
    request_id: u64,
    change: ConfigurationChange,
    outcome: Option<ConfigurationChangeOutcome>,
    status_request: Option<u64>,
    verified_revision: Option<u64>,
}

pub struct Pipeline {
    engine: Engine,
    effect_gate: Option<EffectGate>,
    effect_authority: Option<EffectAuthoritySettings>,
    configuration_hold: Option<ConfigurationHold>,
    configuration_reconciled: bool,
    configuration_status_request: Option<u64>,
    hold_confirmation: Option<HoldConfirmation>,
    completed_configuration_hold: Option<(String, ConfigurationChange, ConfigurationChangeOutcome)>,
    effect_bindings: BTreeMap<EffectSource, (Arc<Snapshot>, Vec<u8>)>,
    deferred_effect_mutation: Option<(Request, Operation)>,
    identity_generation: Option<IdentityGeneration>,
    #[cfg(test)]
    identity_generator: Option<Generator>,
    #[cfg(test)]
    configured_identity_loader: Option<Generator<ConfiguredIdentity>>,
    pending: Option<Pending>,
    deferred_import: Option<DeferredImport>,
    identity: Arc<Shared>,
    host_configuration: HostConfigurationState,
    refresh_identity: Option<Arc<ConfiguredIdentity>>,
    host_configuration_retry_seconds: u64,
    identity_migration_checked: bool,
    pairing_notice: Option<PairingNoticeStage>,
    recovery_needed: bool,
    recovery_request: Option<(u64, bool, Option<u64>)>,
    recovery_retry_at: Option<Instant>,
}

enum PairingNoticeStage {
    Waiting(u64),
    Releasing(u64),
}

enum HostConfigurationState {
    Disabled,
    Start,
    WaitingReadiness(u64),
    LoadingIdentity(IdentityWorker<ConfiguredIdentity>),
    WaitingGet(u64),
    WaitingPairingState {
        request: u64,
        snapshot: PairingStateSnapshot,
    },
    WaitingMigrationMarkerDelete {
        request: u64,
        paths: EffectiveIdentityPaths,
        expected_revision: u64,
    },
    Ready {
        identity: Arc<ConfiguredIdentity>,
        refresh_at: u64,
    },
    Failed {
        retry_at: u64,
    },
}

impl Pipeline {
    #[cfg(test)]
    pub fn new(settings: Settings) -> Self {
        Self::with_identity(settings, Arc::new(Shared::default()))
    }

    #[cfg(test)]
    pub fn with_identity(settings: Settings, identity: Arc<Shared>) -> Self {
        Self::with_host_configuration(settings, identity, false)
    }

    pub fn with_host_configuration(
        settings: Settings,
        identity: Arc<Shared>,
        enabled: bool,
    ) -> Self {
        let host_configuration = if enabled {
            HostConfigurationState::Start
        } else {
            HostConfigurationState::Disabled
        };
        let effect_authority = settings.effect_authority.clone();
        let configuration_reconciled = effect_authority
            .as_ref()
            .is_none_or(|policy| policy.configuration_owner.is_none());
        Self {
            engine: Engine::new(settings),
            effect_gate: EffectGate::new().ok(),
            effect_authority,
            configuration_hold: None,
            configuration_reconciled,
            configuration_status_request: None,
            hold_confirmation: None,
            completed_configuration_hold: None,
            effect_bindings: BTreeMap::new(),
            deferred_effect_mutation: None,
            identity_generation: None,
            #[cfg(test)]
            identity_generator: None,
            #[cfg(test)]
            configured_identity_loader: None,
            pending: None,
            deferred_import: None,
            identity,
            host_configuration,
            refresh_identity: None,
            host_configuration_retry_seconds: 1,
            identity_migration_checked: false,
            pairing_notice: None,
            recovery_needed: false,
            recovery_request: None,
            recovery_retry_at: None,
        }
    }

    pub fn authority_available(&self) -> bool {
        (self.effect_authority.is_none() || self.effect_gate.is_some())
            && (self.configuration_reconciled
                || !matches!(self.host_configuration, HostConfigurationState::Disabled))
    }

    pub fn invalidate_identity(&mut self) {
        if let Some(gate) = &mut self.effect_gate {
            gate.fence();
        }
        self.effect_bindings.clear();
        self.identity.invalidate();
    }

    fn request_pairing_notice(
        &mut self,
        request: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) {
        if self.pairing_notice.is_some() {
            return;
        }
        if let Ok(Response::Accepted { id }) = send(Command::Request {
            service: SYSTEM_TRAY_SERVICE.into(),
            payload: json!({"action":"notify_pairing"}),
            timeout_ms: 5_000,
            operation_id: format!("pairing.{request}.tray-notification"),
            parent: None,
        }) {
            self.pairing_notice = Some(PairingNoticeStage::Waiting(id));
        }
    }

    fn progress_pairing_notice(&mut self, send: &mut impl FnMut(Command) -> Transport) {
        let Some(stage) = self.pairing_notice.take() else {
            return;
        };
        self.pairing_notice = match stage {
            PairingNoticeStage::Waiting(request) => match send(Command::TakeResult { request }) {
                Ok(Response::Result { .. }) => Some(PairingNoticeStage::Releasing(request)),
                Ok(Response::Error { code, .. }) if code == "not_found" => None,
                _ => Some(PairingNoticeStage::Waiting(request)),
            },
            PairingNoticeStage::Releasing(request) => {
                match send(Command::ReleaseResult { request }) {
                    Ok(Response::Ok) => None,
                    Ok(Response::Error { code, .. }) if code == "not_found" => None,
                    _ => Some(PairingNoticeStage::Releasing(request)),
                }
            }
        };
    }

    pub fn tick(&mut self, send: impl FnMut(Command) -> Transport) -> ModuleResult<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "pairing clock failure")?
            .as_secs();
        self.tick_at(now, send)
    }

    pub fn tick_at(
        &mut self,
        now: u64,
        mut send: impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        self.progress_pairing_notice(&mut send);
        self.progress_configuration_reconciliation(&mut send)?;
        self.progress_hold_confirmation(&mut send)?;
        if self.identity_generation.is_some() {
            let result = self.advance_identity_generation(now, &mut send);
            if result.is_err() {
                self.invalidate_identity();
                self.recovery_needed = true;
            }
            return result;
        }
        let result = match self.advance_host_configuration(now, &mut send) {
            Ok(true) => {
                let validation_time = self
                    .configured_identity()
                    .map_or(now, |identity| now.max(identity.minimum_validation_time));
                self.drive(validation_time, &mut send)
            }
            Ok(false)
                if matches!(
                    &self.host_configuration,
                    HostConfigurationState::Failed { .. }
                ) =>
            {
                self.reject_unconfigured(now, &mut send)
            }
            Ok(false) if self.effect_authority.is_some() => {
                self.poll_effect_completion(now, &mut send)
            }
            Ok(false) => Ok(()),
            Err(failure) => Err(failure),
        };
        if result.is_err() {
            self.invalidate_identity();
            self.recovery_needed = true;
        }
        result
    }

    fn advance_host_configuration(
        &mut self,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<bool> {
        if let HostConfigurationState::LoadingIdentity(worker) = &mut self.host_configuration {
            return match worker.take_finished() {
                None => Ok(false),
                Some(result) => self.finish_configured_identity(result, now),
            };
        }
        if matches!(
            &self.host_configuration,
            HostConfigurationState::WaitingPairingState { .. }
                | HostConfigurationState::WaitingMigrationMarkerDelete { .. }
        ) {
            return self.advance_imported_identity_migration(now, send);
        }
        match &self.host_configuration {
            HostConfigurationState::Disabled => return Ok(true),
            HostConfigurationState::Ready { refresh_at, .. }
                if now < *refresh_at
                    || self.pending.is_some()
                    || self.deferred_effect_mutation.is_some() =>
            {
                return Ok(true);
            }
            HostConfigurationState::Failed { retry_at } if now < *retry_at => return Ok(false),
            HostConfigurationState::Ready { identity, .. } => {
                self.refresh_identity = Some(identity.clone());
                self.host_configuration = HostConfigurationState::Start;
            }
            HostConfigurationState::Failed { .. } => {
                self.host_configuration = HostConfigurationState::Start;
            }
            HostConfigurationState::WaitingReadiness(_) | HostConfigurationState::WaitingGet(_) => {
            }
            HostConfigurationState::WaitingPairingState { .. }
            | HostConfigurationState::WaitingMigrationMarkerDelete { .. }
            | HostConfigurationState::LoadingIdentity(_) => return Ok(false),
            HostConfigurationState::Start => {}
        }
        if matches!(&self.host_configuration, HostConfigurationState::Start) {
            self.host_configuration = match self.request_host_configuration("readiness", send) {
                Ok(Some(request)) => HostConfigurationState::WaitingReadiness(request),
                _ => {
                    self.fail_host_configuration(now);
                    return Ok(false);
                }
            };
            return Ok(false);
        }

        let request = match &self.host_configuration {
            HostConfigurationState::WaitingReadiness(request)
            | HostConfigurationState::WaitingGet(request) => *request,
            _ => return Ok(false),
        };
        let waiting_for_readiness = matches!(
            &self.host_configuration,
            HostConfigurationState::WaitingReadiness(_)
        );
        match send(Command::TakeResult { request }) {
            Ok(Response::Pending) => Ok(false),
            Ok(Response::Result { payload }) if waiting_for_readiness => {
                if payload["ok"] != true || payload["ready"] != true {
                    self.fail_host_configuration(now);
                    return Ok(false);
                }
                self.host_configuration = match self.request_host_configuration("get", send) {
                    Ok(Some(request)) => HostConfigurationState::WaitingGet(request),
                    _ => {
                        self.fail_host_configuration(now);
                        return Ok(false);
                    }
                };
                Ok(false)
            }
            Ok(Response::Result { payload }) => {
                let Some(revision) = payload["revision"].as_u64() else {
                    self.fail_host_configuration(now);
                    return Ok(false);
                };
                if payload["ok"] != true || payload["ready"] != true {
                    self.fail_host_configuration(now);
                    return Ok(false);
                }
                let Some(certificate_path) = payload["effective"]["cert"].as_str() else {
                    self.fail_host_configuration(now);
                    return Ok(false);
                };
                let Some(private_key_path) = payload["effective"]["pkey"].as_str() else {
                    self.fail_host_configuration(now);
                    return Ok(false);
                };
                let paths = EffectiveIdentityPaths {
                    provider_revision: revision,
                    certificate_path: certificate_path.to_owned(),
                    private_key_path: private_key_path.to_owned(),
                };
                if self.identity_migration_checked {
                    self.load_configured_identity(paths, now)
                } else {
                    self.begin_imported_identity_migration(paths, now, send)
                }
            }
            _ => {
                self.fail_host_configuration(now);
                Ok(false)
            }
        }
    }

    fn load_configured_identity(
        &mut self,
        paths: EffectiveIdentityPaths,
        now: u64,
    ) -> ModuleResult<bool> {
        // The existing-only loader cannot generate RSA, including when files
        // disappear after this check. Provisioning always runs on an owned worker.
        if !std::path::Path::new(&paths.certificate_path).exists()
            && !std::path::Path::new(&paths.private_key_path).exists()
        {
            self.invalidate_identity();
            self.recovery_needed = true;
            let loader: Generator<ConfiguredIdentity> = Box::new(move || {
                ConfiguredIdentity::load(
                    &paths.certificate_path,
                    &paths.private_key_path,
                    paths.provider_revision,
                    now,
                )
            });
            #[cfg(test)]
            let loader = self.configured_identity_loader.take().unwrap_or(loader);
            let worker = IdentityWorker::start(loader);
            match worker {
                Ok(worker) => {
                    self.host_configuration = HostConfigurationState::LoadingIdentity(worker)
                }
                Err(_) => self.fail_host_configuration(now),
            }
            return Ok(false);
        }
        let result = ConfiguredIdentity::load_existing(
            &paths.certificate_path,
            &paths.private_key_path,
            paths.provider_revision,
            now,
        );
        self.finish_configured_identity(result, now)
    }

    fn finish_configured_identity(
        &mut self,
        result: Result<ConfiguredIdentity, &'static str>,
        now: u64,
    ) -> ModuleResult<bool> {
        match result {
            Ok(identity) => {
                let configured_identity = match self.refresh_identity.take() {
                    Some(previous)
                        if previous.provider_revision == identity.provider_revision
                            && previous.certificate_pem == identity.certificate_pem
                            && previous.private_key_pem == identity.private_key_pem =>
                    {
                        previous
                    }
                    Some(previous)
                        if previous.certificate_pem == identity.certificate_pem
                            && previous.private_key_pem == identity.private_key_pem =>
                    {
                        // A configuration revision can change for logging or
                        // tuning without changing this admitted identity.
                        self.identity.advance_configuration_revision(
                            previous.provider_revision,
                            identity.provider_revision,
                        )?;
                        Arc::new(identity)
                    }
                    Some(_) => {
                        self.invalidate_identity();
                        self.recovery_needed = true;
                        Arc::new(identity)
                    }
                    None => Arc::new(identity),
                };
                self.host_configuration = HostConfigurationState::Ready {
                    identity: configured_identity,
                    refresh_at: now.saturating_add(HOST_CONFIGURATION_REFRESH_SECONDS),
                };
                self.host_configuration_retry_seconds = 1;
                Ok(true)
            }
            Err(_) => {
                self.fail_host_configuration(now);
                Ok(false)
            }
        }
    }

    fn begin_imported_identity_migration(
        &mut self,
        paths: EffectiveIdentityPaths,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<bool> {
        let snapshot = PairingStateSnapshot {
            paths,
            revision: None,
            cursor: None,
            values: Map::new(),
        };
        match self.request_pairing_state_snapshot(&snapshot, send) {
            Ok(Some(request)) => {
                self.host_configuration =
                    HostConfigurationState::WaitingPairingState { request, snapshot };
                Ok(false)
            }
            _ => {
                self.fail_host_configuration(now);
                Ok(false)
            }
        }
    }

    fn request_pairing_state_snapshot(
        &mut self,
        snapshot: &PairingStateSnapshot,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<Option<u64>> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| "pairing state entropy unavailable")?;
        let mut payload = json!({"action":"snapshot"});
        if let Some(cursor) = &snapshot.cursor {
            payload["cursor"] = cursor.clone();
        }
        match send(Command::Request {
            service: STATE_SERVICE.into(),
            payload,
            timeout_ms: HOST_CONFIGURATION_TIMEOUT_MS,
            operation_id: format!("pairing.imported-identity.{}", u128::from_le_bytes(nonce)),
            parent: None,
        })? {
            Response::Accepted { id } => Ok(Some(id)),
            _ => Ok(None),
        }
    }

    fn advance_imported_identity_migration(
        &mut self,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<bool> {
        let (request, deleting_marker) = match &self.host_configuration {
            HostConfigurationState::WaitingPairingState { request, .. } => (*request, false),
            HostConfigurationState::WaitingMigrationMarkerDelete { request, .. } => {
                (*request, true)
            }
            _ => return Ok(false),
        };
        match send(Command::TakeResult { request }) {
            Ok(Response::Pending) => Ok(false),
            Ok(Response::Result { payload }) if deleting_marker => {
                self.finish_imported_identity_marker_delete(payload, now)
            }
            Ok(Response::Result { payload }) => {
                self.consume_pairing_state_snapshot_page(payload, now, send)
            }
            _ => {
                self.fail_host_configuration(now);
                Ok(false)
            }
        }
    }

    fn consume_pairing_state_snapshot_page(
        &mut self,
        payload: Value,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<bool> {
        let HostConfigurationState::WaitingPairingState {
            snapshot: mut state_snapshot,
            ..
        } = std::mem::replace(&mut self.host_configuration, HostConfigurationState::Start)
        else {
            return Ok(false);
        };
        let Some(revision) = payload["revision"]
            .as_u64()
            .filter(|_| payload["ok"] == true)
        else {
            self.fail_host_configuration(now);
            return Ok(false);
        };
        if state_snapshot
            .revision
            .is_some_and(|previous| previous != revision)
        {
            self.fail_host_configuration(now);
            return Ok(false);
        }
        let Some(values) = payload["values"].as_object() else {
            self.fail_host_configuration(now);
            return Ok(false);
        };
        state_snapshot.revision = Some(revision);
        state_snapshot.values.extend(
            values
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        state_snapshot.cursor = payload
            .get("next_cursor")
            .filter(|cursor| !cursor.is_null())
            .cloned();

        let has_pairing = state_snapshot.values.contains_key(STATE_KEY);
        let has_marker = state_snapshot
            .values
            .contains_key(IMPORTED_IDENTITY_MIGRATION_KEY);
        if !(has_pairing && has_marker) && state_snapshot.cursor.is_some() {
            match self.request_pairing_state_snapshot(&state_snapshot, send) {
                Ok(Some(request)) => {
                    self.host_configuration = HostConfigurationState::WaitingPairingState {
                        request,
                        snapshot: state_snapshot,
                    };
                    return Ok(false);
                }
                _ => {
                    self.fail_host_configuration(now);
                    return Ok(false);
                }
            }
        }

        let values = Value::Object(state_snapshot.values.clone());
        let reconciliation = match crate::identity::reconcile_imported_identity(
            &values,
            &state_snapshot.paths.certificate_path,
            &state_snapshot.paths.private_key_path,
            now,
        ) {
            Ok(reconciliation) => reconciliation,
            Err(_) => {
                self.fail_host_configuration(now);
                return Ok(false);
            }
        };
        match reconciliation {
            ImportedIdentityReconciliation::NotPending => {
                self.identity_migration_checked = true;
                self.load_configured_identity(state_snapshot.paths, now)
            }
            ImportedIdentityReconciliation::Reconciled
            | ImportedIdentityReconciliation::Superseded => {
                let Some(expected_revision) = state_snapshot.revision else {
                    self.fail_host_configuration(now);
                    return Ok(false);
                };
                match self.request_imported_identity_marker_delete(expected_revision, send) {
                    Ok(Some(request)) => {
                        self.host_configuration =
                            HostConfigurationState::WaitingMigrationMarkerDelete {
                                request,
                                paths: state_snapshot.paths,
                                expected_revision,
                            };
                        Ok(false)
                    }
                    _ => {
                        self.fail_host_configuration(now);
                        Ok(false)
                    }
                }
            }
        }
    }

    fn request_imported_identity_marker_delete(
        &mut self,
        expected_revision: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<Option<u64>> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| "pairing state entropy unavailable")?;
        match send(Command::Request {
            service: STATE_SERVICE.into(),
            payload: json!({
                "action":"delete",
                "key":IMPORTED_IDENTITY_MIGRATION_KEY,
                "expected_revision":expected_revision
            }),
            timeout_ms: HOST_CONFIGURATION_TIMEOUT_MS,
            operation_id: format!(
                "pairing.imported-identity.consume.{}",
                u128::from_le_bytes(nonce)
            ),
            parent: None,
        })? {
            Response::Accepted { id } => Ok(Some(id)),
            _ => Ok(None),
        }
    }

    fn finish_imported_identity_marker_delete(
        &mut self,
        payload: Value,
        now: u64,
    ) -> ModuleResult<bool> {
        let HostConfigurationState::WaitingMigrationMarkerDelete {
            paths,
            expected_revision,
            ..
        } = std::mem::replace(&mut self.host_configuration, HostConfigurationState::Start)
        else {
            return Ok(false);
        };
        if payload["ok"] != true
            || payload["deleted"] != true
            || payload["revision"].as_u64() != expected_revision.checked_add(1)
        {
            self.fail_host_configuration(now);
            return Ok(false);
        }
        self.identity_migration_checked = true;
        self.load_configured_identity(paths, now)
    }

    fn fail_host_configuration(&mut self, now: u64) {
        self.refresh_identity = None;
        self.host_configuration = HostConfigurationState::Failed {
            retry_at: now.saturating_add(self.host_configuration_retry_seconds),
        };
        self.host_configuration_retry_seconds =
            (self.host_configuration_retry_seconds * 2).min(HOST_CONFIGURATION_MAX_RETRY_SECONDS);
        self.invalidate_identity();
        self.recovery_needed = true;
    }

    fn request_host_configuration(
        &mut self,
        action: &str,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<Option<u64>> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| "host configuration entropy unavailable")?;
        match send(Command::Request {
            service: HOST_CONFIGURATION_SERVICE.into(),
            payload: json!({"action": action}),
            timeout_ms: HOST_CONFIGURATION_TIMEOUT_MS,
            operation_id: format!(
                "pairing.host-configuration.{action}.{}",
                u128::from_le_bytes(nonce)
            ),
            parent: None,
        })? {
            Response::Accepted { id } => Ok(Some(id)),
            _ => Ok(None),
        }
    }

    fn reject_unconfigured(
        &mut self,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if let Some(deferred) = &self.deferred_import {
            if Instant::now() < deferred.deadline {
                return Ok(());
            }
            let deferred = self.deferred_import.take().unwrap();
            let Response::Request { id, .. } = deferred.response else {
                unreachable!()
            };
            return complete(send, id, error("host_configuration_unavailable"));
        }
        match send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                initiator,
                service,
                payload,
                ..
            } if service == EFFECT_SERVICE => {
                self.handle_effect_request(id, &caller, &initiator, payload, now, send)
            }
            response if self.authorized_import_for_retry(&response) => {
                let Response::Request { remaining_ms, .. } = &response else {
                    unreachable!()
                };
                let deadline = Instant::now() + Duration::from_millis((*remaining_ms).min(30_000));
                self.deferred_import = Some(DeferredImport { response, deadline });
                Ok(())
            }
            Response::Request { id, service, .. } if service == SERVICE => {
                complete(send, id, error("host_configuration_unavailable"))
            }
            Response::Request { id, .. } => complete(send, id, error("invalid_service")),
            _ => Err("pairing poll failure".into()),
        }
    }

    fn authorized_import_for_retry(&self, response: &Response) -> bool {
        let Response::Request {
            caller,
            service,
            payload,
            ..
        } = response
        else {
            return false;
        };
        if service != SERVICE || payload["action"] != "import_identity" {
            return false;
        }
        if serde_json::to_vec(payload).map_or(true, |bytes| bytes.len() > 12_000) {
            return false;
        }
        let Ok(operation) = serde_json::from_value::<Operation>(payload.clone()) else {
            return false;
        };
        self.engine.admit(caller, &operation).is_ok()
    }

    fn drive(&mut self, now: u64, mut send: impl FnMut(Command) -> Transport) -> ModuleResult<()> {
        if let Some(pending) = self.pending.take() {
            return self.advance(pending, now, &mut send);
        }
        if let Some((request, operation)) = self.deferred_effect_mutation.take() {
            if Instant::now() >= request.deadline {
                return complete(&mut send, request.id, error("deadline_exceeded"));
            }
            if !matches!(
                send(Command::CheckRequest {
                    request: request.id
                })?,
                Response::Ok
            ) {
                return complete(&mut send, request.id, error("request_ended"));
            }
            if let Some(gate) = &mut self.effect_gate {
                gate.fence();
            }
            if self.effect_gate.as_ref().is_none_or(|gate| !gate.drained()) {
                self.deferred_effect_mutation = Some((request, operation));
                return self.poll_effect_completion(now, &mut send);
            }
            return self.submit(
                request,
                Stage::Loading(Box::new(operation)),
                json!({"action":"get","key":STATE_KEY}),
                &mut send,
            );
        }
        if self
            .effect_gate
            .as_ref()
            .is_some_and(|gate| gate.drain_required())
        {
            return self.poll_effect_completion(now, &mut send);
        }
        // One request at a time: state CAS operations cannot race internally and
        // identity writes and pairing commits are serialized without stale caches.
        let incoming = match self.deferred_import.take() {
            Some(deferred) if Instant::now() < deferred.deadline => {
                let Response::Request { id, .. } = &deferred.response else {
                    unreachable!()
                };
                if !matches!(send(Command::CheckRequest { request: *id })?, Response::Ok) {
                    return Ok(());
                }
                deferred.response
            }
            Some(deferred) => {
                let Response::Request { id, .. } = deferred.response else {
                    unreachable!()
                };
                return complete(&mut send, id, error("host_configuration_unavailable"));
            }
            None => send(Command::Poll {})?,
        };
        match incoming {
            Response::Pending => self.recover_identity(now, &mut send),
            Response::Request {
                id,
                caller,
                initiator,
                service,
                payload,
                remaining_ms,
                ..
            } => {
                if !matches!(send(Command::CheckRequest { request: id })?, Response::Ok) {
                    return complete(&mut send, id, error("request_ended"));
                }
                if let Some((_, stale, _)) = &mut self.recovery_request {
                    *stale = true;
                }
                if service == EFFECT_SERVICE {
                    return self
                        .handle_effect_request(id, &caller, &initiator, payload, now, &mut send);
                }
                if service != SERVICE {
                    return complete(&mut send, id, error("invalid_service"));
                }
                if serde_json::to_vec(&payload).map_or(true, |bytes| bytes.len() > 12_000) {
                    return complete(&mut send, id, error("invalid_request"));
                }
                let Ok(operation) = serde_json::from_value::<Operation>(payload) else {
                    return complete(&mut send, id, error("invalid_request"));
                };
                if let Err(code) = self.engine.admit(&caller, &operation) {
                    return complete(&mut send, id, error(code));
                }
                if operation.may_write()
                    && (!self.configuration_reconciled || self.configuration_hold.is_some())
                {
                    return complete(&mut send, id, error("configuration_change_pending"));
                }
                if let (
                    Some(configured_identity),
                    Operation::ImportIdentity {
                        certificate_pem,
                        private_key_pem,
                        ..
                    },
                ) = (self.configured_identity(), &operation)
                    && (certificate_pem != &configured_identity.certificate_pem
                        || private_key_pem != &configured_identity.private_key_pem)
                {
                    return complete(
                        &mut send,
                        id,
                        error("identity_managed_by_host_configuration"),
                    );
                }
                let request = Request {
                    id,
                    caller,
                    deadline: Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
                };
                if operation.may_write() && !is_client_configuration_update(&operation) {
                    if self.effect_authority.is_some() && self.effect_gate.is_none() {
                        return complete(&mut send, id, error("authority_unavailable"));
                    }
                    self.invalidate_identity();
                    self.recovery_needed = true;
                    if self.effect_authority.is_some()
                        && self.effect_gate.as_ref().is_none_or(|gate| !gate.drained())
                    {
                        self.deferred_effect_mutation = Some((request, operation));
                        return Ok(());
                    }
                }
                self.submit(
                    request,
                    Stage::Loading(Box::new(operation)),
                    json!({"action":"get","key":STATE_KEY}),
                    &mut send,
                )
            }
            _ => Err("pairing poll failure".into()),
        }
    }

    fn poll_effect_completion(
        &mut self,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        match send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                initiator,
                service,
                payload,
                ..
            } if service == EFFECT_SERVICE
                && matches!(
                    payload["action"].as_str(),
                    Some(
                        "complete"
                            | "complete_stream_lease"
                            | "lookup_stream_lease"
                            | "lookup"
                            | "acknowledge"
                            | "status"
                            | "drain"
                            | "recover"
                            | "fence_protocol_restart"
                            | "fence_effect_owner_restart"
                            | "read_effect_owner_restart"
                            | "confirm_effect_owner_restart"
                            | "fence_source"
                            | "complete_source_retirement"
                            | "lookup_source_retirement"
                            | "lookup_current_source_retirement"
                            | "begin_configuration_change"
                            | "finish_configuration_change"
                            | "reconcile_configuration_change"
                    )
                ) =>
            {
                self.handle_effect_request(id, &caller, &initiator, payload, now, send)
            }
            Response::Request { id, .. } => complete(send, id, error("authority_draining")),
            _ => Err("pairing poll failure".into()),
        }
    }

    fn handle_effect_request(
        &mut self,
        id: u64,
        caller: &str,
        initiator: &str,
        payload: Value,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if !matches!(send(Command::CheckRequest { request: id })?, Response::Ok) {
            return complete(send, id, error("request_ended"));
        }
        if let Ok(request) = serde_json::from_value::<EffectRequest>(payload.clone()) {
            match request {
                EffectRequest::FinishConfigurationChange { change, result } => {
                    return self.start_hold_confirmation(
                        id,
                        EffectCaller { caller, initiator },
                        change,
                        Some(result),
                        None,
                        send,
                    );
                }
                EffectRequest::ReconcileConfigurationChange {
                    change,
                    current_authority_epoch,
                } => {
                    return self.start_hold_confirmation(
                        id,
                        EffectCaller { caller, initiator },
                        change,
                        None,
                        Some(current_authority_epoch),
                        send,
                    );
                }
                _ => {}
            }
        }
        let response = self.apply_effect_request(caller, initiator, payload, now);
        complete(
            send,
            id,
            match response {
                Ok(value) => value,
                Err(code) => error(code),
            },
        )
    }

    fn apply_effect_request(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: Value,
        now: u64,
    ) -> Result<Value, &'static str> {
        let policy = self
            .effect_authority
            .as_ref()
            .ok_or("authority_unavailable")?;
        if caller != initiator
            || !serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() <= 12_000)
        {
            return Err("forbidden");
        }
        if source_retirement::is_source_retirement(&payload) {
            return self.apply_source_retirement(caller, payload);
        }
        if owner_restart::is_owner_restart(&payload) {
            return self.apply_owner_restart(caller, payload);
        }
        let request: EffectRequest =
            serde_json::from_value(payload).map_err(|_| "invalid_request")?;
        let permitted = match request {
            EffectRequest::BeginConfigurationChange { .. }
            | EffectRequest::FinishConfigurationChange { .. }
            | EffectRequest::ReconcileConfigurationChange { .. } => {
                policy.configuration_owner.as_deref() == Some(caller)
            }
            EffectRequest::Bind { .. } | EffectRequest::BindStream { .. } => {
                caller == policy.protocol_owner
            }
            EffectRequest::Status {} => {
                caller == policy.protocol_owner
                    || caller == policy.effect_owner
                    || policy.configuration_owner.as_deref() == Some(caller)
            }
            _ => caller == policy.effect_owner,
        };
        if !permitted {
            return Err("forbidden");
        }
        if let EffectRequest::Recover { epoch, owner_epoch } = &request {
            if !self.configuration_reconciled {
                return Err("configuration_change_pending");
            }
            if self.configuration_hold.is_some() {
                let gate = self.effect_gate.as_mut().ok_or("authority_unavailable")?;
                if !gate.startup_recovery_required() {
                    return Err("configuration_change_pending");
                }
                gate.require_epoch(epoch)?;
                gate.recover(owner_epoch)?;
                gate.fence();
                self.effect_bindings.clear();
                return Ok(
                    json!({"ok":true,"epoch":gate.epoch(),"held":true,"fence_generation":gate.fence_generation()}),
                );
            }
        }
        // Recovery reopens admission. While the authoritative host identity is
        // being loaded or refreshed, the old snapshot cannot authorize that
        // transition even after the effect owner has retired its old workers.
        if matches!(&request, EffectRequest::Recover { .. })
            && !matches!(
                &self.host_configuration,
                HostConfigurationState::Disabled | HostConfigurationState::Ready { .. }
            )
        {
            return Err("host_configuration_unavailable");
        }
        match request {
            EffectRequest::BeginConfigurationChange { change } => {
                if !change.valid() {
                    return Err("invalid_request");
                }
                if let Some(hold) = &self.configuration_hold {
                    if hold.owner != caller || hold.change != change {
                        return Err("configuration_change_pending");
                    }
                } else {
                    if !self.configuration_reconciled {
                        return Err("configuration_change_pending");
                    }
                    let revisions = self.current_identity_revisions()?;
                    if revisions
                        != Some((
                            change.expected_trust_revision,
                            change.expected_configuration_revision,
                        ))
                    {
                        return Err("revision_conflict");
                    }
                    let gate = self.effect_gate.as_mut().ok_or("authority_unavailable")?;
                    gate.require_epoch(&change.authority_epoch)?;
                    gate.fence_for_configuration_change()?;
                    self.effect_bindings.clear();
                    self.configuration_hold = Some(ConfigurationHold {
                        owner: caller.into(),
                        change: change.clone(),
                    });
                }
                let gate = self.effect_gate.as_ref().ok_or("authority_unavailable")?;
                return Ok(json!({
                    "ok": true,
                    "state": if gate.drained() { "drained" } else { "pending" },
                    "epoch": gate.epoch(),
                    "fence_generation": gate.fence_generation(),
                    "operation_id": change.operation_id,
                }));
            }
            EffectRequest::FinishConfigurationChange { change, .. }
            | EffectRequest::ReconcileConfigurationChange { change, .. } => {
                if !change.valid() {
                    return Err("invalid_request");
                }
                if self
                    .configuration_hold
                    .as_ref()
                    .is_none_or(|hold| hold.owner != caller || hold.change != change)
                {
                    return Err("configuration_change_unknown");
                }
                return Ok(json!({"ok":true,"state":"pending"}));
            }
            EffectRequest::Bind {
                epoch,
                certificate_pem,
                source,
            } => return self.bind_effect_source(epoch, certificate_pem, source, None, now),
            EffectRequest::BindStream {
                epoch,
                certificate_pem,
                source,
                scope,
            } => return self.bind_effect_source(epoch, certificate_pem, source, Some(scope), now),
            _ => {}
        }
        let revisions = self.current_identity_revisions()?;
        let gate = self.effect_gate.as_mut().ok_or("authority_unavailable")?;
        match request {
            EffectRequest::BeginConfigurationChange { .. }
            | EffectRequest::FinishConfigurationChange { .. }
            | EffectRequest::ReconcileConfigurationChange { .. } => unreachable!("handled above"),
            EffectRequest::Recover { epoch, owner_epoch } => {
                gate.require_epoch(&epoch)?;
                gate.recover(&owner_epoch)?;
                self.effect_bindings.clear();
                Ok(json!({"ok":true,"epoch":gate.epoch()}))
            }
            EffectRequest::Bind { .. } | EffectRequest::BindStream { .. } => {
                unreachable!("handled above")
            }
            EffectRequest::AdmitStreamLease {
                epoch,
                source,
                operation_id,
                sequence,
                scope,
            } => {
                gate.require_epoch(&epoch)?;
                let (snapshot, certificate) =
                    self.effect_bindings.get(&source).ok_or("source_denied")?;
                if !self.identity.current(snapshot, now) {
                    return Err("source_denied");
                }
                authorize_feedback_scope(policy, snapshot, certificate, &scope, now)?;
                let ticket = gate
                    .admit_stream_lease(&source, &operation_id, sequence, scope.clone())?
                    .to_owned();
                Ok(
                    json!({"ok":true,"epoch":gate.epoch(),"operation_id":operation_id,"ticket":ticket,"kind":"stream_lease","source":source,"scope":scope}),
                )
            }
            EffectRequest::LookupStreamLease {
                epoch,
                source,
                operation_id,
                scope,
            } => {
                gate.require_epoch(&epoch)?;
                let (ticket, fenced) = gate.lookup_stream_lease(&source, &operation_id, &scope)?;
                Ok(
                    json!({"ok":true,"epoch":gate.epoch(),"operation_id":operation_id,"ticket":ticket,"kind":"stream_lease","fenced":fenced,"source":source,"scope":scope}),
                )
            }
            EffectRequest::CompleteStreamLease {
                epoch,
                operation_id,
                ticket,
                retirement,
            } => {
                gate.complete_stream_lease(&epoch, &operation_id, &ticket, &retirement)?;
                Ok(json!({"ok":true,"epoch":gate.epoch(),"operation_id":operation_id}))
            }
            EffectRequest::Admit { .. } => Err("scope_required"),
            EffectRequest::Status {} => Ok(
                json!({"ok":true,"epoch":gate.epoch(),"owner_epoch":gate.owner_epoch(),"startup_recovery_required":gate.startup_recovery_required(),"fence_generation":gate.fence_generation(),"fenced":gate.fenced(),"drain_required":gate.drain_required(),"in_flight":gate.in_flight(),"bound_sources":gate.bound_sources(),"pending_source_retirements":gate.pending_source_retirements(),"revisions":revisions.map(|(trust, configuration)| json!({"trust":trust,"configuration":configuration}))}),
            ),
            EffectRequest::Drain {
                epoch,
                fence_generation,
            } => {
                gate.acknowledge_drain(&epoch, fence_generation)?;
                Ok(json!({"ok":true,"epoch":gate.epoch(),"fence_generation":fence_generation}))
            }
            EffectRequest::Lookup {
                epoch,
                operation_id,
            } => {
                gate.require_epoch(&epoch)?;
                let (ticket, fenced) = gate.lookup(&operation_id)?;
                Ok(
                    json!({"ok":true,"epoch":gate.epoch(),"operation_id":operation_id,"ticket":ticket,"fenced":fenced}),
                )
            }
            EffectRequest::Acknowledge {
                epoch,
                operation_id,
                ticket,
            } => {
                gate.acknowledge(&epoch, &operation_id, &ticket)?;
                Ok(json!({"ok":true,"epoch":gate.epoch(),"operation_id":operation_id}))
            }
            EffectRequest::Complete {
                epoch,
                operation_id,
                ticket,
            } => {
                gate.complete(&epoch, &operation_id, &ticket)?;
                Ok(json!({"ok":true,"epoch":gate.epoch(),"operation_id":operation_id}))
            }
        }
    }

    fn bind_effect_source(
        &mut self,
        epoch: String,
        certificate_pem: String,
        source: EffectSource,
        stream_scope: Option<crate::effect_gate::StreamScope>,
        now: u64,
    ) -> Result<Value, &'static str> {
        let gate = self.effect_gate.as_mut().ok_or("authority_unavailable")?;
        gate.require_epoch(&epoch)?;
        // Keep the production endpoint closed until the protocol authenticates
        // the lifecycle join and the end-to-end retirement barrier is proven.
        if !source_join_available() {
            return Err("source_attestation_unavailable");
        }
        let certificate = crate::crypto::parse_certificate(&certificate_pem, Some(now))
            .map_err(|_| "source_denied")?;
        let der = certificate
            .certificate
            .to_der()
            .map_err(|_| "source_denied")?;
        let snapshot = self.identity.get(now).map_err(|_| "source_denied")?;
        if !snapshot.verify_client(&der, now) {
            return Err("source_denied");
        }
        if let Some(scope) = &stream_scope {
            let policy = self
                .effect_authority
                .as_ref()
                .ok_or("authority_unavailable")?;
            authorize_feedback_scope(policy, &snapshot, &der, scope, now)?;
        }
        if !self.identity.current(&snapshot, now) {
            return Err("source_denied");
        }
        if self
            .effect_bindings
            .get(&source)
            .is_some_and(|(previous, bound_certificate)| {
                bound_certificate != &der || !Arc::ptr_eq(previous, &snapshot)
            })
        {
            return Err("source_owner_conflict");
        }
        gate.bind_scoped(source.clone(), stream_scope)?;
        self.effect_bindings.insert(source, (snapshot, der));
        Ok(json!({"ok":true,"epoch":gate.epoch()}))
    }

    fn submit(
        &mut self,
        request: Request,
        stage: Stage,
        payload: Value,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let timeout_ms = request
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if timeout_ms == 0 {
            return complete(send, request.id, error("deadline_exceeded"));
        }
        let stage_name = match &stage {
            Stage::Loading(_) => "load",
            Stage::Saving { .. } => "save",
        };
        match send(Command::Request {
            service: STATE_SERVICE.into(),
            payload,
            timeout_ms: timeout_ms.min(30_000),
            operation_id: format!("pairing.{}.{}", request.id, stage_name),
            parent: Some(request.id),
        })? {
            Response::Accepted { id } => {
                self.pending = Some(Pending {
                    request,
                    state_request: id,
                    stage,
                });
                Ok(())
            }
            _ => {
                self.invalidate_identity();
                self.recovery_needed = true;
                complete(send, request.id, error("state_unavailable"))
            }
        }
    }

    fn advance(
        &mut self,
        pending: Pending,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        match send(Command::TakeResult {
            request: pending.state_request,
        })? {
            Response::Pending => {
                self.pending = Some(pending);
                Ok(())
            }
            Response::Result { payload }
                if payload["ok"] == true && payload["revision"].as_u64().is_some() =>
            {
                match pending.stage {
                    Stage::Saving {
                        response,
                        snapshot,
                        expected_ack_revision,
                        preserve_base_revision,
                    } => {
                        if payload["revision"].as_u64() != Some(expected_ack_revision) {
                            self.invalidate_identity();
                            self.recovery_needed = true;
                            return complete(send, pending.request.id, error("state_unavailable"));
                        }
                        if let Some(base_revision) = preserve_base_revision {
                            if self
                                .identity
                                .advance_pairing_revision(base_revision, expected_ack_revision)
                                .is_err()
                            {
                                self.invalidate_identity();
                                self.recovery_needed = true;
                                return complete(
                                    send,
                                    pending.request.id,
                                    error("state_unavailable"),
                                );
                            }
                        } else {
                            self.publish_identity(expected_ack_revision, snapshot)?;
                        }
                        self.recovery_needed = false;
                        complete(send, pending.request.id, response)
                    }
                    Stage::Loading(operation) => {
                        if Instant::now() >= pending.request.deadline {
                            if is_client_configuration_update(&operation) {
                                self.invalidate_identity();
                                self.recovery_needed = true;
                            }
                            return complete(send, pending.request.id, error("deadline_exceeded"));
                        }
                        if !matches!(
                            send(Command::CheckRequest {
                                request: pending.request.id
                            })?,
                            Response::Ok
                        ) {
                            if is_client_configuration_update(&operation) {
                                self.invalidate_identity();
                                self.recovery_needed = true;
                            }
                            return complete(send, pending.request.id, error("request_ended"));
                        }
                        let configured_identity = self.configured_identity();
                        let document =
                            match read_document(&payload, now, configured_identity.is_none()) {
                                Ok(document) => document,
                                Err(code) => {
                                    self.invalidate_identity();
                                    self.recovery_needed = true;
                                    return complete(send, pending.request.id, error(code));
                                }
                            };
                        self.apply_loaded(
                            pending.request,
                            *operation,
                            document,
                            payload["revision"].as_u64().unwrap(),
                            now,
                            send,
                        )
                    }
                }
            }
            Response::Result { payload }
                if matches!(&pending.stage, Stage::Saving { .. })
                    && payload["ok"] == false
                    && payload["error"]["code"] == "revision_conflict"
                    && payload["error"]["current_revision"].as_u64().is_some() =>
            {
                self.invalidate_identity();
                self.recovery_needed = true;
                complete(
                    send,
                    pending.request.id,
                    json!({
                        "ok": false,
                        "code": "revision_conflict",
                        "current_revision": payload["error"]["current_revision"]
                    }),
                )
            }
            // Never echo transport/state messages: they can contain input values.
            _ => {
                self.invalidate_identity();
                self.recovery_needed = true;
                complete(send, pending.request.id, error("state_unavailable"))
            }
        }
    }
    fn start_identity_generation(
        &mut self,
        request: Request,
        document: Document,
        revision: u64,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        #[cfg(test)]
        let generator = self
            .identity_generator
            .take()
            .unwrap_or_else(|| Box::new(crate::crypto::generate_identity));
        #[cfg(not(test))]
        let generator: Generator = Box::new(crate::crypto::generate_identity);
        match IdentityWorker::start(generator) {
            Ok(worker) => {
                self.identity_generation = Some(IdentityGeneration {
                    worker,
                    initialization: Some(Initialization {
                        request,
                        document,
                        revision,
                        requested_at: now,
                    }),
                });
                Ok(())
            }
            Err(code) => complete(send, request.id, error(code)),
        }
    }

    fn advance_identity_generation(
        &mut self,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let mut generation = self
            .identity_generation
            .take()
            .expect("owned identity worker");
        if let Some(initialization) = &generation.initialization {
            let ended = if Instant::now() >= initialization.request.deadline {
                Some("deadline_exceeded")
            } else {
                match send(Command::CheckRequest {
                    request: initialization.request.id,
                }) {
                    Ok(Response::Ok) => None,
                    Ok(_) => Some("request_ended"),
                    Err(failure) => {
                        self.identity_generation = Some(generation);
                        return Err(failure);
                    }
                }
            };
            if let Some(code) = ended {
                let initialization = generation
                    .initialization
                    .take()
                    .expect("live initialization");
                // Keep the owned worker even if completion transport fails.
                let result = complete(send, initialization.request.id, error(code));
                self.identity_generation = Some(generation);
                return result;
            }
        }
        match generation.worker.take_finished() {
            Some(result) => {
                let Some(initialization) = generation.initialization.take() else {
                    return Ok(());
                };
                match result {
                    Ok(identity) => {
                        let initialization = Initialization {
                            requested_at: now.max(initialization.requested_at),
                            ..initialization
                        };
                        self.apply_loaded_with_identity(
                            initialization,
                            Operation::Initialize {},
                            Some(identity),
                            send,
                        )
                    }
                    Err(code) => complete(send, initialization.request.id, error(code)),
                }
            }
            None => {
                let canceled = generation.initialization.is_none();
                self.identity_generation = Some(generation);
                if canceled {
                    match send(Command::Poll {})? {
                        Response::Pending => Ok(()),
                        Response::Request { id, .. } => {
                            complete(send, id, error("identity_generation_busy"))
                        }
                        _ => Err("pairing poll failure".into()),
                    }
                } else {
                    Ok(())
                }
            }
        }
    }

    fn apply_loaded(
        &mut self,
        request: Request,
        operation: Operation,
        document: Document,
        revision: u64,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        self.apply_loaded_with_identity(
            Initialization {
                request,
                document,
                revision,
                requested_at: now,
            },
            operation,
            None,
            send,
        )
    }

    fn apply_loaded_with_identity(
        &mut self,
        initialization: Initialization,
        operation: Operation,
        generated_identity: Option<Identity>,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let Initialization {
            request,
            document,
            revision,
            requested_at: now,
        } = initialization;
        let loaded = match self.prepare_snapshot(&document, now) {
            Ok(snapshot) => snapshot,
            Err(code) => {
                self.invalidate_identity();
                self.recovery_needed = true;
                return complete(send, request.id, error(code));
            }
        };
        if matches!(operation, Operation::Initialize {})
            && document.identity.is_none()
            && generated_identity.is_none()
        {
            return self.start_identity_generation(request, document, revision, now, send);
        }
        // Only this operation replaces configuration on an existing certificate.
        // Publish the validated durable base while its CAS is pending; trust and
        // identity mutations retain their fail-closed invalidation boundary.
        let mut configuration_only =
            is_client_configuration_update(&operation) && self.effect_authority.is_none();
        if configuration_only {
            self.publish_identity(revision, loaded.clone())?;
        }
        if self.configured_identity().is_some()
            && document.identity.is_none()
            && !matches!(
                &operation,
                Operation::Initialize {} | Operation::ImportIdentity { .. }
            )
        {
            self.publish_identity(revision, loaded)?;
            return complete(send, request.id, error("not_initialized"));
        }
        let expected_revision = match &operation {
            Operation::UpdateClientConfigOverrides {
                expected_revision, ..
            }
            | Operation::UpdateClientPermissions {
                expected_revision, ..
            }
            | Operation::UpdateClientFeedbackGrants {
                expected_revision, ..
            }
            | Operation::SetPermissionMode {
                expected_revision, ..
            } => Some(*expected_revision),
            Operation::Unpair {
                expected_revision, ..
            } => *expected_revision,
            _ => None,
        };
        if expected_revision.is_some_and(|expected| expected != revision) {
            self.publish_identity(revision, loaded)?;
            return complete(
                send,
                request.id,
                json!({"ok":false,"code":"revision_conflict","current_revision":revision}),
            );
        }
        if self.effect_authority.is_some() && is_client_configuration_update(&operation) {
            let current_trust_revision = self.identity.revisions()?.map(|(trust, _)| trust);
            configuration_only = current_trust_revision == Some(revision)
                && is_snapshot_tuning_update(&operation, &document);
            if !configuration_only {
                self.invalidate_identity();
                self.recovery_needed = true;
                if self.effect_gate.as_ref().is_none_or(|gate| !gate.drained()) {
                    self.deferred_effect_mutation = Some((request, operation));
                    return Ok(());
                }
            }
        }
        let include_revision = matches!(&operation, Operation::ListClients {});
        let initialize = matches!(&operation, Operation::Initialize {});
        let persisted_identity = document.identity.clone();
        let mut engine_now = now;
        let initialized_document = if initialize && persisted_identity.is_none() {
            engine_now = match SystemTime::now().duration_since(UNIX_EPOCH) {
                Ok(current_time) => now.max(current_time.as_secs()),
                Err(_) => return complete(send, request.id, error("clock_failure")),
            };
            let durable_document = Document {
                identity: generated_identity,
                clients: document.clients.clone(),
                permission_mode_enabled: document.permission_mode_enabled,
            };
            if durable_document.identity.is_none() {
                return complete(send, request.id, error("identity_failure"));
            }
            if let Err(code) = durable_document.validate(engine_now) {
                self.publish_identity(revision, loaded)?;
                return complete(send, request.id, error(code));
            }
            Some(durable_document)
        } else {
            None
        };
        let durable_document = initialized_document.as_ref().unwrap_or(&document);
        let engine_document = match self.configured_identity() {
            Some(configured_identity) => overlay_document(durable_document, configured_identity),
            None => Document {
                identity: durable_document.identity.clone(),
                clients: durable_document.clients.clone(),
                permission_mode_enabled: durable_document.permission_mode_enabled,
            },
        };
        let is_server_certificate_request = matches!(&operation, Operation::GetServerCert { .. });
        match self
            .engine
            .apply(&request.caller, operation, engine_document, engine_now)
        {
            Ok(Change::Read(mut response)) => {
                if is_server_certificate_request && response["pending"] == true {
                    self.request_pairing_notice(request.id, send);
                }
                if include_revision {
                    response["revision"] = revision.into();
                }
                if let Some(document) = initialized_document {
                    self.invalidate_identity();
                    self.recovery_needed = true;
                    let Some(expected_ack_revision) = revision.checked_add(1) else {
                        return complete(send, request.id, error("state_unavailable"));
                    };
                    let snapshot = match self.prepare_snapshot(&document, engine_now) {
                        Ok(snapshot) => snapshot,
                        Err(code) => {
                            return complete(send, request.id, error(code));
                        }
                    };
                    return self.submit(
                        request,
                        Stage::Saving {
                            response,
                            snapshot,
                            expected_ack_revision,
                            preserve_base_revision: None,
                        },
                        json!({
                            "action":"set",
                            "key":STATE_KEY,
                            "expected_revision":revision,
                            "value":document
                        }),
                        send,
                    );
                }
                self.publish_identity(revision, loaded)?;
                self.recovery_needed = false;
                complete(send, request.id, response)
            }
            Ok(Change::Write {
                mut document,
                response,
            }) => {
                if let Some(identity) = persisted_identity {
                    // Engine used the configured identity in memory, but the
                    // durable pairing document keeps its original host PEMs.
                    document.identity = Some(identity);
                }
                if !configuration_only {
                    self.invalidate_identity();
                    self.recovery_needed = true;
                }
                let Some(expected_ack_revision) = revision.checked_add(1) else {
                    self.invalidate_identity();
                    self.recovery_needed = true;
                    return complete(send, request.id, error("state_unavailable"));
                };
                let snapshot_now = if initialize {
                    match SystemTime::now().duration_since(UNIX_EPOCH) {
                        Ok(current) => now.max(current.as_secs()),
                        Err(_) => return complete(send, request.id, error("clock_failure")),
                    }
                } else {
                    now
                };
                let snapshot = match self.prepare_snapshot(&document, snapshot_now) {
                    Ok(snapshot) => snapshot,
                    Err(code) => {
                        self.invalidate_identity();
                        self.recovery_needed = true;
                        return complete(send, request.id, error(code));
                    }
                };
                self.submit(
                    request,
                    Stage::Saving {
                        response,
                        snapshot,
                        expected_ack_revision,
                        preserve_base_revision: (configuration_only
                            && self.effect_authority.is_some())
                        .then_some(revision),
                    },
                    json!({
                        "action":"set",
                        "key":STATE_KEY,
                        "expected_revision":revision,
                        "value":document
                    }),
                    send,
                )
            }
            Err(code) => {
                self.publish_identity(revision, loaded)?;
                self.recovery_needed = false;
                complete(send, request.id, error(code))
            }
        }
    }

    // Only run in an idle tick. An overlapping foreground operation marks the
    // read stale, so its older response can never republish revoked authority.
    fn recover_identity(
        &mut self,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if let Some((request, stale, configuration_revision)) = self.recovery_request.take() {
            match send(Command::TakeResult { request })? {
                Response::Pending => {
                    self.recovery_request = Some((request, stale, configuration_revision));
                    return Ok(());
                }
                Response::Result { payload }
                    if payload["ok"] == true
                        && !stale
                        && configuration_revision
                            == self
                                .configured_identity()
                                .map(|value| value.provider_revision) =>
                {
                    if let (Some(revision), Ok(document)) = (
                        payload["revision"].as_u64(),
                        read_document(&payload, now, self.configured_identity().is_none()),
                    ) && let Ok(snapshot) = self.prepare_snapshot(&document, now)
                        && self.publish_identity(revision, snapshot).is_ok()
                    {
                        self.recovery_needed = false;
                        self.recovery_retry_at = None;
                        return Ok(());
                    }
                }
                _ => {}
            }
            self.recovery_retry_at = Some(Instant::now() + Duration::from_secs(1));
        }
        if !self.recovery_needed
            || self
                .recovery_retry_at
                .is_some_and(|deadline| Instant::now() < deadline)
        {
            return Ok(());
        }
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| "pairing recovery entropy unavailable")?;
        match send(Command::Request {
            service: STATE_SERVICE.into(),
            payload: json!({"action":"get","key":STATE_KEY}),
            timeout_ms: 5_000,
            operation_id: format!("pairing.recovery.{}", u128::from_le_bytes(nonce)),
            parent: None,
        })? {
            Response::Accepted { id } => {
                self.recovery_request = Some((
                    id,
                    false,
                    self.configured_identity()
                        .map(|value| value.provider_revision),
                ))
            }
            _ => self.recovery_retry_at = Some(Instant::now() + Duration::from_secs(1)),
        }
        Ok(())
    }

    fn prepare_snapshot(
        &self,
        document: &Document,
        now: u64,
    ) -> Result<Option<Arc<Snapshot>>, &'static str> {
        match &self.host_configuration {
            HostConfigurationState::Ready {
                identity: configured_identity,
                ..
            } => Snapshot::prepare_with_configured_identity(document, now, configured_identity)
                .map(Some),
            HostConfigurationState::Disabled => Snapshot::prepare(document, now),
            _ => Err("host_configuration_unavailable"),
        }
    }

    fn configured_identity(&self) -> Option<&ConfiguredIdentity> {
        match &self.host_configuration {
            HostConfigurationState::Ready {
                identity: configured_identity,
                ..
            } => Some(configured_identity),
            _ => None,
        }
    }

    fn current_identity_revisions(&self) -> Result<Option<(u64, u64)>, &'static str> {
        Ok(self
            .identity
            .revisions()?
            .map(|(trust_revision, snapshot_revision)| {
                (
                    trust_revision,
                    self.configured_identity()
                        .map_or(snapshot_revision, |identity| identity.provider_revision),
                )
            }))
    }

    fn publish_identity(
        &self,
        pairing_revision: u64,
        snapshot: Option<Arc<Snapshot>>,
    ) -> Result<(), &'static str> {
        if self
            .effect_gate
            .as_ref()
            .is_some_and(|gate| gate.drain_required())
        {
            return Err("effect_retirement_pending");
        }
        let configuration_revision = match &self.host_configuration {
            HostConfigurationState::Ready {
                identity: configured_identity,
                ..
            } => configured_identity.provider_revision,
            HostConfigurationState::Disabled => 0,
            _ => return Err("host_configuration_unavailable"),
        };
        self.identity.publish_with_configuration_revision(
            pairing_revision,
            configuration_revision,
            snapshot,
        )
    }
}

fn authorize_feedback_scope(
    policy: &EffectAuthoritySettings,
    snapshot: &Snapshot,
    certificate: &[u8],
    scope: &crate::effect_gate::StreamScope,
    now: u64,
) -> Result<(), &'static str> {
    if !scope.valid() {
        return Err("scope_denied");
    }
    for capability in &scope.capabilities {
        let selected = policy
            .feedback_policies
            .iter()
            .find(|rule| &rule.capability == capability)
            .ok_or("scope_denied")?;
        snapshot
            .check_feedback_grant(
                certificate,
                now,
                capability,
                match selected.required_permission {
                    crate::protocol::FeedbackPermission::View => {
                        amaterasu_identity::PERMISSION_VIEW
                    }
                    crate::protocol::FeedbackPermission::Launch => {
                        amaterasu_identity::PERMISSION_LAUNCH
                    }
                },
            )
            .map_err(|_| "scope_denied")?;
    }
    Ok(())
}

fn source_join_available() -> bool {
    cfg!(any(test, feature = "feedback-qualification"))
}

fn is_client_configuration_update(operation: &Operation) -> bool {
    matches!(operation, Operation::UpdateClientConfigOverrides { .. })
}

fn is_snapshot_tuning_update(operation: &Operation, document: &Document) -> bool {
    let Operation::UpdateClientConfigOverrides {
        certificate_id,
        config_overrides,
        ..
    } = operation
    else {
        return false;
    };
    document
        .clients
        .iter()
        .find(|client| &client.certificate_id == certificate_id)
        .is_some_and(|client| client.config_overrides.same_effect_policy(config_overrides))
}

fn read_document(
    payload: &Value,
    now: u64,
    validate_stored_identity: bool,
) -> Result<Document, &'static str> {
    let document = match payload["found"].as_bool() {
        Some(false) => Ok(Document::default()),
        Some(true) => {
            let document: Document =
                serde_json::from_value(payload["value"].clone()).map_err(|_| "invalid_state")?;
            Ok(document)
        }
        None => Err("invalid_state"),
    }?;
    if validate_stored_identity {
        document.validate(now)?;
    }
    Ok(document)
}

fn overlay_document(document: &Document, configured_identity: &ConfiguredIdentity) -> Document {
    let mut overlay = Document {
        identity: document.identity.clone(),
        clients: document.clients.clone(),
        permission_mode_enabled: document.permission_mode_enabled,
    };
    if let Some(identity) = overlay.identity.as_mut() {
        identity.certificate_pem = configured_identity.certificate_pem.clone();
        identity.private_key_pem = configured_identity.private_key_pem.clone();
    }
    overlay
}

fn complete(
    send: &mut impl FnMut(Command) -> Transport,
    request: u64,
    payload: Value,
) -> ModuleResult<()> {
    match send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        _ => Err("pairing completion failure".into()),
    }
}

#[cfg(test)]
mod tests;
