use super::*;

fn entry(name: &str) -> Entry {
    serde_json::from_value(json!({"name":name,"launch":{"kind":"executable","program":"/usr/bin/game","arguments":[]}})).unwrap()
}

fn create(document: Document, name: &str) -> Document {
    let operation = Operation::Create {
        entry: entry(name),
        expected_revision: document.revision,
    };
    let Change::Write { document, .. } = apply(operation, Some(document)).unwrap() else {
        panic!()
    };
    document
}

#[test]
fn stable_ids_survive_rename_reload_and_deletion_without_reuse() {
    let document = create(Document::default(), "First");
    let id = document.entries.keys().next().unwrap().clone();
    let Change::Write { document, .. } = apply(
        Operation::Update {
            id: id.clone(),
            entry: entry("Renamed"),
            expected_revision: 1,
        },
        Some(document),
    )
    .unwrap() else {
        panic!()
    };
    let reloaded: Document =
        serde_json::from_value(serde_json::to_value(document).unwrap()).unwrap();
    assert_eq!(reloaded.entries[&id].name, "Renamed");
    reloaded.validate().unwrap();
    let Change::Write { document, .. } = apply(
        Operation::Delete {
            id: id.clone(),
            expected_revision: 2,
        },
        Some(reloaded),
    )
    .unwrap() else {
        panic!()
    };
    let document = create(document, "New");
    assert!(!document.entries.contains_key(&id));
    assert!(document.entries.contains_key("app-0000000000000002"));
    assert_eq!(document.revision, 4);
}

#[test]
fn mutations_require_revision_and_reject_unknown_authority_fields() {
    assert!(
        serde_json::from_value::<Operation>(json!({"action":"create","entry":entry("Game")}))
            .is_err()
    );
    assert!(
        serde_json::from_value::<Operation>(json!({"action":"list","caller":"other"})).is_err()
    );
    let document = create(Document::default(), "Game");
    let failure = apply(
        Operation::Delete {
            id: "app-0000000000000001".into(),
            expected_revision: 0,
        },
        Some(document),
    )
    .err()
    .unwrap();
    assert_eq!(failure["error"]["code"], "revision_conflict");
    assert_eq!(failure["error"]["current_revision"], 1);
}

#[test]
fn pagination_is_bounded_and_rejects_stale_or_cross_query_cursors() {
    let document = create(
        create(create(Document::default(), "Arcade One"), "Puzzle"),
        "Arcade Two",
    );
    let Change::Read(first) = page(&document, "arcade", None, Some(1), false).unwrap() else {
        panic!()
    };
    assert_eq!(first["entries"].as_array().unwrap().len(), 1);
    let cursor = || serde_json::from_value(first["next_cursor"].clone()).unwrap();
    let Change::Read(second) = page(&document, "arcade", Some(cursor()), Some(1), false).unwrap()
    else {
        panic!()
    };
    assert_eq!(second["entries"][0]["entry"]["name"], "Arcade Two");
    assert!(second["next_cursor"].is_null());
    assert!(page(&document, "puzzle", Some(cursor()), Some(1), false).is_err());
    assert!(page(&document, "", None, Some(33), false).is_err());
    let changed = create(document, "Another");
    assert!(page(&changed, "arcade", Some(cursor()), Some(1), false).is_err());
}

#[test]
fn validates_launch_metadata_cross_platform_and_persisted_identity() {
    for program in [
        "/opt/games/game",
        "C:\\Games\\game.exe",
        "\\\\server\\games\\game.exe",
    ] {
        let mut game = entry("Game");
        game.launch = crate::model::Launch::Executable {
            program: program.into(),
            arguments: vec!["--literal=$HOME".into()],
            working_directory: None,
        };
        game.validate().unwrap();
    }
    for program in ["game", "/games/../bin/game", "C:game.exe", "/bad\0path"] {
        let mut game = entry("Game");
        game.launch = crate::model::Launch::Application {
            path: program.into(),
        };
        assert!(game.validate().is_err());
    }
    let mut document = create(Document::default(), "Game");
    document.next_id = 1;
    assert!(document.validate().is_err());
}
