//! Development loopback HTTP management lives in this independently loaded module.
mod assets;
mod capture_management;
mod csrf_policy;
mod extensions;
mod health_lifecycle;
mod http;
mod peer_policy;
mod product_settings;
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
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use amaterasu_package::repository::RepositoryStore;
use peer_policy::PeerTier;
use serde::Deserialize;
use serde_json::json;
use server::Server;
use std::net::SocketAddr;
use std::path::Path;
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
    rtx_hdr_installer: Option<rtx_hdr_install::Installer>,
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
        let rtx_hdr_installer = match (&settings.rtx_hdr_runtime_parent, &network) {
            (Some(parent), Some(network)) => {
                let mut installer =
                    rtx_hdr_install::Installer::new(parent.clone(), network.clone())?;
                let repository_directory = settings
                    .web
                    .as_ref()
                    .and_then(|web| web.repository_directory().ok());
                let package_root = match runtime.send(Command::GetPackageRoot {}) {
                    Ok(Response::Result { payload }) => payload
                        .get("package_root")
                        .and_then(serde_json::Value::as_str)
                        .map(std::path::PathBuf::from),
                    _ => None,
                };
                installer.configure_package(repository_directory, package_root);
                Some(installer)
            }
            _ => None,
        };
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
            )?;
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
            rtx_hdr_installer,
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
        let server_ready = self
            .server
            .as_mut()
            .is_none_or(|server| server.quiesce(&self.runtime));
        let settings_ready = self
            .product_settings
            .quiesce(Instant::now(), &mut |command| self.runtime.send(command));
        let tray_ready = self.tray_events.quiesce(&self.runtime);
        let health_ready = self.operational_health.as_mut().is_none_or(|health| {
            health.quiesce(Instant::now(), &mut |command| self.runtime.send(command))
        });
        if health_ready {
            self.operational_health = None;
        }
        Ok(installer_ready && server_ready && settings_ready && tray_ready && health_ready)
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if self.quiescing {
            return Ok(());
        }
        if let Some(installer) = &mut self.rtx_hdr_installer {
            installer.advance();
        }
        self.product_settings
            .tick(Instant::now(), &mut |command| self.runtime.send(command))?;
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
                    let result = if let Some(installer) = &mut self.rtx_hdr_installer {
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
                let payload = if payload.get("action").and_then(serde_json::Value::as_str)
                    == Some("repository_list")
                    && caller == "amaterasu.management"
                {
                    match &self.repositories {
                        Some(store) => match store.list(
                            &amaterasu_package::current_target(),
                            amaterasu_package::unix_timestamp(),
                        ) {
                            Ok(mut repositories) => {
                                let offset = payload
                                    .get("offset")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(0)
                                    as usize;
                                let mut seen = 0usize;
                                let mut included = 0usize;
                                for repository in &mut repositories {
                                    repository.candidates.retain(|_| {
                                        let keep = seen >= offset && included < 32;
                                        seen += 1;
                                        if keep {
                                            included += 1;
                                        }
                                        keep
                                    });
                                }
                                let next_offset = (seen > offset.saturating_add(included))
                                    .then_some(offset.saturating_add(included));
                                json!({"ok":true,"repositories":repositories,"next_offset":next_offset})
                            }
                            Err(_) => json!({"ok":false,"code":"repository_state_unavailable"}),
                        },
                        None => json!({"ok":false,"code":"repository_state_unavailable"}),
                    }
                } else if payload == json!({"action":"package_selection_status"})
                    && caller == "amaterasu.management"
                {
                    match self.runtime.send(Command::GetPackageRoot {}) {
                        Ok(Response::Result { payload }) => payload
                            .get("package_root")
                            .and_then(serde_json::Value::as_str)
                            .and_then(|root| {
                                package_selection_status_with_running(
                                    root,
                                    running_package_status(&self.runtime),
                                )
                            })
                            .unwrap_or_else(package_selection_unavailable),
                        _ => package_selection_unavailable(),
                    }
                } else if payload == json!({"action":"status"}) {
                    json!({"ok":true,"enabled":self.server.is_some(),"bound_address":self.server.as_ref().map(Server::address),"tls_address":self.server.as_ref().and_then(Server::tls_address)})
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

fn package_selection_status(package_root: &str) -> Option<serde_json::Value> {
    let root = Path::new(package_root);
    if !root.is_absolute() {
        return None;
    }
    let metadata = std::fs::symlink_metadata(root).ok()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return None;
    }
    let canonical = root.canonicalize().ok()?;
    if canonical != root {
        return None;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return None;
        }
    }

    let selection = amaterasu_package::repository::read_native_selection(root).ok()?;
    for digest in [
        Some(selection.revision.as_str()),
        selection.active.as_deref(),
        selection.previous.as_deref(),
        selection.pending.as_deref(),
        selection.trial.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return None;
        }
    }

    Some(json!({
        "ok": true,
        "revision": selection.revision,
        "active": selection.active,
        "previous": selection.previous,
        "pending": selection.pending,
        "trial": selection.trial,
        "highest_sequence": selection.highest_sequence.to_string(),
    }))
}

fn package_selection_status_with_running(
    package_root: &str,
    mut running: serde_json::Value,
) -> Option<serde_json::Value> {
    let mut status = package_selection_status(package_root)?;
    if running["available"] == true
        && let Some(digest) = running["digest"]
            .as_str()
            .filter(|digest| is_sha256(digest))
        && let Some(modules) = running_package_modules(Path::new(package_root), digest)
    {
        running["modules"] = modules;
    }
    status["running"] = running;
    Some(status)
}

fn running_package_status(runtime: &RuntimeClient) -> serde_json::Value {
    match runtime.send(Command::GetRunningPackageDigest {}) {
        Ok(Response::Result { payload }) => {
            running_package_status_value(payload.get("digest").and_then(serde_json::Value::as_str))
        }
        _ => running_package_status_value(None),
    }
}

fn running_package_modules(root: &Path, digest: &str) -> Option<serde_json::Value> {
    let path = amaterasu_package::package_directory(root, digest)
        .ok()?
        .join("manifest.json");
    if path.canonicalize().ok()? != path {
        return None;
    }
    let raw = amaterasu_package::read_bounded(&path, 4 * 1024 * 1024).ok()?;
    // The running digest comes from the host's verified loaded bytes. Never
    // derive this inventory from persisted active/pending selection or settings.
    if amaterasu_package::sha256(&raw) != digest {
        return None;
    }
    let manifest: amaterasu_package::ReleaseManifest = serde_json::from_slice(&raw).ok()?;
    let modules: Vec<_> = manifest
        .components
        .iter()
        .map(|component| amaterasu_module_sdk::RunningModuleIdentity {
            id: component.id.clone(),
            version: component.version.clone(),
            sha256: component.sha256.clone(),
            runtime: component.runtime,
        })
        .collect();
    let value = serde_json::to_value(modules).ok()?;
    valid_running_modules(&value).then_some(value)
}

fn valid_running_modules(value: &serde_json::Value) -> bool {
    let Ok(modules) =
        serde_json::from_value::<Vec<amaterasu_module_sdk::RunningModuleIdentity>>(value.clone())
    else {
        return false;
    };
    let mut ids = std::collections::BTreeSet::new();
    !modules.is_empty()
        && modules.len() <= 128
        && modules.iter().all(|module| {
            !module.id.is_empty()
                && module.id.len() <= 128
                && module.id.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
                })
                && !module.version.is_empty()
                && module.version.len() <= 128
                && !module.version.chars().any(char::is_control)
                && is_sha256(&module.sha256)
                && ids.insert(&module.id)
        })
}

fn running_package_status_value(digest: Option<&str>) -> serde_json::Value {
    digest.filter(|digest| is_sha256(digest)).map_or_else(
        || json!({"available":false}),
        |digest| json!({"available":true,"digest":digest}),
    )
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn package_selection_unavailable() -> serde_json::Value {
    json!({"ok":false,"code":"package_selection_unavailable"})
}

impl ManagementModule {
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

    fn temporary_package_root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-management-selection-{}",
            amaterasu_package::random_identifier().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        root.canonicalize().unwrap()
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

    #[test]
    fn package_selection_status_reads_fresh_bounded_phase_fields_only() {
        let root = temporary_package_root();
        let active = "a".repeat(64);
        let pending = "b".repeat(64);
        let trial = "c".repeat(64);
        amaterasu_package::write_json_atomically(
            &root.join("selection.json"),
            &json!({
                "active": active.clone(),
                "previous": null,
                "pending": pending.clone(),
                "trial": trial.clone(),
                "highest_sequence": u64::MAX,
                "repository_sets": {"set": {"repository_id":"private-publisher", "key_fingerprint":"d".repeat(64)}},
                "repository_floors": {},
                "healthy_sets": [],
            }),
        )
        .unwrap();

        let status = package_selection_status_with_running(
            root.to_str().unwrap(),
            running_package_status_value(Some(&"e".repeat(64))),
        )
        .unwrap();
        assert_eq!(status["ok"], true);
        assert!(is_sha256(status["revision"].as_str().unwrap()));
        assert_eq!(status["active"], active);
        assert_eq!(status["previous"], serde_json::Value::Null);
        assert_eq!(status["pending"], pending);
        assert_eq!(status["trial"], trial);
        assert_eq!(status["highest_sequence"], u64::MAX.to_string());
        assert_eq!(status["running"]["available"], true);
        assert_eq!(status["running"]["digest"], "e".repeat(64));
        assert!(status.get("package_root").is_none());
        assert!(status.get("repository_sets").is_none());
        assert!(!status.to_string().contains("private-publisher"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_selection_status_fails_closed_for_missing_malformed_or_unsafe_state() {
        let root = temporary_package_root();
        let empty = package_selection_status(root.to_str().unwrap()).unwrap();
        assert_eq!(empty["active"], serde_json::Value::Null);
        assert_eq!(empty["pending"], serde_json::Value::Null);
        assert_eq!(
            empty["revision"],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        std::fs::write(root.join("selection.json"), b"not json").unwrap();
        assert!(package_selection_status(root.to_str().unwrap()).is_none());

        amaterasu_package::write_json_atomically(
            &root.join("selection.json"),
            &json!({
                "active": "A".repeat(64),
                "previous": null,
                "pending": null,
                "trial": null,
                "highest_sequence": 1,
                "repository_sets": {},
                "repository_floors": {},
                "healthy_sets": [],
            }),
        )
        .unwrap();
        assert!(package_selection_status(root.to_str().unwrap()).is_none());
        assert!(package_selection_status("relative/package-root").is_none());
        assert_eq!(
            running_package_status_value(None),
            json!({"available":false})
        );
        assert_eq!(
            running_package_status_value(Some("not-a-digest")),
            json!({"available":false})
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn package_selection_status_rejects_symlinked_state_files_and_roots() {
        use std::os::unix::fs::symlink;

        let root = temporary_package_root();
        let target = root.join("target.json");
        std::fs::write(
            &target,
            br#"{"active":null,"previous":null,"pending":null,"trial":null,"highest_sequence":0,"repository_sets":{},"repository_floors":{},"healthy_sets":[]}"#,
        )
        .unwrap();
        symlink(&target, root.join("selection.json")).unwrap();
        assert!(package_selection_status(root.to_str().unwrap()).is_none());

        let alias = root.with_extension("alias");
        symlink(&root, &alias).unwrap();
        assert!(package_selection_status(alias.to_str().unwrap()).is_none());

        std::fs::remove_file(alias).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn module_inventory_uses_the_running_manifest_and_rejects_tampered_bytes() {
        let root = temporary_package_root();
        let components: Vec<_> = [
            "amaterasu.runtime",
            "amaterasu.display",
            "amaterasu.frame-limiter",
        ]
        .iter()
        .enumerate()
        .map(|(index, id)| {
            json!({
                "id": id,
                "version": "0.1.0",
                "file": "private-library.so",
                "sha256": "b".repeat(64),
                "size": 1,
                "abi_major": 1,
                "runtime": index == 0,
                "requested": {}
            })
        })
        .collect();
        let raw = serde_json::to_vec(&json!({
            "schema": 1,
            "host_context_schema": 1,
            "project": "amaterasu",
            "sequence": 1,
            "expires_unix": u64::MAX,
            "target": "test",
            "components": components
        }))
        .unwrap();
        let digest = amaterasu_package::sha256(&raw);
        let directory = amaterasu_package::package_directory(&root, &digest).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("manifest.json"), &raw).unwrap();
        let modules = running_package_modules(&root, &digest).unwrap();
        assert_eq!(modules[1]["id"], "amaterasu.display");
        assert_eq!(modules[2]["id"], "amaterasu.frame-limiter");
        assert!(modules[0].get("file").is_none());
        assert!(
            running_package_modules(&root, &"f".repeat(64)).is_none(),
            "a persisted active digest is never a substitute for the host's running digest"
        );
        std::fs::write(directory.join("manifest.json"), b"{}").unwrap();
        assert!(running_package_modules(&root, &digest).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
