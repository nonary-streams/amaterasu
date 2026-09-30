//! Explicit product API surface; never accepts an arbitrary runtime command.
use serde_json::Value;

#[derive(Clone, Copy)]
pub struct Route {
    pub service: &'static str,
    pub action: &'static str,
    pub scope: Option<&'static str>,
    pub requires_empty_body: bool,
}

pub fn resolve(path: &str) -> Option<Route> {
    let (domain, action) = path.strip_prefix("/v1/")?.split_once('/')?;
    let (service, action, scope) = match (domain, action) {
        ("auth", "login") => ("amaterasu.authentication/1", "login", None),
        ("auth", "refresh") => ("amaterasu.authentication/1", "refresh", None),
        ("auth", "logout") => ("amaterasu.authentication/1", "logout", None),
        ("auth", "settings") => (
            "amaterasu.authentication/1",
            "settings_get",
            Some("configuration.read"),
        ),
        ("auth", "settings_update") => (
            "amaterasu.authentication/1",
            "settings_update",
            Some("configuration.write"),
        ),
        ("management_settings", "get") => (
            "amaterasu.management/1",
            "product_get",
            Some("configuration.read"),
        ),
        ("management_settings", "update") => (
            "amaterasu.management/1",
            "product_update",
            Some("configuration.write"),
        ),
        ("catalog", action) => (
            "amaterasu.catalog/1",
            catalog_action(action)?,
            Some(if matches!(action, "get" | "list" | "search") {
                "catalog.read"
            } else {
                "catalog.write"
            }),
        ),
        ("configuration", "get") => (
            "amaterasu.configuration/1",
            "get",
            Some("configuration.read"),
        ),
        ("configuration", "register") => (
            "amaterasu.configuration/1",
            "register",
            Some("configuration.write"),
        ),
        ("configuration", "update") => (
            "amaterasu.configuration/1",
            "update",
            Some("configuration.write"),
        ),
        ("configuration", "migrate") => (
            "amaterasu.configuration/1",
            "migrate",
            Some("configuration.write"),
        ),
        ("host-configuration", "get") => (
            "amaterasu.host-configuration/1",
            "get",
            Some("configuration.read"),
        ),
        ("host-configuration", "update") => (
            "amaterasu.host-configuration/1",
            "update",
            Some("configuration.write"),
        ),
        ("nvhttp", "settings") => (
            "amaterasu.nvhttp/1",
            "settings_get",
            Some("configuration.read"),
        ),
        ("nvhttp", "settings_update") => (
            "amaterasu.nvhttp/1",
            "settings_update",
            Some("configuration.write"),
        ),
        ("session", action) => (
            "amaterasu.session/1",
            session_action(action)?,
            Some(if matches!(action, "list" | "snapshot") {
                "session.read"
            } else {
                "session.write"
            }),
        ),
        ("application", "list") => ("amaterasu.application/1", "list", Some("application.read")),
        ("application", "status") => (
            "amaterasu.application/1",
            "status",
            Some("application.read"),
        ),
        ("application", "launch") => (
            "amaterasu.application/1",
            "launch",
            Some("application.launch"),
        ),
        ("application", "terminate") => (
            "amaterasu.application/1",
            "terminate",
            Some("application.terminate"),
        ),
        ("steam", "list") => ("amaterasu.steam/1", "list", Some("steam.read")),
        ("steam", "artwork_read") => ("amaterasu.steam/1", "artwork_read", Some("steam.read")),
        ("steam", "status") => ("amaterasu.steam/1", "status", Some("steam.read")),
        ("steam", "refresh") => ("amaterasu.steam/1", "refresh", Some("steam.refresh")),
        ("steam", "force_sync") => ("amaterasu.steam/1", "force_sync", Some("steam.refresh")),
        ("steam", "settings") => (
            "amaterasu.steam/1",
            "settings_get",
            Some("configuration.read"),
        ),
        ("steam", "settings_update") => (
            "amaterasu.steam/1",
            "settings_update",
            Some("configuration.write"),
        ),
        ("lutris", "settings") => (
            "amaterasu.lutris/1",
            "settings_get",
            Some("configuration.read"),
        ),
        ("lutris", "settings_update") => (
            "amaterasu.lutris/1",
            "settings_update",
            Some("configuration.write"),
        ),
        ("lutris", "status") => ("amaterasu.lutris/1", "status", Some("configuration.read")),
        ("lutris", "refresh") => ("amaterasu.lutris/1", "refresh", Some("configuration.write")),
        ("lutris", "artwork_read") => ("amaterasu.lutris/1", "artwork_read", Some("lutris.read")),
        ("playnite", "settings") => (
            "amaterasu.playnite/1",
            "settings_get",
            Some("configuration.read"),
        ),
        ("playnite", "settings_update") => (
            "amaterasu.playnite/1",
            "settings_update",
            Some("configuration.write"),
        ),
        ("playnite", "status") => ("amaterasu.playnite/1", "status", Some("configuration.read")),
        ("playnite", "refresh") => (
            "amaterasu.playnite/1",
            "refresh",
            Some("configuration.write"),
        ),
        ("input", "settings_get") => (
            "amaterasu.input/1",
            "settings_get",
            Some("configuration.read"),
        ),
        ("input", "status") => ("amaterasu.input/1", "status", Some("configuration.read")),
        ("input", "settings_update") => (
            "amaterasu.input/1",
            "settings_update",
            Some("configuration.write"),
        ),
        ("history", action) => {
            let (action, scope) = history_action(action)?;
            ("amaterasu.session-history/1", action, Some(scope))
        }
        ("host-metrics", "settings") => (
            "amaterasu.host-metrics/1",
            "settings_get",
            Some("configuration.read"),
        ),
        ("host-metrics", "settings_update") => (
            "amaterasu.host-metrics/1",
            "settings_update",
            Some("configuration.write"),
        ),
        ("host-metrics", "stats") => (
            "amaterasu.host-metrics/1",
            "stats_read",
            Some("diagnostics.read"),
        ),
        ("updater", "settings") => (
            "amaterasu.updater/1",
            "settings_get",
            Some("configuration.read"),
        ),
        ("updater", "settings_update") => (
            "amaterasu.updater/1",
            "settings_update",
            Some("configuration.write"),
        ),
        ("updater", "status") => ("amaterasu.updater/1", "status", Some("configuration.read")),
        ("updater", "check") => ("amaterasu.updater/1", "check", Some("configuration.write")),
        ("system-tray", "settings") => (
            "amaterasu.system-tray/1",
            "settings_get",
            Some("configuration.read"),
        ),
        ("system-tray", "settings_update") => (
            "amaterasu.system-tray/1",
            "settings_update",
            Some("configuration.write"),
        ),
        ("diagnostics", "query") => ("amaterasu.diagnostics/1", "query", Some("diagnostics.read")),
        ("diagnostics", "status") => (
            "amaterasu.diagnostics/1",
            "status",
            Some("diagnostics.read"),
        ),
        ("diagnostics", "refresh_logging") => (
            "amaterasu.diagnostics/1",
            "refresh_logging",
            Some("configuration.write"),
        ),
        ("diagnostics", "counters") => (
            "amaterasu.diagnostics/1",
            "counters",
            Some("diagnostics.read"),
        ),
        ("diagnostics", "record") => (
            "amaterasu.diagnostics/1",
            "record",
            Some("diagnostics.write"),
        ),
        ("diagnostics", "increment") => (
            "amaterasu.diagnostics/1",
            "increment",
            Some("diagnostics.write"),
        ),
        ("launch", "start") => ("amaterasu.launch/1", "launch", Some("application.launch")),
        ("launch", "status") => ("amaterasu.launch/1", "status", Some("application.read")),
        ("launch", "list") => ("amaterasu.launch/1", "list", Some("application.read")),
        ("launch", "stop") => ("amaterasu.launch/1", "stop", Some("application.terminate")),
        ("pairing", action) => {
            let action = pairing_action(action)?;
            ("amaterasu.pairing/1", action, Some("pairing.admin"))
        }
        ("display", action) => {
            let (action, scope) = display_action(action)?;
            ("amaterasu.display/1", action, Some(scope))
        }
        ("media", action) => {
            let (service, action) = media_discovery(action)?;
            (service, action, Some("media.read"))
        }
        ("capture", "pipewire_status") => (
            "amaterasu.capture-pipewire/1",
            "status",
            Some("capture.read"),
        ),
        ("capture", "pipewire_selection_begin") => (
            "amaterasu.capture-pipewire/1",
            "select_source",
            Some("capture.write"),
        ),
        ("capture", "pipewire_selection_poll") => (
            "amaterasu.capture-pipewire/1",
            "poll_selection",
            Some("capture.read"),
        ),
        ("capture", "pipewire_selection_cancel") => (
            "amaterasu.capture-pipewire/1",
            "cancel_selection",
            Some("capture.write"),
        ),
        _ => return None,
    };
    Some(Route {
        service,
        action,
        scope,
        requires_empty_body: domain == "media"
            || (domain == "steam" && action == "force_sync")
            || (domain == "host-metrics" && action == "stats_read"),
    })
}

fn media_discovery(action: &str) -> Option<(&'static str, &'static str)> {
    match action {
        "dxgi_outputs" => Some(("amaterasu.capture-dxgi/1", "enumerate")),
        "wgc_outputs" => Some(("amaterasu.capture-wgc/1", "enumerate")),
        "audio_endpoints" => Some(("amaterasu.audio/1", "enumerate")),
        "audio_driver_status" => Some(("amaterasu.audio/1", "status")),
        "encoders" => Some(("amaterasu.encode/1", "enumerate")),
        "audio_encoders" => Some(("amaterasu.audio-encode/1", "enumerate")),
        "video_status" => Some(("amaterasu.video/1", "status")),
        _ => None,
    }
}

fn history_action(action: &str) -> Option<(&'static str, &'static str)> {
    match action {
        "settings_get" => Some(("settings_get", "configuration.read")),
        "settings_update" => Some(("settings_update", "configuration.write")),
        "list_sessions" => Some(("list_sessions", "session.read")),
        "session_detail" => Some(("session_detail", "session.read")),
        "active_sessions" => Some(("active_sessions", "session.read")),
        "history_status" => Some(("history_status", "session.read")),
        "delete_session" => Some(("delete_session", "session.write")),
        _ => None,
    }
}

fn catalog_action(action: &str) -> Option<&'static str> {
    match action {
        "create" => Some("create"),
        "get" => Some("get"),
        "list" => Some("list"),
        "search" => Some("search"),
        "update" => Some("update"),
        "update_provider_overrides" => Some("update_provider_overrides"),
        "delete" => Some("delete"),
        _ => None,
    }
}

fn session_action(action: &str) -> Option<&'static str> {
    match action {
        "create" => Some("create"),
        "activate" => Some("activate"),
        "stop" => Some("stop"),
        "finish_stop" => Some("finish_stop"),
        "cancel" => Some("cancel"),
        "fail" => Some("fail"),
        "forget" => Some("forget"),
        "acquire" => Some("acquire"),
        "release" => Some("release"),
        "list" => Some("list"),
        "snapshot" => Some("snapshot"),
        _ => None,
    }
}

fn pairing_action(action: &str) -> Option<&'static str> {
    match action {
        "identity" => Some("identity"),
        "pending" => Some("pending"),
        "list_clients" => Some("list_clients"),
        "approve_pin" => Some("approve_pin"),
        "unpair" => Some("unpair"),
        "update_client_config_overrides" => Some("update_client_config_overrides"),
        _ => None,
    }
}

fn display_action(action: &str) -> Option<(&'static str, &'static str)> {
    match action {
        "status" => Some(("status", "display.read")),
        "enumerate" => Some(("enumerate", "display.read")),
        "apply" => Some(("apply", "display.write")),
        "restore" => Some(("restore", "display.write")),
        "recover" => Some(("recover", "display.write")),
        _ => None,
    }
}

pub fn payload(route: Route, bytes: &[u8]) -> Result<Value, &'static str> {
    let mut value: Value = serde_json::from_slice(bytes).map_err(|_| "body must contain JSON")?;
    let object = value.as_object_mut().ok_or("body must be a JSON object")?;
    if object.contains_key("action") {
        return Err("the route defines the action");
    }
    crate::route_payload::validate(route, object)?;
    object.insert("action".into(), route.action.into());
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lutris_artwork_route_pins_provider_action_and_read_scope() {
        let route = resolve("/v1/lutris/artwork_read").unwrap();
        assert_eq!(route.service, "amaterasu.lutris/1");
        assert_eq!(route.action, "artwork_read");
        assert_eq!(route.scope, Some("lutris.read"));
        assert!(!route.requires_empty_body);
        assert!(resolve("/v1/lutris/artwork_get").is_none());
        assert!(resolve("/v1/lutris/artwork_update").is_none());
    }

    #[test]
    fn history_routes_expose_only_management_operations_with_bounded_fields() {
        for (action, scope) in [
            ("settings_get", "configuration.read"),
            ("settings_update", "configuration.write"),
            ("list_sessions", "session.read"),
            ("session_detail", "session.read"),
            ("active_sessions", "session.read"),
            ("history_status", "session.read"),
            ("delete_session", "session.write"),
        ] {
            let route = resolve(&format!("/v1/history/{action}")).unwrap();
            assert_eq!(route.service, "amaterasu.session-history/1");
            assert_eq!(route.scope, Some(scope));
        }
        for action in [
            "session_begin",
            "session_end",
            "record_sample",
            "record_event",
        ] {
            assert!(resolve(&format!("/v1/history/{action}")).is_none());
        }

        let update = resolve("/v1/history/settings_update").unwrap();
        let valid = br#"{"settings":{"enabled":true,"ttl_days":0,"db_size_limit_mb":2147483647},"expected_revision":0}"#;
        assert_eq!(payload(update, valid).unwrap()["action"], "settings_update");
        for invalid in [
            br#"{"settings":{"enabled":true,"ttl_days":-1,"db_size_limit_mb":0},"expected_revision":0}"#.as_slice(),
            br#"{"settings":{"enabled":true,"ttl_days":2147483648,"db_size_limit_mb":0},"expected_revision":0}"#,
            br#"{"settings":{"enabled":true,"ttl_days":0,"db_size_limit_mb":0,"extra":1},"expected_revision":0}"#,
            br#"{"settings":{"enabled":true,"ttl_days":0,"db_size_limit_mb":0},"expected_revision":-1}"#,
        ] {
            assert!(payload(update, invalid).is_err());
        }

        let list = resolve("/v1/history/list_sessions").unwrap();
        assert!(payload(list, br#"{"limit":25,"offset":0}"#).is_ok());
        assert!(payload(list, br#"{"limit":26}"#).is_err());
        let active = resolve("/v1/history/active_sessions").unwrap();
        assert_eq!(active.scope, Some("session.read"));
        assert_eq!(
            payload(active, br#"{}"#).unwrap()["action"],
            "active_sessions"
        );
        let second_page = payload(active, br#"{"limit":16,"offset":12}"#).unwrap();
        assert_eq!(second_page["action"], "active_sessions");
        assert_eq!(second_page["limit"], 16);
        assert_eq!(second_page["offset"], 12);
        for invalid in [
            br#"{"offset":-1}"#.as_slice(),
            br#"{"offset":1.5}"#,
            br#"{"offset":"12"}"#,
            br#"{"offset":null}"#,
            br#"{"offset":true}"#,
            br#"{"limit":17,"offset":12}"#,
            br#"{"offset":12,"unrelated":true}"#,
        ] {
            assert!(payload(active, invalid).is_err());
        }
        let detail = resolve("/v1/history/session_detail").unwrap();
        assert!(payload(detail, br#"{"session_id":"session-1"}"#).is_ok());
        assert!(payload(detail, br#"{"session_id":""}"#).is_err());
    }

    #[test]
    fn host_metrics_routes_expose_settings_and_empty_body_stats_read() {
        for (path, action, scope) in [
            (
                "/v1/host-metrics/settings",
                "settings_get",
                "configuration.read",
            ),
            (
                "/v1/host-metrics/settings_update",
                "settings_update",
                "configuration.write",
            ),
            ("/v1/host-metrics/stats", "stats_read", "diagnostics.read"),
        ] {
            let route = resolve(path).unwrap();
            assert_eq!(route.service, "amaterasu.host-metrics/1");
            assert_eq!(route.action, action);
            assert_eq!(route.scope, Some(scope));
            assert_eq!(route.requires_empty_body, action == "stats_read");
        }
        assert!(resolve("/v1/host-metrics/stream_activity_begin").is_none());
        assert!(resolve("/v1/host-metrics/stream_activity_end").is_none());
    }

    #[test]
    fn updater_and_tray_routes_expose_only_management_settings_and_actions() {
        for (path, service, action, scope) in [
            (
                "/v1/updater/settings",
                "amaterasu.updater/1",
                "settings_get",
                "configuration.read",
            ),
            (
                "/v1/updater/settings_update",
                "amaterasu.updater/1",
                "settings_update",
                "configuration.write",
            ),
            (
                "/v1/updater/status",
                "amaterasu.updater/1",
                "status",
                "configuration.read",
            ),
            (
                "/v1/updater/check",
                "amaterasu.updater/1",
                "check",
                "configuration.write",
            ),
            (
                "/v1/system-tray/settings",
                "amaterasu.system-tray/1",
                "settings_get",
                "configuration.read",
            ),
            (
                "/v1/system-tray/settings_update",
                "amaterasu.system-tray/1",
                "settings_update",
                "configuration.write",
            ),
        ] {
            let route = resolve(path).unwrap();
            assert_eq!(route.service, service);
            assert_eq!(route.action, action);
            assert_eq!(route.scope, Some(scope));
        }
        assert!(resolve("/v1/system-tray/events").is_none());
    }

    #[test]
    fn diagnostics_status_is_read_scoped_and_accepts_only_an_empty_object() {
        let route = resolve("/v1/diagnostics/status").unwrap();
        assert_eq!(route.service, "amaterasu.diagnostics/1");
        assert_eq!(route.scope, Some("diagnostics.read"));
        assert_eq!(payload(route, b"{}").unwrap()["action"], "status");
        for body in [
            br#"{"scope":"diagnostics.write"}"#.as_slice(),
            br#"{"action":"query"}"#,
            br#"{"limit":1}"#,
        ] {
            assert!(payload(route, body).is_err());
        }
    }

    #[test]
    fn diagnostics_query_and_counters_accept_only_bounded_read_payloads() {
        let query = resolve("/v1/diagnostics/query").unwrap();
        let counters = resolve("/v1/diagnostics/counters").unwrap();
        assert_eq!(query.scope, Some("diagnostics.read"));
        assert_eq!(counters.scope, Some("diagnostics.read"));
        assert_eq!(
            payload(query, br#"{"after":7,"limit":4}"#).unwrap()["after"],
            7
        );
        assert!(payload(query, b"{}").is_ok());
        assert!(payload(counters, b"{}").is_ok());
        for body in [
            br#"{"after":-1}"#.as_slice(),
            br#"{"after":"7"}"#,
            br#"{"limit":0}"#,
            br#"{"limit":5}"#,
            br#"{"limit":1.5}"#,
            br#"{"scope":"diagnostics.write"}"#,
        ] {
            assert!(payload(query, body).is_err());
            assert!(payload(counters, body).is_err());
        }
    }

    #[test]
    fn diagnostics_logging_refresh_requires_configuration_write_and_a_positive_revision() {
        let route = resolve("/v1/diagnostics/refresh_logging").unwrap();
        assert_eq!(route.service, "amaterasu.diagnostics/1");
        assert_eq!(route.scope, Some("configuration.write"));
        assert_eq!(
            payload(route, br#"{"minimum_revision":7}"#).unwrap()["action"],
            "refresh_logging"
        );
        for body in [
            br#"{}"#.as_slice(),
            br#"{"minimum_revision":0}"#,
            br#"{"minimum_revision":-1}"#,
            br#"{"minimum_revision":1.5}"#,
            br#"{"minimum_revision":"7"}"#,
            br#"{"minimum_revision":7,"scope":"diagnostics.write"}"#,
        ] {
            assert!(payload(route, body).is_err());
        }
    }

    #[test]
    fn media_discovery_requires_its_scope_and_cannot_forward_native_commands() {
        for action in [
            "dxgi_outputs",
            "wgc_outputs",
            "audio_endpoints",
            "audio_driver_status",
            "encoders",
            "audio_encoders",
            "video_status",
        ] {
            let route = resolve(&format!("/v1/media/{action}")).unwrap();
            assert_eq!(route.scope, Some("media.read"));
            assert!(payload(route, b"{}").is_ok());
            for body in [
                br#"{"service":"other/1"}"#.as_slice(),
                br#"{"factory":1}"#,
                br#"{"stream":1}"#,
            ] {
                assert!(payload(route, body).is_err());
            }
        }
        for action in [
            "open",
            "close",
            "native_bind",
            "native_register",
            "read_audio",
        ] {
            assert!(resolve(&format!("/v1/media/{action}")).is_none());
        }
    }

    #[test]
    fn coordinated_launch_routes_pin_action_and_reject_process_overrides() {
        let start = resolve("/v1/launch/start").unwrap();
        assert_eq!(start.service, "amaterasu.launch/1");
        assert_eq!(start.action, "launch");
        assert_eq!(start.scope, Some("application.launch"));
        let valid =
            serde_json::json!({"operation_id":"browser.launch.1","app_id":"app-0000000000000001"});
        assert_eq!(
            payload(start, &serde_json::to_vec(&valid).unwrap()).unwrap()["action"],
            "launch"
        );
        for field in [
            "executable",
            "session_id",
            "process_id",
            "caller",
            "arguments",
            "environment",
        ] {
            let mut invalid = valid.clone();
            invalid[field] = "override".into();
            assert!(payload(start, &serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        for (action, scope) in [
            ("status", "application.read"),
            ("list", "application.read"),
            ("stop", "application.terminate"),
        ] {
            assert_eq!(
                resolve(&format!("/v1/launch/{action}")).unwrap().scope,
                Some(scope)
            );
        }
        assert!(payload(resolve("/v1/launch/list").unwrap(), br#"{"limit":17}"#).is_err());
        assert!(
            payload(
                resolve("/v1/launch/stop").unwrap(),
                br#"{"launch_id":1,"process_id":1}"#
            )
            .is_err()
        );
        assert!(resolve("/v1/launch/resolve_launch").is_none());
    }

    #[test]
    fn provider_override_route_is_catalog_write_only_and_pins_its_action() {
        let route = resolve("/v1/catalog/update_provider_overrides").unwrap();
        assert_eq!(route.service, "amaterasu.catalog/1");
        assert_eq!(route.action, "update_provider_overrides");
        assert_eq!(route.scope, Some("catalog.write"));
        assert!(payload(
            route,
            br#"{"id":"app-0000000000000001","expected_revision":4,"config_overrides":{"fec_percentage":null}}"#,
        )
        .is_ok());
        assert!(resolve("/v1/catalog/provider_overrides").is_none());
    }

    #[test]
    fn pairing_admin_routes_exclude_identity_import_and_transport_transcripts() {
        for action in [
            "identity",
            "pending",
            "list_clients",
            "approve_pin",
            "unpair",
            "update_client_config_overrides",
        ] {
            let route = resolve(&format!("/v1/pairing/{action}")).unwrap();
            assert_eq!(route.service, "amaterasu.pairing/1");
            assert_eq!(route.scope, Some("pairing.admin"));
        }
        for action in [
            "initialize",
            "import_identity",
            "import_client",
            "begin",
            "cancel",
            "get_server_cert",
            "client_challenge",
            "server_challenge_response",
            "client_pairing_secret",
            "check_client_certificate",
        ] {
            assert!(resolve(&format!("/v1/pairing/{action}")).is_none());
        }
        let approve = resolve("/v1/pairing/approve_pin").unwrap();
        assert!(payload(approve, br#"{"pairing_id":"pending-id","pin":"0123"}"#).is_ok());
        for body in [
            br#"{"pairing_id":"pending-id","pin":1234}"#.as_slice(),
            br#"{"pairing_id":"pending-id","pin":"1234","private_key_pem":"secret"}"#,
            br#"{"pairing_id":"pending-id","pin":"1234","caller":"administrator"}"#,
            br#"{"pairing_id":"pending-id","pin":"1234","action":"initialize"}"#,
        ] {
            assert!(payload(approve, body).is_err());
        }
        assert!(
            payload(
                resolve("/v1/pairing/identity").unwrap(),
                br#"{"private_key":true}"#
            )
            .is_err()
        );
        let update = resolve("/v1/pairing/update_client_config_overrides").unwrap();
        assert!(payload(
            update,
            br#"{"certificate_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expected_revision":7,"config_overrides":{"fec_percentage":37}}"#,
        ).is_ok());
        assert!(payload(
            update,
            br#"{"certificate_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expected_revision":7,"config_overrides":{"fec_percentage":null}}"#,
        ).is_ok());
        assert!(payload(
            update,
            br#"{"certificate_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expected_revision":7,"config_overrides":{"fec_percentage":37,"stream_audio":false}}"#,
        ).is_ok());
        assert!(payload(
            update,
            br#"{"certificate_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expected_revision":7,"config_overrides":{}}"#,
        ).is_ok());
        for body in [
            br#"{"certificate_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expected_revision":7,"client_id":"forged","config_overrides":{"fec_percentage":37}}"#.as_slice(),
            br#"{"certificate_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expected_revision":7,"config_overrides":{"fec_percentage":0}}"#,
            br#"{"certificate_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expected_revision":7,"config_overrides":{"fec_percentage":256}}"#,
            br#"{"certificate_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expected_revision":7,"config_overrides":{"fec_percentage":"37"}}"#,
            br#"{"certificate_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expected_revision":7,"config_overrides":{"stream_audio":"false"}}"#,
            br#"{"certificate_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expected_revision":7,"config_overrides":{"fec_percentage":37,"unknown":true}}"#,
            br#"{"certificate_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expected_revision":7,"config_overrides":null}"#,
            br#"{"certificate_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expected_revision":-1,"config_overrides":{"fec_percentage":37}}"#,
            br#"{"certificate_id":"not-a-fingerprint","expected_revision":7,"config_overrides":{"fec_percentage":37}}"#,
        ] {
            assert!(payload(update, body).is_err());
        }
    }

    #[test]
    fn keybindings_routes_preserve_order_empty_and_null_without_accepting_authority_fields() {
        for path in [
            "/v1/catalog/update_provider_overrides",
            "/v1/pairing/update_client_config_overrides",
        ] {
            let route = resolve(path).unwrap();
            let request = if path.contains("catalog") {
                serde_json::json!({"id":"app-0000000000000001","expected_revision":7})
            } else {
                serde_json::json!({"certificate_id":"a".repeat(64),"expected_revision":7})
            };
            for selected in [
                serde_json::json!(null),
                serde_json::json!([]),
                serde_json::json!([0, 255, 65, 66, 65, 67]),
                serde_json::json!(vec![0_u8; 512]),
            ] {
                let mut body = request.clone();
                body["config_overrides"] = serde_json::json!({"keybindings":selected});
                assert_eq!(
                    payload(route, &serde_json::to_vec(&body).unwrap()).unwrap()["config_overrides"]
                        ["keybindings"],
                    selected
                );
                for authority in ["caller", "owner", "client_id", "certificate_id"] {
                    let mut forged = body.clone();
                    forged["config_overrides"][authority] = serde_json::json!("forged");
                    assert!(
                        payload(route, &serde_json::to_vec(&forged).unwrap()).is_err(),
                        "{forged}"
                    );
                }
            }
            for invalid in [
                serde_json::json!([65]),
                serde_json::json!([-1, 0]),
                serde_json::json!([0, 256]),
                serde_json::json!([65.0, 66]),
                serde_json::json!([true, 66]),
                serde_json::json!(["65", 66]),
                serde_json::json!({}),
                serde_json::json!(vec![0_u8; 514]),
            ] {
                let mut body = request.clone();
                body["config_overrides"] = serde_json::json!({"keybindings":invalid});
                assert!(
                    payload(route, &serde_json::to_vec(&body).unwrap()).is_err(),
                    "{body}"
                );
            }
        }
        let global = resolve("/v1/input/settings_update").unwrap();
        for selected in [
            serde_json::json!([]),
            serde_json::json!([0, 255, 65, 66, 65, 67]),
            serde_json::json!(vec![0_u8; 512]),
        ] {
            let body = serde_json::json!({"keyboard":true,"mouse":true,"expected_revision":0,"keybindings":selected});
            assert_eq!(
                payload(global, &serde_json::to_vec(&body).unwrap()).unwrap()["keybindings"],
                selected
            );
        }
        for invalid in [
            serde_json::json!(null),
            serde_json::json!([65]),
            serde_json::json!([-1, 0]),
            serde_json::json!([0, 256]),
            serde_json::json!([65.0, 66]),
            serde_json::json!([true, 66]),
            serde_json::json!(["65", 66]),
            serde_json::json!({}),
            serde_json::json!(vec![0_u8; 514]),
        ] {
            let body = serde_json::json!({"keyboard":true,"mouse":true,"expected_revision":0,"keybindings":invalid});
            assert!(
                payload(global, &serde_json::to_vec(&body).unwrap()).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn scoped_advanced_input_routes_preserve_inheritance_false_and_zero() {
        for path in [
            "/v1/catalog/update_provider_overrides",
            "/v1/pairing/update_client_config_overrides",
        ] {
            let route = resolve(path).unwrap();
            let request = if path.contains("catalog") {
                serde_json::json!({"id":"app-0000000000000001", "expected_revision":7})
            } else {
                serde_json::json!({"certificate_id":"a".repeat(64), "expected_revision":7})
            };
            for overrides in [
                serde_json::json!({}),
                serde_json::json!({
                    "high_resolution_scrolling":false,"always_send_scancodes":false,
                    "key_rightalt_to_key_win":true,"key_repeat_delay":0,"key_repeat_frequency":0.0,
                }),
                serde_json::json!({
                    "high_resolution_scrolling":null,"always_send_scancodes":null,
                    "key_rightalt_to_key_win":null,"key_repeat_delay":null,"key_repeat_frequency":null,
                }),
            ] {
                let mut body = request.clone();
                body["config_overrides"] = overrides.clone();
                assert_eq!(
                    payload(route, &serde_json::to_vec(&body).unwrap()).unwrap()["config_overrides"],
                    overrides
                );
            }
            for (key, value) in [
                ("high_resolution_scrolling", serde_json::json!("false")),
                ("always_send_scancodes", serde_json::json!(1)),
                ("key_rightalt_to_key_win", serde_json::json!([])),
                ("key_repeat_delay", serde_json::json!(-1)),
                ("key_repeat_delay", serde_json::json!(0.5)),
                ("key_repeat_delay", serde_json::json!(2147483648_u64)),
                ("key_repeat_frequency", serde_json::json!(-0.1)),
                ("key_repeat_frequency", serde_json::json!("24.9")),
            ] {
                let mut body = request.clone();
                body["config_overrides"] = serde_json::json!({key:value});
                assert!(
                    payload(route, &serde_json::to_vec(&body).unwrap()).is_err(),
                    "{body}"
                );
            }
        }
    }

    #[test]
    fn display_write_payloads_reject_nested_authority_and_unknown_mode_fields() {
        for action in ["status", "enumerate", "apply", "restore", "recover"] {
            let route = resolve(&format!("/v1/display/{action}")).unwrap();
            assert_eq!(route.service, "amaterasu.display/1");
            assert_eq!(
                route.scope,
                Some(if matches!(action, "status" | "enumerate") {
                    "display.read"
                } else {
                    "display.write"
                })
            );
        }
        let apply = resolve("/v1/display/apply").unwrap();
        let valid = serde_json::json!({"configuration":{"displays":[{"id":"monitor","width":1920,"height":1080,"refresh":{"numerator":60000,"denominator":1001},"x":0,"y":0,"rotation":"identity"}]}});
        assert!(payload(apply, &serde_json::to_vec(&valid).unwrap()).is_ok());
        for location in ["configuration", "display", "refresh"] {
            let mut invalid = valid.clone();
            match location {
                "configuration" => invalid["configuration"]["owner"] = "other".into(),
                "display" => {
                    invalid["configuration"]["displays"][0]["allow_mutation"] = true.into()
                }
                _ => invalid["configuration"]["displays"][0]["refresh"]["force"] = true.into(),
            }
            assert!(payload(apply, &serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        let enumerate = resolve("/v1/display/enumerate").unwrap();
        assert!(payload(enumerate, br#"{"display":null,"offset":0,"limit":32}"#).is_ok());
        assert!(payload(enumerate, br#"{"limit":33}"#).is_err());
        let restore = resolve("/v1/display/restore").unwrap();
        assert!(payload(restore, br#"{"lease":"display-0000000000000001"}"#).is_ok());
        assert!(
            payload(
                restore,
                br#"{"lease":"display-0000000000000001","owner":"other"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn configuration_migration_requires_write_authority_and_cannot_be_retargeted() {
        let route = resolve("/v1/configuration/migrate").unwrap();
        assert_eq!(route.scope, Some("configuration.write"));
        assert_eq!(route.service, "amaterasu.configuration/1");
        assert_eq!(
            payload(route, br#"{"expected_revision":1,"fields":{}}"#).unwrap()["action"],
            "migrate"
        );
        assert!(payload(route, br#"{"action":"get"}"#).is_err());
        assert!(resolve("/v1/auth/issue_token").is_none());
        assert!(resolve("/v1/state/set").is_none());
    }

    #[test]
    fn authentication_routes_expose_login_refresh_logout_and_admin_settings() {
        let login = resolve("/v1/auth/login").unwrap();
        assert_eq!(login.service, "amaterasu.authentication/1");
        assert_eq!(login.action, "login");
        assert_eq!(login.scope, None);
        assert!(
            payload(
                login,
                br#"{"username":"admin","password":"correct horse battery staple"}"#
            )
            .is_ok()
        );
        assert!(payload(
            login,
            br#"{"username":"admin","password":"correct horse battery staple","remember_me":true}"#
        )
        .is_ok());
        assert!(payload(
            login,
            br#"{"username":"admin","password":"correct horse battery staple","caller":"console"}"#
        )
        .is_err());

        let token = format!("ama_{}", "a".repeat(64));
        let refresh = resolve("/v1/auth/refresh").unwrap();
        assert_eq!(refresh.scope, None);
        assert_eq!(refresh.action, "refresh");
        assert!(
            payload(
                refresh,
                &serde_json::to_vec(&serde_json::json!({"refresh_token":token.clone()})).unwrap()
            )
            .is_ok()
        );
        assert!(payload(refresh, br#"{"refresh_token":"malformed"}"#).is_err());
        assert!(resolve("/v1/auth/issue_token").is_none());

        let logout = resolve("/v1/auth/logout").unwrap();
        assert_eq!(logout.scope, None);
        assert!(
            payload(
                logout,
                &serde_json::to_vec(&serde_json::json!({"refresh_token":token.clone()})).unwrap()
            )
            .is_ok()
        );
        assert!(payload(logout, br#"{}"#).is_err());
        assert!(
            payload(
                logout,
                &serde_json::to_vec(
                    &serde_json::json!({"bearer":token.clone(),"refresh_token":token})
                )
                .unwrap()
            )
            .is_err()
        );

        let settings = resolve("/v1/auth/settings").unwrap();
        assert_eq!(settings.service, "amaterasu.authentication/1");
        assert_eq!(settings.action, "settings_get");
        assert_eq!(settings.scope, Some("configuration.read"));
        assert_eq!(
            payload(settings, br#"{}"#).unwrap()["action"],
            "settings_get"
        );
        assert!(payload(settings, br#"{"owner":"amaterasu.other"}"#).is_err());

        let update = resolve("/v1/auth/settings_update").unwrap();
        assert_eq!(update.scope, Some("configuration.write"));
        assert_eq!(
            payload(
                update,
                br#"{"session_token_ttl_seconds":3600,"remember_me_refresh_token_ttl_seconds":259200,"expected_revision":0}"#
            )
            .unwrap()["action"],
            "settings_update"
        );
        for rejected in [
            br#"{"session_token_ttl_seconds":3600,"remember_me_refresh_token_ttl_seconds":259200}"#.as_slice(),
            br#"{"session_token_ttl_seconds":0,"remember_me_refresh_token_ttl_seconds":259200,"expected_revision":0}"#,
            br#"{"session_token_ttl_seconds":3600,"remember_me_refresh_token_ttl_seconds":2147483648,"expected_revision":0}"#,
            br#"{"session_token_ttl_seconds":3600.5,"remember_me_refresh_token_ttl_seconds":259200,"expected_revision":0}"#,
            br#"{"session_token_ttl_seconds":3600,"remember_me_refresh_token_ttl_seconds":259200,"expected_revision":0,"owner":"amaterasu.other"}"#,
        ] {
            assert!(payload(update, rejected).is_err());
        }
    }

    #[test]
    fn host_configuration_routes_require_signed_scopes_and_full_typed_updates() {
        let get = resolve("/v1/host-configuration/get").unwrap();
        assert_eq!(get.service, "amaterasu.host-configuration/1");
        assert_eq!(get.scope, Some("configuration.read"));
        assert_eq!(payload(get, br#"{}"#).unwrap()["action"], "get");
        assert!(payload(get, br#"{"owner":"other"}"#).is_err());

        let update = resolve("/v1/host-configuration/update").unwrap();
        assert_eq!(update.service, "amaterasu.host-configuration/1");
        assert_eq!(update.scope, Some("configuration.write"));
        let mut valid = serde_json::json!({
            "expected_revision":4,
            "file_state":"sunshine_state.json",
            "vibeshine_file_state":"vibeshine_state.json",
            "credentials_file":"",
            "pkey":"credentials/cakey.pem",
            "cert":"credentials/cacert.pem",
            "log_path":"logs/service.log",
            "min_log_level":"warning"
        });
        assert_eq!(
            payload(update, &serde_json::to_vec(&valid).unwrap()).unwrap()["action"],
            "update"
        );
        for level in [
            "product_default",
            "verbose",
            "debug",
            "info",
            "warning",
            "error",
            "fatal",
            "none",
        ] {
            valid["min_log_level"] = serde_json::json!(level);
            assert!(payload(update, &serde_json::to_vec(&valid).unwrap()).is_ok());
        }
        for (key, value) in [
            ("expected_revision", serde_json::json!(-1)),
            ("min_log_level", serde_json::json!("trace")),
            ("log_path", serde_json::json!("../escape.log")),
            ("log_path", serde_json::json!("./")),
            ("log_path", serde_json::json!("")),
            ("log_path", serde_json::json!("bad\npath")),
            ("cert", serde_json::json!(null)),
            ("pkey", serde_json::json!("x".repeat(4097))),
        ] {
            let mut rejected = valid.clone();
            rejected[key] = value;
            assert!(
                payload(update, &serde_json::to_vec(&rejected).unwrap()).is_err(),
                "accepted {key}"
            );
        }
        if cfg!(windows) {
            let mut drive_relative = valid.clone();
            drive_relative["log_path"] = serde_json::json!("C:relative.log");
            assert!(payload(update, &serde_json::to_vec(&drive_relative).unwrap()).is_err());
        }
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove("file_state");
        assert!(payload(update, &serde_json::to_vec(&missing).unwrap()).is_err());
        valid["owner"] = serde_json::json!("other");
        assert!(payload(update, &serde_json::to_vec(&valid).unwrap()).is_err());
        assert!(resolve("/v1/host-configuration/readiness").is_none());
    }

    #[test]
    fn nvhttp_encryption_updates_preserve_nullable_modes_and_reject_coercion() {
        let route = resolve("/v1/nvhttp/settings_update").unwrap();
        assert_eq!(route.service, "amaterasu.nvhttp/1");
        assert_eq!(route.scope, Some("configuration.write"));
        let base = serde_json::json!({
            "hostname": "Living Room",
            "port": 47989,
            "expected_revision": 7,
        });
        let omitted = payload(route, &serde_json::to_vec(&base).unwrap()).unwrap();
        assert!(omitted.get("lan_encryption_mode").is_none());
        assert!(omitted.get("wan_encryption_mode").is_none());

        for field in ["lan_encryption_mode", "wan_encryption_mode"] {
            for mode in [
                serde_json::Value::Null,
                serde_json::json!(0),
                serde_json::json!(1),
                serde_json::json!(2),
            ] {
                let mut request = base.clone();
                request[field] = mode.clone();
                let parsed = payload(route, &serde_json::to_vec(&request).unwrap()).unwrap();
                assert_eq!(parsed[field], mode);
                assert_eq!(parsed["expected_revision"], 7);
                assert_eq!(parsed["action"], "settings_update");
            }
            for invalid in [
                serde_json::json!(-1),
                serde_json::json!(3),
                serde_json::json!(1.0),
                serde_json::json!(1.5),
                serde_json::json!("2"),
                serde_json::json!(false),
                serde_json::json!([]),
                serde_json::json!({}),
            ] {
                let mut request = base.clone();
                request[field] = invalid;
                assert!(
                    payload(route, &serde_json::to_vec(&request).unwrap()).is_err(),
                    "accepted invalid {field}: {}",
                    request[field]
                );
            }
        }

        let mut independent_modes = base;
        independent_modes["lan_encryption_mode"] = serde_json::json!(2);
        independent_modes["wan_encryption_mode"] = serde_json::json!(0);
        let parsed = payload(route, &serde_json::to_vec(&independent_modes).unwrap()).unwrap();
        assert_eq!(parsed["lan_encryption_mode"], 2);
        assert_eq!(parsed["wan_encryption_mode"], 0);
    }

    #[test]
    fn nvhttp_settings_routes_pin_domain_and_reject_unexpected_fields() {
        let get = resolve("/v1/nvhttp/settings").unwrap();
        assert_eq!(get.service, "amaterasu.nvhttp/1");
        assert_eq!(get.scope, Some("configuration.read"));
        assert_eq!(payload(get, br#"{}"#).unwrap()["action"], "settings_get");
        assert!(payload(get, br#"{"owner":"amaterasu.other"}"#).is_err());

        let update = resolve("/v1/nvhttp/settings_update").unwrap();
        assert_eq!(update.scope, Some("configuration.write"));
        assert_eq!(
            payload(
                update,
                br#"{"hostname":"Living Room","port":47989,"expected_revision":0}"#
            )
            .unwrap()["action"],
            "settings_update"
        );
        let network_override = serde_json::json!({
            "hostname":"Living Room",
            "port":47989,
            "address_family":"both",
            "bind_address":"::1",
            "expected_revision":1
        });
        let parsed_override =
            payload(update, &serde_json::to_vec(&network_override).unwrap()).unwrap();
        assert_eq!(parsed_override["address_family"], "both");
        assert_eq!(parsed_override["bind_address"], "::1");
        let reset = payload(
            update,
            br#"{"hostname":"Living Room","port":47989,"address_family":null,"bind_address":null,"expected_revision":2}"#,
        )
        .unwrap();
        assert!(reset["address_family"].is_null());
        assert!(reset["bind_address"].is_null());
        let explicit_empty = payload(
            update,
            br#"{"hostname":"Living Room","port":47989,"address_family":"ipv4","bind_address":"","expected_revision":2}"#,
        )
        .unwrap();
        assert_eq!(explicit_empty["address_family"], "ipv4");
        assert_eq!(explicit_empty["bind_address"], "");
        for output_name in [
            serde_json::Value::Null,
            serde_json::json!(""),
            serde_json::json!("DISPLAY1"),
        ] {
            let request = serde_json::json!({
                "hostname":"Living Room",
                "port":47989,
                "expected_revision":2,
                "output_name":output_name,
            });
            assert!(payload(update, &serde_json::to_vec(&request).unwrap()).is_ok());
        }
        for stream_audio in [true, false] {
            let request = serde_json::json!({
                "hostname":"Living Room",
                "port":47989,
                "expected_revision":2,
                "stream_audio":stream_audio,
            });
            assert!(payload(update, &serde_json::to_vec(&request).unwrap()).is_ok());
        }
        for stream_audio in [serde_json::Value::Null, serde_json::json!("false")] {
            let request = serde_json::json!({
                "hostname":"Living Room",
                "port":47989,
                "expected_revision":2,
                "stream_audio":stream_audio,
            });
            assert!(payload(update, &serde_json::to_vec(&request).unwrap()).is_err());
        }
        for capture in [
            serde_json::Value::Null,
            serde_json::json!("auto"),
            serde_json::json!("ddx"),
            serde_json::json!("wgc"),
            serde_json::json!("wgcc"),
        ] {
            let request = serde_json::json!({
                "hostname":"Living Room",
                "port":47989,
                "expected_revision":2,
                "capture":capture,
            });
            assert!(payload(update, &serde_json::to_vec(&request).unwrap()).is_ok());
        }
        for capture in [
            serde_json::json!("dxgi"),
            serde_json::json!("WGC"),
            serde_json::json!(true),
        ] {
            let request = serde_json::json!({
                "hostname":"Living Room",
                "port":47989,
                "expected_revision":2,
                "capture":capture,
            });
            assert!(payload(update, &serde_json::to_vec(&request).unwrap()).is_err());
        }
        for size in [
            serde_json::Value::Null,
            serde_json::json!(0),
            serde_json::json!(16),
            serde_json::json!(20),
            serde_json::json!(32),
            serde_json::json!(64),
        ] {
            let request = serde_json::json!({
                "hostname":"Living Room", "port":47989, "expected_revision":2,
                "video_max_batch_size_kb":size,
            });
            assert!(payload(update, &serde_json::to_vec(&request).unwrap()).is_ok());
        }
        for size in [
            serde_json::json!(-1),
            serde_json::json!(65),
            serde_json::json!(16.5),
            serde_json::json!("16"),
        ] {
            let request = serde_json::json!({
                "hostname":"Living Room", "port":47989, "expected_revision":2,
                "video_max_batch_size_kb":size,
            });
            assert!(payload(update, &serde_json::to_vec(&request).unwrap()).is_err());
        }
        for bitrate in [
            serde_json::Value::Null,
            serde_json::json!(-2_147_483_648_i64),
            serde_json::json!(0),
            serde_json::json!(100_000),
            serde_json::json!(2_147_483_647_i64),
        ] {
            let request = serde_json::json!({
                "hostname":"Living Room", "port":47989, "expected_revision":2,
                "max_bitrate":bitrate,
            });
            assert!(payload(update, &serde_json::to_vec(&request).unwrap()).is_ok());
        }
        for bitrate in [
            serde_json::json!(-2_147_483_649_i64),
            serde_json::json!(2_147_483_648_i64),
            serde_json::json!(123.5),
            serde_json::json!("5000"),
        ] {
            let request = serde_json::json!({
                "hostname":"Living Room", "port":47989, "expected_revision":2,
                "max_bitrate":bitrate,
            });
            assert!(payload(update, &serde_json::to_vec(&request).unwrap()).is_err());
        }
        for output_name in [
            serde_json::json!(" "),
            serde_json::json!(" DISPLAY1"),
            serde_json::json!("DISPLAY1 "),
            serde_json::json!("bad\nname"),
            serde_json::json!("é".repeat(257)),
            serde_json::json!(true),
        ] {
            let request = serde_json::json!({
                "hostname":"Living Room",
                "port":47989,
                "expected_revision":2,
                "output_name":output_name,
            });
            assert!(payload(update, &serde_json::to_vec(&request).unwrap()).is_err());
        }
        for rejected in [
            br#"{"hostname":"Living Room","port":47989}"#.as_slice(),
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"owner":"other"}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":-1}"#,
            br#"{"hostname":"Long\nName","port":47989,"expected_revision":0}"#,
            br#"{"hostname":"Living Room","port":1028,"expected_revision":0}"#,
            br#"{"hostname":"Living Room","port":65515,"expected_revision":0}"#,
            br#"{"hostname":"Living Room","port":47989.5,"expected_revision":0}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"address_family":"ipv6"}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"address_family":1}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"bind_address":false}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"bind_address":"localhost"}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"bind_address":" "}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"bind_address":"127.0.0.1/24"}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"bind_address":"127.0.0.1:4"}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"address_family":"ipv4","bind_address":"::1"}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"bind_address":"224.0.0.1"}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"bind_address":"255.255.255.255"}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"bind_address":"ff02::1"}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"bind_address":"::ffff:127.0.0.1"}"#,
        ] {
            assert!(payload(update, rejected).is_err());
        }
        assert!(
            payload(
                update,
                br#"{"hostname":"","port":47989,"expected_revision":0}"#
            )
            .is_ok()
        );
    }

    #[test]
    fn steam_settings_routes_require_configuration_scopes_and_fixed_payloads() {
        let get = resolve("/v1/steam/settings").unwrap();
        assert_eq!(get.service, "amaterasu.steam/1");
        assert_eq!(get.scope, Some("configuration.read"));
        assert_eq!(payload(get, br#"{}"#).unwrap()["action"], "settings_get");
        assert!(payload(get, br#"{"principal":"amaterasu.other"}"#).is_err());

        let update = resolve("/v1/steam/settings_update").unwrap();
        assert_eq!(update.scope, Some("configuration.write"));
        let valid = serde_json::json!({
            "enabled":true,
            "auto_sync":false,
            "exclude_games":[
                {"id":"730","name":"Counter-Strike 2"},
                {"name":"Portal 2"}
            ],
            "include_tools":false,
            "sync_all_installed":false,
            "recent_games":10,
            "recent_max_age_days":30,
            "autosync_remove_uninstalled":true,
            "expected_revision":0
        });
        assert_eq!(
            payload(update, &serde_json::to_vec(&valid).unwrap()).unwrap()["action"],
            "settings_update"
        );
        let legacy = serde_json::json!({
            "enabled":true,
            "auto_sync":false,
            "exclude_games":[],
            "expected_revision":7
        });
        let legacy_payload = payload(update, &serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(legacy_payload["action"], "settings_update");
        assert!(legacy_payload.get("include_tools").is_none());
        assert!(legacy_payload.get("autosync_remove_uninstalled").is_none());

        let mut invalid_payloads = vec![
            serde_json::json!({"enabled":true,"expected_revision":0}),
            serde_json::json!({"enabled":1,"auto_sync":false,"exclude_games":[],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":0,"exclude_games":[],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"expected_revision":0,"exclude_games":[] ,"owner":"other"}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[{}],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[{"name":" "}],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[{"name":"bad\nname"}],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[{"name":"x".repeat(513)}],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[{"id":730}],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[{"id":"0"}],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[{"id":"0730"}],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[{"id":"4294967296"}],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[{"id":"730","name":"Game","extra":true}],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[{"id":"730"},{"id":"730","name":"Game"}],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[{"name":"Game"},{"name":"game"}],"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[],"include_tools":1,"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[],"sync_all_installed":"true","expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[],"recent_games":10001,"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[],"recent_max_age_days":36501,"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[],"recent_games":1.5,"expected_revision":0}),
            serde_json::json!({"enabled":true,"auto_sync":false,"exclude_games":[],"autosync_remove_uninstalled":"true","expected_revision":0}),
        ];
        invalid_payloads.push(serde_json::json!({
            "enabled":true,
            "auto_sync":false,
            "exclude_games":(0..257).map(|index| serde_json::json!({"id":(index + 1).to_string()})).collect::<Vec<_>>(),
            "expected_revision":0
        }));
        for invalid in invalid_payloads {
            assert!(
                payload(update, &serde_json::to_vec(&invalid).unwrap()).is_err(),
                "accepted malformed Steam settings payload: {invalid}"
            );
        }
    }

    #[test]
    fn steam_force_sync_route_is_fixed_scoped_and_requires_an_empty_object() {
        let route = resolve("/v1/steam/force_sync").unwrap();
        assert_eq!(route.service, "amaterasu.steam/1");
        assert_eq!(route.action, "force_sync");
        assert_eq!(route.scope, Some("steam.refresh"));
        assert!(route.requires_empty_body);
        assert_eq!(
            payload(route, br#"{}"#).unwrap(),
            serde_json::json!({"action":"force_sync"})
        );
        for invalid_body in [
            br#"{"force":true}"#.as_slice(),
            br#"{"action":"refresh"}"#,
            br#"[]"#,
            br#"null"#,
        ] {
            assert!(payload(route, invalid_body).is_err());
        }
        assert!(resolve("/v1/steam/force_sync/extra").is_none());
    }

    #[test]
    fn steam_artwork_route_is_read_scoped_and_accepts_only_a_bounded_cursor() {
        let route = resolve("/v1/steam/artwork_read").unwrap();
        assert_eq!(route.service, "amaterasu.steam/1");
        assert_eq!(route.action, "artwork_read");
        assert_eq!(route.scope, Some("steam.read"));
        let valid = serde_json::json!({"app_id":42,"revision":"18446744073709551615","offset":16*1024*1024});
        let forwarded = payload(route, &serde_json::to_vec(&valid).unwrap()).unwrap();
        assert_eq!(forwarded["action"], "artwork_read");
        for invalid in [
            serde_json::json!({"app_id":0,"revision":"1","offset":0}),
            serde_json::json!({"app_id":4294967296_u64,"revision":"1","offset":0}),
            serde_json::json!({"app_id":42,"revision":"../art","offset":0}),
            serde_json::json!({"app_id":42,"revision":"1","offset":1}),
            serde_json::json!({"app_id":42,"revision":"1","offset":16*1024*1024+8192}),
            serde_json::json!({"app_id":42,"revision":"1","offset":0,"path":"C:\\other.png"}),
        ] {
            assert!(payload(route, &serde_json::to_vec(&invalid).unwrap()).is_err());
        }
    }

    #[test]
    fn lutris_routes_pin_the_provider_and_validate_settings_fields() {
        for (path, action, scope) in [
            ("/v1/lutris/settings", "settings_get", "configuration.read"),
            (
                "/v1/lutris/settings_update",
                "settings_update",
                "configuration.write",
            ),
            ("/v1/lutris/status", "status", "configuration.read"),
            ("/v1/lutris/refresh", "refresh", "configuration.write"),
        ] {
            let route = resolve(path).unwrap();
            assert_eq!(route.service, "amaterasu.lutris/1");
            assert_eq!(route.action, action);
            assert_eq!(route.scope, Some(scope));
        }
        for path in [
            "/v1/lutris/settings",
            "/v1/lutris/status",
            "/v1/lutris/refresh",
        ] {
            assert!(payload(resolve(path).unwrap(), br#"{}"#).is_ok());
            assert!(payload(resolve(path).unwrap(), br#"{"owner":"other"}"#).is_err());
        }

        let update = resolve("/v1/lutris/settings_update").unwrap();
        let valid = serde_json::json!({
            "enabled":false,
            "auto_sync":false,
            "autosync_remove_uninstalled":true,
            "include_steam":false,
            "exclude_games":["730",{"id":"7","name":"Old title"},{"name":"Portal 2"}],
            "expected_revision":8
        });
        let forwarded = payload(update, &serde_json::to_vec(&valid).unwrap()).unwrap();
        assert_eq!(forwarded["action"], "settings_update");
        assert_eq!(forwarded["exclude_games"][0], "730");
        assert!(payload(update, br#"{"enabled":false,"expected_revision":0}"#).is_ok());

        let mut invalid = vec![
            serde_json::json!({"enabled":1,"expected_revision":0}),
            serde_json::json!({"enabled":false,"expected_revision":-1}),
            serde_json::json!({"enabled":false,"expected_revision":0,"owner":"other"}),
            serde_json::json!({"enabled":false,"expected_revision":0,"auto_sync":"false"}),
            serde_json::json!({"enabled":false,"expected_revision":0,"exclude_games":"730"}),
            serde_json::json!({"enabled":false,"expected_revision":0,"exclude_games":["0"]}),
            serde_json::json!({"enabled":false,"expected_revision":0,"exclude_games":["18446744073709551616"]}),
            serde_json::json!({"enabled":false,"expected_revision":0,"exclude_games":[{}]}),
            serde_json::json!({"enabled":false,"expected_revision":0,"exclude_games":[{"id":"1","name":"Title","owner":"other"}]}),
            serde_json::json!({"enabled":false,"expected_revision":0,"exclude_games":[{"name":"  "}]}),
            serde_json::json!({"enabled":false,"expected_revision":0,"exclude_games":[{"name":"bad\nname"}]}),
            serde_json::json!({"enabled":false,"expected_revision":0,"exclude_games":[{"name":"x".repeat(513)}]}),
            serde_json::json!({"enabled":false,"expected_revision":0,"exclude_games":["7",{"id":"7","name":"same ID"}]}),
            serde_json::json!({"enabled":false,"expected_revision":0,"exclude_games":[{"name":"Portal"},{"name":"portal"}]}),
        ];
        invalid.push(serde_json::json!({
            "enabled":false,
            "expected_revision":0,
            "exclude_games":(0..257).map(|_| serde_json::json!("7")).collect::<Vec<_>>()
        }));
        for value in invalid {
            assert!(
                payload(update, &serde_json::to_vec(&value).unwrap()).is_err(),
                "accepted malformed Lutris settings payload: {value}"
            );
        }
    }

    #[test]
    fn input_settings_routes_pin_domain_scopes_and_bounded_payloads() {
        let status = resolve("/v1/input/status").unwrap();
        assert_eq!(status.service, "amaterasu.input/1");
        assert_eq!(status.scope, Some("configuration.read"));
        assert_eq!(payload(status, br#"{}"#).unwrap()["action"], "status");
        assert!(payload(status, br#"{"owner":"other"}"#).is_err());

        let get = resolve("/v1/input/settings_get").unwrap();
        assert_eq!(get.service, "amaterasu.input/1");
        assert_eq!(get.scope, Some("configuration.read"));
        assert_eq!(payload(get, br#"{}"#).unwrap()["action"], "settings_get");
        for rejected in [
            br#"{"owner":"amaterasu.other"}"#.as_slice(),
            br#"{"owner":"other"}"#,
        ] {
            assert!(payload(get, rejected).is_err());
        }

        let update = resolve("/v1/input/settings_update").unwrap();
        assert_eq!(update.service, "amaterasu.input/1");
        assert_eq!(update.scope, Some("configuration.write"));
        let valid = payload(
            update,
            br#"{"keyboard":false,"mouse":true,"expected_revision":7}"#,
        )
        .unwrap();
        assert_eq!(valid["action"], "settings_update");
        assert_eq!(valid["keyboard"], false);
        assert_eq!(valid["expected_revision"], 7);
        let high_resolution = payload(
            update,
            br#"{"keyboard":true,"mouse":false,"high_resolution_scrolling":false,"expected_revision":8}"#,
        )
        .unwrap();
        assert_eq!(high_resolution["high_resolution_scrolling"], false);
        let scan_codes = payload(
            update,
            br#"{"keyboard":true,"mouse":false,"always_send_scancodes":false,"expected_revision":9}"#,
        )
        .unwrap();
        assert_eq!(scan_codes["always_send_scancodes"], false);
        let right_alt_mapping = payload(
            update,
            br#"{"keyboard":true,"mouse":false,"key_rightalt_to_key_win":true,"expected_revision":10}"#,
        )
        .unwrap();
        assert_eq!(right_alt_mapping["key_rightalt_to_key_win"], true);
        let repeat_policy = payload(
            update,
            br#"{"keyboard":true,"mouse":true,"key_repeat_delay":0,"key_repeat_frequency":31.25,"expected_revision":11}"#,
        )
        .unwrap();
        assert_eq!(repeat_policy["key_repeat_delay"], 0);
        assert_eq!(repeat_policy["key_repeat_frequency"], 31.25);
        let gamepad = payload(
            update,
            br#"{"keyboard":true,"mouse":true,"gamepad":"auto","expected_revision":12}"#,
        )
        .unwrap();
        assert_eq!(gamepad["gamepad"], "auto");
        for rejected in [
            br#"{"keyboard":false,"mouse":true}"#.as_slice(),
            br#"{"keyboard":0,"mouse":true,"expected_revision":0}"#,
            br#"{"keyboard":false,"mouse":"true","expected_revision":0}"#,
            br#"{"keyboard":false,"mouse":true,"high_resolution_scrolling":"true","expected_revision":0}"#,
            br#"{"keyboard":false,"mouse":true,"key_rightalt_to_key_win":"true","expected_revision":0}"#,
            br#"{"keyboard":false,"mouse":true,"key_repeat_delay":2147483648,"expected_revision":0}"#,
            br#"{"keyboard":false,"mouse":true,"key_repeat_frequency":-0.1,"expected_revision":0}"#,
            br#"{"keyboard":false,"mouse":true,"expected_revision":-1}"#,
            br#"{"keyboard":false,"mouse":true,"expected_revision":0,"owner":"other"}"#,
        ] {
            assert!(payload(update, rejected).is_err());
        }
    }

    #[test]
    fn playnite_routes_pin_settings_status_and_refresh_actions() {
        for (path, action, scope) in [
            (
                "/v1/playnite/settings",
                "settings_get",
                "configuration.read",
            ),
            (
                "/v1/playnite/settings_update",
                "settings_update",
                "configuration.write",
            ),
            ("/v1/playnite/status", "status", "configuration.read"),
            ("/v1/playnite/refresh", "refresh", "configuration.write"),
        ] {
            let route = resolve(path).unwrap();
            assert_eq!(route.service, "amaterasu.playnite/1");
            assert_eq!(route.action, action);
            assert_eq!(route.scope, Some(scope));
        }

        let settings = resolve("/v1/playnite/settings").unwrap();
        assert_eq!(
            payload(settings, br#"{}"#).unwrap()["action"],
            "settings_get"
        );
        let status = resolve("/v1/playnite/status").unwrap();
        assert_eq!(payload(status, br#"{}"#).unwrap()["action"], "status");
        let refresh = resolve("/v1/playnite/refresh").unwrap();
        assert_eq!(payload(refresh, br#"{}"#).unwrap()["action"], "refresh");
        assert!(payload(status, br#"{"action":"launch_start"}"#).is_err());
        assert!(resolve("/v1/playnite/launch/start").is_none());
    }
}
