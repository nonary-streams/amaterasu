//! Versioned, read-only foreground observations supplied by a selected provider.
//!
//! A snapshot describes an observation at the time of the call, not a durable
//! identity or authorization. An unavailable observation must not be cached as
//! though it established which application is foreground.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SERVICE: &str = "amaterasu.foreground-context/1";
pub const MAX_EXECUTABLE_BYTES: usize = 4096;
pub const MAX_RESPONSE_BYTES: usize = 8192;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Snapshot,
}

/// An absolute executable path and, when observable, fullscreen state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub executable: String,
    pub fullscreen: Option<bool>,
}

impl Snapshot {
    pub fn validate(&self) -> Result<(), &'static str> {
        let path = self.executable.as_str();
        let bytes = path.as_bytes();
        let windows_drive = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\');
        let absolute = path.starts_with('/') || path.starts_with("\\\\") || windows_drive;
        if path.len() > MAX_EXECUTABLE_BYTES || !absolute || path.chars().any(char::is_control) {
            return Err("invalid_executable_path");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotResponse {
    pub ok: bool,
    pub snapshot: Option<Snapshot>,
}

impl SnapshotResponse {
    /// Parse and validate a complete provider response before using its facts.
    pub fn parse(value: &Value) -> Result<Self, &'static str> {
        if serde_json::to_vec(value)
            .map_err(|_| "invalid_response")?
            .len()
            > MAX_RESPONSE_BYTES
        {
            return Err("response_too_large");
        }
        let object = value.as_object().ok_or("invalid_response")?;
        if !object.contains_key("snapshot")
            || object.get("snapshot").is_some_and(|snapshot| {
                snapshot.is_object() && snapshot.get("fullscreen").is_none()
            })
        {
            return Err("invalid_response");
        }
        let response: Self =
            serde_json::from_value(value.clone()).map_err(|_| "invalid_response")?;
        if !response.ok {
            return Err("provider_failed");
        }
        if let Some(snapshot) = &response.snapshot {
            snapshot.validate()?;
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replacement_providers_supply_the_same_snapshot_contract() {
        fn consume(reply: Value) -> Option<Snapshot> {
            SnapshotResponse::parse(&reply).unwrap().snapshot
        }
        assert_eq!(
            serde_json::to_value(Request::Snapshot).unwrap(),
            json!({"action":"snapshot"})
        );
        for executable in [
            "/games/example",
            "C:\\Games\\example.exe",
            "\\\\host\\share\\game.exe",
        ] {
            let actual =
                consume(json!({"ok":true,"snapshot":{"executable":executable,"fullscreen":null}}));
            assert_eq!(actual.unwrap().executable, executable);
        }
        assert!(consume(json!({"ok":true,"snapshot":null})).is_none());
        assert_eq!(
            consume(json!({"ok":true,"snapshot":{"executable":"/game","fullscreen":true}}))
                .unwrap()
                .fullscreen,
            Some(true)
        );
    }

    #[test]
    fn rejects_invalid_or_unbounded_provider_responses() {
        for reply in [
            json!({"ok":false,"snapshot":null}),
            json!({"ok":true}),
            json!({"ok":true,"snapshot":{"executable":"/game"}}),
            json!({"ok":true,"snapshot":{"executable":"relative","fullscreen":false}}),
            json!({"ok":true,"snapshot":{"executable":"/game\n","fullscreen":false}}),
            json!({"ok":true,"snapshot":{"executable":"/game","fullscreen":false,"extra":1}}),
            json!({"ok":true,"snapshot":{"executable":format!("/{}", "x".repeat(MAX_EXECUTABLE_BYTES)),"fullscreen":false}}),
            json!({"ok":true,"snapshot":null,"extra":"x".repeat(MAX_RESPONSE_BYTES)}),
        ] {
            assert!(SnapshotResponse::parse(&reply).is_err(), "accepted {reply}");
        }
    }
}
