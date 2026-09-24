//! Field validation for routes with explicit payload restrictions.
//! Native domain services retain semantic validation and effect authorization.
use crate::routes::Route;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};

type Object = Map<String, Value>;
const INVALID: &str = "unexpected or malformed route fields";

pub fn validate(route: Route, object: &Object) -> Result<(), &'static str> {
    if route.requires_empty_body {
        return fields(object, &[], &[]);
    }
    match route.service {
        "amaterasu.authentication/1" => authentication(route.action, object),
        "amaterasu.pairing/1" => pairing(route.action, object),
        "amaterasu.display/1" => display(route.action, object),
        "amaterasu.launch/1" => launch(route.action, object),
        "amaterasu.nvhttp/1" => nvhttp(route.action, object),
        "amaterasu.steam/1" => steam(route.action, object),
        "amaterasu.lutris/1" => lutris(route.action, object),
        "amaterasu.playnite/1" => playnite(route.action, object),
        "amaterasu.input/1" => input(route.action, object),
        _ => Ok(()),
    }
}

fn authentication(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "settings_get" => fields(object, &[], &[]),
        "settings_update" => {
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

fn steam(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "settings_get" => fields(object, &[], &[]),
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
    match action {
        "settings_get" | "status" | "refresh" => fields(object, &[], &[]),
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
        "settings_get" => fields(object, &[], &[]),
        "settings_update" => {
            fields(
                object,
                &["keyboard", "mouse", "expected_revision"],
                &["high_resolution_scrolling", "always_send_scancodes"],
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

fn playnite(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "settings_get" | "status" | "refresh" => fields(object, &[], &[]),
        "settings_update" => {
            fields(
                object,
                &["enabled", "expected_revision"],
                &[
                    "auto_sync",
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
            for field in ["auto_sync", "sync_all_installed"] {
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

fn nvhttp(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "settings_get" => fields(object, &[], &[]),
        "settings_update" => {
            fields(
                object,
                &["hostname", "port", "expected_revision"],
                &["address_family", "bind_address"],
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
            Ok(())
        }
        _ => Err(INVALID),
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
            fields(object, &["certificate_id"], &[])?;
            let id = text(object, "certificate_id")?;
            if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
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
            fields(config_overrides, &[], &["fec_percentage"])?;
            if let Some(percentage) = config_overrides.get("fec_percentage")
                && !percentage.is_null()
                && !percentage
                    .as_u64()
                    .is_some_and(|value| (1..=u8::MAX as u64).contains(&value))
            {
                return Err(INVALID);
            }
            Ok(())
        }
        _ => Err(INVALID),
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
        ] {
            assert!(validate(route, object.as_object().unwrap()).is_err());
        }
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
