use super::*;
use serde_json::json;

#[test]
fn settings_are_fixed_to_the_management_principal() {
    assert!(settings_caller_is_authorized("amaterasu.management"));
    assert!(!settings_caller_is_authorized("amaterasu.nvhttp"));
    assert!(!settings_caller_is_authorized("amaterasu.playnite.other"));
}

#[test]
fn settings_update_is_flat_and_cannot_retarget_a_product_owner() {
    let action: Action = serde_json::from_value(json!({
        "action":"settings_update",
        "enabled":true,
        "sync_all_installed":true,
        "expected_revision":3
    }))
    .unwrap();
    assert!(action.is_settings());

    assert!(
        serde_json::from_value::<Action>(json!({
            "action":"settings_update",
            "enabled":true,
            "expected_revision":3,
            "owner":"amaterasu.steam"
        }))
        .is_err()
    );
}

#[test]
fn status_and_refresh_accept_no_owner_selector() {
    let status: Action = serde_json::from_value(json!({"action":"status"})).unwrap();
    assert!(!status.is_settings());
    let refresh: Action = serde_json::from_value(json!({"action":"refresh"})).unwrap();
    assert!(!refresh.is_settings());
}

#[test]
fn disconnected_snapshot_worker_does_not_stick_in_busy_state() {
    let (sender, receiver) = mpsc::channel();
    drop(sender);
    let mut worker = SnapshotWorker {
        receiver: Some(receiver),
        _thread: None,
        settings_epoch: 0,
    };

    assert!(worker.take_result().unwrap().is_err());
    assert!(!worker.busy());
}
