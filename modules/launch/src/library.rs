//! Opt-in library/executor composition. No catalog importer or store enum is
//! involved: a selected library resolves IDs into its approved executor routes.
use amaterasu_module_sdk::application_library::v2::{self, ApplicationReference, LaunchRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub service: String,
    pub callers: BTreeSet<String>,
    pub executors: BTreeMap<String, String>,
}

pub fn validate_routes(routes: &BTreeMap<String, Route>) -> Result<(), &'static str> {
    let key = crate::protocol::valid_catalog_principal;
    if routes.len() > 32
        || routes.iter().any(|(name, route)| {
            !key(name)
                || route
                    .service
                    .strip_suffix("/2")
                    .is_none_or(|name| !key(name))
                || route.service == v2::LAUNCH_SERVICE
                || route.callers.len() > 32
                || route.callers.iter().any(|caller| !key(caller))
                || route.executors.len() > 32
                || route.executors.iter().any(|(name, service)| {
                    !key(name)
                        || service.strip_suffix("/1").is_none_or(|name| !key(name))
                        || service == crate::SERVICE
                })
        })
    {
        return Err("invalid_library_routes");
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Selection {
    pub application: ApplicationReference,
    pub revision: String,
    #[serde(skip)]
    pub service: String,
}

/// Both public launch endpoints retain their exact versioned payloads. This
/// parser does not fall back from an unknown version to a legacy command.
pub fn decode(
    service: &str,
    payload: Value,
    caller: &str,
    initiator: &str,
) -> Result<crate::protocol::Operation, &'static str> {
    use crate::protocol::Operation;
    if service == crate::SERVICE {
        let operation: Operation =
            serde_json::from_value(payload).map_err(|_| "invalid_request")?;
        operation.validate()?;
        return Ok(operation);
    }
    if service != v2::LAUNCH_SERVICE {
        return Err("invalid_service");
    }
    // This initial composition supports direct component consumers only. A
    // future authenticated device/user context must be verified separately.
    if caller != initiator {
        return Err("permission_denied");
    }
    let request: LaunchRequest = serde_json::from_value(payload).map_err(|_| "invalid_request")?;
    request.validate()?;
    Ok(match request {
        LaunchRequest::Describe {} => Operation::DescribeLibrary {},
        LaunchRequest::Launch {
            operation_id,
            application,
            revision,
        } => Operation::LaunchLibrary {
            operation_id,
            application,
            revision,
        },
        LaunchRequest::Lookup { operation_id } => Operation::Lookup { operation_id },
        LaunchRequest::Stop { launch_id } => Operation::Stop { launch_id },
        LaunchRequest::Status { launch_id } => Operation::Status { launch_id },
    })
}

pub fn resolve_target(
    selection: &Selection,
    route: &Route,
    payload: Value,
) -> Result<(crate::protocol::LaunchTarget, String, String), &'static str> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        ok: bool,
        target: v2::LaunchTarget,
    }
    if !serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() <= v2::MAX_RESPONSE_BYTES) {
        return Err("invalid_library_response");
    }
    let reply: Reply = serde_json::from_value(payload).map_err(|_| "invalid_library_response")?;
    if !reply.ok {
        return Err("invalid_library_response");
    }
    reply
        .target
        .validate_for(&selection.application.id, &selection.revision)?;
    let service = route
        .executors
        .get(&reply.target.executor)
        .ok_or("executor_not_configured")?;
    // Aliases/libraries addressing the same execution target share a lease.
    // The service is fixed by approved startup settings, not by the response.
    let resource_bytes = serde_json::to_vec(&(service, &reply.target.application_id))
        .map_err(|_| "invalid_library_response")?;
    let resource = format!("library-execution/{:x}", Sha256::digest(resource_bytes));
    Ok((
        crate::protocol::LaunchTarget::Provider {
            provider: reply.target.executor,
            application_id: reply.target.application_id,
            exit_timeout_seconds: 10,
            working_directory: None,
            preparation: vec![],
        },
        service.clone(),
        resource,
    ))
}

pub fn rejection_code(payload: &Value) -> &'static str {
    match payload.pointer("/error/code").and_then(Value::as_str) {
        Some("revision_conflict") => "revision_conflict",
        Some("not_ready") => "not_ready",
        Some("not_found") => "application_not_found",
        Some("unauthorized") | Some("permission_denied") => "permission_denied",
        Some("unsupported_capability") => "unsupported_capability",
        _ => "library_rejected",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn route() -> Route {
        Route {
            service: "selected.library/2".into(),
            callers: ["consumer".into()].into(),
            executors: BTreeMap::from([("runner".into(), "selected.executor/1".into())]),
        }
    }

    fn selection(id: &str) -> Selection {
        Selection {
            application: ApplicationReference {
                library: "games".into(),
                id: id.into(),
            },
            revision: "v1".into(),
            service: "selected.library/2".into(),
        }
    }

    fn reply(id: &str) -> Value {
        json!({"ok":true,"target":{"id":id,"revision":"v1","executor":"runner","application_id":"game"}})
    }

    #[test]
    fn protocol_version_is_explicit_and_legacy_cannot_invoke_new_actions() {
        let payload = json!({"action":"launch","operation_id":"op","application":{"library":"games","id":"opaque"},"revision":"v1"});
        assert!(decode(v2::LAUNCH_SERVICE, payload.clone(), "consumer", "consumer").is_ok());
        assert!(
            decode(
                "amaterasu.launch/3",
                payload.clone(),
                "consumer",
                "consumer"
            )
            .is_err()
        );
        assert!(decode(crate::SERVICE, payload.clone(), "consumer", "consumer").is_err());
        assert!(decode(v2::LAUNCH_SERVICE, payload, "proxy", "consumer").is_err());
        assert!(
            decode(
                crate::SERVICE,
                json!({"action":"describe_library"}),
                "consumer",
                "consumer"
            )
            .is_err()
        );
    }

    #[test]
    fn routes_reject_self_reference_and_wrong_protocol_versions() {
        let mut route = route();
        for service in ["amaterasu.launch/2", "selected.library/1", "../library/2"] {
            route.service = service.into();
            assert!(validate_routes(&BTreeMap::from([("games".into(), route.clone())])).is_err());
        }
        route = self::route();
        route
            .executors
            .insert("runner".into(), crate::SERVICE.into());
        assert!(validate_routes(&BTreeMap::from([("games".into(), route)])).is_err());
    }

    #[test]
    fn handoff_cannot_inject_route_or_change_requested_identity() {
        let mut payload = reply("opaque");
        for (field, value) in [
            ("id", "other"),
            ("revision", "v2"),
            ("executor", "unapproved"),
            ("service", "injected/1"),
        ] {
            let mut changed = payload.clone();
            changed["target"][field] = json!(value);
            assert!(resolve_target(&selection("opaque"), &route(), changed).is_err());
        }
        payload["command"] = json!("injected");
        assert!(resolve_target(&selection("opaque"), &route(), payload).is_err());
    }

    #[test]
    fn aliases_share_execution_lease_and_service_is_part_of_identity() {
        let (_, _, first) = resolve_target(&selection("a"), &route(), reply("a")).unwrap();
        let (_, _, alias) = resolve_target(&selection("b"), &route(), reply("b")).unwrap();
        assert_eq!(first, alias);
        let mut other = route();
        other
            .executors
            .insert("runner".into(), "another.executor/1".into());
        let (_, _, independent) = resolve_target(&selection("a"), &other, reply("a")).unwrap();
        assert_ne!(first, independent);
    }
}
