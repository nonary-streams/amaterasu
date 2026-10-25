//! Development loopback HTTP management lives in this independently loaded module.
mod assets;
mod capture_management;
mod csrf_policy;
mod http;
mod peer_policy;
mod product_settings;
mod route_payload;
mod routes;
mod server;
mod tls;
mod tray_events;

#[cfg(test)]
mod csrf_adversarial_tests;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use amaterasu_package::repository::RepositoryStore;
use peer_policy::PeerTier;
use serde::Deserialize;
use serde_json::json;
use server::Server;
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

const SERVICE: &str = "amaterasu.management/1";
const AUTH_SERVICE: &str = "amaterasu.authentication/1";

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    enabled: bool,
    bind: Option<SocketAddr>,
    tls_bind: Option<SocketAddr>,
    tls_certificate_path: Option<String>,
    tls_private_key_path: Option<String>,
    #[serde(default)]
    allowed_origins: Vec<String>,
    web: Option<assets::WebSettings>,
}

struct ManagementModule {
    runtime: RuntimeClient,
    server: Option<Server>,
    repositories: Option<RepositoryStore>,
    operational_health: Option<OperationalHealth>,
    product_settings: product_settings::ProductSettings,
    tray_events: tray_events::TrayEventConsumer,
}

const CATALOG_SERVICE: &str = "amaterasu.catalog/1";
const PAIRING_SERVICE: &str = "amaterasu.pairing/1";

enum HealthProbe {
    Authentication,
    Catalog,
    Pairing,
}

struct OperationalHealth {
    request: u64,
    child: u64,
    deadline: Instant,
    probe: HealthProbe,
}

impl ManagedModule for ManagementModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("management settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
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
        let server = if settings.enabled {
            Some(Server::bind(
                settings
                    .bind
                    .ok_or("enabled management requires an explicit loopback bind")?,
                settings.allowed_origins,
                settings.web.map(assets::Assets::new).transpose()?,
                settings.tls_bind,
                tls_configuration,
            )?)
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
            operational_health: None,
            product_settings: product_settings::ProductSettings::default(),
            tray_events: tray_events::TrayEventConsumer::default(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.product_settings
            .tick(Instant::now(), &mut |command| self.runtime.send(command))?;
        self.advance_operational_health()?;
        if let Some(server) = &mut self.server {
            server.set_peer_policy(self.product_settings.maximum_peer());
            server.set_csrf_policy(self.product_settings.csrf_origins());
            server.tick(&self.runtime)?;
        }
        self.tray_events.tick(&self.runtime, self.server.as_ref())?;
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
    running: serde_json::Value,
) -> Option<serde_json::Value> {
    let mut status = package_selection_status(package_root)?;
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
        let deadline = Instant::now() + Duration::from_millis(remaining_ms.min(10_000));
        let child = submit_health_request(
            &self.runtime,
            request,
            deadline,
            AUTH_SERVICE,
            json!({"action":"status"}),
            "authentication",
        )?;
        self.operational_health = Some(OperationalHealth {
            request,
            child,
            deadline,
            probe: HealthProbe::Authentication,
        });
        Ok(())
    }

    fn advance_operational_health(&mut self) -> ModuleResult<()> {
        let Some(check) = self.operational_health.take() else {
            return Ok(());
        };
        if Instant::now() >= check.deadline {
            let _ = self.runtime.send(Command::Cancel {
                request: check.child,
            });
            let _ = self.runtime.send(Command::ReleaseResult {
                request: check.child,
            });
            return complete(
                &self.runtime,
                check.request,
                json!({"ok":false,"code":"health_timeout"}),
            );
        }
        match self.runtime.send(Command::TakeResult {
            request: check.child,
        })? {
            Response::Pending => {
                self.operational_health = Some(check);
                Ok(())
            }
            Response::Result { payload } if health_response(&check.probe, &payload) => {
                let (probe, service, payload, name) = match check.probe {
                    HealthProbe::Authentication => (
                        HealthProbe::Catalog,
                        CATALOG_SERVICE,
                        json!({"action":"list","cursor":null,"limit":1}),
                        "catalog",
                    ),
                    HealthProbe::Catalog => (
                        HealthProbe::Pairing,
                        PAIRING_SERVICE,
                        json!({"action":"identity"}),
                        "pairing",
                    ),
                    HealthProbe::Pairing => {
                        return complete(
                            &self.runtime,
                            check.request,
                            json!({
                                "ok":true,
                                "authentication_verifier":true,
                                "catalog_loaded":true,
                                "pairing_identity":true,
                            }),
                        );
                    }
                };
                let child = submit_health_request(
                    &self.runtime,
                    check.request,
                    check.deadline,
                    service,
                    payload,
                    name,
                )?;
                self.operational_health = Some(OperationalHealth {
                    probe,
                    child,
                    ..check
                });
                Ok(())
            }
            Response::Result { .. } => complete(
                &self.runtime,
                check.request,
                json!({"ok":false,"code":"required_state_unavailable"}),
            ),
            _ => complete(
                &self.runtime,
                check.request,
                json!({"ok":false,"code":"required_service_unavailable"}),
            ),
        }
    }
}

fn submit_health_request(
    runtime: &RuntimeClient,
    parent: u64,
    deadline: Instant,
    service: &str,
    payload: serde_json::Value,
    name: &str,
) -> ModuleResult<u64> {
    let timeout_ms = deadline
        .saturating_duration_since(Instant::now())
        .as_millis() as u64;
    if timeout_ms == 0 {
        return Err("operational health deadline elapsed".into());
    }
    match runtime.send(Command::Request {
        service: service.into(),
        payload,
        timeout_ms,
        operation_id: format!("management.health.{parent}.{name}"),
        parent: Some(parent),
    })? {
        Response::Accepted { id } => Ok(id),
        _ => Err("operational health service is unavailable".into()),
    }
}

fn health_response(probe: &HealthProbe, payload: &serde_json::Value) -> bool {
    payload.get("ok").and_then(serde_json::Value::as_bool) == Some(true)
        && match probe {
            HealthProbe::Authentication => {
                payload
                    .get("verifier_available")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            }
            HealthProbe::Catalog => payload
                .get("entries")
                .and_then(serde_json::Value::as_array)
                .is_some(),
            HealthProbe::Pairing => payload
                .get("host_id")
                .and_then(serde_json::Value::as_str)
                .is_some(),
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
}
