//! Field validation for routes with explicit payload restrictions.
//! Native domain services retain semantic validation and effect authorization.
use crate::routes::Route;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

type Object = Map<String, Value>;
const INVALID: &str = "unexpected or malformed route fields";

pub fn validate(route: Route, object: &Object) -> Result<(), &'static str> {
    if route.requires_empty_body {
        return fields(object, &[], &[]);
    }
    match route.service {
        "amaterasu.management/1" if route.action == "extension_list" => fields(object, &[], &[]),
        "amaterasu.management/1" if route.action == "extension_call" => {
            fields(object, &["id", "digest", "method", "payload"], &[])?;
            for name in ["id", "digest", "method"] {
                text(object, name)?;
            }
            Ok(())
        }
        "amaterasu.management/1" if route.action == "repository_list" => {
            fields(object, &[], &["offset"])?;
            if object
                .get("offset")
                .is_some_and(|value| !value.as_u64().is_some_and(|offset| offset <= 4096))
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "amaterasu.management/1" if route.action == "package_selection_status" => {
            fields(object, &[], &[])
        }
        "amaterasu.management/1" if route.action == "repository_add" => {
            fields(
                object,
                &["id", "location", "public_key", "fingerprint"],
                &[],
            )?;
            repository_text(object, "id", 128)?;
            repository_text(object, "location", 256)?;
            repository_text(object, "public_key", 64)?;
            repository_text(object, "fingerprint", 64)
        }
        "amaterasu.management/1"
            if route.action == "repository_refresh" || route.action == "repository_remove" =>
        {
            fields(object, &["id"], &[])?;
            repository_text(object, "id", 128)
        }
        "amaterasu.management/1" if route.action == "module_describe" => {
            fields(object, &["id"], &[])?;
            repository_text(object, "id", 128)
        }
        "amaterasu.extension-registry/1" => session_extensions(route.action, object),
        "amaterasu.extension-actions/1" => extension_actions(route.action, object),
        "amaterasu.session-lifecycle/1" if route.action == "snapshot" => fields(object, &[], &[]),
        "amaterasu.authentication/1" => authentication(route.action, object),
        "amaterasu.management/1" => management_settings(route.action, object),
        "amaterasu.host-configuration/1" => host_configuration(route.action, object),
        "amaterasu.catalog/1" => catalog(route.action, object),
        "amaterasu.library/1" => library_artwork(route.action, object),
        "amaterasu.pairing/1" => pairing(route.action, object),
        "amaterasu.display/1" => display(route.action, object),
        "amaterasu.video/1" => video(route.action, object),
        "amaterasu.launch/1" => launch(route.action, object),
        "amaterasu.nvhttp/1" => nvhttp(route.action, object),
        "amaterasu.steam/1" => steam(route.action, object),
        "amaterasu.lutris/1" => lutris(route.action, object),
        "amaterasu.playnite/1" => playnite(route.action, object),
        "amaterasu.input/1" => input(route.action, object),
        "amaterasu.session-history/1" => history(route.action, object),
        "amaterasu.host-metrics/1"
            if route.action == "settings_get" || route.action == "stats_read" =>
        {
            fields(object, &[], &[])
        }
        "amaterasu.host-metrics/1" if route.action == "settings_update" => {
            host_metrics_settings_update(object)
        }
        "amaterasu.updater/1"
            if route.action == "settings_get"
                || route.action == "status"
                || route.action == "check" =>
        {
            fields(object, &[], &[])
        }
        "amaterasu.updater/1" if route.action == "settings_update" => {
            updater_settings_update(object)
        }
        "amaterasu.system-tray/1" if route.action == "settings_get" => fields(object, &[], &[]),
        "amaterasu.system-tray/1" if route.action == "settings_update" => {
            system_tray_settings_update(object)
        }
        "amaterasu.diagnostics/1" if route.action == "status" || route.action == "counters" => {
            fields(object, &[], &[])
        }
        amaterasu_contracts::HOST_LOG_SERVICE => host_logs(route.action, object),
        "amaterasu.capture-pipewire/1" => capture_pipewire(route.action, object),
        "amaterasu.diagnostics/1" if route.action == "query" => {
            fields(object, &[], &["after", "limit"])?;
            if object
                .get("after")
                .is_some_and(|value| value.as_u64().is_none())
                || object.get("limit").is_some_and(|value| {
                    !value.as_u64().is_some_and(|limit| (1..=4).contains(&limit))
                })
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "amaterasu.diagnostics/1" if route.action == "refresh_logging" => {
            fields(object, &["minimum_revision"], &[])?;
            if object
                .get("minimum_revision")
                .and_then(Value::as_u64)
                .is_none_or(|revision| revision == 0)
            {
                return Err(INVALID);
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn library_artwork(action: &str, object: &Object) -> Result<(), &'static str> {
    if action != "artwork_open" {
        return Err(INVALID);
    }
    fields(object, &["id"], &["revision"])?;
    if let Some(revision) = object.get("revision")
        && revision
            .as_str()
            .is_none_or(|r| r.is_empty() || r.len() > 128 || r.chars().any(char::is_control))
    {
        return Err(INVALID);
    }
    let id = text(object, "id")?;
    if id.len() != 20
        || !id.starts_with("app-")
        || !id[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || id[4..].bytes().all(|b| b == b'0')
    {
        return Err(INVALID);
    }
    Ok(())
}

fn host_logs(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "open" | "export_start" => fields(object, &[], &[]),
        "read" | "export_read" | "close" | "export_status" => {
            if action == "read" || action == "export_read" {
                fields(object, &["snapshot", "offset", "limit"], &[])?;
                if object.get("offset").and_then(Value::as_u64).is_none()
                    || !object
                        .get("limit")
                        .and_then(Value::as_u64)
                        .is_some_and(|limit| {
                            (1..=if action == "read" { 4096 } else { 8192 }).contains(&limit)
                        })
                {
                    return Err(INVALID);
                }
            } else {
                fields(object, &["snapshot"], &[])?;
            }
            let snapshot = text(object, "snapshot")?;
            if snapshot.len() != 32
                || !snapshot
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(INVALID);
            }
            Ok(())
        }
        _ => Err(INVALID),
    }
}

fn capture_pipewire(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "status" => fields(object, &[], &[]),
        "select_source" => {
            fields(object, &["source_type", "operation_id"], &[])?;
            if !matches!(text(object, "source_type")?, "monitor" | "window")
                || !high_entropy_operation_id(text(object, "operation_id")?)
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "poll_selection" | "cancel_selection" => {
            fields(object, &["operation_id"], &[])?;
            if !high_entropy_operation_id(text(object, "operation_id")?) {
                return Err(INVALID);
            }
            Ok(())
        }
        _ => Err(INVALID),
    }
}

fn high_entropy_operation_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23]
            .into_iter()
            .all(|index| bytes[index] == b'-')
        && bytes.iter().enumerate().all(|(index, byte)| {
            [8, 13, 18, 23].contains(&index)
                || byte.is_ascii_digit()
                || (b'a'..=b'f').contains(byte)
        })
        && bytes[14] == b'4'
        && matches!(bytes[19], b'8'..=b'b')
}

#[cfg(test)]
mod capture_pipewire_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capture_routes_require_exact_scopes_and_strict_payloads() {
        for (path, action, scope) in [
            ("/v1/capture/pipewire_status", "status", "capture.read"),
            (
                "/v1/capture/pipewire_selection_begin",
                "select_source",
                "capture.write",
            ),
            (
                "/v1/capture/pipewire_selection_poll",
                "poll_selection",
                "capture.read",
            ),
            (
                "/v1/capture/pipewire_selection_cancel",
                "cancel_selection",
                "capture.write",
            ),
        ] {
            let route = crate::routes::resolve(path).unwrap();
            assert_eq!(route.service, "amaterasu.capture-pipewire/1");
            assert_eq!(route.action, action);
            assert_eq!(route.scope, Some(scope));
            assert!(!route.requires_empty_body);
        }

        let status = crate::routes::resolve("/v1/capture/pipewire_status").unwrap();
        assert!(validate(status, json!({}).as_object().unwrap()).is_ok());
        assert!(
            validate(
                status,
                json!({"operation_id":"untrusted"}).as_object().unwrap()
            )
            .is_err()
        );

        let begin = crate::routes::resolve("/v1/capture/pipewire_selection_begin").unwrap();
        let id = "a12f6f1e-04cb-4a97-bdf8-f53bb9cf2dd1";
        assert!(
            validate(
                begin,
                json!({"source_type":"monitor","operation_id":id})
                    .as_object()
                    .unwrap()
            )
            .is_ok()
        );
        for payload in [
            json!({"source_type":"monitor","operation_id":"id"}),
            json!({"source_type":"desktop","operation_id":id}),
            json!({"source_type":"window","operation_id":id,"caller":"other"}),
            json!({"source_type":"window","operation_id":"a12f6f1e-04cb-4A97-bdf8-f53bb9cf2dd1"}),
        ] {
            assert!(validate(begin, payload.as_object().unwrap()).is_err());
        }

        for (path, action) in [
            ("/v1/capture/pipewire_selection_poll", "poll_selection"),
            ("/v1/capture/pipewire_selection_cancel", "cancel_selection"),
        ] {
            let route = crate::routes::resolve(path).unwrap();
            assert_eq!(route.action, action);
            assert!(validate(route, json!({"operation_id":id}).as_object().unwrap()).is_ok());
            assert!(
                validate(
                    route,
                    json!({"operation_id":id,"subject":"other"})
                        .as_object()
                        .unwrap()
                )
                .is_err()
            );
        }
    }
}

fn management_settings(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "product_get" => fields(object, &[], &[]),
        "product_update" => {
            fields(
                object,
                &["expected_revision"],
                &["origin_web_ui_allowed", "csrf_allowed_origins"],
            )?;
            if object
                .get("expected_revision")
                .and_then(Value::as_u64)
                .is_none()
                || object.get("origin_web_ui_allowed").is_some_and(|value| {
                    !value.is_null() && !matches!(value.as_str(), Some("pc" | "lan" | "wan"))
                })
                || object.get("csrf_allowed_origins").is_some_and(|value| {
                    !value.is_null()
                        && serde_json::from_value::<crate::csrf_policy::CsrfOrigins>(value.clone())
                            .is_err()
                })
            {
                return Err(INVALID);
            }
            Ok(())
        }
        _ => Err(INVALID),
    }
}

#[cfg(test)]
mod management_policy_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn management_policy_routes_require_exact_scopes_and_bounded_fields() {
        let get = crate::routes::resolve("/v1/management_settings/get").unwrap();
        assert_eq!(get.service, "amaterasu.management/1");
        assert_eq!(get.scope, Some("configuration.read"));
        assert!(validate(get, json!({}).as_object().unwrap()).is_ok());
        assert!(validate(get, json!({"owner":"other"}).as_object().unwrap()).is_err());
        let update = crate::routes::resolve("/v1/management_settings/update").unwrap();
        assert_eq!(update.scope, Some("configuration.write"));
        for selection in [json!("pc"), json!("lan"), json!("wan"), Value::Null] {
            assert!(
                validate(
                    update,
                    json!({"expected_revision":0,"origin_web_ui_allowed":selection})
                        .as_object()
                        .unwrap()
                )
                .is_ok()
            );
        }
        assert!(validate(update, json!({"expected_revision":0}).as_object().unwrap()).is_ok());
        for origins in [
            Value::Null,
            json!([]),
            json!(["https://trusted.example:47990"]),
        ] {
            assert!(
                validate(
                    update,
                    json!({"expected_revision":0,"csrf_allowed_origins":origins})
                        .as_object()
                        .unwrap()
                )
                .is_ok()
            );
        }
        for payload in [
            json!({}),
            json!({"expected_revision":-1}),
            json!({"expected_revision":1.5}),
            json!({"expected_revision":0,"origin_web_ui_allowed":"WAN"}),
            json!({"expected_revision":0,"origin_web_ui_allowed":false}),
            json!({"expected_revision":0,"origin_web_ui_allowed":"wan","caller":"other"}),
            json!({"expected_revision":0,"csrf_allowed_origins":"https://trusted.example"}),
            json!({"expected_revision":0,"csrf_allowed_origins":["http://trusted.example"]}),
            json!({"expected_revision":0,"csrf_allowed_origins":["https://trusted.example/path"]}),
            json!({"expected_revision":0,"csrf_allowed_origins":["https://trusted.example","https://trusted.example"]}),
        ] {
            assert!(validate(update, payload.as_object().unwrap()).is_err());
        }
    }
}

fn video(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "select_portal_source" => {
            fields(object, &["source_type", "operation_id"], &[])?;
            if !matches!(text(object, "source_type")?, "monitor" | "window") {
                return Err(INVALID);
            }
        }
        "poll_portal_selection" | "cancel_portal_selection" => {
            fields(object, &["operation_id"], &[])?;
        }
        _ => return Err(INVALID),
    }
    let operation_id = text(object, "operation_id")?;
    if operation_id.is_empty()
        || operation_id.len() > 128
        || !operation_id.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(INVALID);
    }
    Ok(())
}

fn extension_actions(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "open_epoch" => {
            fields(object, &["operation_id"], &[])?;
            text(object, "operation_id")?;
        }
        "close_epoch" | "retire_epoch" => {
            fields(object, &["epoch"], &[])?;
            text(object, "epoch")?;
        }
        "status" => {
            fields(object, &["epoch", "sequence"], &[])?;
            text(object, "epoch")?;
            if object
                .get("sequence")
                .and_then(Value::as_u64)
                .is_none_or(|n| n == 0)
            {
                return Err(INVALID);
            }
        }
        "invoke" => {
            fields(
                object,
                &[
                    "epoch",
                    "sequence",
                    "operation_id",
                    "action_id",
                    "target",
                    "arguments",
                    "expected_settings_revision",
                ],
                &[],
            )?;
            for name in ["epoch", "operation_id", "action_id"] {
                text(object, name)?;
            }
            if object
                .get("sequence")
                .and_then(Value::as_u64)
                .is_none_or(|n| n == 0)
                || !object.get("arguments").is_some_and(Value::is_object)
                || !object
                    .get("expected_settings_revision")
                    .is_some_and(|v| v.is_null() || v.is_string())
            {
                return Err(INVALID);
            }
            let Some(target) = object.get("target").and_then(Value::as_object) else {
                return Err(INVALID);
            };
            fields(target, &["kind", "host_epoch"], &[])?;
            if target.get("kind").and_then(Value::as_str) != Some("host") {
                return Err(INVALID);
            }
            text(target, "host_epoch")?;
        }
        _ => return Err(INVALID),
    }
    Ok(())
}

fn session_extensions(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "list" => {
            fields(object, &[], &["after", "limit", "revision"])?;
            if object
                .get("revision")
                .is_some_and(|value| !value.is_null() && value.as_u64().is_none())
            {
                return Err(INVALID);
            }
            if object.get("after").is_some_and(|value| {
                !value.is_null()
                    && !value
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s.len() <= 256)
            }) || object
                .get("limit")
                .is_some_and(|value| !value.as_u64().is_some_and(|n| (1..=64).contains(&n)))
            {
                return Err(INVALID);
            }
        }
        "describe" => {
            fields(object, &["provider"], &[])?;
            text(object, "provider")?;
        }
        "settings_migrate" => {
            fields(object, &["provider", "expected_revision"], &[])?;
            text(object, "provider")?;
            if object
                .get("expected_revision")
                .and_then(Value::as_u64)
                .is_none()
            {
                return Err(INVALID);
            }
        }
        "settings_get" | "settings_update" => {
            if action == "settings_get" {
                fields(object, &["provider", "scope"], &[])?;
            } else {
                fields(
                    object,
                    &["provider", "scope", "expected_revision", "values"],
                    &[],
                )?;
            }
            text(object, "provider")?;
            validate_extension_settings_scope(object.get("scope"))?;
            if action == "settings_update"
                && (object
                    .get("expected_revision")
                    .and_then(Value::as_u64)
                    .is_none()
                    || !object.get("values").is_some_and(Value::is_object))
            {
                return Err(INVALID);
            }
        }
        _ => return Err(INVALID),
    }
    Ok(())
}

fn validate_extension_settings_scope(scope: Option<&Value>) -> Result<(), &'static str> {
    let scope = scope.and_then(Value::as_object).ok_or(INVALID)?;
    match scope.get("kind").and_then(Value::as_str) {
        Some("host") => fields(scope, &["kind"], &[]),
        Some("application") => {
            fields(scope, &["kind", "id"], &[])?;
            let id = text(scope, "id")?;
            let number = id
                .strip_prefix("app-")
                .and_then(|value| u64::from_str_radix(value, 16).ok())
                .ok_or(INVALID)?;
            if number == 0 || id != format!("app-{number:016x}") {
                return Err(INVALID);
            }
            Ok(())
        }
        // HTTP has not authenticated ownership of session/client targets.
        _ => Err(INVALID),
    }
}

fn host_configuration(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "get" => fields(object, &[], &[]),
        "update" => {
            fields(
                object,
                &[
                    "expected_revision",
                    "file_state",
                    "vibeshine_file_state",
                    "credentials_file",
                    "pkey",
                    "cert",
                    "log_path",
                    "min_log_level",
                ],
                &[],
            )?;
            if object
                .get("expected_revision")
                .and_then(Value::as_u64)
                .is_none()
                || !matches!(
                    text(object, "min_log_level")?,
                    "product_default"
                        | "verbose"
                        | "debug"
                        | "info"
                        | "warning"
                        | "error"
                        | "fatal"
                        | "none"
                )
            {
                return Err(INVALID);
            }
            for key in [
                "file_state",
                "vibeshine_file_state",
                "credentials_file",
                "pkey",
                "cert",
                "log_path",
            ] {
                let path = text(object, key)?;
                if (path.is_empty() && key != "credentials_file")
                    || path.len() > 4096
                    || path.chars().any(char::is_control)
                    || Path::new(path)
                        .components()
                        .any(|component| matches!(component, std::path::Component::ParentDir))
                    || (cfg!(windows)
                        && !Path::new(path).is_absolute()
                        && Path::new(path)
                            .components()
                            .any(|component| matches!(component, std::path::Component::Prefix(_))))
                    || (!path.is_empty()
                        && !Path::new(path).is_absolute()
                        && Path::new(path)
                            .components()
                            .all(|component| matches!(component, std::path::Component::CurDir)))
                {
                    return Err(INVALID);
                }
            }
            Ok(())
        }
        _ => Err(INVALID),
    }
}

#[cfg(test)]
mod steam_artwork_tests {
    use super::*;

    #[test]
    fn artwork_cursor_accepts_the_sixteen_mib_contract_only_on_chunk_boundaries() {
        let route = Route {
            service: "amaterasu.steam/1",
            action: "artwork_read",
            scope: Some("steam.read"),
            requires_empty_body: false,
        };
        for offset in [0, 8 * 1024 * 1024, 16 * 1024 * 1024] {
            let request = serde_json::json!({"app_id":730,"revision":"42","offset":offset});
            assert!(validate(route, request.as_object().unwrap()).is_ok());
        }
        for offset in [16 * 1024 * 1024 + 1, 16 * 1024 * 1024 + 8192] {
            let request = serde_json::json!({"app_id":730,"revision":"42","offset":offset});
            assert!(validate(route, request.as_object().unwrap()).is_err());
        }
    }
}

#[cfg(test)]
mod lutris_artwork_tests {
    use crate::routes::{payload, resolve};
    use serde_json::json;

    #[test]
    fn artwork_payload_matches_provider_bounds_and_rejects_overrides() {
        let route = resolve("/v1/lutris/artwork").unwrap();
        let revision = "a".repeat(64);
        for game_id in [1_i64, i64::MAX, 42] {
            let request = json!({"game_id":game_id,"revision":revision});
            let forwarded = payload(route, request.to_string().as_bytes()).unwrap();
            assert_eq!(forwarded["action"], "artwork_open");
            assert_eq!(forwarded["game_id"], game_id);
        }
        let valid = json!({"game_id":42,"revision":revision});
        for (field, value) in [
            ("game_id", json!(0)),
            ("game_id", json!(-1)),
            ("game_id", json!(u64::MAX)),
            ("game_id", json!("42")),
            ("revision", json!("a".repeat(63))),
            ("revision", json!("A".repeat(64))),
            ("revision", json!("g".repeat(64))),
            ("offset", json!(-1)),
            ("offset", json!(1)),
            ("offset", json!(16 * 1024 * 1024)),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(
                payload(route, invalid.to_string().as_bytes()).is_err(),
                "{field}: {invalid}"
            );
        }
        for extra in ["action", "path", "caller", "service", "app_id"] {
            let mut invalid = valid.clone();
            invalid[extra] = json!("override");
            assert!(
                payload(route, invalid.to_string().as_bytes()).is_err(),
                "{extra}"
            );
        }
        for missing in ["game_id", "revision"] {
            let mut invalid = valid.clone();
            invalid.as_object_mut().unwrap().remove(missing);
            assert!(
                payload(route, invalid.to_string().as_bytes()).is_err(),
                "{missing}"
            );
        }
    }
}

fn authentication(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "enroll_first_admin" => {
            fields(object, &["code", "username", "password"], &[])?;
            for name in ["code", "username", "password"] {
                text(object, name)?;
            }
            Ok(())
        }
        "account_sessions_list" => fields(object, &[], &[]),
        "account_sessions_revoke" => {
            fields(object, &["session_id"], &[])?;
            let id = text(object, "session_id")?;
            if id.len() != 64
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "account_tokens_list" => fields(object, &[], &[]),
        "account_tokens_issue" => {
            fields(object, &["scopes", "ttl_seconds"], &[])?;
            let scopes = object
                .get("scopes")
                .and_then(Value::as_array)
                .ok_or(INVALID)?;
            if scopes.is_empty()
                || scopes.len() > 64
                || scopes
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != scopes.len()
                || scopes.iter().any(|scope| {
                    !scope.as_str().is_some_and(|scope| {
                        !scope.is_empty()
                            && scope.len() <= 64
                            && scope.bytes().all(|byte| {
                                byte.is_ascii_alphanumeric() || b"._-:/".contains(&byte)
                            })
                    })
                })
                || !object
                    .get("ttl_seconds")
                    .and_then(Value::as_u64)
                    .is_some_and(|seconds| (1..=30 * 24 * 60 * 60).contains(&seconds))
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "account_tokens_revoke" => {
            fields(object, &["token_id"], &[])?;
            let id = text(object, "token_id")?;
            if id.len() != 64
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "list_accounts" => fields(object, &[], &[]),
        "create_account" => {
            fields(object, &["username", "password", "role"], &[])?;
            for name in ["username", "password", "role"] {
                text(object, name)?;
            }
            Ok(())
        }
        "update_account" => {
            fields(object, &["username", "role", "enabled"], &[])?;
            text(object, "username")?;
            text(object, "role")?;
            object
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or(INVALID)?;
            Ok(())
        }
        "delete_account" => {
            fields(object, &["username"], &[])?;
            text(object, "username")?;
            Ok(())
        }
        "set_account_password" => {
            fields(object, &["username", "password"], &[])?;
            text(object, "username")?;
            text(object, "password")?;
            Ok(())
        }
        "change_own_password" => {
            fields(object, &["current_password", "new_password"], &[])?;
            text(object, "current_password")?;
            text(object, "new_password")?;
            Ok(())
        }
        "settings_get" | "account_settings_get" => fields(object, &[], &[]),
        "settings_update" | "account_settings_update" => {
            fields(
                object,
                &[
                    "session_token_ttl_seconds",
                    "remember_me_refresh_token_ttl_seconds",
                    "expected_revision",
                ],
                &[],
            )?;
            let session_ttl = object
                .get("session_token_ttl_seconds")
                .and_then(Value::as_u64);
            let refresh_ttl = object
                .get("remember_me_refresh_token_ttl_seconds")
                .and_then(Value::as_u64);
            if !session_ttl.is_some_and(|value| (1..=i32::MAX as u64).contains(&value))
                || !refresh_ttl.is_some_and(|value| (1..=i32::MAX as u64).contains(&value))
                || object
                    .get("expected_revision")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "login" => {
            fields(object, &["username", "password"], &["remember_me"])?;
            text(object, "username")?;
            text(object, "password")?;
            if object
                .get("remember_me")
                .is_some_and(|value| value.as_bool().is_none())
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "refresh" => {
            fields(object, &["refresh_token"], &[])?;
            authentication_token(text(object, "refresh_token")?)
        }
        "logout" => {
            fields(object, &[], &["bearer", "refresh_token"])?;
            match (object.get("bearer"), object.get("refresh_token")) {
                (Some(bearer), None) => authentication_token(bearer.as_str().ok_or(INVALID)?),
                (None, Some(refresh_token)) => {
                    authentication_token(refresh_token.as_str().ok_or(INVALID)?)
                }
                _ => Err(INVALID),
            }
        }
        _ => Err(INVALID),
    }
}

fn authentication_token(value: &str) -> Result<(), &'static str> {
    let Some(digest) = value.strip_prefix("ama_") else {
        return Err(INVALID);
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(INVALID);
    }
    Ok(())
}

fn history(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "settings_get" | "history_status" => fields(object, &[], &[]),
        "settings_update" => {
            fields(object, &["settings", "expected_revision"], &[])?;
            let settings = object
                .get("settings")
                .and_then(Value::as_object)
                .ok_or(INVALID)?;
            fields(settings, &["enabled", "ttl_days", "db_size_limit_mb"], &[])?;
            if settings.get("enabled").and_then(Value::as_bool).is_none()
                || ["ttl_days", "db_size_limit_mb"].iter().any(|field| {
                    settings
                        .get(*field)
                        .and_then(Value::as_i64)
                        .is_none_or(|value| !(0..=i64::from(i32::MAX)).contains(&value))
                })
                || object
                    .get("expected_revision")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "list_sessions" => {
            fields(object, &[], &["limit", "offset"])?;
            if object.get("limit").is_some_and(|value| {
                !value
                    .as_u64()
                    .is_some_and(|limit| (1..=25).contains(&limit))
            }) || object
                .get("offset")
                .is_some_and(|value| value.as_u64().is_none())
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "active_sessions" => {
            fields(object, &[], &["limit", "offset"])?;
            if object.get("limit").is_some_and(|value| {
                !value
                    .as_u64()
                    .is_some_and(|limit| (1..=16).contains(&limit))
            }) || object
                .get("offset")
                .is_some_and(|value| value.as_u64().is_none())
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "session_detail" | "delete_session" => {
            fields(object, &["session_id"], &[])?;
            let id = text(object, "session_id")?;
            if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
                return Err(INVALID);
            }
            Ok(())
        }
        "timeline_open" => {
            fields(object, &["session_id"], &["sample_limit"])?;
            let id = text(object, "session_id")?;
            if id.is_empty()
                || id.len() > 128
                || id.chars().any(char::is_control)
                || object.get("sample_limit").is_some_and(|value| {
                    !value
                        .as_u64()
                        .is_some_and(|limit| (1..=2000).contains(&limit))
                })
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "timeline_page" => {
            fields(object, &["session_id", "kind", "cursor"], &["limit"])?;
            let id = text(object, "session_id")?;
            if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
                return Err(INVALID);
            }
            let maximum = match text(object, "kind")? {
                "samples" => 32,
                "events" => 8,
                _ => return Err(INVALID),
            };
            if object.get("limit").is_some_and(|value| {
                !value
                    .as_u64()
                    .is_some_and(|limit| (1..=maximum).contains(&limit))
            }) {
                return Err(INVALID);
            }
            let cursor = object
                .get("cursor")
                .and_then(Value::as_object)
                .ok_or(INVALID)?;
            fields(
                cursor,
                &[
                    "session_id",
                    "kind",
                    "first_id",
                    "last_id",
                    "after_id",
                    "total",
                ],
                &[],
            )?;
            if text(cursor, "session_id")? != id || text(cursor, "kind")? != text(object, "kind")? {
                return Err(INVALID);
            }
            let parse_id = |value: &Value| {
                value.as_str().and_then(|value| {
                    if value.is_empty()
                        || value.len() > 19
                        || value.starts_with('0')
                        || !value.bytes().all(|byte| byte.is_ascii_digit())
                    {
                        return None;
                    }
                    value.parse::<i64>().ok().filter(|id| *id > 0)
                })
            };
            let first = parse_id(cursor.get("first_id").ok_or(INVALID)?).ok_or(INVALID)?;
            let last = parse_id(cursor.get("last_id").ok_or(INVALID)?).ok_or(INVALID)?;
            let after = match cursor.get("after_id").ok_or(INVALID)? {
                Value::Null => None,
                value => Some(parse_id(value).ok_or(INVALID)?),
            };
            if first > last
                || after.is_some_and(|value| value < first || value >= last)
                || !cursor
                    .get("total")
                    .and_then(Value::as_u64)
                    .is_some_and(|total| (1..=9_007_199_254_740_991).contains(&total))
            {
                return Err(INVALID);
            }
            Ok(())
        }
        _ => Err(INVALID),
    }
}

fn host_metrics_settings_update(object: &Object) -> Result<(), &'static str> {
    fields(object, &["settings", "expected_revision"], &[])?;
    let settings = object
        .get("settings")
        .and_then(Value::as_object)
        .ok_or(INVALID)?;
    fields(
        settings,
        &[
            "enabled",
            "poll_interval_ms",
            "history_retention_seconds",
            "max_history_points",
            "pause_when_hidden",
            "show_active_sessions",
            "show_host_stats",
            "show_host_charts",
            "show_session_history",
        ],
        &[],
    )?;
    if [
        "enabled",
        "pause_when_hidden",
        "show_active_sessions",
        "show_host_stats",
        "show_host_charts",
        "show_session_history",
    ]
    .iter()
    .any(|field| settings.get(*field).and_then(Value::as_bool).is_none())
        || !settings
            .get("poll_interval_ms")
            .and_then(Value::as_u64)
            .is_some_and(|value| (250..=60_000).contains(&value))
        || !settings
            .get("history_retention_seconds")
            .and_then(Value::as_u64)
            .is_some_and(|value| (30..=3_600).contains(&value))
        || !settings
            .get("max_history_points")
            .and_then(Value::as_u64)
            .is_some_and(|value| (30..=2_000).contains(&value))
        || object
            .get("expected_revision")
            .and_then(Value::as_u64)
            .is_none()
    {
        return Err(INVALID);
    }
    Ok(())
}

fn updater_settings_update(object: &Object) -> Result<(), &'static str> {
    fields(object, &["settings", "expected_revision"], &[])?;
    let settings = object
        .get("settings")
        .and_then(Value::as_object)
        .ok_or(INVALID)?;
    fields(
        settings,
        &["notify_pre_releases", "update_check_interval"],
        &[],
    )?;
    if settings
        .get("notify_pre_releases")
        .and_then(Value::as_bool)
        .is_none()
        || !settings
            .get("update_check_interval")
            .and_then(Value::as_u64)
            .is_some_and(|interval| interval <= i32::MAX as u64)
        || object
            .get("expected_revision")
            .and_then(Value::as_u64)
            .is_none()
    {
        return Err(INVALID);
    }
    Ok(())
}

fn system_tray_settings_update(object: &Object) -> Result<(), &'static str> {
    fields(object, &["settings", "expected_revision"], &[])?;
    let settings = object
        .get("settings")
        .and_then(Value::as_object)
        .ok_or(INVALID)?;
    fields(settings, &["system_tray"], &[])?;
    if settings
        .get("system_tray")
        .and_then(Value::as_bool)
        .is_none()
        || object
            .get("expected_revision")
            .and_then(Value::as_u64)
            .is_none()
    {
        return Err(INVALID);
    }
    Ok(())
}

fn steam(action: &str, object: &Object) -> Result<(), &'static str> {
    if action == "artwork_open" {
        fields(object, &["app_id", "revision"], &[])?;
        let id = object
            .get("app_id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or(INVALID)?;
        let revision = text(object, "revision")?;
        if id > u32::MAX as u64
            || !revision.bytes().all(|b| b.is_ascii_digit())
            || revision.len() > 20
            || revision.is_empty()
            || revision.len() > 128
            || revision.chars().any(char::is_control)
        {
            return Err(INVALID);
        }
        return Ok(());
    }

    match action {
        "settings_get" => fields(object, &[], &[]),
        "artwork_read" => {
            fields(object, &["app_id", "revision", "offset"], &[])?;
            if !object
                .get("app_id")
                .and_then(Value::as_u64)
                .is_some_and(|app_id| app_id > 0 && app_id <= u64::from(u32::MAX))
                || !object
                    .get("revision")
                    .and_then(Value::as_str)
                    .is_some_and(|revision| {
                        !revision.is_empty()
                            && revision.len() <= 20
                            && revision.bytes().all(|byte| byte.is_ascii_digit())
                            && revision.parse::<u64>().is_ok()
                    })
                || !object
                    .get("offset")
                    .and_then(Value::as_u64)
                    .is_some_and(|offset| offset <= 16 * 1024 * 1024 && offset % 8192 == 0)
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "settings_update" => {
            fields(
                object,
                &["enabled", "auto_sync", "exclude_games", "expected_revision"],
                &[
                    "include_tools",
                    "sync_all_installed",
                    "recent_games",
                    "recent_max_age_days",
                    "autosync_remove_uninstalled",
                ],
            )?;
            if object.get("enabled").and_then(Value::as_bool).is_none()
                || object.get("auto_sync").and_then(Value::as_bool).is_none()
                || object
                    .get("expected_revision")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err(INVALID);
            }
            for field in [
                "include_tools",
                "sync_all_installed",
                "autosync_remove_uninstalled",
            ] {
                if object
                    .get(field)
                    .is_some_and(|value| value.as_bool().is_none())
                {
                    return Err(INVALID);
                }
            }
            for (field, maximum) in [
                ("recent_games", 10_000_u64),
                ("recent_max_age_days", 36_500_u64),
            ] {
                if object
                    .get(field)
                    .is_some_and(|value| !value.as_u64().is_some_and(|value| value <= maximum))
                {
                    return Err(INVALID);
                }
            }
            steam_exclusions(object.get("exclude_games").ok_or(INVALID)?)
        }
        _ => Ok(()),
    }
}

fn lutris(action: &str, object: &Object) -> Result<(), &'static str> {
    if action == "artwork_open" {
        fields(object, &["game_id", "revision"], &[])?;
        let id = object
            .get("game_id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or(INVALID)?;
        let revision = text(object, "revision")?;
        if id > i64::MAX as u64
            || revision.len() != 64
            || !revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || revision.is_empty()
            || revision.len() > 128
            || revision.chars().any(char::is_control)
        {
            return Err(INVALID);
        }
        return Ok(());
    }

    match action {
        "settings_get" | "status" | "refresh" => fields(object, &[], &[]),
        "artwork_read" => {
            fields(object, &["game_id", "revision", "offset"], &[])?;
            if !object
                .get("game_id")
                .and_then(Value::as_i64)
                .is_some_and(|game_id| game_id > 0)
                || !object
                    .get("revision")
                    .and_then(Value::as_str)
                    .is_some_and(|revision| {
                        revision.len() == 64
                            && revision
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    })
                || !object
                    .get("offset")
                    .and_then(Value::as_u64)
                    .is_some_and(|offset| offset < 16 * 1024 * 1024 && offset % 8192 == 0)
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "settings_update" => {
            fields(
                object,
                &["enabled", "expected_revision"],
                &[
                    "auto_sync",
                    "autosync_remove_uninstalled",
                    "include_steam",
                    "exclude_games",
                ],
            )?;
            if object.get("enabled").and_then(Value::as_bool).is_none()
                || object
                    .get("expected_revision")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err(INVALID);
            }
            for field in ["auto_sync", "autosync_remove_uninstalled", "include_steam"] {
                if object
                    .get(field)
                    .is_some_and(|value| value.as_bool().is_none())
                {
                    return Err(INVALID);
                }
            }
            if let Some(exclusions) = object.get("exclude_games") {
                lutris_exclusions(exclusions)?;
            }
            Ok(())
        }
        _ => Err(INVALID),
    }
}

fn lutris_exclusions(value: &Value) -> Result<(), &'static str> {
    let exclusions = value.as_array().ok_or(INVALID)?;
    if exclusions.len() > 256 {
        return Err(INVALID);
    }
    let mut unique = BTreeSet::new();
    for exclusion in exclusions {
        let identity = if let Some(id) = exclusion.as_str() {
            valid_lutris_id(id)?;
            format!("id:{id}")
        } else {
            let object = exclusion.as_object().ok_or(INVALID)?;
            fields(object, &[], &["id", "name"])?;
            let id = object
                .get("id")
                .map(|value| value.as_str().ok_or(INVALID))
                .transpose()?;
            let name = object
                .get("name")
                .map(|value| value.as_str().ok_or(INVALID))
                .transpose()?;
            match (id, name) {
                (Some(id), Some(name)) if !id.is_empty() => {
                    valid_lutris_id(id)?;
                    if !name.is_empty() {
                        valid_lutris_name(name)?;
                    }
                    format!("id:{id}")
                }
                (Some(""), Some(name)) => {
                    valid_lutris_name(name)?;
                    format!("name:{}", name.to_lowercase())
                }
                (Some(id), None) if !id.is_empty() => {
                    valid_lutris_id(id)?;
                    format!("id:{id}")
                }
                (None, Some(name)) => {
                    valid_lutris_name(name)?;
                    format!("name:{}", name.to_lowercase())
                }
                _ => return Err(INVALID),
            }
        };
        if !unique.insert(identity) {
            return Err(INVALID);
        }
    }
    Ok(())
}

fn valid_lutris_id(value: &str) -> Result<(), &'static str> {
    if value.is_empty()
        || value.len() > 512
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || value.parse::<u64>().map_or(true, |id| id == 0)
    {
        return Err(INVALID);
    }
    Ok(())
}

fn valid_lutris_name(value: &str) -> Result<(), &'static str> {
    if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(INVALID);
    }
    Ok(())
}

fn input(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "status" => fields(object, &[], &[]),
        "settings_get" => fields(object, &[], &[]),
        "settings_update" => {
            fields(
                object,
                &["keyboard", "mouse", "expected_revision"],
                &[
                    "high_resolution_scrolling",
                    "always_send_scancodes",
                    "key_rightalt_to_key_win",
                    "keybindings",
                    "key_repeat_delay",
                    "key_repeat_frequency",
                    "back_button_timeout",
                    "gamepad",
                    "controller",
                    "ds4_back_as_touchpad_click",
                    "motion_as_ds4",
                    "touchpad_as_ds4",
                    "ds5_inputtino_randomize_mac",
                    "proton_dualsense_compatibility",
                    "native_pen_touch",
                ],
            )?;
            if object.get("keyboard").and_then(Value::as_bool).is_none()
                || object.get("mouse").and_then(Value::as_bool).is_none()
                || object
                    .get("high_resolution_scrolling")
                    .is_some_and(|value| value.as_bool().is_none())
                || object
                    .get("always_send_scancodes")
                    .is_some_and(|value| value.as_bool().is_none())
                || object
                    .get("key_rightalt_to_key_win")
                    .is_some_and(|value| value.as_bool().is_none())
                || object.get("keybindings").is_some_and(|value| {
                    serde_json::from_value::<amaterasu_input_policy::KeyBindings>(value.clone())
                        .is_err()
                })
                || object
                    .get("key_repeat_delay")
                    .is_some_and(|value| value.as_u64().is_none_or(|delay| delay > i32::MAX as u64))
                || object.get("key_repeat_frequency").is_some_and(|value| {
                    value
                        .as_f64()
                        .is_none_or(|frequency| !frequency.is_finite() || frequency < 0.0)
                })
                || object.get("back_button_timeout").is_some_and(|value| {
                    value
                        .as_i64()
                        .is_none_or(|timeout| i32::try_from(timeout).is_err())
                })
                || object.get("gamepad").is_some_and(|value| {
                    value
                        .as_str()
                        .is_none_or(|profile| !valid_gamepad_profile(profile))
                })
                || object
                    .get("controller")
                    .is_some_and(|value| value.as_bool().is_none())
                || object
                    .get("ds4_back_as_touchpad_click")
                    .is_some_and(|value| value.as_bool().is_none())
                || object
                    .get("motion_as_ds4")
                    .is_some_and(|value| value.as_bool().is_none())
                || object
                    .get("touchpad_as_ds4")
                    .is_some_and(|value| value.as_bool().is_none())
                || object
                    .get("ds5_inputtino_randomize_mac")
                    .is_some_and(|value| value.as_bool().is_none())
                || object
                    .get("proton_dualsense_compatibility")
                    .is_some_and(|value| value.as_bool().is_none())
                || object
                    .get("native_pen_touch")
                    .is_some_and(|value| value.as_bool().is_none())
                || object
                    .get("expected_revision")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err(INVALID);
            }
            Ok(())
        }
        _ => Err(INVALID),
    }
}

fn valid_gamepad_profile(profile: &str) -> bool {
    #[cfg(windows)]
    {
        matches!(
            profile,
            "auto"
                | "x360"
                | "vhf"
                | "vhf_xbox"
                | "vhf_xbox_one"
                | "vhf_ds4"
                | "vhf_ds5"
                | "vhf_switch"
                | "ds4"
        )
    }
    #[cfg(target_os = "linux")]
    {
        matches!(profile, "auto" | "xone" | "ds4" | "ds5" | "switch")
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        profile == "auto"
    }
}

fn playnite(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "settings_get" | "status" | "refresh" => fields(object, &[], &[]),
        "settings_update" => {
            fields(
                object,
                &["enabled", "expected_revision"],
                &[
                    "auto_sync",
                    "autosync_remove_uninstalled",
                    "autosync_delete_after_days",
                    "autosync_require_replacement",
                    "focus_attempts",
                    "focus_timeout_secs",
                    "focus_exit_on_first",
                    "fullscreen_entry_enabled",
                    "sync_all_installed",
                    "recent_games",
                    "recent_max_age_days",
                    "sync_categories",
                    "exclude_categories",
                    "sync_plugins",
                    "exclude_plugins",
                    "exclude_games",
                ],
            )?;
            if object.get("enabled").and_then(Value::as_bool).is_none()
                || object
                    .get("expected_revision")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err(INVALID);
            }
            for field in [
                "auto_sync",
                "autosync_remove_uninstalled",
                "autosync_require_replacement",
                "focus_exit_on_first",
                "fullscreen_entry_enabled",
                "sync_all_installed",
            ] {
                if object
                    .get(field)
                    .is_some_and(|value| value.as_bool().is_none())
                {
                    return Err(INVALID);
                }
            }
            for (field, maximum) in [
                ("autosync_delete_after_days", 2_147_483_647_u64),
                ("focus_attempts", 2_147_483_647_u64),
                ("focus_timeout_secs", 2_147_483_647_u64),
                ("recent_games", 10_000_u64),
                ("recent_max_age_days", 36_500_u64),
            ] {
                if object
                    .get(field)
                    .is_some_and(|value| !value.as_u64().is_some_and(|value| value <= maximum))
                {
                    return Err(INVALID);
                }
            }
            for field in [
                "sync_categories",
                "exclude_categories",
                "sync_plugins",
                "exclude_plugins",
                "exclude_games",
            ] {
                if let Some(values) = object.get(field) {
                    playnite_filter_values(values)?;
                }
            }
            Ok(())
        }
        _ => Err(INVALID),
    }
}

fn playnite_filter_values(value: &Value) -> Result<(), &'static str> {
    let values = value.as_array().ok_or(INVALID)?;
    if values.len() > 256 {
        return Err(INVALID);
    }
    let mut unique = BTreeSet::new();
    for value in values {
        let value = value.as_str().ok_or(INVALID)?;
        if value.trim().is_empty()
            || value.len() > 512
            || value.chars().any(char::is_control)
            || !unique.insert(value.to_lowercase())
        {
            return Err(INVALID);
        }
    }
    Ok(())
}

fn catalog(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "get" => {
            fields(object, &["id"], &[])?;
            catalog_id(text(object, "id")?)
        }
        "list" => {
            fields(object, &[], &["cursor", "limit"])?;
            catalog_page_options(object)
        }
        "search" => {
            fields(object, &["query"], &["cursor", "limit"])?;
            let query = text(object, "query")?;
            if query.trim().is_empty() || query.len() > 160 || query.chars().any(char::is_control) {
                return Err(INVALID);
            }
            catalog_page_options(object)
        }
        "create" => {
            fields(object, &["entry", "expected_revision"], &[])?;
            unsigned_u64(object, "expected_revision")?;
            catalog_entry(object.get("entry").ok_or(INVALID)?)
        }
        "update" => {
            fields(object, &["id", "entry", "expected_revision"], &[])?;
            catalog_id(text(object, "id")?)?;
            unsigned_u64(object, "expected_revision")?;
            catalog_entry(object.get("entry").ok_or(INVALID)?)
        }
        "update_provider_overrides" => {
            fields(
                object,
                &["id", "expected_revision", "config_overrides"],
                &[],
            )?;
            catalog_id(text(object, "id")?)?;
            unsigned_u64(object, "expected_revision")?;
            catalog_config_overrides(object.get("config_overrides").ok_or(INVALID)?)
        }
        "delete" => {
            fields(object, &["id", "expected_revision"], &[])?;
            catalog_id(text(object, "id")?)?;
            unsigned_u64(object, "expected_revision")
        }
        _ => Err(INVALID),
    }
}

fn catalog_page_options(object: &Object) -> Result<(), &'static str> {
    if object.get("limit").is_some_and(|value| {
        !value
            .as_u64()
            .is_some_and(|limit| (1..=32).contains(&limit))
    }) {
        return Err(INVALID);
    }
    if let Some(cursor) = object.get("cursor")
        && !cursor.is_null()
    {
        let cursor = cursor.as_object().ok_or(INVALID)?;
        fields(cursor, &["revision", "after", "query"], &[])?;
        unsigned_u64(cursor, "revision")?;
        catalog_id(text(cursor, "after")?)?;
        let query = text(cursor, "query")?;
        if query.len() > 160 || query.chars().any(char::is_control) {
            return Err(INVALID);
        }
    }
    Ok(())
}

fn catalog_id(value: &str) -> Result<(), &'static str> {
    let Some(suffix) = value.strip_prefix("app-") else {
        return Err(INVALID);
    };
    if suffix.len() != 16
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || suffix.bytes().all(|byte| byte == b'0')
    {
        return Err(INVALID);
    }
    Ok(())
}

fn catalog_entry(value: &Value) -> Result<(), &'static str> {
    let entry = value.as_object().ok_or(INVALID)?;
    fields(
        entry,
        &["name", "launch"],
        &[
            "description",
            "tags",
            "config_overrides",
            "display-output",
            "display_output",
            "virtual-screen",
            "virtual_screen",
            "virtual-display-mode",
            "virtual_display_mode",
            "virtual-display-layout",
            "virtual_display_layout",
            "steam_id",
            "steam_install_dir",
            "exclude_global_prep_cmd",
            "exclude-global-prep-cmd",
            "elevated",
            "output",
            "prep_cmd",
            "prep-cmd",
        ],
    )?;
    optional_string(entry, "display-output", "display_output")?;
    optional_boolean(entry, "virtual-screen", "virtual_screen")?;
    optional_string(entry, "virtual-display-mode", "virtual_display_mode")?;
    optional_string(entry, "virtual-display-layout", "virtual_display_layout")?;
    if entry
        .get("display-output")
        .or_else(|| entry.get("display_output"))
        .and_then(Value::as_str)
        .is_some_and(|value| value.len() > 1024 || value.chars().any(char::is_control))
        || entry
            .get("virtual-display-mode")
            .or_else(|| entry.get("virtual_display_mode"))
            .and_then(Value::as_str)
            .is_some_and(|value| !matches!(value, "disabled" | "per_client" | "shared"))
        || entry
            .get("virtual-display-layout")
            .or_else(|| entry.get("virtual_display_layout"))
            .and_then(Value::as_str)
            .is_some_and(|value| {
                !matches!(
                    value,
                    "exclusive"
                        | "extended"
                        | "extended_primary"
                        | "extended_isolated"
                        | "extended_primary_isolated"
                )
            })
    {
        return Err(INVALID);
    }
    let processless = entry
        .get("launch")
        .and_then(Value::as_object)
        .and_then(|launch| launch.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "desktop" | "playnite_fullscreen"));
    if processless
        && ["steam_id", "steam_install_dir"]
            .into_iter()
            .any(|field| entry.get(field).is_some_and(|value| !value.is_null()))
    {
        return Err(INVALID);
    }
    if let Some(output) = entry.get("output") {
        match output {
            Value::Null => {}
            Value::String(path) if path.len() <= 1024 && !path.chars().any(char::is_control) => {}
            _ => return Err(INVALID),
        }
    }
    if entry.contains_key("prep_cmd") && entry.contains_key("prep-cmd") {
        return Err(INVALID);
    }
    let raw_preparation_count = entry
        .get("prep_cmd")
        .or_else(|| entry.get("prep-cmd"))
        .map(catalog_raw_preparation)
        .transpose()?
        .unwrap_or(0);
    if entry
        .get("elevated")
        .is_some_and(|value| value.as_bool().is_none())
        || (entry.get("elevated").and_then(Value::as_bool) == Some(true)
            && entry
                .get("launch")
                .and_then(Value::as_object)
                .and_then(|launch| launch.get("kind"))
                .and_then(Value::as_str)
                != Some("executable"))
    {
        return Err(INVALID);
    }
    if entry.contains_key("exclude_global_prep_cmd")
        && entry.contains_key("exclude-global-prep-cmd")
    {
        return Err(INVALID);
    }
    if entry
        .get("exclude_global_prep_cmd")
        .or_else(|| entry.get("exclude-global-prep-cmd"))
        .is_some_and(|value| value.as_bool().is_none())
    {
        return Err(INVALID);
    }
    let name = text(entry, "name")?;
    if name.trim().is_empty() || name.len() > 160 || name.chars().any(char::is_control) {
        return Err(INVALID);
    }
    if let Some(description) = entry.get("description") {
        let description = description.as_str().ok_or(INVALID)?;
        if description.len() > 1024 || description.chars().any(char::is_control) {
            return Err(INVALID);
        }
    }
    if let Some(tags) = entry.get("tags") {
        let tags = tags.as_array().ok_or(INVALID)?;
        if tags.len() > 16
            || tags.iter().any(|tag| {
                tag.as_str().is_none_or(|tag| {
                    tag.trim().is_empty() || tag.len() > 64 || tag.chars().any(char::is_control)
                })
            })
        {
            return Err(INVALID);
        }
    }
    if let Some(overrides) = entry.get("config_overrides") {
        catalog_config_overrides(overrides)?;
    }
    if let Some(steam_id) = entry.get("steam_id") {
        let steam_id = steam_id.as_str().ok_or(INVALID)?;
        let app_id = steam_id.parse::<u32>().map_err(|_| INVALID)?;
        if app_id == 0 || app_id.to_string() != steam_id {
            return Err(INVALID);
        }
    }
    if let Some(install_dir) = entry.get("steam_install_dir") {
        let install_dir = install_dir.as_str().ok_or(INVALID)?;
        if !valid_steam_install_dir(install_dir) {
            return Err(INVALID);
        }
    }
    catalog_launch(entry.get("launch").ok_or(INVALID)?, raw_preparation_count)
}

fn catalog_raw_preparation(value: &Value) -> Result<usize, &'static str> {
    let steps = value.as_array().ok_or(INVALID)?;
    if steps.len() > 8 {
        return Err(INVALID);
    }
    for step in steps {
        let step = step.as_object().ok_or(INVALID)?;
        fields(step, &[], &["do", "undo", "elevated"])?;
        for key in ["do", "undo"] {
            if step.get(key).is_some_and(|value| {
                value
                    .as_str()
                    .is_none_or(|command| command.len() > 2048 || command.contains('\0'))
            }) {
                return Err(INVALID);
            }
        }
        if step
            .get("elevated")
            .is_some_and(|value| value.as_bool().is_none())
        {
            return Err(INVALID);
        }
    }
    Ok(steps.len())
}

fn valid_steam_install_dir(path: &str) -> bool {
    if path.is_empty()
        || path.len() > 1024
        || path.trim() != path
        || path.chars().any(char::is_control)
    {
        return false;
    }
    let normalized = path.replace('\\', "/");
    let (segments, minimum) = if let Some(unc) = normalized.strip_prefix("//") {
        (unc, 3)
    } else if normalized.len() >= 3
        && normalized.as_bytes()[0].is_ascii_alphabetic()
        && normalized.as_bytes()[1] == b':'
        && normalized.as_bytes()[2] == b'/'
    {
        (&normalized[3..], 1)
    } else if path.starts_with('/') {
        (&normalized[1..], 1)
    } else {
        return false;
    };
    let segments: Vec<_> = segments
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    segments.len() >= minimum
        && segments
            .iter()
            .all(|segment| *segment != "." && *segment != "..")
}

fn catalog_config_overrides(value: &Value) -> Result<(), &'static str> {
    let overrides = value.as_object().ok_or(INVALID)?;
    optional_software_encoder_options(overrides)?;
    fields(
        overrides,
        &[],
        &[
            "fec_percentage",
            "nvenc_preset",
            "nvenc_h264_cavlc",
            "nvenc_twopass",
            "nvenc_vbv_increase",
            "nvenc_split_encode",
            "nvenc_force_split_encode",
            "nvenc_realtime_hags",
            "nvenc_opengl_vulkan_on_dxgi",
            "nvenc_latency_over_power",
            "vaapi_strict_rc_buffer",
            "sw_preset",
            "sw_tune",
            "encoder",
            "min_threads",
            "qp",
            "stream_audio",
            "audio_sink",
            "virtual_sink",
            "audio_sink_capture_only",
            "back_button_timeout",
            "controller",
            "gamepad",
            "keyboard",
            "mouse",
            "native_pen_touch",
            "high_resolution_scrolling",
            "always_send_scancodes",
            "key_rightalt_to_key_win",
            "keybindings",
            "key_repeat_delay",
            "key_repeat_frequency",
            "frame_limiter_enable",
            "frame_limiter_fps_limit",
            "frame_limiter_provider",
            "frame_limiter_disable_vsync",
            "frame_limiter_auto_virtual_framegen",
            "lossless_scaling_framegen",
            "lossless_scaling_target_fps",
            "frame_generation_provider",
            "frame_generation_mode",
            "rtss_frame_limit_type",
            "ds4_back_as_touchpad_click",
            "motion_as_ds4",
            "touchpad_as_ds4",
            "ds5_inputtino_randomize_mac",
            "proton_dualsense_compatibility",
        ],
    )?;
    if let Some(fec) = overrides.get("fec_percentage")
        && !fec.is_null()
        && !fec
            .as_u64()
            .is_some_and(|value| (1..=u8::MAX as u64).contains(&value))
    {
        return Err(INVALID);
    }
    if overrides
        .get("stream_audio")
        .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
    {
        return Err(INVALID);
    }
    optional_audio_sink(overrides)?;
    optional_virtual_sink(overrides)?;
    optional_audio_sink_capture_only(overrides)?;
    optional_back_button_timeout(overrides)?;
    optional_input_overrides(overrides)?;
    optional_nvenc_settings(overrides)?;
    optional_catalog_frame_limiter_overrides(overrides)?;
    Ok(())
}

fn optional_nvenc_settings(overrides: &Object) -> Result<(), &'static str> {
    if overrides.get("nvenc_preset").is_some_and(|value| {
        !value.is_null()
            && !value
                .as_u64()
                .is_some_and(|preset| (1..=7).contains(&preset))
    }) || overrides.get("nvenc_vbv_increase").is_some_and(|value| {
        !value.is_null() && !value.as_u64().is_some_and(|increase| increase <= 400)
    }) || overrides.get("nvenc_twopass").is_some_and(|value| {
        !value.is_null()
            && !matches!(
                value.as_str(),
                Some("disabled" | "quarter_res" | "full_res")
            )
    }) || overrides
        .get("nvenc_split_encode")
        .or_else(|| overrides.get("nvenc_force_split_encode"))
        .is_some_and(|value| {
            !value.is_null()
                && !matches!(
                    value.as_str(),
                    Some("auto" | "enabled" | "disabled" | "driver_decides")
                )
        })
        || (overrides.contains_key("nvenc_split_encode")
            && overrides.contains_key("nvenc_force_split_encode"))
    {
        return Err(INVALID);
    }
    for field in [
        "nvenc_h264_cavlc",
        "nvenc_realtime_hags",
        "nvenc_opengl_vulkan_on_dxgi",
        "nvenc_latency_over_power",
        "vaapi_strict_rc_buffer",
        "nvenc_spatial_aq",
    ] {
        if overrides
            .get(field)
            .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
        {
            return Err(INVALID);
        }
    }
    Ok(())
}

fn optional_catalog_frame_limiter_overrides(overrides: &Object) -> Result<(), &'static str> {
    for key in [
        "frame_limiter_enable",
        "frame_limiter_disable_vsync",
        "ds4_back_as_touchpad_click",
        "lossless_scaling_framegen",
        "motion_as_ds4",
        "touchpad_as_ds4",
        "ds5_inputtino_randomize_mac",
        "proton_dualsense_compatibility",
    ] {
        if overrides
            .get(key)
            .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
        {
            return Err(INVALID);
        }
    }

    if overrides
        .get("lossless_scaling_target_fps")
        .is_some_and(|value| {
            !value.is_null() && !value.as_u64().is_some_and(|fps| (1..=1000).contains(&fps))
        })
    {
        return Err(INVALID);
    }

    if overrides
        .get("frame_limiter_fps_limit")
        .is_some_and(|value| {
            !value.is_null()
                && !value
                    .as_f64()
                    .is_some_and(valid_catalog_frame_limiter_fps_limit)
        })
        || overrides
            .get("frame_limiter_provider")
            .is_some_and(|value| {
                !value.is_null()
                    && value
                        .as_str()
                        .is_none_or(|provider| !valid_catalog_frame_limiter_provider(provider))
            })
        || overrides
            .get("frame_limiter_auto_virtual_framegen")
            .is_some_and(|value| {
                !value.is_null()
                    && match value {
                        Value::Bool(_) => false,
                        Value::String(mode) => !matches!(
                            mode.as_str(),
                            "true" | "false" | "enabled" | "vrr" | "legacy" | "disabled"
                        ),
                        _ => true,
                    }
            })
        || overrides
            .get("frame_generation_provider")
            .is_some_and(|value| {
                !value.is_null()
                    && value.as_str().is_none_or(|provider| {
                        !matches!(
                            provider,
                            "lossless-scaling" | "game-provided" | "nvidia-smooth-motion"
                        )
                    })
            })
        || overrides.get("frame_generation_mode").is_some_and(|value| {
            !value.is_null()
                && value.as_str().is_none_or(|mode| {
                    !matches!(
                        mode,
                        "off" | "lossless-scaling" | "game-provided" | "nvidia-smooth-motion"
                    )
                })
        })
        || overrides.get("rtss_frame_limit_type").is_some_and(|value| {
            !value.is_null()
                && (!cfg!(windows)
                    || value.as_str().is_none_or(|limit_type| {
                        !matches!(
                            limit_type,
                            "async" | "front edge sync" | "back edge sync" | "nvidia reflex"
                        )
                    }))
        })
    {
        return Err(INVALID);
    }
    Ok(())
}

fn valid_catalog_frame_limiter_fps_limit(value: f64) -> bool {
    if !value.is_finite() || !(0.0..=1000.0).contains(&value) {
        return false;
    }
    let millihertz = value * 1000.0;
    let rounding_tolerance = f64::EPSILON * millihertz.abs().max(1.0) * 4.0;
    (millihertz - millihertz.round()).abs() <= rounding_tolerance
}

fn valid_catalog_frame_limiter_provider(provider: &str) -> bool {
    #[cfg(windows)]
    {
        matches!(provider, "auto" | "rtss" | "nvidia-control-panel" | "none")
    }
    #[cfg(target_os = "linux")]
    {
        matches!(
            provider,
            "auto" | "mangohud" | "proton" | "mangohud-proton" | "none"
        )
    }
    #[cfg(target_os = "macos")]
    {
        provider == "auto"
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        provider == "auto"
    }
}

fn optional_audio_sink(overrides: &Object) -> Result<(), &'static str> {
    if overrides
        .get("audio_sink")
        .is_some_and(|value| match value {
            Value::Null => false,
            Value::String(endpoint) => {
                endpoint.len() > 4096 || endpoint.chars().any(char::is_control)
            }
            _ => true,
        })
    {
        return Err(INVALID);
    }
    Ok(())
}

fn optional_virtual_sink(overrides: &Object) -> Result<(), &'static str> {
    if overrides
        .get("virtual_sink")
        .is_some_and(|value| match value {
            Value::Null => false,
            Value::String(selector) => {
                selector.len() > 4096 || selector.chars().any(char::is_control)
            }
            _ => true,
        })
    {
        return Err(INVALID);
    }
    Ok(())
}

fn optional_audio_sink_capture_only(overrides: &Object) -> Result<(), &'static str> {
    if overrides
        .get("audio_sink_capture_only")
        .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
    {
        return Err(INVALID);
    }
    Ok(())
}

fn optional_back_button_timeout(overrides: &Object) -> Result<(), &'static str> {
    if overrides.get("back_button_timeout").is_some_and(|value| {
        !value.is_null()
            && !value
                .as_i64()
                .is_some_and(|timeout| i32::try_from(timeout).is_ok())
    }) {
        return Err(INVALID);
    }
    Ok(())
}

fn optional_input_overrides(overrides: &Object) -> Result<(), &'static str> {
    if overrides.get("keybindings").is_some_and(|value| {
        !value.is_null()
            && serde_json::from_value::<amaterasu_input_policy::KeyBindings>(value.clone()).is_err()
    }) {
        return Err(INVALID);
    }
    for key in [
        "controller",
        "keyboard",
        "mouse",
        "native_pen_touch",
        "ds4_back_as_touchpad_click",
        "motion_as_ds4",
        "touchpad_as_ds4",
        "ds5_inputtino_randomize_mac",
        "proton_dualsense_compatibility",
        "high_resolution_scrolling",
        "always_send_scancodes",
        "key_rightalt_to_key_win",
    ] {
        if overrides
            .get(key)
            .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
        {
            return Err(INVALID);
        }
    }
    if overrides.get("key_repeat_delay").is_some_and(|value| {
        !value.is_null() && !value.as_u64().is_some_and(|delay| delay <= i32::MAX as u64)
    }) || overrides.get("key_repeat_frequency").is_some_and(|value| {
        !value.is_null()
            && !value
                .as_f64()
                .is_some_and(|frequency| frequency.is_finite() && frequency >= 0.0)
    }) {
        return Err(INVALID);
    }
    if overrides.get("gamepad").is_some_and(|value| {
        !value.is_null()
            && value
                .as_str()
                .is_none_or(|profile| !valid_gamepad_profile(profile))
    }) {
        return Err(INVALID);
    }
    Ok(())
}

fn catalog_launch(value: &Value, raw_preparation_count: usize) -> Result<(), &'static str> {
    let launch = value.as_object().ok_or(INVALID)?;
    match text(launch, "kind")? {
        "application" => {
            fields(launch, &["kind", "path"], &[])?;
            text(launch, "path")?;
        }
        "playnite_fullscreen" | "desktop" => {
            fields(
                launch,
                &["kind"],
                &[
                    "exit_timeout_seconds",
                    "working_directory",
                    "working-dir",
                    "preparation",
                ],
            )?;
            if launch
                .get("exit_timeout_seconds")
                .is_some_and(|value| !value.as_u64().is_some_and(|timeout| timeout <= 300))
            {
                return Err(INVALID);
            }
            optional_string(launch, "working_directory", "working-dir")?;
            if let Some(preparation) = launch.get("preparation") {
                catalog_preparation(preparation)?;
                if preparation.as_array().ok_or(INVALID)?.len() + raw_preparation_count > 8 {
                    return Err(INVALID);
                }
            }
        }
        "executable" => {
            fields(
                launch,
                &["kind", "program"],
                &[
                    "arguments",
                    "working_directory",
                    "working-dir",
                    "auto_detach",
                    "auto-detach",
                    "wait_all",
                    "wait-all",
                    "exit_timeout_seconds",
                    "exit-timeout",
                    "detached",
                    "preparation",
                ],
            )?;
            text(launch, "program")?;
            optional_string(launch, "working_directory", "working-dir")?;
            optional_boolean(launch, "auto_detach", "auto-detach")?;
            optional_boolean(launch, "wait_all", "wait-all")?;
            optional_timeout(launch, "exit_timeout_seconds", "exit-timeout")?;
            if let Some(arguments) = launch.get("arguments") {
                string_array(arguments, 32, 1024, true)?;
            }
            if let Some(detached) = launch.get("detached") {
                string_array(detached, 16, 2048, false)?;
            }
            if let Some(preparation) = launch.get("preparation") {
                catalog_preparation(preparation)?;
                let structured_count = preparation.as_array().ok_or(INVALID)?.len();
                if structured_count + raw_preparation_count > 8 {
                    return Err(INVALID);
                }
            } else if raw_preparation_count > 8 {
                return Err(INVALID);
            }
        }
        // Playnite identities can only be written by the trusted provider.
        _ => return Err(INVALID),
    }
    Ok(())
}

fn optional_string(object: &Object, canonical: &str, alias: &str) -> Result<(), &'static str> {
    if object.contains_key(canonical) && object.contains_key(alias) {
        return Err(INVALID);
    }
    if object
        .get(canonical)
        .or_else(|| object.get(alias))
        .is_some_and(|value| !value.is_null() && value.as_str().is_none())
    {
        return Err(INVALID);
    }
    Ok(())
}

fn optional_boolean(object: &Object, canonical: &str, alias: &str) -> Result<(), &'static str> {
    if object.contains_key(canonical) && object.contains_key(alias) {
        return Err(INVALID);
    }
    if object
        .get(canonical)
        .or_else(|| object.get(alias))
        .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
    {
        return Err(INVALID);
    }
    Ok(())
}

fn optional_nullable_boolean(object: &Object, key: &str) -> Result<(), &'static str> {
    if object
        .get(key)
        .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
    {
        return Err(INVALID);
    }
    Ok(())
}

fn optional_timeout(object: &Object, canonical: &str, alias: &str) -> Result<(), &'static str> {
    if object.contains_key(canonical) && object.contains_key(alias) {
        return Err(INVALID);
    }
    if object
        .get(canonical)
        .or_else(|| object.get(alias))
        .is_some_and(|value| {
            !value.is_null() && !value.as_u64().is_some_and(|timeout| timeout <= 300)
        })
    {
        return Err(INVALID);
    }
    Ok(())
}

fn string_array(
    value: &Value,
    maximum_items: usize,
    maximum_bytes: usize,
    allow_empty: bool,
) -> Result<(), &'static str> {
    let values = value.as_array().ok_or(INVALID)?;
    if values.len() > maximum_items
        || values.iter().any(|value| {
            value.as_str().is_none_or(|value| {
                (!allow_empty && value.is_empty())
                    || value.len() > maximum_bytes
                    || value.chars().any(char::is_control)
            })
        })
    {
        return Err(INVALID);
    }
    Ok(())
}

fn catalog_preparation(value: &Value) -> Result<(), &'static str> {
    let steps = value.as_array().ok_or(INVALID)?;
    if steps.len() > 8 {
        return Err(INVALID);
    }
    for step in steps {
        let step = step.as_object().ok_or(INVALID)?;
        fields(step, &["run"], &["undo"])?;
        catalog_command(step.get("run").ok_or(INVALID)?)?;
        if let Some(undo) = step.get("undo")
            && !undo.is_null()
        {
            catalog_command(undo)?;
        }
    }
    Ok(())
}

fn global_preparation(value: &Value) -> Result<(), &'static str> {
    if value.is_null() {
        return Ok(());
    }
    catalog_preparation(value)?;
    let steps = value.as_array().ok_or(INVALID)?;
    for step in steps {
        let step = step.as_object().ok_or(INVALID)?;
        validate_global_preparation_command(step.get("run").ok_or(INVALID)?)?;
        if let Some(undo) = step.get("undo")
            && !undo.is_null()
        {
            validate_global_preparation_command(undo)?;
        }
    }
    Ok(())
}

fn validate_global_preparation_command(value: &Value) -> Result<(), &'static str> {
    let command = value.as_object().ok_or(INVALID)?;
    let executable = text(command, "executable")?;
    let executable_path = Path::new(executable);
    if executable.is_empty()
        || executable.len() > 1024
        || executable.contains('\0')
        || !executable_path.is_absolute()
        || executable_path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
            })
    {
        return Err(INVALID);
    }
    if let Some(cwd) = command.get("cwd").filter(|cwd| !cwd.is_null()) {
        let cwd = cwd.as_str().ok_or(INVALID)?;
        if cwd.is_empty() || cwd.len() > 1024 || cwd.contains('\0') || !Path::new(cwd).is_absolute()
        {
            return Err(INVALID);
        }
    }
    Ok(())
}

fn catalog_command(value: &Value) -> Result<(), &'static str> {
    let command = value.as_object().ok_or(INVALID)?;
    fields(
        command,
        &["executable"],
        &["args", "environment", "cwd", "timeout_ms"],
    )?;
    text(command, "executable")?;
    if let Some(args) = command.get("args") {
        command_arguments(args)?;
    }
    if command
        .get("cwd")
        .is_some_and(|value| !value.is_null() && value.as_str().is_none())
    {
        return Err(INVALID);
    }
    if let Some(environment) = command.get("environment") {
        let environment = environment.as_object().ok_or(INVALID)?;
        let mut folded_names = BTreeSet::new();
        if environment.len() > 32
            || environment.iter().any(|(key, value)| {
                let invalid_key = key.is_empty()
                    || key.len() > 128
                    || !key
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
                invalid_key
                    || value
                        .as_str()
                        .is_none_or(|value| value.len() > 1024 || value.contains('\0'))
                    || !folded_names.insert(key.to_ascii_uppercase())
            })
            || environment
                .iter()
                .map(|(key, value)| key.len() + value.as_str().map_or(0, str::len))
                .sum::<usize>()
                > 4096
        {
            return Err(INVALID);
        }
    }
    if command.get("timeout_ms").is_some_and(|value| {
        !value
            .as_u64()
            .is_some_and(|timeout| (1..=30_000).contains(&timeout))
    }) {
        return Err(INVALID);
    }
    Ok(())
}

fn command_arguments(value: &Value) -> Result<(), &'static str> {
    let arguments = value.as_array().ok_or(INVALID)?;
    if arguments.len() > 64
        || arguments.iter().any(|argument| {
            argument
                .as_str()
                .is_none_or(|argument| argument.len() > 1024 || argument.contains('\0'))
        })
        || arguments
            .iter()
            .filter_map(Value::as_str)
            .map(str::len)
            .sum::<usize>()
            > 8192
    {
        return Err(INVALID);
    }
    Ok(())
}

fn steam_exclusions(value: &Value) -> Result<(), &'static str> {
    let exclusions = value.as_array().ok_or(INVALID)?;
    if exclusions.len() > 256 {
        return Err(INVALID);
    }

    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for value in exclusions {
        let exclusion = value.as_object().ok_or(INVALID)?;
        if exclusion
            .keys()
            .any(|key| !matches!(key.as_str(), "id" | "name"))
        {
            return Err(INVALID);
        }
        let id = exclusion
            .get("id")
            .map(|value| value.as_str().ok_or(INVALID))
            .transpose()?;
        let name = exclusion
            .get("name")
            .map(|value| value.as_str().ok_or(INVALID))
            .transpose()?;
        if id.is_none() && name.is_none() {
            return Err(INVALID);
        }
        if let Some(id) = id {
            let parsed = id.parse::<u32>().ok().filter(|id| *id != 0);
            if parsed.is_none_or(|parsed| parsed.to_string() != id) || !ids.insert(id) {
                return Err(INVALID);
            }
        }
        if let Some(name) = name {
            if name.trim().is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
                return Err(INVALID);
            }
            if id.is_none() && !names.insert(name.to_lowercase()) {
                return Err(INVALID);
            }
        }
    }
    Ok(())
}

fn management_launch_identifier(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn nvhttp(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "management_launch_status" => {
            fields(object, &[], &["reference"])?;
            if object
                .get("reference")
                .is_some_and(|value| !management_launch_identifier(value))
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "management_stop_launch" => {
            fields(object, &["reference", "revision"], &[])?;
            if ["reference", "revision"]
                .iter()
                .any(|key| !object.get(*key).is_some_and(management_launch_identifier))
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "settings_get" => fields(object, &[], &[]),
        "settings_update" => {
            fields(
                object,
                &["hostname", "port", "expected_revision"],
                &[
                    "address_family",
                    "bind_address",
                    "output_name",
                    "stream_audio",
                    "capture",
                    "sw_preset",
                    "sw_tune",
                    "encoder",
                    "min_threads",
                    "qp",
                    "video_max_batch_size_kb",
                    "max_bitrate",
                    "minimum_fps_target",
                    "audio_sink",
                    "virtual_sink",
                    "audio_sink_capture_only",
                    "install_steam_audio_drivers",
                    "external_ip",
                    "wgc_pacing_smoothing",
                    "nvenc_spatial_aq",
                    "nvenc_preset",
                    "nvenc_h264_cavlc",
                    "nvenc_twopass",
                    "nvenc_vbv_increase",
                    "nvenc_split_encode",
                    "nvenc_force_split_encode",
                    "nvenc_realtime_hags",
                    "nvenc_opengl_vulkan_on_dxgi",
                    "nvenc_latency_over_power",
                    "vaapi_strict_rc_buffer",
                    "ping_timeout_ms",
                    "global_preparation",
                    "lan_encryption_mode",
                    "wan_encryption_mode",
                    "upnp",
                ],
            )?;
            let hostname = text(object, "hostname")?;
            let port = object.get("port").and_then(Value::as_u64);
            if hostname.len() > 128
                || hostname.chars().any(char::is_control)
                || !port.is_some_and(|port| (1029..=65514).contains(&port))
                || object
                    .get("expected_revision")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err(INVALID);
            }
            if let Some(address_family) = object.get("address_family") {
                match address_family {
                    Value::Null => {}
                    Value::String(value) if matches!(value.as_str(), "ipv4" | "both") => {}
                    _ => return Err(INVALID),
                }
            }
            if let Some(bind_address) = object.get("bind_address") {
                match bind_address {
                    Value::Null => {}
                    Value::String(value) if value.is_empty() => {}
                    Value::String(value) => {
                        let address = safe_bind_address(value).ok_or(INVALID)?;
                        if object.get("address_family").and_then(Value::as_str) == Some("ipv4")
                            && !address.is_ipv4()
                        {
                            return Err(INVALID);
                        }
                    }
                    _ => return Err(INVALID),
                }
            }
            if let Some(output_name) = object.get("output_name") {
                match output_name {
                    Value::Null => {}
                    Value::String(value)
                        if value.len() <= 512
                            && value.trim() == value
                            && !value.chars().any(char::is_control) => {}
                    _ => return Err(INVALID),
                }
            }
            if let Some(virtual_sink) = object.get("virtual_sink") {
                match virtual_sink {
                    Value::Null => {}
                    Value::String(value)
                        if value.len() <= 4096 && !value.chars().any(char::is_control) => {}
                    _ => return Err(INVALID),
                }
            }
            if object
                .get("stream_audio")
                .is_some_and(|value| value.as_bool().is_none())
            {
                return Err(INVALID);
            }
            if object
                .get("upnp")
                .is_some_and(|value| value.as_bool().is_none())
            {
                return Err(INVALID);
            }
            if let Some(capture) = object.get("capture") {
                match capture {
                    Value::Null => {}
                    Value::String(value)
                        if matches!(value.as_str(), "auto" | "ddx" | "wgc" | "wgcc") => {}
                    _ => return Err(INVALID),
                }
            }
            if object.get("video_max_batch_size_kb").is_some_and(|value| {
                !value.is_null() && !value.as_u64().is_some_and(|size| size <= 64)
            }) {
                return Err(INVALID);
            }
            if object.get("max_bitrate").is_some_and(|value| {
                !value.is_null()
                    && !value
                        .as_i64()
                        .is_some_and(|bitrate| i32::try_from(bitrate).is_ok())
            }) {
                return Err(INVALID);
            }
            if object.get("minimum_fps_target").is_some_and(|value| {
                !value.is_null()
                    && !value.as_f64().is_some_and(|target| {
                        target.is_finite() && (0.0..=1000.0).contains(&target)
                    })
            }) {
                return Err(INVALID);
            }
            if object.get("audio_sink").is_some_and(|value| match value {
                Value::Null => false,
                Value::String(endpoint) => {
                    endpoint.len() > 4096 || endpoint.chars().any(char::is_control)
                }
                _ => true,
            }) {
                return Err(INVALID);
            }
            if object
                .get("audio_sink_capture_only")
                .is_some_and(|value| value.as_bool().is_none())
            {
                return Err(INVALID);
            }
            if object
                .get("install_steam_audio_drivers")
                .is_some_and(|value| value.as_bool().is_none())
            {
                return Err(INVALID);
            }
            if object.get("external_ip").is_some_and(|value| match value {
                Value::Null => false,
                Value::String(address) => {
                    address.len() > 4096 || address.chars().any(char::is_control)
                }
                _ => true,
            }) {
                return Err(INVALID);
            }
            optional_software_encoder_options(object)?;
            optional_nullable_boolean(object, "wgc_pacing_smoothing")?;
            optional_nullable_boolean(object, "nvenc_spatial_aq")?;
            if object.get("nvenc_preset").is_some_and(|value| {
                !value.is_null()
                    && !value
                        .as_u64()
                        .is_some_and(|preset| (1..=7).contains(&preset))
            }) || object.get("nvenc_vbv_increase").is_some_and(|value| {
                !value.is_null() && !value.as_u64().is_some_and(|increase| increase <= 400)
            }) || object.get("nvenc_twopass").is_some_and(|value| {
                !value.is_null()
                    && !matches!(
                        value.as_str(),
                        Some("disabled" | "quarter_res" | "full_res")
                    )
            }) || object
                .get("nvenc_split_encode")
                .or_else(|| object.get("nvenc_force_split_encode"))
                .is_some_and(|value| {
                    !value.is_null()
                        && !matches!(
                            value.as_str(),
                            Some("auto" | "enabled" | "disabled" | "driver_decides")
                        )
                })
                || (object.contains_key("nvenc_split_encode")
                    && object.contains_key("nvenc_force_split_encode"))
            {
                return Err(INVALID);
            }
            for field in [
                "nvenc_h264_cavlc",
                "nvenc_realtime_hags",
                "nvenc_opengl_vulkan_on_dxgi",
                "nvenc_latency_over_power",
                "vaapi_strict_rc_buffer",
            ] {
                optional_nullable_boolean(object, field)?;
            }
            for field in ["lan_encryption_mode", "wan_encryption_mode"] {
                if object.get(field).is_some_and(|value| {
                    !value.is_null() && !value.as_u64().is_some_and(|mode| mode <= 2)
                }) {
                    return Err(INVALID);
                }
            }
            if object.get("ping_timeout_ms").is_some_and(|value| {
                !value.is_null()
                    && !value
                        .as_i64()
                        .is_some_and(|timeout| (-1..=i32::MAX as i64).contains(&timeout))
            }) {
                return Err(INVALID);
            }
            if let Some(preparation) = object.get("global_preparation") {
                global_preparation(preparation)?;
            }
            Ok(())
        }
        _ => Err(INVALID),
    }
}

#[cfg(test)]
mod nvhttp_tests {
    use super::*;

    #[test]
    fn video_encryption_modes_can_be_set_through_authenticated_nvhttp_settings() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        for value in [
            Value::Null,
            serde_json::json!(0),
            serde_json::json!(1),
            serde_json::json!(2),
        ] {
            let body = serde_json::json!({"hostname":"Ama","port":48050,
                "expected_revision":1,"lan_encryption_mode":value,"wan_encryption_mode":value});
            assert!(validate(route, body.as_object().unwrap()).is_ok(), "{body}");
        }
        for value in [
            serde_json::json!(-1),
            serde_json::json!(3),
            serde_json::json!("1"),
            serde_json::json!(true),
        ] {
            let body = serde_json::json!({"hostname":"Ama","port":48050,
                "expected_revision":1,"lan_encryption_mode":value});
            assert!(
                validate(route, body.as_object().unwrap()).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn upnp_update_accepts_explicit_booleans_only() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        for value in [serde_json::json!(true), serde_json::json!(false)] {
            let payload = serde_json::json!({
                "hostname":"Host",
                "port":47989,
                "expected_revision":0,
                "upnp":value,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "{payload}"
            );
        }
        for value in [Value::Null, serde_json::json!(1), serde_json::json!("true")] {
            let payload = serde_json::json!({
                "hostname":"Host",
                "port":47989,
                "expected_revision":0,
                "upnp":value,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn nvenc_spatial_aq_update_accepts_optional_nullable_booleans_only() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        let required_fields = serde_json::json!({
            "hostname": "Host",
            "port": 47989,
            "expected_revision": 0,
        });
        assert!(validate(route, required_fields.as_object().unwrap()).is_ok());

        for value in [
            Value::Null,
            serde_json::json!(true),
            serde_json::json!(false),
        ] {
            let payload = serde_json::json!({
                "hostname": "Host",
                "port": 47989,
                "expected_revision": 0,
                "nvenc_spatial_aq": value,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "{payload}"
            );
        }

        for value in [
            serde_json::json!(1),
            serde_json::json!("true"),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            let payload = serde_json::json!({
                "hostname": "Host",
                "port": 47989,
                "expected_revision": 0,
                "nvenc_spatial_aq": value,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn nvenc_settings_update_accepts_bounded_reference_values_only() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        let base = serde_json::json!({
            "hostname":"Host", "port":47989, "expected_revision":0,
        });
        for overrides in [
            serde_json::json!({"nvenc_preset":1,"nvenc_twopass":"disabled","nvenc_vbv_increase":0,"nvenc_split_encode":"auto"}),
            serde_json::json!({"nvenc_preset":7,"nvenc_h264_cavlc":true,"nvenc_twopass":"full_res","nvenc_vbv_increase":400,"nvenc_split_encode":"disabled","nvenc_realtime_hags":false,"nvenc_opengl_vulkan_on_dxgi":true,"nvenc_latency_over_power":false,"vaapi_strict_rc_buffer":true}),
            serde_json::json!({"nvenc_force_split_encode":"driver_decides"}),
        ] {
            let mut payload = base.as_object().unwrap().clone();
            payload.extend(overrides.as_object().unwrap().clone());
            assert!(validate(route, &payload).is_ok(), "{payload:?}");
        }
        for overrides in [
            serde_json::json!({"nvenc_preset":0}),
            serde_json::json!({"nvenc_preset":8}),
            serde_json::json!({"nvenc_vbv_increase":401}),
            serde_json::json!({"nvenc_vbv_increase":-1}),
            serde_json::json!({"nvenc_twopass":"quarter"}),
            serde_json::json!({"nvenc_split_encode":"yes"}),
            serde_json::json!({"nvenc_h264_cavlc":"true"}),
            serde_json::json!({"nvenc_split_encode":"auto","nvenc_force_split_encode":"enabled"}),
        ] {
            let mut payload = base.as_object().unwrap().clone();
            payload.extend(overrides.as_object().unwrap().clone());
            assert!(validate(route, &payload).is_err(), "{payload:?}");
        }
    }

    #[test]
    fn ping_timeout_update_accepts_reset_and_reference_range() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        for timeout in [
            Value::Null,
            serde_json::json!(-1),
            serde_json::json!(0),
            serde_json::json!(10_000),
            serde_json::json!(i32::MAX),
        ] {
            let payload = serde_json::json!({
                "hostname":"Host", "port":47989, "expected_revision":0,
                "ping_timeout_ms":timeout,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "{payload}"
            );
        }
        for timeout in [
            serde_json::json!(-2),
            serde_json::json!(i32::MAX as i64 + 1),
            serde_json::json!("10000"),
            serde_json::json!(1.5),
        ] {
            let payload = serde_json::json!({
                "hostname":"Host", "port":47989, "expected_revision":0,
                "ping_timeout_ms":timeout,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn minimum_fps_target_update_accepts_null_zero_and_fractional_values() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        for target in [
            Value::Null,
            serde_json::json!(0),
            serde_json::json!(59.94),
            serde_json::json!(1000),
        ] {
            let payload = serde_json::json!({
                "hostname": "Host",
                "port": 47989,
                "expected_revision": 0,
                "minimum_fps_target": target,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "{payload}"
            );
        }
        for target in [
            serde_json::json!(-0.1),
            serde_json::json!(1000.1),
            serde_json::json!("20"),
            serde_json::json!(false),
        ] {
            let payload = serde_json::json!({
                "hostname": "Host",
                "port": 47989,
                "expected_revision": 0,
                "minimum_fps_target": target,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn audio_sink_update_preserves_exact_endpoint_ids_and_rejects_controls() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        for endpoint in [
            Value::Null,
            serde_json::json!(""),
            serde_json::json!("  Endpoint ID  "),
        ] {
            let payload = serde_json::json!({
                "hostname": "Host", "port": 47989, "expected_revision": 0,
                "audio_sink": endpoint,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "{payload}"
            );
        }
        for endpoint in [
            serde_json::json!(42),
            serde_json::json!("bad\nendpoint"),
            serde_json::json!("x".repeat(4097)),
        ] {
            let payload = serde_json::json!({
                "hostname": "Host", "port": 47989, "expected_revision": 0,
                "audio_sink": endpoint,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn virtual_sink_update_preserves_explicit_empty_and_rejects_controls() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        for selector in [
            Value::Null,
            serde_json::json!(""),
            serde_json::json!("Steam Streaming Speakers"),
        ] {
            let payload = serde_json::json!({
                "hostname":"Host", "port":47989, "expected_revision":0,
                "virtual_sink":selector,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "{payload}"
            );
        }
        for selector in [
            serde_json::json!(42),
            serde_json::json!("bad\nselector"),
            serde_json::json!("x".repeat(4097)),
        ] {
            let payload = serde_json::json!({
                "hostname":"Host", "port":47989, "expected_revision":0,
                "virtual_sink":selector,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn audio_sink_capture_only_accepts_only_explicit_booleans() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        for value in [serde_json::json!(true), serde_json::json!(false)] {
            let payload = serde_json::json!({
                "hostname": "Host", "port": 47989, "expected_revision": 0,
                "audio_sink_capture_only": value,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "{payload}"
            );
        }
        for value in [Value::Null, serde_json::json!(1), serde_json::json!("true")] {
            let payload = serde_json::json!({
                "hostname": "Host", "port": 47989, "expected_revision": 0,
                "audio_sink_capture_only": value,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn steam_audio_driver_install_policy_is_global_and_accepts_only_booleans() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        for value in [serde_json::json!(true), serde_json::json!(false)] {
            let payload = serde_json::json!({
                "hostname":"Host", "port":47989, "expected_revision":0,
                "install_steam_audio_drivers":value,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "{payload}"
            );
        }
        for value in [
            Value::Null,
            serde_json::json!(1),
            serde_json::json!("enabled"),
        ] {
            let payload = serde_json::json!({
                "hostname":"Host", "port":47989, "expected_revision":0,
                "install_steam_audio_drivers":value,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "{payload}"
            );
        }

        let paired_route = Route {
            service: "amaterasu.pairing/1",
            action: "update_client_config_overrides",
            scope: Some("pairing.admin"),
            requires_empty_body: false,
        };
        let paired = serde_json::json!({
            "certificate_id":"a".repeat(64),
            "expected_revision":7,
            "config_overrides":{"install_steam_audio_drivers":false},
        });
        assert!(validate(paired_route, paired.as_object().unwrap()).is_err());
    }

    #[test]
    fn external_ip_update_preserves_exact_text_and_rejects_controls() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        for address in [
            Value::Null,
            serde_json::json!(""),
            serde_json::json!(" 203.0.113.42 "),
        ] {
            let payload = serde_json::json!({
                "hostname":"Host", "port":47989, "expected_revision":0,
                "external_ip":address,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "{payload}"
            );
        }
        for address in [
            serde_json::json!(42),
            serde_json::json!("bad\naddress"),
            serde_json::json!("x".repeat(4097)),
        ] {
            let payload = serde_json::json!({
                "hostname":"Host", "port":47989, "expected_revision":0,
                "external_ip":address,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn wgc_pacing_update_accepts_null_and_booleans_only() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        for policy in [
            Value::Null,
            serde_json::json!(true),
            serde_json::json!(false),
        ] {
            let payload = serde_json::json!({
                "hostname":"Host", "port":47989, "expected_revision":0,
                "wgc_pacing_smoothing":policy,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "{payload}"
            );
        }
        for policy in [serde_json::json!("false"), serde_json::json!(0)] {
            let payload = serde_json::json!({
                "hostname":"Host", "port":47989, "expected_revision":0,
                "wgc_pacing_smoothing":policy,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn global_preparation_update_is_sparse_clearable_and_strictly_bounded() {
        let route = Route {
            service: "amaterasu.nvhttp/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        let executable = std::env::current_exe().unwrap();
        let overlong_executable = format!("{}{}", executable.display(), "x".repeat(1025));
        let overlong_cwd = format!(
            "{}{}",
            std::env::current_dir().unwrap().display(),
            "x".repeat(1025)
        );
        let command = serde_json::json!({
            "executable": executable,
            "args": ["--prepare"],
            "environment": {"MODE":"safe"},
            "cwd": std::env::current_dir().unwrap(),
            "timeout_ms": 30000,
        });
        for preparation in [
            Value::Null,
            serde_json::json!([]),
            serde_json::json!([{"run":command,"undo":null}]),
            serde_json::json!([{"run":command.clone(),"undo":command.clone()}]),
        ] {
            let payload = serde_json::json!({
                "hostname":"Host", "port":47989, "expected_revision":0,
                "global_preparation":preparation,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "rejected valid preparation: {payload}"
            );
        }
        let omitted = serde_json::json!({
            "hostname":"Host", "port":47989, "expected_revision":0,
        });
        assert!(validate(route, omitted.as_object().unwrap()).is_ok());

        let invalid = [
            Value::Bool(false),
            serde_json::json!({}),
            serde_json::json!([{"run":{"executable":"relative.exe"}}]),
            serde_json::json!([{"run":{"executable":std::env::current_exe().unwrap(),"cwd":"relative"}}]),
            serde_json::json!([{"run":{"executable":format!("{}.cmd", executable.display())}}]),
            serde_json::json!([{"run":{"executable":format!("{}\0", executable.display())}}]),
            serde_json::json!([{"run":{"executable":overlong_executable}}]),
            serde_json::json!([{"run":{"executable":executable,"cwd":overlong_cwd}}]),
            serde_json::json!([{"run":{"executable":executable,"timeout_ms":0}}]),
            serde_json::json!([{"run":{"executable":executable,"unknown":true}}]),
            serde_json::json!([{"run":{"executable":executable,"args":[1]}}]),
            serde_json::json!([{"run":{"executable":executable,"args":std::iter::repeat_n("x", 9).map(|_| "x".repeat(1024)).collect::<Vec<_>>()}}]),
            serde_json::json!([{"run":{"executable":executable,"environment":{"PATH":"a","Path":"b"}}}]),
            serde_json::json!(
                std::iter::repeat_n(
                    serde_json::json!({"run":{"executable":std::env::current_exe().unwrap()}}),
                    9,
                )
                .collect::<Vec<_>>()
            ),
        ];
        for preparation in invalid {
            let payload = serde_json::json!({
                "hostname":"Host", "port":47989, "expected_revision":0,
                "global_preparation":preparation,
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "accepted invalid preparation: {payload}"
            );
        }
    }
}

#[cfg(test)]
mod pairing_override_tests {
    use super::*;

    #[test]
    fn audio_sink_override_is_sparse_and_bounded_for_paired_clients() {
        let route = Route {
            service: "amaterasu.pairing/1",
            action: "update_client_config_overrides",
            scope: Some("pairing.admin"),
            requires_empty_body: false,
        };
        for endpoint in [
            Value::Null,
            serde_json::json!(""),
            serde_json::json!("exact endpoint"),
        ] {
            let payload = serde_json::json!({
                "certificate_id":"a".repeat(64), "expected_revision":0,
                "config_overrides":{"audio_sink":endpoint},
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "{payload}"
            );
        }
        for endpoint in [
            serde_json::json!(42),
            serde_json::json!("bad\nendpoint"),
            serde_json::json!("x".repeat(4097)),
        ] {
            let payload = serde_json::json!({
                "certificate_id":"a".repeat(64), "expected_revision":0,
                "config_overrides":{"audio_sink":endpoint},
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn ds4_back_touchpad_click_override_accepts_only_optional_booleans() {
        let route = Route {
            service: "amaterasu.pairing/1",
            action: "update_client_config_overrides",
            scope: Some("pairing.admin"),
            requires_empty_body: false,
        };
        for value in [
            Value::Null,
            serde_json::json!(true),
            serde_json::json!(false),
        ] {
            let payload = serde_json::json!({
                "certificate_id":"a".repeat(64), "expected_revision":0,
                "config_overrides":{"ds4_back_as_touchpad_click":value},
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_ok(),
                "{payload}"
            );
        }
        for value in [
            serde_json::json!(0),
            serde_json::json!("false"),
            serde_json::json!([]),
        ] {
            let payload = serde_json::json!({
                "certificate_id":"a".repeat(64), "expected_revision":0,
                "config_overrides":{"ds4_back_as_touchpad_click":value},
            });
            assert!(
                validate(route, payload.as_object().unwrap()).is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn virtual_sink_override_is_sparse_and_bounded_for_apps_and_clients() {
        for (service, action, scope) in [
            (
                "amaterasu.catalog/1",
                "update_provider_overrides",
                "catalog.write",
            ),
            (
                "amaterasu.pairing/1",
                "update_client_config_overrides",
                "pairing.admin",
            ),
        ] {
            let route = Route {
                service,
                action,
                scope: Some(scope),
                requires_empty_body: false,
            };
            for selector in [
                Value::Null,
                serde_json::json!(""),
                serde_json::json!("Steam Streaming Speakers"),
            ] {
                let payload = if service == "amaterasu.catalog/1" {
                    serde_json::json!({
                        "id":"app-0000000000000001", "expected_revision":4,
                        "config_overrides":{"virtual_sink":selector},
                    })
                } else {
                    serde_json::json!({
                        "certificate_id":"a".repeat(64), "expected_revision":4,
                        "config_overrides":{"virtual_sink":selector},
                    })
                };
                assert!(
                    validate(route, payload.as_object().unwrap()).is_ok(),
                    "{payload}"
                );
            }
            for selector in [
                serde_json::json!(42),
                serde_json::json!("bad\nselector"),
                serde_json::json!("x".repeat(4097)),
            ] {
                let payload = if service == "amaterasu.catalog/1" {
                    serde_json::json!({
                        "id":"app-0000000000000001", "expected_revision":4,
                        "config_overrides":{"virtual_sink":selector},
                    })
                } else {
                    serde_json::json!({
                        "certificate_id":"a".repeat(64), "expected_revision":4,
                        "config_overrides":{"virtual_sink":selector},
                    })
                };
                assert!(
                    validate(route, payload.as_object().unwrap()).is_err(),
                    "{payload}"
                );
            }
        }
    }

    #[test]
    fn capture_only_override_preserves_false_and_rejects_other_types() {
        let pairing = Route {
            service: "amaterasu.pairing/1",
            action: "update_client_config_overrides",
            scope: Some("pairing.admin"),
            requires_empty_body: false,
        };
        for value in [
            Value::Null,
            serde_json::json!(true),
            serde_json::json!(false),
        ] {
            let overrides = serde_json::json!({"audio_sink_capture_only": value});
            let pairing_payload = serde_json::json!({
                "certificate_id":"a".repeat(64), "expected_revision":0,
                "config_overrides":overrides,
            });
            assert!(validate(pairing, pairing_payload.as_object().unwrap()).is_ok());
            assert!(catalog_config_overrides(&overrides).is_ok());
        }
        for value in [serde_json::json!(0), serde_json::json!("false")] {
            let overrides = serde_json::json!({"audio_sink_capture_only": value});
            let pairing_payload = serde_json::json!({
                "certificate_id":"a".repeat(64), "expected_revision":0,
                "config_overrides":overrides,
            });
            assert!(validate(pairing, pairing_payload.as_object().unwrap()).is_err());
            assert!(catalog_config_overrides(&overrides).is_err());
        }
    }

    #[test]
    fn native_pen_touch_override_is_sparse_boolean_for_apps_and_clients() {
        let routes = [
            Route {
                service: "amaterasu.catalog/1",
                action: "update_provider_overrides",
                scope: Some("catalog.write"),
                requires_empty_body: false,
            },
            Route {
                service: "amaterasu.pairing/1",
                action: "update_client_config_overrides",
                scope: Some("pairing.admin"),
                requires_empty_body: false,
            },
        ];

        for route in routes {
            for value in [
                serde_json::json!(true),
                serde_json::json!(false),
                Value::Null,
            ] {
                let overrides = serde_json::json!({"native_pen_touch":value});
                let payload = if route.service == "amaterasu.catalog/1" {
                    serde_json::json!({
                        "id":"app-0000000000000001", "expected_revision":4,
                        "config_overrides":overrides,
                    })
                } else {
                    serde_json::json!({
                        "certificate_id":"a".repeat(64), "expected_revision":4,
                        "config_overrides":overrides,
                    })
                };
                assert!(
                    validate(route, payload.as_object().unwrap()).is_ok(),
                    "{payload}"
                );
                assert_eq!(payload["config_overrides"]["native_pen_touch"], value);
            }

            let payload = if route.service == "amaterasu.catalog/1" {
                serde_json::json!({
                    "id":"app-0000000000000001", "expected_revision":4,
                    "config_overrides":{},
                })
            } else {
                serde_json::json!({
                    "certificate_id":"a".repeat(64), "expected_revision":4,
                    "config_overrides":{},
                })
            };
            assert!(validate(route, payload.as_object().unwrap()).is_ok());

            for value in [
                serde_json::json!(0),
                serde_json::json!("true"),
                serde_json::json!([]),
                serde_json::json!({}),
            ] {
                let overrides = serde_json::json!({"native_pen_touch":value});
                let payload = if route.service == "amaterasu.catalog/1" {
                    serde_json::json!({
                        "id":"app-0000000000000001", "expected_revision":4,
                        "config_overrides":overrides,
                    })
                } else {
                    serde_json::json!({
                        "certificate_id":"a".repeat(64), "expected_revision":4,
                        "config_overrides":overrides,
                    })
                };
                assert!(
                    validate(route, payload.as_object().unwrap()).is_err(),
                    "{payload}"
                );
            }
        }
    }

    #[test]
    fn controller_profile_and_proton_overrides_are_sparse_booleans_for_apps_and_clients() {
        let routes = [
            Route {
                service: "amaterasu.catalog/1",
                action: "update_provider_overrides",
                scope: Some("catalog.write"),
                requires_empty_body: false,
            },
            Route {
                service: "amaterasu.pairing/1",
                action: "update_client_config_overrides",
                scope: Some("pairing.admin"),
                requires_empty_body: false,
            },
        ];
        let names = [
            "motion_as_ds4",
            "touchpad_as_ds4",
            "ds5_inputtino_randomize_mac",
            "proton_dualsense_compatibility",
        ];

        for route in routes {
            for name in names {
                for value in [Value::Bool(true), Value::Bool(false), Value::Null] {
                    let payload = if route.service == "amaterasu.catalog/1" {
                        serde_json::json!({
                            "id":"app-0000000000000001", "expected_revision":4,
                            "config_overrides":{(name):value},
                        })
                    } else {
                        serde_json::json!({
                            "certificate_id":"a".repeat(64), "expected_revision":4,
                            "config_overrides":{(name):value},
                        })
                    };
                    assert!(
                        validate(route, payload.as_object().unwrap()).is_ok(),
                        "{payload}"
                    );
                }

                for value in [serde_json::json!(0), serde_json::json!("false")] {
                    let payload = if route.service == "amaterasu.catalog/1" {
                        serde_json::json!({
                            "id":"app-0000000000000001", "expected_revision":4,
                            "config_overrides":{(name):value},
                        })
                    } else {
                        serde_json::json!({
                            "certificate_id":"a".repeat(64), "expected_revision":4,
                            "config_overrides":{(name):value},
                        })
                    };
                    assert!(
                        validate(route, payload.as_object().unwrap()).is_err(),
                        "{payload}"
                    );
                }
            }
        }
    }

    #[test]
    fn back_button_timeout_override_accepts_sparse_signed_i32_values() {
        let provider = Route {
            service: "amaterasu.catalog/1",
            action: "update_provider_overrides",
            scope: Some("catalog.write"),
            requires_empty_body: false,
        };
        let pairing = Route {
            service: "amaterasu.pairing/1",
            action: "update_client_config_overrides",
            scope: Some("pairing.admin"),
            requires_empty_body: false,
        };
        for value in [
            Value::Null,
            serde_json::json!(i32::MIN),
            serde_json::json!(-1),
            serde_json::json!(0),
            serde_json::json!(i32::MAX),
        ] {
            let overrides = serde_json::json!({"back_button_timeout": value});
            let provider_payload = serde_json::json!({
                "id":"app-0000000000000001", "expected_revision":0,
                "config_overrides":overrides,
            });
            let pairing_payload = serde_json::json!({
                "certificate_id":"a".repeat(64), "expected_revision":0,
                "config_overrides":overrides,
            });
            assert!(catalog_config_overrides(&overrides).is_ok());
            assert!(validate(provider, provider_payload.as_object().unwrap()).is_ok());
            assert!(validate(pairing, pairing_payload.as_object().unwrap()).is_ok());
        }

        let invalid_values = [
            serde_json::json!(i64::from(i32::MIN) - 1),
            serde_json::json!(i64::from(i32::MAX) + 1),
            serde_json::json!(1.5),
            serde_json::json!("-1"),
            serde_json::json!(true),
        ];
        for value in invalid_values {
            let overrides = serde_json::json!({"back_button_timeout": value});
            let provider_payload = serde_json::json!({
                "id":"app-0000000000000001", "expected_revision":0,
                "config_overrides":overrides,
            });
            let pairing_payload = serde_json::json!({
                "certificate_id":"a".repeat(64), "expected_revision":0,
                "config_overrides":overrides,
            });
            assert!(catalog_config_overrides(&overrides).is_err());
            assert!(validate(provider, provider_payload.as_object().unwrap()).is_err());
            assert!(validate(pairing, pairing_payload.as_object().unwrap()).is_err());
        }
        let float_integer: Value = serde_json::from_str(r#"{"back_button_timeout":1.0}"#).unwrap();
        assert!(catalog_config_overrides(&float_integer).is_err());
        assert!(catalog_config_overrides(&serde_json::json!({})).is_ok());
    }

    #[test]
    fn scoped_input_overrides_accept_null_and_false_but_reject_wrong_platform_profiles() {
        let provider = Route {
            service: "amaterasu.catalog/1",
            action: "update_provider_overrides",
            scope: Some("catalog.write"),
            requires_empty_body: false,
        };
        let pairing = Route {
            service: "amaterasu.pairing/1",
            action: "update_client_config_overrides",
            scope: Some("pairing.admin"),
            requires_empty_body: false,
        };
        let overrides = serde_json::json!({
            "controller":false,
            "gamepad":"auto",
            "keyboard":null,
            "mouse":true
        });
        assert!(catalog_config_overrides(&overrides).is_ok());
        for (route, payload) in [
            (
                provider,
                serde_json::json!({
                    "id":"app-0000000000000001", "expected_revision":0,
                    "config_overrides":overrides.clone(),
                }),
            ),
            (
                pairing,
                serde_json::json!({
                    "certificate_id":"a".repeat(64), "expected_revision":0,
                    "config_overrides":overrides.clone(),
                }),
            ),
        ] {
            assert!(validate(route, payload.as_object().unwrap()).is_ok());
        }

        #[cfg(windows)]
        let wrong_platform = "xone";
        #[cfg(target_os = "linux")]
        let wrong_platform = "vhf";
        #[cfg(not(any(windows, target_os = "linux")))]
        let wrong_platform = "ds4";
        for invalid in [
            serde_json::json!({"controller":"false"}),
            serde_json::json!({"gamepad":wrong_platform}),
            serde_json::json!({"keyboard":0}),
            serde_json::json!({"mouse":[]}),
        ] {
            assert!(catalog_config_overrides(&invalid).is_err(), "{invalid}");
            let pairing_payload = serde_json::json!({
                "certificate_id":"a".repeat(64), "expected_revision":0,
                "config_overrides":invalid,
            });
            assert!(validate(pairing, pairing_payload.as_object().unwrap()).is_err());
        }
    }
}

fn safe_bind_address(value: &str) -> Option<IpAddr> {
    let address = value.parse::<IpAddr>().ok()?;
    match address {
        IpAddr::V4(address) if !address.is_multicast() && address != Ipv4Addr::BROADCAST => {
            Some(IpAddr::V4(address))
        }
        IpAddr::V6(address) if !address.is_multicast() && address.to_ipv4_mapped().is_none() => {
            Some(IpAddr::V6(address))
        }
        _ => None,
    }
}

fn fields(object: &Object, required: &[&str], optional: &[&str]) -> Result<(), &'static str> {
    if required.iter().any(|key| !object.contains_key(*key))
        || object
            .keys()
            .any(|key| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        return Err(INVALID);
    }
    Ok(())
}

/// A short printable identifier, location or key. The management service
/// decides whether the value is acceptable; this only bounds what reaches it.
fn repository_text(object: &Object, key: &str, maximum_bytes: usize) -> Result<(), &'static str> {
    let value = text(object, key)?;
    if value.is_empty()
        || value.len() > maximum_bytes
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(INVALID);
    }
    Ok(())
}

fn text<'a>(object: &'a Object, key: &str) -> Result<&'a str, &'static str> {
    object.get(key).and_then(Value::as_str).ok_or(INVALID)
}

fn pairing(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "identity" | "pending" | "list_clients" => fields(object, &[], &[]),
        "approve_pin" => {
            fields(object, &["pairing_id", "pin"], &[])?;
            let id = text(object, "pairing_id")?;
            let pin = text(object, "pin")?;
            if id.is_empty()
                || id.len() > 32
                || pin.len() != 4
                || !pin.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "unpair" => {
            fields(object, &["certificate_id"], &["expected_revision"])?;
            let id = text(object, "certificate_id")?;
            if id.len() != 64
                || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
                || object
                    .get("expected_revision")
                    .is_some_and(|value| value.as_u64().is_none())
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "update_client_permissions" => {
            fields(
                object,
                &["certificate_id", "expected_revision", "permissions"],
                &[],
            )?;
            let certificate_id = text(object, "certificate_id")?;
            if certificate_id.len() != 64
                || !certificate_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(INVALID);
            }
            unsigned_u64(object, "expected_revision")?;
            let permissions = object
                .get("permissions")
                .and_then(Value::as_object)
                .ok_or(INVALID)?;
            let names = [
                "list_apps",
                "view_streams",
                "launch_apps",
                "terminate_streams",
            ];
            fields(permissions, &names, &[])?;
            if names
                .iter()
                .any(|name| !permissions.get(*name).is_some_and(Value::is_boolean))
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "set_permission_mode" => {
            fields(object, &["enabled", "expected_revision"], &[])?;
            unsigned_u64(object, "expected_revision")?;
            if !object.get("enabled").is_some_and(Value::is_boolean) {
                return Err(INVALID);
            }
            Ok(())
        }
        "update_client_config_overrides" => {
            fields(
                object,
                &["certificate_id", "expected_revision", "config_overrides"],
                &[],
            )?;
            let certificate_id = text(object, "certificate_id")?;
            if certificate_id.len() != 64
                || !certificate_id.bytes().all(|byte| byte.is_ascii_hexdigit())
                || object
                    .get("expected_revision")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err(INVALID);
            }
            let config_overrides = object
                .get("config_overrides")
                .and_then(Value::as_object)
                .ok_or(INVALID)?;
            fields(
                config_overrides,
                &[],
                &[
                    "fec_percentage",
                    "sw_preset",
                    "sw_tune",
                    "encoder",
                    "min_threads",
                    "qp",
                    "max_bitrate",
                    "minimum_fps_target",
                    "video_max_batch_size_kb",
                    "frame_limiter_fps_limit",
                    "frame_limiter_enable",
                    "frame_limiter_provider",
                    "frame_limiter_disable_vsync",
                    "frame_limiter_auto_virtual_framegen",
                    "rtss_frame_limit_type",
                    "stream_audio",
                    "audio_sink",
                    "virtual_sink",
                    "audio_sink_capture_only",
                    "nvenc_spatial_aq",
                    "nvenc_preset",
                    "nvenc_h264_cavlc",
                    "nvenc_twopass",
                    "nvenc_vbv_increase",
                    "nvenc_split_encode",
                    "nvenc_force_split_encode",
                    "nvenc_realtime_hags",
                    "nvenc_opengl_vulkan_on_dxgi",
                    "nvenc_latency_over_power",
                    "vaapi_strict_rc_buffer",
                    "back_button_timeout",
                    "controller",
                    "gamepad",
                    "keyboard",
                    "mouse",
                    "native_pen_touch",
                    "ds4_back_as_touchpad_click",
                    "motion_as_ds4",
                    "touchpad_as_ds4",
                    "ds5_inputtino_randomize_mac",
                    "proton_dualsense_compatibility",
                    "high_resolution_scrolling",
                    "always_send_scancodes",
                    "key_rightalt_to_key_win",
                    "keybindings",
                    "key_repeat_delay",
                    "key_repeat_frequency",
                ],
            )?;
            if let Some(percentage) = config_overrides.get("fec_percentage")
                && !percentage.is_null()
                && !percentage
                    .as_u64()
                    .is_some_and(|value| (1..=u8::MAX as u64).contains(&value))
            {
                return Err(INVALID);
            }
            if config_overrides
                .get("stream_audio")
                .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
            {
                return Err(INVALID);
            }
            if config_overrides
                .get("ds4_back_as_touchpad_click")
                .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
            {
                return Err(INVALID);
            }
            optional_software_encoder_options(config_overrides)?;
            optional_nvenc_settings(config_overrides)?;
            optional_audio_sink(config_overrides)?;
            optional_virtual_sink(config_overrides)?;
            optional_audio_sink_capture_only(config_overrides)?;
            optional_back_button_timeout(config_overrides)?;
            optional_input_overrides(config_overrides)?;
            optional_paired_client_extended_overrides(config_overrides)?;
            Ok(())
        }
        _ => Err(INVALID),
    }
}

fn optional_software_encoder_options(object: &Object) -> Result<(), &'static str> {
    for (key, limit) in [("sw_preset", 128), ("sw_tune", 256)] {
        if object.get(key).is_some_and(|value| {
            !value.is_null()
                && !value
                    .as_str()
                    .is_some_and(|text| text.len() <= limit && !text.chars().any(char::is_control))
        }) {
            return Err(INVALID);
        }
    }
    if object.get("encoder").is_some_and(|value| {
        !value.is_null()
            && !value
                .as_str()
                .is_some_and(|text| matches!(text, "auto" | "software"))
    }) {
        return Err(INVALID);
    }
    for key in ["min_threads", "qp"] {
        if object.get(key).is_some_and(|value| {
            !value.is_null()
                && !value
                    .as_i64()
                    .is_some_and(|number| i32::try_from(number).is_ok())
        }) {
            return Err(INVALID);
        }
    }
    Ok(())
}

fn optional_paired_client_extended_overrides(overrides: &Object) -> Result<(), &'static str> {
    for key in [
        "frame_limiter_enable",
        "frame_limiter_disable_vsync",
        "nvenc_spatial_aq",
        "high_resolution_scrolling",
        "always_send_scancodes",
        "key_rightalt_to_key_win",
    ] {
        if overrides
            .get(key)
            .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
        {
            return Err(INVALID);
        }
    }
    if overrides.get("max_bitrate").is_some_and(|value| {
        !value.is_null()
            && !value
                .as_i64()
                .is_some_and(|bitrate| i32::try_from(bitrate).is_ok())
    }) || overrides
        .get("video_max_batch_size_kb")
        .is_some_and(|value| !value.is_null() && !value.as_u64().is_some_and(|size| size <= 64))
        || overrides.get("key_repeat_delay").is_some_and(|value| {
            !value.is_null() && !value.as_u64().is_some_and(|delay| delay <= i32::MAX as u64)
        })
    {
        return Err(INVALID);
    }
    if overrides.get("minimum_fps_target").is_some_and(|value| {
        !value.is_null()
            && !value
                .as_f64()
                .is_some_and(|target| target.is_finite() && (0.0..=1000.0).contains(&target))
    }) || overrides.get("key_repeat_frequency").is_some_and(|value| {
        !value.is_null()
            && !value
                .as_f64()
                .is_some_and(|frequency| frequency.is_finite() && frequency >= 0.0)
    }) || overrides
        .get("frame_limiter_fps_limit")
        .is_some_and(|value| {
            !value.is_null() && !value.as_f64().is_some_and(valid_paired_frame_limit)
        })
    {
        return Err(INVALID);
    }
    if overrides
        .get("frame_limiter_provider")
        .is_some_and(|value| {
            !value.is_null()
                && !value
                    .as_str()
                    .is_some_and(valid_catalog_frame_limiter_provider)
        })
        || overrides
            .get("frame_limiter_auto_virtual_framegen")
            .is_some_and(|value| {
                !value.is_null()
                    && (!cfg!(windows)
                        || !value.as_str().is_some_and(|mode| {
                            matches!(mode, "enabled" | "vrr" | "legacy" | "disabled")
                        }))
            })
        || overrides.get("rtss_frame_limit_type").is_some_and(|value| {
            !value.is_null()
                && (!cfg!(windows)
                    || !value.as_str().is_some_and(|mode| {
                        matches!(
                            mode,
                            "async" | "front edge sync" | "back edge sync" | "nvidia reflex"
                        )
                    }))
        })
        || overrides
            .get("frame_limiter_disable_vsync")
            .is_some_and(|value| !value.is_null() && !cfg!(windows))
    {
        return Err(INVALID);
    }
    Ok(())
}

fn valid_paired_frame_limit(fps: f64) -> bool {
    if !fps.is_finite() || !(0.0..=1000.0).contains(&fps) {
        return false;
    }
    let millihertz = fps * 1000.0;
    (millihertz - millihertz.round()).abs() <= 0.000_001
}

#[cfg(test)]
mod paired_client_roundtrip_tests {
    use super::*;
    use serde_json::json;

    fn route() -> Route {
        Route {
            service: "amaterasu.pairing/1",
            action: "update_client_config_overrides",
            scope: Some("pairing.admin"),
            requires_empty_body: false,
        }
    }

    fn request(overrides: Value) -> Value {
        json!({
            "certificate_id":"a".repeat(64),
            "expected_revision":7,
            "config_overrides":overrides,
        })
    }

    #[test]
    fn full_provider_override_document_can_be_preserved_by_an_editor_save() {
        let mut overrides = json!({
            "fec_percentage":47,
            "max_bitrate":-15000,
            "minimum_fps_target":0.0,
            "video_max_batch_size_kb":64,
            "frame_limiter_fps_limit":59.875,
            "frame_limiter_enable":false,
            "frame_limiter_provider":"auto",
            "stream_audio":false,
            "audio_sink":"",
            "virtual_sink":"",
            "audio_sink_capture_only":false,
            "nvenc_spatial_aq":false,
            "back_button_timeout":-1,
            "controller":false,
            "gamepad":"auto",
            "keyboard":true,
            "mouse":false,
            "native_pen_touch":false,
            "high_resolution_scrolling":false,
            "always_send_scancodes":false,
            "key_rightalt_to_key_win":true,
            "key_repeat_delay":0,
            "key_repeat_frequency":24.9,
        });
        if cfg!(windows) {
            overrides["frame_limiter_disable_vsync"] = json!(false);
            overrides["frame_limiter_auto_virtual_framegen"] = json!("vrr");
            overrides["rtss_frame_limit_type"] = json!("async");
        }
        assert!(validate(route(), request(overrides).as_object().unwrap()).is_ok());
    }

    #[test]
    fn extended_overrides_accept_null_reset_and_reject_malformed_values() {
        for key in [
            "max_bitrate",
            "minimum_fps_target",
            "video_max_batch_size_kb",
            "frame_limiter_fps_limit",
            "frame_limiter_enable",
            "frame_limiter_provider",
            "frame_limiter_disable_vsync",
            "frame_limiter_auto_virtual_framegen",
            "rtss_frame_limit_type",
            "nvenc_spatial_aq",
            "high_resolution_scrolling",
            "always_send_scancodes",
            "key_rightalt_to_key_win",
            "keybindings",
            "key_repeat_delay",
            "key_repeat_frequency",
        ] {
            let payload = request(json!({key: null}));
            assert!(
                validate(route(), payload.as_object().unwrap()).is_ok(),
                "{key}"
            );
        }
        for (key, value) in [
            ("max_bitrate", json!(i32::MAX as u64 + 1)),
            ("minimum_fps_target", json!(-0.1)),
            ("video_max_batch_size_kb", json!(65)),
            ("frame_limiter_fps_limit", json!(59.1234)),
            ("frame_limiter_enable", json!(0)),
            ("frame_limiter_provider", json!("unknown")),
            ("frame_limiter_disable_vsync", json!("false")),
            ("frame_limiter_auto_virtual_framegen", json!("true")),
            ("rtss_frame_limit_type", json!("unknown")),
            ("nvenc_spatial_aq", json!("false")),
            ("high_resolution_scrolling", json!(1)),
            ("always_send_scancodes", json!("false")),
            ("key_rightalt_to_key_win", json!(0)),
            ("key_repeat_delay", json!(i32::MAX as u64 + 1)),
            ("key_repeat_frequency", json!(-0.1)),
        ] {
            let payload = request(json!({key: value}));
            assert!(
                validate(route(), payload.as_object().unwrap()).is_err(),
                "{key}"
            );
        }
    }
}

fn display(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "status" => fields(object, &[], &[]),
        "enumerate" => enumeration(object),
        "apply" => {
            fields(object, &["configuration"], &[])?;
            configuration(
                object
                    .get("configuration")
                    .and_then(Value::as_object)
                    .ok_or(INVALID)?,
            )
        }
        "restore" | "recover" => {
            fields(object, &["lease"], &[])?;
            if text(object, "lease")?.is_empty() {
                return Err(INVALID);
            }
            Ok(())
        }
        _ => Err(INVALID),
    }
}

fn enumeration(object: &Object) -> Result<(), &'static str> {
    fields(object, &[], &["display", "offset", "limit"])?;
    if object
        .get("display")
        .is_some_and(|value| !value.is_null() && !value.is_string())
        || object
            .get("offset")
            .is_some_and(|value| !value.is_null() && value.as_u64().is_none())
        || object.get("limit").is_some_and(|value| {
            !value.is_null()
                && !value
                    .as_u64()
                    .is_some_and(|limit| (1..=32).contains(&limit))
        })
    {
        return Err(INVALID);
    }
    Ok(())
}

fn configuration(object: &Object) -> Result<(), &'static str> {
    fields(object, &["displays"], &[])?;
    let displays = object
        .get("displays")
        .and_then(Value::as_array)
        .ok_or(INVALID)?;
    if displays.is_empty() || displays.len() > 16 {
        return Err(INVALID);
    }
    for display in displays {
        display_setting(display.as_object().ok_or(INVALID)?)?;
    }
    Ok(())
}

fn display_setting(object: &Object) -> Result<(), &'static str> {
    fields(
        object,
        &["id", "width", "height", "refresh", "x", "y"],
        &["rotation"],
    )?;
    text(object, "id")?;
    for key in ["width", "height"] {
        unsigned(object, key)?;
    }
    for key in ["x", "y"] {
        if !object
            .get(key)
            .and_then(Value::as_i64)
            .is_some_and(|value| i32::try_from(value).is_ok())
        {
            return Err(INVALID);
        }
    }
    let refresh = object
        .get("refresh")
        .and_then(Value::as_object)
        .ok_or(INVALID)?;
    fields(refresh, &["numerator", "denominator"], &[])?;
    unsigned(refresh, "numerator")?;
    unsigned(refresh, "denominator")?;
    if object.get("rotation").is_some_and(|value| {
        !matches!(
            value.as_str(),
            Some("identity" | "rotate90" | "rotate180" | "rotate270")
        )
    }) {
        return Err(INVALID);
    }
    Ok(())
}

fn unsigned(object: &Object, key: &str) -> Result<(), &'static str> {
    if object
        .get(key)
        .and_then(Value::as_u64)
        .is_some_and(|value| u32::try_from(value).is_ok())
    {
        Ok(())
    } else {
        Err(INVALID)
    }
}

fn unsigned_u64(object: &Object, key: &str) -> Result<(), &'static str> {
    if object.get(key).and_then(Value::as_u64).is_some() {
        Ok(())
    } else {
        Err(INVALID)
    }
}

fn launch(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "launch" => {
            fields(object, &["operation_id", "app_id"], &[])?;
            let operation = text(object, "operation_id")?;
            let app = text(object, "app_id")?;
            let id = app.strip_prefix("app-").ok_or(INVALID)?;
            if operation.is_empty()
                || operation.len() > 128
                || !operation.bytes().all(|byte| byte.is_ascii_graphic())
                || id.len() != 16
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || id.bytes().all(|byte| byte == b'0')
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "status" | "stop" => {
            fields(object, &["launch_id"], &[])?;
            if object
                .get("launch_id")
                .and_then(Value::as_u64)
                .is_none_or(|id| id == 0)
            {
                return Err(INVALID);
            }
            Ok(())
        }
        "list" => {
            fields(object, &[], &["offset", "limit"])?;
            if object
                .get("offset")
                .is_some_and(|value| value.as_u64().is_none())
                || object.get("limit").is_some_and(|value| {
                    !value
                        .as_u64()
                        .is_some_and(|limit| (1..=16).contains(&limit))
                })
            {
                return Err(INVALID);
            }
            Ok(())
        }
        _ => Err(INVALID),
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;

    #[test]
    fn input_policy_route_fields_are_exact_and_typed() {
        let route = Route {
            service: "amaterasu.input/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        for object in [
            serde_json::json!({"keyboard":true,"mouse":false,"expected_revision":0}),
            serde_json::json!({"keyboard":false,"mouse":true,"expected_revision":u64::MAX}),
            serde_json::json!({"keyboard":true,"mouse":true,"high_resolution_scrolling":false,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"always_send_scancodes":false,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"key_rightalt_to_key_win":true,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"key_repeat_delay":0,"key_repeat_frequency":0.0,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"key_repeat_delay":2147483647,"key_repeat_frequency":24.9,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"back_button_timeout":-1,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"back_button_timeout":0,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"back_button_timeout":2147483647,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"back_button_timeout":-2147483648,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"gamepad":"auto","expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"controller":false,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"ds4_back_as_touchpad_click":true,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"ds4_back_as_touchpad_click":false,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"native_pen_touch":true,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":true,"native_pen_touch":false,"expected_revision":0}),
            #[cfg(windows)]
            serde_json::json!({"keyboard":true,"mouse":true,"gamepad":"ds4","expected_revision":0}),
            #[cfg(target_os = "linux")]
            serde_json::json!({"keyboard":true,"mouse":true,"gamepad":"xone","expected_revision":0}),
        ] {
            assert!(validate(route, object.as_object().unwrap()).is_ok());
        }
        for object in [
            serde_json::json!({"keyboard":1,"mouse":true,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":"false","expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"expected_revision":-1}),
            serde_json::json!({"keyboard":true,"mouse":false,"expected_revision":0,"owner":"other"}),
            serde_json::json!({"keyboard":true,"mouse":false,"high_resolution_scrolling":"true","expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"high_resolution_scrolling":null,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"always_send_scancodes":"true","expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"key_rightalt_to_key_win":"true","expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"key_rightalt_to_key_win":null,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"key_repeat_delay":-1,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"key_repeat_delay":0.5,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"key_repeat_delay":2147483648u64,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"key_repeat_delay":"500","expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"key_repeat_frequency":-0.1,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"key_repeat_frequency":"24.9","expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"key_repeat_frequency":null,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"back_button_timeout":2147483648i64,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"back_button_timeout":-2147483649i64,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"back_button_timeout":0.5,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"back_button_timeout":"-1","expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"gamepad":null,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"controller":null,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"controller":"false","expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"ds4_back_as_touchpad_click":null,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"ds4_back_as_touchpad_click":0,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"ds4_back_as_touchpad_click":"false","expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"native_pen_touch":null,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"native_pen_touch":0,"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"native_pen_touch":"false","expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"native_pen_touch":[],"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"native_pen_touch":{},"expected_revision":0}),
            serde_json::json!({"keyboard":true,"mouse":false,"gamepad":"invented","expected_revision":0}),
            #[cfg(windows)]
            serde_json::json!({"keyboard":true,"mouse":false,"gamepad":"xone","expected_revision":0}),
            #[cfg(target_os = "linux")]
            serde_json::json!({"keyboard":true,"mouse":false,"gamepad":"vhf","expected_revision":0}),
            #[cfg(not(any(windows, target_os = "linux")))]
            serde_json::json!({"keyboard":true,"mouse":false,"gamepad":"x360","expected_revision":0}),
        ] {
            assert!(validate(route, object.as_object().unwrap()).is_err());
        }
    }

    #[test]
    fn input_status_route_accepts_only_an_empty_read_request() {
        let route = Route {
            service: "amaterasu.input/1",
            action: "status",
            scope: Some("configuration.read"),
            requires_empty_body: false,
        };
        assert!(validate(route, &serde_json::Map::new()).is_ok());
        assert!(
            validate(
                route,
                serde_json::json!({"owner":"other"}).as_object().unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn gamepad_profile_validation_matches_source_platform_choices() {
        #[cfg(windows)]
        let (recognized, wrong_platform) = (
            [
                "auto",
                "x360",
                "vhf",
                "vhf_xbox",
                "vhf_xbox_one",
                "vhf_ds4",
                "vhf_ds5",
                "vhf_switch",
                "ds4",
            ]
            .as_slice(),
            "xone",
        );
        #[cfg(target_os = "linux")]
        let (recognized, wrong_platform) =
            (["auto", "xone", "ds4", "ds5", "switch"].as_slice(), "vhf");
        #[cfg(not(any(windows, target_os = "linux")))]
        let (recognized, wrong_platform) = (["auto"].as_slice(), "x360");

        for profile in recognized {
            assert!(valid_gamepad_profile(profile), "accepted {profile}");
        }
        assert!(!valid_gamepad_profile(wrong_platform));
        assert!(!valid_gamepad_profile("unknown"));
    }
}

#[cfg(test)]
mod playnite_tests {
    use super::*;

    #[test]
    fn playnite_update_is_patch_compatible_and_enforces_product_bounds() {
        let route = Route {
            service: "amaterasu.playnite/1",
            action: "settings_update",
            scope: Some("configuration.write"),
            requires_empty_body: false,
        };
        let valid = serde_json::json!({
            "enabled":false,
            "expected_revision":7,
            "sync_all_installed":true,
            "autosync_remove_uninstalled":false,
            "autosync_delete_after_days":2147483647,
            "autosync_require_replacement":true,
            "focus_attempts":2147483647,
            "focus_timeout_secs":2147483647,
            "focus_exit_on_first":false,
            "fullscreen_entry_enabled":false,
            "recent_games":0,
            "recent_max_age_days":36500,
            "sync_categories":["Action", "RPG"],
            "exclude_categories":[],
            "sync_plugins":["example.plugin"],
            "exclude_plugins":[],
            "exclude_games":["game-id"]
        });
        assert!(validate(route, valid.as_object().unwrap()).is_ok());
        let partial = serde_json::json!({"enabled":true,"expected_revision":u64::MAX});
        assert!(validate(route, partial.as_object().unwrap()).is_ok());

        let mut invalid = vec![
            serde_json::json!({"enabled":false}),
            serde_json::json!({"enabled":0,"expected_revision":0}),
            serde_json::json!({"enabled":false,"expected_revision":-1}),
            serde_json::json!({"enabled":false,"expected_revision":0,"auto_sync":"true"}),
            serde_json::json!({"enabled":false,"expected_revision":0,"autosync_remove_uninstalled":"false"}),
            serde_json::json!({"enabled":false,"expected_revision":0,"autosync_delete_after_days":2147483648_u64}),
            serde_json::json!({"enabled":false,"expected_revision":0,"autosync_delete_after_days":-1}),
            serde_json::json!({"enabled":false,"expected_revision":0,"autosync_require_replacement":"true"}),
            serde_json::json!({"enabled":false,"expected_revision":0,"focus_attempts":2147483648_u64}),
            serde_json::json!({"enabled":false,"expected_revision":0,"focus_timeout_secs":-1}),
            serde_json::json!({"enabled":false,"expected_revision":0,"focus_exit_on_first":"false"}),
            serde_json::json!({"enabled":false,"expected_revision":0,"fullscreen_entry_enabled":"false"}),
            serde_json::json!({"enabled":false,"expected_revision":0,"recent_games":10001}),
            serde_json::json!({"enabled":false,"expected_revision":0,"recent_max_age_days":36501}),
            serde_json::json!({"enabled":false,"expected_revision":0,"sync_categories":"Action"}),
            serde_json::json!({"enabled":false,"expected_revision":0,"sync_categories":[1]}),
            serde_json::json!({"enabled":false,"expected_revision":0,"sync_categories":["  "]}),
            serde_json::json!({"enabled":false,"expected_revision":0,"sync_categories":["bad\nname"]}),
            serde_json::json!({"enabled":false,"expected_revision":0,"sync_categories":["Action", "action"]}),
            serde_json::json!({"enabled":false,"expected_revision":0,"owner":"other"}),
        ];
        invalid.push(serde_json::json!({
            "enabled":false,
            "expected_revision":0,
            "exclude_games":(0..257).map(|index| format!("game-{index}")).collect::<Vec<_>>()
        }));
        invalid.push(serde_json::json!({
            "enabled":false,
            "expected_revision":0,
            "exclude_games":["x".repeat(513)]
        }));
        for value in invalid {
            assert!(
                validate(route, value.as_object().unwrap()).is_err(),
                "accepted malformed Playnite settings payload: {value}"
            );
        }
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    fn route(action: &'static str) -> Route {
        Route {
            service: "amaterasu.catalog/1",
            action,
            scope: Some("catalog.write"),
            requires_empty_body: false,
        }
    }

    fn catalog_create_with_overrides(config_overrides: Value) -> Value {
        serde_json::json!({
            "expected_revision": 4,
            "entry": {
                "name": "Game",
                "launch": {"kind": "application", "path": "/game"},
                "config_overrides": config_overrides,
            },
        })
    }

    #[test]
    fn catalog_editor_accepts_sparse_fec_and_supported_executable_policy() {
        let create = serde_json::json!({
            "expected_revision": 4,
            "entry": {
                "name": "Configured app",
                "steam_id": "4294967295",
                "steam_install_dir": "C:\\Games\\Configured",
                "elevated": true,
                "exclude_global_prep_cmd": true,
                "config_overrides": {"fec_percentage": 255, "stream_audio": false},
                "launch": {
                    "kind": "executable",
                    "program": "C:\\Games\\game.exe",
                    "arguments": ["--fullscreen"],
                    "working-dir": "",
                    "auto-detach": false,
                    "wait-all": false,
                    "exit-timeout": 0,
                    "detached": ["first", "second"],
                    "preparation": [{
                        "run": {"executable": "C:\\Tools\\prepare.exe"},
                        "undo": null
                    }]
                }
            }
        });
        assert!(validate(route("create"), create.as_object().unwrap()).is_ok());
        let update = serde_json::json!({
            "id":"app-0000000000000001",
            "expected_revision":4,
            "entry": {
                "name":"Updated elevated app",
                "elevated":true,
                "launch":{"kind":"executable","program":"C:\\Games\\game.exe"}
            }
        });
        assert!(validate(route("update"), update.as_object().unwrap()).is_ok());
        let legacy = serde_json::json!({
            "entry": {"name":"Legacy", "launch":{"kind":"application","path":"/opt/example"}},
            "expected_revision": 0
        });
        assert!(validate(route("create"), legacy.as_object().unwrap()).is_ok());
        for key in ["exclude_global_prep_cmd", "exclude-global-prep-cmd"] {
            let entry = serde_json::json!({
                "expected_revision":0,
                "entry":{"name":"Excluded","exclude_global_prep_cmd":true,"launch":{"kind":"application","path":"/opt/example"}}
            });
            let mut entry = entry;
            if key == "exclude-global-prep-cmd" {
                let fields = entry["entry"].as_object_mut().unwrap();
                fields.remove("exclude_global_prep_cmd");
                fields.insert(key.into(), Value::Bool(true));
            }
            assert!(validate(route("create"), entry.as_object().unwrap()).is_ok());
        }
    }

    #[test]
    fn catalog_output_matches_launch_optional_path_shape_and_bounds() {
        for output in [
            Value::Null,
            serde_json::json!(""),
            serde_json::json!("null"),
            serde_json::json!("logs/game.log"),
            serde_json::json!("/var/log/game.log"),
            serde_json::json!("x".repeat(1024)),
        ] {
            let payload = serde_json::json!({
                "expected_revision": 0,
                "entry": {
                    "name": "Game",
                    "launch": {"kind": "application", "path": "/game"},
                    "output": output,
                }
            });
            assert!(validate(route("create"), payload.as_object().unwrap()).is_ok());
        }

        for output in [
            serde_json::json!(false),
            serde_json::json!([]),
            serde_json::json!("x".repeat(1025)),
            serde_json::json!("bad\npath"),
        ] {
            let payload = serde_json::json!({
                "expected_revision": 0,
                "entry": {
                    "name": "Game",
                    "launch": {"kind": "application", "path": "/game"},
                    "output": output,
                }
            });
            assert!(validate(route("create"), payload.as_object().unwrap()).is_err());
        }
    }

    #[test]
    fn catalog_raw_preparation_matches_catalog_bounds_defaults_and_combined_limit() {
        let create = |prep_cmd: Value, launch: Value| {
            serde_json::json!({
                "expected_revision": 0,
                "entry": {"name": "Game", "launch": launch, "prep_cmd": prep_cmd}
            })
        };
        let raw_step = serde_json::json!({
            "do": "  configure --profile=one  ",
            "undo": " restore ",
            "elevated": true
        });
        let valid = create(
            serde_json::json!([raw_step, {}, {"do": "x".repeat(2048)}]),
            serde_json::json!({"kind": "executable", "program": "/game"}),
        );
        assert!(validate(route("create"), valid.as_object().unwrap()).is_ok());

        let alias = serde_json::json!({
            "expected_revision": 0,
            "entry": {
                "name": "Game",
                "launch": {"kind": "application", "path": "/game"},
                "prep-cmd": [{"do": "raw"}]
            }
        });
        assert!(validate(route("create"), alias.as_object().unwrap()).is_ok());

        let too_many_raw = create(
            serde_json::json!([{}, {}, {}, {}, {}, {}, {}, {}, {}]),
            serde_json::json!({"kind": "application", "path": "/game"}),
        );
        let too_long = create(
            serde_json::json!([{"undo": "x".repeat(2049)}]),
            serde_json::json!({"kind": "application", "path": "/game"}),
        );
        let nul_command = create(
            serde_json::json!([{"do": "tool\0argument"}]),
            serde_json::json!({"kind": "application", "path": "/game"}),
        );
        let wrong_types = create(
            serde_json::json!([{"do": null}, {"elevated": "true"}]),
            serde_json::json!({"kind": "application", "path": "/game"}),
        );
        let unknown_field = create(
            serde_json::json!([{"do": "raw", "owner": "forged"}]),
            serde_json::json!({"kind": "application", "path": "/game"}),
        );
        let duplicate_aliases = serde_json::json!({
            "expected_revision": 0,
            "entry": {
                "name": "Game",
                "launch": {"kind": "application", "path": "/game"},
                "prep_cmd": [],
                "prep-cmd": []
            }
        });
        let combined_overflow = create(
            serde_json::json!([{}, {}, {}, {}, {}, {}, {}, {}]),
            serde_json::json!({
                "kind": "executable",
                "program": "/game",
                "preparation": [{"run": {"executable": "/prepare"}}]
            }),
        );
        for invalid in [
            too_many_raw,
            too_long,
            nul_command,
            wrong_types,
            unknown_field,
            duplicate_aliases,
            combined_overflow,
        ] {
            assert!(validate(route("create"), invalid.as_object().unwrap()).is_err());
        }

        let combined_at_limit = create(
            serde_json::json!([{}, {}, {}, {}, {}, {}, {}]),
            serde_json::json!({
                "kind": "executable",
                "program": "/game",
                "preparation": [{"run": {"executable": "/prepare"}}]
            }),
        );
        assert!(validate(route("create"), combined_at_limit.as_object().unwrap()).is_ok());
    }

    #[test]
    fn provider_override_route_accepts_sparse_streaming_patch_and_revision() {
        let route = route("update_provider_overrides");
        for body in [
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"fec_percentage":1}}"#.as_slice(),
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"fec_percentage":255}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"fec_percentage":null}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"stream_audio":false}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"stream_audio":null}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"audio_sink":""}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"audio_sink":"exact endpoint"}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"audio_sink":null}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{}}"#,
        ] {
            let value: Value = serde_json::from_slice(body).unwrap();
            assert!(
                validate(route, value.as_object().unwrap()).is_ok(),
                "rejected valid patch: {}",
                String::from_utf8_lossy(body)
            );
        }
        for body in [
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"fec_percentage":0}}"#.as_slice(),
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"fec_percentage":256}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"fec_percentage":"37"}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"stream_audio":"false"}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"audio_sink":42}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"audio_sink":"bad\nendpoint"}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"fec_percentage":37,"unknown":true}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":-1,"config_overrides":{"fec_percentage":37}}"#,
            br#"{"id":"app-0000000000000001","expected_revision":4,"client_id":"forged","config_overrides":{"fec_percentage":37}}"#,
            br#"{"id":"app-0000000000000000","expected_revision":4,"config_overrides":{"fec_percentage":37}}"#,
        ] {
            let value: Value = serde_json::from_slice(body).unwrap();
            assert!(
                validate(route, value.as_object().unwrap()).is_err(),
                "accepted invalid patch: {}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn catalog_frame_limiter_overrides_preserve_sparse_inheritance_and_field_domains() {
        let route = route("create");
        let empty = catalog_create_with_overrides(serde_json::json!({}));
        assert!(validate(route, empty.as_object().unwrap()).is_ok());

        for key in [
            "frame_limiter_enable",
            "frame_limiter_fps_limit",
            "frame_limiter_provider",
            "frame_limiter_disable_vsync",
            "frame_limiter_auto_virtual_framegen",
            "lossless_scaling_framegen",
            "lossless_scaling_target_fps",
            "frame_generation_provider",
            "frame_generation_mode",
            "rtss_frame_limit_type",
            "ds4_back_as_touchpad_click",
        ] {
            let mut inherited_overrides = serde_json::Map::new();
            inherited_overrides.insert(key.to_owned(), Value::Null);
            let inherited = catalog_create_with_overrides(Value::Object(inherited_overrides));
            assert!(
                validate(route, inherited.as_object().unwrap()).is_ok(),
                "rejected inherited Catalog override {key}"
            );
        }

        let explicit = catalog_create_with_overrides(serde_json::json!({
            "frame_limiter_enable": false,
            "frame_limiter_fps_limit": 0,
            "frame_limiter_provider": "auto",
            "frame_limiter_disable_vsync": false,
            "frame_limiter_auto_virtual_framegen": "disabled",
            "lossless_scaling_framegen": false,
            "ds4_back_as_touchpad_click": false,
        }));
        assert!(validate(route, explicit.as_object().unwrap()).is_ok());
        assert_eq!(
            explicit["entry"]["config_overrides"]["frame_limiter_enable"].as_bool(),
            Some(false)
        );
        assert_eq!(
            explicit["entry"]["config_overrides"]["frame_limiter_fps_limit"].as_f64(),
            Some(0.0)
        );
        assert_eq!(
            explicit["entry"]["config_overrides"]["ds4_back_as_touchpad_click"].as_bool(),
            Some(false)
        );
        assert_eq!(
            explicit["entry"]["config_overrides"]["lossless_scaling_framegen"].as_bool(),
            Some(false)
        );
        let enabled = catalog_create_with_overrides(serde_json::json!({
            "lossless_scaling_framegen": true
        }));
        assert!(validate(route, enabled.as_object().unwrap()).is_ok());
        assert_eq!(
            enabled["entry"]["config_overrides"]["lossless_scaling_framegen"].as_bool(),
            Some(true)
        );

        for fps in [1, 60, 1000] {
            let overrides = serde_json::json!({"lossless_scaling_target_fps": fps});
            assert!(catalog_config_overrides(&overrides).is_ok());
        }

        let valid_providers = {
            let mut providers = vec!["auto"];
            if cfg!(windows) {
                providers.extend(["rtss", "nvidia-control-panel", "none"]);
            } else if cfg!(target_os = "linux") {
                providers.extend(["mangohud", "proton", "mangohud-proton", "none"]);
            }
            providers
        };
        for provider in valid_providers {
            let overrides = serde_json::json!({"frame_limiter_provider": provider});
            assert!(
                catalog_config_overrides(&overrides).is_ok(),
                "rejected Catalog frame limiter provider {provider}"
            );
        }
        for mode in ["enabled", "vrr", "legacy", "disabled", "true", "false"] {
            let overrides = serde_json::json!({"frame_limiter_auto_virtual_framegen": mode});
            assert!(catalog_config_overrides(&overrides).is_ok());
        }
        for provider in ["lossless-scaling", "game-provided", "nvidia-smooth-motion"] {
            let overrides = serde_json::json!({"frame_generation_provider": provider});
            assert!(catalog_config_overrides(&overrides).is_ok());
        }
        for mode in [
            "off",
            "lossless-scaling",
            "game-provided",
            "nvidia-smooth-motion",
        ] {
            let overrides = serde_json::json!({"frame_generation_mode": mode});
            assert!(catalog_config_overrides(&overrides).is_ok());
        }
        for enabled in [true, false] {
            let overrides = serde_json::json!({"frame_limiter_auto_virtual_framegen": enabled});
            assert!(catalog_config_overrides(&overrides).is_ok());
        }
        if cfg!(windows) {
            for limit_type in [
                "async",
                "front edge sync",
                "back edge sync",
                "nvidia reflex",
            ] {
                let overrides = serde_json::json!({"rtss_frame_limit_type": limit_type});
                assert!(catalog_config_overrides(&overrides).is_ok());
            }
        }
        for fps_limit in [0.0, 59.94, 1000.0] {
            let overrides = serde_json::json!({"frame_limiter_fps_limit": fps_limit});
            assert!(catalog_config_overrides(&overrides).is_ok());
        }

        let unsupported_provider = if cfg!(windows) {
            "mangohud"
        } else if cfg!(target_os = "linux") {
            "rtss"
        } else {
            "proton"
        };
        let mut invalid_overrides = vec![
            serde_json::json!({"frame_limiter_enable":"false"}),
            serde_json::json!({"frame_limiter_fps_limit":"60"}),
            serde_json::json!({"frame_limiter_fps_limit":-0.001}),
            serde_json::json!({"frame_limiter_fps_limit":1000.001}),
            serde_json::json!({"frame_limiter_fps_limit":1.0001}),
            serde_json::json!({"frame_limiter_fps_limit":true}),
            serde_json::json!({"frame_limiter_provider":"unknown"}),
            serde_json::json!({"frame_limiter_provider":false}),
            serde_json::json!({"frame_limiter_provider":unsupported_provider}),
            serde_json::json!({"frame_limiter_disable_vsync":0}),
            serde_json::json!({"lossless_scaling_framegen":"true"}),
            serde_json::json!({"lossless_scaling_framegen":0}),
            serde_json::json!({"lossless_scaling_target_fps":0}),
            serde_json::json!({"lossless_scaling_target_fps":1001}),
            serde_json::json!({"lossless_scaling_target_fps":"60"}),
            serde_json::json!({"frame_generation_provider":"unknown"}),
            serde_json::json!({"frame_generation_provider":false}),
            serde_json::json!({"frame_generation_mode":"disabled"}),
            serde_json::json!({"frame_generation_mode":0}),
            serde_json::json!({"frame_limiter_auto_virtual_framegen":"vrrr"}),
            serde_json::json!({"frame_limiter_auto_virtual_framegen":1}),
            serde_json::json!({"frame_limiter_auto_virtual_framegen":[]}),
            serde_json::json!({"rtss_frame_limit_type":"unknown"}),
            serde_json::json!({"rtss_frame_limit_type":false}),
            serde_json::json!({"ds4_back_as_touchpad_click":"false"}),
            serde_json::json!({"ds4_back_as_touchpad_click":0}),
            serde_json::json!({"unrelated_override":true}),
            serde_json::json!({"dd_refresh_rate_option":"auto"}),
            serde_json::json!({"prefer_10bit_sdr":false}),
        ];
        if !cfg!(windows) {
            invalid_overrides.push(serde_json::json!({"rtss_frame_limit_type":"async"}));
        }
        for overrides in invalid_overrides {
            let request = catalog_create_with_overrides(overrides.clone());
            assert!(
                validate(route, request.as_object().unwrap()).is_err(),
                "accepted invalid Catalog frame limiter override: {overrides}"
            );
        }
    }

    #[test]
    fn catalog_routes_reject_unowned_fields_and_out_of_range_overrides() {
        let fullscreen = serde_json::json!({"expected_revision":0,"entry":{"name":"Fullscreen","launch":{"kind":"playnite_fullscreen","exit_timeout_seconds":0}}});
        assert!(validate(route("create"), fullscreen.as_object().unwrap()).is_ok());
        let invalid = [
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","owner":"other","launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","elevated":"true","launch":{"kind":"executable","program":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","elevated":true,"launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","elevated":true,"launch":{"kind":"playnite_fullscreen"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","config_overrides":{"fec_percentage":0},"launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","config_overrides":{"fec_percentage":256},"launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","config_overrides":{"fec_percentage":"37"},"launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","config_overrides":{"stream_audio":"false"},"launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","exclude_global_prep_cmd":"true","launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","exclude_global_prep_cmd":true,"exclude-global-prep-cmd":false,"launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","steam_id":"0","launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","steam_id":"0001","launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","steam_id":"4294967296","launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","steam_install_dir":"/","launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","steam_install_dir":"C:\\\\","launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","steam_install_dir":"\\game","launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","steam_install_dir":"/opt/../games","launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","steam_install_dir":"\\\\server\\share","launch":{"kind":"application","path":"/x"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","launch":{"kind":"executable","program":"/x","auto_detach":"false"}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","launch":{"kind":"executable","program":"/x","auto_detach":true,"auto-detach":false}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","launch":{"kind":"executable","program":"/x","exit-timeout":301}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","launch":{"kind":"executable","program":"/x","detached":["bad\ncommand"]}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","launch":{"kind":"playnite","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab","exit_timeout_seconds":10}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","launch":{"kind":"playnite_fullscreen","exit_timeout_seconds":301}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","launch":{"kind":"playnite_fullscreen","exit_timeout_seconds":-1}}}),
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","launch":{"kind":"playnite_fullscreen","exit_timeout_seconds":10,"path":"/forged"}}}),
            serde_json::json!({"id":"app-0000000000000001","expected_revision":0,"action":"delete"}),
        ];
        for value in invalid {
            assert!(
                validate(route("create"), value.as_object().unwrap()).is_err(),
                "accepted malformed catalog editor payload: {value}"
            );
        }
    }

    #[test]
    fn desktop_and_fullscreen_entries_preserve_supported_launch_and_display_fields() {
        for kind in ["desktop", "playnite_fullscreen"] {
            let mut request = serde_json::json!({"expected_revision":2,"entry":{
                "name":"Desktop", "display-output":"Virtual-1", "virtual-screen":false,
                "virtual-display-mode":"per_client", "virtual-display-layout":"extended",
                "prep_cmd":[{"do":"prepare","undo":"restore"}],
                "launch":{"kind":kind,"exit_timeout_seconds":30,"working_directory":"/home/player",
                    "preparation":[{"run":{"executable":"/usr/bin/true"}}]}
            }});
            assert!(validate(route("create"), request.as_object().unwrap()).is_ok());
            request["entry"]["launch"]["program"] = serde_json::json!("/forged");
            assert!(validate(route("create"), request.as_object().unwrap()).is_err());
            request["entry"]["launch"]
                .as_object_mut()
                .unwrap()
                .remove("program");
            request["entry"]["steam_id"] = serde_json::json!("123");
            assert!(validate(route("create"), request.as_object().unwrap()).is_err());
        }
        let aliases = serde_json::json!({"expected_revision":0,"entry":{
            "name":"Desktop", "display_output":"Display-1", "virtual_screen":true,
            "virtual_display_mode":"shared", "virtual_display_layout":"exclusive", "launch":{"kind":"desktop"}}});
        assert!(validate(route("create"), aliases.as_object().unwrap()).is_ok());
        let mut duplicate = aliases.clone();
        duplicate["entry"]["display-output"] = serde_json::json!("conflict");
        assert!(validate(route("create"), duplicate.as_object().unwrap()).is_err());
        let mut invalid_mode = aliases.clone();
        invalid_mode["entry"]["virtual_display_mode"] = serde_json::json!("unknown");
        assert!(validate(route("create"), invalid_mode.as_object().unwrap()).is_err());
        let mut oversized = aliases;
        oversized["entry"]["display_output"] = serde_json::json!("x".repeat(1025));
        assert!(validate(route("create"), oversized.as_object().unwrap()).is_err());
    }

    #[test]
    fn catalog_page_and_revision_payloads_are_bounded() {
        let list = serde_json::json!({
            "limit": 32,
            "cursor": {"revision": 5, "after": "app-0000000000000001", "query": "game"}
        });
        assert!(validate(route("list"), list.as_object().unwrap()).is_ok());
        for value in [
            serde_json::json!({"limit":33}),
            serde_json::json!({"limit":0}),
            serde_json::json!({"extra":true}),
            serde_json::json!({"cursor":{"revision":0,"after":"app-0000000000000000","query":""}}),
        ] {
            assert!(validate(route("list"), value.as_object().unwrap()).is_err());
        }
        let update = serde_json::json!({
            "id":"app-0000000000000001",
            "expected_revision":u64::MAX,
            "entry":{"name":"Updated","launch":{"kind":"application","path":"/opt/app"}}
        });
        assert!(validate(route("update"), update.as_object().unwrap()).is_ok());
        assert!(validate(route("delete"), update.as_object().unwrap()).is_err());
    }
}

#[cfg(test)]
mod software_encoder_tests {
    use super::*;
    use crate::routes::{payload, resolve};
    use serde_json::json;
    #[test]
    fn signed_video_settings_and_scopes_retain_exact_null_empty_and_combined_tunes() {
        for fields in [
            json!({"sw_preset":null,"sw_tune":null,"encoder":null,"min_threads":null,"qp":null}),
            json!({"sw_preset":"","sw_tune":"film,zerolatency","encoder":"software","min_threads":i32::MIN,"qp":i32::MAX}),
        ] {
            assert!(catalog_config_overrides(&fields).is_ok());
            let route = resolve("/v1/nvhttp/settings_update").unwrap();
            let mut request = json!({"hostname":"Host","port":47989,"expected_revision":4});
            request
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let forwarded = payload(route, request.to_string().as_bytes()).unwrap();
            for key in ["sw_preset", "sw_tune", "encoder", "min_threads", "qp"] {
                assert_eq!(forwarded[key], fields[key]);
            }
            let paired = resolve("/v1/pairing/update_client_config_overrides").unwrap();
            let request = json!({"certificate_id":"a".repeat(64),"expected_revision":4,"config_overrides":fields});
            assert!(payload(paired, request.to_string().as_bytes()).is_ok());
        }
        for fields in [
            json!({"sw_preset":false}),
            json!({"sw_tune":"x".repeat(257)}),
            json!({"encoder":"nvenc"}),
            json!({"min_threads":1.5}),
            json!({"min_threads":i64::from(i32::MAX) + 1}),
            json!({"qp":i64::from(i32::MIN) - 1}),
            json!({"qp":1.5}),
        ] {
            assert!(catalog_config_overrides(&fields).is_err());
            let mut request = json!({"hostname":"Host","port":47989,"expected_revision":4});
            request
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            assert!(
                payload(
                    resolve("/v1/nvhttp/settings_update").unwrap(),
                    request.to_string().as_bytes()
                )
                .is_err()
            );
        }
    }
}

#[cfg(test)]
mod host_metrics_tests {
    use crate::routes::{payload, resolve};

    #[test]
    fn settings_update_is_cas_versioned_strict_and_bounded() {
        let route = resolve("/v1/host-metrics/settings_update").unwrap();
        let valid = serde_json::json!({
            "settings": {
                "enabled": true,
                "poll_interval_ms": 2000,
                "history_retention_seconds": 300,
                "max_history_points": 300,
                "pause_when_hidden": true,
                "show_active_sessions": true,
                "show_host_stats": true,
                "show_host_charts": true,
                "show_session_history": true,
            },
            "expected_revision": 0,
        });
        let forwarded = payload(route, valid.to_string().as_bytes()).unwrap();
        assert_eq!(forwarded["action"], "settings_update");

        for invalid in [
            serde_json::json!({"poll_interval_ms":249}),
            serde_json::json!({"poll_interval_ms":60001}),
            serde_json::json!({"history_retention_seconds":29}),
            serde_json::json!({"max_history_points":2001}),
            serde_json::json!({"enabled":"true"}),
            serde_json::json!({"extra":1}),
        ] {
            let mut request = valid.clone();
            request["settings"]
                .as_object_mut()
                .unwrap()
                .extend(invalid.as_object().unwrap().clone());
            assert!(
                payload(route, request.to_string().as_bytes()).is_err(),
                "{request}"
            );
        }
        let mut missing_revision = valid;
        missing_revision
            .as_object_mut()
            .unwrap()
            .remove("expected_revision");
        assert!(payload(route, missing_revision.to_string().as_bytes()).is_err());
    }

    #[test]
    fn stats_request_is_explicitly_empty() {
        let route = resolve("/v1/host-metrics/stats").unwrap();
        assert_eq!(payload(route, b"{}").unwrap()["action"], "stats_read");
        assert!(payload(route, br#"{"limit":1}"#).is_err());
    }
}

#[cfg(test)]
mod updater_and_tray_tests {
    use crate::routes::{payload, resolve};

    #[test]
    fn updater_settings_update_is_strict_and_bounds_seconds() {
        let route = resolve("/v1/updater/settings_update").unwrap();
        let valid = serde_json::json!({
            "settings": {"notify_pre_releases": false, "update_check_interval": 0},
            "expected_revision": 5,
        });
        assert_eq!(
            payload(route, valid.to_string().as_bytes()).unwrap()["action"],
            "settings_update"
        );
        for settings in [
            serde_json::json!({"notify_pre_releases":false,"update_check_interval":-1}),
            serde_json::json!({"notify_pre_releases":false,"update_check_interval":2147483648u64}),
            serde_json::json!({"notify_pre_releases":"false","update_check_interval":60}),
            serde_json::json!({"notify_pre_releases":false,"update_check_interval":60,"extra":true}),
        ] {
            let mut request = valid.clone();
            request["settings"] = settings;
            assert!(payload(route, request.to_string().as_bytes()).is_err());
        }
        let mut invalid_revision = valid;
        invalid_revision["expected_revision"] = serde_json::json!(-1);
        assert!(payload(route, invalid_revision.to_string().as_bytes()).is_err());
    }

    #[test]
    fn tray_settings_update_requires_one_boolean_and_revision() {
        let route = resolve("/v1/system-tray/settings_update").unwrap();
        let valid = serde_json::json!({
            "settings": {"system_tray": true},
            "expected_revision": 0,
        });
        assert_eq!(
            payload(route, valid.to_string().as_bytes()).unwrap()["action"],
            "settings_update"
        );
        for settings in [
            serde_json::json!({}),
            serde_json::json!({"system_tray":"true"}),
            serde_json::json!({"system_tray":true,"extra":false}),
        ] {
            let mut request = valid.clone();
            request["settings"] = settings;
            assert!(payload(route, request.to_string().as_bytes()).is_err());
        }
        let mut missing_revision = valid;
        missing_revision
            .as_object_mut()
            .unwrap()
            .remove("expected_revision");
        assert!(payload(route, missing_revision.to_string().as_bytes()).is_err());
    }
}
