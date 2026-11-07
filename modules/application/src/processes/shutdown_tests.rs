use super::*;

fn missing_request(operation_id: &str) -> Value {
    json!({"action":"launch", "operation_id":operation_id,
        "executable":std::env::temp_dir().join("amaterasu-no-such-shutdown-launcher")})
}

fn cleanup_lease(manager: &mut ProcessManager, operation_id: &str) {
    let reply = manager.handle("alice", json!({
        "action":"prepare", "operation_id":operation_id,
        "preparation":[{"undo":std::env::temp_dir().join("amaterasu-no-such-shutdown-undo").to_string_lossy()}],
    }));
    assert_eq!(reply["preparation"]["state"], "ready", "{reply}");
}

#[test]
fn announcement_closes_new_admission_and_preserves_existing_replay_and_consumers() {
    let mut manager = ProcessManager::default();
    let request = missing_request("owned");
    let reply = manager.handle("alice", request.clone());
    let id = reply["process"]["process_id"].as_u64().unwrap();
    cleanup_lease(&mut manager, "owned-prepare");
    let now = Instant::now();
    let deadline = now + Duration::from_secs(20);
    manager.prepare_shutdown_until(deadline);
    assert!(!manager.shutdown_started);
    let lease = manager.preparations.values().next().unwrap();
    assert!(!lease.stopping);
    assert!(lease.lifecycle.is_ready());
    assert_eq!(
        manager.handle("alice", request)["process"]["process_id"],
        id
    );
    assert_eq!(
        manager.handle("alice", missing_request("new"))["code"],
        "host_stopping"
    );
    assert_eq!(
        manager.handle(
            "alice",
            json!({"action":"prepare", "operation_id":"new-prepare", "preparation":[{}]})
        )["code"],
        "host_stopping"
    );
    assert_eq!(
        manager.handle("alice", json!({"action":"status", "process_id":id}))["ok"],
        true
    );
    assert_eq!(
        manager.handle("alice", json!({"action":"lookup", "operation_id":"owned"}))["ok"],
        true
    );
    manager.prepare_shutdown_until(deadline + Duration::from_secs(100));
    assert_eq!(manager.shutdown_deadline, Some(deadline));
}

#[test]
fn announcement_caps_300_second_main_grace_without_resetting_an_earlier_attempt() {
    let mut manager = ProcessManager::default();
    let reply = manager.handle("alice", missing_request("main-grace"));
    let id = reply["process"]["process_id"].as_u64().unwrap();
    let now = Instant::now();
    let record = manager.records.get_mut(&id).unwrap();
    record.spec.exit_timeout_seconds = 300;
    record.stop_deadline = Some(now + Duration::from_secs(300));
    let deadline = now + Duration::from_secs(20);
    manager.prepare_shutdown_until(deadline);
    assert_eq!(
        manager.records.get(&id).unwrap().stop_deadline,
        Some(deadline - REAP_TIMEOUT)
    );
    manager.records.get_mut(&id).unwrap().stop_deadline = Some(now + Duration::from_secs(1));
    manager.prepare_shutdown_until(deadline + Duration::from_secs(100));
    assert_eq!(
        manager.records.get(&id).unwrap().stop_deadline,
        Some(now + Duration::from_secs(1))
    );
}

#[test]
fn expired_repeated_shutdown_keeps_owed_undo_and_never_renews_the_deadline() {
    let mut manager = ProcessManager::default();
    cleanup_lease(&mut manager, "expired");
    let deadline = Instant::now();
    manager.prepare_shutdown_until(deadline);
    assert_eq!(
        manager.poll_shutdown(deadline),
        Err("process_cleanup_failed")
    );
    let lease = manager.preparations.values().next().unwrap();
    assert!(lease.lifecycle.cleanup_failed());
    assert_eq!(lease.lifecycle.cleanup.remaining, 1);
    assert!(!lease.lifecycle.has_active_command());
    manager.prepare_shutdown_until(deadline + Duration::from_secs(300));
    assert_eq!(manager.shutdown_deadline, Some(deadline));
    assert_eq!(manager.shutdown(), Err("process_cleanup_failed"));
    assert_eq!(manager.shutdown_deadline, Some(deadline));
    let before_drop = Instant::now();
    drop(manager);
    assert!(before_drop.elapsed() < Duration::from_secs(1));
}

#[test]
fn announced_drop_does_not_start_an_owed_undo() {
    let mut manager = ProcessManager::default();
    cleanup_lease(&mut manager, "no-undo-on-drop");
    let deadline = Instant::now() + Duration::from_secs(20);
    manager.prepare_shutdown_until(deadline);
    // The declared consumer has not authorized application quiesce yet.
    let before_drop = Instant::now();
    drop(manager);
    assert!(before_drop.elapsed() < Duration::from_secs(1));
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_steam_tracker_created_after_announcement_inherits_the_retained_deadline() {
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-late-steam-deadline-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir(&directory).unwrap();
    let mut manager = ProcessManager::default();
    let reply = manager.handle("alice", missing_request("late-steam"));
    let id = reply["process"]["process_id"].as_u64().unwrap();
    let now = Instant::now();
    let deadline = now + Duration::from_secs(20);
    manager.prepare_shutdown_until(deadline);
    let record = manager.records.get_mut(&id).unwrap();
    record.spec.exit_timeout_seconds = 300;
    record.spec.steam_id = Some("42".into());
    record.state = ProcessState::Running;
    record.steam_tracker = SteamProcessTracker::begin(directory.to_str().unwrap(), Some("42"));
    assert!(record.steam_tracker.is_some());
    std::fs::remove_dir(&directory).unwrap();
    crate::steam_tracking::fail_next_process_scan();
    record.begin_steam_stop(now);
    assert!(matches!(record.state, ProcessState::CleanupFailed { .. }));
    crate::steam_tracking::fail_next_process_scan();
    assert_eq!(
        record
            .steam_tracker
            .as_mut()
            .unwrap()
            .advance_stop(deadline),
        SteamStopProgress::Failed("steam_process_scan_failed"),
    );
}
