//! Tick-owned, nonblocking network connections and asynchronous service delegation.
use crate::assets::Assets;
use crate::http::{BrowserAuth, RequestTarget};
use crate::{AUTH_SERVICE, http};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use rustls::{ServerConfig, ServerConnection};
use serde_json::{Value, json};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_CONNECTIONS: usize = 16;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Server {
    listener: TcpListener,
    tls_listener: Option<TcpListener>,
    tls_configuration: Option<Arc<ServerConfig>>,
    address: String,
    origins: Vec<String>,
    connections: Vec<Connection>,
    next_operation: u64,
    assets: Option<Assets>,
}

impl Server {
    pub fn bind(
        address: SocketAddr,
        origins: Vec<String>,
        assets: Option<Assets>,
        tls_bind: Option<SocketAddr>,
        tls_configuration: Option<Arc<ServerConfig>>,
    ) -> ModuleResult<Self> {
        let listener = TcpListener::bind(address).map_err(|_| "management bind failed")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "management nonblocking listener unavailable")?;
        let address = listener
            .local_addr()
            .map_err(|_| "management local address unavailable")?
            .to_string();
        let tls_listener = if let Some(bind) = tls_bind {
            let listener = TcpListener::bind(bind).map_err(|_| "management TLS bind failed")?;
            listener
                .set_nonblocking(true)
                .map_err(|_| "management TLS listener unavailable")?;
            Some(listener)
        } else {
            None
        };
        Ok(Self {
            listener,
            tls_listener,
            tls_configuration,
            address,
            origins,
            connections: Vec::new(),
            next_operation: 1,
            assets,
        })
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn tls_address(&self) -> Option<SocketAddr> {
        self.tls_listener.as_ref()?.local_addr().ok()
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<()> {
        self.accept()?;
        for connection in &mut self.connections {
            connection.tick(
                runtime,
                &self.address,
                &self.origins,
                &mut self.next_operation,
                &mut self.assets,
            )?;
        }
        self.connections
            .retain(|connection| !matches!(connection.stage, Stage::Closed));
        Ok(())
    }

    fn accept(&mut self) -> ModuleResult<()> {
        Self::accept_from(&self.listener, None, &mut self.connections)?;
        if let Some(listener) = &self.tls_listener {
            Self::accept_from(
                listener,
                self.tls_configuration.as_ref(),
                &mut self.connections,
            )?;
        }
        Ok(())
    }

    fn accept_from(
        listener: &TcpListener,
        configuration: Option<&Arc<ServerConfig>>,
        connections: &mut Vec<Connection>,
    ) -> ModuleResult<()> {
        for _ in 0..4 {
            if connections.len() >= MAX_CONNECTIONS {
                break;
            }
            match listener.accept() {
                Ok((stream, peer)) if peer.ip().is_loopback() => {
                    if stream.set_nonblocking(true).is_ok() {
                        let tls = configuration
                            .map(|config| ServerConnection::new(config.clone()))
                            .transpose()
                            .map_err(|_| "management TLS connection failed")?;
                        connections.push(Connection {
                            stream,
                            tls,
                            deadline: Instant::now() + CONNECTION_TIMEOUT,
                            stage: Stage::Reading(Vec::new()),
                            origin: None,
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

struct Connection {
    stream: TcpStream,
    tls: Option<ServerConnection>,
    deadline: Instant,
    origin: Option<String>,
    stage: Stage,
}

enum Stage {
    Reading(Vec<u8>),
    Verifying {
        request_id: u64,
        request: http::Request,
    },
    Dispatching {
        request_id: u64,
        login: bool,
        operational_health: bool,
        browser_auth: Option<BrowserAuth>,
    },
    Writing {
        bytes: Vec<u8>,
        offset: usize,
    },
    Closed,
}

impl Connection {
    fn tick(
        &mut self,
        runtime: &RuntimeClient,
        authority: &str,
        origins: &[String],
        operation: &mut u64,
        assets: &mut Option<Assets>,
    ) -> ModuleResult<()> {
        if Instant::now() >= self.deadline {
            self.expire(runtime);
            return Ok(());
        }
        if self.tls.is_some() && !self.tls_io() {
            self.stage = Stage::Closed;
            return Ok(());
        }
        match std::mem::replace(&mut self.stage, Stage::Closed) {
            Stage::Reading(bytes) => {
                self.read_request(runtime, bytes, authority, origins, operation, assets)?
            }
            Stage::Verifying {
                request_id,
                request,
            } => self.verify_result(runtime, request_id, request, operation)?,
            Stage::Dispatching {
                request_id,
                login,
                operational_health,
                browser_auth,
            } => {
                self.service_result(runtime, request_id, login, operational_health, browser_auth)?
            }
            Stage::Writing { bytes, offset } => self.write_response(bytes, offset),
            Stage::Closed => {}
        }
        if self.tls.is_some() && !self.tls_io() {
            self.stage = Stage::Closed;
        }
        Ok(())
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

    fn expire(&mut self, runtime: &RuntimeClient) {
        if let Stage::Verifying { request_id, .. } | Stage::Dispatching { request_id, .. } =
            self.stage
        {
            let _ = runtime.send(Command::Cancel {
                request: request_id,
            });
            let _ = runtime.send(Command::ReleaseResult {
                request: request_id,
            });
        }
        self.stage = Stage::Closed;
    }

    fn read_request(
        &mut self,
        runtime: &RuntimeClient,
        mut bytes: Vec<u8>,
        authority: &str,
        origins: &[String],
        operation: &mut u64,
        assets: &mut Option<Assets>,
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
            authority.to_owned()
        };
        match http::parse_transport(&bytes, &authority, origins, transport) {
            Ok(Some(request)) => self.authorize(runtime, request, operation, assets)?,
            Ok(None) => self.stage = Stage::Reading(bytes),
            Err(failure) => self.fail(failure.status, failure.code),
        }
        Ok(())
    }

    fn verify_result(
        &mut self,
        runtime: &RuntimeClient,
        request_id: u64,
        request: http::Request,
        operation: &mut u64,
    ) -> ModuleResult<()> {
        match runtime.send_ephemeral(Command::TakeResult {
            request: request_id,
        })? {
            Response::Pending => {
                self.stage = Stage::Verifying {
                    request_id,
                    request,
                }
            }
            Response::Result { payload } if authorized(&payload, request_scope(&request)) => {
                self.dispatch(runtime, request, operation)?
            }
            Response::Result { payload } => {
                if payload.get("code").and_then(Value::as_str) == Some("rate_limited") {
                    self.fail(429, "rate_limited");
                } else {
                    self.fail(403, "unauthorized");
                }
            }
            _ => self.fail(503, "authentication_unavailable"),
        }
        Ok(())
    }

    fn service_result(
        &mut self,
        runtime: &RuntimeClient,
        request_id: u64,
        login: bool,
        operational_health: bool,
        browser_auth: Option<BrowserAuth>,
    ) -> ModuleResult<()> {
        match runtime.send_ephemeral(Command::TakeResult {
            request: request_id,
        })? {
            Response::Pending => {
                self.stage = Stage::Dispatching {
                    request_id,
                    login,
                    operational_health,
                    browser_auth,
                }
            }
            Response::Result { payload } => match browser_auth {
                Some(action) => self.respond_browser_auth(action, payload),
                None => self.respond(service_status(&payload, login, operational_health), payload),
            },
            _ => self.fail(503, "service_unavailable"),
        }
        Ok(())
    }

    fn write_response(&mut self, bytes: Vec<u8>, mut offset: usize) {
        if offset == bytes.len() {
            if self.tls.as_ref().is_some_and(|tls| tls.wants_write()) {
                self.stage = Stage::Writing { bytes, offset };
            }
            return;
        }
        let end = (offset + 16_384).min(bytes.len());
        let write = match &mut self.tls {
            Some(tls) => tls.writer().write(&bytes[offset..end]),
            None => self.stream.write(&bytes[offset..end]),
        };
        match write {
            Ok(0) if self.tls.is_some() => self.stage = Stage::Writing { bytes, offset },
            Ok(0) => {}
            Ok(length) => {
                offset += length;
                if offset < bytes.len() || self.tls.as_ref().is_some_and(|tls| tls.wants_write()) {
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
        operation: &mut u64,
        assets: &mut Option<Assets>,
    ) -> ModuleResult<()> {
        self.origin.clone_from(&request.origin);
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
                };
            }
        } else if request.preflight {
            self.respond(204, Value::Null);
        } else if let Some(scope) = request_scope(&request) {
            let payload =
                json!({"action":"verify","bearer":request.bearer.take(),"required_scopes":[scope]});
            if let Some(request_id) = self.submit(runtime, AUTH_SERVICE, payload, operation)? {
                self.stage = Stage::Verifying {
                    request_id,
                    request,
                };
            }
        } else {
            self.dispatch(runtime, request, operation)?;
        }
        Ok(())
    }

    fn dispatch(
        &mut self,
        runtime: &RuntimeClient,
        request: http::Request,
        operation: &mut u64,
    ) -> ModuleResult<()> {
        let RequestTarget::Api(route) = request.target else {
            if let RequestTarget::BrowserAuth(action) = request.target {
                if let Some(request_id) =
                    self.submit(runtime, crate::AUTH_SERVICE, request.payload, operation)?
                {
                    self.stage = Stage::Dispatching {
                        request_id,
                        login: true,
                        operational_health: false,
                        browser_auth: Some(action),
                    };
                }
                return Ok(());
            }
            self.fail(404, "route_not_found");
            return Ok(());
        };
        if let Some(request_id) = self.submit(runtime, route.service, request.payload, operation)? {
            self.stage = Stage::Dispatching {
                request_id,
                login: route.scope.is_none(),
                operational_health: false,
                browser_auth: None,
            };
        }
        Ok(())
    }

    fn submit(
        &mut self,
        runtime: &RuntimeClient,
        service: &str,
        payload: Value,
        operation: &mut u64,
    ) -> ModuleResult<Option<u64>> {
        let Some(next) = operation.checked_add(1) else {
            self.fail(503, "operation_ids_exhausted");
            return Ok(None);
        };
        let operation_id = format!("management.{}", *operation);
        *operation = next;
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
        match runtime.send(Command::Request {
            service: service.into(),
            payload,
            timeout_ms: remaining - 100,
            operation_id,
            parent: None,
        })? {
            Response::Accepted { id } => Ok(Some(id)),
            _ => {
                self.fail(503, "service_unavailable");
                Ok(None)
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
            } else if action == BrowserAuth::Logout {
                cookies.push("__Secure-amaterasu-access=; Path=/v1; Max-Age=0; Secure; HttpOnly; SameSite=Strict".into());
                cookies.push("__Secure-amaterasu-refresh=; Path=/v1/auth/browser; Max-Age=0; Secure; HttpOnly; SameSite=Strict".into());
            }
        }
        if let Some(object) = payload.as_object_mut() {
            object.remove("bearer");
            object.remove("refresh_token");
        }
        self.stage = Stage::Writing {
            bytes: http::response_with_cookies(status, &payload, None, &cookies),
            offset: 0,
        };
    }
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

fn authorized(payload: &Value, required_scope: Option<&str>) -> bool {
    required_scope.is_some()
        && payload.get("ok").and_then(Value::as_bool) == Some(true)
        && payload
            .get("scopes")
            .and_then(Value::as_array)
            .is_some_and(|scopes| scopes.iter().any(|scope| scope.as_str() == required_scope))
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
mod tests {
    use super::*;

    fn browser_reply(action: BrowserAuth, payload: Value) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let mut connection = Connection {
            stream: client,
            tls: None,
            deadline: Instant::now() + CONNECTION_TIMEOUT,
            origin: None,
            stage: Stage::Closed,
        };
        connection.respond_browser_auth(action, payload);
        let Stage::Writing { bytes, .. } = connection.stage else {
            panic!("expected response")
        };
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn browser_login_and_rotation_hide_secrets_and_scope_cookies() {
        let access = format!("ama_{}", "a".repeat(64));
        let refresh = format!("ama_{}", "b".repeat(64));
        for action in [BrowserAuth::Login, BrowserAuth::Refresh] {
            let reply = browser_reply(
                action,
                json!({"ok":true,"bearer":access,"refresh_token":refresh,"kind":"administrator_session","subject":"admin","scopes":["configuration.read"],"expires_at":1_800_000_000,"remember_me":false}),
            );
            let (headers, body) = reply.split_once("\r\n\r\n").unwrap();
            assert!(headers.contains(&format!("Set-Cookie: __Secure-amaterasu-access={access}; Path=/v1; Secure; HttpOnly; SameSite=Strict")));
            assert!(headers.contains(&format!("Set-Cookie: __Secure-amaterasu-refresh={refresh}; Path=/v1/auth/browser; Secure; HttpOnly; SameSite=Strict")));
            assert!(!body.contains(&access));
            assert!(!body.contains(&refresh));
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
    fn saturated_tls_plaintext_buffer_retains_unsent_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
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
            deadline: Instant::now() + CONNECTION_TIMEOUT,
            origin: None,
            stage: Stage::Closed,
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
    fn lutris_artwork_scope_cannot_be_satisfied_by_configuration_or_steam_read() {
        let scope = request_scope(&http::Request {
            target: http::RequestTarget::Api(
                crate::routes::resolve("/v1/lutris/artwork_read").unwrap(),
            ),
            payload: json!({}),
            bearer: None,
            origin: None,
            preflight: false,
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
}
