//! Bounded HTTP/TLS transport for Moonlight-compatible control and pairing.
#![recursion_limit = "256"]

mod frame_limiter;
mod launch;
mod network_settings;
mod preparation;
mod product_settings;
mod server;
mod settings;
mod tls;
mod wire;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use launch::CaptureMode;
use network_settings::NetworkSettings;
use preparation::LegacyPreparationStep;
use product_settings::{
    AdminAction, EncryptionModeUpdate, FieldUpdate, FrameLimiterUpdate, NetworkTarget,
    ProductSettings,
};
use serde::{Deserialize, Deserializer};
use serde_json::json;
use settings::Settings;
use std::time::{Duration, Instant};

const SERVICE: &str = "amaterasu.nvhttp/1";
const MANAGEMENT_PRINCIPAL: &str = "amaterasu.management";

fn should_attempt_listener_rebind(
    server: Option<&server::Server>,
    product_settings: &ProductSettings,
) -> bool {
    server.is_some() && product_settings.listeners_need_application()
}

fn listener_settings_for_target(
    startup_settings: &Settings,
    target: &NetworkTarget,
) -> Result<Settings, String> {
    settings::validate_port(target.port).map_err(str::to_owned)?;
    let mut listener_settings = startup_settings.clone();
    listener_settings.http_bind.set_port(target.port);
    listener_settings
        .https_bind
        .set_port(settings::https_port(target.port));
    listener_settings.address_family = target.address_family;
    listener_settings.bind_address = target.bind_address.clone();
    listener_settings.validate().map_err(str::to_owned)?;
    Ok(listener_settings)
}

fn listener_target_matches_server(
    server: &server::Server,
    startup_settings: &Settings,
    target: &NetworkTarget,
) -> bool {
    let Ok(listener_settings) = listener_settings_for_target(startup_settings, target) else {
        return false;
    };
    let Ok(http_addresses) = listener_settings.resolved_http_binds() else {
        return false;
    };
    let Ok(https_addresses) = listener_settings.resolved_https_binds() else {
        return false;
    };
    server.http_addresses() == http_addresses && server.https_addresses() == https_addresses
}

fn apply_listener_target(
    server: &mut server::Server,
    startup_settings: &Settings,
    target: &NetworkTarget,
) -> Result<(), String> {
    let listener_settings = listener_settings_for_target(startup_settings, target)?;
    server.rebind_settings(&listener_settings)
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
// The update variant intentionally carries all typed fields to preserve the
// omission/null/value distinction across the signed settings request.
#[allow(clippy::large_enum_variant)]
enum Operation {
    Status,
    SettingsGet,
    SettingsUpdate {
        hostname: String,
        port: u16,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        address_family: FieldUpdate<settings::AddressFamily>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        bind_address: FieldUpdate<String>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        output_name: FieldUpdate<String>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        audio_sink: FieldUpdate<String>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        virtual_sink: FieldUpdate<String>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        audio_sink_capture_only: FieldUpdate<bool>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        nvenc_spatial_aq: FieldUpdate<bool>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        external_ip: FieldUpdate<String>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        stream_audio: FieldUpdate<bool>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        capture: FieldUpdate<CaptureMode>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        video_max_batch_size_kb: FieldUpdate<u8>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        max_bitrate: FieldUpdate<i32>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        minimum_fps_target: FieldUpdate<f64>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        wgc_pacing_smoothing: FieldUpdate<bool>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        ping_timeout_ms: FieldUpdate<i32>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        global_preparation: FieldUpdate<Vec<preparation::PreparationStep>>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        legacy_global_preparation: FieldUpdate<Vec<LegacyPreparationStep>>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        frame_limiter_enable: FieldUpdate<bool>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        frame_limiter_fps_limit: FieldUpdate<f64>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        frame_limiter_provider: FieldUpdate<String>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        frame_limiter_disable_vsync: FieldUpdate<bool>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        frame_limiter_auto_virtual_framegen: FieldUpdate<String>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        rtss_frame_limit_type: FieldUpdate<String>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        rtss_allow_virtual_display_override: FieldUpdate<bool>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        rtss_install_path: FieldUpdate<String>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        mangohud_limiter_method: FieldUpdate<String>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        mangohud_preset: FieldUpdate<String>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        mangohud_always_show_graph: FieldUpdate<bool>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        rtss_disable_vsync_ullm: FieldUpdate<bool>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        lan_encryption_mode: FieldUpdate<i64>,
        #[serde(default, deserialize_with = "deserialize_field_update")]
        wan_encryption_mode: FieldUpdate<i64>,
        expected_revision: u64,
    },
}

fn deserialize_field_update<'de, D, T>(deserializer: D) -> Result<FieldUpdate<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(|value| match value {
        Some(value) => FieldUpdate::Value(value),
        None => FieldUpdate::Null,
    })
}

struct NvhttpModule {
    runtime: RuntimeClient,
    startup_settings: Settings,
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
        let mut product_settings =
            ProductSettings::with_bootstrap_settings(hostname.clone(), &settings);
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
        if let Some(server) = &server {
            let target = product_settings.http_target();
            product_settings.set_active_http_state(
                Some(target),
                Some(
                    server
                        .http_addresses()
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                ),
                Some(
                    server
                        .https_addresses()
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                ),
            );
        }
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            startup_settings: settings,
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
            if self.server.as_ref().is_some_and(|server| {
                !listener_target_matches_server(
                    server,
                    &self.startup_settings,
                    &self.product_settings.http_target(),
                )
            }) {
                self.next_listener_rebind = Instant::now();
            }
        }
        if let Some(router) = &mut self.router {
            // Apply settings before completing management writes and before dispatching
            // HTTP requests later in this tick, so a returned update is launch-visible.
            router.set_output_name_settings(
                self.product_settings.loaded(),
                self.product_settings.configured_output_name(),
            );
            router.set_audio_sink_settings(self.product_settings.configured_audio_sink());
            router.set_virtual_sink_settings(self.product_settings.configured_virtual_sink());
            router.set_global_preparation(self.product_settings.global_preparation());
            router.set_audio_sink_capture_only(self.product_settings.audio_sink_capture_only());
            router.set_global_stream_audio(self.product_settings.stream_audio());
            router.set_global_ping_timeout_ms(self.product_settings.effective_ping_timeout_ms());
            router.set_capture_mode(self.product_settings.configured_capture());
            router.set_global_video_max_batch_size_kb(
                self.product_settings.effective_video_max_batch_size_kb(),
            );
            router.set_global_max_bitrate_kbps(self.product_settings.effective_max_bitrate_kbps());
            router.set_global_minimum_fps_target(
                self.product_settings.effective_minimum_fps_target(),
            );
            router.set_global_wgc_pacing_smoothing(
                self.product_settings.effective_wgc_pacing_smoothing(),
            );
        }
        if self.product_settings.loaded() {
            self.network_settings.tick(
                &self.runtime,
                self.product_settings.port(),
                &mut self.product_settings,
            )?;
        }
        if self.server.is_none() {
            self.product_settings.record_listener_disabled();
        } else if should_attempt_listener_rebind(self.server.as_ref(), &self.product_settings)
            && Instant::now() >= self.next_listener_rebind
        {
            let target = self.product_settings.http_target();
            let result = apply_listener_target(
                self.server
                    .as_mut()
                    .expect("a live listener is required to apply network settings"),
                &self.startup_settings,
                &target,
            );
            match result {
                Ok(()) => {
                    self.product_settings.set_active_http_target(target);
                    self.update_active_listener_ports();
                    self.product_settings.record_listener_apply_success();
                    self.listener_rebind_delay = Duration::from_secs(1);
                    self.next_listener_rebind = Instant::now() + self.listener_rebind_delay;
                }
                Err(error) => {
                    self.product_settings
                        .record_listener_apply_failure(&target, error);
                    self.next_listener_rebind = Instant::now() + self.listener_rebind_delay;
                    self.listener_rebind_delay =
                        (self.listener_rebind_delay * 2).min(Duration::from_secs(30));
                }
            }
        } else if self.server.is_some() && !self.product_settings.listeners_need_application() {
            self.product_settings.record_listener_apply_success();
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
            .map(|server| (server.http_addresses(), server.https_addresses()));
        if let Some((http, https)) = addresses {
            if let Some(router) = &mut self.router {
                router.set_ports(http[0].port(), https[0].port());
            }
            self.product_settings.set_active_http_addresses(
                Some(http.iter().map(ToString::to_string).collect()),
                Some(https.iter().map(ToString::to_string).collect()),
            );
        } else {
            self.product_settings
                .set_active_http_state(None, None, None);
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
                            "settings_address_family":self.product_settings.effective_address_family(),
                            "settings_bind_address":self.product_settings.effective_bind_address(),
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
                                address_family,
                                bind_address,
                                output_name,
                                audio_sink,
                                virtual_sink,
                                audio_sink_capture_only,
                                nvenc_spatial_aq,
                                external_ip,
                                stream_audio,
                                capture,
                                video_max_batch_size_kb,
                                max_bitrate,
                                minimum_fps_target,
                                wgc_pacing_smoothing,
                                ping_timeout_ms,
                                global_preparation,
                                legacy_global_preparation,
                                frame_limiter_enable,
                                frame_limiter_fps_limit,
                                frame_limiter_provider,
                                frame_limiter_disable_vsync,
                                frame_limiter_auto_virtual_framegen,
                                rtss_frame_limit_type,
                                rtss_allow_virtual_display_override,
                                rtss_install_path,
                                mangohud_limiter_method,
                                mangohud_preset,
                                mangohud_always_show_graph,
                                rtss_disable_vsync_ullm,
                                lan_encryption_mode,
                                wan_encryption_mode,
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
                                    address_family,
                                    bind_address,
                                    output_name,
                                    audio_sink,
                                    virtual_sink,
                                    audio_sink_capture_only,
                                    nvenc_spatial_aq,
                                    external_ip,
                                    stream_audio,
                                    capture,
                                    video_max_batch_size_kb,
                                    max_bitrate,
                                    minimum_fps_target,
                                    wgc_pacing_smoothing,
                                    ping_timeout_ms,
                                    global_preparation,
                                    legacy_global_preparation,
                                    frame_limiter: FrameLimiterUpdate {
                                        frame_limiter_enable,
                                        frame_limiter_fps_limit,
                                        frame_limiter_provider,
                                        frame_limiter_disable_vsync,
                                        frame_limiter_auto_virtual_framegen,
                                        rtss_frame_limit_type,
                                        rtss_allow_virtual_display_override,
                                        rtss_install_path,
                                        mangohud_limiter_method,
                                        mangohud_preset,
                                        mangohud_always_show_graph,
                                        rtss_disable_vsync_ullm,
                                    },
                                    encryption_modes: EncryptionModeUpdate {
                                        lan_encryption_mode,
                                        wan_encryption_mode,
                                    },
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn startup_listener_settings() -> Settings {
        Settings {
            enabled: true,
            http_bind: "127.0.0.1:0".parse().unwrap(),
            https_bind: "127.0.0.1:0".parse().unwrap(),
            ..Settings::default()
        }
    }

    fn occupied_valid_http_port() -> (TcpListener, u16) {
        for _ in 0..32 {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            if settings::validate_port(port).is_err() {
                continue;
            }
            if let Ok(https_listener) = TcpListener::bind(("127.0.0.1", settings::https_port(port)))
            {
                drop(https_listener);
                return (listener, port);
            }
        }
        panic!("could not reserve an available HTTP/HTTPS port pair");
    }

    fn target(port: u16) -> NetworkTarget {
        NetworkTarget {
            port,
            address_family: Some(settings::AddressFamily::Ipv4),
            bind_address: Some("127.0.0.1".into()),
        }
    }

    #[test]
    fn settings_update_deserializer_distinguishes_omitted_null_and_values() {
        let Operation::SettingsUpdate {
            address_family: omitted_family,
            bind_address: omitted_bind,
            output_name: omitted_output,
            audio_sink: omitted_sink,
            stream_audio: omitted_audio,
            capture: omitted_capture,
            video_max_batch_size_kb: omitted_batch_size,
            max_bitrate: omitted_max_bitrate,
            minimum_fps_target: omitted_minimum_fps,
            wgc_pacing_smoothing: omitted_wgc_pacing,
            ping_timeout_ms: omitted_ping_timeout,
            lan_encryption_mode: omitted_lan_encryption,
            wan_encryption_mode: omitted_wan_encryption,
            nvenc_spatial_aq: omitted_nvenc_spatial_aq,
            ..
        } = serde_json::from_value(json!({
            "action":"settings_update","hostname":"Host","port":48000,"expected_revision":2
        }))
        .unwrap()
        else {
            panic!("settings update expected");
        };
        assert_eq!(omitted_family, FieldUpdate::Omitted);
        assert_eq!(omitted_bind, FieldUpdate::Omitted);
        assert_eq!(omitted_output, FieldUpdate::Omitted);
        assert_eq!(omitted_sink, FieldUpdate::Omitted);
        assert_eq!(omitted_audio, FieldUpdate::Omitted);
        assert_eq!(omitted_capture, FieldUpdate::Omitted);
        assert_eq!(omitted_batch_size, FieldUpdate::Omitted);
        assert_eq!(omitted_max_bitrate, FieldUpdate::Omitted);
        assert_eq!(omitted_minimum_fps, FieldUpdate::Omitted);
        assert_eq!(omitted_wgc_pacing, FieldUpdate::Omitted);
        assert_eq!(omitted_ping_timeout, FieldUpdate::Omitted);
        assert_eq!(omitted_lan_encryption, FieldUpdate::Omitted);
        assert_eq!(omitted_wan_encryption, FieldUpdate::Omitted);
        assert_eq!(omitted_nvenc_spatial_aq, FieldUpdate::Omitted);

        let Operation::SettingsUpdate {
            address_family: reset_family,
            bind_address: blank_bind,
            output_name: reset_output,
            audio_sink: reset_sink,
            stream_audio: null_audio,
            capture: null_capture,
            video_max_batch_size_kb: null_batch_size,
            max_bitrate: null_max_bitrate,
            minimum_fps_target: null_minimum_fps,
            wgc_pacing_smoothing: null_wgc_pacing,
            ping_timeout_ms: null_ping_timeout,
            lan_encryption_mode: null_lan_encryption,
            wan_encryption_mode: null_wan_encryption,
            nvenc_spatial_aq: null_nvenc_spatial_aq,
            ..
        } = serde_json::from_value(json!({
            "action":"settings_update","hostname":"Host","port":48000,"expected_revision":2,
            "address_family":null,"bind_address":"","output_name":null,"audio_sink":null,"stream_audio":null,"capture":null,
            "video_max_batch_size_kb":null,"max_bitrate":null,"minimum_fps_target":null,"wgc_pacing_smoothing":null,"ping_timeout_ms":null,
            "lan_encryption_mode":null,"wan_encryption_mode":null,"nvenc_spatial_aq":null
        }))
        .unwrap()
        else {
            panic!("settings update expected");
        };
        assert_eq!(reset_family, FieldUpdate::Null);
        assert_eq!(blank_bind, FieldUpdate::Value(String::new()));
        assert_eq!(reset_output, FieldUpdate::Null);
        assert_eq!(reset_sink, FieldUpdate::Null);
        assert_eq!(null_audio, FieldUpdate::Null);
        assert_eq!(null_capture, FieldUpdate::Null);
        assert_eq!(null_batch_size, FieldUpdate::Null);
        assert_eq!(null_max_bitrate, FieldUpdate::Null);
        assert_eq!(null_minimum_fps, FieldUpdate::Null);
        assert_eq!(null_wgc_pacing, FieldUpdate::Null);
        assert_eq!(null_ping_timeout, FieldUpdate::Null);
        assert_eq!(null_lan_encryption, FieldUpdate::Null);
        assert_eq!(null_wan_encryption, FieldUpdate::Null);
        assert_eq!(null_nvenc_spatial_aq, FieldUpdate::Null);

        let Operation::SettingsUpdate {
            address_family: family,
            bind_address: address,
            output_name,
            audio_sink,
            stream_audio,
            capture,
            video_max_batch_size_kb,
            max_bitrate,
            minimum_fps_target,
            wgc_pacing_smoothing,
            ping_timeout_ms,
            lan_encryption_mode,
            wan_encryption_mode,
            nvenc_spatial_aq,
            ..
        } = serde_json::from_value(json!({
            "action":"settings_update","hostname":"Host","port":48000,"expected_revision":2,
            "address_family":"both","bind_address":"::1","output_name":"stable-display-id","audio_sink":"render.selected","stream_audio":false,"capture":"wgcc",
            "video_max_batch_size_kb":32,"max_bitrate":-15000,"minimum_fps_target":23.5,"wgc_pacing_smoothing":false,"ping_timeout_ms":-1,
            "lan_encryption_mode":2,"wan_encryption_mode":0,"nvenc_spatial_aq":false
        }))
        .unwrap()
        else {
            panic!("settings update expected");
        };
        assert_eq!(family, FieldUpdate::Value(settings::AddressFamily::Both));
        assert_eq!(address, FieldUpdate::Value("::1".into()));
        assert_eq!(output_name, FieldUpdate::Value("stable-display-id".into()));
        assert_eq!(audio_sink, FieldUpdate::Value("render.selected".into()));
        assert_eq!(stream_audio, FieldUpdate::Value(false));
        assert_eq!(capture, FieldUpdate::Value(CaptureMode::Wgcc));
        assert_eq!(video_max_batch_size_kb, FieldUpdate::Value(32));
        assert_eq!(max_bitrate, FieldUpdate::Value(-15_000));
        assert_eq!(minimum_fps_target, FieldUpdate::Value(23.5));
        assert_eq!(wgc_pacing_smoothing, FieldUpdate::Value(false));
        assert_eq!(ping_timeout_ms, FieldUpdate::Value(-1));
        assert_eq!(lan_encryption_mode, FieldUpdate::Value(2));
        assert_eq!(wan_encryption_mode, FieldUpdate::Value(0));
        assert_eq!(nvenc_spatial_aq, FieldUpdate::Value(false));

        assert!(
            serde_json::from_value::<Operation>(json!({
                "action":"settings_update","hostname":"Host","port":48000,
                "expected_revision":2,"capture":"fallback"
            }))
            .is_err()
        );
        for invalid in [json!(0), json!("false")] {
            assert!(
                serde_json::from_value::<Operation>(json!({
                    "action":"settings_update","hostname":"Host","port":48000,
                    "expected_revision":2,"nvenc_spatial_aq":invalid
                }))
                .is_err()
            );
        }
    }

    #[test]
    fn frame_limiter_settings_update_preserves_nullable_false_zero_and_legacy_alias() {
        let Operation::SettingsUpdate {
            frame_limiter_enable: omitted_enable,
            frame_limiter_fps_limit: omitted_fps_limit,
            frame_limiter_disable_vsync: omitted_disable_vsync,
            rtss_disable_vsync_ullm: omitted_alias,
            ..
        } = serde_json::from_value(json!({
            "action":"settings_update","hostname":"Host","port":48000,"expected_revision":2
        }))
        .unwrap()
        else {
            panic!("settings update expected");
        };
        assert_eq!(omitted_enable, FieldUpdate::Omitted);
        assert_eq!(omitted_fps_limit, FieldUpdate::Omitted);
        assert_eq!(omitted_disable_vsync, FieldUpdate::Omitted);
        assert_eq!(omitted_alias, FieldUpdate::Omitted);

        let Operation::SettingsUpdate {
            frame_limiter_enable: explicit_false,
            frame_limiter_fps_limit: explicit_zero,
            rtss_disable_vsync_ullm: legacy_alias,
            ..
        } = serde_json::from_value(json!({
            "action":"settings_update","hostname":"Host","port":48000,"expected_revision":2,
            "frame_limiter_enable":false,"frame_limiter_fps_limit":0.0,
            "rtss_disable_vsync_ullm":false
        }))
        .unwrap()
        else {
            panic!("settings update expected");
        };
        assert_eq!(explicit_false, FieldUpdate::Value(false));
        assert_eq!(explicit_zero, FieldUpdate::Value(0.0));
        assert_eq!(legacy_alias, FieldUpdate::Value(false));

        let Operation::SettingsUpdate {
            frame_limiter_enable: reset,
            frame_limiter_fps_limit: null_limit,
            ..
        } = serde_json::from_value(json!({
            "action":"settings_update","hostname":"Host","port":48000,"expected_revision":2,
            "frame_limiter_enable":null,"frame_limiter_fps_limit":null
        }))
        .unwrap()
        else {
            panic!("settings update expected");
        };
        assert_eq!(reset, FieldUpdate::Null);
        assert_eq!(null_limit, FieldUpdate::Null);
    }

    #[test]
    fn audio_sink_capture_only_update_keeps_omitted_null_and_explicit_false_distinct() {
        let omitted: Operation = serde_json::from_value(json!({
            "action":"settings_update","hostname":"Host","port":48000,
            "expected_revision":2
        }))
        .unwrap();
        let Operation::SettingsUpdate {
            audio_sink_capture_only,
            ..
        } = omitted
        else {
            panic!("settings update expected");
        };
        assert_eq!(audio_sink_capture_only, FieldUpdate::Omitted);

        let reset: Operation = serde_json::from_value(json!({
            "action":"settings_update","hostname":"Host","port":48000,
            "expected_revision":2,"audio_sink_capture_only":null
        }))
        .unwrap();
        let Operation::SettingsUpdate {
            audio_sink_capture_only,
            ..
        } = reset
        else {
            panic!("settings update expected");
        };
        assert_eq!(audio_sink_capture_only, FieldUpdate::Null);

        let explicit_false: Operation = serde_json::from_value(json!({
            "action":"settings_update","hostname":"Host","port":48000,
            "expected_revision":2,"audio_sink_capture_only":false
        }))
        .unwrap();
        let Operation::SettingsUpdate {
            audio_sink_capture_only,
            ..
        } = explicit_false
        else {
            panic!("settings update expected");
        };
        assert_eq!(audio_sink_capture_only, FieldUpdate::Value(false));
    }

    #[test]
    fn virtual_sink_update_keeps_omitted_null_and_explicit_empty_distinct() {
        let deserialize = |value: Option<serde_json::Value>| {
            let mut request = json!({
                "action":"settings_update","hostname":"Host","port":48000,
                "expected_revision":2
            });
            if let Some(value) = value {
                request["virtual_sink"] = value;
            }
            let Operation::SettingsUpdate { virtual_sink, .. } =
                serde_json::from_value(request).unwrap()
            else {
                panic!("settings update expected");
            };
            virtual_sink
        };

        assert_eq!(deserialize(None), FieldUpdate::Omitted);
        assert_eq!(
            deserialize(Some(serde_json::Value::Null)),
            FieldUpdate::Null
        );
        assert_eq!(
            deserialize(Some(json!(""))),
            FieldUpdate::Value(String::new())
        );
        assert_eq!(
            deserialize(Some(json!("Steam Streaming Speakers"))),
            FieldUpdate::Value("Steam Streaming Speakers".into())
        );
    }

    #[test]
    fn global_preparation_update_distinguishes_omission_null_and_list() {
        let deserialize = |value: Option<serde_json::Value>| {
            let mut request = json!({
                "action":"settings_update","hostname":"Host","port":48000,
                "expected_revision":2
            });
            if let Some(value) = value {
                request["global_preparation"] = value;
            }
            let Operation::SettingsUpdate {
                global_preparation, ..
            } = serde_json::from_value(request).unwrap()
            else {
                panic!("settings update expected");
            };
            global_preparation
        };

        assert_eq!(deserialize(None), FieldUpdate::Omitted);
        assert_eq!(
            deserialize(Some(serde_json::Value::Null)),
            FieldUpdate::Null
        );
        let list = json!([{"run":{"executable":"C:\\games\\prepare.exe"}}]);
        match deserialize(Some(list)) {
            FieldUpdate::Value(steps) => {
                assert_eq!(steps.len(), 1);
                assert!(steps[0].run.executable.ends_with("prepare.exe"));
            }
            _ => panic!("preparation list expected"),
        }
    }

    #[test]
    fn listener_target_derives_the_https_port_and_applies_bind_policy() {
        let startup = startup_listener_settings();
        let configured_target = target(48_000);

        let listener_settings = listener_settings_for_target(&startup, &configured_target).unwrap();

        assert_eq!(listener_settings.http_bind.port(), configured_target.port);
        assert_eq!(
            listener_settings.https_bind.port(),
            settings::https_port(configured_target.port)
        );
        assert_eq!(
            listener_settings.address_family,
            configured_target.address_family
        );
        assert_eq!(
            listener_settings.bind_address,
            configured_target.bind_address
        );
    }

    #[test]
    fn invalid_and_occupied_targets_keep_the_old_pair_until_retry_succeeds() {
        let startup = startup_listener_settings();
        let mut server = server::Server::bind(&startup).unwrap();
        let old_http_addresses = server.http_addresses();
        let old_https_addresses = server.https_addresses();

        assert!(apply_listener_target(&mut server, &startup, &target(0)).is_err());
        assert_eq!(server.http_addresses(), old_http_addresses);
        assert_eq!(server.https_addresses(), old_https_addresses);

        let (occupied_http_port, http_port) = occupied_valid_http_port();
        let configured_target = target(http_port);
        assert!(apply_listener_target(&mut server, &startup, &configured_target).is_err());
        assert_eq!(server.http_addresses(), old_http_addresses);
        assert_eq!(server.https_addresses(), old_https_addresses);

        drop(occupied_http_port);
        apply_listener_target(&mut server, &startup, &configured_target).unwrap();

        assert!(listener_target_matches_server(
            &server,
            &startup,
            &configured_target
        ));
        assert_eq!(server.http_addresses()[0].port(), http_port);
        assert_eq!(
            server.https_addresses()[0].port(),
            settings::https_port(http_port)
        );
    }
}
