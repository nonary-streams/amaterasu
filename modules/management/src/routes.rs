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
        ("steam", "status") => ("amaterasu.steam/1", "status", Some("steam.read")),
        ("steam", "refresh") => ("amaterasu.steam/1", "refresh", Some("steam.refresh")),
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
        ("diagnostics", "query") => ("amaterasu.diagnostics/1", "query", Some("diagnostics.read")),
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
        _ => return None,
    };
    Some(Route {
        service,
        action,
        scope,
        requires_empty_body: domain == "media",
    })
}

fn media_discovery(action: &str) -> Option<(&'static str, &'static str)> {
    match action {
        "dxgi_outputs" => Some(("amaterasu.capture-dxgi/1", "enumerate")),
        "wgc_outputs" => Some(("amaterasu.capture-wgc/1", "enumerate")),
        "audio_endpoints" => Some(("amaterasu.audio/1", "enumerate")),
        "encoders" => Some(("amaterasu.encode/1", "enumerate")),
        "audio_encoders" => Some(("amaterasu.audio-encode/1", "enumerate")),
        "video_status" => Some(("amaterasu.video/1", "status")),
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
    fn media_discovery_requires_its_scope_and_cannot_forward_native_commands() {
        for action in [
            "dxgi_outputs",
            "wgc_outputs",
            "audio_endpoints",
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
    fn pairing_admin_routes_exclude_identity_import_and_transport_transcripts() {
        for action in [
            "identity",
            "pending",
            "list_clients",
            "approve_pin",
            "unpair",
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
    fn authentication_routes_only_expose_login_refresh_and_pair_logout() {
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
        for rejected in [
            br#"{"hostname":"Living Room","port":47989}"#.as_slice(),
            br#"{"hostname":"Living Room","port":47989,"expected_revision":0,"owner":"other"}"#,
            br#"{"hostname":"Living Room","port":47989,"expected_revision":-1}"#,
            br#"{"hostname":"Long\nName","port":47989,"expected_revision":0}"#,
            br#"{"hostname":"Living Room","port":1028,"expected_revision":0}"#,
            br#"{"hostname":"Living Room","port":65515,"expected_revision":0}"#,
            br#"{"hostname":"Living Room","port":47989.5,"expected_revision":0}"#,
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
        assert_eq!(
            payload(update, br#"{"enabled":true,"expected_revision":0}"#).unwrap()["action"],
            "settings_update"
        );
        for rejected in [
            br#"{"enabled":true}"#.as_slice(),
            br#"{"enabled":1,"expected_revision":0}"#,
            br#"{"enabled":true,"expected_revision":0,"owner":"other"}"#,
        ] {
            assert!(payload(update, rejected).is_err());
        }
    }
}
