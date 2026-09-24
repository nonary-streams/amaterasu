//! Caller-scoped NVHTTP product settings persisted by the configuration service.
use crate::settings::{self, AddressFamily, Settings};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const CONFIGURATION_SERVICE: &str = "amaterasu.configuration/1";
const LEGACY_SETTINGS_VERSION: u64 = 1;
const PORT_SETTINGS_VERSION: u64 = 2;
const NETWORK_SETTINGS_VERSION: u64 = 3;
const SETTINGS_VERSION: u64 = 4;
const MAX_OUTPUT_NAME_BYTES: usize = 512;
const MAX_QUEUED_ADMIN_REQUESTS: usize = 32;
const CONFIGURATION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub enum AdminAction {
    Get,
    Update {
        hostname: String,
        port: u16,
        address_family: FieldUpdate<AddressFamily>,
        bind_address: FieldUpdate<String>,
        output_name: FieldUpdate<String>,
        expected_revision: u64,
        require_listener_apply: bool,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum FieldUpdate<T> {
    #[default]
    Omitted,
    Null,
    Value(T),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkTarget {
    pub port: u16,
    pub address_family: Option<AddressFamily>,
    pub bind_address: Option<String>,
}

impl From<u16> for NetworkTarget {
    fn from(port: u16) -> Self {
        Self {
            port,
            address_family: None,
            bind_address: None,
        }
    }
}

impl From<&NetworkTarget> for NetworkTarget {
    fn from(target: &NetworkTarget) -> Self {
        target.clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSettings {
    pub hostname: String,
    pub port: u16,
    pub address_family: Option<AddressFamily>,
    pub bind_address: Option<String>,
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
        target: NetworkTarget,
        output_name: Option<String>,
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
    address_family: Option<AddressFamily>,
    bind_address: Option<String>,
    output_name: Option<String>,
}

/// Maintains the last confirmed product hostname and serializes its CAS writes.
pub struct ProductSettings {
    bootstrap_settings: Settings,
    bootstrap_hostname: String,
    bootstrap_port: u16,
    bootstrap_address_family: Option<AddressFamily>,
    bootstrap_bind_address: Option<String>,
    bootstrap_http_ip: std::net::IpAddr,
    hostname: String,
    configured_hostname: String,
    port: u16,
    configured_address_family: Option<AddressFamily>,
    configured_bind_address: Option<String>,
    configured_output_name: Option<String>,
    active_http_port: Option<u16>,
    active_https_port: Option<u16>,
    active_rtsp_base_port: Option<u16>,
    active_http_addresses: Option<Vec<String>>,
    active_https_addresses: Option<Vec<String>>,
    active_rtsp_addresses: Option<Vec<String>>,
    active_http_target: Option<NetworkTarget>,
    active_rtsp_target: Option<NetworkTarget>,
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
    required_http_target: Option<NetworkTarget>,
    required_rtsp_target: Option<NetworkTarget>,
}

impl ProductSettings {
    #[cfg(test)]
    pub fn new(bootstrap_hostname: String, bootstrap_port: u16) -> Self {
        Self::with_bootstrap_listener(
            bootstrap_hostname,
            bootstrap_port,
            None,
            None,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            false,
            Settings::default(),
        )
    }

    pub fn with_bootstrap_settings(bootstrap_hostname: String, settings: &Settings) -> Self {
        Self::with_bootstrap_listener(
            bootstrap_hostname,
            settings.http_bind.port(),
            settings.address_family,
            settings.bind_address.clone(),
            settings.http_bind.ip(),
            settings.allow_remote,
            settings.clone(),
        )
    }

    fn with_bootstrap_listener(
        bootstrap_hostname: String,
        bootstrap_port: u16,
        bootstrap_address_family: Option<AddressFamily>,
        bootstrap_bind_address: Option<String>,
        bootstrap_http_ip: std::net::IpAddr,
        bootstrap_allow_remote: bool,
        mut bootstrap_settings: Settings,
    ) -> Self {
        let bootstrap_port = if settings::validate_port(bootstrap_port).is_ok() {
            bootstrap_port
        } else {
            settings::DEFAULT_PORT
        };
        bootstrap_settings.http_bind.set_port(bootstrap_port);
        bootstrap_settings
            .https_bind
            .set_port(settings::https_port(bootstrap_port));
        bootstrap_settings.address_family = bootstrap_address_family;
        bootstrap_settings.bind_address = bootstrap_bind_address.clone();
        bootstrap_settings.allow_remote = bootstrap_allow_remote;
        Self {
            bootstrap_settings,
            hostname: bootstrap_hostname.clone(),
            configured_hostname: String::new(),
            bootstrap_hostname,
            bootstrap_port,
            bootstrap_address_family,
            bootstrap_bind_address,
            bootstrap_http_ip,
            port: bootstrap_port,
            configured_address_family: None,
            configured_bind_address: None,
            configured_output_name: None,
            active_http_port: None,
            active_https_port: None,
            active_rtsp_base_port: None,
            active_http_addresses: None,
            active_https_addresses: None,
            active_rtsp_addresses: None,
            active_http_target: None,
            active_rtsp_target: None,
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

    pub fn configured_output_name(&self) -> Option<&str> {
        self.configured_output_name.as_deref()
    }

    pub fn effective_output_name(&self) -> Option<&str> {
        self.configured_output_name.as_deref().or_else(|| {
            self.bootstrap_settings
                .streaming
                .as_ref()
                .map(|streaming| streaming.capture.output.as_str())
        })
    }

    pub fn capture_provider(&self) -> Option<&'static str> {
        self.bootstrap_settings.streaming.as_ref().map(|streaming| {
            match streaming.capture.capture_provider {
                crate::launch::CaptureProvider::Dxgi => "dxgi",
                crate::launch::CaptureProvider::Wgc => "wgc",
            }
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn http_target(&self) -> NetworkTarget {
        NetworkTarget {
            port: self.port,
            address_family: self
                .configured_address_family
                .or(self.bootstrap_address_family),
            bind_address: self
                .configured_bind_address
                .clone()
                .or_else(|| self.bootstrap_bind_address.clone()),
        }
    }

    /// `None` values deliberately pass through to RTSP's own startup binds.
    pub fn rtsp_target(&self) -> NetworkTarget {
        NetworkTarget {
            port: self.port,
            address_family: self.configured_address_family,
            bind_address: self.configured_bind_address.clone(),
        }
    }

    pub fn effective_address_family(&self) -> AddressFamily {
        if let Some(addresses) = &self.active_http_addresses {
            let has_v6 = addresses
                .iter()
                .filter_map(|address| address.parse::<std::net::SocketAddr>().ok())
                .any(|address| address.is_ipv6());
            return if has_v6 {
                AddressFamily::Both
            } else {
                AddressFamily::Ipv4
            };
        }
        let target = self.http_target();
        self.bootstrap_settings.effective_address_family(
            target.address_family,
            target.bind_address.as_deref(),
            self.bootstrap_http_ip,
        )
    }

    pub fn effective_bind_address(&self) -> String {
        self.active_http_addresses
            .as_ref()
            .and_then(|addresses| addresses.first())
            .and_then(|address| address.parse::<std::net::SocketAddr>().ok())
            .map(|address| address.ip().to_string())
            .or_else(|| {
                self.http_target()
                    .bind_address
                    .filter(|address| !address.trim().is_empty())
            })
            .unwrap_or_else(|| self.bootstrap_http_ip.to_string())
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

    pub fn set_active_http_state(
        &mut self,
        target: Option<NetworkTarget>,
        http_addresses: Option<Vec<String>>,
        https_addresses: Option<Vec<String>>,
    ) {
        self.active_http_target = target;
        self.set_active_http_addresses(http_addresses, https_addresses);
    }

    pub fn set_active_http_addresses(
        &mut self,
        http_addresses: Option<Vec<String>>,
        https_addresses: Option<Vec<String>>,
    ) {
        self.active_http_addresses = http_addresses;
        self.active_https_addresses = https_addresses;
        self.active_http_port = self
            .active_http_addresses
            .as_ref()
            .and_then(|addresses| addresses.first())
            .and_then(|address| address.parse::<std::net::SocketAddr>().ok())
            .map(|address| address.port());
        self.active_https_port = self
            .active_https_addresses
            .as_ref()
            .and_then(|addresses| addresses.first())
            .and_then(|address| address.parse::<std::net::SocketAddr>().ok())
            .map(|address| address.port());
    }

    pub fn set_active_http_target(&mut self, target: NetworkTarget) {
        self.active_http_target = Some(target);
    }

    pub fn set_active_rtsp_state(
        &mut self,
        target: Option<NetworkTarget>,
        port: Option<u16>,
        addresses: Option<Vec<String>>,
    ) {
        self.active_rtsp_target = target;
        self.active_rtsp_base_port = port;
        self.active_rtsp_addresses = addresses;
    }

    pub fn rtsp_completion_request(&self, target: impl Into<NetworkTarget>) -> Option<u64> {
        let target = target.into();
        self.completions
            .iter()
            .find(|completion| completion.required_rtsp_target.as_ref() == Some(&target))
            .map(|completion| completion.request)
    }

    #[cfg(test)]
    pub(crate) fn rtsp_completion_payload(&self, request: u64) -> Option<&Value> {
        self.completions
            .iter()
            .find(|completion| completion.request == request)
            .map(|completion| &completion.payload)
    }

    pub fn listeners_need_application(&self) -> bool {
        if !self.loaded || self.active_http_target.as_ref() == Some(&self.http_target()) {
            return false;
        }
        let bootstrap_target = NetworkTarget {
            port: self.bootstrap_port,
            address_family: self.bootstrap_address_family,
            bind_address: self.bootstrap_bind_address.clone(),
        };
        self.active_http_target.is_some() || self.http_target() != bootstrap_target
    }

    #[cfg(test)]
    fn port_needs_application(&self) -> bool {
        self.listeners_need_application()
    }

    #[cfg(test)]
    pub fn set_active_ports(&mut self, http_port: Option<u16>, https_port: Option<u16>) {
        self.active_http_port = http_port;
        self.active_https_port = https_port;
        if let Some(port) = http_port {
            let mut target = self.http_target();
            target.port = port;
            self.active_http_target = Some(target);
        }
    }

    #[cfg(test)]
    pub(crate) fn set_active_rtsp_base_port(&mut self, port: Option<u16>) {
        self.active_rtsp_base_port = port;
        self.active_rtsp_target = port.map(|port| {
            let mut target = self.rtsp_target();
            target.port = port;
            target
        });
    }

    #[cfg(test)]
    fn record_port_apply_failure(&mut self, port: u16, message: String) {
        let mut target = self.http_target();
        target.port = port;
        self.record_listener_apply_failure(&target, message);
    }

    pub fn record_listener_apply_failure(&mut self, target: &NetworkTarget, message: String) {
        self.listener_error = Some(message.clone());
        if let Some(completion) = self
            .completions
            .iter_mut()
            .find(|completion| completion.required_http_target.as_ref() == Some(target))
        {
            completion.payload = json!({
                "ok":false,
                "code":"listener_apply_failed",
                "message":format!("settings were persisted but listener rebinding failed: {message}"),
                "persisted":true,
            });
            completion.required_http_target = None;
        }
    }

    pub fn record_listener_apply_success(&mut self) {
        self.listener_error = None;
    }

    pub fn record_rtsp_apply_failure(&mut self, target: impl Into<NetworkTarget>, message: String) {
        let target = target.into();
        self.rtsp_listener_error = Some(message.clone());
        let hostname = self.hostname.clone();
        let revision = self.revision;
        let address_family = self.effective_address_family();
        let bind_address = self.effective_bind_address();
        let configured_address_family = self.configured_address_family;
        let configured_bind_address = self.configured_bind_address.clone();
        if let Some(completion) = self
            .completions
            .iter_mut()
            .find(|completion| completion.required_rtsp_target.as_ref() == Some(&target))
        {
            completion.payload = json!({
                "ok":false,
                "code":"listener_apply_pending",
                "message":format!("settings were persisted but RTSP/UDP listeners are not active: {message}"),
                "persisted":true,
                "hostname":hostname,
                "revision":revision,
                "address_family":address_family,
                "bind_address":bind_address,
                "configured_address_family":configured_address_family,
                "configured_bind_address":configured_bind_address,
            });
            completion.required_rtsp_target = None;
        }
    }

    pub fn record_rtsp_apply_success(&mut self) {
        self.rtsp_listener_error = None;
    }

    pub fn record_listener_disabled(&mut self) {
        if self.listeners_need_application() {
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
        let previous_target = self.http_target();
        self.advance_pending(runtime);
        self.expire_queued();
        if self.pending.is_none() {
            self.start_next();
        }
        let target = self.http_target();
        let changed = self.hostname != previous_hostname || target != previous_target;
        Ok(changed.then(|| RuntimeSettings {
            hostname: self.hostname.clone(),
            port: target.port,
            address_family: target.address_family,
            bind_address: target.bind_address,
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
            completion.required_http_target.is_some() || completion.required_rtsp_target.is_some()
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
                address_family,
                bind_address,
                output_name,
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
                let output_name = match apply_output_name_update(
                    output_name,
                    self.configured_output_name.clone(),
                ) {
                    Ok(output_name) => output_name,
                    Err(()) => {
                        self.queue_completion(
                            request.request,
                            failure("invalid_output_name", "output name is invalid"),
                        );
                        return;
                    }
                };
                let target = NetworkTarget {
                    port,
                    address_family: apply_field_update(
                        address_family,
                        self.configured_address_family,
                    ),
                    bind_address: apply_field_update(
                        bind_address,
                        self.configured_bind_address.clone(),
                    ),
                };
                let mut validation_settings = self.bootstrap_settings.clone();
                validation_settings.http_bind.set_port(port);
                validation_settings
                    .https_bind
                    .set_port(settings::https_port(port));
                validation_settings.address_family =
                    target.address_family.or(self.bootstrap_address_family);
                validation_settings.bind_address = target
                    .bind_address
                    .clone()
                    .or_else(|| self.bootstrap_bind_address.clone());
                if validation_settings
                    .validate_network_policy(
                        validation_settings.address_family,
                        validation_settings.bind_address.as_deref(),
                    )
                    .is_err()
                {
                    self.queue_completion(
                        request.request,
                        failure(
                            "invalid_network_policy",
                            "listener address policy is invalid",
                        ),
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
                    product_update_payload(
                        expected_revision,
                        &hostname,
                        &target,
                        output_name.as_deref(),
                    ),
                    Some(request.request),
                    remaining_ms.min(CONFIGURATION_TIMEOUT.as_millis() as u64),
                    Purpose::Update {
                        request: request.request,
                        hostname,
                        target,
                        output_name,
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
                    Ok(document) if self.validate_document(&document).is_ok() => {
                        self.apply_document(document);
                        self.record_load_success();
                        if let Some(followup) = followup {
                            self.complete_followup(followup, Ok(()));
                        }
                    }
                    Ok(_) => {
                        let error = invalid_state("persisted listener address policy is invalid");
                        let description = format!("{}: {}", error.code, error.message);
                        self.record_load_failure(description.clone());
                        self.fail_queued(&description);
                        if let Some(followup) = followup {
                            self.complete_followup(followup, Err(error));
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
                target,
                output_name,
                expected_revision,
                require_listener_apply,
            } => match outcome {
                CallOutcome::Payload(payload) => {
                    match update_result(
                        &payload,
                        expected_revision,
                        &hostname,
                        &target,
                        output_name.as_deref(),
                    ) {
                        Ok(document) if self.validate_document(&document).is_ok() => {
                            let previous_http_target = self.http_target();
                            let previous_rtsp_target = self.rtsp_target();
                            self.apply_document(document);
                            let desired_http_target = self.http_target();
                            let desired_rtsp_target = self.rtsp_target();
                            let network_changed = previous_http_target != desired_http_target
                                || previous_rtsp_target != desired_rtsp_target;
                            self.record_load_success();
                            if !require_listener_apply && network_changed {
                                self.listener_error = Some(
                                    "NVHTTP listener is disabled; stored network settings are not active".into(),
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
                                self.queue_listener_update_completion(
                                    request,
                                    self.settings_payload(),
                                    (require_listener_apply
                                        && self.active_http_target.as_ref()
                                            != Some(&desired_http_target))
                                    .then_some(desired_http_target),
                                    (require_listener_apply
                                        && self.active_rtsp_target.as_ref()
                                            != Some(&desired_rtsp_target))
                                    .then_some(desired_rtsp_target),
                                );
                            }
                        }
                        Ok(_) => {
                            self.start_load(Some(Followup::Unknown {
                                request,
                                reason: "product_update committed invalid settings".into(),
                            }));
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
            required_http_target: None,
            required_rtsp_target: None,
        });
    }

    pub(crate) fn queue_listener_update_completion(
        &mut self,
        request: u64,
        payload: Value,
        required_http_target: Option<NetworkTarget>,
        required_rtsp_target: Option<NetworkTarget>,
    ) {
        self.completions.push_back(Completion {
            request,
            payload,
            required_http_target,
            required_rtsp_target,
        });
    }

    #[cfg(test)]
    pub(crate) fn queue_update_completion(
        &mut self,
        request: u64,
        payload: Value,
        required_http_port: Option<u16>,
        required_rtsp_port: Option<u16>,
    ) {
        let http_target = required_http_port.map(|port| {
            let mut target = self.http_target();
            target.port = port;
            target
        });
        let rtsp_target = required_rtsp_port.map(|port| {
            let mut target = self.rtsp_target();
            target.port = port;
            target
        });
        self.queue_listener_update_completion(request, payload, http_target, rtsp_target);
    }

    pub fn flush_completion(&mut self, runtime: &RuntimeClient) {
        let Some(completion) = self.completions.front() else {
            return;
        };
        if completion
            .required_http_target
            .as_ref()
            .is_some_and(|target| {
                self.active_http_target.as_ref() != Some(target)
                    || self.active_http_port != Some(target.port)
                    || self.active_https_port != Some(settings::https_port(target.port))
            })
        {
            return;
        }
        if completion
            .required_rtsp_target
            .as_ref()
            .is_some_and(|target| {
                self.active_rtsp_target.as_ref() != Some(target)
                    || self.active_rtsp_base_port != Some(target.port)
            })
        {
            return;
        }
        let mut payload = completion.payload.clone();
        if payload.get("ok").and_then(Value::as_bool) == Some(true) {
            payload["active_http_port"] = json!(self.active_http_port);
            payload["active_https_port"] = json!(self.active_https_port);
            payload["active_rtsp_port"] = json!(self.active_rtsp_base_port);
            payload["http_addresses"] = json!(self.active_http_addresses);
            payload["https_addresses"] = json!(self.active_https_addresses);
            payload["rtsp_addresses"] = json!(self.active_rtsp_addresses);
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

    fn validate_document(&self, document: &ProductDocument) -> Result<(), SettingFailure> {
        if document
            .output_name
            .as_deref()
            .is_some_and(|name| normalize_output_name(name).is_err())
        {
            return Err(invalid_state("persisted output name is invalid"));
        }
        let mut validation_settings = self.bootstrap_settings.clone();
        let port = document.port.unwrap_or(self.bootstrap_port);
        validation_settings.http_bind.set_port(port);
        validation_settings
            .https_bind
            .set_port(settings::https_port(port));
        validation_settings.address_family =
            document.address_family.or(self.bootstrap_address_family);
        validation_settings.bind_address = document
            .bind_address
            .clone()
            .or_else(|| self.bootstrap_bind_address.clone());
        validation_settings
            .validate_network_policy(
                validation_settings.address_family,
                validation_settings.bind_address.as_deref(),
            )
            .map_err(|_| invalid_state("persisted listener address policy is invalid"))
    }

    fn apply_document(&mut self, document: ProductDocument) {
        self.revision = document.revision;
        self.configured_hostname = document.hostname.unwrap_or_default();
        self.hostname = if self.configured_hostname.is_empty() {
            self.bootstrap_hostname.clone()
        } else {
            self.configured_hostname.clone()
        };
        self.port = document.port.unwrap_or(self.bootstrap_port);
        self.configured_address_family = document.address_family;
        self.configured_bind_address = document.bind_address;
        self.configured_output_name = document.output_name;
        self.loaded = true;
    }

    fn settings_payload(&self) -> Value {
        json!({
            "ok":true,
            "hostname":self.hostname,
            "configured_hostname":self.configured_hostname,
            "port":self.port,
            "https_port":settings::https_port(self.port),
            "address_family":self.effective_address_family(),
            "bind_address":self.effective_bind_address(),
            "configured_address_family":self.configured_address_family,
            "configured_bind_address":self.configured_bind_address,
            "configured_output_name":self.configured_output_name,
            "effective_output_name":self.effective_output_name(),
            "capture_provider":self.capture_provider(),
            "active_http_port":self.active_http_port,
            "active_https_port":self.active_https_port,
            "http_addresses":self.active_http_addresses,
            "https_addresses":self.active_https_addresses,
            "listener_error":self.listener_error,
            "active_rtsp_port":self.active_rtsp_base_port,
            "rtsp_addresses":self.active_rtsp_addresses,
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

fn product_update_payload(
    expected_revision: u64,
    hostname: &str,
    target: &NetworkTarget,
    output_name: Option<&str>,
) -> Value {
    json!({
        "action":"product_update",
        "expected_revision":expected_revision,
        "version":SETTINGS_VERSION,
        "values":{
            "hostname":hostname,
            "port":target.port,
            "address_family":target.address_family,
            "bind_address":target.bind_address,
            "output_name":output_name,
        },
    })
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
            address_family: None,
            bind_address: None,
            output_name: None,
        });
    }

    let version = payload
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_state("product settings version is missing"))?;
    if !matches!(
        version,
        LEGACY_SETTINGS_VERSION
            | PORT_SETTINGS_VERSION
            | NETWORK_SETTINGS_VERSION
            | SETTINGS_VERSION
    ) || revision == 0
    {
        return Err(invalid_state(
            "product settings version or revision is unsupported",
        ));
    }
    let expected_value_count = match version {
        LEGACY_SETTINGS_VERSION => 1,
        PORT_SETTINGS_VERSION => 2,
        NETWORK_SETTINGS_VERSION => 4,
        SETTINGS_VERSION => 5,
        _ => unreachable!(),
    };
    if values.len() != expected_value_count
        || !values.contains_key("hostname")
        || (version >= PORT_SETTINGS_VERSION && !values.contains_key("port"))
        || (version >= NETWORK_SETTINGS_VERSION
            && (!values.contains_key("address_family") || !values.contains_key("bind_address")))
        || (version == SETTINGS_VERSION && !values.contains_key("output_name"))
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
    let (address_family, bind_address) = if version >= NETWORK_SETTINGS_VERSION {
        let address_family = serde_json::from_value::<Option<AddressFamily>>(
            values.get("address_family").cloned().unwrap_or(Value::Null),
        )
        .map_err(|_| invalid_state("persisted address_family is invalid"))?;
        let bind_address = serde_json::from_value::<Option<String>>(
            values.get("bind_address").cloned().unwrap_or(Value::Null),
        )
        .map_err(|_| invalid_state("persisted bind_address is invalid"))?;
        (address_family, bind_address)
    } else {
        (None, None)
    };
    let output_name = if version == SETTINGS_VERSION {
        match values.get("output_name") {
            Some(Value::Null) => None,
            Some(Value::String(value)) => normalize_output_name(value)
                .map_err(|_| invalid_state("persisted output name is invalid"))?,
            _ => return Err(invalid_state("persisted output name is invalid")),
        }
    } else {
        None
    };
    Ok(ProductDocument {
        revision,
        hostname: Some(hostname.to_owned()),
        port,
        address_family,
        bind_address,
        output_name,
    })
}

fn update_result(
    payload: &Value,
    expected_revision: u64,
    hostname: &str,
    target: &NetworkTarget,
    output_name: Option<&str>,
) -> Result<ProductDocument, SettingFailure> {
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(parse_failure(payload));
    }
    if payload.get("version").and_then(Value::as_u64) != Some(SETTINGS_VERSION) {
        return Err(invalid_state(
            "product_update did not confirm the v4 output settings",
        ));
    }
    let document = parse_document(payload)?;
    if document.hostname.as_deref() != Some(hostname)
        || document.port != Some(target.port)
        || document.address_family != target.address_family
        || document.bind_address != target.bind_address
        || document.output_name.as_deref() != output_name
        || expected_revision.checked_add(1) != Some(document.revision)
    {
        return Err(invalid_state(
            "product_update response does not match the requested write",
        ));
    }
    Ok(document)
}

fn normalize_output_name(value: &str) -> Result<Option<String>, ()> {
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_OUTPUT_NAME_BYTES
        || value.trim() != value
        || value.trim().is_empty()
        || value.chars().any(char::is_control)
    {
        return Err(());
    }
    Ok(Some(value.to_owned()))
}

fn apply_output_name_update(
    update: FieldUpdate<String>,
    current: Option<String>,
) -> Result<Option<String>, ()> {
    match update {
        FieldUpdate::Omitted => Ok(current),
        FieldUpdate::Null => Ok(None),
        FieldUpdate::Value(value) => normalize_output_name(&value),
    }
}

fn apply_field_update<T>(update: FieldUpdate<T>, current: Option<T>) -> Option<T> {
    match update {
        FieldUpdate::Omitted => current,
        FieldUpdate::Null => None,
        FieldUpdate::Value(value) => Some(value),
    }
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

    fn bootstrap_with_capture_output(output: &str) -> Settings {
        Settings {
            streaming: Some(crate::launch::StreamingConfig {
                capture: crate::launch::CaptureConfig {
                    capture_provider: crate::launch::CaptureProvider::Dxgi,
                    output: output.into(),
                    encoder: 0,
                    audio_endpoint: "default".into(),
                },
                profile: crate::launch::StreamProfile {
                    max_width: 1920,
                    max_height: 1080,
                    max_fps: 60,
                    bitrate_kbps: 10_000,
                    max_reference_frames: 1,
                    packet_size: 1024,
                    fec_percentage: 20,
                    min_parity_shards: 2,
                    max_fec_blocks: 1,
                    packet_ms: 10,
                    audio_bitrate: 128_000,
                    encrypt_video: true,
                    encrypt_audio: true,
                    permit_legacy_audio_iv_wrap: false,
                },
            }),
            ..Settings::default()
        }
    }

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
        assert_eq!(persisted.output_name, None);

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
        let network_document = parse_document(&json!({
            "ok":true,"found":true,"version":3,"revision":10,
            "values":{"hostname":"Studio PC","port":48000,"address_family":null,"bind_address":null}
        }))
        .unwrap();
        assert_eq!(network_document.output_name, None);
    }

    #[test]
    fn output_name_update_uses_null_empty_and_omitted_as_intended() {
        assert_eq!(
            apply_output_name_update(FieldUpdate::Omitted, Some("existing-id".into())),
            Ok(Some("existing-id".into()))
        );
        assert_eq!(
            apply_output_name_update(FieldUpdate::Null, Some("existing-id".into())),
            Ok(None)
        );
        assert_eq!(
            apply_output_name_update(
                FieldUpdate::Value(String::new()),
                Some("existing-id".into())
            ),
            Ok(None)
        );
        assert_eq!(
            apply_output_name_update(FieldUpdate::Value("selected-display-id".into()), None),
            Ok(Some("selected-display-id".into()))
        );

        for invalid in [" ", " display-id", "display-id ", "display\nid"] {
            assert_eq!(normalize_output_name(invalid), Err(()));
        }
        assert_eq!(
            normalize_output_name(&"é".repeat(256)),
            Ok(Some("é".repeat(256)))
        );
        assert_eq!(normalize_output_name(&"é".repeat(257)), Err(()));
    }

    #[test]
    fn v4_requires_nullable_output_name_and_normalizes_empty_to_automatic() {
        let automatic = parse_document(&json!({
            "ok":true,"found":true,"version":4,"revision":1,
            "values":{"hostname":"Host","port":47989,"address_family":null,"bind_address":null,"output_name":""}
        }))
        .unwrap();
        assert_eq!(automatic.output_name, None);

        for payload in [
            json!({"ok":true,"found":true,"version":4,"revision":1,"values":{"hostname":"Host","port":47989,"address_family":null,"bind_address":null}}),
            json!({"ok":true,"found":true,"version":4,"revision":1,"values":{"hostname":"Host","port":47989,"address_family":null,"bind_address":null,"output_name":" "}}),
            json!({"ok":true,"found":true,"version":4,"revision":1,"values":{"hostname":"Host","port":47989,"address_family":null,"bind_address":null,"output_name":" display-id"}}),
            json!({"ok":true,"found":true,"version":4,"revision":1,"values":{"hostname":"Host","port":47989,"address_family":null,"bind_address":null,"output_name":"display\nid"}}),
        ] {
            assert_eq!(
                parse_document(&payload).unwrap_err().code,
                "invalid_state_response"
            );
        }
    }

    #[test]
    fn v4_output_selection_survives_restart_and_null_uses_bootstrap_output() {
        let bootstrap = bootstrap_with_capture_output("bootstrap-display-id");
        let selected_document = json!({
            "ok":true,"found":true,"version":4,"revision":11,
            "values":{"hostname":"Host","port":47989,"address_family":null,"bind_address":null,"output_name":"selected-display-id"}
        });
        let mut first_start =
            ProductSettings::with_bootstrap_settings("Bootstrap".into(), &bootstrap);
        first_start.apply_document(parse_document(&selected_document).unwrap());
        assert_eq!(
            first_start.configured_output_name(),
            Some("selected-display-id")
        );
        assert_eq!(
            first_start.effective_output_name(),
            Some("selected-display-id")
        );
        assert_eq!(first_start.settings_payload()["capture_provider"], "dxgi");
        assert_eq!(
            first_start.settings_payload()["configured_output_name"],
            "selected-display-id"
        );
        assert_eq!(
            first_start.settings_payload()["effective_output_name"],
            "selected-display-id"
        );

        let mut restarted =
            ProductSettings::with_bootstrap_settings("Bootstrap".into(), &bootstrap);
        restarted.apply_document(parse_document(&selected_document).unwrap());
        assert_eq!(
            restarted.effective_output_name(),
            Some("selected-display-id")
        );

        let automatic_document = json!({
            "ok":true,"found":true,"version":4,"revision":12,
            "values":{"hostname":"Host","port":47989,"address_family":null,"bind_address":null,"output_name":null}
        });
        restarted.apply_document(parse_document(&automatic_document).unwrap());
        assert_eq!(restarted.configured_output_name(), None);
        assert_eq!(
            restarted.effective_output_name(),
            Some("bootstrap-display-id")
        );
        assert_eq!(
            restarted.settings_payload()["effective_output_name"],
            "bootstrap-display-id"
        );
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
            "ok":true,"found":true,"version":4,"revision":4,
            "values":{"hostname":"New Host","port":48000,"address_family":null,"bind_address":null,"output_name":null}
        });
        let target = NetworkTarget {
            port: 48_000,
            address_family: None,
            bind_address: None,
        };
        assert_eq!(
            update_result(&success, 3, "New Host", &target, None)
                .unwrap()
                .revision,
            4
        );
        assert_eq!(
            update_result(&success, 2, "New Host", &target, None)
                .unwrap_err()
                .code,
            "invalid_state_response"
        );
        assert_eq!(
            update_result(&success, 3, "Other Host", &target, None)
                .unwrap_err()
                .code,
            "invalid_state_response"
        );
        assert_eq!(
            update_result(
                &success,
                3,
                "New Host",
                &NetworkTarget {
                    port: 48_001,
                    ..target.clone()
                },
                None,
            )
            .unwrap_err()
            .code,
            "invalid_state_response"
        );

        let reset_to_machine_name = json!({
            "ok":true,"found":true,"version":4,"revision":5,
            "values":{"hostname":"","port":48000,"address_family":null,"bind_address":null,"output_name":null}
        });
        assert_eq!(
            update_result(&reset_to_machine_name, 4, "", &target, None)
                .unwrap()
                .hostname
                .as_deref(),
            Some("")
        );

        let selected = json!({
            "ok":true,"found":true,"version":4,"revision":9,
            "values":{"hostname":"New Host","port":48000,"address_family":null,"bind_address":null,"output_name":"selected-display-id"}
        });
        assert_eq!(
            update_result(
                &selected,
                8,
                "New Host",
                &target,
                Some("selected-display-id")
            )
            .unwrap()
            .output_name
            .as_deref(),
            Some("selected-display-id")
        );
        assert_eq!(
            update_result(&selected, 8, "New Host", &target, None)
                .unwrap_err()
                .code,
            "invalid_state_response"
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
                "address_family":"ipv4",
                "bind_address":"127.0.0.1",
                "configured_address_family":null,
                "configured_bind_address":null,
                "configured_output_name":null,
                "effective_output_name":null,
                "capture_provider":null,
                "active_http_port":null,
                "active_https_port":null,
                "http_addresses":null,
                "https_addresses":null,
                "active_rtsp_port":null,
                "rtsp_addresses":null,
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
                target: NetworkTarget {
                    port: 48_000,
                    address_family: None,
                    bind_address: None,
                },
                output_name: None,
                expected_revision: 1,
                require_listener_apply: false,
            },
            CallOutcome::Payload(json!({
                "ok":true,
                "found":true,
                "version":4,
                "revision":2,
                "values":{"hostname":"New Host","port":48000,"address_family":null,"bind_address":null,"output_name":null}
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
    fn hostname_only_update_succeeds_without_waiting_for_rtsp_port_activation() {
        let mut settings = ProductSettings::new("Computer Name".into(), settings::DEFAULT_PORT);
        settings.apply_document(
            parse_document(&json!({
                "ok":true,"found":true,"version":2,"revision":1,
                "values":{"hostname":"Old Host","port":47989}
            }))
            .unwrap(),
        );
        let http_target = settings.http_target();
        let rtsp_target = settings.rtsp_target();
        settings.set_active_http_state(
            Some(http_target),
            Some(vec!["127.0.0.1:47989".into()]),
            Some(vec!["127.0.0.1:47984".into()]),
        );
        settings.set_active_rtsp_state(
            Some(rtsp_target),
            Some(47_989),
            Some(vec!["127.0.0.1:48010".into()]),
        );
        settings.finish_call(
            Purpose::Update {
                request: 12,
                hostname: "New Host".into(),
                target: NetworkTarget {
                    port: 47_989,
                    address_family: None,
                    bind_address: None,
                },
                output_name: None,
                expected_revision: 1,
                require_listener_apply: true,
            },
            CallOutcome::Payload(json!({
                "ok":true,"found":true,"version":4,"revision":2,
                "values":{"hostname":"New Host","port":47989,"address_family":null,"bind_address":null,"output_name":null}
            })),
        );

        let completion = settings.completions.front().unwrap();
        assert_eq!(completion.payload["ok"], true);
        assert_eq!(completion.payload["configured_hostname"], "New Host");
        assert_eq!(completion.required_http_target, None);
        assert_eq!(completion.required_rtsp_target, None);
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
        assert_eq!(completion.required_http_target, None);
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

    #[test]
    fn v3_preserves_null_and_explicit_empty_bind_address_distinctly() {
        let cleared = parse_document(&json!({
            "ok":true,"found":true,"version":3,"revision":8,
            "values":{"hostname":"Host","port":47989,"address_family":null,"bind_address":null}
        }))
        .unwrap();
        let empty = parse_document(&json!({
            "ok":true,"found":true,"version":3,"revision":9,
            "values":{"hostname":"Host","port":47989,"address_family":"both","bind_address":""}
        }))
        .unwrap();
        assert_eq!(cleared.address_family, None);
        assert_eq!(cleared.bind_address, None);
        assert_eq!(empty.address_family, Some(AddressFamily::Both));
        assert_eq!(empty.bind_address.as_deref(), Some(""));
    }

    #[test]
    fn resetting_the_last_network_override_reapplies_startup_listener_policy() {
        let bootstrap = Settings {
            address_family: Some(AddressFamily::Ipv4),
            bind_address: Some("127.0.0.1".into()),
            ..Settings::default()
        };
        let mut product = ProductSettings::with_bootstrap_settings("Bootstrap".into(), &bootstrap);
        let overridden = parse_document(&json!({
            "ok":true,"found":true,"version":3,"revision":4,
            "values":{"hostname":"Host","port":47989,"address_family":"both","bind_address":"::1"}
        }))
        .unwrap();
        product.apply_document(overridden);
        product.set_active_http_state(
            Some(product.http_target()),
            Some(vec!["[::1]:47989".into()]),
            Some(vec!["[::1]:47984".into()]),
        );
        assert!(!product.listeners_need_application());

        product.apply_document(
            parse_document(&json!({
                "ok":true,"found":true,"version":3,"revision":5,
                "values":{"hostname":"Host","port":47989,"address_family":null,"bind_address":null}
            }))
            .unwrap(),
        );
        assert!(product.listeners_need_application());
        assert_eq!(
            product.http_target().address_family,
            Some(AddressFamily::Ipv4)
        );
        assert_eq!(
            product.http_target().bind_address.as_deref(),
            Some("127.0.0.1")
        );
    }

    #[test]
    fn persisted_null_reset_waits_for_startup_bind_to_reapply() {
        let bootstrap = Settings {
            address_family: Some(AddressFamily::Ipv4),
            bind_address: Some(String::new()),
            ..Settings::default()
        };
        let mut product = ProductSettings::with_bootstrap_settings("Bootstrap".into(), &bootstrap);
        product.apply_document(
            parse_document(&json!({
                "ok":true,"found":true,"version":3,"revision":3,
                "values":{"hostname":"Host","port":47989,"address_family":"both","bind_address":"::1"}
            }))
            .unwrap(),
        );
        let old_http = product.http_target();
        let old_rtsp = product.rtsp_target();
        product.set_active_http_state(
            Some(old_http),
            Some(vec!["[::1]:47989".into()]),
            Some(vec!["[::1]:47984".into()]),
        );
        product.set_active_rtsp_state(
            Some(old_rtsp),
            Some(47_989),
            Some(vec!["[::1]:48010".into()]),
        );

        product.finish_call(
            Purpose::Update {
                request: 21,
                hostname: "Host".into(),
                target: NetworkTarget {
                    port: 47_989,
                    address_family: None,
                    bind_address: None,
                },
                output_name: None,
                expected_revision: 3,
                require_listener_apply: true,
            },
            CallOutcome::Payload(json!({
                "ok":true,"found":true,"version":4,"revision":4,
                "values":{"hostname":"Host","port":47989,"address_family":null,"bind_address":null,"output_name":null}
            })),
        );

        let completion = product.completions.front().unwrap();
        assert_eq!(completion.required_http_target, Some(product.http_target()));
        assert_eq!(completion.required_rtsp_target, Some(product.rtsp_target()));
        assert!(product.listeners_need_application());
    }

    #[test]
    fn persisted_remote_override_is_rejected_when_bootstrap_disallows_remote_access() {
        let product = ProductSettings::new("Bootstrap".into(), settings::DEFAULT_PORT);
        let document = parse_document(&json!({
            "ok":true,"found":true,"version":3,"revision":2,
            "values":{"hostname":"Host","port":47989,"address_family":"ipv4","bind_address":"0.0.0.0"}
        }))
        .unwrap();
        assert_eq!(
            product.validate_document(&document).unwrap_err().code,
            "invalid_state_response"
        );
    }

    #[test]
    fn omitted_update_fields_preserve_while_null_clears_overrides() {
        assert_eq!(
            apply_field_update(FieldUpdate::Omitted, Some(AddressFamily::Both)),
            Some(AddressFamily::Both)
        );
        assert_eq!(
            apply_field_update::<AddressFamily>(FieldUpdate::Null, Some(AddressFamily::Both)),
            None
        );
        assert_eq!(
            apply_field_update(FieldUpdate::Value(String::new()), Some("127.0.0.1".into())),
            Some(String::new())
        );
    }

    #[test]
    fn v4_product_update_payload_keeps_nulls_and_blank_overrides_exact() {
        let reset = product_update_payload(
            7,
            "Host",
            &NetworkTarget {
                port: 48_000,
                address_family: None,
                bind_address: None,
            },
            None,
        );
        assert_eq!(reset["version"], 4);
        assert_eq!(reset["expected_revision"], 7);
        assert_eq!(reset["values"]["address_family"], Value::Null);
        assert_eq!(reset["values"]["bind_address"], Value::Null);

        let explicit_blank = product_update_payload(
            8,
            "Host",
            &NetworkTarget {
                port: 48_000,
                address_family: Some(AddressFamily::Both),
                bind_address: Some(String::new()),
            },
            Some("stable-display-id"),
        );
        assert_eq!(explicit_blank["values"]["address_family"], "both");
        assert_eq!(explicit_blank["values"]["bind_address"], "");
        assert_eq!(explicit_blank["values"]["output_name"], "stable-display-id");
    }
}
