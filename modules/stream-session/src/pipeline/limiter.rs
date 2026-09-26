use super::record::Record;
use super::rpc::{self, Send};
use serde_json::{Value, json};

pub(super) fn apply(record: &Record) -> Value {
    let snapshot = record
        .config
        .frame_limiter_settings
        .as_ref()
        .expect("validated frozen limiter snapshot");
    let mut request = snapshot.clone();
    let fields = request.as_object_mut().expect("validated snapshot object");
    fields.insert("action".into(), json!("apply"));
    fields.insert("session_id".into(), json!(record.limiter_session()));
    fields.insert(
        "executable_path".into(),
        json!(record.config.frame_limiter_executable_path),
    );
    request
}

/// An apply is owned only after the provider says it verified the effect.
/// Status can also reveal a lease after an uncertain apply result.
pub(super) fn accept_apply(record: &mut Record, response: &Value) {
    if matches_session_reply(record, response, "apply")
        && response["ok"] == true
        && response["applied"] == true
        && matches!(
            response["state"].as_str(),
            Some("applied" | "already_applied")
        )
        && let Some(lease) = response["lease"].as_str().filter(|lease| !lease.is_empty())
    {
        record.limiter_lease = Some(lease.into());
        record.limiter_state = "applied".into();
        return;
    }
    record.limiter_state =
        if !matches_session_reply(record, response, "apply") || response["applied"] == true {
            "apply_unverified".into()
        } else {
            response["state"]
                .as_str()
                .unwrap_or("runtime_unavailable")
                .into()
        };
    if matches_session_reply(record, response, "apply") {
        adopt_cleanup_lease(record, response);
    }
}

pub(super) fn accept_status(record: &mut Record, response: &Value) {
    if !matches_session_reply(record, response, "status") || response["ok"] != true {
        record.limiter_state = "status_unverified".into();
        return;
    }
    if !adopt_cleanup_lease(record, response) {
        record.limiter_state = "status_unverified".into();
        return;
    }
    if record.limiter_lease.is_none() {
        record.limiter_state = response["state"]
            .as_str()
            .unwrap_or("runtime_unavailable")
            .into();
    }
}

pub(super) fn status_confirms_no_lease(record: &Record, response: &Value) -> bool {
    response["ok"] == true
        && response["version"] == 1
        && response["action"] == "status"
        && response["state"] == "not_found"
        && response["applied"] == false
        && response["session_id"] == record.limiter_session()
        && response["leases"].as_array().is_some_and(Vec::is_empty)
}

fn adopt_cleanup_lease(record: &mut Record, response: &Value) -> bool {
    let Some(entries) = response["leases"].as_array() else {
        return false;
    };
    if entries.is_empty() {
        return true;
    }
    // A status result is used to recover ownership after an uncertain Apply.
    // Require the provider's exact executable identity, not just the session ID.
    let Some(entry) = entries.first().filter(|_| entries.len() == 1) else {
        return false;
    };
    if entry["session_id"] != record.limiter_session()
        || entry["executable_path"].as_str()
            != record.config.frame_limiter_executable_path.as_deref()
        || entry["provider"] != "rtss"
        || !matches!(entry["state"].as_str(), Some("applied" | "stale_lease"))
        || entry["applied"] != (entry["state"] == "applied")
    {
        return false;
    }
    let Some(lease) = entry["lease"].as_str().filter(|lease| !lease.is_empty()) else {
        return false;
    };
    record.limiter_lease = Some(lease.into());
    record.limiter_state = if entry["applied"] == true {
        "applied".into()
    } else {
        "cleanup_pending".into()
    };
    true
}

pub(super) fn accept_restore(record: &mut Record, response: &Value) -> bool {
    if response["ok"] == true
        && response["version"] == 1
        && response["action"] == "restore"
        && response["state"] == "restored"
        && response["applied"] == false
    {
        record.limiter_lease = None;
        record.limiter_state = "restored".into();
        true
    } else {
        record.limiter_state = response["state"]
            .as_str()
            .unwrap_or("restore_failed")
            .into();
        false
    }
}

fn matches_session_reply(record: &Record, response: &Value, action: &str) -> bool {
    response["version"] == 1
        && response["action"] == action
        && response["session_id"] == record.limiter_session()
}

/// Module unload has no tick loop. Spend a fixed number of broker turns on
/// status and restore, retaining ownership in the record if either is unknown.
pub(super) fn shutdown(record: &mut Record, send: &mut Send<'_>) -> bool {
    if !record.limiter_attempted && record.limiter_lease.is_none() {
        return true;
    }
    if record.limiter_lease.is_none() {
        let request = json!({"action":"status","version":1,"session_id":record.limiter_session()});
        let Some(response) = exchange(record, "status", request, send) else {
            record.limiter_state = "cleanup_pending".into();
            return false;
        };
        accept_status(record, &response);
        if record.limiter_lease.is_none() {
            if status_confirms_no_lease(record, &response) {
                record.limiter_attempted = false;
                return true;
            }
            return false;
        }
    }
    let request = json!({"action":"restore","version":1,"lease":record.limiter_lease});
    let Some(response) = exchange(record, "restore", request, send) else {
        record.limiter_state = "cleanup_pending".into();
        return false;
    };
    accept_restore(record, &response)
}

fn exchange(record: &Record, action: &str, request: Value, send: &mut Send<'_>) -> Option<Value> {
    let mut pending = rpc::Pending::new(
        "amaterasu.frame-limiter/1",
        request,
        format!("{}-shutdown-{action}", record.limiter_session()),
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
