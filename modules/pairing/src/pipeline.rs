use crate::engine::{Change, Engine};
use crate::identity::{ConfiguredIdentity, Shared, Snapshot};
use crate::protocol::{Document, Operation, Settings, error};
use crate::{SERVICE, STATE_SERVICE};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const STATE_KEY: &str = "pairing/v1";
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
enum Stage {
    Loading(Box<Operation>),
    Saving {
        response: Value,
        snapshot: Option<Arc<Snapshot>>,
    },
}

pub struct Pipeline {
    engine: Engine,
    pending: Option<Pending>,
    deferred_import: Option<DeferredImport>,
    identity: Arc<Shared>,
    host_configuration: HostConfigurationState,
    host_configuration_retry_seconds: u64,
}

enum HostConfigurationState {
    Disabled,
    Start,
    WaitingReadiness(u64),
    WaitingGet(u64),
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
            pending: None,
            deferred_import: None,
            identity,
            host_configuration,
            host_configuration_retry_seconds: 1,
        }
    }

    pub fn invalidate_identity(&self) {
        self.identity.invalidate();
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
        let result = match self.advance_host_configuration(now, &mut send) {
            Ok(true) => self.drive(now, &mut send),
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
        }
        result
    }

    fn advance_host_configuration(
        &mut self,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<bool> {
        match &self.host_configuration {
            HostConfigurationState::Disabled => return Ok(true),
            HostConfigurationState::Ready { refresh_at, .. } if now < *refresh_at => {
                return Ok(true);
            }
            HostConfigurationState::Failed { retry_at } if now < *retry_at => return Ok(false),
            HostConfigurationState::Ready { .. } | HostConfigurationState::Failed { .. } => {
                self.identity.invalidate();
                self.host_configuration = HostConfigurationState::Start;
            }
            HostConfigurationState::WaitingReadiness(_) | HostConfigurationState::WaitingGet(_) => {
            }
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
                match ConfiguredIdentity::load(certificate_path, private_key_path, revision, now) {
                    Ok(identity) => {
                        self.host_configuration = HostConfigurationState::Ready {
                            identity: Arc::new(identity),
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
            _ => {
                self.fail_host_configuration(now);
                Ok(false)
            }
        }
    }

    fn fail_host_configuration(&mut self, now: u64) {
        self.host_configuration = HostConfigurationState::Failed {
            retry_at: now.saturating_add(self.host_configuration_retry_seconds),
        };
        self.host_configuration_retry_seconds =
            (self.host_configuration_retry_seconds * 2).min(HOST_CONFIGURATION_MAX_RETRY_SECONDS);
        self.identity.invalidate();
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
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                service,
                payload,
                remaining_ms,
                ..
            } => {
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
                if operation.may_write() {
                    self.identity.invalidate();
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
                    Stage::Saving { response, snapshot } => {
                        self.publish_identity(payload["revision"].as_u64().unwrap(), snapshot)?;
                        complete(send, pending.request.id, response)
                    }
                    Stage::Loading(operation) => {
                        if Instant::now() >= pending.request.deadline {
                            return complete(send, pending.request.id, error("deadline_exceeded"));
                        }
                        if !matches!(
                            send(Command::CheckRequest {
                                request: pending.request.id
                            })?,
                            Response::Ok
                        ) {
                            return complete(send, pending.request.id, error("request_ended"));
                        }
                        let configured_identity = self.configured_identity();
                        let document =
                            match read_document(&payload, now, configured_identity.is_none()) {
                                Ok(document) => document,
                                Err(code) => {
                                    self.identity.invalidate();
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
                complete(send, pending.request.id, error("state_unavailable"))
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
        let loaded = match self.prepare_snapshot(&document, now) {
            Ok(snapshot) => snapshot,
            Err(code) => {
                self.identity.invalidate();
                return complete(send, request.id, error(code));
            }
        };
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
        if let Operation::UpdateClientConfigOverrides {
            expected_revision, ..
        } = &operation
            && *expected_revision != revision
        {
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
        let mut initialized_document = None;
        let mut engine_now = now;
        let engine_document = match self.configured_identity() {
            Some(configured_identity) if initialize && persisted_identity.is_none() => {
                let generated_identity = match crate::crypto::generate_identity() {
                    Ok(identity) => identity,
                    Err(code) => {
                        self.publish_identity(revision, loaded)?;
                        return complete(send, request.id, error(code));
                    }
                };
                engine_now = match SystemTime::now().duration_since(UNIX_EPOCH) {
                    Ok(current_time) => now.max(current_time.as_secs()),
                    Err(_) => {
                        self.publish_identity(revision, loaded)?;
                        return complete(send, request.id, error("clock_failure"));
                    }
                };
                let durable_document = Document {
                    identity: Some(generated_identity),
                    clients: document.clients,
                };
                if let Err(code) = durable_document.validate(engine_now) {
                    self.publish_identity(revision, loaded)?;
                    return complete(send, request.id, error(code));
                }
                let engine_document = overlay_document(&durable_document, configured_identity);
                initialized_document = Some(durable_document);
                engine_document
            }
            Some(configured_identity) => overlay_document(&document, configured_identity),
            None => document,
        };
        match self
            .engine
            .apply(&request.caller, operation, engine_document, engine_now)
        {
            Ok(Change::Read(mut response)) => {
                if include_revision {
                    response["revision"] = revision.into();
                }
                if let Some(document) = initialized_document {
                    self.identity.invalidate();
                    let snapshot = match self.prepare_snapshot(&document, engine_now) {
                        Ok(snapshot) => snapshot,
                        Err(code) => {
                            return complete(send, request.id, error(code));
                        }
                    };
                    return self.submit(
                        request,
                        Stage::Saving { response, snapshot },
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
                self.identity.invalidate();
                let snapshot = match self.prepare_snapshot(&document, now) {
                    Ok(snapshot) => snapshot,
                    Err(code) => {
                        return complete(send, request.id, error(code));
                    }
                };
                self.submit(request,Stage::Saving {response,snapshot},json!({"action":"set","key":STATE_KEY,"expected_revision":revision,"value":document}),send)
            }
            Err(code) => {
                self.publish_identity(revision, loaded)?;
                complete(send, request.id, error(code))
            }
        }
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
