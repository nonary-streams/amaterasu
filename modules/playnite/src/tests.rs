use super::*;
use serde_json::json;

fn process(process_id: u32, path: &str) -> discovery::ProcessImage {
    discovery::ProcessImage {
        process_id,
        path: path.into(),
    }
}

#[test]
fn immediate_stop_refresh_sees_restarted_playnite_during_discovery_throttle() {
    let now = Instant::now();
    let original = process(41, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
    let restarted = process(73, r"C:\Program Files\Playnite\Playnite.FullscreenApp.exe");
    let mut last_discovery = discovery::Discovery {
        running: true,
        processes: vec![original.clone()],
        executable_paths: vec![original.path.clone()],
    };
    let mut next_discovery = now + Duration::from_secs(2);
    let mut discovery_calls = 0;

    refresh_discovery_at(&mut last_discovery, &mut next_discovery, now, false, || {
        discovery_calls += 1;
        discovery::Discovery::default()
    });
    assert_eq!(discovery_calls, 0);
    assert_eq!(last_discovery.processes, vec![original]);

    refresh_discovery_at(&mut last_discovery, &mut next_discovery, now, true, || {
        discovery_calls += 1;
        discovery::Discovery {
            running: true,
            processes: vec![restarted.clone()],
            executable_paths: vec![restarted.path.clone()],
        }
    });

    assert_eq!(discovery_calls, 1);
    assert_eq!(last_discovery.processes, vec![restarted]);
    assert!(next_discovery > now);
}

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
