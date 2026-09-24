//! Bounded HTTP/TLS transport for Moonlight-compatible control and pairing.
mod launch;
mod network_settings;
mod product_settings;
mod server;
mod settings;
mod tls;
mod wire;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use network_settings::NetworkSettings;
use product_settings::{AdminAction, ProductSettings};
use serde::Deserialize;
use serde_json::json;
use settings::Settings;
use std::time::{Duration, Instant};

const SERVICE: &str = "amaterasu.nvhttp/1";
const MANAGEMENT_PRINCIPAL: &str = "amaterasu.management";

fn should_attempt_listener_rebind(
    server: Option<&server::Server>,
    product_settings: &ProductSettings,
) -> bool {
    server.is_some() && product_settings.port_needs_application()
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Status,
    SettingsGet,
    SettingsUpdate {
        hostname: String,
        port: u16,
        expected_revision: u64,
    },
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
    product_settings: ProductSettings,
    network_settings: NetworkSettings,
    next_listener_rebind: Instant,
    listener_rebind_delay: Duration,
}

impl ManagedModule for NvhttpModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("nvhttp settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid nvhttp settings")?;
        settings.validate()?;
        let hostname = settings::effective_hostname(&settings.hostname);
        let product_settings = ProductSettings::new(hostname.clone(), settings.http_bind.port());
        let (server, router, identity) = if settings.enabled {
            let identity = tls::Identity::bind(&runtime)?;
            let server = server::Server::bind(&settings)?;
            let launches = launch::Controller::new(settings.streaming.clone())?;
            let router = wire::Router::new(
                wire::RouterConfig {
                    hostname: hostname.clone(),
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
            product_settings,
            network_settings: NetworkSettings::default(),
            next_listener_rebind: Instant::now(),
            listener_rebind_delay: Duration::from_secs(1),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.update_active_listener_ports();
        if let Some(settings) = self.product_settings.tick(&self.runtime)? {
            if let Some(router) = &mut self.router {
                router.set_hostname(settings.hostname);
            }
            if self
                .server
                .as_ref()
                .is_some_and(|server| server.http_address().port() != settings.port)
            {
                self.next_listener_rebind = Instant::now();
            }
        }
        if self.product_settings.loaded() {
            self.network_settings.tick(
                &self.runtime,
                self.product_settings.port(),
                &mut self.product_settings,
            )?;
            self.product_settings
                .set_active_rtsp_base_port(self.network_settings.active_base_port());
        }
        if self.server.is_none() {
            self.product_settings.record_listener_disabled();
        } else if should_attempt_listener_rebind(self.server.as_ref(), &self.product_settings)
            && Instant::now() >= self.next_listener_rebind
        {
            let port = self.product_settings.port();
            let result = self
                .server
                .as_mut()
                .expect("a live listener is required to apply a port")
                .rebind_port(port);
            match result {
                Ok(()) => {
                    self.update_active_listener_ports();
                    self.product_settings.record_port_apply_success();
                    self.listener_rebind_delay = Duration::from_secs(1);
                    self.next_listener_rebind = Instant::now() + self.listener_rebind_delay;
                }
                Err(error) => {
                    self.product_settings.record_port_apply_failure(port, error);
                    self.next_listener_rebind = Instant::now() + self.listener_rebind_delay;
                    self.listener_rebind_delay =
                        (self.listener_rebind_delay * 2).min(Duration::from_secs(30));
                }
            }
        } else if self.server.is_some() && self.product_settings.port_is_active() {
            self.product_settings.record_port_apply_success();
        }
        self.product_settings.flush_completion(&self.runtime);
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
    fn update_active_listener_ports(&mut self) {
        let addresses = self
            .server
            .as_ref()
            .map(|server| (server.http_address().port(), server.https_address().port()));
        if let Some((http_port, https_port)) = addresses {
            if let Some(router) = &mut self.router {
                router.set_ports(http_port, https_port);
            }
            self.product_settings
                .set_active_ports(Some(http_port), Some(https_port));
        } else {
            self.product_settings.set_active_ports(None, None);
        }
    }

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

    fn poll_control(&mut self) -> ModuleResult<()> {
        match self.runtime.send(Command::Poll {})? {
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
                    return complete_control(
                        &self.runtime,
                        id,
                        json!({"ok":false,"code":"invalid_request"}),
                    );
                }
                let operation = match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) => operation,
                    Err(_) => {
                        return complete_control(
                            &self.runtime,
                            id,
                            json!({"ok":false,"code":"invalid_request"}),
                        );
                    }
                };
                match operation {
                    Operation::Status => complete_control(
                        &self.runtime,
                        id,
                        json!({
                            "ok":true,
                            "enabled":self.server.is_some(),
                            "http_address":self.server.as_ref().map(|server|server.http_address().to_string()),
                            "https_address":self.server.as_ref().map(|server|server.https_address().to_string()),
                            "http_addresses":self.server.as_ref().map(|server|server.http_addresses().iter().map(ToString::to_string).collect::<Vec<_>>()),
                            "https_addresses":self.server.as_ref().map(|server|server.https_addresses().iter().map(ToString::to_string).collect::<Vec<_>>()),
                            "tls_ready":self.server.as_ref().is_some_and(|server|server.tls_ready()),
                            "connections":self.server.as_ref().map_or(0,|server|server.connections()),
                            "tls_error":self.tls_error,
                            "settings_hostname":self.product_settings.hostname(),
                            "settings_configured_hostname":self.product_settings.configured_hostname(),
                            "settings_port":self.product_settings.port(),
                            "active_http_port":self.server.as_ref().map(|server|server.http_address().port()),
                            "active_https_port":self.server.as_ref().map(|server|server.https_address().port()),
                            "settings_loaded":self.product_settings.revision().is_some(),
                            "settings_revision":self.product_settings.revision(),
                            "settings_error":self.product_settings.last_error(),
                        }),
                    ),
                    settings_action @ (Operation::SettingsGet
                    | Operation::SettingsUpdate { .. }) => {
                        if caller != MANAGEMENT_PRINCIPAL {
                            return complete_control(
                                &self.runtime,
                                id,
                                json!({"ok":false,"code":"permission_denied"}),
                            );
                        }
                        let action = match settings_action {
                            Operation::SettingsGet => AdminAction::Get,
                            Operation::SettingsUpdate {
                                hostname,
                                port,
                                expected_revision,
                            } => {
                                if settings::validate_configured_hostname(&hostname).is_err() {
                                    return complete_control(
                                        &self.runtime,
                                        id,
                                        json!({"ok":false,"code":"invalid_hostname"}),
                                    );
                                }
                                if settings::validate_port(port).is_err() {
                                    return complete_control(
                                        &self.runtime,
                                        id,
                                        json!({"ok":false,"code":"invalid_port"}),
                                    );
                                }
                                AdminAction::Update {
                                    hostname,
                                    port,
                                    expected_revision,
                                    require_listener_apply: self.server.is_some(),
                                }
                            }
                            Operation::Status => unreachable!(),
                        };
                        if let Err(code) = self.product_settings.enqueue(id, remaining_ms, action) {
                            complete_control(
                                &self.runtime,
                                id,
                                json!({"ok":false,"code":"resource_exhausted","message":code}),
                            )
                        } else {
                            Ok(())
                        }
                    }
                }
            }
            _ => Err("nvhttp poll failed".into()),
        }
    }
}

fn complete_control(
    runtime: &RuntimeClient,
    request: u64,
    payload: serde_json::Value,
) -> ModuleResult<()> {
    match runtime.send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        _ => Err("nvhttp response unavailable".into()),
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
