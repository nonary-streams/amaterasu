//! Field validation for routes with explicit payload restrictions.
//! Native domain services retain semantic validation and effect authorization.
use crate::routes::Route;
use serde_json::{Map, Value};

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
        _ => Ok(()),
    }
}

fn authentication(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
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
            fields(object, &["enabled", "expected_revision"], &[])?;
            if object.get("enabled").and_then(Value::as_bool).is_none()
                || object
                    .get("expected_revision")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err(INVALID);
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn nvhttp(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
        "settings_get" => fields(object, &[], &[]),
        "settings_update" => {
            fields(object, &["hostname", "port", "expected_revision"], &[])?;
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
            Ok(())
        }
        _ => Err(INVALID),
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
