//! Development loopback HTTP management lives in this independently loaded module.
mod assets;
mod csrf_policy;
mod http;
mod peer_policy;
mod product_settings;
mod route_payload;
mod routes;
mod server;
mod tls;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::Deserialize;
use serde_json::json;
use server::Server;
use std::net::SocketAddr;
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
    operational_health: Option<OperationalHealth>,
    product_settings: product_settings::ProductSettings,
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
            operational_health: None,
            product_settings: product_settings::ProductSettings::default(),
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
                let payload = if payload == json!({"action":"status"}) {
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
}
