//! Tick-owned, nonblocking network connections and asynchronous service delegation.
use crate::assets::Assets;
use crate::capture_management::{self, SelectionAction, SelectionOwners};
use crate::csrf_policy::CsrfOrigins;
use crate::http::{BrowserAuth, RequestTarget};
use crate::peer_policy::{self, PeerTier};
use crate::{AUTH_SERVICE, http};
use amaterasu_module_sdk::network::{Network, TcpListener, TcpStream};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use rustls::{ServerConfig, ServerConnection};
use serde_json::{Value, json};
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_CONNECTIONS: usize = 16;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct AudioProviders {
    pub capture: String,
    pub encoder: String,
    pub processing: Option<ProcessingRoutes>,
}

pub(crate) struct ProcessingRoutes {
    pub video: String,
    pub stream: String,
    pub lifecycle: Option<String>,
    pub registry: Option<String>,
}

impl Default for AudioProviders {
    fn default() -> Self {
        Self {
            capture: "amaterasu.audio/1".into(),
            encoder: "amaterasu.audio-encode/1".into(),
            processing: None,
        }
    }
}

impl AudioProviders {
    fn service<'a>(&'a self, service: &'a str) -> &'a str {
        match service {
            "amaterasu.audio/1" => &self.capture,
            "amaterasu.audio-encode/1" => &self.encoder,
            "amaterasu.video/1" if self.processing.is_some() => {
                &self.processing.as_ref().unwrap().video
            }
            "amaterasu.stream-session/1" if self.processing.is_some() => {
                &self.processing.as_ref().unwrap().stream
            }
            "amaterasu.session-lifecycle/1" if self.processing.is_some() => self
                .processing
                .as_ref()
                .unwrap()
                .lifecycle
                .as_deref()
                .unwrap_or(""),
            "amaterasu.extension-registry/1" if self.processing.is_some() => self
                .processing
                .as_ref()
                .unwrap()
                .registry
                .as_deref()
                .unwrap_or(""),
            _ => service,
        }
    }
}

pub struct Server {
    audio_providers: AudioProviders,
    listener: TcpListener,
    tls_listener: Option<TcpListener>,
    tls_configuration: Option<Arc<ServerConfig>>,
    address: String,
    origins: Vec<String>,
    connections: Vec<Connection>,
    next_operation: OperationSequence,
    assets: Option<Assets>,
    maximum_peer: Option<PeerTier>,
    csrf_origins: CsrfOrigins,
    selection_owners: SelectionOwners,
    authentication_admission: AuthenticationAdmission,
    extensions: Option<crate::extensions::Extensions>,
    accepting: bool,
}

impl Server {
    #[cfg(test)]
    pub fn bind(
        address: SocketAddr,
        origins: Vec<String>,
        assets: Option<Assets>,
        tls_bind: Option<SocketAddr>,
        tls_configuration: Option<Arc<ServerConfig>>,
    ) -> ModuleResult<Self> {
        Self::bind_with_network(
            Network::test_provider(),
            address,
            origins,
            assets,
            tls_bind,
            tls_configuration,
        )
    }

    pub fn bind_with_network(
        network: Network,
        address: SocketAddr,
        origins: Vec<String>,
        assets: Option<Assets>,
        tls_bind: Option<SocketAddr>,
        tls_configuration: Option<Arc<ServerConfig>>,
    ) -> ModuleResult<Self> {
        let listener = network
            .bind_tcp(address)
            .map_err(|_| "management bind failed")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "management nonblocking listener unavailable")?;
        let address = listener
            .local_addr()
            .map_err(|_| "management local address unavailable")?
            .to_string();
        let tls_listener = if let Some(bind) = tls_bind {
            let listener = network
                .bind_tcp(bind)
                .map_err(|_| "management TLS bind failed")?;
            listener
                .set_nonblocking(true)
                .map_err(|_| "management TLS listener unavailable")?;
            Some(listener)
        } else {
            None
        };
        Ok(Self {
            audio_providers: AudioProviders::default(),
            listener,
            tls_listener,
            tls_configuration,
            address,
            origins,
            connections: Vec::new(),
            next_operation: OperationSequence::new()?,
            assets,
            maximum_peer: None,
            csrf_origins: CsrfOrigins::default(),
            selection_owners: SelectionOwners::default(),
            authentication_admission: AuthenticationAdmission::default(),
            extensions: None,
            accepting: true,
        })
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn tls_address(&self) -> Option<SocketAddr> {
        self.tls_listener.as_ref()?.local_addr().ok()
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<()> {
        if self.accepting {
            self.accept()?;
        }
        let mut tick = ConnectionTick {
            runtime,
            audio_providers: &self.audio_providers,
            authority: &self.address,
            origins: &self.origins,
            csrf_origins: &self.csrf_origins,
            operation: &mut self.next_operation,
            assets: &mut self.assets,
            selection_owners: &mut self.selection_owners,
            authentication_admission: &mut self.authentication_admission,
            extensions: self.extensions.as_ref(),
        };
        for connection in &mut self.connections {
            if !peer_policy::permits(self.maximum_peer, connection.peer, connection.tls.is_some())
                || connection
                    .csrf_exception
                    .as_deref()
                    .is_some_and(|origin| !self.csrf_origins.permits(Some(origin)))
            {
                connection.expire(runtime, true);
                continue;
            }
            // Existing requests may finish while dependents drain, but a
            // partially read external request must not start new work.
            if !self.accepting && matches!(connection.stage, Stage::Reading(_)) {
                continue;
            }
            connection.tick(&mut tick)?;
        }
        self.connections
            .retain(|connection| !matches!(connection.stage, Stage::Closed));
        Ok(())
    }

    /// Seal external admission as soon as host shutdown is announced.
    pub(crate) fn prepare_shutdown(&mut self) {
        self.accepting = false;
    }

    /// Retire connection-owned requests while their providers are still live.
    /// Failure to contact the broker leaves the exact request queued for retry.
    pub(crate) fn quiesce(&mut self, runtime: &RuntimeClient) -> bool {
        self.prepare_shutdown();
        for connection in &mut self.connections {
            connection.expire(runtime, false);
        }
        self.connections
            .retain(|connection| !matches!(connection.stage, Stage::Closed));
        self.connections.is_empty()
    }

    pub(crate) fn set_audio_providers(&mut self, providers: AudioProviders) {
        self.audio_providers = providers;
    }

    pub(crate) fn set_peer_policy(&mut self, maximum: Option<PeerTier>) {
        self.maximum_peer = maximum;
    }

    pub(crate) fn set_csrf_policy(&mut self, origins: CsrfOrigins) {
        self.csrf_origins = origins;
    }

    pub(crate) fn set_extensions(&mut self, extensions: Option<crate::extensions::Extensions>) {
        self.extensions = extensions;
    }

    fn accept(&mut self) -> ModuleResult<()> {
        Self::accept_from(
            &self.listener,
            None,
            self.maximum_peer,
            &mut self.connections,
        )?;
        if let Some(listener) = &self.tls_listener {
            Self::accept_from(
                listener,
                self.tls_configuration.as_ref(),
                self.maximum_peer,
                &mut self.connections,
            )?;
        }
        Ok(())
    }

    fn accept_from(
        listener: &TcpListener,
        configuration: Option<&Arc<ServerConfig>>,
        maximum_peer: Option<PeerTier>,
        connections: &mut Vec<Connection>,
    ) -> ModuleResult<()> {
        for _ in 0..4 {
            if connections.len() >= MAX_CONNECTIONS {
                break;
            }
            match listener.accept() {
                Ok((stream, peer))
                    if peer_policy::permits(maximum_peer, peer.ip(), configuration.is_some()) =>
                {
                    if stream.set_nonblocking(true).is_ok() {
                        let tls = configuration
                            .map(|config| ServerConnection::new(config.clone()))
                            .transpose()
                            .map_err(|_| "management TLS connection failed")?;
                        connections.push(Connection {
                            stream,
                            tls,
                            tls_closing: false,
                            deadline: Instant::now() + CONNECTION_TIMEOUT,
                            stage: Stage::Reading(Vec::new()),
                            terminal_result: None,
                            released_request: None,
                            origin: None,
                            csrf_exception: None,
                            peer: peer.ip(),
                        });
                    }
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Err("management accept failed".into()),
            }
        }
        Ok(())
    }
}

fn retirement_succeeded(response: ModuleResult<Response>) -> bool {
    matches!(response, Ok(Response::Ok))
        || matches!(response, Ok(Response::Error { code, .. }) if code == "not_found")
}

fn is_terminal_caller_result(response: &Response) -> bool {
    match response {
        Response::Result { .. } => true,
        Response::Error { code, .. } => matches!(code.as_str(), "deadline_exceeded" | "cancelled"),
        _ => false,
    }
}

struct OperationSequence {
    instance: String,
    next: u64,
}

impl OperationSequence {
    fn new() -> ModuleResult<Self> {
        Ok(Self {
            instance: amaterasu_package::random_identifier()?,
            next: 1,
        })
    }

    fn next_id(&mut self) -> Option<String> {
        let next = self.next.checked_add(1)?;
        let id = format!("management.{}.{}", self.instance, self.next);
        self.next = next;
        Some(id)
    }
}

struct Connection {
    stream: TcpStream,
    tls: Option<ServerConnection>,
    tls_closing: bool,
    deadline: Instant,
    origin: Option<String>,
    csrf_exception: Option<String>,
    stage: Stage,
    terminal_result: Option<(u64, Response)>,
    released_request: Option<u64>,
    peer: IpAddr,
}

struct VerifiedDispatch<'a> {
    extensions: Option<&'a crate::extensions::Extensions>,
    audio_providers: &'a AudioProviders,
}

struct ConnectionTick<'a> {
    runtime: &'a RuntimeClient,
    audio_providers: &'a AudioProviders,
    authority: &'a str,
    origins: &'a [String],
    csrf_origins: &'a CsrfOrigins,
    operation: &'a mut OperationSequence,
    assets: &'a mut Option<Assets>,
    selection_owners: &'a mut SelectionOwners,
    authentication_admission: &'a mut AuthenticationAdmission,
    extensions: Option<&'a crate::extensions::Extensions>,
}

#[derive(Default)]
struct AuthenticationAdmission {
    attempts: std::collections::VecDeque<(Instant, IpAddr, String)>,
}

impl AuthenticationAdmission {
    fn permits(&mut self, peer: IpAddr, request: &http::Request, now: Instant) -> bool {
        let sensitive = match request.target {
            RequestTarget::BrowserAuth(BrowserAuth::Login) => true,
            RequestTarget::Api(route) => {
                route.service == AUTH_SERVICE
                    && matches!(route.action, "login" | "enroll_first_admin")
            }
            _ => false,
        };
        if !sensitive || request.preflight {
            return true;
        }
        while self
            .attempts
            .front()
            .is_some_and(|(time, _, _)| now.duration_since(*time) >= Duration::from_secs(60))
        {
            self.attempts.pop_front();
        }
        let username = request
            .payload
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let username = username.to_ascii_lowercase();
        let peer = peer.to_canonical();
        if self
            .attempts
            .iter()
            .filter(|(_, address, _)| *address == peer)
            .count()
            >= 10
            || self
                .attempts
                .iter()
                .filter(|(_, address, account)| *address == peer && *account == username)
                .count()
                >= 5
        {
            return false;
        }
        // A full ledger must not reject every unrelated peer. Eviction keeps
        // memory bounded, but under distributed pressure it can shorten the
        // remembered window for the oldest peers. Authentication also applies
        // a separate global admission budget as a CPU backstop.
        if self.attempts.len() >= 1024 {
            self.attempts.pop_front();
        }
        self.attempts.push_back((now, peer, username));
        true
    }
}

enum Stage {
    Artwork {
        request_id: u64,
        service: String,
    },
    Reading(Vec<u8>),
    Verifying {
        request_id: u64,
        request: Box<http::Request>,
    },
    Dispatching {
        request_id: u64,
        login: bool,
        operational_health: bool,
        browser_auth: Option<BrowserAuth>,
        capture: Option<CaptureDispatch>,
    },
    SessionExtensionDispatching {
        request_id: u64,
        action: &'static str,
    },
    ExtensionDispatching {
        request_id: u64,
    },
    Writing {
        bytes: Vec<u8>,
        offset: usize,
    },
    Retiring {
        request_id: u64,
        settled: bool,
        revoked: bool,
        cancelled: bool,
    },
    AdmissionUncertain {
        command: Command,
        first_sent: Instant,
        revoked: bool,
    },
    Closed,
}

#[derive(Clone)]
struct CaptureDispatch {
    action: SelectionAction,
    operation_id: Option<String>,
    created: bool,
}

struct ServiceReply<'a> {
    login: bool,
    operational_health: bool,
    browser_auth: Option<BrowserAuth>,
    capture: Option<CaptureDispatch>,
    selection_owners: &'a mut SelectionOwners,
}

impl Connection {
    fn tick(&mut self, context: &mut ConnectionTick<'_>) -> ModuleResult<()> {
        let runtime = context.runtime;
        if matches!(self.stage, Stage::AdmissionUncertain { .. }) {
            self.expire(runtime, Instant::now() >= self.deadline);
            return Ok(());
        }
        if matches!(self.stage, Stage::Retiring { .. }) || Instant::now() >= self.deadline {
            self.expire(runtime, Instant::now() >= self.deadline);
            return Ok(());
        }
        if self.tls.is_some() && !self.tls_io() {
            self.expire(runtime, true);
            return Ok(());
        }
        let request_id = self.active_request();
        self.released_request = None;
        let result = (|| -> ModuleResult<()> {
            match std::mem::replace(&mut self.stage, Stage::Closed) {
                Stage::Reading(bytes) => self.read_request(bytes, context)?,
                Stage::Verifying {
                    request_id,
                    request,
                } => self.verify_result(
                    runtime,
                    request_id,
                    *request,
                    context.operation,
                    context.selection_owners,
                    VerifiedDispatch {
                        extensions: context.extensions,
                        audio_providers: context.audio_providers,
                    },
                )?,
                Stage::Dispatching {
                    request_id,
                    login,
                    operational_health,
                    browser_auth,
                    capture,
                } => self.service_result(
                    runtime,
                    request_id,
                    ServiceReply {
                        login,
                        operational_health,
                        browser_auth,
                        capture,
                        selection_owners: context.selection_owners,
                    },
                )?,
                Stage::SessionExtensionDispatching { request_id, action } => {
                    match self.take_result(runtime, request_id)? {
                        Response::Pending => {
                            self.stage = Stage::SessionExtensionDispatching { request_id, action }
                        }
                        Response::Result { payload } => {
                            let (status, body) = session_extension_response(action, payload);
                            self.respond(status, body);
                        }
                        _ => self.fence_request(request_id),
                    }
                }
                Stage::ExtensionDispatching { request_id } => {
                    match self.take_result(runtime, request_id)? {
                        Response::Pending => {
                            self.stage = Stage::ExtensionDispatching { request_id }
                        }
                        Response::Result { payload } => {
                            self.respond(200, json!({"ok":true,"result":payload}))
                        }
                        _ => self.fence_request(request_id),
                    }
                }
                Stage::Artwork {
                    request_id,
                    service,
                } => {
                    match self.take_result(runtime, request_id)? {
                        Response::Pending => {
                            self.stage = Stage::Artwork {
                                request_id,
                                service,
                            }
                        }
                        Response::Result { payload } => {
                            let image = (|| -> ModuleResult<_> {
                                if payload["ok"] != true {
                                    return Err("artwork unavailable".into());
                                }
                                let offer = serde_json::from_value(payload["offer"].clone())
                                    .map_err(|e| e.to_string())?;
                                // SAFETY: only the authorized startup route selects this provider.
                                unsafe {
                                    amaterasu_module_sdk::artwork_transfer::receive(
                                        runtime, &service, offer,
                                    )
                                }
                            })();
                            match image {
                                Ok(image) => {
                                    self.stage = Stage::Writing {
                                        bytes: http::image_response(
                                            &image.mime_type,
                                            image.bytes,
                                            self.origin.as_deref(),
                                        ),
                                        offset: 0,
                                    }
                                }
                                Err(_) => self.fail(404, "artwork_unavailable"),
                            }
                        }
                        _ => self.fence_request(request_id),
                    }
                }
                Stage::Writing { bytes, offset } => self.write_response(bytes, offset),
                Stage::Retiring { .. } => unreachable!("retirement is handled before socket I/O"),
                Stage::AdmissionUncertain { .. } => {
                    unreachable!("admission recovery is handled before socket I/O")
                }
                Stage::Closed => {}
            }
            Ok(())
        })();
        if let Err(error) = result {
            // A failed result or release call does not relinquish this broker
            // request. Keep its ID available to the shutdown retirement path.
            if matches!(self.stage, Stage::Closed)
                && let Some(request_id) = request_id
                && self.released_request != Some(request_id)
            {
                self.stage = Stage::Retiring {
                    request_id,
                    settled: false,
                    revoked: false,
                    cancelled: false,
                };
            }
            return Err(error);
        }
        if self.tls.is_some() && !self.tls_io() {
            self.expire(runtime, true);
        }
        Ok(())
    }

    fn active_request(&self) -> Option<u64> {
        match self.stage {
            Stage::Artwork { request_id, .. }
            | Stage::Verifying { request_id, .. }
            | Stage::Dispatching { request_id, .. }
            | Stage::ExtensionDispatching { request_id }
            | Stage::SessionExtensionDispatching { request_id, .. }
            | Stage::Retiring { request_id, .. } => Some(request_id),
            _ => None,
        }
    }

    fn fence_request(&mut self, request_id: u64) {
        self.stage = Stage::Retiring {
            request_id,
            settled: false,
            revoked: false,
            cancelled: false,
        };
    }

    /// Settle the caller-owned broker result before discarding its response.
    /// Provider delivery retirement has a separate receipt.
    fn take_result(&mut self, runtime: &RuntimeClient, request_id: u64) -> ModuleResult<Response> {
        self.settle_result(request_id, |command| runtime.send(command))
    }

    fn settle_result(
        &mut self,
        request_id: u64,
        mut send: impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<Response> {
        if self.terminal_result.is_none() {
            match send(Command::TakeResult {
                request: request_id,
            }) {
                Ok(result) if is_terminal_caller_result(&result) => {
                    self.terminal_result = Some((request_id, result));
                }
                Ok(response) => return Ok(response),
                Err(_) => return Ok(Response::Pending),
            }
        }
        if !self
            .terminal_result
            .as_ref()
            .is_some_and(|(id, _)| *id == request_id)
        {
            return Err("management retained result does not match request".into());
        }
        if !retirement_succeeded(send(Command::ReleaseResult {
            request: request_id,
        })) {
            return Ok(Response::Pending);
        }
        let (_, result) = self
            .terminal_result
            .take()
            .expect("checked retained result");
        self.released_request = Some(request_id);
        Ok(result)
    }

    fn tls_io(&mut self) -> bool {
        let tls = self.tls.as_mut().expect("TLS connection");
        if tls.wants_read() && matches!(self.stage, Stage::Reading(_)) {
            match tls.read_tls(&mut (&mut self.stream).take(16_384)) {
                Ok(0) => return false,
                Ok(_) => {
                    if tls.process_new_packets().is_err() {
                        return false;
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        if tls.wants_write() {
            match tls.write_tls(&mut self.stream) {
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        true
    }

    fn expire(&mut self, runtime: &RuntimeClient, revoke: bool) {
        let (request_id, mut settled, revoked, mut cancelled) = match self.stage {
            Stage::Artwork { request_id, .. }
            | Stage::Verifying { request_id, .. }
            | Stage::Dispatching { request_id, .. }
            | Stage::ExtensionDispatching { request_id }
            | Stage::SessionExtensionDispatching { request_id, .. } => {
                (request_id, false, revoke, false)
            }
            Stage::Retiring {
                request_id,
                settled,
                revoked,
                cancelled,
            } => (request_id, settled, revoked || revoke, cancelled),
            // With no parent request, replay could create a new side effect.
            // Keep the exact unknown admission fenced during shutdown.
            Stage::AdmissionUncertain {
                ref mut revoked, ..
            } => {
                *revoked |= revoke;
                self.lookup_admission(runtime);
                return;
            }
            _ => {
                self.stage = Stage::Closed;
                return;
            }
        };
        let result_observed = self
            .terminal_result
            .as_ref()
            .is_some_and(|(id, _)| *id == request_id);
        if !settled {
            settled = self.released_request == Some(request_id)
                || self
                    .take_result(runtime, request_id)
                    .is_ok_and(|response| is_terminal_caller_result(&response));
        }
        if revoked && !settled && !result_observed && !cancelled {
            // Revoke client authority promptly, but cancellation is not proof
            // that a delivered provider effect has retired.
            cancelled = retirement_succeeded(runtime.send(Command::Cancel {
                request: request_id,
            }));
        }
        if settled {
            self.terminal_result = None;
            self.stage = Stage::Closed;
        } else {
            self.stage = Stage::Retiring {
                request_id,
                settled,
                revoked,
                cancelled,
            };
        }
    }

    fn lookup_admission(&mut self, runtime: &RuntimeClient) {
        let Stage::AdmissionUncertain {
            command,
            first_sent,
            revoked,
        } = &self.stage
        else {
            return;
        };
        let first_sent = *first_sent;
        let revoked = *revoked;
        if first_sent.elapsed() >= Duration::from_secs(300) {
            return;
        }
        let query = match command {
            Command::Request {
                service,
                payload,
                operation_id,
                parent,
                ..
            } => Command::LookupRequest {
                service: service.clone(),
                payload: payload.clone(),
                operation_id: operation_id.clone(),
                parent: *parent,
            },
            Command::CallApi {
                service,
                method,
                interface_major,
                interface_minor,
                payload,
                operation_id,
                parent,
                ..
            } => Command::LookupApiRequest {
                service: service.clone(),
                method: method.clone(),
                interface_major: *interface_major,
                interface_minor: *interface_minor,
                payload: payload.clone(),
                operation_id: operation_id.clone(),
                parent: *parent,
            },
            _ => return,
        };
        match runtime.send(query) {
            Ok(Response::Accepted { id }) => {
                self.stage = Stage::Retiring {
                    request_id: id,
                    settled: false,
                    revoked,
                    cancelled: false,
                };
                self.expire(runtime, false);
            }
            Ok(Response::Error { code, .. })
                if code == "not_found" && first_sent.elapsed() < Duration::from_secs(300) =>
            {
                // No other code retries or releases an unknown admission, and
                // the broker has not yet expired its original retry record.
                self.stage = Stage::Closed;
            }
            _ => {}
        }
    }

    fn read_request(
        &mut self,
        mut bytes: Vec<u8>,
        context: &mut ConnectionTick<'_>,
    ) -> ModuleResult<()> {
        let mut buffer = [0; 4096];
        if self.tls.as_ref().is_some_and(|tls| tls.is_handshaking()) {
            self.stage = Stage::Reading(bytes);
            return Ok(());
        }
        let read = match &mut self.tls {
            Some(tls) => tls.reader().read(&mut buffer),
            None => self.stream.read(&mut buffer),
        };
        match read {
            Ok(0) => return Ok(()),
            Ok(length) => bytes.extend_from_slice(&buffer[..length]),
            Err(error)
                if error.kind() == ErrorKind::WouldBlock
                    || error.kind() == ErrorKind::Interrupted =>
            {
                self.stage = Stage::Reading(bytes);
                return Ok(());
            }
            Err(_) => return Ok(()),
        }
        if bytes.len() > http::MAX_INPUT {
            self.fail(413, "request_too_large");
            return Ok(());
        }
        let transport = if self.tls.is_some() {
            http::Transport::Tls
        } else {
            http::Transport::Plaintext
        };
        let authority = if self.tls.is_some() {
            self.stream
                .local_addr()
                .map_err(|_| "management TLS address unavailable")?
                .to_string()
        } else {
            context.authority.to_owned()
        };
        match http::parse_policy_transport(
            &bytes,
            &authority,
            context.origins,
            transport,
            context.csrf_origins,
        ) {
            Ok(Some(request)) => {
                if context
                    .authentication_admission
                    .permits(self.peer, &request, Instant::now())
                {
                    if let RequestTarget::Asset(path) = &request.target
                        && let Some(path) = path.strip_prefix("/v1/extensions/")
                    {
                        let parts: Vec<_> = path.splitn(4, '/').collect();
                        let asset = match parts.as_slice() {
                            [id, digest, "assets", name] => {
                                context.extensions.and_then(|e| e.asset(id, digest, name))
                            }
                            _ => None,
                        };
                        self.stage = Stage::Writing {
                            bytes: asset.map_or_else(
                                || crate::assets::failure(404, "Not found"),
                                |(kind, bytes)| crate::assets::asset_response(200, kind, &bytes),
                            ),
                            offset: 0,
                        };
                        return Ok(());
                    }
                    self.authorize(
                        context.runtime,
                        request,
                        context.operation,
                        context.assets,
                        context.selection_owners,
                        context.audio_providers,
                    )?
                } else {
                    self.fail(429, "rate_limited");
                }
            }
            Ok(None) => self.stage = Stage::Reading(bytes),
            Err(failure) => self.fail(failure.status, failure.code),
        }
        Ok(())
    }

    fn verify_result(
        &mut self,
        runtime: &RuntimeClient,
        request_id: u64,
        mut request: http::Request,
        operation: &mut OperationSequence,
        selection_owners: &mut SelectionOwners,
        providers: VerifiedDispatch<'_>,
    ) -> ModuleResult<()> {
        match self.take_result(runtime, request_id)? {
            Response::Pending => {
                self.stage = Stage::Verifying {
                    request_id,
                    request: Box::new(request),
                }
            }
            Response::Result { payload }
                if authorized(&payload, request_scope(&request))
                    && role_permits(&payload, &request) =>
            {
                if matches!(request.target, RequestTarget::Api(route) if route.service == crate::SERVICE && matches!(route.action, "extension_list" | "extension_call"))
                {
                    self.dispatch_extension(
                        runtime,
                        request,
                        &payload,
                        operation,
                        providers.extensions,
                    )?;
                    return Ok(());
                }
                if matches!(request.target, RequestTarget::Api(route) if route.service == "amaterasu.capture-pipewire/1" || route.service == amaterasu_contracts::HOST_LOG_SERVICE)
                {
                    request.verified_identity = capture_management::identity(
                        payload
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        payload
                            .get("subject")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        payload
                            .get("auth_context_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    );
                    if request.verified_identity.is_none() {
                        self.fail(503, "authentication_unavailable");
                        return Ok(());
                    }
                }
                self.dispatch(
                    runtime,
                    request,
                    operation,
                    selection_owners,
                    providers.audio_providers,
                )?
            }
            Response::Result { payload } => {
                match (
                    payload.get("ok").and_then(Value::as_bool),
                    payload.get("code").and_then(Value::as_str),
                ) {
                    (Some(false), Some("unauthorized")) => self.fail(401, "unauthorized"),
                    (_, Some("rate_limited")) => self.fail(429, "rate_limited"),
                    _ => self.fail(403, "unauthorized"),
                }
            }
            _ => self.fence_request(request_id),
        }
        Ok(())
    }

    fn service_result(
        &mut self,
        runtime: &RuntimeClient,
        request_id: u64,
        reply: ServiceReply<'_>,
    ) -> ModuleResult<()> {
        let ServiceReply {
            login,
            operational_health,
            browser_auth,
            capture,
            selection_owners,
        } = reply;
        match self.take_result(runtime, request_id)? {
            Response::Pending => {
                self.stage = Stage::Dispatching {
                    request_id,
                    login,
                    operational_health,
                    browser_auth,
                    capture,
                }
            }
            Response::Result { payload } => match browser_auth {
                Some(action) => self.respond_browser_auth(action, payload),
                None => {
                    if let Some(capture) = capture {
                        let (status, response) =
                            capture_management::response(capture.action, &payload);
                        let remove_owner = capture.action == SelectionAction::Cancel
                            && response.get("cancelled").and_then(Value::as_bool) == Some(true)
                            || capture.action == SelectionAction::Begin
                                && response.get("ok").and_then(Value::as_bool) != Some(true)
                                && capture.created;
                        if remove_owner && let Some(operation_id) = capture.operation_id {
                            selection_owners.remove(&operation_id);
                        }
                        self.respond(status, response);
                    } else {
                        self.respond(service_status(&payload, login, operational_health), payload);
                    }
                }
            },
            _ => self.fence_request(request_id),
        }
        Ok(())
    }

    fn write_response(&mut self, bytes: Vec<u8>, mut offset: usize) {
        if offset == bytes.len() {
            if let Some(tls) = &mut self.tls {
                if !self.tls_closing {
                    tls.send_close_notify();
                    self.tls_closing = true;
                }
                if tls.wants_write() {
                    self.stage = Stage::Writing { bytes, offset };
                }
            }
            return;
        }
        let end = (offset + 256 * 1024).min(bytes.len());
        let write = match &mut self.tls {
            Some(tls) => tls.writer().write(&bytes[offset..end]),
            None => self.stream.write(&bytes[offset..end]),
        };
        match write {
            Ok(0) if self.tls.is_some() => self.stage = Stage::Writing { bytes, offset },
            Ok(0) => {}
            Ok(length) => {
                offset += length;
                if offset < bytes.len() || self.tls.is_some() {
                    self.stage = Stage::Writing { bytes, offset };
                }
            }
            Err(error)
                if error.kind() == ErrorKind::WouldBlock
                    || error.kind() == ErrorKind::Interrupted =>
            {
                self.stage = Stage::Writing { bytes, offset }
            }
            Err(_) => {}
        }
    }

    fn authorize(
        &mut self,
        runtime: &RuntimeClient,
        mut request: http::Request,
        operation: &mut OperationSequence,
        assets: &mut Option<Assets>,
        selection_owners: &mut SelectionOwners,
        audio_providers: &AudioProviders,
    ) -> ModuleResult<()> {
        self.origin.clone_from(&request.origin);
        self.csrf_exception = request
            .csrf_exception
            .then(|| request.origin.clone())
            .flatten();
        if let RequestTarget::Asset(path) = &request.target {
            self.stage = Stage::Writing {
                bytes: assets.as_mut().map_or_else(
                    || crate::assets::failure(404, "Not found"),
                    |assets| assets.response(path),
                ),
                offset: 0,
            };
            return Ok(());
        }
        if matches!(request.target, RequestTarget::OperationalHealth) {
            if peer_policy::classify(self.peer) != PeerTier::Pc {
                self.fail(403, "peer_denied");
                return Ok(());
            }
            if let Some(request_id) = self.submit(
                runtime,
                crate::SERVICE,
                json!({"action":"operational_health"}),
                operation,
            )? {
                self.stage = Stage::Dispatching {
                    request_id,
                    login: false,
                    operational_health: true,
                    browser_auth: None,
                    capture: None,
                };
            }
        } else if request.preflight {
            self.respond(204, Value::Null);
        } else if request_scope(&request).is_some() {
            // Keep the returned-scope check for current tokens so route denial
            // retains its existing response behavior. For predecessor tokens,
            // also send the route's current named scope with its exact HTTP
            // context so a broad legacy path regex cannot bypass policy.
            let payload = verification_payload(&mut request);
            if let Some(request_id) = self.submit(runtime, AUTH_SERVICE, payload, operation)? {
                self.stage = Stage::Verifying {
                    request_id,
                    request: Box::new(request),
                };
            }
        } else {
            self.dispatch(
                runtime,
                request,
                operation,
                selection_owners,
                audio_providers,
            )?;
        }
        Ok(())
    }

    fn dispatch(
        &mut self,
        runtime: &RuntimeClient,
        mut request: http::Request,
        operation: &mut OperationSequence,
        selection_owners: &mut SelectionOwners,
        audio_providers: &AudioProviders,
    ) -> ModuleResult<()> {
        let RequestTarget::Api(route) = &request.target else {
            if let RequestTarget::BrowserAuth(action) = request.target {
                if action == BrowserAuth::Logout && request.payload == json!({}) {
                    self.respond_browser_auth(action, json!({"ok":false,"code":"session_missing"}));
                    return Ok(());
                }
                if let Some(request_id) =
                    self.submit(runtime, crate::AUTH_SERVICE, request.payload, operation)?
                {
                    self.stage = Stage::Dispatching {
                        request_id,
                        login: true,
                        operational_health: false,
                        browser_auth: Some(action),
                        capture: None,
                    };
                }
                return Ok(());
            }
            self.fail(404, "route_not_found");
            return Ok(());
        };
        let route = *route;
        if route.service == "amaterasu.capture-pipewire/1" {
            self.dispatch_capture(
                runtime,
                &mut request,
                route.action,
                operation,
                selection_owners,
            )?;
            return Ok(());
        }
        if route.service == amaterasu_contracts::HOST_LOG_SERVICE {
            let Some(identity) = request.verified_identity.as_ref() else {
                self.fail(503, "authentication_unavailable");
                return Ok(());
            };
            let Some(payload) = request.payload.as_object_mut() else {
                self.fail(400, "invalid_json_body");
                return Ok(());
            };
            payload.insert(
                "owner".into(),
                Value::String(identity.auth_context_id.clone()),
            );
        }
        if route.service == crate::AUTH_SERVICE
            && matches!(
                route.action,
                "list_accounts"
                    | "create_account"
                    | "update_account"
                    | "delete_account"
                    | "set_account_password"
                    | "change_own_password"
                    | "account_settings_get"
                    | "account_settings_update"
                    | "account_sessions_list"
                    | "account_sessions_revoke"
                    | "account_tokens_list"
                    | "account_tokens_issue"
                    | "account_tokens_revoke"
            )
            && let Some(object) = request.payload.as_object_mut()
        {
            object.insert(
                "bearer".into(),
                request.bearer.take().map_or(Value::Null, Value::String),
            );
        }
        if matches!(
            route.service,
            "amaterasu.extension-actions/1" | "amaterasu.session-lifecycle/1"
        ) {
            if let Some(request_id) = self.submit(
                runtime,
                audio_providers.service(route.service),
                request.payload,
                operation,
            )? {
                self.stage = Stage::SessionExtensionDispatching {
                    request_id,
                    action: route.action,
                };
            }
            return Ok(());
        }
        if let Some(request_id) = self.submit(
            runtime,
            audio_providers.service(route.service),
            request.payload,
            operation,
        )? {
            if route.action == "artwork_open" {
                self.deadline = Instant::now() + Duration::from_secs(30);
                self.stage = Stage::Artwork {
                    request_id,
                    service: route.service.into(),
                };
                return Ok(());
            }
            self.stage = Stage::Dispatching {
                request_id,
                login: route.scope.is_none(),
                operational_health: false,
                browser_auth: None,
                capture: None,
            };
        }
        Ok(())
    }

    fn dispatch_extension(
        &mut self,
        runtime: &RuntimeClient,
        mut request: http::Request,
        auth: &Value,
        operation: &mut OperationSequence,
        extensions: Option<&crate::extensions::Extensions>,
    ) -> ModuleResult<()> {
        let RequestTarget::Api(route) = request.target else {
            unreachable!()
        };
        if route.action == "extension_list" {
            self.respond(
                200,
                extensions.map_or_else(|| json!({"ok":true,"extensions":[]}), |e| e.list(auth)),
            );
            return Ok(());
        }
        let Some(extensions) = extensions else {
            self.fail(404, "extension_not_found");
            return Ok(());
        };
        if let Some(object) = request.payload.as_object_mut() {
            object.remove("action");
        }
        let call = match extensions.resolve_call(&request.payload, auth) {
            Ok(call) => call,
            Err(code) => {
                self.fail(403, code);
                return Ok(());
            }
        };
        match runtime.send_ephemeral(call.query())? {
            Response::Result { payload } if call.accepts_approval(&payload) => {}
            _ => {
                self.fail(403, "extension_method_denied");
                return Ok(());
            }
        }
        let Some(operation_id) = operation.next_id() else {
            self.fail(503, "operation_ids_exhausted");
            return Ok(());
        };
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if remaining <= 100 {
            self.fail(504, "deadline_exceeded");
            return Ok(());
        }
        if let Some(id) = self.submit_command(runtime, call.call(remaining - 100, operation_id)) {
            self.stage = Stage::ExtensionDispatching { request_id: id };
        }
        Ok(())
    }

    fn dispatch_capture(
        &mut self,
        runtime: &RuntimeClient,
        request: &mut http::Request,
        action: &str,
        operation: &mut OperationSequence,
        selection_owners: &mut SelectionOwners,
    ) -> ModuleResult<()> {
        let identity = request.verified_identity.as_ref().map(|identity| {
            format!(
                "{}:{}:{}",
                identity.kind, identity.subject, identity.auth_context_id
            )
        });
        let (selection_action, payload, operation_id, created) = match action {
            "status" => (
                SelectionAction::Status,
                json!({"action":"status"}),
                None,
                false,
            ),
            "select_source" => {
                let identity = identity
                    .as_deref()
                    .ok_or("capture authorization identity missing")?;
                let operation_id = request
                    .payload
                    .get("operation_id")
                    .and_then(Value::as_str)
                    .ok_or("capture operation id missing")?
                    .to_owned();
                let source_type = request
                    .payload
                    .get("source_type")
                    .and_then(Value::as_str)
                    .ok_or("capture source type missing")?;
                let created = match selection_owners.begin(&operation_id, identity, Instant::now())
                {
                    Ok(created) => created,
                    Err(()) => {
                        self.fail(409, "operation_conflict_or_limit");
                        return Ok(());
                    }
                };
                (
                    SelectionAction::Begin,
                    json!({"action":"select_source","source_type":source_type,"operation_id":operation_id}),
                    Some(operation_id),
                    created,
                )
            }
            "poll_selection" | "cancel_selection" => {
                let identity = identity
                    .as_deref()
                    .ok_or("capture authorization identity missing")?;
                let operation_id = request
                    .payload
                    .get("operation_id")
                    .and_then(Value::as_str)
                    .ok_or("capture operation id missing")?
                    .to_owned();
                if !selection_owners.permits(&operation_id, identity, Instant::now()) {
                    self.fail(404, "selection_not_found");
                    return Ok(());
                }
                let is_cancel = action == "cancel_selection";
                (
                    if is_cancel {
                        SelectionAction::Cancel
                    } else {
                        SelectionAction::Poll
                    },
                    json!({"action":action,"operation_id":operation_id}),
                    Some(operation_id),
                    false,
                )
            }
            _ => {
                self.fail(404, "route_not_found");
                return Ok(());
            }
        };

        let Some(request_id) =
            self.submit(runtime, "amaterasu.capture-pipewire/1", payload, operation)?
        else {
            if created
                && !matches!(self.stage, Stage::AdmissionUncertain { .. })
                && let Some(operation_id) = &operation_id
            {
                selection_owners.remove(operation_id);
            }
            return Ok(());
        };
        self.stage = Stage::Dispatching {
            request_id,
            login: false,
            operational_health: false,
            browser_auth: None,
            capture: Some(CaptureDispatch {
                action: selection_action,
                operation_id,
                created,
            }),
        };
        Ok(())
    }

    fn submit(
        &mut self,
        runtime: &RuntimeClient,
        service: &str,
        payload: Value,
        operation: &mut OperationSequence,
    ) -> ModuleResult<Option<u64>> {
        let Some(operation_id) = operation.next_id() else {
            self.fail(503, "operation_ids_exhausted");
            return Ok(None);
        };
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if remaining <= 100 {
            self.fail(504, "deadline_exceeded");
            return Ok(None);
        }
        // Network ingress has no runtime parent. Authority is the packaged management
        // principal; token scopes are checked separately before product dispatch.
        let command = Command::Request {
            service: service.into(),
            payload,
            timeout_ms: remaining - 100,
            operation_id,
            parent: None,
        };
        Ok(self.submit_command(runtime, command))
    }

    fn submit_command(&mut self, runtime: &RuntimeClient, command: Command) -> Option<u64> {
        let first_sent = Instant::now();
        match runtime.send(command.clone()) {
            Ok(Response::Accepted { id }) => Some(id),
            Ok(Response::Error { code, .. }) if code != "conflict" => {
                self.fail(503, "service_unavailable");
                None
            }
            _ => {
                self.stage = Stage::AdmissionUncertain {
                    command,
                    first_sent,
                    revoked: false,
                };
                None
            }
        }
    }

    fn fail(&mut self, status: u16, code: &'static str) {
        self.respond(status, json!({"ok":false,"code":code}));
    }

    fn respond(&mut self, status: u16, payload: Value) {
        self.stage = Stage::Writing {
            bytes: http::response(status, &payload, self.origin.as_deref()),
            offset: 0,
        };
    }

    fn respond_browser_auth(&mut self, action: BrowserAuth, mut payload: Value) {
        let status = service_status(&payload, true, false);
        let mut cookies = Vec::new();
        if action == BrowserAuth::Logout
            && (status == 200
                || payload.get("code").and_then(Value::as_str) == Some("session_missing"))
        {
            cookies.extend(browser_cookie_expirations());
        }
        if status == 200 && payload.get("ok").and_then(Value::as_bool) == Some(true) {
            if action != BrowserAuth::Logout && !valid_browser_metadata(&payload) {
                self.fail(502, "invalid_auth_reply");
                return;
            }
            if matches!(action, BrowserAuth::Login | BrowserAuth::Refresh) {
                if payload.get("remember_me").and_then(Value::as_bool) == Some(true)
                    && payload
                        .get("refresh_expires_at")
                        .and_then(Value::as_u64)
                        .is_none_or(|expiry| expiry <= unix_seconds())
                {
                    self.fail(502, "invalid_auth_reply");
                    return;
                }
                let access = payload
                    .get("bearer")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let refresh = payload
                    .get("refresh_token")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let (Some(access), Some(refresh)) = (access, refresh) {
                    if !valid_auth_token(&access) || !valid_auth_token(&refresh) {
                        self.fail(502, "invalid_auth_reply");
                        return;
                    }
                    cookies.push(format!("__Secure-amaterasu-access={access}; Path=/v1; Secure; HttpOnly; SameSite=Strict"));
                    let persistent_age = payload
                        .get("remember_me")
                        .and_then(Value::as_bool)
                        .filter(|remember| *remember)
                        .and_then(|_| payload.get("refresh_expires_at").and_then(Value::as_u64))
                        .and_then(|expires| expires.checked_sub(unix_seconds()))
                        .map(|age| format!("; Max-Age={age}"))
                        .unwrap_or_default();
                    cookies.push(format!("__Secure-amaterasu-refresh={refresh}; Path=/v1/auth/browser; Secure; HttpOnly; SameSite=Strict{persistent_age}"));
                } else {
                    self.fail(502, "invalid_auth_reply");
                    return;
                }
            }
        }
        if let Some(object) = payload.as_object_mut() {
            object.remove("bearer");
            object.remove("refresh_token");
            object.remove("auth_context_id");
        }
        self.stage = Stage::Writing {
            bytes: http::response_with_cookies(status, &payload, None, &cookies),
            offset: 0,
        };
    }
}

fn browser_cookie_expirations() -> [String; 2] {
    [
        "__Secure-amaterasu-access=; Path=/v1; Max-Age=0; Secure; HttpOnly; SameSite=Strict".into(),
        "__Secure-amaterasu-refresh=; Path=/v1/auth/browser; Max-Age=0; Secure; HttpOnly; SameSite=Strict".into(),
    ]
}

fn valid_auth_token(token: &str) -> bool {
    token.strip_prefix("ama_").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn valid_browser_metadata(payload: &Value) -> bool {
    payload.get("kind").and_then(Value::as_str) == Some("administrator_session")
        && matches!(
            payload.get("role").and_then(Value::as_str),
            Some("administrator" | "operator" | "viewer")
        )
        && payload
            .get("subject")
            .and_then(Value::as_str)
            .is_some_and(|subject| !subject.is_empty())
        && payload
            .get("scopes")
            .and_then(Value::as_array)
            .is_some_and(|scopes| scopes.iter().all(|scope| scope.as_str().is_some()))
        && payload
            .get("expires_at")
            .and_then(Value::as_u64)
            .is_some_and(|expiry| (1..=9_007_199_254_740_991).contains(&expiry))
}

fn request_scope(request: &http::Request) -> Option<&'static str> {
    match &request.target {
        RequestTarget::Api(route) => route.scope,
        RequestTarget::BrowserAuth(_) => None,
        RequestTarget::Asset(_) => None,
        RequestTarget::OperationalHealth => None,
    }
}

fn valid_legacy_api_bearer(bearer: &str) -> bool {
    bearer.len() == 32 && bearer.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn verification_payload(request: &mut http::Request) -> Value {
    let legacy_request = request
        .bearer
        .as_deref()
        .filter(|bearer| valid_legacy_api_bearer(bearer))
        .and(request.api_request.as_ref());
    let required_scopes = legacy_request
        .and_then(|_| request_scope(request))
        .into_iter()
        .collect::<Vec<_>>();
    let mut payload = json!({
        "action":"verify",
        "bearer":request.bearer.clone(),
        "required_scopes":required_scopes
    });
    if let Some(context) = legacy_request {
        payload["legacy_request"] = json!({"path":context.path,"method":context.method});
    }
    payload
}

fn authorized(payload: &Value, required_scope: Option<&str>) -> bool {
    required_scope.is_some()
        && payload.get("ok").and_then(Value::as_bool) == Some(true)
        && payload
            .get("scopes")
            .and_then(Value::as_array)
            .is_some_and(|scopes| {
                scopes.iter().all(|scope| scope.as_str().is_some())
                    && scopes.iter().any(|scope| scope.as_str() == required_scope)
            })
}

// Browser roles are an additional boundary on top of the token's exact scopes.
// Legacy and non-browser credentials retain their existing scope policy.
fn session_extension_response(action: &str, mut payload: Value) -> (u16, Value) {
    let Some(object) = payload.as_object_mut() else {
        return (502, json!({"ok":false,"error":{"code":"invalid_response"}}));
    };
    let status = object.get("status").and_then(Value::as_str).unwrap_or("");
    if status == "error" {
        let code = object
            .get("code")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 128)
            .unwrap_or("service_unavailable");
        return (400, json!({"ok":false,"error":{"code":code}}));
    }
    let valid = match action {
        "snapshot" => {
            status == "ok"
                && object
                    .get("host_epoch")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty() && s.len() <= 256)
        }
        "open_epoch" => {
            status == "ok"
                && object
                    .get("epoch")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty() && s.len() <= 256)
        }
        "close_epoch" | "retire_epoch" => status == "ok",
        "invoke" | "status" => {
            matches!(
                status,
                "queued"
                    | "running"
                    | "succeeded"
                    | "rejected"
                    | "cancelled"
                    | "failed"
                    | "recovery_required"
                    | "receipt_expired"
            ) && object
                .get("operation_id")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        }
        _ => false,
    };
    if !valid {
        return (502, json!({"ok":false,"error":{"code":"invalid_response"}}));
    }
    object.insert("ok".into(), Value::Bool(true));
    (200, payload)
}

fn role_permits(payload: &Value, request: &http::Request) -> bool {
    if !payload
        .get("subject")
        .and_then(Value::as_str)
        .is_some_and(|subject| !subject.is_empty())
    {
        return false;
    }
    // Tokens, sessions and trusted publishers are changed only by a signed-in
    // administrator, never by an API token however it is scoped.
    let is_session_only_admin = matches!(request.target, RequestTarget::Api(route)
        if (route.service == crate::AUTH_SERVICE
            && matches!(route.action, "account_tokens_list" | "account_tokens_issue" | "account_tokens_revoke"
                | "account_sessions_list" | "account_sessions_revoke"))
            || (route.service == crate::SERVICE
                && matches!(route.action, "repository_add" | "repository_refresh" | "repository_remove")));
    let role = match payload.get("kind").and_then(Value::as_str) {
        Some("api_token") => return !is_session_only_admin,
        Some("administrator_session") => match payload.get("role").and_then(Value::as_str) {
            Some(role) => role,
            None => return false,
        },
        _ => return false,
    };
    let RequestTarget::Api(route) = request.target else {
        return false;
    };
    if role == "administrator" {
        return true;
    }
    if !matches!(role, "operator" | "viewer") {
        return false;
    }
    let read = matches!(
        (route.service, route.action),
        ("amaterasu.library/1", "artwork_open")
            | ("amaterasu.catalog/1", "get" | "list" | "search")
            | ("amaterasu.launch/1", "status" | "list")
            | ("amaterasu.application/1", "status" | "list")
            | ("amaterasu.session/1", "list" | "snapshot")
            | ("amaterasu.stream-session/1", "management_live_sessions")
            | ("amaterasu.nvhttp/1", "management_launch_status")
            | ("amaterasu.authentication/1", "change_own_password")
            | (
                "amaterasu.management/1",
                "extension_list" | "extension_call"
            )
            | ("amaterasu.steam/1", "list" | "status" | "artwork_open")
            | ("amaterasu.lutris/1", "status" | "artwork_open")
            | ("amaterasu.playnite/1", "status")
            | ("amaterasu.display/1", "status" | "enumerate")
            | (
                "amaterasu.session-history/1",
                "list_sessions"
                    | "session_detail"
                    | "timeline_open"
                    | "timeline_page"
                    | "active_sessions"
                    | "history_status"
            )
    );
    read || (role == "operator"
        && matches!(
            (route.service, route.action),
            ("amaterasu.launch/1", "launch" | "stop")
                | ("amaterasu.nvhttp/1", "management_stop_launch")
        ))
}

fn service_status(payload: &Value, login: bool, operational_health: bool) -> u16 {
    if operational_health {
        return if payload.get("ok").and_then(Value::as_bool) == Some(true) {
            200
        } else {
            503
        };
    }
    if !login || payload.get("ok").and_then(Value::as_bool) == Some(true) {
        return 200;
    }
    if payload.get("code").and_then(Value::as_str) == Some("rate_limited") {
        429
    } else {
        401
    }
}

#[cfg(test)]
fn test_stream_pair() -> (std::net::TcpStream, TcpStream) {
    let network = Network::test_provider();
    let listener = network.bind_tcp("127.0.0.1:0".parse().unwrap()).unwrap();
    let peer = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (stream, _) = listener.accept().unwrap();
    (peer, stream)
}

#[cfg(test)]
mod csrf_revocation_tests;

#[cfg(test)]
mod auth_verification_tests;

#[cfg(test)]
mod provider_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    #[test]
    fn real_loopback_ingress_stays_closed_until_policy_load_and_uses_transport_rules() {
        let tls = crate::tls::configuration(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/browser-cert.pem"
            ),
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/browser-key.pem"
            ),
        )
        .unwrap();
        let mut server = Server::bind(
            "127.0.0.1:0".parse().unwrap(),
            Vec::new(),
            None,
            Some("127.0.0.1:0".parse().unwrap()),
            Some(tls),
        )
        .unwrap();
        let _closed_http = TcpStream::connect(server.address()).unwrap();
        let _closed_tls = TcpStream::connect(server.tls_address().unwrap()).unwrap();
        server.accept().unwrap();
        assert!(server.connections.is_empty());
        server.set_peer_policy(Some(PeerTier::Pc));
        let _http = TcpStream::connect(server.address()).unwrap();
        let _tls = TcpStream::connect(server.tls_address().unwrap()).unwrap();
        server.accept().unwrap();
        assert_eq!(server.connections.len(), 2);
        assert!(
            server
                .connections
                .iter()
                .all(|connection| connection.peer == "127.0.0.1".parse::<IpAddr>().unwrap())
        );
        assert_eq!(
            server
                .connections
                .iter()
                .filter(|connection| connection.tls.is_some())
                .count(),
            1
        );
    }

    fn browser_reply(action: BrowserAuth, payload: Value) -> String {
        let (_peer, client) = test_stream_pair();
        let mut connection = Connection {
            stream: client,
            tls: None,
            tls_closing: false,
            deadline: Instant::now() + CONNECTION_TIMEOUT,
            origin: None,
            csrf_exception: None,
            stage: Stage::Closed,
            terminal_result: None,
            released_request: None,
            peer: "127.0.0.1".parse().unwrap(),
        };
        connection.respond_browser_auth(action, payload);
        let Stage::Writing { bytes, .. } = connection.stage else {
            panic!("expected response")
        };
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn cancelled_terminal_result_retries_release_without_claiming_provider_retirement() {
        let (_peer, client) = test_stream_pair();
        let mut connection = Connection {
            stream: client,
            tls: None,
            tls_closing: false,
            deadline: Instant::now() + CONNECTION_TIMEOUT,
            origin: None,
            csrf_exception: None,
            stage: Stage::Retiring {
                request_id: 41,
                settled: false,
                revoked: true,
                cancelled: true,
            },
            terminal_result: None,
            released_request: None,
            peer: "127.0.0.1".parse().unwrap(),
        };
        let mut commands = Vec::new();
        let first = connection
            .settle_result(41, |command| {
                commands.push(command.clone());
                match command {
                    Command::TakeResult { request: 41 } => {
                        Ok(Response::error("cancelled", "cancelled by request owner"))
                    }
                    Command::ReleaseResult { request: 41 } => {
                        Err("lost release acknowledgement".into())
                    }
                    _ => panic!("unexpected management command"),
                }
            })
            .unwrap();
        assert!(matches!(first, Response::Pending));
        assert!(connection.terminal_result.is_some());
        assert_eq!(connection.released_request, None);

        for failure in [
            Err("release transport failed".to_owned()),
            Ok(Response::error("permission_denied", "wrong owner")),
            Ok(Response::error("unknown", "release outcome unknown")),
        ] {
            assert!(matches!(
                connection
                    .settle_result(41, |command| {
                        commands.push(command.clone());
                        assert!(matches!(command, Command::ReleaseResult { request: 41 }));
                        failure.clone()
                    })
                    .unwrap(),
                Response::Pending
            ));
            assert!(connection.terminal_result.is_some());
            assert_eq!(connection.released_request, None);
        }

        let settled = connection
            .settle_result(41, |command| {
                commands.push(command.clone());
                assert!(matches!(command, Command::ReleaseResult { request: 41 }));
                Ok(Response::error("not_found", "already released"))
            })
            .unwrap();
        assert!(matches!(settled, Response::Error { code, .. } if code == "cancelled"));
        assert!(connection.terminal_result.is_none());
        assert_eq!(connection.released_request, Some(41));
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(command, Command::TakeResult { request: 41 }))
                .count(),
            1
        );
        assert!(
            commands
                .iter()
                .filter(|command| matches!(command, Command::ReleaseResult { request: 41 }))
                .count()
                == 5
        );

        // Broker regression `reliable_retirement_survives_cancel_release_and_result_expiry`
        // proves this caller settlement leaves the provider's unretired receipt intact.
        assert!(matches!(
            connection.stage,
            Stage::Retiring {
                request_id: 41,
                settled: false,
                ..
            }
        ));
    }

    #[test]
    fn browser_login_and_rotation_hide_secrets_and_scope_cookies() {
        let access = format!("ama_{}", "a".repeat(64));
        let refresh = format!("ama_{}", "b".repeat(64));
        for action in [BrowserAuth::Login, BrowserAuth::Refresh] {
            let reply = browser_reply(
                action,
                json!({"ok":true,"bearer":access,"refresh_token":refresh,"kind":"administrator_session","role":"administrator","subject":"admin","scopes":["configuration.read"],"expires_at":1_800_000_000,"remember_me":false,"auth_context_id":"internal-session-id"}),
            );
            let (headers, body) = reply.split_once("\r\n\r\n").unwrap();
            assert!(headers.contains(&format!("Set-Cookie: __Secure-amaterasu-access={access}; Path=/v1; Secure; HttpOnly; SameSite=Strict")));
            assert!(headers.contains(&format!("Set-Cookie: __Secure-amaterasu-refresh={refresh}; Path=/v1/auth/browser; Secure; HttpOnly; SameSite=Strict")));
            assert!(!body.contains(&access));
            assert!(!body.contains(&refresh));
            assert!(!body.contains("auth_context_id"));
            assert!(!body.contains("internal-session-id"));
            assert!(!headers.contains("Access-Control-Allow-Credentials"));
        }
        let logout = browser_reply(BrowserAuth::Logout, json!({"ok":true}));
        assert!(logout.contains("__Secure-amaterasu-access=; Path=/v1; Max-Age=0"));
        assert!(logout.contains("__Secure-amaterasu-refresh=; Path=/v1/auth/browser; Max-Age=0"));
    }

    #[test]
    fn unsuccessful_refresh_does_not_clear_a_newer_cookie_pair() {
        let reply = browser_reply(
            BrowserAuth::Refresh,
            json!({"ok":false,"code":"unauthorized"}),
        );
        assert!(!reply.contains("Set-Cookie:"));
        assert!(reply.starts_with("HTTP/1.1 401"));
    }

    #[test]
    fn logout_without_a_valid_cookie_clears_local_cookies_without_claiming_revocation() {
        let reply = browser_reply(
            BrowserAuth::Logout,
            json!({"ok":false,"code":"session_missing"}),
        );
        assert!(reply.starts_with("HTTP/1.1 401"));
        assert!(reply.contains("__Secure-amaterasu-access=; Path=/v1; Max-Age=0"));
        assert!(reply.contains("__Secure-amaterasu-refresh=; Path=/v1/auth/browser; Max-Age=0"));
        let unavailable = browser_reply(
            BrowserAuth::Logout,
            json!({"ok":false,"code":"service_unavailable"}),
        );
        assert!(!unavailable.contains("Set-Cookie:"));
    }

    #[test]
    fn saturated_tls_plaintext_buffer_retains_unsent_response() {
        let (_peer, client) = test_stream_pair();
        let configuration =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_cert_resolver(Arc::new(rustls::server::ResolvesServerCertUsingSni::new()));
        let mut tls = ServerConnection::new(Arc::new(configuration)).unwrap();
        tls.set_buffer_limit(Some(1));
        assert_eq!(tls.writer().write(b"x").unwrap(), 1);
        let mut connection = Connection {
            stream: client,
            tls: Some(tls),
            tls_closing: false,
            deadline: Instant::now() + CONNECTION_TIMEOUT,
            origin: None,
            csrf_exception: None,
            stage: Stage::Closed,
            terminal_result: None,
            released_request: None,
            peer: "127.0.0.1".parse().unwrap(),
        };
        connection.write_response(b"response".to_vec(), 0);
        assert!(matches!(connection.stage, Stage::Writing { offset: 0, .. }));
    }

    #[test]
    fn verification_requires_explicit_success_and_exact_scope() {
        for payload in [
            json!({"ok":true}),
            json!({"scopes":["catalog.write"]}),
            json!({"ok":false,"scopes":["catalog.write"]}),
            json!({"ok":true,"scopes":["catalog.read"]}),
            json!({"ok":true,"scopes":["*"]}),
        ] {
            assert!(!authorized(&payload, Some("catalog.write")));
        }
        assert!(authorized(
            &json!({"ok":true,"scopes":["catalog.read","catalog.write"]}),
            Some("catalog.write")
        ));
        assert!(!authorized(&json!({"ok":true,"scopes":[null]}), None));
    }

    #[test]
    fn legacy_verification_keeps_route_context_and_named_scope_together() {
        let route = crate::routes::resolve("/v1/catalog/list").unwrap();
        let mut legacy_request = http::Request {
            target: RequestTarget::Api(route),
            payload: json!({}),
            bearer: Some("0123456789abcdefghijklmnopqrstuv".into()),
            verified_identity: None,
            origin: None,
            preflight: false,
            csrf_exception: false,
            api_request: Some(http::ApiRequestContext {
                path: "/v1/catalog/list".into(),
                method: "POST".into(),
            }),
        };
        assert_eq!(
            verification_payload(&mut legacy_request),
            json!({
                "action":"verify",
                "bearer":"0123456789abcdefghijklmnopqrstuv",
                "required_scopes":["catalog.read"],
                "legacy_request":{"path":"/v1/catalog/list","method":"POST"}
            })
        );

        let mut current_request = http::Request {
            target: RequestTarget::Api(route),
            payload: json!({}),
            bearer: Some(format!("ama_{}", "a".repeat(64))),
            verified_identity: None,
            origin: None,
            preflight: false,
            csrf_exception: false,
            api_request: Some(http::ApiRequestContext {
                path: "/v1/catalog/list".into(),
                method: "POST".into(),
            }),
        };
        let current_payload = verification_payload(&mut current_request);
        assert_eq!(current_payload["required_scopes"], json!([]));
        assert!(current_payload.get("legacy_request").is_none());
    }

    #[test]
    fn action_response_distinguishes_receipt_rejection_from_transport_error() {
        let (status, body) = session_extension_response(
            "invoke",
            json!({"status":"rejected","operation_id":"epoch:1","reason":"lifecycle_permit_unavailable"}),
        );
        assert_eq!(status, 200);
        assert_eq!(body["ok"], true);
        assert_eq!(body["status"], "rejected");
        let (status, body) = session_extension_response(
            "invoke",
            json!({"status":"error","code":"permission_denied"}),
        );
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "permission_denied");
        assert_eq!(
            session_extension_response("snapshot", json!({"status":"ok"})).0,
            502
        );
    }

    #[test]
    fn lutris_artwork_scope_cannot_be_satisfied_by_configuration_or_steam_read() {
        let scope = request_scope(&http::Request {
            target: http::RequestTarget::Api(crate::routes::resolve("/v1/lutris/artwork").unwrap()),
            payload: json!({}),
            bearer: None,
            verified_identity: None,
            origin: None,
            preflight: false,
            csrf_exception: false,
            api_request: None,
        });
        assert_eq!(scope, Some("lutris.read"));
        for granted in ["configuration.read", "steam.read", "catalog.read", "*"] {
            assert!(!authorized(&json!({"ok":true,"scopes":[granted]}), scope));
        }
        assert!(authorized(
            &json!({"ok":true,"scopes":["lutris.read"]}),
            scope
        ));
    }

    #[test]
    fn account_roles_limit_routes_even_when_scopes_overlap() {
        let allowed = |role: &str, path: &str| {
            let request = http::Request {
                target: RequestTarget::Api(crate::routes::resolve(path).unwrap()),
                payload: json!({}),
                bearer: None,
                verified_identity: None,
                origin: None,
                preflight: false,
                csrf_exception: false,
                api_request: None,
            };
            role_permits(
                &json!({"kind":"administrator_session", "role":role, "subject":"person"}),
                &request,
            )
        };
        for role in ["viewer", "operator"] {
            assert!(allowed(role, "/v1/launch/status"));
            assert!(allowed(role, "/v1/live-streams/status"));
            assert!(!allowed(role, "/v1/application/launch"));
            assert!(!allowed(role, "/v1/application/terminate"));
            assert!(!allowed(role, "/v1/pairing/list_clients"));
            assert!(!allowed(role, "/v1/pairing/update_client_permissions"));
            assert!(!allowed(role, "/v1/pairing/set_permission_mode"));
            assert!(!allowed(role, "/v1/host-logs/open"));
            assert!(!allowed(role, "/v1/host-logs/export_start"));
            assert!(!allowed(role, "/v1/tokens/issue"));
            assert!(!allowed(role, "/v1/account-sessions/list"));
            assert!(!allowed(role, "/v1/auth/settings"));
            assert!(!allowed(role, "/v1/configuration/get"));
        }
        assert!(!allowed("viewer", "/v1/launch/start"));
        assert!(!allowed("viewer", "/v1/launch/stop"));
        assert!(allowed("viewer", "/v1/nvhttp/launch_status"));
        assert!(!allowed("viewer", "/v1/nvhttp/stop_launch"));
        assert!(allowed("operator", "/v1/launch/start"));
        assert!(allowed("operator", "/v1/launch/stop"));
        assert!(allowed("operator", "/v1/nvhttp/launch_status"));
        assert!(allowed("operator", "/v1/nvhttp/stop_launch"));
        assert!(allowed("administrator", "/v1/pairing/list_clients"));
        assert!(allowed(
            "administrator",
            "/v1/pairing/update_client_permissions"
        ));
        assert!(allowed("administrator", "/v1/pairing/set_permission_mode"));
        assert!(allowed("administrator", "/v1/host-logs/open"));
        assert!(allowed("administrator", "/v1/host-logs/export_start"));
        assert!(allowed("administrator", "/v1/tokens/issue"));
        assert!(allowed("administrator", "/v1/account-sessions/list"));
        let request = http::Request {
            target: RequestTarget::Api(crate::routes::resolve("/v1/tokens/issue").unwrap()),
            payload: json!({}),
            bearer: None,
            verified_identity: None,
            origin: None,
            preflight: false,
            csrf_exception: false,
            api_request: None,
        };
        assert!(!role_permits(
            &json!({"kind":"api_token", "subject":"token"}),
            &request
        ));
        let browser_sessions = http::Request {
            target: RequestTarget::Api(
                crate::routes::resolve("/v1/account-sessions/list").unwrap(),
            ),
            payload: json!({}),
            bearer: None,
            verified_identity: None,
            origin: None,
            preflight: false,
            csrf_exception: false,
            api_request: None,
        };
        assert!(!role_permits(
            &json!({"kind":"api_token", "subject":"token"}),
            &browser_sessions
        ));

        assert!(!allowed("admin", "/v1/pairing/list_clients"));
        assert!(!allowed("", "/v1/launch/status"));

        // Publishers are enrolled and removed only from an administrator's
        // own session; an API token may read the listing and nothing more.
        for path in [
            "/v1/repositories/add",
            "/v1/repositories/refresh",
            "/v1/repositories/remove",
        ] {
            assert!(allowed("administrator", path));
            assert!(!allowed("operator", path));
            assert!(!allowed("viewer", path));
            let change = http::Request {
                target: RequestTarget::Api(crate::routes::resolve(path).unwrap()),
                payload: json!({}),
                bearer: None,
                verified_identity: None,
                origin: None,
                preflight: false,
                csrf_exception: false,
                api_request: None,
            };
            assert!(!role_permits(
                &json!({"kind":"api_token", "subject":"token"}),
                &change
            ));
        }
        let listing = http::Request {
            target: RequestTarget::Api(crate::routes::resolve("/v1/repositories/list").unwrap()),
            payload: json!({}),
            bearer: None,
            verified_identity: None,
            origin: None,
            preflight: false,
            csrf_exception: false,
            api_request: None,
        };
        assert!(role_permits(
            &json!({"kind":"api_token", "subject":"token"}),
            &listing
        ));
    }

    #[test]
    fn login_admission_uses_socket_peer_and_case_folded_account_with_expiry() {
        let mut limiter = AuthenticationAdmission::default();
        let now = Instant::now();
        let request = |username: &str| http::Request {
            target: RequestTarget::BrowserAuth(BrowserAuth::Login),
            payload: json!({"username": username}),
            bearer: None,
            verified_identity: None,
            origin: None,
            preflight: false,
            csrf_exception: false,
            api_request: None,
        };
        let peer = "192.168.1.1".parse().unwrap();
        for _ in 0..5 {
            assert!(limiter.permits(peer, &request("Admin"), now));
        }
        assert!(!limiter.permits(peer, &request("ADMIN"), now));
        assert!(!limiter.permits(
            "::ffff:192.168.1.1".parse().unwrap(),
            &request("admin"),
            now
        ));
        assert!(limiter.permits("192.168.1.20".parse().unwrap(), &request("ADMIN"), now));
        assert!(limiter.permits(peer, &request("different"), now));
        assert!(limiter.permits(peer, &request("admin"), now + Duration::from_secs(60)));
        for n in 0..9 {
            assert!(limiter.permits(
                peer,
                &request(&format!("user{n}")),
                now + Duration::from_secs(60)
            ));
        }
        assert!(!limiter.permits(peer, &request("another"), now + Duration::from_secs(60)));
    }

    #[test]
    fn forwarded_addresses_cannot_change_login_admission_peer() {
        let mut limiter = AuthenticationAdmission::default();
        let now = Instant::now();
        let peer = "192.168.1.5".parse().unwrap();
        for n in 0..6 {
            let body = r#"{"username":"Admin","password":"wrong"}"#;
            let wire = format!(
                "POST /v1/auth/login HTTP/1.1\r\nHost: 127.0.0.1:1234\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Forwarded-For: 192.168.1.{}\r\n\r\n{body}",
                body.len(),
                n + 10
            );
            let request = http::parse(wire.as_bytes(), "127.0.0.1:1234", &[])
                .unwrap()
                .unwrap();
            assert_eq!(limiter.permits(peer, &request, now), n < 5);
        }
    }

    #[test]
    fn full_login_ledger_evicts_oldest_without_blocking_other_peers() {
        let mut limiter = AuthenticationAdmission::default();
        let now = Instant::now();
        let request = http::Request {
            target: RequestTarget::BrowserAuth(BrowserAuth::Login),
            payload: json!({"username":"admin"}),
            bearer: None,
            verified_identity: None,
            origin: None,
            preflight: false,
            csrf_exception: false,
            api_request: None,
        };
        for n in 0..1030 {
            let peer = std::net::Ipv4Addr::new(10, 0, (n / 256) as u8, (n % 256) as u8).into();
            assert!(limiter.permits(peer, &request, now));
        }
        assert_eq!(limiter.attempts.len(), 1024);
        let limited_peer = "192.168.1.5".parse().unwrap();
        for _ in 0..5 {
            assert!(limiter.permits(limited_peer, &request, now));
        }
        for n in 0..5 {
            let other = http::Request {
                target: RequestTarget::BrowserAuth(BrowserAuth::Login),
                payload: json!({"username":format!("other{n}")}),
                bearer: None,
                verified_identity: None,
                origin: None,
                preflight: false,
                csrf_exception: false,
                api_request: None,
            };
            assert!(limiter.permits(limited_peer, &other, now));
        }
        assert_eq!(limiter.attempts.len(), 1024);
        assert!(limiter.permits("192.168.1.6".parse().unwrap(), &request, now));
        assert_eq!(limiter.attempts.len(), 1024);
        assert!(!limiter.permits(limited_peer, &request, now));
        let other = http::Request {
            target: RequestTarget::BrowserAuth(BrowserAuth::Login),
            payload: json!({"username":"unused"}),
            bearer: None,
            verified_identity: None,
            origin: None,
            preflight: false,
            csrf_exception: false,
            api_request: None,
        };
        assert!(!limiter.permits(limited_peer, &other, now));
    }
}
