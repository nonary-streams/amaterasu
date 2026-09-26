use super::record::Record;
use super::rpc::{self, Send};
use serde_json::{Value, json};

pub(super) fn apply(record: &Record) -> Value {
    let request = record
        .config
        .display_request
        .as_ref()
        .expect("validated display request");
    let mut payload = json!({
        "action":"apply",
        "configuration":request.configuration,
        "expected_topology_revision":request.expected_topology_revision,
        "operation_id":record.display_operation(),
    });
    if let Some(policy) = &request.mode_policy {
        payload["mode_policy"] = policy.clone();
    }
    if let Some(policies) = &request.mode_policies {
        payload["mode_policies"] = json!(policies);
    }
    payload
}

pub(super) fn accept_apply(record: &mut Record, response: &Value) {
    if response["ok"] == true
        && response["applied"] == true
        && response["operation_id"] == record.display_operation()
        && let Some(lease) = response["lease"]
            .as_str()
            .filter(|lease| valid_lease(lease))
    {
        record.display_lease = Some(lease.into());
        record.display_state = "applied".into();
        return;
    }
    if (response["recovery_required"] == true || response["state"] == "recovery_required")
        && response["operation_id"] == record.display_operation()
        && let Some(lease) = response["lease"]
            .as_str()
            .filter(|lease| valid_lease(lease))
    {
        record.display_lease = Some(lease.into());
        record.display_state = "recovery_required".into();
        return;
    }
    record.display_state = response["error"]["code"]
        .as_str()
        .unwrap_or("apply_unverified")
        .into();
}

pub(super) fn accept_status(record: &mut Record, response: &Value) -> bool {
    record.display_status_verified = false;
    if response["ok"] != true {
        return false;
    }
    if response["operation_id"] != record.display_operation() && response["state"] != "idle" {
        return false;
    }
    match response["state"].as_str() {
        Some("idle") if response["lease"].is_null() && response["operation_id"].is_null() => {
            record.display_lease = None;
            record.display_attempted = false;
            record.display_status_verified = false;
            true
        }
        Some("leased" | "recovery_required") if response["owned_by_caller"] == true => {
            if record.display_attempted
                && response["lease"].as_str().is_some_and(valid_lease)
                && (record.display_lease.is_none()
                    || response["lease"].as_str() == record.display_lease.as_deref())
            {
                record.display_lease = response["lease"].as_str().map(str::to_owned);
                record.display_state = response["state"].as_str().unwrap().into();
                record.display_status_verified = true;
                return true;
            }
            false
        }
        _ => false,
    }
}

pub(super) fn accept_restore(record: &mut Record, response: &Value) -> bool {
    if record.display_status_verified
        && response["ok"] == true
        && response["restored"] == true
        && response["lease"].as_str() == record.display_lease.as_deref()
    {
        record.display_lease = None;
        record.display_attempted = false;
        record.display_status_verified = false;
        record.display_state = "restored".into();
        return true;
    }
    if response["recovery_required"] == true {
        record.display_state = "recovery_required".into();
    }
    false
}

fn valid_lease(lease: &str) -> bool {
    lease.starts_with("display-")
        && lease.len() == 24
        && lease[8..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Module unload has no provider tick loop. Try the owner-scoped journal once;
/// a failed exchange leaves the provider's durable recovery journal intact.
pub(super) fn shutdown(record: &mut Record, send: &mut Send<'_>) -> bool {
    if !record.display_attempted && record.display_lease.is_none() {
        return true;
    }
    if !record.display_status_verified {
        let Some(response) = exchange(record, "status", json!({"action":"status"}), send) else {
            record.display_state = "cleanup_pending".into();
            return false;
        };
        if !accept_status(record, &response) {
            record.display_state = "cleanup_pending".into();
            return false;
        }
        if record.display_lease.is_none() {
            return true;
        }
    }
    let action = if record.display_state == "recovery_required" {
        "recover"
    } else {
        "restore"
    };
    let Some(response) = exchange(
        record,
        action,
        json!({"action":action,"lease":record.display_lease}),
        send,
    ) else {
        record.display_state = "cleanup_pending".into();
        return false;
    };
    accept_restore(record, &response)
}

fn exchange(record: &Record, action: &str, request: Value, send: &mut Send<'_>) -> Option<Value> {
    let mut pending = rpc::Pending::new(
        "amaterasu.display/1",
        request,
        format!("ss-{:x}-display-shutdown-{action}", record.id),
        None,
    );
    for turn in 0..8 {
        match pending.poll(send, turn) {
            rpc::Progress::Result(response) => {
                let _ = pending.release_result(send);
                return Some(response);
            }
            rpc::Progress::Waiting => {}
            rpc::Progress::Failed | rpc::Progress::Unresolved => break,
        }
    }
    let _ = pending.shutdown(send);
    None
}
