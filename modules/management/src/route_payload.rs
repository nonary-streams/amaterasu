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
        "amaterasu.authentication/1" => authentication(route.action, object),
        "amaterasu.catalog/1" => catalog(route.action, object),
        "amaterasu.pairing/1" => pairing(route.action, object),
        "amaterasu.display/1" => display(route.action, object),
        "amaterasu.launch/1" => launch(route.action, object),
        "amaterasu.nvhttp/1" => nvhttp(route.action, object),
        "amaterasu.steam/1" => steam(route.action, object),
        "amaterasu.lutris/1" => lutris(route.action, object),
        "amaterasu.playnite/1" => playnite(route.action, object),
        "amaterasu.input/1" => input(route.action, object),
        "amaterasu.session-history/1" => history(route.action, object),
        _ => Ok(()),
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
            fields(object, &[], &["limit"])?;
            if object.get("limit").is_some_and(|value| {
                !value
                    .as_u64()
                    .is_some_and(|limit| (1..=16).contains(&limit))
            }) {
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
        _ => Err(INVALID),
    }
}

fn steam(action: &str, object: &Object) -> Result<(), &'static str> {
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
                    "key_repeat_delay",
                    "key_repeat_frequency",
                    "back_button_timeout",
                    "gamepad",
                    "controller",
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
            "steam_id",
            "steam_install_dir",
            "exclude_global_prep_cmd",
            "exclude-global-prep-cmd",
            "elevated",
        ],
    )?;
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
    catalog_launch(entry.get("launch").ok_or(INVALID)?)
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
    fields(
        overrides,
        &[],
        &[
            "fec_percentage",
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
            "frame_limiter_enable",
            "frame_limiter_fps_limit",
            "frame_limiter_provider",
            "frame_limiter_disable_vsync",
            "frame_limiter_auto_virtual_framegen",
            "rtss_frame_limit_type",
            "ds4_back_as_touchpad_click",
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
    optional_catalog_frame_limiter_overrides(overrides)?;
    Ok(())
}

fn optional_catalog_frame_limiter_overrides(overrides: &Object) -> Result<(), &'static str> {
    for key in [
        "frame_limiter_enable",
        "frame_limiter_disable_vsync",
        "ds4_back_as_touchpad_click",
    ] {
        if overrides
            .get(key)
            .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
        {
            return Err(INVALID);
        }
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
    for key in ["controller", "keyboard", "mouse", "native_pen_touch"] {
        if overrides
            .get(key)
            .is_some_and(|value| !value.is_null() && value.as_bool().is_none())
        {
            return Err(INVALID);
        }
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

fn catalog_launch(value: &Value) -> Result<(), &'static str> {
    let launch = value.as_object().ok_or(INVALID)?;
    match text(launch, "kind")? {
        "application" => {
            fields(launch, &["kind", "path"], &[])?;
            text(launch, "path")?;
        }
        "playnite_fullscreen" => {
            fields(launch, &["kind", "exit_timeout_seconds"], &[])?;
            if !launch
                .get("exit_timeout_seconds")
                .and_then(Value::as_u64)
                .is_some_and(|timeout| timeout <= 300)
            {
                return Err(INVALID);
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

fn nvhttp(action: &str, object: &Object) -> Result<(), &'static str> {
    match action {
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
                    "video_max_batch_size_kb",
                    "max_bitrate",
                    "minimum_fps_target",
                    "audio_sink",
                    "virtual_sink",
                    "audio_sink_capture_only",
                    "external_ip",
                    "wgc_pacing_smoothing",
                    "nvenc_spatial_aq",
                    "ping_timeout_ms",
                    "global_preparation",
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
            if object.get("external_ip").is_some_and(|value| match value {
                Value::Null => false,
                Value::String(address) => {
                    address.len() > 4096 || address.chars().any(char::is_control)
                }
                _ => true,
            }) {
                return Err(INVALID);
            }
            optional_nullable_boolean(object, "wgc_pacing_smoothing")?;
            optional_nullable_boolean(object, "nvenc_spatial_aq")?;
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
            fields(
                config_overrides,
                &[],
                &[
                    "fec_percentage",
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
            optional_audio_sink(config_overrides)?;
            optional_virtual_sink(config_overrides)?;
            optional_audio_sink_capture_only(config_overrides)?;
            optional_back_button_timeout(config_overrides)?;
            optional_input_overrides(config_overrides)?;
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
            serde_json::json!({"expected_revision":0,"entry":{"name":"x","launch":{"kind":"playnite_fullscreen"}}}),
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
