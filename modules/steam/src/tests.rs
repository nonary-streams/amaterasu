use super::*;
use crate::configuration::CONFIGURATION_TIMEOUT_MS;
use crate::settings::{Exclusion, ProductSettings};
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT,
    STATUS_OK,
};
use amaterasu_module_sdk::{Command, ManagedModule, Response};
use image::ImageEncoder;
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;
use std::time::{Duration, Instant};

fn upgrade_game(app_id: u32, needs_upgrade: bool) -> Game {
    Game {
        stable_id: format!("steam:{app_id}"),
        app_id,
        name: "Art test".into(),
        install_directory: "C:/steam/steamapps/common/test".into(),
        library_directory: "C:/steam".into(),
        state_flags: 4,
        installed: true,
        last_updated: 123,
        app_type: Some("game".into()),
        last_played: None,
        playtime_minutes: None,
        artwork_revision: Some("local-42".into()),
        artwork_source_path: Some("C:/steam/appcache/42.png".into()),
        artwork_is_remote: false,
        artwork_local_revision: Some("local-42".into()),
        artwork_needs_remote_upgrade: needs_upgrade,
        artwork_candidate_signature: "candidate-1".into(),
    }
}

fn remote_test_jpeg() -> Vec<u8> {
    let pixels = vec![128_u8; 600 * 900 * 3];
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut bytes)
        .write_image(&pixels, 600, 900, image::ExtendedColorType::Rgb8)
        .unwrap();
    bytes
}

fn drain_artwork_until(provider: &mut Provider, deadline: Instant) -> Vec<(u64, Value)> {
    while Instant::now() < deadline {
        let result = provider.drain_remote_artwork();
        if provider.upgrade_pending.is_none() {
            return result;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("fake artwork worker did not finish");
}

#[test]
fn local_validation_resumes_batches_without_publishing_partial_scans() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "amaterasu-steam-validation-{}-{unique}",
        std::process::id()
    ));
    let artwork_directory = root.join("appcache/librarycache");
    std::fs::create_dir_all(&artwork_directory).unwrap();
    let mut provider = Provider {
        enabled: true,
        snapshot_complete: true,
        revision: 1,
        roots: vec![root.clone()],
        ..Provider::default()
    };
    provider.games.insert(99, upgrade_game(99, false));
    provider.local_artwork.set_validation_batch_targets(2);
    provider
        .local_artwork
        .set_validation_batch_duration(Duration::from_nanos(1));
    let mut scan = Scan::default();
    scan.finished = true;
    for app_id in 1..=6 {
        let path = artwork_directory.join(format!("{app_id}_library_600x900_2x.jpg"));
        if app_id == 1 {
            let mut corrupt = vec![
                0xff, 0xd8, 0xff, 0xc0, 0, 17, 8, 3, 132, 2, 88, 3, 1, 17, 0, 2, 17, 0, 3, 17, 0,
            ];
            corrupt.extend_from_slice(&[0xff, 0xd9]);
            std::fs::write(&path, corrupt).unwrap();
            let fallback = artwork_directory.join("1_library_600x900.png");
            let pixels = vec![64_u8; 300 * 450 * 4];
            let mut png = Vec::new();
            image::codecs::png::PngEncoder::new(&mut png)
                .write_image(&pixels, 300, 450, image::ExtendedColorType::Rgba8)
                .unwrap();
            std::fs::write(fallback, png).unwrap();
        } else {
            std::fs::write(&path, remote_test_jpeg()).unwrap();
        }
        let mut game = upgrade_game(app_id, false);
        game.library_directory = root.display().to_string();
        game.artwork_source_path = Some(path.display().to_string());
        scan.games.insert(app_id, game);
    }
    provider.scan = Some(scan);

    let deadline = Instant::now() + Duration::from_secs(5);
    while provider.artwork_validation_results.len() < 2 {
        provider.tick();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(provider.artwork_validation_results.len(), 2);
    assert!(provider.games.contains_key(&99));
    assert_eq!(provider.revision, 1);

    while provider.scan.is_some() {
        provider.tick();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(provider.revision, 2);
    assert_eq!(provider.games.len(), 6);
    assert!(!provider.games.contains_key(&99));
    assert!(provider.games.values().all(|game| !game.artwork_is_remote));
    assert!(provider.games[&1].artwork_needs_remote_upgrade);
    assert!(
        provider.games[&1]
            .artwork_source_path
            .as_deref()
            .unwrap()
            .ends_with("1_library_600x900.png")
    );
    drop(provider);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn disabling_steam_discards_partial_local_validation() {
    let mut provider = Provider {
        enabled: true,
        ..Provider::default()
    };
    provider.local_artwork.set_validation_batch_targets(1);
    let mut scan = Scan::default();
    scan.finished = true;
    for app_id in 1..=6 {
        scan.games.insert(app_id, upgrade_game(app_id, false));
    }
    provider.scan = Some(scan);
    let deadline = Instant::now() + Duration::from_secs(5);
    while provider.artwork_validation_results.is_empty() {
        provider.tick();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    provider.set_enabled(false);
    assert!(provider.artwork_validation_results.is_empty());
    assert!(provider.scan.is_none());
    provider.process_local_artwork();
    assert!(provider.artwork_validation_results.is_empty());
}

#[test]
fn low_resolution_art_promotes_only_after_successful_current_remote_fetch() {
    let mut provider = Provider {
        enabled: true,
        snapshot_complete: true,
        ..Provider::default()
    };
    provider.games.insert(42, upgrade_game(42, true));
    let bytes = remote_test_jpeg();
    provider
        .remote_artwork
        .start_with_fetcher(move |app_id| {
            assert_eq!(app_id, 42);
            Ok(bytes.clone())
        })
        .unwrap();
    provider.upgrade_queue.push_back(42);
    provider.enqueue_next_upgrade();
    assert_eq!(
        provider.games[&42].artwork_revision.as_deref(),
        Some("local-42")
    );
    assert!(drain_artwork_until(&mut provider, Instant::now() + Duration::from_secs(2)).is_empty());
    let remote = artwork::remote_revision(42, 123).unwrap();
    assert_eq!(
        provider.games[&42].artwork_revision.as_deref(),
        Some(remote.as_str())
    );
    assert!(provider.games[&42].artwork_is_remote);
    assert_eq!(provider.revision, 1);
    assert!(provider.read_artwork(9, 42, "local-42", 0, 1000).is_err());
    assert!(
        provider
            .read_artwork(10, 42, &remote, 0, 1000)
            .unwrap()
            .is_none()
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let responses = loop {
        let responses = provider.drain_remote_artwork();
        if !responses.is_empty() {
            break responses;
        }
        assert!(
            Instant::now() < deadline,
            "fake artwork read did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].0, 10);
    assert_eq!(responses[0].1["revision"], remote);
    assert_eq!(responses[0].1["mime_type"], "image/png");
}

#[test]
fn restarted_cache_promotes_each_queued_portrait_and_repairs_corrupt_entry() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "amaterasu-steam-upgrade-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    let first_revision = artwork::remote_revision(42, 123).unwrap();
    let second_revision = artwork::remote_revision(43, 123).unwrap();
    let cache_directory = root.join("amaterasu-steam-artwork-v1");
    let first_path = cache_directory.join(format!("42-{first_revision}.png"));
    let second_path = cache_directory.join(format!("43-{second_revision}.png"));

    let mut initial = Provider {
        enabled: true,
        snapshot_complete: true,
        ..Provider::default()
    };
    initial.games.insert(42, upgrade_game(42, true));
    initial.remote_artwork.set_cache_root(Some(root.clone()));
    initial
        .remote_artwork
        .start_with_fetcher(|_| Ok(remote_test_jpeg()))
        .unwrap();
    initial.upgrade_queue.push_back(42);
    initial.enqueue_next_upgrade();
    drain_artwork_until(&mut initial, Instant::now() + Duration::from_secs(5));
    assert!(first_path.exists());
    drop(initial);

    std::fs::write(&second_path, b"corrupt cached portrait").unwrap();
    let fetch_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fetch_count_in_worker = std::sync::Arc::clone(&fetch_count);
    let mut restarted = Provider {
        enabled: true,
        snapshot_complete: true,
        ..Provider::default()
    };
    restarted.games.insert(42, upgrade_game(42, true));
    restarted.games.insert(43, upgrade_game(43, true));
    restarted.remote_artwork.set_cache_root(Some(root.clone()));
    restarted
        .remote_artwork
        .start_with_fetcher(move |app_id| {
            assert_eq!(app_id, 43, "the first portrait should come from disk");
            fetch_count_in_worker.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(remote_test_jpeg())
        })
        .unwrap();
    restarted.upgrade_queue.extend([42, 43]);
    restarted.enqueue_next_upgrade();
    let deadline = Instant::now() + Duration::from_secs(10);
    while restarted.upgrade_pending.is_some() || !restarted.upgrade_queue.is_empty() {
        restarted.drain_remote_artwork();
        assert!(Instant::now() < deadline, "queued upgrades did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(fetch_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        restarted.games[&42].artwork_revision.as_deref(),
        Some(first_revision.as_str())
    );
    assert_eq!(
        restarted.games[&43].artwork_revision.as_deref(),
        Some(second_revision.as_str())
    );
    assert!(restarted.games.values().all(|game| game.artwork_is_remote));
    assert_ne!(
        std::fs::read(&second_path).unwrap(),
        b"corrupt cached portrait"
    );
    drop(restarted);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn high_resolution_local_art_stays_local_and_failed_upgrade_keeps_fallback() {
    let mut high = Provider {
        enabled: true,
        snapshot_complete: true,
        ..Provider::default()
    };
    high.games.insert(42, upgrade_game(42, false));
    high.upgrade_queue.push_back(42);
    high.enqueue_next_upgrade();
    assert!(high.upgrade_pending.is_none());
    assert_eq!(
        high.games[&42].artwork_revision.as_deref(),
        Some("local-42")
    );
    let mut uninstalled = upgrade_game(43, true);
    uninstalled.installed = false;
    high.games.insert(43, uninstalled);
    high.upgrade_queue.push_back(43);
    high.enqueue_next_upgrade();
    assert!(high.upgrade_pending.is_none());

    let mut low = Provider {
        enabled: true,
        snapshot_complete: true,
        ..Provider::default()
    };
    low.games.insert(42, upgrade_game(42, true));
    low.remote_artwork
        .start_with_fetcher(|_| Err("artwork_unavailable"))
        .unwrap();
    low.upgrade_queue.push_back(42);
    low.enqueue_next_upgrade();
    assert!(drain_artwork_until(&mut low, Instant::now() + Duration::from_secs(2)).is_empty());
    assert_eq!(low.games[&42].artwork_revision.as_deref(), Some("local-42"));
    assert!(!low.games[&42].artwork_is_remote);
}

#[test]
fn stale_upgrade_after_local_change_or_disable_cannot_replace_newer_reference() {
    let (release, proceed) = std::sync::mpsc::channel::<()>();
    let mut provider = Provider {
        enabled: true,
        snapshot_complete: true,
        ..Provider::default()
    };
    provider.games.insert(42, upgrade_game(42, true));
    provider
        .remote_artwork
        .start_with_fetcher(move |_| {
            proceed.recv().unwrap();
            Ok(remote_test_jpeg())
        })
        .unwrap();
    provider.upgrade_queue.push_back(42);
    provider.enqueue_next_upgrade();
    provider
        .games
        .get_mut(&42)
        .unwrap()
        .artwork_candidate_signature = "candidate-2".into();
    release.send(()).unwrap();
    drain_artwork_until(&mut provider, Instant::now() + Duration::from_secs(2));
    assert_eq!(
        provider.games[&42].artwork_revision.as_deref(),
        Some("local-42")
    );

    provider.upgrade_queue.push_back(42);
    provider.enqueue_next_upgrade();
    provider.set_enabled(false);
    provider.set_enabled(true);
    provider.games.insert(42, upgrade_game(42, true));
    provider.snapshot_complete = true;
    provider.drain_remote_artwork();
    assert_eq!(
        provider.games[&42].artwork_revision.as_deref(),
        Some("local-42")
    );
}

#[test]
fn new_scan_retires_in_flight_upgrade_before_its_old_completion() {
    let (started_sender, started_receiver) = std::sync::mpsc::channel();
    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    let mut provider = Provider {
        enabled: true,
        snapshot_complete: true,
        ..Provider::default()
    };
    provider.games.insert(42, upgrade_game(42, true));
    provider
        .remote_artwork
        .start_with_fetcher(move |_| {
            started_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
            Ok(remote_test_jpeg())
        })
        .unwrap();
    provider.upgrade_queue.push_back(42);
    provider.enqueue_next_upgrade();
    started_receiver
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    assert!(provider.start_scan());
    assert!(provider.scan_in_progress());
    release_sender.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while provider.artwork_completion_count == 0 {
        provider.drain_remote_artwork();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        provider.games[&42].artwork_revision.as_deref(),
        Some("local-42")
    );
    assert_eq!(provider.revision, 0);
}

#[test]
fn expired_client_read_does_not_demote_a_promoted_remote_reference() {
    let mut provider = Provider {
        enabled: true,
        revision: 1,
        snapshot_complete: true,
        ..Provider::default()
    };
    let mut game = upgrade_game(42, true);
    let remote = artwork::remote_revision(42, game.last_updated).unwrap();
    game.artwork_revision = Some(remote.clone());
    game.artwork_is_remote = true;
    provider.games.insert(42, game);
    provider
        .remote_artwork
        .start_with_fetcher(|_| panic!("expired read must never fetch"))
        .unwrap();
    assert!(
        provider
            .read_artwork(7, 42, &remote, 0, 0)
            .unwrap()
            .is_none()
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let responses = loop {
        let responses = provider.drain_remote_artwork();
        if !responses.is_empty() {
            break responses;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(responses[0].1["code"], "request_ended");
    assert_eq!(
        provider.games[&42].artwork_revision.as_deref(),
        Some(remote.as_str())
    );
    assert!(provider.games[&42].artwork_is_remote);
}

#[test]
fn failed_remote_read_restores_local_artwork_with_a_new_revision() {
    let mut provider = Provider {
        enabled: true,
        revision: 1,
        snapshot_complete: true,
        ..Provider::default()
    };
    let mut game = upgrade_game(42, true);
    let remote = artwork::remote_revision(42, game.last_updated).unwrap();
    game.artwork_revision = Some(remote.clone());
    game.artwork_is_remote = true;
    provider.games.insert(42, game);
    provider
        .remote_artwork
        .start_with_fetcher(|_| Err("artwork_unavailable"))
        .unwrap();
    assert!(
        provider
            .read_artwork(7, 42, &remote, 0, 1000)
            .unwrap()
            .is_none()
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let responses = loop {
        let responses = provider.drain_remote_artwork();
        if !responses.is_empty() {
            break responses;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(responses[0].1["code"], "artwork_changed");
    assert_eq!(
        provider.games[&42].artwork_revision.as_deref(),
        Some("local-42")
    );
    assert!(!provider.games[&42].artwork_is_remote);
    assert_eq!(provider.revision, 2);
}

#[test]
fn product_settings_actions_require_the_management_principal() {
    assert!(is_management_caller("amaterasu.management"));
    assert!(!is_management_caller("amaterasu.console"));
    assert!(!is_management_caller("amaterasu.nvhttp"));
}

#[test]
fn artwork_reads_allow_library_and_require_a_current_discovery_record() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings(true, 1));
    host.queue_service_request(
        10,
        "amaterasu.console",
        json!({"action":"artwork_read","app_id":42,"revision":"1","offset":0}),
    );
    host.queue_service_request(
        11,
        MANAGEMENT_CALLER,
        json!({"action":"artwork_read","app_id":42,"revision":"1","offset":0}),
    );
    host.queue_service_request(
        12,
        LIBRARY_CALLER,
        json!({"action":"artwork_read","app_id":42,"revision":"1","offset":0}),
    );
    host.queue_service_request(13, LIBRARY_CALLER, json!({"action":"refresh"}));
    host.queue_service_request(14, LIBRARY_CALLER, json!({"action":"status"}));
    let mut module = start_test_module(&mut host);

    for _ in 0..5 {
        module.tick().unwrap();
    }

    assert_eq!(
        host.completed[0],
        (10, json!({"ok":false,"code":"forbidden"}))
    );
    assert_eq!(host.completed[1].0, 11);
    assert_eq!(host.completed[1].1["ok"], false);
    assert_eq!(host.completed[1].1["code"], "artwork_unavailable");
    assert_eq!(host.completed[2].1["code"], "artwork_unavailable");
    assert_eq!(host.completed[3].1["code"], "forbidden");
    assert_eq!(host.completed[4].1["code"], "forbidden");
}

#[test]
fn custom_library_principal_replaces_default_for_artwork() {
    let mut host = Box::new(FakeHost {
        startup_settings: Some(json!({"library_principal":"custom.library"})),
        ..FakeHost::default()
    });
    host.product_get_results
        .push_back(persisted_settings(true, 1));
    for (id, caller, action) in [
        (30, "custom.library", "artwork_read"),
        (31, "custom.library", "status"),
        (32, LIBRARY_CALLER, "artwork_read"),
        (33, LIBRARY_CALLER, "status"),
        (34, "amaterasu.nvhttp", "artwork_read"),
    ] {
        let payload = if action == "artwork_read" {
            json!({"action":action,"app_id":42,"revision":"1","offset":0})
        } else {
            json!({"action":action})
        };
        host.queue_service_request(id, caller, payload);
    }
    let mut module = start_test_module(&mut host);
    for _ in 0..5 {
        module.tick().unwrap();
    }
    assert_eq!(host.completed[0].1["code"], "artwork_unavailable");
    for (_, response) in &host.completed[1..] {
        assert_eq!(response["code"], "forbidden");
    }
}

#[test]
fn nvhttp_cannot_call_steam_directly_but_library_may_forward_artwork() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings(true, 1));
    for (id, payload) in [
        (20, json!({"action":"status"})),
        (21, json!({"action":"list"})),
        (22, json!({"action":"refresh"})),
        (
            23,
            json!({"action":"artwork_read","app_id":42,"revision":"1","offset":0}),
        ),
    ] {
        host.queue_service_request(id, "amaterasu.nvhttp", payload);
    }
    host.service_requests.push_back(Response::Request {
        id: 24,
        caller: LIBRARY_CALLER.into(),
        initiator: "amaterasu.nvhttp".into(),
        service: SERVICE.into(),
        payload: json!({"action":"artwork_read","app_id":42,"revision":"1","offset":0}),
        remaining_ms: CONFIGURATION_TIMEOUT_MS,
    });
    let mut module = start_test_module(&mut host);
    for _ in 0..5 {
        module.tick().unwrap();
    }
    for (_, response) in &host.completed[..4] {
        assert_eq!(response["code"], "forbidden");
    }
    assert_eq!(host.completed[4].1["code"], "artwork_unavailable");
}

#[test]
fn artwork_read_uses_only_the_current_app_revision_and_never_returns_a_path() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "amaterasu-steam-read-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join("appcache/librarycache")).unwrap();
    let image = root
        .join("appcache/librarycache")
        .join("42_library_600x900_2x.jpg");
    let mut image_bytes = vec![0; 24];
    image_bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    image_bytes[12..16].copy_from_slice(b"IHDR");
    image_bytes[16..20].copy_from_slice(&600_u32.to_be_bytes());
    image_bytes[20..24].copy_from_slice(&900_u32.to_be_bytes());
    std::fs::write(&image, &image_bytes).unwrap();
    let root = root.canonicalize().unwrap();
    let artwork = artwork::discover(42, &root, &[]);
    let revision = artwork.revision.clone().unwrap();
    let mut provider = Provider::new(vec![root.clone()]);
    provider.enabled = true;
    provider.revision = 1;
    provider.snapshot_complete = true;
    provider.games.insert(
        42,
        Game {
            stable_id: "steam:42".into(),
            app_id: 42,
            name: "Example".into(),
            install_directory: root.join("steamapps/common/example").display().to_string(),
            library_directory: root.display().to_string(),
            state_flags: 4,
            installed: true,
            last_updated: 0,
            app_type: Some("game".into()),
            last_played: None,
            playtime_minutes: None,
            artwork_revision: artwork.revision,
            artwork_source_path: artwork.source_path,
            artwork_is_remote: false,
            artwork_local_revision: None,
            artwork_needs_remote_upgrade: false,
            artwork_candidate_signature: String::new(),
        },
    );

    assert!(
        provider
            .read_artwork(11, 42, &revision, 0, 1_000)
            .unwrap()
            .is_none()
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let payload = loop {
        provider.process_local_artwork();
        if let Some((id, payload)) = provider.drain_local_artwork().pop() {
            assert_eq!(id, 11);
            break payload;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["app_id"], 42);
    assert_eq!(payload["revision"], revision);
    assert_eq!(payload["mime_type"], "image/png");
    assert_eq!(payload["total_bytes"], image_bytes.len() as u64);
    assert_eq!(payload["next_offset"], Value::Null);
    assert!(payload["data"].as_str().unwrap().starts_with("iVBORw0KGgo"));
    assert!(payload.get("path").is_none());
    assert_eq!(
        provider.read_artwork(11, 42, "1", 0, 1_000).unwrap_err(),
        "artwork_changed"
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_management_settings_updates_preserve_recent_and_tool_policy() {
    let current = ProductSettings {
        enabled: true,
        auto_sync: true,
        include_tools: true,
        sync_all_installed: false,
        recent_games: 6,
        recent_max_age_days: 14,
        autosync_remove_uninstalled: false,
        ..ProductSettings::default()
    };
    let Action::SettingsUpdate {
        enabled,
        auto_sync,
        exclude_games,
        include_tools,
        sync_all_installed,
        recent_games,
        recent_max_age_days,
        autosync_remove_uninstalled,
        ..
    } = serde_json::from_value(json!({
        "action":"settings_update",
        "enabled":false,
        "auto_sync":false,
        "exclude_games":[],
        "expected_revision":3
    }))
    .unwrap()
    else {
        panic!("expected settings update action");
    };
    let updated = ProductSettingsPatch {
        enabled,
        auto_sync,
        exclude_games,
        include_tools,
        sync_all_installed,
        recent_games,
        recent_max_age_days,
        autosync_remove_uninstalled,
    }
    .apply(&current);
    assert!(!updated.enabled);
    assert!(!updated.auto_sync);
    assert!(updated.include_tools);
    assert_eq!(updated.recent_games, 6);
    assert_eq!(updated.recent_max_age_days, 14);
    assert!(!updated.autosync_remove_uninstalled);
}

#[test]
fn disabled_steam_rejects_refresh_and_drops_the_prior_discovery_snapshot() {
    let mut provider = Provider {
        enabled: true,
        revision: 4,
        scan: Some(Scan::new(&[])),
        ..Provider::default()
    };
    provider.games.insert(
        42,
        Game {
            stable_id: "steam:42".into(),
            app_id: 42,
            name: "Example".into(),
            install_directory: "C:/Steam/steamapps/common/Example".into(),
            library_directory: "C:/Steam".into(),
            state_flags: 4,
            installed: true,
            last_updated: 0,
            app_type: None,
            last_played: None,
            playtime_minutes: None,
            artwork_revision: None,
            artwork_source_path: None,
            artwork_is_remote: false,
            artwork_local_revision: None,
            artwork_needs_remote_upgrade: false,
            artwork_candidate_signature: String::new(),
        },
    );

    provider.set_enabled(false);
    assert!(provider.scan.is_none());
    assert!(provider.games.is_empty());
    assert_eq!(provider.revision, 5);
    assert_eq!(provider.status()["enabled"], false);

    let ActionResult::Immediate(refresh) = provider.handle_action(Action::Refresh {}) else {
        panic!("refresh should return an immediate disabled error");
    };
    assert_eq!(refresh["code"], "steam_disabled");
    let ActionResult::Immediate(list) = provider.handle_action(Action::List {
        revision: None,
        after: None,
        limit: None,
    }) else {
        panic!("list should return the empty disabled snapshot");
    };
    assert_eq!(list["enabled"], false);
    assert_eq!(list["games"], json!([]));
}

#[test]
fn enabling_steam_allows_explicit_refresh_of_configured_roots() {
    let mut provider = Provider::default();
    let ActionResult::Immediate(disabled) = provider.handle_action(Action::Refresh {}) else {
        panic!("disabled refresh should fail synchronously");
    };
    assert_eq!(disabled["code"], "steam_disabled");

    provider.set_enabled(true);
    let ActionResult::Immediate(refresh) = provider.handle_action(Action::Refresh {}) else {
        panic!("enabled refresh should be accepted");
    };
    assert_eq!(refresh["enabled"], true);
    assert_eq!(refresh["scanning"], true);

    provider.tick();
    assert_eq!(provider.status()["scanning"], false);
    assert_eq!(provider.status()["revision"], 1);
}

#[test]
fn product_settings_expose_bounded_tool_and_recent_game_policy() {
    let defaults = ProductSettings::default();
    assert!(!defaults.include_tools);
    assert!(!defaults.sync_all_installed);
    assert_eq!(defaults.recent_games, 10);
    assert_eq!(defaults.recent_max_age_days, 30);
    assert!(
        ProductSettings {
            recent_games: 10_001,
            ..defaults.clone()
        }
        .validate()
        .is_err()
    );
    assert!(
        ProductSettings {
            recent_max_age_days: 36_501,
            ..defaults
        }
        .validate()
        .is_err()
    );
}

#[derive(Default)]
struct FakeHost {
    network_bindings: BTreeMap<u64, usize>,
    deny_network: bool,
    next_child: u64,
    startup_settings: Option<Value>,
    product_get_results: VecDeque<Response>,
    product_update_results: VecDeque<Response>,
    child_results: BTreeMap<u64, Response>,
    service_requests: VecDeque<Response>,
    completed: Vec<(u64, Value)>,
    released: Vec<u64>,
    configuration_actions: Vec<String>,
    catalog_requests: Vec<Value>,
    catalog_revision: u64,
    catalog_generation: u64,
    catalog_next_sequence: u64,
    catalog_finish_failures_remaining: u8,
    catalog_hold_next_request: bool,
    catalog_cancel_failures_remaining: u8,
    catalog_release_failures_remaining: u8,
    canceled: Vec<u64>,
}

impl FakeHost {
    fn queue_service_request(&mut self, id: u64, caller: &str, payload: Value) {
        self.service_requests.push_back(Response::Request {
            id,
            caller: caller.into(),
            initiator: caller.into(),
            service: SERVICE.into(),
            payload,
            remaining_ms: CONFIGURATION_TIMEOUT_MS,
        });
    }

    fn dispatch(&mut self, command: Command) -> Response {
        match command {
            Command::GetSettings {} => Response::Result {
                payload: self
                    .startup_settings
                    .clone()
                    .unwrap_or_else(|| json!({"roots":[]})),
            },
            Command::Register { .. } => Response::Ok,
            Command::NativeBindTyped {
                service,
                interface_major,
                min_minor,
                min_size,
                contract,
            } => {
                if contract != "amaterasu.network-interface/1" {
                    return Response::error("unsupported_contract", "unknown network contract");
                }
                self.dispatch(Command::NativeBind {
                    service,
                    interface_major,
                    min_minor,
                    min_size,
                })
            }
            Command::NativeBind {
                service,
                interface_major,
                min_minor,
                min_size,
            } => {
                if self.deny_network || service != "amaterasu.network/1" {
                    return Response::error(
                        "permission_denied",
                        "test network provider unavailable",
                    );
                }
                let factory = amaterasu_network_sockets::Factory::new();
                let query = amaterasu_module_sdk::abi::NativeInterfaceQuery {
                    header: AbiHeader::of::<amaterasu_module_sdk::abi::NativeInterfaceQuery>(),
                    interface_major,
                    min_minor,
                    min_size,
                };
                let mut output = std::ptr::null();
                // SAFETY: explicitly injected test provider owns each returned
                // binding until the fake runtime's NativeRelease handler.
                let status = unsafe {
                    (factory.api.bind.unwrap())(
                        factory.api.context,
                        BorrowedBytes::new(b"amaterasu.steam"),
                        &query,
                        &mut output,
                    )
                };
                if status != STATUS_OK {
                    return Response::error("provider_rejected", "test network binding rejected");
                }
                let binding = self.network_bindings.len() as u64 + 1;
                self.network_bindings.insert(binding, output as usize);
                Response::NativeBound {
                    binding,
                    address: output as usize,
                }
            }
            Command::NativeRelease { binding } => {
                let Some(address) = self.network_bindings.remove(&binding) else {
                    return Response::error("not_found", "test network binding released");
                };
                // SAFETY: each factory output is live and uniquely owned until
                // this release, after the module joined its artwork worker.
                unsafe {
                    let base = *(address as *const amaterasu_module_sdk::abi::NativeInterface);
                    (base.release.unwrap())(base.context);
                }
                Response::Ok
            }
            Command::Request {
                service, payload, ..
            } => {
                self.next_child += 1;
                let request = self.next_child;
                let action = payload["action"].as_str().unwrap_or_default();
                let result = if service == "amaterasu.configuration/1" {
                    self.configuration_actions.push(action.to_owned());
                    match action {
                        "product_get" => self
                            .product_get_results
                            .pop_front()
                            .unwrap_or(Response::Pending),
                        "product_update" => self
                            .product_update_results
                            .pop_front()
                            .unwrap_or(Response::Pending),
                        _ => Response::error("invalid_request", "unexpected configuration action"),
                    }
                } else if service == "amaterasu.catalog/1" {
                    self.catalog_requests.push(payload.clone());
                    if self.catalog_hold_next_request {
                        self.catalog_hold_next_request = false;
                        self.child_results.insert(request, Response::Pending);
                        return Response::Accepted { id: request };
                    }
                    let result = match action {
                        "provider_snapshot_begin" => {
                            self.catalog_generation += 1;
                            self.catalog_next_sequence = 0;
                            json!({
                                "ok":true,
                                "revision":self.catalog_revision,
                                "generation":self.catalog_generation,
                                "next_sequence":0,"staged":true
                            })
                        }
                        "provider_snapshot_batch" => {
                            self.catalog_revision += 1;
                            self.catalog_next_sequence += 1;
                            json!({
                                "ok":true,
                                "revision":self.catalog_revision,
                                "generation":self.catalog_generation,
                                "next_sequence":self.catalog_next_sequence
                            })
                        }
                        "provider_snapshot_finish"
                            if self.catalog_finish_failures_remaining > 0 =>
                        {
                            self.catalog_finish_failures_remaining -= 1;
                            json!({
                                "ok":false,
                                "error":{"code":"catalog_unavailable","message":"temporary test failure"}
                            })
                        }
                        "provider_snapshot_finish" => json!({
                            "ok":true,
                            "revision":self.catalog_revision,
                            "generation":self.catalog_generation,
                            "complete":true,
                            "cleanup_pending":false,
                            "remove_stale":payload["remove_stale"]
                        }),
                        _ => {
                            json!({"ok":false,"error":{"code":"invalid_request","message":"unexpected action"}})
                        }
                    };
                    Response::Result { payload: result }
                } else {
                    Response::error("service_unavailable", "unexpected target service")
                };
                self.child_results.insert(request, result);
                Response::Accepted { id: request }
            }
            Command::Poll {} => self
                .service_requests
                .pop_front()
                .unwrap_or(Response::Pending),
            Command::TakeResult { request } => self
                .child_results
                .get(&request)
                .cloned()
                .unwrap_or_else(|| Response::error("not_found", "result was released")),
            Command::ReleaseResult { request } => {
                if self.catalog_release_failures_remaining > 0 {
                    self.catalog_release_failures_remaining -= 1;
                    return Response::error("unavailable", "release failed in fake host");
                }
                if matches!(self.child_results.get(&request), Some(Response::Pending)) {
                    return Response::error("request_pending", "request is still active");
                }
                if self.child_results.remove(&request).is_some() {
                    self.released.push(request);
                    Response::Ok
                } else {
                    Response::error("not_found", "result was already released")
                }
            }
            Command::Cancel { request } => {
                if self.catalog_cancel_failures_remaining > 0 {
                    self.catalog_cancel_failures_remaining -= 1;
                    return Response::error("unavailable", "cancel failed in fake host");
                }
                if let Some(result) = self.child_results.get_mut(&request) {
                    self.canceled.push(request);
                    if matches!(result, Response::Pending) {
                        *result = Response::error("cancelled", "request cancelled");
                    }
                    Response::Ok
                } else {
                    Response::error("not_found", "request was already released")
                }
            }
            Command::Complete { request, payload } => {
                self.completed.push((request, payload));
                Response::Ok
            }
            _ => Response::error("unexpected", "test host received an unsupported command"),
        }
    }
}

unsafe extern "C" fn invoke_fake_host(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> amaterasu_module_sdk::abi::Status {
    // SAFETY: the test owns the boxed host for the full SteamModule lifetime;
    // the ABI input and reply buffers are borrowed only for this callback.
    let Ok(input) = (unsafe { input.read() }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(input) else {
        return STATUS_INVALID_ARGUMENT;
    };
    // SAFETY: context points to the live `FakeHost` box installed by the test.
    let response = unsafe { &mut *(context as *mut FakeHost) }.dispatch(command);
    let Ok(output) = serde_json::to_vec(&response) else {
        return STATUS_INTERNAL_ERROR;
    };
    // SAFETY: the output vector remains alive until the reply callback copies it.
    unsafe { reply.send(&output) };
    STATUS_OK
}

fn start_test_module(host: &mut Box<FakeHost>) -> SteamModule {
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut **host as *mut FakeHost).cast(),
        invoke: invoke_fake_host,
    };
    // SAFETY: `host` remains boxed and alive for each call made by the module.
    let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
    let mut module = SteamModule::start(runtime).unwrap();
    // Most fake-host tests supply snapshots directly; keep them independent
    // of a Steam installation on the developer's Windows machine.
    module.provider.automatic_roots = false;
    module.provider.roots.clear();
    module.provider.steam_executable = None;
    module
}

fn persisted_settings(enabled: bool, revision: u64) -> Response {
    Response::Result {
        payload: json!({
            "ok":true,
            "found":true,
            "version":1,
            "revision":revision,
            "values":{"enabled":enabled}
        }),
    }
}

fn persisted_settings_with_sync_policy(enabled: bool, auto_sync: bool, revision: u64) -> Response {
    Response::Result {
        payload: json!({
            "ok":true,
            "found":true,
            "version":1,
            "revision":revision,
            "values":{"enabled":enabled,"auto_sync":auto_sync,"sync_all_installed":true}
        }),
    }
}

fn persisted_settings_with_auto_sync(exclusions: Value, revision: u64) -> Response {
    Response::Result {
        payload: json!({
            "ok":true,
            "found":true,
            "version":1,
            "revision":revision,
            "values":{"enabled":true,"auto_sync":true,"exclude_games":exclusions,"sync_all_installed":true}
        }),
    }
}

fn set_complete_test_snapshot(provider: &mut Provider, revision: u64, name: &str) {
    provider.scan = None;
    provider.steam_executable = Some(std::env::current_exe().unwrap());
    provider.games = [(
        42,
        Game {
            stable_id: "steam:42".into(),
            app_id: 42,
            name: name.into(),
            install_directory: "/Steam/steamapps/common/Game".into(),
            library_directory: "/Steam".into(),
            state_flags: 4,
            installed: true,
            last_updated: 0,
            app_type: Some("game".into()),
            last_played: None,
            playtime_minutes: None,
            artwork_revision: Some("123".into()),
            artwork_source_path: Some("/Steam/art/42.jpg".into()),
            artwork_is_remote: false,
            artwork_local_revision: None,
            artwork_needs_remote_upgrade: false,
            artwork_candidate_signature: String::new(),
        },
    )]
    .into_iter()
    .collect();
    provider.revision = revision;
    provider.snapshot_complete = true;
    provider.appinfo_complete = true;
    provider.history_complete = true;
}

#[test]
fn force_sync_is_management_only_and_follows_host_enablement_policy() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings(false, 1));
    let mut module = start_test_module(&mut host);

    module
        .handle_request(
            10,
            "amaterasu.console",
            json!({"action":"force_sync"}),
            CONFIGURATION_TIMEOUT_MS,
        )
        .unwrap();
    assert_eq!(
        host.completed[0],
        (10, json!({"ok":false,"code":"forbidden"}))
    );
    assert!(module.provider.scan.is_none());

    module
        .handle_request(
            11,
            MANAGEMENT_CALLER,
            json!({"action":"force_sync"}),
            CONFIGURATION_TIMEOUT_MS,
        )
        .unwrap();
    assert_eq!(
        host.completed[1],
        (11, json!({"ok":false,"code":"settings_loading"}))
    );

    module.tick().unwrap();
    module.tick().unwrap();
    module
        .handle_request(
            12,
            MANAGEMENT_CALLER,
            json!({"action":"force_sync"}),
            CONFIGURATION_TIMEOUT_MS,
        )
        .unwrap();
    #[cfg(not(target_os = "linux"))]
    {
        assert_eq!(host.completed[2].1["code"], "steam_disabled");
        assert!(module.provider.scan.is_none());
    }
    #[cfg(target_os = "linux")]
    {
        // Linux intentionally keeps Steam enabled when loading a disabled document.
        assert!(module.configuration.settings().enabled);
        assert!(module.provider.enabled);
        assert_eq!(host.completed[2].1["accepted"], true);
        assert_eq!(host.completed[2].1["force_sync_pending"], true);
        assert!(module.provider.scan.is_some());
    }
}

#[test]
fn force_sync_rejects_an_exhausted_discovery_revision() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings_with_sync_policy(true, false, 1));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();
    // Model an exhausted, idle provider after the startup scan has retired.
    module.provider.scan = None;
    module.provider.revision = u64::MAX;

    module
        .handle_request(
            13,
            MANAGEMENT_CALLER,
            json!({"action":"force_sync"}),
            CONFIGURATION_TIMEOUT_MS,
        )
        .unwrap();

    assert_eq!(
        host.completed.last().unwrap().1,
        json!({"ok":false,"code":"revision_exhausted"})
    );
    assert!(module.provider.scan.is_none());
    assert_eq!(module.catalog_sync.status()["force_sync_pending"], false);
}

#[test]
fn force_sync_coalesces_duplicates_and_reconciles_a_new_snapshot_with_auto_sync_off() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings_with_sync_policy(true, false, 1));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();
    set_complete_test_snapshot(&mut module.provider, 7, "Before force sync");
    module.tick().unwrap();

    module
        .handle_request(
            20,
            MANAGEMENT_CALLER,
            json!({"action":"force_sync"}),
            CONFIGURATION_TIMEOUT_MS,
        )
        .unwrap();
    assert_eq!(host.completed.last().unwrap().1["accepted"], true);
    assert_eq!(host.completed.last().unwrap().1["force_sync_pending"], true);
    assert_eq!(module.provider.status()["scanning"], true);

    module
        .handle_request(
            21,
            MANAGEMENT_CALLER,
            json!({"action":"force_sync"}),
            CONFIGURATION_TIMEOUT_MS,
        )
        .unwrap();
    assert_eq!(host.completed.last().unwrap().1["accepted"], true);
    assert_eq!(module.provider.status()["scanning"], true);

    module
        .handle_request(
            22,
            MANAGEMENT_CALLER,
            json!({"action":"status"}),
            CONFIGURATION_TIMEOUT_MS,
        )
        .unwrap();
    assert_eq!(host.completed.last().unwrap().1["force_sync_pending"], true);
    module
        .handle_request(
            23,
            MANAGEMENT_CALLER,
            json!({"action":"list","limit":16}),
            CONFIGURATION_TIMEOUT_MS,
        )
        .unwrap();
    assert_eq!(host.completed.last().unwrap().1["force_sync_pending"], true);

    set_complete_test_snapshot(&mut module.provider, 8, "After force sync");
    for _ in 0..8 {
        module.tick().unwrap();
    }

    assert_eq!(
        host.catalog_requests
            .iter()
            .map(|request| request["action"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "provider_snapshot_begin",
            "provider_snapshot_batch",
            "provider_snapshot_finish"
        ]
    );
    assert!(
        host.catalog_requests
            .iter()
            .all(|request| request["source_revision"] == 8)
    );
    assert_eq!(module.catalog_sync.status()["force_sync_pending"], false);

    host.queue_service_request(24, "amaterasu.console", json!({"action":"status"}));
    module.tick().unwrap();
    let status = host
        .completed
        .iter()
        .find(|(id, _)| *id == 24)
        .unwrap()
        .1
        .clone();
    assert_eq!(status["complete"], true);
    assert_eq!(status["force_sync_pending"], false);
    assert_eq!(status["catalog_sync"]["force_sync_pending"], false);
    assert_eq!(status["catalog_sync"]["last_error"], Value::Null);
}

#[test]
fn incomplete_force_sync_discovery_clears_the_request_without_catalog_writes() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings_with_sync_policy(true, false, 1));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();

    module
        .handle_request(
            30,
            MANAGEMENT_CALLER,
            json!({"action":"force_sync"}),
            CONFIGURATION_TIMEOUT_MS,
        )
        .unwrap();
    assert_eq!(module.catalog_sync.status()["force_sync_pending"], true);

    module.tick().unwrap();
    assert_eq!(module.provider.status()["complete"], false);
    assert_eq!(module.catalog_sync.status()["force_sync_pending"], false);
    assert!(host.catalog_requests.is_empty());

    host.queue_service_request(31, "amaterasu.console", json!({"action":"status"}));
    module.tick().unwrap();
    let status = host.completed.last().unwrap().1.clone();
    assert_eq!(host.completed.last().unwrap().0, 31);
    assert_eq!(status["complete"], false);
    assert_eq!(status["force_sync_pending"], false);
    assert_eq!(status["catalog_sync"]["force_sync_pending"], false);
    assert!(
        status["catalog_sync"]["last_error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())
    );
}

#[test]
fn force_sync_catalog_reconciliation_failure_clears_pending_status_with_error() {
    let mut host = Box::new(FakeHost::default());
    host.catalog_finish_failures_remaining = 1;
    host.product_get_results
        .push_back(persisted_settings_with_sync_policy(true, false, 1));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();
    set_complete_test_snapshot(&mut module.provider, 7, "Before force sync");
    module.tick().unwrap();

    module
        .handle_request(
            50,
            MANAGEMENT_CALLER,
            json!({"action":"force_sync"}),
            CONFIGURATION_TIMEOUT_MS,
        )
        .unwrap();
    assert_eq!(host.completed.last().unwrap().1["accepted"], true);
    set_complete_test_snapshot(&mut module.provider, 8, "Failed force sync");
    for _ in 0..8 {
        module.tick().unwrap();
    }

    assert_eq!(host.catalog_requests.len(), 3);
    host.queue_service_request(51, "amaterasu.console", json!({"action":"status"}));
    module.tick().unwrap();
    let status = host.completed.last().unwrap().1.clone();
    assert_eq!(host.completed.last().unwrap().0, 51);
    assert_eq!(status["force_sync_pending"], false);
    assert_eq!(status["catalog_sync"]["force_sync_pending"], false);
    assert_eq!(status["catalog_sync"]["syncing"], false);
    assert!(
        status["catalog_sync"]["last_error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())
    );
}

#[test]
fn unusable_force_sync_snapshot_clears_the_request_without_catalog_writes() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings_with_sync_policy(true, false, 1));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();

    module
        .handle_request(
            40,
            MANAGEMENT_CALLER,
            json!({"action":"force_sync"}),
            CONFIGURATION_TIMEOUT_MS,
        )
        .unwrap();
    set_complete_test_snapshot(&mut module.provider, 1, "Missing client");
    module.provider.steam_executable = None;
    module.tick().unwrap();

    assert_eq!(module.catalog_sync.status()["force_sync_pending"], false);
    assert!(
        module.catalog_sync.status()["last_error"]
            .as_str()
            .unwrap()
            .contains("executable")
    );
    assert!(host.catalog_requests.is_empty());
}

#[test]
fn ordinary_refresh_remains_discovery_only_when_auto_sync_is_disabled() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings_with_sync_policy(true, false, 1));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();

    module
        .handle_request(
            50,
            "amaterasu.console",
            json!({"action":"refresh"}),
            CONFIGURATION_TIMEOUT_MS,
        )
        .unwrap();
    assert_eq!(host.completed.last().unwrap().1["scanning"], true);
    assert_eq!(
        host.completed.last().unwrap().1["force_sync_pending"],
        false
    );

    set_complete_test_snapshot(&mut module.provider, 1, "Refresh only");
    for _ in 0..4 {
        module.tick().unwrap();
    }
    assert!(host.catalog_requests.is_empty());
}

#[test]
fn terminal_results_are_released_and_lost_release_ack_retries_safely() {
    let mut released = Vec::new();
    let result = configuration::release_terminal_child_result(
        44,
        Response::Result {
            payload: json!({"ok":true}),
        },
        |request| {
            released.push(request);
            Ok(Response::Ok)
        },
    )
    .unwrap();
    assert!(matches!(result, Response::Result { .. }));
    assert_eq!(released, vec![44]);

    assert!(
        configuration::release_terminal_child_result(
            45,
            Response::Result {
                payload: json!({"ok":true}),
            },
            |_| Err("release acknowledgement was lost".into()),
        )
        .is_none()
    );
    let expired = configuration::release_terminal_child_result(
        45,
        Response::error(
            "not_found",
            "result was released after lost acknowledgement",
        ),
        |_| panic!("already released result must not be released again"),
    )
    .unwrap();
    assert!(matches!(expired, Response::Error { ref code, .. } if code == "not_found"));
}

#[test]
fn status_and_list_are_served_while_startup_settings_are_pending() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results.push_back(Response::Pending);
    host.queue_service_request(10, "amaterasu.console", json!({"action":"status"}));
    host.queue_service_request(11, "amaterasu.console", json!({"action":"list"}));
    let mut module = start_test_module(&mut host);

    module.tick().unwrap();
    module.tick().unwrap();

    assert_eq!(host.completed.len(), 2);
    assert_eq!(host.completed[0].0, 10);
    assert_eq!(host.completed[0].1["settings_ready"], false);
    assert_eq!(host.completed[1].0, 11);
    assert_eq!(host.completed[1].1["settings_ready"], false);
    assert_eq!(host.completed[1].1["games"], json!([]));
}

#[test]
fn cancelled_update_releases_results_reads_back_state_and_never_claims_success() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings(true, 3));
    // Simulate the update having committed but its state result acknowledgement
    // being lost before Steam can consume it.
    host.product_update_results
        .push_back(Response::error("deadline_exceeded", "acknowledgement lost"));
    host.product_get_results.push_back(Response::error(
        "unavailable",
        "the first authoritative readback failed",
    ));
    host.product_get_results
        .push_back(persisted_settings(false, 4));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();
    assert_eq!(module.configuration.revision(), 3);
    assert!(module.provider.enabled);

    host.queue_service_request(
        20,
        MANAGEMENT_CALLER,
        json!({"action":"settings_update","enabled":false,"expected_revision":3}),
    );
    module.tick().unwrap(); // accept the management update
    module.tick().unwrap(); // submit the product update

    host.queue_service_request(21, "amaterasu.console", json!({"action":"status"}));
    host.queue_service_request(22, "amaterasu.console", json!({"action":"refresh"}));
    host.queue_service_request(23, "amaterasu.console", json!({"action":"list"}));
    host.queue_service_request(24, "amaterasu.console", json!({"action":"status"}));
    module.tick().unwrap(); // consume the ambiguous update and begin readback
    module.tick().unwrap(); // submit readback and serve status
    module.tick().unwrap(); // fail readback and serve refresh
    assert_eq!(host.configuration_actions.len(), 3);
    module.tick().unwrap(); // backoff tick still serves list
    assert_eq!(host.configuration_actions.len(), 3);
    module.tick().unwrap(); // retry readback while serving status
    module.tick().unwrap(); // apply retried authoritative settings

    let update = host
        .completed
        .iter()
        .find(|(id, _)| *id == 20)
        .expect("management update is completed");
    assert_eq!(update.1["ok"], false);
    assert_eq!(update.1["error"]["code"], "settings_outcome_unknown");
    assert_eq!(update.1["error"]["current_revision"], 3);
    assert_eq!(
        host.completed.iter().find(|(id, _)| *id == 21).unwrap().1["enabled"],
        true
    );
    let refresh = &host.completed.iter().find(|(id, _)| *id == 22).unwrap().1;
    assert_eq!(refresh["enabled"], true);
    assert_eq!(refresh["scanning"], true);
    let list = &host.completed.iter().find(|(id, _)| *id == 23).unwrap().1;
    assert_eq!(list["enabled"], true);
    assert_eq!(list["settings_ready"], true);
    let retried_status = &host.completed.iter().find(|(id, _)| *id == 24).unwrap().1;
    assert_eq!(retried_status["enabled"], true);
    assert_eq!(host.configuration_actions.len(), 4);
    assert_eq!(host.released.len(), 4);
    assert_eq!(module.provider.enabled, cfg!(target_os = "linux"));
    assert_eq!(module.configuration.revision(), 4);
}

#[test]
fn automatic_sync_imports_only_included_installed_games_with_a_complete_snapshot() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings_with_auto_sync(
            json!([{"id":"20","name":"Excluded game"}]),
            2,
        ));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();

    module.provider.scan = None;
    module.provider.steam_executable = Some(std::env::current_exe().unwrap());
    module.provider.games.insert(
        10,
        Game {
            stable_id: "steam:10".into(),
            app_id: 10,
            name: "Included game".into(),
            install_directory: "/Steam/steamapps/common/Included".into(),
            library_directory: "/Steam".into(),
            state_flags: 4,
            installed: true,
            last_updated: 0,
            app_type: Some("game".into()),
            last_played: None,
            playtime_minutes: None,
            artwork_revision: None,
            artwork_source_path: None,
            artwork_is_remote: false,
            artwork_local_revision: None,
            artwork_needs_remote_upgrade: false,
            artwork_candidate_signature: String::new(),
        },
    );
    module.provider.games.insert(
        20,
        Game {
            stable_id: "steam:20".into(),
            app_id: 20,
            name: "Excluded game".into(),
            install_directory: "/Steam/steamapps/common/Excluded".into(),
            library_directory: "/Steam".into(),
            state_flags: 4,
            installed: true,
            last_updated: 0,
            app_type: Some("game".into()),
            last_played: None,
            playtime_minutes: None,
            artwork_revision: None,
            artwork_source_path: None,
            artwork_is_remote: false,
            artwork_local_revision: None,
            artwork_needs_remote_upgrade: false,
            artwork_candidate_signature: String::new(),
        },
    );
    module.provider.revision = 1;
    module.provider.snapshot_complete = true;
    module.provider.appinfo_complete = true;
    module.provider.history_complete = true;

    for _ in 0..8 {
        module.tick().unwrap();
    }

    assert_eq!(
        host.catalog_requests
            .iter()
            .map(|request| request["action"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "provider_snapshot_begin",
            "provider_snapshot_batch",
            "provider_snapshot_finish"
        ]
    );
    let batch = &host.catalog_requests[1];
    assert_eq!(batch["source_revision"], 1);
    assert_eq!(batch["sequence"], 0);
    assert_eq!(batch["entries"].as_array().unwrap().len(), 1);
    assert_eq!(batch["entries"][0]["source_id"], "steam:10");
    assert_eq!(
        batch["entries"][0]["entry"]["launch"]["arguments"],
        json!(["-applaunch", "10"])
    );
    assert_eq!(host.catalog_requests[2]["complete"], true);
    assert_eq!(host.released.len(), 4);
}

#[test]
fn automatic_sync_skips_an_identical_new_revision_and_imports_changed_output() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings_with_auto_sync(json!([]), 2));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();
    let settings = module.configuration.settings().clone();
    set_complete_test_snapshot(&mut module.provider, 1, "Original name");
    let now = Instant::now();

    for _ in 0..8 {
        module
            .catalog_sync
            .tick_at(&module.runtime, &mut module.provider, &settings, now)
            .unwrap();
    }
    assert_eq!(host.catalog_requests.len(), 3);
    assert_eq!(
        host.catalog_requests[2]["action"],
        "provider_snapshot_finish"
    );

    module.provider.revision = 2;
    module
        .catalog_sync
        .tick_at(
            &module.runtime,
            &mut module.provider,
            &settings,
            now + Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(host.catalog_requests.len(), 3);

    module.provider.games.get_mut(&42).unwrap().name = "Changed name".into();
    module.provider.revision = 3;
    module
        .catalog_sync
        .tick_at(
            &module.runtime,
            &mut module.provider,
            &settings,
            now + Duration::from_secs(2),
        )
        .unwrap();
    assert_eq!(host.catalog_requests.len(), 4);
    assert_eq!(
        host.catalog_requests[3]["action"],
        "provider_snapshot_begin"
    );
}

#[test]
fn interrupted_scan_settles_pending_catalog_child_before_current_reconciliation() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings_with_auto_sync(json!([]), 2));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();
    let settings = module.configuration.settings().clone();
    set_complete_test_snapshot(&mut module.provider, 1, "Old membership");
    host.catalog_hold_next_request = true;
    let now = Instant::now();

    for _ in 0..3 {
        module
            .catalog_sync
            .tick_at(&module.runtime, &mut module.provider, &settings, now)
            .unwrap();
        if host.catalog_requests.len() == 1 {
            break;
        }
    }
    assert_eq!(
        host.catalog_requests[0]["action"],
        "provider_snapshot_begin"
    );
    let old_request = host.next_child;
    assert_eq!(module.catalog_sync.status()["syncing"], true);

    module.provider.scan = Some(Scan::new(&[]));
    host.catalog_cancel_failures_remaining = 1;
    host.catalog_release_failures_remaining = 2;
    module
        .catalog_sync
        .tick_at(&module.runtime, &mut module.provider, &settings, now)
        .unwrap();
    assert!(host.canceled.is_empty());
    assert_eq!(module.catalog_sync.status()["syncing"], true);
    assert!(matches!(
        host.child_results.get(&old_request),
        Some(Response::Pending)
    ));
    module
        .catalog_sync
        .tick_at(&module.runtime, &mut module.provider, &settings, now)
        .unwrap();
    assert_eq!(host.canceled, vec![old_request]);
    assert_eq!(module.catalog_sync.status()["syncing"], true);
    module
        .catalog_sync
        .tick_at(&module.runtime, &mut module.provider, &settings, now)
        .unwrap();
    assert!(!module.catalog_sync.status()["syncing"].as_bool().unwrap());
    assert!(host.released.contains(&old_request));
    assert_eq!(host.catalog_requests.len(), 1);

    set_complete_test_snapshot(&mut module.provider, 2, "Current membership");
    for _ in 0..8 {
        module
            .catalog_sync
            .tick_at(&module.runtime, &mut module.provider, &settings, now)
            .unwrap();
    }
    assert_eq!(
        host.catalog_requests[1]["action"],
        "provider_snapshot_begin"
    );
    assert_eq!(
        host.catalog_requests[2]["entries"][0]["entry"]["name"],
        "Current membership"
    );
    assert_eq!(
        host.catalog_requests[3]["action"],
        "provider_snapshot_finish"
    );
}

#[test]
fn failed_automatic_finish_retries_after_the_interval_without_caching_source() {
    let mut host = Box::new(FakeHost::default());
    host.catalog_finish_failures_remaining = 1;
    host.product_get_results
        .push_back(persisted_settings_with_auto_sync(json!([]), 2));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();
    let settings = module.configuration.settings().clone();
    set_complete_test_snapshot(&mut module.provider, 1, "Retry game");
    let attempt_at = Instant::now();

    for _ in 0..8 {
        module
            .catalog_sync
            .tick_at(&module.runtime, &mut module.provider, &settings, attempt_at)
            .unwrap();
    }
    assert_eq!(host.catalog_requests.len(), 3);
    assert_eq!(module.catalog_sync.status()["syncing"], false);
    assert!(module.catalog_sync.status()["last_error"].is_string());

    module
        .catalog_sync
        .tick_at(
            &module.runtime,
            &mut module.provider,
            &settings,
            attempt_at + Duration::from_secs(29),
        )
        .unwrap();
    assert_eq!(host.catalog_requests.len(), 3);

    module
        .catalog_sync
        .tick_at(
            &module.runtime,
            &mut module.provider,
            &settings,
            attempt_at + Duration::from_secs(30),
        )
        .unwrap();
    assert!(module.provider.scan_in_progress());
    module.provider.scan = None;
    module.provider.revision = 2;
    module
        .catalog_sync
        .tick_at(
            &module.runtime,
            &mut module.provider,
            &settings,
            attempt_at + Duration::from_secs(31),
        )
        .unwrap();
    assert_eq!(host.catalog_requests.len(), 4);
    assert_eq!(
        host.catalog_requests[3]["action"],
        "provider_snapshot_begin"
    );
}

#[test]
fn catalog_cleanup_waits_for_appinfo_when_tool_filtering_is_active() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results.push_back(Response::Result {
        payload: json!({
            "ok":true,
            "found":true,
            "version":1,
            "revision":2,
            "values":{"enabled":true,"auto_sync":true,"sync_all_installed":true}
        }),
    });
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();

    module.provider.scan = None;
    module.provider.steam_executable = Some(std::env::current_exe().unwrap());
    module.provider.games.insert(
        10,
        Game {
            stable_id: "steam:10".into(),
            app_id: 10,
            name: "Installed game".into(),
            install_directory: "/Steam/steamapps/common/Game".into(),
            library_directory: "/Steam".into(),
            state_flags: 4,
            installed: true,
            last_updated: 0,
            app_type: Some("game".into()),
            last_played: None,
            playtime_minutes: None,
            artwork_revision: None,
            artwork_source_path: None,
            artwork_is_remote: false,
            artwork_local_revision: None,
            artwork_needs_remote_upgrade: false,
            artwork_candidate_signature: String::new(),
        },
    );
    module.provider.revision = 1;
    module.provider.snapshot_complete = true;
    module.tick().unwrap();
    assert!(host.catalog_requests.is_empty());
    assert!(
        module.catalog_sync.status()["last_error"]
            .as_str()
            .unwrap()
            .contains("appinfo")
    );

    module.provider.appinfo_complete = true;
    module.provider.revision = 2;
    module.tick().unwrap();
    assert_eq!(
        host.catalog_requests[0]["action"],
        "provider_snapshot_begin"
    );
}

#[test]
fn a_settings_change_stops_before_the_next_catalog_snapshot_step() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings_with_auto_sync(json!([]), 2));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();

    module.provider.scan = None;
    module.provider.steam_executable = Some(std::env::current_exe().unwrap());
    module.provider.games.insert(
        10,
        Game {
            stable_id: "steam:10".into(),
            app_id: 10,
            name: "Included game".into(),
            install_directory: "/Steam/steamapps/common/Included".into(),
            library_directory: "/Steam".into(),
            state_flags: 4,
            installed: true,
            last_updated: 0,
            app_type: Some("game".into()),
            last_played: None,
            playtime_minutes: None,
            artwork_revision: None,
            artwork_source_path: None,
            artwork_is_remote: false,
            artwork_local_revision: None,
            artwork_needs_remote_upgrade: false,
            artwork_candidate_signature: String::new(),
        },
    );
    module.provider.revision = 1;
    module.provider.snapshot_complete = true;
    module.provider.appinfo_complete = true;
    module.provider.history_complete = true;

    module.tick().unwrap();
    assert_eq!(host.catalog_requests.len(), 1);
    assert_eq!(
        host.catalog_requests[0]["action"],
        "provider_snapshot_begin"
    );

    let changed_settings = ProductSettings {
        enabled: true,
        auto_sync: true,
        exclude_games: vec![Exclusion {
            id: Some("20".into()),
            name: None,
        }],
        ..ProductSettings::default()
    };
    module.provider.revision = 2;
    module
        .catalog_sync
        .tick(&module.runtime, &mut module.provider, &changed_settings)
        .unwrap();

    assert_eq!(host.catalog_requests.len(), 1);
    assert_eq!(module.catalog_sync.status()["syncing"], false);
}

#[test]
fn steam_startup_rejects_a_missing_network_provider_instead_of_falling_back() {
    let mut host = Box::new(FakeHost {
        deny_network: true,
        ..Default::default()
    });
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut *host as *mut FakeHost).cast(),
        invoke: invoke_fake_host,
    };
    // SAFETY: boxed host remains alive until the failed start returns.
    let result = SteamModule::start(unsafe { RuntimeClient::from_endpoint(endpoint) });
    assert!(result.is_err());
    assert!(host.network_bindings.is_empty());
}
