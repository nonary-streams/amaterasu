//! Bounded HTTP/TLS transport for Moonlight-compatible control and pairing.
mod launch;
mod network_settings;
mod product_settings;
mod server;
mod settings;
mod tls;
mod wire;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use launch::CaptureMode;
use network_settings::NetworkSettings;
use product_settings::{AdminAction, FieldUpdate, ProductSettings};
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
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
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
            if self
                .server
                .as_ref()
                .is_some_and(|server| server.http_address().port() != settings.port)
            {
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
            router.set_global_stream_audio(self.product_settings.stream_audio());
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
            let mut settings = self.startup_settings.clone();
            settings.http_bind.set_port(target.port);
            settings
                .https_bind
                .set_port(settings::https_port(target.port));
            settings.address_family = target.address_family;
            settings.bind_address = target.bind_address.clone();
            let result = settings.validate().map_err(str::to_owned).and_then(|()| {
                self.server
                    .as_mut()
                    .expect("a live listener is required to apply network settings")
                    .rebind_settings(&settings)
            });
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
                                external_ip,
                                stream_audio,
                                capture,
                                video_max_batch_size_kb,
                                max_bitrate,
                                minimum_fps_target,
                                wgc_pacing_smoothing,
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
                                    external_ip,
                                    stream_audio,
                                    capture,
                                    video_max_batch_size_kb,
                                    max_bitrate,
                                    minimum_fps_target,
                                    wgc_pacing_smoothing,
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
            ..
        } = serde_json::from_value(json!({
            "action":"settings_update","hostname":"Host","port":48000,"expected_revision":2,
            "address_family":null,"bind_address":"","output_name":null,"audio_sink":null,"stream_audio":null,"capture":null,
            "video_max_batch_size_kb":null,"max_bitrate":null,"minimum_fps_target":null,"wgc_pacing_smoothing":null
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
            ..
        } = serde_json::from_value(json!({
            "action":"settings_update","hostname":"Host","port":48000,"expected_revision":2,
            "address_family":"both","bind_address":"::1","output_name":"stable-display-id","audio_sink":"render.selected","stream_audio":false,"capture":"wgcc",
            "video_max_batch_size_kb":32,"max_bitrate":-15000,"minimum_fps_target":23.5,"wgc_pacing_smoothing":false
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

        assert!(
            serde_json::from_value::<Operation>(json!({
                "action":"settings_update","hostname":"Host","port":48000,
                "expected_revision":2,"capture":"fallback"
            }))
            .is_err()
        );
    }
}
