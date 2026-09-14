//! Tick-owned, nonblocking network connections and asynchronous service delegation.
use crate::assets::Assets;
use crate::http::RequestTarget;
use crate::{AUTH_SERVICE, http};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

const MAX_CONNECTIONS: usize = 16;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Server {
    listener: TcpListener,
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
    ) -> ModuleResult<Self> {
        let listener = TcpListener::bind(address).map_err(|_| "management bind failed")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "management nonblocking listener unavailable")?;
        let address = listener
            .local_addr()
            .map_err(|_| "management local address unavailable")?
            .to_string();
        Ok(Self {
            listener,
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
        for _ in 0..4 {
            if self.connections.len() >= MAX_CONNECTIONS {
                break;
            }
            match self.listener.accept() {
                Ok((stream, peer)) if peer.ip().is_loopback() => {
                    if stream.set_nonblocking(true).is_ok() {
                        self.connections.push(Connection {
                            stream,
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
        match std::mem::replace(&mut self.stage, Stage::Closed) {
            Stage::Reading(bytes) => {
                self.read_request(runtime, bytes, authority, origins, operation, assets)?
            }
            Stage::Verifying {
                request_id,
                request,
            } => self.verify_result(runtime, request_id, request, operation)?,
            Stage::Dispatching { request_id, login } => {
                self.service_result(runtime, request_id, login)?
            }
            Stage::Writing { bytes, offset } => self.write_response(bytes, offset),
            Stage::Closed => {}
        }
        Ok(())
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
        match self.stream.read(&mut buffer) {
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
        match http::parse(&bytes, authority, origins) {
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
    ) -> ModuleResult<()> {
        match runtime.send_ephemeral(Command::TakeResult {
            request: request_id,
        })? {
            Response::Pending => self.stage = Stage::Dispatching { request_id, login },
            Response::Result { payload } => self.respond(service_status(&payload, login), payload),
            _ => self.fail(503, "service_unavailable"),
        }
        Ok(())
    }

    fn write_response(&mut self, bytes: Vec<u8>, mut offset: usize) {
        let end = (offset + 16_384).min(bytes.len());
        match self.stream.write(&bytes[offset..end]) {
            Ok(0) => {}
            Ok(length) => {
                offset += length;
                if offset < bytes.len() {
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
        if request.preflight {
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
            self.fail(404, "route_not_found");
            return Ok(());
        };
        if let Some(request_id) = self.submit(runtime, route.service, request.payload, operation)? {
            self.stage = Stage::Dispatching {
                request_id,
                login: route.scope.is_none(),
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
}

fn request_scope(request: &http::Request) -> Option<&'static str> {
    match &request.target {
        RequestTarget::Api(route) => route.scope,
        RequestTarget::Asset(_) => None,
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

fn service_status(payload: &Value, login: bool) -> u16 {
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
}
