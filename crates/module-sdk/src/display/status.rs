//! Validated discovery of an authenticated caller's durable scoped operation.
use serde::Deserialize;
use serde_json::Value;

/// An absent operation means this caller has no recoverable placement admission.
/// A retained reference whose placement was verified as not applied also has no
/// operation. A pending admission may have made native changes; query
/// `ScopedReconcile` with this
/// exact operation ID rather than constructing a replacement configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedStatus {
    /// Whether the exact caller, identity and reference still own a durable
    /// reservation. A reservation may remain after a placement was not applied.
    /// False also excludes earlier queued or pending placement admissions;
    /// it is not inferred merely from an absent completed operation ID.
    pub reference_owned: bool,
    pub operation_id: Option<String>,
    pub pending: bool,
}

/// Decode a status response and bind it to the exact requested scope. Status
/// reveals journal admission only, independently of current native readiness.
pub fn scoped_status_from_reply(
    payload: Value,
    identity: &str,
    reference_id: &str,
) -> Result<ScopedStatus, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        ok: bool,
        identity: String,
        reference_id: String,
        reference_owned: bool,
        operation_id: Option<String>,
        pending: bool,
    }

    if !valid_identifier(identity) || !valid_identifier(reference_id) {
        return Err("invalid_scoped_status_request".into());
    }
    // Require the nullable operation field: a truncated reply must not be
    // interpreted as proof that the caller has no durable admission.
    if payload.get("operation_id").is_none() {
        return Err("invalid_scoped_status_response".into());
    }
    let reply: Reply =
        serde_json::from_value(payload).map_err(|_| "invalid_scoped_status_response")?;
    if !reply.ok
        || reply.identity != identity
        || reply.reference_id != reference_id
        || !reply.reference_owned && (reply.operation_id.is_some() || reply.pending)
        || reply
            .operation_id
            .as_deref()
            .is_some_and(|operation| !valid_identifier(operation))
        || reply.pending && reply.operation_id.is_none()
    {
        return Err("invalid_scoped_status_response".into());
    }
    Ok(ScopedStatus {
        reference_owned: reply.reference_owned,
        operation_id: reply.operation_id,
        pending: reply.pending,
    })
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_distinguishes_absent_completed_and_pending_exact_admission() {
        for (reference_owned, operation_id, pending) in [
            (false, None, false),
            (true, None, false),
            (true, Some("op"), false),
            (true, Some("op"), true),
        ] {
            let status = scoped_status_from_reply(
                json!({"ok":true,"identity":"owner","reference_id":"reference","reference_owned":reference_owned,"operation_id":operation_id,"pending":pending}),
                "owner",
                "reference",
            )
            .unwrap();
            assert_eq!(status.reference_owned, reference_owned);
            assert_eq!(status.operation_id.as_deref(), operation_id);
            assert_eq!(status.pending, pending);
        }
    }

    #[test]
    fn malformed_or_different_scope_is_not_absence_or_recovery_proof() {
        let valid = json!({"ok":true,"identity":"owner","reference_id":"reference","reference_owned":true,"operation_id":"op","pending":true});
        for (field, value) in [
            ("ok", json!(false)),
            ("identity", json!("foreign")),
            ("reference_id", json!("different")),
            ("reference_owned", json!(false)),
            ("reference_owned", json!("false")),
            ("operation_id", Value::Null),
            ("operation_id", json!("x".repeat(129))),
            ("operation_id", json!("bad operation")),
            ("pending", json!("true")),
        ] {
            let mut malformed = valid.clone();
            malformed[field] = value;
            assert!(scoped_status_from_reply(malformed, "owner", "reference").is_err());
        }
        for field in [
            "identity",
            "reference_id",
            "reference_owned",
            "operation_id",
            "pending",
        ] {
            let mut truncated = valid.clone();
            truncated.as_object_mut().unwrap().remove(field);
            assert!(scoped_status_from_reply(truncated, "owner", "reference").is_err());
        }
    }
}
