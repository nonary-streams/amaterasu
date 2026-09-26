use super::*;
use crate::Provider;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Libraries(PathBuf);

impl Libraries {
    fn new() -> Self {
        let unique = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "amaterasu-steam-{}-{time}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn library(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.join("steamapps/common")).unwrap();
        path.canonicalize().unwrap()
    }

    fn manifest(&self, library: &Path, id: u32, name: &str, flags: u64) {
        fs::create_dir_all(library.join("steamapps/common").join(format!("game-{id}"))).unwrap();
        let document = format!(
            "\"AppState\" {{ \"appid\" \"{id}\" \"name\" \"{name}\" \"installdir\" \"game-{id}\" \"StateFlags\" \"{flags}\" }}"
        );
        fs::write(
            library.join(format!("steamapps/appmanifest_{id}.acf")),
            document,
        )
        .unwrap();
    }
}

impl Drop for Libraries {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn no_configured_roots_are_incomplete_and_do_not_imply_uninstallations() {
    let mut scan = Scan::new(&[]);
    finish(&mut scan);
    assert_eq!(scan.warning_count, 1);
    assert_eq!(scan.warnings, vec!["no_library_roots"]);
    assert!(!scan.finished || scan.warning_count != 0);
}

#[test]
fn steam_client_resolution_uses_the_configured_install_root_not_a_game_library() {
    let fixture = Libraries::new();
    let install = fixture.library("steam-install");
    let client = install.join("steam.exe");
    fs::write(&client, "fixture executable path").unwrap();

    assert_eq!(
        find_steam_executable(&[install.join("steamapps")]),
        Some(client.canonicalize().unwrap())
    );
}

#[test]
fn completed_refresh_re_resolves_a_moved_steam_client() {
    let fixture = Libraries::new();
    let first = fixture.library("first-install");
    let second = fixture.library("second-install");
    fixture.manifest(&first, 42, "Example", 4);
    let first_client = first.join("steam.exe");
    let second_client = second.join("steam.exe");
    fs::write(&first_client, "first client").unwrap();
    let mut provider = Provider {
        enabled: true,
        roots: vec![first.clone(), second.clone()],
        ..Provider::default()
    };

    assert!(provider.start_scan());
    for _ in 0..10_000 {
        provider.tick();
        if !provider.scan_in_progress() {
            break;
        }
    }
    assert_eq!(
        provider.steam_executable,
        Some(first_client.canonicalize().unwrap())
    );
    assert_eq!(provider.games[&42].name, "Example");

    fs::remove_file(&first_client).unwrap();
    fs::write(&second_client, "second client").unwrap();
    assert!(provider.start_scan());
    for _ in 0..10_000 {
        provider.tick();
        if !provider.scan_in_progress() {
            break;
        }
    }
    assert!(!provider.scan_in_progress());
    assert_eq!(
        provider.steam_executable,
        Some(second_client.canonicalize().unwrap())
    );
    assert_eq!(provider.games[&42].name, "Example");
}

#[cfg(windows)]
#[test]
fn windows_default_client_resolution_prefers_registered_custom_install() {
    let fixture = Libraries::new();
    let registered = fixture.library("custom-steam");
    let program_files = fixture.library("program-files");
    let registered_client = registered.join("steam.exe");
    let fallback_client = program_files.join("Steam/steam.exe");
    fs::write(&registered_client, "registered client").unwrap();
    fs::create_dir_all(fallback_client.parent().unwrap()).unwrap();
    fs::write(&fallback_client, "fallback client").unwrap();

    let candidates =
        windows_default_client_candidates(Some(registered), None, Some(program_files), None);
    assert_eq!(
        resolve_client_candidates(candidates.clone()),
        Some(registered_client.canonicalize().unwrap())
    );

    fs::remove_file(&registered_client).unwrap();
    assert_eq!(
        resolve_client_candidates(candidates),
        Some(fallback_client.canonicalize().unwrap())
    );
}

#[cfg(windows)]
#[test]
fn windows_default_roots_recover_after_steam_is_installed() {
    let fixture = Libraries::new();
    let candidate = fixture.0.join("late-steam");
    let mut provider = Provider {
        enabled: true,
        automatic_roots: true,
        ..Provider::default()
    };

    assert!(
        provider.start_scan_with_defaults(|| { existing_library_roots(vec![candidate.clone()]) })
    );
    for _ in 0..100 {
        provider.tick();
    }
    assert!(!provider.scan_in_progress());
    assert_eq!(provider.status()["complete"], false);
    assert_eq!(provider.status()["game_count"], 0);

    let installed_root = fixture.library("late-steam");
    fixture.manifest(&installed_root, 42, "Later installation", 4);
    assert!(provider.start_scan_with_defaults(|| {
        existing_library_roots(vec![candidate.clone(), candidate.clone()])
    }));
    for _ in 0..100 {
        provider.tick();
    }
    assert!(!provider.scan_in_progress());
    assert_eq!(provider.roots, vec![installed_root]);
    assert_eq!(provider.status()["complete"], true);
    assert_eq!(provider.games[&42].name, "Later installation");
}

#[cfg(windows)]
#[test]
fn configured_windows_roots_override_install_defaults() {
    let fixture = Libraries::new();
    let configured = fixture.library("configured");
    fixture.manifest(&configured, 7, "Configured game", 4);
    let mut provider = Provider::new(vec![configured.clone()]);
    provider.set_enabled(true);
    assert!(provider.start_scan_with_defaults(|| panic!("default lookup should be skipped")));
    for _ in 0..100 {
        provider.tick();
    }
    assert_eq!(provider.roots, vec![configured]);
    assert_eq!(provider.games[&7].name, "Configured game");
}

#[test]
fn linux_user_root_policy_rejects_machine_and_privileged_processes() {
    let user_status = "Uid:\t1000\t1000\t1000\t1000\nCapEff:\t0000000000000000\n";
    let root_status = "Uid:\t0\t0\t0\t0\nCapEff:\t0000000000000000\n";
    let privileged_status = "Uid:\t1000\t1000\t1000\t1000\nCapEff:\t0000000000200000\n";

    assert!(linux_status_allows_user_roots(user_status, false));
    assert!(!linux_status_allows_user_roots(user_status, true));
    assert!(!linux_status_allows_user_roots(root_status, false));
    assert!(!linux_status_allows_user_roots(privileged_status, false));
    assert!(!linux_status_allows_user_roots("Uid:\t1000\t1000\n", false));
    assert!(!linux_status_allows_user_roots("CapEff:\t0\n", false));
}

#[test]
fn linux_session_defaults_recover_and_explicit_roots_override_them() {
    let fixture = Libraries::new();
    let home = fixture.0.join("home");
    let xdg = fixture.0.join("xdg");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&xdg).unwrap();
    let mut provider = Provider {
        enabled: true,
        automatic_roots: true,
        ..Provider::default()
    };
    let defaults = || linux_default_library_roots(Some(&home), Some(&xdg), true);
    assert!(defaults().is_empty());
    assert!(linux_default_library_roots(Some(&home), Some(&xdg), false).is_empty());
    assert!(provider.start_scan_with_defaults(defaults));
    finish_provider(&mut provider);
    assert_eq!(provider.status()["complete"], false);
    assert_eq!(provider.status()["warnings"], json!(["no_library_roots"]));

    let steam_root = fixture.library("home/.local/share/Steam");
    fixture.manifest(&steam_root, 42, "Installed later", 4);
    let xdg_root = fixture.library("xdg/Steam");
    fixture.manifest(&xdg_root, 43, "XDG game", 4);
    let flatpak_root = fixture.library("home/.var/app/com.valvesoftware.Steam/data/Steam");
    fixture.manifest(&flatpak_root, 44, "Flatpak game", 4);
    let snap_root = fixture.library("home/snap/steam/common/.local/share/Steam");
    fixture.manifest(&snap_root, 45, "Snap game", 4);
    assert_eq!(
        defaults(),
        vec![
            steam_root.clone(),
            flatpak_root,
            snap_root,
            xdg_root.clone()
        ]
    );
    assert!(provider.start_scan_with_defaults(defaults));
    finish_provider(&mut provider);
    assert_eq!(provider.status()["complete"], true);
    assert_eq!(provider.games[&42].name, "Installed later");
    assert_eq!(provider.games[&43].name, "XDG game");
    assert_eq!(provider.games[&44].name, "Flatpak game");
    assert_eq!(provider.games[&45].name, "Snap game");

    let configured = fixture.library("configured");
    fixture.manifest(&configured, 7, "Explicit game", 4);
    let mut explicit = Provider::new(vec![configured.clone()]);
    explicit.set_enabled(true);
    assert!(explicit.start_scan_with_defaults(|| panic!("defaults must not be read")));
    finish_provider(&mut explicit);
    assert_eq!(explicit.roots, vec![configured]);
    assert_eq!(explicit.games.keys().copied().collect::<Vec<_>>(), vec![7]);
}

fn finish_provider(provider: &mut Provider) {
    for _ in 0..10_000 {
        provider.tick();
        if !provider.scan_in_progress() {
            return;
        }
    }
    panic!("Steam provider scan did not finish");
}

#[test]
fn discovery_tracks_local_artwork_changes_without_exposing_host_paths() {
    let fixture = Libraries::new();
    let library = fixture.library("steam-install");
    fixture.manifest(&library, 42, "Example", 4);
    let artwork_directory = library.join("appcache/librarycache");
    fs::create_dir_all(&artwork_directory).unwrap();
    let artwork_path = artwork_directory.join("42_library_600x900_2x.jpg");
    fs::write(&artwork_path, b"\x89PNG\r\n\x1a\nfirst artwork").unwrap();

    let mut scan = Scan::new(std::slice::from_ref(&library));
    finish(&mut scan);
    let first = scan.games.get(&42).unwrap();
    assert_eq!(first.artwork_source_path.as_deref(), artwork_path.to_str());
    assert!(!first.artwork_is_remote);
    let first_revision = first.artwork_revision.clone().unwrap();
    let first_candidate_signature = first.artwork_candidate_signature.clone();
    assert!(!first_candidate_signature.is_empty());
    let listed = serde_json::to_value(first).unwrap();
    assert_eq!(listed["artwork_revision"], first_revision);
    assert!(listed.get("artwork_source_path").is_none());
    assert!(listed.get("artwork_candidate_signature").is_none());

    fs::write(&artwork_path, b"\x89PNG\r\n\x1a\nchanged artwork data").unwrap();
    let mut refreshed = Scan::new(&[library]);
    finish(&mut refreshed);
    assert_ne!(
        refreshed.games[&42].artwork_revision.as_deref(),
        Some(first_revision.as_str())
    );
    assert_ne!(
        refreshed.games[&42].artwork_candidate_signature,
        first_candidate_signature
    );
}

#[test]
fn missing_local_artwork_exposes_only_a_stable_remote_artwork_revision() {
    let fixture = Libraries::new();
    let library = fixture.library("steam-install");
    fixture.manifest(&library, 42, "Example", 4);

    let mut scan = Scan::new(std::slice::from_ref(&library));
    finish(&mut scan);
    let game = scan.games.get(&42).unwrap();
    let expected_revision = artwork::remote_revision(42, game.last_updated).unwrap();
    assert!(game.artwork_is_remote);
    assert!(game.artwork_source_path.is_none());
    assert_eq!(
        game.artwork_revision.as_deref(),
        Some(expected_revision.as_str())
    );

    let listed = serde_json::to_value(game).unwrap();
    assert_eq!(listed["artwork_revision"], expected_revision);
    assert!(listed.get("artwork_source_path").is_none());
    assert!(listed.get("artwork_is_remote").is_none());
}

fn finish(scan: &mut Scan) {
    for _ in 0..10_000 {
        scan.tick();
        if scan.finished {
            return;
        }
    }
    panic!("scan did not finish");
}

#[test]
fn discovers_multiple_libraries_and_resolves_duplicates_independent_of_order() {
    let fixture = Libraries::new();
    let first = fixture.library("first");
    let second = fixture.library("second");
    fixture.manifest(&first, 10, "Incomplete", 2);
    fixture.manifest(&second, 10, "Installed", 4);
    fixture.manifest(&second, 20, "Another", 4);
    let escaped = second.to_str().unwrap().replace('\\', "\\\\");
    fs::write(
        first.join("steamapps/libraryfolders.vdf"),
        format!("libraryfolders {{ 0 {{ path \"{escaped}\" apps {{ 10 123 }} }} }}"),
    )
    .unwrap();
    let mut scan = Scan::new(std::slice::from_ref(&first));
    finish(&mut scan);
    assert_eq!(scan.warning_count, 0, "{:?}", scan.warnings);
    assert_eq!(scan.games.len(), 2);
    assert_eq!(scan.games[&10].name, "Installed");
    assert_eq!(scan.games[&10].stable_id, "steam:10");
    let mut reverse = Scan::new(&[second, first]);
    finish(&mut reverse);
    assert_eq!(
        serde_json::to_value(&scan.games).unwrap(),
        serde_json::to_value(&reverse.games).unwrap()
    );
}

#[test]
fn invalid_manifests_do_not_hide_valid_entries_and_partial_scan_is_visible() {
    let fixture = Libraries::new();
    let root = fixture.library("root");
    fixture.manifest(&root, 1, "Valid", 4);
    fs::write(
        root.join("steamapps/appmanifest_2.acf"),
        "AppState { appid 3 }",
    )
    .unwrap();
    fs::write(
        root.join("steamapps/appmanifest_3.acf"),
        "AppState { appid 3 name Bad installdir ../escape StateFlags 4 }",
    )
    .unwrap();
    let mut provider = Provider {
        enabled: true,
        roots: vec![root],
        ..Provider::default()
    };
    assert_eq!(
        provider.handle(json!({"action":"refresh"}))["scanning"],
        true
    );
    for _ in 0..100 {
        provider.tick();
    }
    let status = provider.handle(json!({"action":"status"}));
    assert_eq!(status["game_count"], 1);
    assert_eq!(status["warning_count"], 2);
    assert_eq!(status["complete"], false);
}

#[test]
fn pagination_requires_matching_snapshot_and_refresh_is_atomic() {
    let fixture = Libraries::new();
    let root = fixture.library("root");
    for id in 1..=40 {
        fixture.manifest(&root, id, "Game", 4);
    }
    let mut provider = Provider {
        enabled: true,
        roots: vec![root],
        ..Provider::default()
    };
    provider.handle(json!({"action":"refresh"}));
    for _ in 0..100 {
        provider.tick();
    }
    let first = provider.handle(json!({"action":"list","limit":17}));
    assert_eq!(first["games"].as_array().unwrap().len(), 17);
    assert_eq!(first["next_after"], 17);
    let second = provider.handle(json!({"action":"list","revision":1,"after":17,"limit":32}));
    assert_eq!(second["games"].as_array().unwrap().len(), 23);
    assert_eq!(second["next_after"], serde_json::Value::Null);
    provider.handle(json!({"action":"refresh"}));
    assert_eq!(provider.handle(json!({"action":"list"}))["revision"], 1);
    for _ in 0..100 {
        provider.tick();
    }
    assert_eq!(
        provider.handle(json!({"action":"list","revision":1,"after":17}))["code"],
        "revision_conflict"
    );
    assert_eq!(
        provider.handle(json!({"action":"list","after":17}))["code"],
        "invalid_cursor"
    );
}

#[test]
fn legacy_library_format_and_strict_ids() {
    let path = if cfg!(windows) { "C:\\games" } else { "/games" };
    let escaped = path.replace('\\', "\\\\");
    let document = keyvalues::parse(&format!(
        "LibraryFolders {{ TimeNextStatsReport 0 1 \"{escaped}\" }}"
    ))
    .unwrap();
    assert_eq!(library_paths(&document).unwrap(), vec![PathBuf::from(path)]);
    for name in [
        "appmanifest_0.acf",
        "appmanifest_01.acf",
        "appmanifest_4294967296.acf",
        "appmanifest_1.acf.bak",
    ] {
        assert!(manifest_id(name).is_none());
    }
}

#[test]
fn escaped_metadata_pages_fit_runtime_budget_without_losing_entries() {
    let mut provider = Provider {
        revision: 1,
        ..Provider::default()
    };
    for app_id in 1..=40 {
        provider.games.insert(
            app_id,
            Game {
                stable_id: format!("steam:{app_id}"),
                app_id,
                name: "\"".repeat(512),
                install_directory: "/games/".into(),
                library_directory: "/library/".into(),
                state_flags: 4,
                installed: true,
                last_updated: 0,
                app_type: None,
                last_played: None,
                artwork_revision: None,
                artwork_source_path: None,
                artwork_is_remote: false,
                artwork_candidate_signature: String::new(),
            },
        );
    }
    let mut after = 0;
    let mut count = 0;
    loop {
        let page = provider.handle(json!({"action":"list","revision":1,"after":after,"limit":32}));
        assert!(serde_json::to_vec(&page).unwrap().len() <= 16_384);
        let games = page["games"].as_array().unwrap();
        assert!(!games.is_empty());
        count += games.len();
        match page["next_after"].as_u64() {
            Some(next) => {
                assert!(next > after);
                after = next;
            }
            None => break,
        }
    }
    assert_eq!(count, 40);
}

#[cfg(unix)]
#[test]
fn symlinked_manifest_is_rejected() {
    let fixture = Libraries::new();
    let root = fixture.library("root");
    fixture.manifest(&root, 1, "One", 4);
    std::os::unix::fs::symlink(
        root.join("steamapps/appmanifest_1.acf"),
        root.join("steamapps/appmanifest_2.acf"),
    )
    .unwrap();
    let mut scan = Scan::new(&[root]);
    finish(&mut scan);
    assert_eq!(scan.warning_count, 1);
    assert_eq!(scan.games.len(), 1);
}
