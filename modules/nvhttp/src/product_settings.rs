//! Caller-scoped NVHTTP product settings persisted by the configuration service.
use crate::settings;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const CONFIGURATION_SERVICE: &str = "amaterasu.configuration/1";
const LEGACY_SETTINGS_VERSION: u64 = 1;
const SETTINGS_VERSION: u64 = 2;
const MAX_QUEUED_ADMIN_REQUESTS: usize = 32;
const CONFIGURATION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub enum AdminAction {
    Get,
    Update {
        hostname: String,
        port: u16,
        expected_revision: u64,
        require_listener_apply: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSettings {
    pub hostname: String,
    pub port: u16,
}

struct AdminRequest {
    request: u64,
    deadline: Instant,
    action: AdminAction,
}

struct PendingCall {
    operation_id: String,
    payload: Value,
    timeout_ms: u64,
    parent: Option<u64>,
    purpose: Purpose,
    state: CallState,
}

enum Purpose {
    Load {
        followup: Option<Followup>,
    },
    Update {
        request: u64,
        hostname: String,
        port: u16,
        expected_revision: u64,
        require_listener_apply: bool,
    },
}

enum Followup {
    Conflict { request: u64, revision: Option<u64> },
    Unknown { request: u64, reason: String },
}

enum CallState {
    Submitting,
    Waiting { child: u64 },
    Releasing { child: u64, outcome: CallOutcome },
}

enum CallOutcome {
    Payload(Value),
    Failure { code: String, message: String },
}

#[derive(Debug)]
struct ProductDocument {
    revision: u64,
    hostname: Option<String>,
    port: Option<u16>,
}

/// Maintains the last confirmed product hostname and serializes its CAS writes.
pub struct ProductSettings {
    bootstrap_hostname: String,
    bootstrap_port: u16,
    hostname: String,
    configured_hostname: String,
    port: u16,
    port_configured: bool,
    active_http_port: Option<u16>,
    active_https_port: Option<u16>,
    active_rtsp_base_port: Option<u16>,
    revision: u64,
    loaded: bool,
    last_error: Option<String>,
    listener_error: Option<String>,
    rtsp_listener_error: Option<String>,
    queued: VecDeque<AdminRequest>,
    completions: VecDeque<Completion>,
    pending: Option<PendingCall>,
    sequence: u64,
    next_retry: Instant,
    retry_delay: Duration,
}

struct Completion {
    request: u64,
    payload: Value,
    required_http_port: Option<u16>,
    required_rtsp_port: Option<u16>,
}

impl ProductSettings {
    pub fn new(bootstrap_hostname: String, bootstrap_port: u16) -> Self {
        let bootstrap_port = if settings::validate_port(bootstrap_port).is_ok() {
            bootstrap_port
        } else {
            settings::DEFAULT_PORT
        };
        Self {
            hostname: bootstrap_hostname.clone(),
            configured_hostname: String::new(),
            bootstrap_hostname,
            bootstrap_port,
            port: bootstrap_port,
            port_configured: false,
            active_http_port: None,
            active_https_port: None,
            active_rtsp_base_port: None,
            revision: 0,
            loaded: false,
            last_error: None,
            listener_error: None,
            rtsp_listener_error: None,
            queued: VecDeque::new(),
            completions: VecDeque::new(),
            pending: None,
            sequence: 0,
            next_retry: Instant::now(),
            retry_delay: Duration::from_secs(1),
        }
    }

    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    pub fn configured_hostname(&self) -> &str {
        &self.configured_hostname
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn loaded(&self) -> bool {
        self.loaded
    }

    #[cfg(test)]
    pub(crate) fn active_rtsp_base_port(&self) -> Option<u16> {
        self.active_rtsp_base_port
    }

    #[cfg(test)]
    pub(crate) fn rtsp_listener_error(&self) -> Option<&str> {
        self.rtsp_listener_error.as_deref()
    }

    pub fn set_active_rtsp_base_port(&mut self, port: Option<u16>) {
        self.active_rtsp_base_port = port;
    }

    pub fn rtsp_completion_request(&self, port: u16) -> Option<u64> {
        self.completions
            .iter()
            .find(|completion| completion.required_rtsp_port == Some(port))
            .map(|completion| completion.request)
    }

    #[cfg(test)]
    pub(crate) fn rtsp_completion_payload(&self, request: u64) -> Option<&Value> {
        self.completions
            .iter()
            .find(|completion| completion.request == request)
            .map(|completion| &completion.payload)
    }

    pub fn port_needs_application(&self) -> bool {
        self.loaded
            && self.port_configured
            && (self.active_http_port != Some(self.port)
                || self.active_https_port != Some(settings::https_port(self.port)))
    }

    pub fn port_is_active(&self) -> bool {
        self.loaded
            && self.port_configured
            && self.active_http_port == Some(self.port)
            && self.active_https_port == Some(settings::https_port(self.port))
    }

    pub fn set_active_ports(&mut self, http_port: Option<u16>, https_port: Option<u16>) {
        self.active_http_port = http_port;
        self.active_https_port = https_port;
    }

    pub fn record_port_apply_failure(&mut self, port: u16, message: String) {
        self.listener_error = Some(message.clone());
        if let Some(completion) = self
            .completions
            .iter_mut()
            .find(|completion| completion.required_http_port == Some(port))
        {
            completion.payload = json!({
                "ok":false,
                "code":"listener_apply_failed",
                "message":format!("settings were persisted but listener rebinding failed: {message}"),
                "persisted":true,
            });
            completion.required_http_port = None;
        }
    }

    pub fn record_port_apply_success(&mut self) {
        self.listener_error = None;
    }

    pub fn record_rtsp_apply_failure(&mut self, port: u16, message: String) {
        self.rtsp_listener_error = Some(message.clone());
        if let Some(completion) = self
            .completions
            .iter_mut()
            .find(|completion| completion.required_rtsp_port == Some(port))
        {
            completion.payload = json!({
                "ok":false,
                "code":"listener_apply_pending",
                "message":format!("settings were persisted but RTSP/UDP listeners are not active: {message}"),
                "persisted":true,
                "hostname":self.hostname,
                "revision":self.revision,
            });
            completion.required_rtsp_port = None;
        }
    }

    pub fn record_rtsp_apply_success(&mut self) {
        self.rtsp_listener_error = None;
    }

    pub fn record_listener_disabled(&mut self) {
        if self.port_needs_application() {
            self.listener_error =
                Some("NVHTTP listener is disabled; stored port will apply when enabled".into());
        }
    }

    pub fn revision(&self) -> Option<u64> {
        self.loaded.then_some(self.revision)
    }

    pub fn last_error(&self) -> Option<&str> {
        self.listener_error
            .as_deref()
            .or(self.rtsp_listener_error.as_deref())
            .or(self.last_error.as_deref())
    }

    pub fn enqueue(
        &mut self,
        request: u64,
        remaining_ms: u64,
        action: AdminAction,
    ) -> Result<(), &'static str> {
        let active_admin = self.pending.as_ref().is_some_and(|pending| {
            matches!(&pending.purpose, Purpose::Update { .. })
                || matches!(&pending.purpose, Purpose::Load { followup: Some(_) })
        });
        if self.queued.len() + self.completions.len() + usize::from(active_admin)
            >= MAX_QUEUED_ADMIN_REQUESTS
        {
            return Err("settings request capacity exhausted");
        }
        let timeout = Duration::from_millis(remaining_ms.min(30_000));
        self.queued.push_back(AdminRequest {
            request,
            deadline: Instant::now() + timeout,
            action,
        });
        Ok(())
    }

    /// Advances one configuration call or starts the next required operation.
    /// Returns a hostname only after a complete read or committed update confirms it.
    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<Option<RuntimeSettings>> {
        let previous_hostname = self.hostname.clone();
        let previous_port = self.port;
        self.advance_pending(runtime);
        self.expire_queued();
        if self.pending.is_none() {
            self.start_next();
        }
        let changed = self.hostname != previous_hostname || self.port != previous_port;
        Ok(changed.then(|| RuntimeSettings {
            hostname: self.hostname.clone(),
            port: self.port,
        }))
    }

    fn advance_pending(&mut self, runtime: &RuntimeClient) {
        let Some(mut pending) = self.pending.take() else {
            return;
        };
        match &mut pending.state {
            CallState::Submitting => match runtime.send(request_command(&pending)) {
                Ok(Response::Accepted { id }) => pending.state = CallState::Waiting { child: id },
                Ok(Response::Error { code, message }) => {
                    self.finish_submission_failure(
                        pending.purpose,
                        SettingFailure {
                            code,
                            message,
                            current_revision: None,
                        },
                    );
                    return;
                }
                Ok(response) => {
                    self.finish_submission_failure(
                        pending.purpose,
                        SettingFailure {
                            code: "configuration_unavailable".into(),
                            message: format!("unexpected configuration response: {response:?}"),
                            current_revision: None,
                        },
                    );
                    return;
                }
                Err(error) => {
                    self.last_error = Some(error);
                    self.pending = Some(pending);
                    return;
                }
            },
            CallState::Waiting { child } => match runtime
                .send(Command::TakeResult { request: *child })
            {
                Ok(Response::Pending) => {
                    self.pending = Some(pending);
                    return;
                }
                Ok(Response::Result { payload }) => {
                    pending.state = CallState::Releasing {
                        child: *child,
                        outcome: CallOutcome::Payload(payload),
                    };
                }
                Ok(Response::Error { code, message }) => {
                    if code == "not_found" {
                        self.finish_call(pending.purpose, CallOutcome::Failure { code, message });
                        return;
                    }
                    pending.state = CallState::Releasing {
                        child: *child,
                        outcome: CallOutcome::Failure { code, message },
                    };
                }
                Ok(response) => {
                    pending.state = CallState::Releasing {
                        child: *child,
                        outcome: CallOutcome::Failure {
                            code: "configuration_unavailable".into(),
                            message: format!("unexpected configuration result: {response:?}"),
                        },
                    };
                }
                Err(error) => {
                    self.last_error = Some(error);
                    self.pending = Some(pending);
                    return;
                }
            },
            CallState::Releasing { child, .. } => {
                match runtime.send(Command::ReleaseResult { request: *child }) {
                    Ok(Response::Ok) => {
                        let CallState::Releasing { outcome, .. } = pending.state else {
                            unreachable!()
                        };
                        self.finish_call(pending.purpose, outcome);
                        return;
                    }
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        let CallState::Releasing { outcome, .. } = pending.state else {
                            unreachable!()
                        };
                        self.finish_call(pending.purpose, outcome);
                        return;
                    }
                    Ok(response) => {
                        self.last_error =
                            Some(format!("configuration result cleanup failed: {response:?}"));
                        self.pending = Some(pending);
                        return;
                    }
                    Err(error) => {
                        self.last_error = Some(error);
                        self.pending = Some(pending);
                        return;
                    }
                }
            }
        }

        // A just accepted child is polled on the next tick.
        self.pending = Some(pending);
    }

    fn expire_queued(&mut self) {
        let now = Instant::now();
        let mut active = VecDeque::new();
        while let Some(request) = self.queued.pop_front() {
            if request.deadline <= now {
                self.queue_completion(
                    request.request,
                    failure(
                        "settings_timeout",
                        "settings request expired before execution",
                    ),
                );
            } else {
                active.push_back(request);
            }
        }
        self.queued = active;
    }

    fn start_next(&mut self) {
        if self.completions.iter().any(|completion| {
            completion.required_http_port.is_some() || completion.required_rtsp_port.is_some()
        }) {
            return;
        }
        if !self.loaded {
            if Instant::now() >= self.next_retry {
                self.start_load(None);
            } else if let Some(error) = self.last_error.clone() {
                self.fail_queued(&error);
            }
            return;
        }

        let Some(request) = self.queued.pop_front() else {
            return;
        };
        match request.action {
            AdminAction::Get => {
                self.queue_completion(request.request, self.settings_payload());
            }
            AdminAction::Update {
                hostname,
                port,
                expected_revision,
                require_listener_apply,
            } => {
                if settings::validate_configured_hostname(&hostname).is_err() {
                    self.queue_completion(
                        request.request,
                        failure("invalid_hostname", "hostname is invalid"),
                    );
                    return;
                }
                if settings::validate_port(port).is_err() {
                    self.queue_completion(
                        request.request,
                        failure("invalid_port", "port is outside the supported range"),
                    );
                    return;
                }
                let remaining_ms = request
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64;
                if remaining_ms == 0 {
                    self.queue_completion(
                        request.request,
                        failure(
                            "settings_timeout",
                            "settings request expired before execution",
                        ),
                    );
                    return;
                }
                self.start_call(
                    json!({
                        "action":"product_update",
                        "expected_revision":expected_revision,
                        "version":SETTINGS_VERSION,
                        "values":{"hostname":hostname,"port":port},
                    }),
                    Some(request.request),
                    remaining_ms.min(CONFIGURATION_TIMEOUT.as_millis() as u64),
                    Purpose::Update {
                        request: request.request,
                        hostname,
                        port,
                        expected_revision,
                        require_listener_apply,
                    },
                );
            }
        }
    }

    fn start_load(&mut self, followup: Option<Followup>) {
        self.loaded = false;
        self.start_call(
            json!({"action":"product_get"}),
            None,
            CONFIGURATION_TIMEOUT.as_millis() as u64,
            Purpose::Load { followup },
        );
    }

    fn start_call(
        &mut self,
        payload: Value,
        parent: Option<u64>,
        timeout_ms: u64,
        purpose: Purpose,
    ) {
        let Some(sequence) = self.sequence.checked_add(1) else {
            match purpose {
                Purpose::Load { .. } => {
                    self.record_load_failure("configuration operation identifier exhausted".into())
                }
                Purpose::Update { request, .. } => self.queue_completion(
                    request,
                    failure(
                        "resource_exhausted",
                        "configuration operation identifier exhausted",
                    ),
                ),
            }
            return;
        };
        self.sequence = sequence;
        let action = payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("call");
        self.pending = Some(PendingCall {
            operation_id: format!("nvhttp.product_settings.{sequence}.{action}"),
            payload,
            timeout_ms,
            parent,
            purpose,
            state: CallState::Submitting,
        });
    }

    fn finish_submission_failure(&mut self, purpose: Purpose, error: SettingFailure) {
        match purpose {
            Purpose::Load { .. } => {
                let description = format!("{}: {}", error.code, error.message);
                self.record_load_failure(description.clone());
                self.fail_queued(&description);
            }
            Purpose::Update { request, .. } => {
                self.queue_completion(request, error_response(&error));
            }
        }
    }

    fn finish_call(&mut self, purpose: Purpose, outcome: CallOutcome) {
        match purpose {
            Purpose::Load { followup } => {
                let result = match outcome {
                    CallOutcome::Payload(payload) => parse_document(&payload),
                    CallOutcome::Failure { code, message } => Err(SettingFailure {
                        code,
                        message,
                        current_revision: None,
                    }),
                };
                match result {
                    Ok(document) => {
                        self.apply_document(document);
                        self.record_load_success();
                        if let Some(followup) = followup {
                            self.complete_followup(followup, Ok(()));
                        }
                    }
                    Err(error) => {
                        let description = format!("{}: {}", error.code, error.message);
                        self.record_load_failure(description.clone());
                        self.fail_queued(&description);
                        if let Some(followup) = followup {
                            self.complete_followup(followup, Err(error));
                        }
                    }
                }
            }
            Purpose::Update {
                request,
                hostname,
                port,
                expected_revision,
                require_listener_apply,
            } => match outcome {
                CallOutcome::Payload(payload) => {
                    match update_result(&payload, expected_revision, &hostname, port) {
                        Ok(document) => {
                            let port_changed = self.port != port;
                            self.apply_document(document);
                            self.record_load_success();
                            if !require_listener_apply && port_changed {
                                self.listener_error = Some(
                                    "NVHTTP listener is disabled; stored port is not active".into(),
                                );
                                self.queue_completion(
                                    request,
                                    json!({
                                        "ok":false,
                                        "code":"listener_not_active",
                                        "message":"settings were persisted, but NVHTTP is disabled",
                                        "persisted":true,
                                    }),
                                );
                            } else {
                                self.queue_update_completion(
                                    request,
                                    self.settings_payload(),
                                    require_listener_apply.then_some(port),
                                    require_listener_apply.then_some(port),
                                );
                            }
                        }
                        Err(error) if error.code == "revision_conflict" => {
                            let revision = error.current_revision;
                            self.start_load(Some(Followup::Conflict { request, revision }));
                        }
                        Err(error) if known_precommit_error(&error.code) => {
                            self.queue_completion(request, error_response(&error));
                        }
                        Err(error) => {
                            self.start_load(Some(Followup::Unknown {
                                request,
                                reason: format!("{}: {}", error.code, error.message),
                            }));
                        }
                    }
                }
                CallOutcome::Failure { code, message } => {
                    self.start_load(Some(Followup::Unknown {
                        request,
                        reason: format!("{code}: {message}"),
                    }));
                }
            },
        }
    }

    fn complete_followup(
        &mut self,
        followup: Followup,
        reconciliation: Result<(), SettingFailure>,
    ) {
        let (request, payload) = match followup {
            Followup::Conflict { request, revision } => {
                let current_revision = if reconciliation.is_ok() {
                    Some(self.revision)
                } else {
                    revision
                };
                (
                    request,
                    failure_with_revision(
                        "revision_conflict",
                        "configuration revision differs",
                        current_revision,
                    ),
                )
            }
            Followup::Unknown { request, reason } => {
                let detail = reconciliation
                    .err()
                    .map(|failure| {
                        format!("; readback failed: {}: {}", failure.code, failure.message)
                    })
                    .unwrap_or_default();
                (
                    request,
                    failure(
                        "settings_outcome_unknown",
                        format!("update outcome could not be confirmed ({reason}){detail}"),
                    ),
                )
            }
        };
        self.queue_completion(request, payload);
    }

    fn queue_completion(&mut self, request: u64, payload: Value) {
        self.completions.push_back(Completion {
            request,
            payload,
            required_http_port: None,
            required_rtsp_port: None,
        });
    }

    pub(crate) fn queue_update_completion(
        &mut self,
        request: u64,
        payload: Value,
        required_http_port: Option<u16>,
        required_rtsp_port: Option<u16>,
    ) {
        self.completions.push_back(Completion {
            request,
            payload,
            required_http_port,
            required_rtsp_port,
        });
    }

    pub fn flush_completion(&mut self, runtime: &RuntimeClient) {
        let Some(completion) = self.completions.front() else {
            return;
        };
        if completion.required_http_port.is_some_and(|port| {
            self.active_http_port != Some(port)
                || self.active_https_port != Some(settings::https_port(port))
        }) {
            return;
        }
        if completion
            .required_rtsp_port
            .is_some_and(|port| self.active_rtsp_base_port != Some(port))
        {
            return;
        }
        let mut payload = completion.payload.clone();
        if payload.get("ok").and_then(Value::as_bool) == Some(true) {
            payload["active_http_port"] = json!(self.active_http_port);
            payload["active_https_port"] = json!(self.active_https_port);
            payload["active_rtsp_port"] = json!(self.active_rtsp_base_port);
            payload["listener_error"] = json!(self.listener_error);
            payload["rtsp_listener_error"] = json!(self.rtsp_listener_error);
        }
        match runtime.send(Command::Complete {
            request: completion.request,
            payload,
        }) {
            Ok(Response::Ok) => {
                self.completions.pop_front();
            }
            Ok(Response::Error { code, .. }) if code == "request_ended" || code == "not_found" => {
                self.completions.pop_front();
            }
            Ok(response) => {
                self.last_error =
                    Some(format!("settings response completion failed: {response:?}"));
            }
            Err(error) => self.last_error = Some(error),
        }
    }

    fn apply_document(&mut self, document: ProductDocument) {
        self.revision = document.revision;
        self.configured_hostname = document.hostname.unwrap_or_default();
        self.hostname = if self.configured_hostname.is_empty() {
            self.bootstrap_hostname.clone()
        } else {
            self.configured_hostname.clone()
        };
        self.port_configured = document.port.is_some();
        self.port = document.port.unwrap_or(self.bootstrap_port);
        self.loaded = true;
    }

    fn settings_payload(&self) -> Value {
        json!({
            "ok":true,
            "hostname":self.hostname,
            "configured_hostname":self.configured_hostname,
            "port":self.port,
            "https_port":settings::https_port(self.port),
            "active_http_port":self.active_http_port,
            "active_https_port":self.active_https_port,
            "listener_error":self.listener_error,
            "active_rtsp_port":self.active_rtsp_base_port,
            "rtsp_listener_error":self.rtsp_listener_error,
            "revision":self.revision,
        })
    }

    fn record_load_success(&mut self) {
        self.last_error = None;
        self.retry_delay = Duration::from_secs(1);
        self.next_retry = Instant::now() + self.retry_delay;
    }

    fn record_load_failure(&mut self, message: String) {
        self.loaded = false;
        self.last_error = Some(message);
        self.next_retry = Instant::now() + self.retry_delay;
        self.retry_delay = (self.retry_delay * 2).min(MAX_RETRY_DELAY);
    }

    fn fail_queued(&mut self, reason: &str) {
        while let Some(request) = self.queued.pop_front() {
            self.queue_completion(
                request.request,
                failure(
                    "configuration_unavailable",
                    format!("product settings could not be loaded ({reason})"),
                ),
            );
        }
    }
}

fn request_command(pending: &PendingCall) -> Command {
    Command::Request {
        service: CONFIGURATION_SERVICE.into(),
        payload: pending.payload.clone(),
        timeout_ms: pending.timeout_ms,
        operation_id: pending.operation_id.clone(),
        parent: pending.parent,
    }
}

#[derive(Debug)]
struct SettingFailure {
    code: String,
    message: String,
    current_revision: Option<u64>,
}

fn parse_document(payload: &Value) -> Result<ProductDocument, SettingFailure> {
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(parse_failure(payload));
    }
    let found = payload
        .get("found")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid_state("product_get omitted found"))?;
    let revision = payload
        .get("revision")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_state("product_get omitted revision"))?;
    let values = payload
        .get("values")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_state("product_get values are not an object"))?;

    if !found {
        if revision != 0
            || !payload.get("version").is_some_and(Value::is_null)
            || !values.is_empty()
        {
            return Err(invalid_state(
                "missing product document has inconsistent metadata",
            ));
        }
        return Ok(ProductDocument {
            revision: 0,
            hostname: None,
            port: None,
        });
    }

    let version = payload
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_state("product settings version is missing"))?;
    if !matches!(version, LEGACY_SETTINGS_VERSION | SETTINGS_VERSION) || revision == 0 {
        return Err(invalid_state(
            "product settings version or revision is unsupported",
        ));
    }
    let expected_value_count = if version == LEGACY_SETTINGS_VERSION {
        1
    } else {
        2
    };
    if values.len() != expected_value_count
        || !values.contains_key("hostname")
        || (version == SETTINGS_VERSION && !values.contains_key("port"))
    {
        return Err(invalid_state(
            "product settings contain unknown or missing values",
        ));
    }
    let hostname = values
        .get("hostname")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_state("product hostname is not a string"))?;
    settings::validate_configured_hostname(hostname)
        .map_err(|_| invalid_state("persisted product hostname is invalid"))?;
    let port = if version == LEGACY_SETTINGS_VERSION {
        None
    } else {
        let port = values
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .ok_or_else(|| invalid_state("persisted product port is not a 16-bit integer"))?;
        settings::validate_port(port)
            .map_err(|_| invalid_state("persisted product port is outside the supported range"))?;
        Some(port)
    };
    Ok(ProductDocument {
        revision,
        hostname: Some(hostname.to_owned()),
        port,
    })
}

fn update_result(
    payload: &Value,
    expected_revision: u64,
    hostname: &str,
    port: u16,
) -> Result<ProductDocument, SettingFailure> {
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(parse_failure(payload));
    }
    let document = parse_document(payload)?;
    if document.hostname.as_deref() != Some(hostname)
        || document.port != Some(port)
        || expected_revision.checked_add(1) != Some(document.revision)
    {
        return Err(invalid_state(
            "product_update response does not match the requested write",
        ));
    }
    Ok(document)
}

fn parse_failure(payload: &Value) -> SettingFailure {
    let error = payload.get("error");
    SettingFailure {
        code: error
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("configuration_unavailable")
            .to_owned(),
        message: error
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("configuration service rejected the request")
            .to_owned(),
        current_revision: error
            .and_then(|value| value.get("current_revision"))
            .and_then(Value::as_u64),
    }
}

fn invalid_state(message: &str) -> SettingFailure {
    SettingFailure {
        code: "invalid_state_response".into(),
        message: message.into(),
        current_revision: None,
    }
}

fn known_precommit_error(code: &str) -> bool {
    matches!(
        code,
        "invalid_request" | "invalid_values" | "invalid_version" | "resource_exhausted"
    )
}

fn error_response(error: &SettingFailure) -> Value {
    failure_with_revision(&error.code, &error.message, error.current_revision)
}

fn failure(code: &str, message: impl Into<String>) -> Value {
    json!({"ok":false,"code":code,"message":message.into()})
}

fn failure_with_revision(code: &str, message: &str, revision: Option<u64>) -> Value {
    match revision {
        Some(revision) => {
            json!({"ok":false,"code":code,"message":message,"current_revision":revision})
        }
        None => json!({"ok":false,"code":code,"message":message}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_missing_and_persisted_hostname_documents() {
        let missing = parse_document(&json!({
            "ok":true,"found":false,"version":null,"revision":0,"values":{}
        }))
        .unwrap();
        assert_eq!(missing.revision, 0);
        assert_eq!(missing.hostname, None);
        assert_eq!(missing.port, None);

        let persisted = parse_document(&json!({
            "ok":true,"found":true,"version":1,"revision":7,"values":{"hostname":"Studio PC"}
        }))
        .unwrap();
        assert_eq!(persisted.revision, 7);
        assert_eq!(persisted.hostname.as_deref(), Some("Studio PC"));
        assert_eq!(persisted.port, None);

        let defaulted = parse_document(&json!({
            "ok":true,"found":true,"version":1,"revision":8,"values":{"hostname":""}
        }))
        .unwrap();
        assert_eq!(defaulted.hostname.as_deref(), Some(""));

        let migrated = parse_document(&json!({
            "ok":true,"found":true,"version":2,"revision":9,"values":{"hostname":"Studio PC","port":48000}
        }))
        .unwrap();
        assert_eq!(migrated.hostname.as_deref(), Some("Studio PC"));
        assert_eq!(migrated.port, Some(48_000));
    }

    #[test]
    fn rejects_unsupported_or_malformed_persisted_settings() {
        for payload in [
            json!({"ok":true,"found":true,"version":3,"revision":1,"values":{"hostname":"Host","port":47989}}),
            json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"hostname":"Host","extra":true}}),
            json!({"ok":true,"found":true,"version":2,"revision":1,"values":{"hostname":"Host"}}),
            json!({"ok":true,"found":true,"version":2,"revision":1,"values":{"hostname":"Host","port":1028}}),
            json!({"ok":true,"found":false,"version":null,"revision":1,"values":{}}),
        ] {
            assert_eq!(
                parse_document(&payload).unwrap_err().code,
                "invalid_state_response"
            );
        }
    }

    #[test]
    fn update_reply_must_confirm_the_requested_revision_and_hostname() {
        let success = json!({
            "ok":true,"found":true,"version":2,"revision":4,"values":{"hostname":"New Host","port":48000}
        });
        assert_eq!(
            update_result(&success, 3, "New Host", 48_000)
                .unwrap()
                .revision,
            4
        );
        assert_eq!(
            update_result(&success, 2, "New Host", 48_000)
                .unwrap_err()
                .code,
            "invalid_state_response"
        );
        assert_eq!(
            update_result(&success, 3, "Other Host", 48_000)
                .unwrap_err()
                .code,
            "invalid_state_response"
        );
        assert_eq!(
            update_result(&success, 3, "New Host", 48_001)
                .unwrap_err()
                .code,
            "invalid_state_response"
        );

        let reset_to_machine_name = json!({
            "ok":true,"found":true,"version":2,"revision":5,"values":{"hostname":"","port":48000}
        });
        assert_eq!(
            update_result(&reset_to_machine_name, 4, "", 48_000)
                .unwrap()
                .hostname
                .as_deref(),
            Some("")
        );
    }

    #[test]
    fn bootstrap_hostname_remains_visible_until_a_readback_succeeds() {
        let settings = ProductSettings::new("Bootstrap".into(), settings::DEFAULT_PORT);
        assert_eq!(settings.hostname(), "Bootstrap");
        assert_eq!(settings.configured_hostname(), "");
        assert_eq!(settings.port(), settings::DEFAULT_PORT);
        assert_eq!(settings.revision(), None);
        assert!(!settings.loaded);
    }

    #[test]
    fn empty_stored_name_uses_bootstrap_but_remains_configured_as_empty() {
        let mut settings = ProductSettings::new("Computer Name".into(), settings::DEFAULT_PORT);
        let document = parse_document(&json!({
            "ok":true,"found":true,"version":1,"revision":3,"values":{"hostname":""}
        }))
        .unwrap();
        settings.apply_document(document);

        assert_eq!(settings.hostname(), "Computer Name");
        assert_eq!(settings.configured_hostname(), "");
        assert_eq!(
            settings.settings_payload(),
            json!({
                "ok":true,
                "hostname":"Computer Name",
                "configured_hostname":"",
                "port":47989,
                "https_port":47984,
                "active_http_port":null,
                "active_https_port":null,
                "active_rtsp_port":null,
                "listener_error":null,
                "rtsp_listener_error":null,
                "revision":3,
            })
        );
    }

    #[test]
    fn configured_hostname_is_preserved_exactly_in_effective_and_admin_values() {
        let configured = "  Living Room & Games  ";
        let mut settings = ProductSettings::new("Computer Name".into(), settings::DEFAULT_PORT);
        let document = parse_document(&json!({
            "ok":true,"found":true,"version":1,"revision":4,"values":{"hostname":configured}
        }))
        .unwrap();
        settings.apply_document(document);

        assert_eq!(settings.hostname(), configured);
        assert_eq!(settings.configured_hostname(), configured);
        assert_eq!(
            settings.settings_payload()["configured_hostname"],
            configured
        );
    }

    #[test]
    fn legacy_document_keeps_bootstrap_port_and_v2_port_is_applied() {
        let mut settings = ProductSettings::new("Computer Name".into(), 48_100);
        let legacy = parse_document(&json!({
            "ok":true,"found":true,"version":1,"revision":2,"values":{"hostname":"Old Host"}
        }))
        .unwrap();
        settings.apply_document(legacy);
        assert_eq!(settings.port(), 48_100);
        assert!(!settings.port_needs_application());

        let current = parse_document(&json!({
            "ok":true,"found":true,"version":2,"revision":3,"values":{"hostname":"Old Host","port":48000}
        }))
        .unwrap();
        settings.apply_document(current);
        assert_eq!(settings.port(), 48_000);
        assert!(settings.port_needs_application());

        settings.set_active_ports(Some(48_000), Some(47_995));
        assert!(!settings.port_needs_application());
        assert_eq!(settings.settings_payload()["https_port"], 47_995);
        assert_eq!(settings.settings_payload()["active_http_port"], 48_000);
    }

    #[test]
    fn disabled_nvhttp_persists_a_port_but_never_claims_it_is_active() {
        let mut settings = ProductSettings::new("Computer Name".into(), settings::DEFAULT_PORT);
        settings.apply_document(
            parse_document(&json!({
                "ok":true,"found":true,"version":1,"revision":1,"values":{"hostname":"Old Host"}
            }))
            .unwrap(),
        );
        assert!(!crate::should_attempt_listener_rebind(None, &settings));

        settings.finish_call(
            Purpose::Update {
                request: 12,
                hostname: "New Host".into(),
                port: 48_000,
                expected_revision: 1,
                require_listener_apply: false,
            },
            CallOutcome::Payload(json!({
                "ok":true,
                "found":true,
                "version":2,
                "revision":2,
                "values":{"hostname":"New Host","port":48000}
            })),
        );

        assert_eq!(settings.port(), 48_000);
        assert!(settings.port_needs_application());
        assert!(!crate::should_attempt_listener_rebind(None, &settings));
        let completion = settings.completions.front().unwrap();
        assert_eq!(completion.payload["ok"], false);
        assert_eq!(completion.payload["code"], "listener_not_active");
        assert_eq!(completion.payload["persisted"], true);
    }

    #[test]
    fn failed_live_port_apply_never_reports_a_committed_update_as_applied() {
        let mut settings = ProductSettings::new("Computer Name".into(), settings::DEFAULT_PORT);
        settings.set_active_ports(Some(47_989), Some(47_984));
        settings.queue_update_completion(
            12,
            settings.settings_payload(),
            Some(48_000),
            Some(48_000),
        );

        settings.record_port_apply_failure(48_000, "HTTPS listener unavailable".into());

        let completion = settings.completions.front().unwrap();
        assert_eq!(completion.request, 12);
        assert_eq!(completion.payload["ok"], false);
        assert_eq!(completion.payload["code"], "listener_apply_failed");
        assert_eq!(completion.payload["persisted"], true);
        assert_eq!(completion.required_http_port, None);
    }

    #[test]
    fn settings_update_waits_for_rtsp_base_port_and_reports_persisted_pending_on_failure() {
        let mut settings = ProductSettings::new("Computer Name".into(), settings::DEFAULT_PORT);
        settings.apply_document(
            parse_document(&json!({
                "ok":true,"found":true,"version":2,"revision":2,
                "values":{"hostname":"Old Host","port":47989}
            }))
            .unwrap(),
        );
        settings.set_active_ports(Some(47_989), Some(47_984));
        settings.queue_update_completion(
            77,
            settings.settings_payload(),
            Some(47_989),
            Some(47_989),
        );

        assert_eq!(settings.rtsp_completion_request(47_989), Some(77));
        assert_eq!(settings.completions.front().unwrap().payload["ok"], true);

        settings.record_rtsp_apply_failure(47_989, "listener_apply_pending: active stream".into());
        assert_eq!(settings.rtsp_completion_request(47_989), None);
        let completion = settings.completions.front().unwrap();
        let payload = &completion.payload;
        assert_eq!(payload["ok"], false);
        assert_eq!(payload["code"], "listener_apply_pending");
        assert_eq!(payload["persisted"], true);
        assert_eq!(
            settings.settings_payload()["rtsp_listener_error"],
            "listener_apply_pending: active stream"
        );
    }
}
