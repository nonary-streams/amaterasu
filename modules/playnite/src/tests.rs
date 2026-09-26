use super::*;
use serde_json::json;

fn process(process_id: u32, path: &str) -> discovery::ProcessImage {
    discovery::ProcessImage {
        process_id,
        path: path.into(),
        creation_time: u64::from(process_id),
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
fn settings_update_preserves_duplicate_game_exclusions() {
    let duplicate_game_ids = vec!["Game-ID", "game-id", "Game-ID"];
    let action: Action = serde_json::from_value(json!({
        "action":"settings_update",
        "enabled":true,
        "exclude_games":duplicate_game_ids,
        "expected_revision":3
    }))
    .unwrap();

    let Action::SettingsUpdate { exclude_games, .. } = action else {
        panic!("expected settings update")
    };

    assert_eq!(
        exclude_games,
        Some(vec!["Game-ID".into(), "game-id".into(), "Game-ID".into()])
    );
}

#[test]
fn settings_update_preserves_duplicate_category_filters() {
    let sync_categories = vec!["Arcade", "arcade", "Arcade"];
    let exclude_categories = vec!["Strategy", "strategy", "Strategy"];
    let action: Action = serde_json::from_value(json!({
        "action":"settings_update",
        "enabled":true,
        "sync_categories":sync_categories,
        "exclude_categories":exclude_categories,
        "expected_revision":3
    }))
    .unwrap();

    let Action::SettingsUpdate {
        sync_categories,
        exclude_categories,
        ..
    } = action
    else {
        panic!("expected settings update")
    };

    assert_eq!(
        sync_categories,
        Some(vec!["Arcade".into(), "arcade".into(), "Arcade".into()])
    );
    assert_eq!(
        exclude_categories,
        Some(vec![
            "Strategy".into(),
            "strategy".into(),
            "Strategy".into()
        ])
    );
}

#[test]
fn settings_update_is_flat_and_cannot_retarget_a_product_owner() {
    let action: Action = serde_json::from_value(json!({
        "action":"settings_update",
        "enabled":true,
        "sync_all_installed":true,
        "autosync_remove_uninstalled":false,
        "focus_attempts":9,
        "focus_timeout_secs":45,
        "focus_exit_on_first":true,
        "fullscreen_entry_enabled":true,
        "sync_plugins":["included-plugin","second-plugin"],
        "exclude_plugins":["blocked-plugin"],
        "expected_revision":3
    }))
    .unwrap();
    assert!(action.is_settings());
    let Action::SettingsUpdate {
        focus_attempts,
        focus_timeout_secs,
        focus_exit_on_first,
        fullscreen_entry_enabled,
        sync_plugins,
        exclude_plugins,
        ..
    } = action
    else {
        panic!("expected settings update")
    };
    assert_eq!(focus_attempts, Some(9));
    assert_eq!(focus_timeout_secs, Some(45));
    assert_eq!(focus_exit_on_first, Some(true));
    assert_eq!(fullscreen_entry_enabled, Some(true));
    assert_eq!(
        sync_plugins,
        Some(vec!["included-plugin".into(), "second-plugin".into()])
    );
    assert_eq!(exclude_plugins, Some(vec!["blocked-plugin".into()]));

    let legacy_update: Action = serde_json::from_value(json!({
        "action":"settings_update",
        "enabled":true,
        "expected_revision":3
    }))
    .unwrap();
    let Action::SettingsUpdate {
        autosync_remove_uninstalled,
        focus_attempts,
        focus_timeout_secs,
        focus_exit_on_first,
        fullscreen_entry_enabled,
        sync_plugins,
        exclude_plugins,
        ..
    } = legacy_update
    else {
        panic!("expected settings update")
    };
    assert_eq!(autosync_remove_uninstalled, None);
    assert_eq!(focus_attempts, None);
    assert_eq!(focus_timeout_secs, None);
    assert_eq!(focus_exit_on_first, None);
    assert_eq!(fullscreen_entry_enabled, None);
    assert_eq!(sync_plugins, None);
    assert_eq!(exclude_plugins, None);

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
fn fullscreen_ticket_actions_are_launch_only_and_reject_owner_selectors() {
    let start: Action = serde_json::from_value(json!({
        "action":"fullscreen_start",
        "operation_id":"fullscreen-operation"
    }))
    .unwrap();
    assert!(start.is_fullscreen_launch());
    assert!(!start.is_settings());
    assert!(launch_caller_is_authorized(LAUNCH_CALLER));
    assert!(!launch_caller_is_authorized(MANAGEMENT_CALLER));

    let status: Action = serde_json::from_value(json!({
        "action":"fullscreen_status",
        "ticket":"ab".repeat(24)
    }))
    .unwrap();
    assert!(status.is_fullscreen_launch());

    assert!(
        serde_json::from_value::<Action>(json!({
            "action":"fullscreen_start",
            "operation_id":"fullscreen-operation",
            "executable":"C:\\Temp\\attacker.exe"
        }))
        .is_err()
    );
}

#[test]
fn disconnected_snapshot_worker_does_not_stick_in_busy_state() {
    let (sender, receiver) = mpsc::channel();
    drop(sender);
    let mut worker = SnapshotWorker {
        receiver: Some(receiver),
        _thread: None,
        settings_epoch: 0,
        processes: Vec::new(),
    };

    assert!(worker.take_result().unwrap().is_err());
    assert!(!worker.busy());
}

#[test]
fn process_restart_revokes_launch_eligibility_until_a_current_import_completes() {
    let old_process = process(41, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
    let new_process = process(73, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
    let mut eligible = ["a1b2c3d4-1234-5678-9abc-0123456789ab".into()]
        .into_iter()
        .collect();
    let discovered = discovery::Discovery {
        running: true,
        processes: vec![new_process.clone()],
        executable_paths: vec![new_process.path.clone()],
    };
    let now = Instant::now();
    let mut next_auto_sync = Some(now + Duration::from_secs(30));

    update_discovery_state(
        std::slice::from_ref(&old_process),
        &discovered.processes,
        &mut eligible,
        &mut next_auto_sync,
        true,
        now,
    );
    assert!(eligible.is_empty());
    assert_eq!(next_auto_sync, Some(now));
    assert!(!can_publish_eligibility(Some(&[old_process]), &discovered));
    assert!(can_publish_eligibility(Some(&[new_process]), &discovered));

    let stopped = discovery::Discovery::default();
    assert!(!can_publish_eligibility(Some(&[]), &stopped));
}

#[test]
fn manual_mode_keeps_imported_launches_on_focus_change_but_revokes_on_disable() {
    let now = Instant::now();
    let playnite = process(41, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
    let mut current = ProductSettings {
        enabled: true,
        auto_sync: false,
        ..ProductSettings::default()
    };
    let game_id = "a1b2c3d4-1234-5678-9abc-0123456789ab".to_owned();
    let mut eligible = [game_id.clone()].into_iter().collect();
    let mut catalog_processes = Some(vec![playnite.clone()]);
    let mut next_auto_sync = None;

    let previous = current.clone();
    current.focus_attempts += 1;
    current.fullscreen_entry_enabled = true;
    let same_catalog_policy = apply_settings_transition(
        Some(&previous),
        &current,
        &mut eligible,
        &mut catalog_processes,
        &mut next_auto_sync,
        now,
    );
    let (_sender, receiver) = mpsc::channel();
    let mut in_flight = SnapshotWorker {
        receiver: Some(receiver),
        _thread: None,
        settings_epoch: 7,
        processes: vec![playnite.clone()],
    };
    in_flight.retain_for_settings_epoch(8, same_catalog_policy);
    assert_eq!(in_flight.settings_epoch, 8);
    assert!(eligible.contains(&game_id));
    assert_eq!(catalog_processes, Some(vec![playnite.clone()]));
    assert_eq!(next_auto_sync, None);

    let previous = current.clone();
    current.enabled = false;
    let same_catalog_policy = apply_settings_transition(
        Some(&previous),
        &current,
        &mut eligible,
        &mut catalog_processes,
        &mut next_auto_sync,
        now,
    );
    in_flight.retain_for_settings_epoch(9, same_catalog_policy);
    assert_eq!(in_flight.settings_epoch, 8);
    assert!(eligible.is_empty());
    assert_eq!(catalog_processes, None);

    let previous = current.clone();
    current.enabled = true;
    current.auto_sync = true;
    let same_catalog_policy = apply_settings_transition(
        Some(&previous),
        &current,
        &mut eligible,
        &mut catalog_processes,
        &mut next_auto_sync,
        now,
    );
    in_flight.retain_for_settings_epoch(10, same_catalog_policy);
    assert_eq!(in_flight.settings_epoch, 8);
    assert!(eligible.is_empty());
    assert_eq!(next_auto_sync, Some(now));
}

#[test]
fn auto_sync_policy_change_revokes_eligibility_and_schedules_reconciliation() {
    let now = Instant::now();
    let mut previous = ProductSettings {
        enabled: true,
        auto_sync: true,
        ..ProductSettings::default()
    };
    let mut eligible = ["a1b2c3d4-1234-5678-9abc-0123456789ab".to_owned()]
        .into_iter()
        .collect();
    let mut catalog_processes = Some(vec![process(
        41,
        r"C:\Program Files\Playnite\Playnite.DesktopApp.exe",
    )]);
    let mut next_auto_sync = Some(now + Duration::from_secs(30));

    let mut current = previous.clone();
    current.focus_timeout_secs += 1;
    apply_settings_transition(
        Some(&previous),
        &current,
        &mut eligible,
        &mut catalog_processes,
        &mut next_auto_sync,
        now,
    );
    assert_eq!(eligible.len(), 1);
    assert_eq!(next_auto_sync, Some(now + Duration::from_secs(30)));

    previous = current.clone();
    current.recent_games += 1;
    apply_settings_transition(
        Some(&previous),
        &current,
        &mut eligible,
        &mut catalog_processes,
        &mut next_auto_sync,
        now,
    );
    assert!(eligible.is_empty());
    assert!(catalog_processes.is_none());
    assert_eq!(next_auto_sync, Some(now));
}

#[test]
fn in_flight_snapshot_cannot_restore_launches_after_disable_or_process_restart() {
    let now = Instant::now();
    let old_process = process(41, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
    let new_process = process(73, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
    let old_discovery = discovery::Discovery {
        running: true,
        processes: vec![old_process.clone()],
        executable_paths: vec![old_process.path.clone()],
    };
    let new_discovery = discovery::Discovery {
        running: true,
        processes: vec![new_process.clone()],
        executable_paths: vec![new_process.path.clone()],
    };
    let previous = ProductSettings {
        enabled: true,
        auto_sync: true,
        ..ProductSettings::default()
    };
    let mut disabled = previous.clone();
    disabled.enabled = false;
    let mut eligible = ["a1b2c3d4-1234-5678-9abc-0123456789ab".to_owned()]
        .into_iter()
        .collect();
    let mut catalog_processes = Some(vec![old_process.clone()]);
    let mut next_auto_sync = Some(now + Duration::from_secs(30));
    apply_settings_transition(
        Some(&previous),
        &disabled,
        &mut eligible,
        &mut catalog_processes,
        &mut next_auto_sync,
        now,
    );

    assert!(eligible.is_empty());
    assert!(catalog_processes.is_none());
    assert_eq!(next_auto_sync, None);
    assert!(snapshot_processes_match_instance(
        std::slice::from_ref(&old_process),
        &old_discovery,
    ));
    assert!(!snapshot_processes_match_instance(
        std::slice::from_ref(&old_process),
        &new_discovery,
    ));
    assert!(!can_publish_eligibility(
        Some(std::slice::from_ref(&old_process)),
        &new_discovery,
    ));
}

#[test]
fn unsolicited_current_generation_snapshot_starts_reconciliation_only_while_enabled() {
    let old_process = process(41, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
    let new_process = process(73, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
    let discovery = discovery::Discovery {
        running: true,
        processes: vec![old_process.clone()],
        executable_paths: vec![old_process.path.clone()],
    };
    let snapshot = Snapshot {
        games: vec![],
        categories: vec![],
        plugins: vec![],
    };
    let event = connector::SnapshotEvent {
        snapshot: snapshot.clone(),
        processes: vec![old_process],
        settings_epoch: 7,
    };
    let mut settings = ProductSettings {
        enabled: true,
        auto_sync: true,
        ..ProductSettings::default()
    };
    assert!(unsolicited_snapshot_is_current(
        &event, &settings, 7, &discovery,
    ));
    let mut catalog = catalog_sync::CatalogSync::default();
    catalog.start(&event.snapshot, &settings, 7, 0).unwrap();
    assert!(catalog.busy());

    settings.enabled = false;
    assert!(!unsolicited_snapshot_is_current(
        &event, &settings, 8, &discovery,
    ));
    settings.enabled = true;
    let restarted = discovery::Discovery {
        running: true,
        processes: vec![new_process.clone()],
        executable_paths: vec![new_process.path],
    };
    assert!(!unsolicited_snapshot_is_current(
        &event, &settings, 7, &restarted,
    ));
}

#[test]
fn discovery_retries_automatic_sync_immediately_only_for_a_new_running_process() {
    let now = Instant::now();
    let next_attempt = Some(now + Duration::from_secs(30));
    let process = process(73, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
    let mut eligible = Default::default();
    let mut next_auto_sync = next_attempt;

    update_discovery_state(
        &[],
        std::slice::from_ref(&process),
        &mut eligible,
        &mut next_auto_sync,
        true,
        now,
    );
    assert_eq!(next_auto_sync, Some(now));

    next_auto_sync = next_attempt;
    update_discovery_state(
        std::slice::from_ref(&process),
        std::slice::from_ref(&process),
        &mut eligible,
        &mut next_auto_sync,
        true,
        now,
    );
    assert_eq!(next_auto_sync, next_attempt);

    update_discovery_state(
        std::slice::from_ref(&process),
        &[],
        &mut eligible,
        &mut next_auto_sync,
        true,
        now,
    );
    assert_eq!(next_auto_sync, next_attempt);

    update_discovery_state(
        &[],
        std::slice::from_ref(&process),
        &mut eligible,
        &mut next_auto_sync,
        false,
        now,
    );
    assert_eq!(next_auto_sync, next_attempt);
}
