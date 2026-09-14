//! Bounded HTTP/TLS transport for Moonlight-compatible control and pairing.
mod launch;
mod server;
mod settings;
mod tls;
mod wire;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::Deserialize;
use serde_json::json;
use settings::Settings;
use std::time::{Duration, Instant};

const SERVICE: &str = "amaterasu.nvhttp/1";
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Status,
}

struct NvhttpModule {
    runtime: RuntimeClient,
    server: Option<server::Server>,
    router: Option<wire::Router>,
    identity: Option<tls::Identity>,
    certificate: Option<Vec<u8>>,
    tls_error: Option<&'static str>,
    next_refresh: Instant,
    warm: WarmIdentity,
}

impl ManagedModule for NvhttpModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("nvhttp settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid nvhttp settings")?;
        settings.validate()?;
        let (server, router, identity) = if settings.enabled {
            let identity = tls::Identity::bind(&runtime)?;
            let server = server::Server::bind(&settings)?;
            let launches = launch::Controller::new(settings.streaming.clone())?;
            let router = wire::Router::new(
                wire::RouterConfig {
                    hostname: settings.hostname,
                    http_port: server.http_address().port(),
                    https_port: server.https_address().port(),
                },
                launches,
            );
            (Some(server), Some(router), Some(identity))
        } else {
            (None, None, None)
        };
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            server,
            router,
            identity,
            certificate: None,
            tls_error: None,
            next_refresh: Instant::now(),
            warm: WarmIdentity::new(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.refresh_identity();
        if let (Some(server), Some(router)) = (&mut self.server, &mut self.router) {
            let revoked = router.revoke_untrusted(|certificate| {
                self.identity
                    .as_ref()
                    .is_some_and(|identity| identity.trusted(certificate))
            });
            for connection in revoked {
                server.close(connection);
            }
            let events = server.tick();
            for closed in events.closed {
                if closed.response_delivered {
                    router.reply_delivered(closed.id);
                } else {
                    router.cancel(closed.id, &self.runtime);
                }
            }
            for incoming in events.requests {
                let trusted = incoming.secure
                    && incoming
                        .peer_certificate
                        .as_ref()
                        .is_some_and(|certificate| {
                            self.identity
                                .as_ref()
                                .is_some_and(|identity| identity.trusted(certificate))
                        });
                if let Some(reply) = router.dispatch(
                    incoming.id,
                    incoming.request,
                    wire::RequestContext {
                        secure: incoming.secure,
                        remote: incoming.remote,
                        peer_certificate: incoming.peer_certificate,
                        peer_trusted: trusted,
                    },
                    &self.runtime,
                ) {
                    let accepted = server.respond_checked(incoming.id, reply, |certificate| {
                        self.identity
                            .as_ref()
                            .is_some_and(|identity| identity.trusted(certificate))
                    });
                    if !accepted {
                        router.cancel(incoming.id, &self.runtime);
                    }
                }
            }
            for (id, reply) in router.poll(&self.runtime) {
                let accepted = server.respond_checked(id, reply, |certificate| {
                    self.identity
                        .as_ref()
                        .is_some_and(|identity| identity.trusted(certificate))
                });
                if !accepted {
                    router.cancel(id, &self.runtime);
                }
            }
        }
        self.poll_control()
    }
}

impl NvhttpModule {
    fn refresh_identity(&mut self) {
        let Some(identity) = &self.identity else {
            return;
        };
        let available = identity.certificate();
        if available.is_none() {
            self.tls_error = Some("identity_unavailable");
        } else if available.as_ref() == self.certificate.as_ref() {
            self.tls_error = None;
        } else if self.tls_error != Some("tls_configuration_failed") {
            self.tls_error = Some("identity_refresh_pending");
        }
        self.warm.tick(&self.runtime, available.is_none());
        let Some(server) = &mut self.server else {
            return;
        };
        server.set_ready(available.is_some() && available.as_ref() == self.certificate.as_ref());
        if Instant::now() < self.next_refresh {
            return;
        }
        self.next_refresh = Instant::now() + Duration::from_millis(250);
        let Some(certificate) = available else {
            return;
        };
        if self.certificate.as_ref() == Some(&certificate) {
            return;
        }
        match identity.configuration(certificate.clone()) {
            Ok(configuration) => {
                let closed = server.replace_tls(configuration);
                if let Some(router) = &mut self.router {
                    for id in closed {
                        router.cancel(id, &self.runtime);
                    }
                }
                self.certificate = Some(certificate);
                self.tls_error = None;
            }
            Err(_) => {
                server.set_ready(false);
                self.tls_error = Some("tls_configuration_failed");
            }
        }
    }

    fn poll_control(&self) -> ModuleResult<()> {
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                service,
                payload,
                ..
            } => {
                let payload = if service != SERVICE
                    || serde_json::from_value::<Operation>(payload).is_err()
                {
                    json!({"ok":false,"code":"invalid_request"})
                } else {
                    json!({"ok":true,"enabled":self.server.is_some(),"http_address":self.server.as_ref().map(|server|server.http_address().to_string()),"https_address":self.server.as_ref().map(|server|server.https_address().to_string()),"tls_ready":self.server.as_ref().is_some_and(|server|server.tls_ready()),"connections":self.server.as_ref().map_or(0,|server|server.connections()),"tls_error":self.tls_error})
                };
                match self.runtime.send(Command::Complete {
                    request: id,
                    payload,
                })? {
                    Response::Ok => Ok(()),
                    Response::Error { code, .. }
                        if code == "request_ended" || code == "not_found" =>
                    {
                        Ok(())
                    }
                    _ => Err("nvhttp response unavailable".into()),
                }
            }
            _ => Err("nvhttp poll failed".into()),
        }
    }
}

impl Drop for NvhttpModule {
    fn drop(&mut self) {
        if let (Some(server), Some(router)) = (&mut self.server, &mut self.router) {
            for id in server.close_all() {
                router.cancel(id, &self.runtime);
            }
            router.shutdown(&self.runtime);
        }
        // Every rustls connection/configuration is dropped before the identity
        // gate closes and its native binding releases. No worker or signer escapes.
        self.server = None;
        self.warm.cancel(&self.runtime);
        self.identity = None;
    }
}

struct WarmIdentity {
    pending: Option<u64>,
    cancelling: bool,
    sequence: u64,
    next: Instant,
}
impl WarmIdentity {
    fn new() -> Self {
        Self {
            pending: None,
            cancelling: false,
            sequence: 0,
            next: Instant::now(),
        }
    }

    fn tick(&mut self, runtime: &RuntimeClient, needed: bool) {
        if let Some(request) = self.pending {
            if self.cancelling {
                self.cancel(runtime);
                return;
            }
            match runtime.send_ephemeral(Command::TakeResult { request }) {
                Ok(Response::Pending) => {}
                Ok(Response::Result { .. }) => {
                    self.pending = None;
                }
                _ => {
                    self.cancelling = true;
                    self.cancel(runtime);
                }
            }
            return;
        }
        if !needed || Instant::now() < self.next {
            return;
        }
        self.next = Instant::now() + Duration::from_secs(5);
        self.sequence = self.sequence.saturating_add(1);
        let command = Command::Request {
            service: "amaterasu.pairing/1".into(),
            payload: json!({"action":"identity"}),
            timeout_ms: 5000,
            operation_id: format!("nvhttp.identity.{}", self.sequence),
            parent: None,
        };
        if let Ok(Response::Accepted { id }) = runtime.send(command) {
            self.pending = Some(id);
        }
    }

    fn cancel(&mut self, runtime: &RuntimeClient) {
        let Some(request) = self.pending else {
            return;
        };
        for command in [
            Command::Cancel { request },
            Command::ReleaseResult { request },
        ] {
            match runtime.send(command) {
                Ok(Response::Ok) => {}
                Ok(Response::Error { code, .. }) if code == "not_found" => {}
                _ => {
                    self.cancelling = true;
                    return;
                }
            }
        }
        self.pending = None;
        self.cancelling = false;
    }
}

amaterasu_module_sdk::export_module!(NvhttpModule);
