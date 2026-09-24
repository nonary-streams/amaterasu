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
