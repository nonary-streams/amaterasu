use crate::engine::{Change, Engine};
use crate::identity::{
    ConfiguredIdentity, IMPORTED_IDENTITY_MIGRATION_KEY, ImportedIdentityReconciliation, Shared,
    Snapshot,
};
use crate::identity_generation::{Generator, IdentityWorker};
use crate::protocol::{Document, Identity, Operation, Settings, error};
use crate::{SERVICE, STATE_SERVICE};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

pub struct Pipeline {
    engine: Engine,
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
        Self {
            engine: Engine::new(settings),
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

    pub fn invalidate_identity(&self) {
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
        if self.identity_generation.is_some() {
            let result = self.advance_identity_generation(now, &mut send);
            if result.is_err() {
                self.identity.invalidate();
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
                self.reject_unconfigured(&mut send)
            }
            Ok(false) => Ok(()),
            Err(failure) => Err(failure),
        };
        if result.is_err() {
            self.identity.invalidate();
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
                if now < *refresh_at || self.pending.is_some() =>
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
                if self
                    .refresh_identity
                    .as_ref()
                    .is_some_and(|previous| previous.provider_revision != revision)
                {
                    self.identity.invalidate();
                    self.recovery_needed = true;
                }
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
            self.identity.invalidate();
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
                    Some(_) => {
                        self.identity.invalidate();
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
        self.identity.invalidate();
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
                service,
                payload,
                remaining_ms,
                ..
            } => {
                if let Some((_, stale, _)) = &mut self.recovery_request {
                    *stale = true;
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
                if operation.may_write() && !is_client_configuration_update(&operation) {
                    self.identity.invalidate();
                    self.recovery_needed = true;
                }
                let request = Request {
                    id,
                    caller,
                    deadline: Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
                };
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
                self.identity.invalidate();
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
                    } => {
                        if payload["revision"].as_u64() != Some(expected_ack_revision) {
                            self.identity.invalidate();
                            self.recovery_needed = true;
                            return complete(send, pending.request.id, error("state_unavailable"));
                        }
                        self.publish_identity(expected_ack_revision, snapshot)?;
                        self.recovery_needed = false;
                        complete(send, pending.request.id, response)
                    }
                    Stage::Loading(operation) => {
                        if Instant::now() >= pending.request.deadline {
                            if is_client_configuration_update(&operation) {
                                self.identity.invalidate();
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
                                self.identity.invalidate();
                                self.recovery_needed = true;
                            }
                            return complete(send, pending.request.id, error("request_ended"));
                        }
                        let configured_identity = self.configured_identity();
                        let document =
                            match read_document(&payload, now, configured_identity.is_none()) {
                                Ok(document) => document,
                                Err(code) => {
                                    self.identity.invalidate();
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
                self.identity.invalidate();
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
                self.identity.invalidate();
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
                self.identity.invalidate();
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
        let configuration_only = is_client_configuration_update(&operation);
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
                    self.identity.invalidate();
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
                    self.identity.invalidate();
                    self.recovery_needed = true;
                }
                let Some(expected_ack_revision) = revision.checked_add(1) else {
                    self.identity.invalidate();
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
                        self.identity.invalidate();
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

    fn publish_identity(
        &self,
        pairing_revision: u64,
        snapshot: Option<Arc<Snapshot>>,
    ) -> Result<(), &'static str> {
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

fn is_client_configuration_update(operation: &Operation) -> bool {
    matches!(operation, Operation::UpdateClientConfigOverrides { .. })
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
