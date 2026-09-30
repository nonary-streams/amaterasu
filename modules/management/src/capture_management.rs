//! Browser-facing PipeWire selection authorization and response projection.
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const MAX_SELECTIONS: usize = 64;
const SELECTION_LIFETIME: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionAction {
    Status,
    Begin,
    Poll,
    Cancel,
}

#[derive(Clone, Debug)]
struct SelectionOwner {
    identity: String,
    expires_at: Instant,
}

#[derive(Default)]
pub struct SelectionOwners {
    entries: HashMap<String, SelectionOwner>,
}

impl SelectionOwners {
    pub fn begin(&mut self, operation_id: &str, identity: &str, now: Instant) -> Result<bool, ()> {
        self.expire(now);
        if let Some(owner) = self.entries.get_mut(operation_id) {
            if owner.identity != identity {
                return Err(());
            }
            owner.expires_at = now + SELECTION_LIFETIME;
            return Ok(false);
        }
        if self.entries.len() >= MAX_SELECTIONS {
            return Err(());
        }
        self.entries.insert(
            operation_id.into(),
            SelectionOwner {
                identity: identity.into(),
                expires_at: now + SELECTION_LIFETIME,
            },
        );
        Ok(true)
    }

    pub fn permits(&mut self, operation_id: &str, identity: &str, now: Instant) -> bool {
        self.expire(now);
        let Some(owner) = self.entries.get_mut(operation_id) else {
            return false;
        };
        if owner.identity != identity {
            return false;
        }
        owner.expires_at = now + SELECTION_LIFETIME;
        true
    }

    pub fn remove(&mut self, operation_id: &str) {
        self.entries.remove(operation_id);
    }

    fn expire(&mut self, now: Instant) {
        self.entries.retain(|_, owner| owner.expires_at > now);
    }
}

pub fn identity(
    kind: &str,
    subject: &str,
    auth_context_id: &str,
) -> Option<crate::http::VerifiedIdentity> {
    if !matches!(kind, "administrator_session" | "api_token")
        || subject.is_empty()
        || subject.len() > 64
        || auth_context_id.len() != 64
        || !auth_context_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(crate::http::VerifiedIdentity {
        kind: kind.into(),
        subject: subject.into(),
        auth_context_id: auth_context_id.into(),
    })
}

pub fn response(action: SelectionAction, payload: &Value) -> (u16, Value) {
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        let code = payload
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)
            .and_then(safe_error)
            .unwrap_or("capture_unavailable");
        let status = match code {
            "source_selection_busy" => 409,
            "capture_disabled"
            | "native_dependencies_unavailable"
            | "portal_unavailable"
            | "session_bus_unavailable"
            | "unsupported_platform" => 503,
            "selection_permission_denied" | "capture_permission_denied" => 403,
            "selection_not_found" | "not_found" => 404,
            "unsupported_source_type" => 400,
            _ => 503,
        };
        return (status, json!({"ok":false,"code":code}));
    }

    if action == SelectionAction::Cancel {
        return (200, json!({"ok":true,"cancelled":true}));
    }

    if action == SelectionAction::Status {
        let enabled = payload
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let portal_available = payload
            .get("availability")
            .and_then(|value| value.get("available"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let provider_reason = payload
            .get("availability")
            .and_then(|value| value.get("reason"))
            .and_then(Value::as_str)
            .and_then(safe_error);
        let availability = enabled && portal_available;
        let reason = if !enabled {
            Some("capture_disabled")
        } else {
            provider_reason
        };
        let selection_state = payload
            .get("selection_state")
            .and_then(Value::as_str)
            .filter(|state| {
                matches!(
                    *state,
                    "permission_required" | "selecting" | "ready" | "denied" | "failed"
                )
            })
            .unwrap_or("permission_required");
        return (
            200,
            json!({"ok":true,"availability":if availability {"available"} else {"unavailable"},
                "selection_state":selection_state,"reason":reason}),
        );
    }

    let state = payload
        .get("selection_state")
        .and_then(Value::as_str)
        .filter(|state| matches!(*state, "selecting" | "ready" | "denied" | "failed"))
        .unwrap_or("failed");
    let source_type = payload
        .get("source_type")
        .and_then(Value::as_str)
        .filter(|source| matches!(*source, "monitor" | "window"));
    let output = payload.get("output");
    let dimensions = output.and_then(|value| {
        let width = value.get("width")?.as_u64()?;
        let height = value.get("height")?.as_u64()?;
        ((1..=32_768).contains(&width) && (1..=32_768).contains(&height)).then_some((width, height))
    });
    let reason = payload
        .get("error")
        .and_then(Value::as_str)
        .and_then(safe_error);
    let mut result =
        json!({"ok":true,"selection_state":state,"source_type":source_type,"reason":reason});
    if let Some((width, height)) = dimensions {
        result["width"] = json!(width);
        result["height"] = json!(height);
    }
    if matches!(action, SelectionAction::Begin | SelectionAction::Poll)
        && let Some(operation_id) = payload.get("operation_id").and_then(Value::as_str)
    {
        result["operation_id"] = json!(operation_id);
    }
    (200, result)
}

fn safe_error(value: &str) -> Option<&'static str> {
    Some(match value {
        "capture_disabled" => "capture_disabled",
        "native_dependencies_unavailable" => "native_dependencies_unavailable",
        "portal_unavailable" => "portal_unavailable",
        "session_bus_unavailable" => "session_bus_unavailable",
        "unsupported_platform" => "unsupported_platform",
        "source_selection_busy" => "source_selection_busy",
        "source_selection_expired" => "source_selection_expired",
        "source_selection_cancelled" => "source_selection_cancelled",
        "source_selection_denied" => "source_selection_denied",
        "source_revoked" => "source_revoked",
        "source_lease_closed" => "source_lease_closed",
        "capture_shutdown" => "capture_shutdown",
        "selection_not_found" => "selection_not_found",
        "not_found" => "not_found",
        "unsupported_source_type" => "unsupported_source_type",
        "selection_permission_denied" => "selection_permission_denied",
        "capture_permission_denied" => "capture_permission_denied",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "a12f6f1e-04cb-4a97-bdf8-f53bb9cf2dd1";
    const AUTH_CONTEXT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn selection_ownership_is_session_scoped_stable_across_refresh_and_expires() {
        let now = Instant::now();
        let first = identity("administrator_session", "admin", AUTH_CONTEXT).unwrap();
        let after_refresh = identity("administrator_session", "admin", AUTH_CONTEXT).unwrap();
        let other_session = identity(
            "administrator_session",
            "admin",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .unwrap();
        let api_token = identity(
            "api_token",
            "admin",
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        )
        .unwrap();
        let first = format!("{}:{}:{}", first.kind, first.subject, first.auth_context_id);
        let after_refresh = format!(
            "{}:{}:{}",
            after_refresh.kind, after_refresh.subject, after_refresh.auth_context_id
        );
        let other_session = format!(
            "{}:{}:{}",
            other_session.kind, other_session.subject, other_session.auth_context_id
        );
        let api_token = format!(
            "{}:{}:{}",
            api_token.kind, api_token.subject, api_token.auth_context_id
        );
        let mut owners = SelectionOwners::default();
        assert_eq!(owners.begin(ID, &first, now), Ok(true));
        assert!(owners.permits(ID, &first, now));
        assert!(owners.permits(ID, &after_refresh, now));
        assert!(!owners.permits(ID, &other_session, now));
        assert!(!owners.permits(ID, &api_token, now));
        assert_eq!(owners.begin(ID, &other_session, now), Err(()));
        assert!(!owners.permits(ID, "other-user", now));
        assert!(!owners.permits(ID, &first, now + SELECTION_LIFETIME));
    }

    #[test]
    fn status_and_selection_responses_redact_provider_metadata() {
        let (status, sanitized) = response(
            SelectionAction::Status,
            &json!({"ok":true,"enabled":true,"availability":{"available":false,"reason":"session_bus_unavailable","portal_version":9},
                "selection_state":"permission_required","selection_choices":["monitor"],"streams":[{"id":"secret"}],"bus_name":"org.example"}),
        );
        assert_eq!(status, 200);
        assert_eq!(
            sanitized,
            json!({"ok":true,"availability":"unavailable","selection_state":"permission_required","reason":"session_bus_unavailable"})
        );
        let (_, disabled) = response(
            SelectionAction::Status,
            &json!({"ok":true,"enabled":false,"availability":{"available":true,"reason":null},"selection_state":"permission_required"}),
        );
        assert_eq!(disabled["availability"], "unavailable");
        assert_eq!(disabled["reason"], "capture_disabled");

        let (status, selection) = response(
            SelectionAction::Poll,
            &json!({"ok":true,"operation_id":ID,"source_type":"monitor","selection_state":"ready",
                "output":{"id":"opaque-output","pipewire_node":19,"width":1920,"height":1080,"format":"bgra8","bus":"secret"}}),
        );
        assert_eq!(status, 200);
        assert_eq!(
            selection,
            json!({"ok":true,"selection_state":"ready","source_type":"monitor","reason":null,"width":1920,"height":1080,"operation_id":ID})
        );
        assert!(!selection.to_string().contains("opaque-output"));
        assert!(!selection.to_string().contains("pipewire_node"));
        assert!(!selection.to_string().contains("bus"));

        let (status, cancelled) = response(SelectionAction::Cancel, &json!({"ok":true}));
        assert_eq!(status, 200);
        assert_eq!(cancelled, json!({"ok":true,"cancelled":true}));

        let mut owners = SelectionOwners::default();
        let owner = "session:admin:context";
        let now = Instant::now();
        owners.begin(ID, owner, now).unwrap();
        let restarted_owners = SelectionOwners::default();
        assert!(!restarted_owners.entries.contains_key(ID));
        owners.remove(ID);
        assert!(!owners.permits(ID, owner, now));
    }

    #[test]
    fn provider_errors_are_mapped_to_fixed_safe_codes() {
        let (status, response) = response(
            SelectionAction::Begin,
            &json!({"ok":false,"error":{"code":"org.freedesktop.DBus.Error.AccessDenied","message":"private bus data"}}),
        );
        assert_eq!(status, 503);
        assert_eq!(response, json!({"ok":false,"code":"capture_unavailable"}));
    }
}
