//! Development loopback HTTP management lives in this independently loaded module.
mod assets;
#[cfg(target_os = "linux")]
mod candidate_cache;
mod capture_management;
mod csrf_policy;
mod extensions;
mod health_lifecycle;
#[cfg(target_os = "linux")]
mod host_activation;
mod http;
mod package_status;
mod package_update;
mod peer_policy;
mod product_settings;
mod repository_admin;
mod repository_transport;
mod route_payload;
mod routes;
#[cfg(target_os = "linux")]
mod rtx_hdr_candidate;
mod rtx_hdr_install;
mod server;
mod tls;
mod tray_events;

#[cfg(test)]
mod csrf_adversarial_tests;

use amaterasu_module_sdk::network::Network;
use amaterasu_module_sdk::{
    Command, LogLevel, ManagedModule, ModuleResult, Response, RuntimeClient,
};
use amaterasu_package::repository::RepositoryStore;
use peer_policy::PeerTier;
use serde::Deserialize;
use serde_json::json;
use server::Server;
use std::net::SocketAddr;
use std::time::Instant;

const SERVICE: &str = "amaterasu.management/1";
const AUTH_SERVICE: &str = "amaterasu.authentication/1";

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    enabled: bool,
    network_provider: Option<String>,
    audio_capture_provider: Option<String>,
    audio_encoder_provider: Option<String>,
    bind: Option<SocketAddr>,
    tls_bind: Option<SocketAddr>,
    tls_certificate_path: Option<String>,
    tls_private_key_path: Option<String>,
    #[serde(default)]
    allowed_origins: Vec<String>,
    web: Option<assets::WebSettings>,
    extensions: Option<extensions::Settings>,
    rtx_hdr_runtime_parent: Option<std::path::PathBuf>,
}

struct ManagementModule {
    runtime: RuntimeClient,
    server: Option<Server>,
    repositories: Option<RepositoryStore>,
    repository_administration: Option<repository_admin::RepositoryAdministration>,
    rtx_hdr_installer: Option<rtx_hdr_install::Installer>,
    package_update: package_update::PackageUpdate,
    operational_health: Option<health_lifecycle::OperationalHealth>,
    product_settings: product_settings::ProductSettings,
    tray_events: tray_events::TrayEventConsumer,
    shutdown_announced: bool,
    shutdown_poll_drained: bool,
    quiesce_started: bool,
    quiescing: bool,
}

impl ManagedModule for ManagementModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("management settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
        validate_settings(&settings)?;
        let repositories = settings
            .web
            .as_ref()
            .map(|web| RepositoryStore::new(web.repository_directory()?))
            .transpose()?;
        let tls_configuration = match (
            &settings.tls_certificate_path,
            &settings.tls_private_key_path,
        ) {
            (Some(certificate), Some(private_key)) if settings.enabled => {
                Some(tls::configuration(certificate, private_key)?)
            }
            _ => None,
        };
        let extensions = settings
            .extensions
            .map(extensions::Extensions::new)
            .transpose()?;
        let network = if settings.enabled {
            Some(Network::from_settings(&runtime, &payload)?)
        } else {
            None
        };
        let repository_administration = match (&settings.web, &network) {
            (Some(web), Some(network)) => Some(repository_admin::RepositoryAdministration::new(
                web.repository_directory()?,
                web.console_trust(),
                network.clone(),
            )),
            _ => None,
        };
        let rtx_hdr_installer = match (&settings.rtx_hdr_runtime_parent, &network) {
            (Some(parent), Some(network)) => {
                let mut installer =
                    rtx_hdr_install::Installer::new(parent.clone(), network.clone())?;
                let repository_directory = settings
                    .web
                    .as_ref()
                    .and_then(|web| web.repository_directory().ok());
                installer.configure_package(repository_directory, package_root(&runtime));
                Some(installer)
            }
            _ => None,
        };
        let package_update = package_update::PackageUpdate::start(
            &runtime,
            settings
                .web
                .as_ref()
                .and_then(|web| web.repository_directory().ok()),
            network.as_ref(),
        );
        let server = if settings.enabled {
            let network = network.ok_or("management network unavailable")?;
            let mut server = Server::bind_with_network(
                network,
                settings
                    .bind
                    .ok_or("enabled management requires an explicit loopback bind")?,
                settings.allowed_origins,
                settings.web.map(assets::Assets::new).transpose()?,
                settings.tls_bind,
                tls_configuration,
            )
            .inspect_err(|error| {
                let _ = runtime.log(LogLevel::Error, "management.listener", error);
            })?;
            let processing = runtime
                .processing_composition()?
                .map(|composition| -> ModuleResult<_> {
                    use amaterasu_module_sdk::processing_composition as sdk;
                    let video = composition
                        .frame_consumer
                        .as_ref()
                        .ok_or("management frame consumer not selected")?;
                    let owner = composition
                        .stream_owner
                        .as_ref()
                        .ok_or("management stream owner not selected")?;
                    Ok(server::ProcessingRoutes {
                        video: sdk::require_contract(
                            video,
                            amaterasu_module_sdk::video::CONTRACT,
                            1,
                            0,
                        )?
                        .to_owned(),
                        stream: sdk::require_contract(owner, sdk::STREAM_OWNER_CONTRACT, 1, 0)?
                            .to_owned(),
                        lifecycle: composition
                            .lifecycle
                            .as_ref()
                            .map(|binding| {
                                sdk::require_contract(
                                    binding,
                                    amaterasu_module_sdk::extension_api::SESSION_LIFECYCLE_SERVICE,
                                    1,
                                    0,
                                )
                                .map(str::to_owned)
                            })
                            .transpose()?,
                        registry: composition
                            .settings_registry
                            .as_ref()
                            .map(|binding| {
                                sdk::require_contract(
                                    binding,
                                    amaterasu_module_sdk::extension_api::EXTENSION_REGISTRY_SERVICE,
                                    1,
                                    0,
                                )
                                .map(str::to_owned)
                            })
                            .transpose()?,
                    })
                })
                .transpose()?;
            server.set_audio_providers(server::AudioProviders {
                capture: settings
                    .audio_capture_provider
                    .unwrap_or_else(|| "amaterasu.audio/1".into()),
                encoder: settings
                    .audio_encoder_provider
                    .unwrap_or_else(|| "amaterasu.audio-encode/1".into()),
                processing,
            });
            server.set_extensions(extensions);
            Some(server)
        } else {
            None
        };
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            server,
            repositories,
            repository_administration,
            rtx_hdr_installer,
            package_update,
            operational_health: None,
            product_settings: product_settings::ProductSettings::new()?,
            tray_events: tray_events::TrayEventConsumer::new()?,
            shutdown_announced: false,
            shutdown_poll_drained: false,
            quiesce_started: false,
            quiescing: false,
        })
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        self.shutdown_announced = true;
        if let Some(installer) = &mut self.rtx_hdr_installer {
            installer.prepare_shutdown();
        }
        self.package_update.prepare_shutdown();
        if let Some(administration) = &mut self.repository_administration {
            administration.prepare_shutdown();
        }
        if let Some(server) = &mut self.server {
            server.prepare_shutdown();
        }
        self.product_settings.prepare_shutdown();
        self.tray_events.prepare_shutdown();
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        if !self.shutdown_announced {
            self.prepare_shutdown(0)?;
        }
        if !self.quiesce_started {
            self.quiesce_started = true;
            // Consumers could submit after an earlier idle poll during the
            // announcement phase. Observe the queue again after they drain.
            self.shutdown_poll_drained = false;
        }
        // Already accepted server calls can target this module itself. Drain
        // the broker queue before stopping ticks, or they cannot complete.
        if !self.shutdown_poll_drained {
            self.tick()?;
            if !self.shutdown_poll_drained {
                return Ok(false);
            }
        }
        self.quiescing = true;
        let installer_ready = self
            .rtx_hdr_installer
            .as_mut()
            .is_none_or(rtx_hdr_install::Installer::quiesce);
        let package_update_ready = self.package_update.quiesce();
        let repositories_ready = self
            .repository_administration
            .as_mut()
            .is_none_or(repository_admin::RepositoryAdministration::quiesce);
        let server_ready = self
            .server
            .as_mut()
            .is_none_or(|server| server.quiesce(&self.runtime));
        let settings_ready = self
            .product_settings
            .quiesce(Instant::now(), &mut |command| self.runtime.send(command));
        self.follow_automatic_package_updates()?;
        let tray_ready = self.tray_events.quiesce(&self.runtime);
        let health_ready = self.operational_health.as_mut().is_none_or(|health| {
            health.quiesce(Instant::now(), &mut |command| self.runtime.send(command))
        });
        if health_ready {
            self.operational_health = None;
        }
        Ok(installer_ready
            && package_update_ready
            && repositories_ready
            && server_ready
            && settings_ready
            && tray_ready
            && health_ready)
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if self.quiescing {
            return Ok(());
        }
        if let Some(installer) = &mut self.rtx_hdr_installer {
            installer.advance();
        }
        let other_install_active = self.rtx_hdr_install_active();
        self.package_update
            .advance(&self.runtime, other_install_active);
        self.package_update.observe_host(&self.runtime);
        if let Some(administration) = &mut self.repository_administration {
            administration.advance();
        }
        self.product_settings
            .tick(Instant::now(), &mut |command| self.runtime.send(command))?;
        self.follow_automatic_package_updates()?;
        self.advance_operational_health()?;
        if let Some(server) = &mut self.server {
            server.set_peer_policy(self.product_settings.maximum_peer());
            server.set_csrf_policy(self.product_settings.csrf_origins());
            server.tick(&self.runtime)?;
        }
        self.tray_events.tick(&self.runtime, self.server.as_ref())?;
        let polled = self.runtime.send(Command::Poll {})?;
        if self.shutdown_announced {
            self.shutdown_poll_drained = matches!(polled, Response::Pending);
        }
        match polled {
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
                    return complete(
                        &self.runtime,
                        id,
                        json!({"ok":false,"code":"invalid_request"}),
                    );
                }
                if payload == json!({"action":"operational_health"}) {
                    return self.begin_operational_health(id, remaining_ms);
                }
                if payload == json!({"action":"upnp_exposure"}) {
                    let payload = upnp_exposure_payload(
                        &caller,
                        self.product_settings.maximum_peer(),
                        self.server.as_ref().and_then(Server::tls_address),
                    );
                    return complete(&self.runtime, id, payload);
                }
                if payload
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|action| matches!(action, "product_get" | "product_update"))
                {
                    return self.product_settings.receive(
                        &caller,
                        id,
                        remaining_ms,
                        payload,
                        &mut |command| self.runtime.send(command),
                    );
                }
                if payload == json!({"action":"rtx_hdr_activate"}) {
                    if caller != "amaterasu.management" {
                        return complete(
                            &self.runtime,
                            id,
                            json!({"ok":false,"code":"permission_denied"}),
                        );
                    }
                    #[cfg(target_os = "linux")]
                    let result = if self.package_update.in_progress() {
                        // One install at a time owns the repository cache and
                        // the host's activation request.
                        json!({"ok":false,"code":"busy"})
                    } else if let Some(installer) = &mut self.rtx_hdr_installer {
                        match installer.activation_request() {
                            Ok(request) => {
                                match self.runtime.send(Command::HostPackageActivate { request }) {
                                    Ok(Response::Ok) => {
                                        installer.mark_activation_requested();
                                        json!({"ok":true})
                                    }
                                    _ => json!({"ok":false,"code":"activation_unavailable"}),
                                }
                            }
                            Err(code) => json!({"ok":false,"code":code}),
                        }
                    } else {
                        json!({"ok":false,"code":"activation_unavailable"})
                    };
                    #[cfg(not(target_os = "linux"))]
                    let result = json!({"ok":false,"code":"activation_unavailable"});
                    return complete(&self.runtime, id, result);
                }
                if let Some(action @ ("rtx_hdr_status" | "rtx_hdr_install" | "rtx_hdr_cancel")) =
                    payload.get("action").and_then(serde_json::Value::as_str)
                {
                    let result = if caller != "amaterasu.management"
                        || payload != json!({"action": action})
                    {
                        json!({"ok":false,"code":"permission_denied"})
                    } else if action == "rtx_hdr_install" && self.package_update.in_progress() {
                        json!({"ok":false,"code":"busy"})
                    } else if let Some(installer) = &mut self.rtx_hdr_installer {
                        #[cfg(target_os = "linux")]
                        let cancel_waiting =
                            action == "rtx_hdr_cancel" && installer.waiting_for_stream();
                        #[cfg(not(target_os = "linux"))]
                        let cancel_waiting = false;
                        let mut result = if cancel_waiting {
                            match self.runtime.send(Command::HostPackageActivationCancel {}) {
                                Ok(Response::Ok) => {
                                    installer.cancel_pending_activation();
                                    installer.request("rtx_hdr_status")
                                }
                                _ => json!({"ok":false,"code":"activation_in_progress"}),
                            }
                        } else {
                            installer.request(action)
                        };
                        #[cfg(target_os = "linux")]
                        if action == "rtx_hdr_status" {
                            if let Ok(Response::HostPackageActivationState { pending, started }) =
                                self.runtime.send(Command::HostPackageActivationStatus {})
                            {
                                installer.update_activation_state(pending, started);
                                result = installer.request("rtx_hdr_status");
                            }
                            let running =
                                match self.runtime.send(Command::GetRunningPackageDigest {}) {
                                    Ok(Response::Result { payload }) => payload
                                        .get("digest")
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_owned),
                                    _ => None,
                                };
                            installer.reconcile_activation(&mut result, running.as_deref());
                        }
                        result
                    } else {
                        json!({"ok":false,"code":"rtx_hdr_install_unavailable"})
                    };
                    return complete(&self.runtime, id, result);
                }
                let action = payload.get("action").and_then(serde_json::Value::as_str);
                if action.is_some_and(package_update::VersionRequest::names) {
                    let result = self.package_version_request(&caller, &payload);
                    return complete(&self.runtime, id, result);
                }
                if action == Some(package_update::AUTOMATIC_ACTION) {
                    return self.select_automatic_package_updates(
                        id,
                        &caller,
                        remaining_ms,
                        &payload,
                    );
                }
                if let Some(update) = action.and_then(package_update::Action::parse) {
                    let result = self.package_update_request(&caller, update, &payload);
                    return complete(&self.runtime, id, result);
                }
                let payload = if caller != "amaterasu.management" {
                    if payload == json!({"action":"status"}) {
                        self.server_status()
                    } else {
                        json!({"ok":false,"code":"invalid_request"})
                    }
                } else if action == Some("repository_list") {
                    self.repository_list(&payload)
                } else if let Some(
                    change @ ("repository_preview" | "repository_add" | "repository_refresh"
                    | "repository_remove"),
                ) = action
                {
                    self.change_repository(change, &payload)
                } else if action == Some("module_describe") {
                    self.describe_module(&payload)
                } else if payload == json!({"action":"package_selection_status"}) {
                    self.package_selection_status()
                } else if payload == json!({"action":"performance_trace"}) {
                    performance_trace(&self.runtime)
                } else if payload == json!({"action":"status"}) {
                    self.server_status()
                } else {
                    json!({"ok":false,"code":"invalid_request"})
                };
                complete(&self.runtime, id, payload)
            }
            _ => Err("management status polling failed".into()),
        }
    }
}

fn upnp_exposure_payload(
    caller: &str,
    maximum_peer: Option<PeerTier>,
    tls_address: Option<SocketAddr>,
) -> serde_json::Value {
    if caller != "amaterasu.nvhttp" {
        return json!({"ok":false,"code":"permission_denied"});
    }
    let wan_enabled = maximum_peer == Some(PeerTier::Wan);
    json!({
        "ok":true,
        "wan_enabled":wan_enabled,
        "tls_address":if wan_enabled { tls_address.map(|address| address.to_string()) } else { None::<String> },
    })
}

/// Recent flow-trace history for the diagnostics page. A runtime without
/// tracing is reported as unsupported rather than as an empty, healthy host.
fn performance_trace(runtime: &RuntimeClient) -> serde_json::Value {
    match runtime.send(Command::TraceSnapshot {}) {
        Ok(Response::Result { payload }) => json!({"ok":true,"supported":true,"trace":payload}),
        Ok(Response::Error { code, .. }) if code == "invalid_request" => {
            json!({"ok":true,"supported":false,"trace":null})
        }
        _ => json!({"ok":false,"code":"performance_trace_unavailable"}),
    }
}

/// The host root that holds the package selection, when the host shares it.
fn package_root(runtime: &RuntimeClient) -> Option<std::path::PathBuf> {
    match runtime.send(Command::GetPackageRoot {}) {
        Ok(Response::Result { payload }) => payload
            .get("package_root")
            .and_then(serde_json::Value::as_str)
            .map(std::path::PathBuf::from),
        _ => None,
    }
}

impl ManagementModule {
    fn server_status(&self) -> serde_json::Value {
        json!({
            "ok": true,
            "enabled": self.server.is_some(),
            "bound_address": self.server.as_ref().map(Server::address),
            "tls_address": self.server.as_ref().and_then(Server::tls_address),
        })
    }

    fn repository_list(&self, payload: &serde_json::Value) -> serde_json::Value {
        match &self.repositories {
            Some(store) => repository_admin::listing(
                store,
                self.repository_administration.as_ref(),
                payload,
                amaterasu_package::unix_timestamp(),
            ),
            None => json!({"ok":false,"code":"repository_state_unavailable"}),
        }
    }

    fn change_repository(
        &mut self,
        action: &str,
        payload: &serde_json::Value,
    ) -> serde_json::Value {
        let runtime = &self.runtime;
        match &mut self.repository_administration {
            Some(administration) => {
                administration.begin_change(action, payload, || package_root(runtime))
            }
            None => json!({"ok":false,"code":"repository_management_unavailable"}),
        }
    }

    /// Answers a package-update route. Only this module's own console routes
    /// reach it, and a request carries nothing but its action.
    fn package_update_request(
        &mut self,
        caller: &str,
        action: package_update::Action,
        payload: &serde_json::Value,
    ) -> serde_json::Value {
        if caller != "amaterasu.management" || *payload != json!({"action": action.name()}) {
            return json!({"ok":false,"code":"permission_denied"});
        }
        let rtx_hdr_install_active = self.rtx_hdr_install_active();
        self.package_update
            .request(action, &self.runtime, rtx_hdr_install_active)
    }

    /// Answers a request about the retained sets. Only this module's own
    /// console routes reach it; a listing carries nothing and a restore
    /// carries one digest.
    fn package_version_request(
        &mut self,
        caller: &str,
        payload: &serde_json::Value,
    ) -> serde_json::Value {
        let request = package_update::VersionRequest::parse(payload)
            .filter(|_| caller == "amaterasu.management");
        let Some(request) = request else {
            return json!({"ok":false,"code":"permission_denied"});
        };
        let rtx_hdr_install_active = self.rtx_hdr_install_active();
        // A restore to a set from before automatic updates depends on what
        // the settings document says now, or is about to say.
        let settings_carry_automatic = self.product_settings.carries_automatic_package_updates();
        self.package_update.versions(
            request,
            &self.runtime,
            rtx_hdr_install_active,
            settings_carry_automatic,
        )
    }

    /// True while the RTX HDR install prepares or activates a set of its
    /// own, which a package update must not run beside.
    fn rtx_hdr_install_active(&self) -> bool {
        self.rtx_hdr_installer
            .as_ref()
            .is_some_and(rtx_hdr_install::Installer::package_work_in_progress)
    }

    /// Saves the administrator's choice of automatic package updates. Only
    /// this module's own console route reaches it. The reply is the
    /// package-update status, sent once the choice is durable.
    fn select_automatic_package_updates(
        &mut self,
        request: u64,
        caller: &str,
        remaining_ms: u64,
        payload: &serde_json::Value,
    ) -> ModuleResult<()> {
        let refusal = if caller != "amaterasu.management" {
            Some("permission_denied")
        } else if !self.package_update.supported() {
            Some("unsupported_platform")
        } else {
            None
        };
        if let Some(code) = refusal {
            return complete(&self.runtime, request, json!({"ok":false,"code":code}));
        }
        let Some(mode) = package_update::AutomaticMode::requested(payload) else {
            return complete(
                &self.runtime,
                request,
                json!({"ok":false,"code":"invalid_mode"}),
            );
        };
        // The set the host is returning to could not read the settings this
        // choice would save, and would then admit nobody to the console.
        if !mode.is_off()
            && self
                .package_update
                .restoring_set_from_before_automatic_updates()
        {
            return complete(
                &self.runtime,
                request,
                json!({"ok":false,"code":"restore_in_progress"}),
            );
        }
        self.product_settings.select_automatic_package_updates(
            request,
            remaining_ms,
            mode,
            &mut |command| self.runtime.send(command),
        )?;
        self.follow_automatic_package_updates()
    }

    /// Hands the saved choice of automatic package updates to the update
    /// service, and answers the request that chose it once it is durable.
    fn follow_automatic_package_updates(&mut self) -> ModuleResult<()> {
        if let Some(mode) = self.product_settings.automatic_package_updates() {
            self.package_update.select_automatic(mode);
        }
        let Some(request) = self.product_settings.saved_automatic_updates() else {
            return Ok(());
        };
        let status = self.package_update.request(
            package_update::Action::Status,
            &self.runtime,
            self.rtx_hdr_install_active(),
        );
        complete(&self.runtime, request, status)?;
        self.product_settings.automatic_updates_answered();
        Ok(())
    }

    fn package_selection_status(&self) -> serde_json::Value {
        let Some(root) = package_root(&self.runtime) else {
            return package_status::unavailable();
        };
        let Some(root) = root.to_str() else {
            return package_status::unavailable();
        };
        let mut status = package_status::selection_status(
            root,
            package_status::running_digest(&self.runtime).as_deref(),
        );
        if let Ok(Response::Result { payload }) =
            self.runtime.send(Command::GetRunningPackageDigest {})
            && payload["modules"]
                .as_array()
                .is_some_and(|modules| !modules.is_empty())
        {
            status["running"]["modules"] = payload["modules"].clone();
        }
        status
    }

    fn describe_module(&self, payload: &serde_json::Value) -> serde_json::Value {
        let id = payload.get("id").and_then(serde_json::Value::as_str);
        let (Some(id), Some(root)) = (id, package_root(&self.runtime)) else {
            return package_status::unavailable();
        };
        if let Ok(Response::Result { payload }) =
            self.runtime.send(Command::GetRunningPackageDigest {})
            && let Some(description) =
                package_status::loaded_module_description(&payload["components"], id)
        {
            return description;
        }
        let Some(root) = root.to_str() else {
            return package_status::unavailable();
        };
        package_status::module_description(
            root,
            package_status::running_digest(&self.runtime).as_deref(),
            id,
        )
    }

    fn begin_operational_health(&mut self, request: u64, remaining_ms: u64) -> ModuleResult<()> {
        if self.server.is_none() {
            return complete(
                &self.runtime,
                request,
                json!({"ok":false,"code":"management_disabled"}),
            );
        }
        if self.operational_health.is_some() {
            return complete(
                &self.runtime,
                request,
                json!({"ok":false,"code":"health_busy"}),
            );
        }
        self.operational_health = Some(health_lifecycle::OperationalHealth::new(
            request,
            remaining_ms,
            Instant::now(),
        ));
        Ok(())
    }

    fn advance_operational_health(&mut self) -> ModuleResult<()> {
        if self.operational_health.as_mut().is_some_and(|health| {
            health.advance(Instant::now(), &mut |command| self.runtime.send(command))
        }) {
            self.operational_health = None;
        }
        Ok(())
    }
}

fn complete(runtime: &RuntimeClient, request: u64, payload: serde_json::Value) -> ModuleResult<()> {
    match runtime.send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        _ => Err("management completion failed".into()),
    }
}

fn validate_settings(settings: &Settings) -> ModuleResult<()> {
    if settings
        .network_provider
        .as_deref()
        .is_some_and(str::is_empty)
    {
        return Err("management network provider cannot be empty".into());
    }
    for service in [
        &settings.audio_capture_provider,
        &settings.audio_encoder_provider,
    ]
    .into_iter()
    .flatten()
    {
        if !amaterasu_module_sdk::network::valid_provider(service) {
            return Err("invalid management audio provider".into());
        }
    }
    if settings
        .bind
        .is_some_and(|address| !address.ip().is_loopback())
    {
        return Err("management bind must be explicitly loopback".into());
    }
    if settings.tls_bind.is_some() != settings.tls_certificate_path.is_some()
        || settings.tls_bind.is_some() != settings.tls_private_key_path.is_some()
    {
        return Err(
            "management TLS bind, certificate and private key must be configured together".into(),
        );
    }
    if settings.allowed_origins.len() > 16
        || settings
            .allowed_origins
            .iter()
            .any(|origin| !http::valid_origin(origin))
    {
        return Err("management allows at most 16 explicit HTTP(S) origins without paths".into());
    }
    Ok(())
}

amaterasu_module_sdk::export_module!(ManagementModule);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_drains_queued_management_calls_before_stopping_ticks() {
        use amaterasu_module_sdk::abi::{
            AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status,
        };
        use std::{ffi::c_void, sync::Mutex};

        let commands = Mutex::new(Vec::<Command>::new());
        unsafe extern "C" fn invoke(
            context: *mut c_void,
            input: BorrowedBytes,
            reply: ReplySink,
        ) -> Status {
            // SAFETY: the test retains the command ledger and synchronous borrows.
            let commands = unsafe { &*context.cast::<Mutex<Vec<Command>>>() };
            let command: Command =
                serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
            let mut commands = commands.lock().unwrap();
            let response = match &command {
                Command::GetSettings {} => Response::Result { payload: json!({}) },
                Command::Register { .. } | Command::Complete { .. } => Response::Ok,
                Command::Poll {} if !commands.iter().any(|c| matches!(c, Command::Poll {})) => {
                    Response::Request {
                        id: 17,
                        caller: "test".into(),
                        initiator: "test".into(),
                        service: SERVICE.into(),
                        payload: json!({"action":"operational_health"}),
                        remaining_ms: 1000,
                    }
                }
                Command::Poll {} => Response::Pending,
                _ => panic!("unexpected command: {command:?}"),
            };
            commands.push(command);
            let bytes = serde_json::to_vec(&response).unwrap();
            // SAFETY: the sink copies the bytes during this call.
            unsafe { reply.send(&bytes) };
            STATUS_OK
        }
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::from_ref(&commands).cast_mut().cast(),
            invoke,
        };
        // SAFETY: the ledger outlives the module and all calls.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = ManagementModule::start(runtime).unwrap();
        module.prepare_shutdown(1000).unwrap();
        assert!(!module.quiesce().unwrap());
        assert!(module.quiesce().unwrap());
        module.tick().unwrap();
        let commands = commands.lock().unwrap();
        assert_eq!(
            commands
                .iter()
                .filter(|c| matches!(c, Command::Poll {}))
                .count(),
            2
        );
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, Command::Complete { request: 17, .. }))
        );
    }

    /// A runtime that hands a disabled module queued requests, one for each
    /// poll, and records what the module answers.
    struct ScriptedRuntime {
        requests: std::sync::Mutex<std::collections::VecDeque<(&'static str, serde_json::Value)>>,
        replies: std::sync::Mutex<Vec<serde_json::Value>>,
    }

    impl ScriptedRuntime {
        fn new(requests: &[(&'static str, serde_json::Value)]) -> Self {
            Self {
                requests: std::sync::Mutex::new(requests.iter().cloned().collect()),
                replies: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// Starts a module on this runtime. The runtime must outlive it.
        fn start(&self) -> ManagementModule {
            use amaterasu_module_sdk::abi::{AbiHeader, HostApi};
            let endpoint = HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: std::ptr::from_ref(self).cast_mut().cast(),
                invoke: Self::invoke,
            };
            // SAFETY: each test keeps this runtime alive past its module.
            let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
            ManagementModule::start(runtime).unwrap()
        }

        /// Ticks until every queued request has been answered.
        fn answers(&self, module: &mut ManagementModule) -> Vec<serde_json::Value> {
            while !self.requests.lock().unwrap().is_empty() {
                module.tick().unwrap();
            }
            std::mem::take(&mut self.replies.lock().unwrap())
        }

        unsafe extern "C" fn invoke(
            context: *mut std::ffi::c_void,
            input: amaterasu_module_sdk::abi::BorrowedBytes,
            reply: amaterasu_module_sdk::abi::ReplySink,
        ) -> amaterasu_module_sdk::abi::Status {
            // SAFETY: the test retains the runtime and the synchronous borrows.
            let runtime = unsafe { &*context.cast::<Self>() };
            let command: Command =
                serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
            let response = match command {
                Command::GetSettings {} => Response::Result { payload: json!({}) },
                Command::Register { .. } => Response::Ok,
                Command::Complete { payload, .. } => {
                    runtime.replies.lock().unwrap().push(payload);
                    Response::Ok
                }
                Command::Poll {} => match runtime.requests.lock().unwrap().pop_front() {
                    Some((caller, payload)) => Response::Request {
                        id: 1,
                        caller: caller.into(),
                        initiator: caller.into(),
                        service: SERVICE.into(),
                        payload,
                        remaining_ms: 1000,
                    },
                    None => Response::Pending,
                },
                // The module's own settings load finds no configuration service.
                Command::Request { .. } => Response::error("unavailable", "not provided"),
                // A started update service asks whether the host takes restores.
                Command::HostPackageRestoreStatus {} => {
                    Response::error("restore_unavailable", "not provided")
                }
                command => panic!("unexpected command: {command:?}"),
            };
            let bytes = serde_json::to_vec(&response).unwrap();
            // SAFETY: the sink copies the bytes during this call.
            unsafe { reply.send(&bytes) };
            amaterasu_module_sdk::abi::STATUS_OK
        }
    }

    #[test]
    fn package_update_requests_come_only_from_the_console_routes() {
        let denied = json!({"ok":false,"code":"permission_denied"});
        let runtime = ScriptedRuntime::new(&[
            // Another module cannot ask, whatever it sends.
            (
                "amaterasu.nvhttp",
                json!({"action":"package_update_status"}),
            ),
            (
                "amaterasu.nvhttp",
                json!({"action":"package_update_activate"}),
            ),
            // A request carries its action and nothing that names a package.
            (
                "amaterasu.management",
                json!({"action":"package_update_activate","repository":"other"}),
            ),
            (
                "amaterasu.management",
                json!({"action":"package_update_download","digest":"0".repeat(64)}),
            ),
            (
                "amaterasu.management",
                json!({"action":"package_update_status"}),
            ),
            (
                "amaterasu.management",
                json!({"action":"package_update_check"}),
            ),
            (
                "amaterasu.management",
                json!({"action":"package_update_install"}),
            ),
        ]);
        let mut module = runtime.start();
        let replies = runtime.answers(&mut module);
        assert_eq!(
            replies[..4],
            [denied.clone(), denied.clone(), denied.clone(), denied]
        );
        // A module that serves no console has no network provider to check
        // with and answers as a host that cannot update.
        assert_eq!(replies[4]["ok"], true);
        assert_eq!(replies[4]["supported"], false);
        assert_eq!(replies[4]["error"], "unsupported_platform");
        assert_eq!(
            replies[5],
            json!({"ok":false,"code":"unsupported_platform"})
        );
        assert_eq!(replies[6], json!({"ok":false,"code":"invalid_request"}));
    }

    #[test]
    fn package_version_requests_come_only_from_the_console_routes() {
        let denied = json!({"ok":false,"code":"permission_denied"});
        let digest = "0".repeat(64);
        let runtime = ScriptedRuntime::new(&[
            // Another module cannot list the sets or ask for one.
            (
                "amaterasu.nvhttp",
                json!({"action":"package_update_versions"}),
            ),
            (
                "amaterasu.nvhttp",
                json!({"action":"package_update_restore","digest":digest}),
            ),
            // A request carries what its route allows and nothing more.
            (
                "amaterasu.management",
                json!({"action":"package_update_versions","limit":500}),
            ),
            (
                "amaterasu.management",
                json!({"action":"package_update_restore"}),
            ),
            (
                "amaterasu.management",
                json!({"action":"package_update_restore","digest":digest,"force":true}),
            ),
            (
                "amaterasu.management",
                json!({"action":"package_update_restore","digest":["previous"]}),
            ),
            (
                "amaterasu.management",
                json!({"action":"package_update_versions"}),
            ),
            (
                "amaterasu.management",
                json!({"action":"package_update_restore","digest":digest}),
            ),
        ]);
        let mut module = runtime.start();
        let replies = runtime.answers(&mut module);
        assert!(replies[..6].iter().all(|reply| *reply == denied));
        // A module that serves no console cannot ask a host to change sets.
        assert_eq!(
            replies[6],
            json!({"ok":true,"supported":false,"total":0,"versions":[]})
        );
        assert_eq!(
            replies[7],
            json!({"ok":false,"code":"unsupported_platform"})
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rtx_hdr_install_waits_for_a_package_update_to_finish() {
        let rtx_requests = [
            ("amaterasu.management", json!({"action":"rtx_hdr_install"})),
            ("amaterasu.management", json!({"action":"rtx_hdr_activate"})),
        ];
        // This host has no RTX HDR installer, which is what both requests
        // say when nothing stands in their way.
        let runtime = ScriptedRuntime::new(&rtx_requests);
        let mut module = runtime.start();
        assert_eq!(
            runtime.answers(&mut module),
            [
                json!({"ok":false,"code":"rtx_hdr_install_unavailable"}),
                json!({"ok":false,"code":"activation_unavailable"}),
            ]
        );

        // While the host holds a package update's activation request, a
        // second install is refused before it reaches the installer.
        let runtime = ScriptedRuntime::new(&rtx_requests);
        let mut module = runtime.start();
        module.package_update = package_update::PackageUpdate::awaiting_activation();
        let busy = json!({"ok":false,"code":"busy"});
        assert_eq!(runtime.answers(&mut module), [busy.clone(), busy]);
    }

    /// A runtime whose configuration service keeps the management product
    /// document, as the real one does. It outlives the modules started on
    /// it, so a restarted module reads what an earlier one saved.
    #[cfg(target_os = "linux")]
    #[derive(Default)]
    struct SettingsRuntime {
        state: std::sync::Mutex<SettingsRuntimeState>,
    }

    #[cfg(target_os = "linux")]
    #[derive(Default)]
    struct SettingsRuntimeState {
        /// The stored document: its version, revision and values.
        document: Option<serde_json::Value>,
        writes: usize,
        results: std::collections::BTreeMap<u64, serde_json::Value>,
        submitted: u64,
        requests: std::collections::VecDeque<(&'static str, serde_json::Value)>,
        replies: Vec<serde_json::Value>,
        /// The host shell takes restore requests.
        takes_restores: bool,
        /// Every set the module asked the host to return to.
        restores: Vec<String>,
        /// The host holds a restore it has not started.
        restore_held: bool,
    }

    #[cfg(target_os = "linux")]
    impl SettingsRuntimeState {
        /// What the configuration service answers a product operation with.
        fn configuration(&mut self, payload: &serde_json::Value) -> serde_json::Value {
            let revision = self
                .document
                .as_ref()
                .map_or(0, |document| document["revision"].as_u64().unwrap());
            if payload["action"] == "product_update" {
                if payload["expected_revision"] != revision {
                    return json!({"ok":false,"error":{"code":"revision_conflict","current_revision":revision}});
                }
                self.writes += 1;
                self.document = Some(json!({
                    "version": payload["version"],
                    "revision": revision + 1,
                    "values": payload["values"],
                }));
            }
            match &self.document {
                Some(document) => json!({
                    "ok": true,
                    "found": true,
                    "version": document["version"],
                    "revision": document["revision"],
                    "values": document["values"],
                }),
                None => json!({"ok":true,"found":false,"version":null,"revision":0,"values":{}}),
            }
        }
    }

    #[cfg(target_os = "linux")]
    impl SettingsRuntime {
        /// Starts a module that can update its running set and lets it load
        /// its settings. The runtime must outlive it.
        fn start(&self) -> ManagementModule {
            use amaterasu_module_sdk::abi::{AbiHeader, HostApi};
            let endpoint = HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: std::ptr::from_ref(self).cast_mut().cast(),
                invoke: Self::invoke,
            };
            // SAFETY: each test keeps this runtime alive past its module.
            let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
            let mut module = ManagementModule::start(runtime).unwrap();
            module.package_update = package_update::PackageUpdate::without_source();
            for _ in 0..2 {
                module.tick().unwrap();
            }
            module
        }

        /// Queues console requests and ticks until each has been answered.
        fn answers(
            &self,
            module: &mut ManagementModule,
            requests: &[serde_json::Value],
        ) -> Vec<serde_json::Value> {
            self.state.lock().unwrap().requests.extend(
                requests
                    .iter()
                    .map(|request| ("amaterasu.management", request.clone())),
            );
            for _ in 0..requests.len() * 4 {
                module.tick().unwrap();
            }
            let replies = std::mem::take(&mut self.state.lock().unwrap().replies);
            assert_eq!(replies.len(), requests.len(), "{replies:?}");
            replies
        }

        fn saved(&self) -> (usize, Option<serde_json::Value>) {
            let state = self.state.lock().unwrap();
            (state.writes, state.document.clone())
        }

        unsafe extern "C" fn invoke(
            context: *mut std::ffi::c_void,
            input: amaterasu_module_sdk::abi::BorrowedBytes,
            reply: amaterasu_module_sdk::abi::ReplySink,
        ) -> amaterasu_module_sdk::abi::Status {
            // SAFETY: the test retains the runtime and the synchronous borrows.
            let runtime = unsafe { &*context.cast::<Self>() };
            let command: Command =
                serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
            let mut state = runtime.state.lock().unwrap();
            let response = match command {
                Command::GetSettings {} => Response::Result { payload: json!({}) },
                Command::Register { .. }
                | Command::CheckRequest { .. }
                | Command::ReleaseResult { .. } => Response::Ok,
                Command::Complete { payload, .. } => {
                    state.replies.push(payload);
                    Response::Ok
                }
                Command::Poll {} => match state.requests.pop_front() {
                    Some((caller, payload)) => Response::Request {
                        id: 1,
                        caller: caller.into(),
                        initiator: caller.into(),
                        service: SERVICE.into(),
                        payload,
                        remaining_ms: 1000,
                    },
                    None => Response::Pending,
                },
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.configuration/1" => {
                    let result = state.configuration(&payload);
                    state.submitted += 1;
                    let id = 100 + state.submitted;
                    state.results.insert(id, result);
                    Response::Accepted { id }
                }
                Command::Request { .. } => Response::error("unavailable", "not provided"),
                Command::TakeResult { request } => Response::Result {
                    payload: state.results[&request].clone(),
                },
                // A started update service asks whether the host takes restores.
                Command::HostPackageRestoreStatus {} if state.takes_restores => Response::Ok,
                Command::HostPackageRestoreStatus {} => {
                    Response::error("restore_unavailable", "not provided")
                }
                Command::HostPackageRestore { request } if state.takes_restores => {
                    state.restores.push(request.manifest_digest);
                    state.restore_held = true;
                    Response::Ok
                }
                Command::HostPackageActivationStatus {} => Response::HostPackageActivationState {
                    pending: state.restore_held,
                    started: false,
                },
                Command::HostPackageActivationCancel {} => {
                    state.restore_held = false;
                    Response::Ok
                }
                command => panic!("unexpected command: {command:?}"),
            };
            drop(state);
            let bytes = serde_json::to_vec(&response).unwrap();
            // SAFETY: the sink copies the bytes during this call.
            unsafe { reply.send(&bytes) };
            amaterasu_module_sdk::abi::STATUS_OK
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_choice_of_automatic_updates_is_saved_with_the_settings_and_survives_a_restart() {
        let status = json!({"action":"package_update_status"});
        let choose = |mode: &str| json!({"action":"package_update_automatic","mode":mode});
        let runtime = SettingsRuntime::default();
        let mut module = runtime.start();
        assert_eq!(
            runtime.answers(&mut module, std::slice::from_ref(&status))[0]["automatic"],
            json!({"mode":"off","next_check_unix_ms":null})
        );

        // Anything but one known mode is refused and writes nothing.
        let invalid = json!({"ok":false,"code":"invalid_mode"});
        let refused = runtime.answers(
            &mut module,
            &[
                choose("always"),
                json!({"action":"package_update_automatic"}),
                json!({"action":"package_update_automatic","mode":"install","force":true}),
                json!({"action":"package_update_automatic","mode":["install"]}),
            ],
        );
        assert_eq!(refused, vec![invalid; 4]);
        assert_eq!(runtime.saved(), (0, None));

        // The reply to a saved choice is the package-update status.
        let chosen = runtime.answers(&mut module, &[choose("install")]);
        assert_eq!(chosen[0]["ok"], true);
        assert_eq!(chosen[0]["supported"], true);
        assert_eq!(chosen[0]["phase"], "idle");
        assert_eq!(chosen[0]["automatic"]["mode"], "install");
        let (writes, document) = runtime.saved();
        assert_eq!(writes, 1);
        assert_eq!(
            document.unwrap()["values"]["automatic_package_updates"],
            "install"
        );
        // Choosing it again changes nothing and is answered the same way.
        assert_eq!(
            runtime.answers(&mut module, &[choose("install")])[0]["automatic"]["mode"],
            "install"
        );
        assert_eq!(runtime.saved().0, 1);
        drop(module);

        // A module started later, as after a restart or a package update,
        // follows the saved choice.
        let mut restarted = runtime.start();
        assert_eq!(
            runtime.answers(&mut restarted, std::slice::from_ref(&status))[0]["automatic"]["mode"],
            "install"
        );
        let off = runtime.answers(&mut restarted, &[choose("off"), status]);
        assert_eq!(off[0]["automatic"]["mode"], "off");
        assert_eq!(off[1]["automatic"]["mode"], "off");
        let (writes, document) = runtime.saved();
        assert_eq!(writes, 2);
        assert_eq!(
            document.unwrap(),
            json!({"version":2,"revision":2,"values":{"origin_web_ui_allowed":null,"csrf_allowed_origins":null}})
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_set_from_before_automatic_updates_is_never_left_with_settings_it_cannot_read() {
        let versions = json!({"action":"package_update_versions"});
        let cancel = json!({"action":"package_update_cancel"});
        let restore = |digest: &str| json!({"action":"package_update_restore","digest":digest});
        let choose = |mode: &str| json!({"action":"package_update_automatic","mode":mode});
        let refused = |code: &str| json!({"ok":false,"code":code});
        let runtime = SettingsRuntime::default();
        runtime.state.lock().unwrap().takes_restores = true;
        let mut module = runtime.start();
        let (package_update, sets) = package_update::PackageUpdate::on_retaining_host();
        module.package_update = package_update;
        let asked = || runtime.state.lock().unwrap().restores.clone();

        // The module reads the retained sets on a worker thread.
        let deadline = Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let listing = runtime.answers(&mut module, std::slice::from_ref(&versions));
            if listing[0]["supported"] == true && listing[0].get("loading").is_none() {
                assert_eq!(listing[0]["total"], 3);
                break;
            }
            assert!(Instant::now() < deadline, "{listing:?}");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        // With automatic updates saved, the settings document is one the
        // earlier set's module cannot read. The host is not asked.
        let chosen = runtime.answers(&mut module, &[choose("install")]);
        assert_eq!(chosen[0]["automatic"]["mode"], "install");
        assert_eq!(
            runtime.answers(&mut module, &[restore(&sets.before)]),
            [refused("automatic_updates_on")]
        );
        assert!(asked().is_empty());
        // A set that ran here with them is returned to whatever the mode.
        let waiting = runtime.answers(&mut module, &[restore(&sets.since)]);
        assert_eq!(waiting[0]["phase"], "waiting_stream", "{waiting:?}");
        assert_eq!(waiting[0]["restoring"]["digest"], sets.since.as_str());
        assert_eq!(asked(), [sets.since.as_str()]);
        assert_eq!(
            runtime.answers(&mut module, std::slice::from_ref(&cancel))[0]["phase"],
            "idle"
        );

        // `download` is as much a newer document as `install`.
        assert_eq!(
            runtime.answers(&mut module, &[choose("download")])[0]["automatic"]["mode"],
            "download"
        );
        assert_eq!(
            runtime.answers(&mut module, &[restore(&sets.before)]),
            [refused("automatic_updates_on")]
        );

        // Off and saved, the document is the earlier format again and the
        // earlier set may be returned to.
        let off = runtime.answers(&mut module, &[choose("off")]);
        assert_eq!(off[0]["automatic"]["mode"], "off");
        assert_eq!(runtime.saved().1.unwrap()["version"], 2);
        let waiting = runtime.answers(&mut module, &[restore(&sets.before)]);
        assert_eq!(waiting[0]["phase"], "waiting_stream", "{waiting:?}");
        assert_eq!(asked(), [sets.since.as_str(), sets.before.as_str()]);

        // While the host holds that restore, automatic updates stay off:
        // saving a mode now would be found by the earlier set's module.
        let writes = runtime.saved().0;
        assert_eq!(
            runtime.answers(&mut module, &[choose("download"), choose("install")]),
            [
                refused("restore_in_progress"),
                refused("restore_in_progress")
            ]
        );
        assert_eq!(runtime.saved().0, writes);
        assert_eq!(runtime.saved().1.unwrap()["version"], 2);
        // Saving `off` again is no risk and is answered as ever.
        let still = runtime.answers(&mut module, &[choose("off")]);
        assert_eq!(still[0]["automatic"]["mode"], "off");
        assert_eq!(still[0]["restoring"]["digest"], sets.before.as_str());

        // Once the restore is withdrawn the choice is free again.
        assert_eq!(
            runtime.answers(&mut module, std::slice::from_ref(&cancel))[0]["phase"],
            "idle"
        );
        let chosen = runtime.answers(&mut module, &[choose("install")]);
        assert_eq!(chosen[0]["automatic"]["mode"], "install");
        assert_eq!(runtime.saved().1.unwrap()["version"], 3);
    }

    #[test]
    fn choosing_automatic_updates_comes_only_from_the_console_route_of_a_host_that_updates() {
        let choice = json!({"action":"package_update_automatic","mode":"install"});
        let runtime = ScriptedRuntime::new(&[
            ("amaterasu.nvhttp", choice.clone()),
            // A module that serves no console cannot update its set.
            ("amaterasu.management", choice),
            (
                "amaterasu.management",
                json!({"action":"package_update_automatic","mode":"always"}),
            ),
        ]);
        let mut module = runtime.start();
        assert_eq!(
            runtime.answers(&mut module),
            [
                json!({"ok":false,"code":"permission_denied"}),
                json!({"ok":false,"code":"unsupported_platform"}),
                json!({"ok":false,"code":"unsupported_platform"}),
            ]
        );
    }

    #[test]
    fn audio_provider_settings_are_versioned_and_validated_at_startup() {
        let settings: Settings = serde_json::from_value(json!({"audio_capture_provider":"vendor.capture/1", "audio_encoder_provider":"vendor.encoder/1"})).unwrap();
        assert!(validate_settings(&settings).is_ok());
        for bad in ["", "vendor.capture/2", "Vendor/1", "vendor service/1"] {
            let settings: Settings =
                serde_json::from_value(json!({"audio_capture_provider":bad})).unwrap();
            assert!(validate_settings(&settings).is_err());
        }
    }

    #[test]
    fn upnp_exposure_is_internal_and_only_publishes_wan_tls_listener() {
        let tls_address = "0.0.0.0:49999".parse().unwrap();
        assert_eq!(
            upnp_exposure_payload("another.module", Some(PeerTier::Wan), Some(tls_address)),
            json!({"ok":false,"code":"permission_denied"})
        );
        assert_eq!(
            upnp_exposure_payload("amaterasu.nvhttp", Some(PeerTier::Lan), Some(tls_address)),
            json!({"ok":true,"wan_enabled":false,"tls_address":null})
        );
        assert_eq!(
            upnp_exposure_payload("amaterasu.nvhttp", Some(PeerTier::Wan), Some(tls_address)),
            json!({"ok":true,"wan_enabled":true,"tls_address":"0.0.0.0:49999"})
        );
        assert_eq!(
            upnp_exposure_payload("amaterasu.nvhttp", Some(PeerTier::Wan), None),
            json!({"ok":true,"wan_enabled":true,"tls_address":null})
        );
    }

    #[test]
    fn configuration_defaults_disabled_and_rejects_nonloopback_or_wildcard_origins() {
        let settings: Settings = serde_json::from_value(json!({})).unwrap();
        assert!(!settings.enabled);
        validate_settings(&settings).unwrap();
        for settings in [
            json!({"bind":"0.0.0.0:1234"}),
            json!({"bind":"[::]:1234"}),
            json!({"tls_bind":"127.0.0.1:1234"}),
            json!({"tls_certificate_path":"C:/cert.pem","tls_private_key_path":"C:/key.pem"}),
            json!({"allowed_origins":["*"]}),
            json!({"allowed_origins":["https://host/path"]}),
        ] {
            assert!(validate_settings(&serde_json::from_value(settings).unwrap()).is_err());
        }
        // TLS bind is a trusted startup setting; peer access is checked independently.
        for bind in [
            "0.0.0.0:1234",
            "[::]:1234",
            "192.168.0.1:1234",
            "203.0.113.1:1234",
        ] {
            let settings = serde_json::from_value(json!({"tls_bind":bind,"tls_certificate_path":"/cert.pem","tls_private_key_path":"/key.pem"})).unwrap();
            validate_settings(&settings).unwrap();
        }
    }

    #[test]
    fn enabled_server_requires_the_selected_network_provider() {
        use amaterasu_module_sdk::abi::{
            AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INVALID_ARGUMENT, STATUS_OK,
            Status,
        };
        use std::ffi::c_void;
        use std::sync::Mutex;

        struct Host {
            settings: serde_json::Value,
            commands: Mutex<Vec<Command>>,
        }
        unsafe extern "C" fn invoke(
            context: *mut c_void,
            input: BorrowedBytes,
            reply: ReplySink,
        ) -> Status {
            // SAFETY: the test retains the host and all input/reply borrows for
            // each synchronous runtime call.
            let host = unsafe { &*context.cast::<Host>() };
            let Ok(bytes) = (unsafe { input.read() }) else {
                return STATUS_INVALID_ARGUMENT;
            };
            let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
                return STATUS_INVALID_ARGUMENT;
            };
            let response = match &command {
                Command::GetSettings {} => Response::Result {
                    payload: host.settings.clone(),
                },
                Command::NativeBindTyped { .. } | Command::NativeBind { .. } => Response::error(
                    "permission_denied",
                    "test provider is deliberately unavailable",
                ),
                Command::Register { .. } => Response::Ok,
                Command::Poll {} => Response::Pending,
                _ => return STATUS_INVALID_ARGUMENT,
            };
            host.commands.lock().unwrap().push(command);
            let bytes = serde_json::to_vec(&response).unwrap();
            // SAFETY: the sink copies the reply during this call.
            unsafe { reply.send(&bytes) };
            STATUS_OK
        }
        for enabled in [false, true] {
            let host = Host {
                settings: json!({
                    "enabled": enabled,
                    "bind": "127.0.0.1:0",
                    "network_provider": "alternative.network/1"
                }),
                commands: Mutex::new(Vec::new()),
            };
            let endpoint = HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: std::ptr::from_ref(&host).cast_mut().cast(),
                invoke,
            };
            // SAFETY: host outlives the client and any returned disabled module.
            let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
            let module = ManagementModule::start(runtime);
            assert_eq!(module.is_ok(), !enabled);
            if let Ok(mut module) = module {
                assert!(module.quiesce().unwrap());
                // Host ticks after a ready result must not poll and admit work.
                module.tick().unwrap();
            }
            let commands = host.commands.lock().unwrap();
            assert!(
                !commands
                    .iter()
                    .any(|command| matches!(command, Command::NativeBind { .. })),
                "network startup must bind the declared interface contract"
            );
            let selected: Vec<_> = commands
                .iter()
                .filter_map(|command| match command {
                    Command::NativeBindTyped {
                        service, contract, ..
                    } => Some((service.as_str(), contract.as_str())),
                    _ => None,
                })
                .collect();
            assert_eq!(
                selected,
                if enabled {
                    vec![("alternative.network/1", "amaterasu.network-interface/1")]
                } else {
                    Vec::new()
                }
            );
        }
    }
}
