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
    /// Optional independently selected private policy resolver.
    #[serde(default)]
    pub policy_service: Option<String>,
    /// Policies are selected by the library-local application ID, never by an
    /// executor response. They are supplied by trusted startup composition.
    #[serde(default)]
    pub application_policies: BTreeMap<String, ApplicationPolicy>,
    #[serde(default)]
    pub require_application_policy: bool,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationPolicy {
    pub revision: String,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default = "default_exit_timeout_seconds")]
    pub exit_timeout_seconds: u64,
    #[serde(default)]
    pub preparation: Vec<crate::protocol::PreparationStep>,
}

fn default_exit_timeout_seconds() -> u64 {
    10
}

fn valid_revision(revision: &str) -> bool {
    !revision.trim().is_empty() && revision.len() <= 128 && !revision.chars().any(char::is_control)
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
                || route.application_policies.len() > 256
                || route.policy_service.as_ref().is_some_and(|service| {
                    service.strip_suffix("/1").is_none_or(|name| !key(name))
                        || service == crate::SERVICE
                        || service == &route.service
                        || route.executors.values().any(|executor| executor == service)
                        || !route.application_policies.is_empty()
                })
                || route.application_policies.iter().any(|(id, policy)| {
                    ApplicationReference {
                        library: name.clone(),
                        id: id.clone(),
                    }
                    .validate()
                    .is_err()
                        || !valid_revision(&policy.revision)
                        || policy
                            .working_directory
                            .as_deref()
                            .is_some_and(|path| !crate::protocol::valid_native_path(path))
                        || !(1..=crate::protocol::MAX_EXIT_TIMEOUT_SECONDS)
                            .contains(&policy.exit_timeout_seconds)
                        || !crate::protocol::valid_preparation(&policy.preparation)
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
    let policy = route.application_policies.get(&selection.application.id);
    if route.require_application_policy && policy.is_none() && route.policy_service.is_none() {
        return Err("application_policy_unavailable");
    }
    if policy.is_some_and(|policy| policy.revision != selection.revision) {
        return Err("application_policy_revision_conflict");
    }
    let preparation = match policy {
        Some(policy) => crate::protocol::merge_preparation_plan(
            &[],
            &[],
            policy
                .preparation
                .iter()
                .cloned()
                .map(crate::protocol::PlannedPreparationStep::Structured)
                .collect(),
            vec![],
            false,
            policy.working_directory.as_deref(),
        )?,
        None => vec![],
    };
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
            exit_timeout_seconds: policy.map_or(10, |policy| policy.exit_timeout_seconds),
            working_directory: policy.and_then(|policy| policy.working_directory.clone()),
            preparation,
        },
        service.clone(),
        resource,
    ))
}

/// Apply only the exact response from the startup-selected resolver. Provider
/// payloads cannot select an executor or inject an application identity.
pub fn apply_policy(
    selection: &Selection,
    target: &mut crate::protocol::LaunchTarget,
    payload: Value,
) -> Result<String, &'static str> {
    use amaterasu_module_sdk::application_library::policy::{
        MAX_POLICY_BYTES, Request, ResolveAction, Snapshot,
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        ok: bool,
        policy: Snapshot,
    }
    if !serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() <= MAX_POLICY_BYTES) {
        return Err("invalid_policy_response");
    }
    let reply: Reply = serde_json::from_value(payload).map_err(|_| "invalid_policy_response")?;
    let request = Request {
        action: ResolveAction::Resolve,
        application: selection.application.clone(),
        entry_revision: selection.revision.clone(),
    };
    if !reply.ok
        || reply.policy.validate_for(&request).is_err()
        || reply
            .policy
            .working_directory
            .as_deref()
            .is_some_and(|path| !crate::protocol::valid_native_path(path))
        || !crate::protocol::valid_preparation(&reply.policy.preparation)
    {
        return Err("invalid_policy_response");
    }
    let crate::protocol::LaunchTarget::Provider {
        preparation,
        working_directory,
        exit_timeout_seconds,
        ..
    } = target
    else {
        return Err("invalid_policy_target");
    };
    let steps = reply
        .policy
        .preparation
        .into_iter()
        .map(crate::protocol::PlannedPreparationStep::Structured)
        .collect();
    *preparation = crate::protocol::merge_preparation_plan(
        &[],
        &[],
        steps,
        vec![],
        false,
        reply.policy.working_directory.as_deref(),
    )?;
    *working_directory = reply.policy.working_directory;
    *exit_timeout_seconds = reply.policy.exit_timeout_seconds;
    Ok(reply.policy.policy_revision)
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
            policy_service: None,
            application_policies: BTreeMap::new(),
            require_application_policy: false,
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

    #[test]
    fn selected_policy_is_revision_bound_and_never_comes_from_library_reply() {
        let mut route = route();
        route.require_application_policy = true;
        let policy: ApplicationPolicy = serde_json::from_value(json!({
            "revision":"v1",
            "working_directory":std::env::temp_dir(),
            "exit_timeout_seconds":20,
            "preparation":[{"run":{"executable":std::env::current_exe().unwrap()},
                "undo":{"executable":std::env::current_exe().unwrap()}}]
        }))
        .unwrap();
        route.application_policies.insert("opaque".into(), policy);
        assert!(validate_routes(&BTreeMap::from([("games".into(), route.clone())])).is_ok());
        let (target, _, _) = resolve_target(&selection("opaque"), &route, reply("opaque")).unwrap();
        let crate::protocol::LaunchTarget::Provider {
            preparation,
            working_directory,
            exit_timeout_seconds,
            ..
        } = target
        else {
            panic!("expected provider");
        };
        assert_eq!(exit_timeout_seconds, 20);
        let [crate::protocol::PlannedPreparationStep::Structured(step)] = preparation.as_slice()
        else {
            panic!("expected the configured preparation");
        };
        assert_eq!(step.run.cwd, working_directory);
        assert_eq!(step.undo.as_ref().unwrap().cwd, working_directory);

        assert_eq!(
            resolve_target(&selection("other"), &route, reply("other")).unwrap_err(),
            "application_policy_unavailable"
        );
        route
            .application_policies
            .get_mut("opaque")
            .unwrap()
            .revision = "changed".into();
        assert_eq!(
            resolve_target(&selection("opaque"), &route, reply("opaque")).unwrap_err(),
            "application_policy_revision_conflict"
        );
        let mut injected = reply("opaque");
        injected["target"]["preparation"] = json!([]);
        assert!(resolve_target(&selection("opaque"), &route, injected).is_err());
    }

    #[test]
    fn invalid_preparation_settings_fail_before_service_start() {
        let mut route = route();
        let policy: ApplicationPolicy = serde_json::from_value(json!({
            "revision":"v1", "preparation":[{"run":{"executable":"relative"}}]
        }))
        .unwrap();
        route.application_policies.insert("opaque".into(), policy);
        assert!(validate_routes(&BTreeMap::from([("games".into(), route)])).is_err());
    }

    #[test]
    fn selected_resolver_snapshot_cannot_change_reference_or_inject_a_route() {
        let mut route = route();
        route.policy_service = Some("selected.policy/1".into());
        route.require_application_policy = true;
        assert!(validate_routes(&BTreeMap::from([("games".into(), route.clone())])).is_ok());
        let (mut target, _, _) =
            resolve_target(&selection("opaque"), &route, reply("opaque")).unwrap();
        let response = json!({"ok":true,"policy":{"application":{"library":"games","id":"opaque"},
            "entry_revision":"v1","policy_revision":"policy-v2","exit_timeout_seconds":9,"preparation":[]}});
        assert_eq!(
            apply_policy(&selection("opaque"), &mut target, response.clone()).unwrap(),
            "policy-v2"
        );
        for (field, value) in [
            ("entry_revision", json!("stale")),
            ("policy_revision", json!("")),
            ("service", json!("forged/1")),
        ] {
            let mut forged = response.clone();
            forged["policy"][field] = value;
            assert!(apply_policy(&selection("opaque"), &mut target, forged).is_err());
        }
        route.policy_service = Some("selected.executor/1".into());
        assert!(validate_routes(&BTreeMap::from([("games".into(), route)])).is_err());
    }
}
