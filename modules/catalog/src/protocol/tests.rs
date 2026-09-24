use super::*;

fn entry(name: &str) -> Entry {
    serde_json::from_value(json!({
        "name": name,
        "launch": {
            "kind": "executable",
            "program": "/usr/bin/game",
            "arguments": []
        }
    }))
    .unwrap()
}

fn command_path() -> String {
    std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn prepared_entry(preparation: Vec<Value>) -> Entry {
    serde_json::from_value(json!({
        "name": "Prepared game",
        "launch": {
            "kind": "executable",
            "program": "/games/prepared",
            "preparation": preparation
        }
    }))
    .unwrap()
}

fn preparation_step() -> Value {
    json!({"run":{"executable":command_path()}})
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
            auto_detach: Some(true),
            wait_all: Some(true),
            exit_timeout_seconds: Some(10),
            detached: Vec::new(),
            preparation: Vec::new(),
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

#[test]
fn preparation_defaults_serialize_as_canonical_metadata() {
    let executable = command_path();
    let game = prepared_entry(vec![json!({
        "run": {"executable": executable.clone()},
        "undo": {
            "executable": executable.clone(),
            "args": ["restore"],
            "environment": {"MODE": "safe"},
            "cwd": executable.clone(),
            "timeout_ms": 1
        }
    })]);
    game.validate().unwrap();

    let encoded = serde_json::to_value(&game).unwrap();
    assert!(encoded["launch"].get("auto_detach").is_none());
    assert_eq!(
        encoded["launch"]["preparation"],
        json!([{
            "run": {
                "executable": executable.clone(),
                "args": [],
                "environment": {},
                "cwd": null,
                "timeout_ms": 10_000
            },
            "undo": {
                "executable": executable.clone(),
                "args": ["restore"],
                "environment": {"MODE": "safe"},
                "cwd": executable.clone(),
                "timeout_ms": 1
            }
        }])
    );

    let unprepared = serde_json::to_value(entry("Unprepared game")).unwrap();
    assert!(unprepared["launch"].get("preparation").is_none());
}

#[test]
fn auto_detach_can_be_disabled_and_persists_across_reload() {
    let game: Entry = serde_json::from_value(json!({
        "name":"Explicit foreground app",
        "launch":{
            "kind":"executable",
            "program":"/games/example/game",
            "auto-detach":false
        }
    }))
    .unwrap();
    game.validate().unwrap();
    let encoded = serde_json::to_value(&game).unwrap();
    assert_eq!(encoded["launch"]["auto_detach"], false);
    let reloaded: Entry = serde_json::from_value(encoded).unwrap();
    assert_eq!(
        serde_json::to_value(reloaded).unwrap()["launch"]["auto_detach"],
        false
    );
    assert!(
        serde_json::from_value::<Entry>(json!({
            "name":"Ambiguous detach policy",
            "launch":{
                "kind":"executable",
                "program":"/games/example/game",
                "auto-detach":false,
                "auto_detach":true
            }
        }))
        .is_err()
    );
}

#[test]
fn application_lifecycle_policy_defaults_aliases_and_persists() {
    let defaults: Entry = serde_json::from_value(json!({
        "name": "Default lifecycle policy",
        "launch": {"kind": "executable", "program": "/games/example/game"}
    }))
    .unwrap();
    defaults.validate().unwrap();
    let encoded_defaults = serde_json::to_value(defaults).unwrap();
    assert!(encoded_defaults["launch"].get("auto_detach").is_none());
    assert!(encoded_defaults["launch"].get("wait_all").is_none());
    assert!(
        encoded_defaults["launch"]
            .get("exit_timeout_seconds")
            .is_none()
    );
    assert!(encoded_defaults["launch"].get("detached").is_none());

    let entry: Entry = serde_json::from_value(json!({
        "name": "Configured lifecycle policy",
        "launch": {
            "kind": "executable",
            "program": "/games/example/game",
            "working-dir": "/games/example",
            "auto-detach": true,
            "wait-all": false,
            "exit-timeout": 0,
            "detached": ["/games/example/overlay --quiet", "/games/example/metrics"]
        }
    }))
    .unwrap();
    entry.validate().unwrap();

    let Change::Write { document, .. } = apply(
        Operation::Create {
            entry,
            expected_revision: 0,
        },
        None,
    )
    .unwrap() else {
        panic!("creating a catalog entry must persist it");
    };
    let persisted = serde_json::to_value(document).unwrap();
    let reloaded: Document = serde_json::from_value(persisted).unwrap();
    let reloaded = serde_json::to_value(reloaded.entries.values().next().unwrap()).unwrap();
    assert_eq!(reloaded["launch"]["working_directory"], "/games/example");
    assert_eq!(reloaded["launch"]["auto_detach"], true);
    assert_eq!(reloaded["launch"]["wait_all"], false);
    assert_eq!(reloaded["launch"]["exit_timeout_seconds"], 0);
    assert_eq!(
        reloaded["launch"]["detached"],
        json!(["/games/example/overlay --quiet", "/games/example/metrics"])
    );
    assert!(reloaded["launch"].get("wait-all").is_none());
    assert!(reloaded["launch"].get("exit-timeout").is_none());
    assert!(reloaded["launch"].get("working-dir").is_none());
    assert!(reloaded["launch"].get("auto-detach").is_none());
}

#[test]
fn application_lifecycle_policy_rejects_ambiguous_or_unbounded_values() {
    for launch in [
        json!({
            "kind": "executable",
            "program": "/games/example/game",
            "wait-all": false,
            "wait_all": true
        }),
        json!({
            "kind": "executable",
            "program": "/games/example/game",
            "exit-timeout": 10,
            "exit_timeout_seconds": 20
        }),
        json!({
            "kind": "executable",
            "program": "/games/example/game",
            "exit-timeout": 301
        }),
        json!({
            "kind": "executable",
            "program": "/games/example/game",
            "detached": [""]
        }),
    ] {
        assert!(
            serde_json::from_value::<Entry>(json!({
                "name": "Invalid lifecycle policy",
                "launch": launch
            }))
            .map_or(true, |entry| entry.validate().is_err())
        );
    }
}

#[test]
fn vibeshine_working_dir_alias_persists_as_the_canonical_launch_directory() {
    let entry: Entry = serde_json::from_value(json!({
        "name": "Game with a working directory",
        "launch": {
            "kind": "executable",
            "program": "/games/example/bin/game",
            "working-dir": "/games/example"
        }
    }))
    .unwrap();
    entry.validate().unwrap();

    let Change::Write { document, .. } = apply(
        Operation::Create {
            entry,
            expected_revision: 0,
        },
        None,
    )
    .unwrap() else {
        panic!("creating a catalog entry must persist it");
    };
    let persisted_document = serde_json::to_value(&document).unwrap();
    let persisted: Document = serde_json::from_value(persisted_document).unwrap();
    let persisted = serde_json::to_value(persisted.entries.values().next().unwrap()).unwrap();
    assert_eq!(persisted["launch"]["working_directory"], "/games/example");
    assert!(persisted["launch"].get("working-dir").is_none());

    let empty_alias: Entry = serde_json::from_value(json!({
        "name": "Use executable directory",
        "launch": {
            "kind": "executable",
            "program": "/games/example/bin/game",
            "working-dir": ""
        }
    }))
    .unwrap();
    empty_alias.validate().unwrap();
    let empty_alias = serde_json::to_value(empty_alias).unwrap();
    assert!(empty_alias["launch"]["working_directory"].is_null());
    assert!(empty_alias["launch"].get("working-dir").is_none());

    let duplicate_aliases = json!({
        "name": "Ambiguous working directory",
        "launch": {
            "kind": "executable",
            "program": "/games/example/bin/game",
            "working-dir": "/games/example",
            "working_directory": "/games/other"
        }
    });
    assert!(serde_json::from_value::<Entry>(duplicate_aliases).is_err());
}

#[test]
fn preparation_rejects_unknown_fields_at_every_nested_level() {
    let mut launch = json!({
        "name": "Game",
        "launch": {
            "kind": "executable",
            "program": "/games/game",
            "preparation": [preparation_step()]
        }
    });
    launch["launch"]["unexpected"] = true.into();
    assert!(serde_json::from_value::<Entry>(launch).is_err());

    let mut step = preparation_step();
    step["unexpected"] = true.into();
    assert!(
        serde_json::from_value::<Entry>(json!({
            "name": "Game",
            "launch": {
                "kind": "executable",
                "program": "/games/game",
                "preparation": [step]
            }
        }))
        .is_err()
    );

    let mut run = preparation_step();
    run["run"]["unexpected"] = true.into();
    assert!(
        serde_json::from_value::<Entry>(json!({
            "name": "Game",
            "launch": {
                "kind": "executable",
                "program": "/games/game",
                "preparation": [run]
            }
        }))
        .is_err()
    );

    let mut undo = preparation_step();
    undo["undo"] = json!({"executable":command_path(),"unexpected":true});
    assert!(
        serde_json::from_value::<Entry>(json!({
            "name": "Game",
            "launch": {
                "kind": "executable",
                "program": "/games/game",
                "preparation": [undo]
            }
        }))
        .is_err()
    );
}

#[test]
fn preparation_and_command_bounds_are_enforced_for_run_and_undo() {
    let step = preparation_step();
    prepared_entry(vec![step.clone(); 8]).validate().unwrap();
    assert_eq!(
        prepared_entry(vec![step.clone(); 9]).validate(),
        Err("at most 8 preparation steps are allowed")
    );

    for timeout_ms in [1, 30_000] {
        let mut bounded = step.clone();
        bounded["run"]["timeout_ms"] = timeout_ms.into();
        prepared_entry(vec![bounded]).validate().unwrap();
    }
    for (target, timeout_ms) in [("run", 0), ("undo", 30_001)] {
        let mut unbounded = step.clone();
        unbounded[target] = json!({"executable":command_path(),"timeout_ms":timeout_ms});
        assert_eq!(
            prepared_entry(vec![unbounded]).validate(),
            Err("command timeout exceeds bounds")
        );
    }

    let mut too_many_args = step.clone();
    too_many_args["run"]["args"] = json!(vec!["argument"; 65]);
    assert_eq!(
        prepared_entry(vec![too_many_args]).validate(),
        Err("command arguments exceed bounds")
    );

    let environment: serde_json::Map<String, Value> = (0..33)
        .map(|index| (format!("KEY_{index}"), json!("value")))
        .collect();
    let mut too_many_environment_entries = step.clone();
    too_many_environment_entries["run"]["environment"] = environment.into();
    assert_eq!(
        prepared_entry(vec![too_many_environment_entries]).validate(),
        Err("command environment exceeds bounds")
    );

    let mut ambiguous_environment = step;
    ambiguous_environment["run"]["environment"] = json!({"PATH":"one","Path":"two"});
    assert_eq!(
        prepared_entry(vec![ambiguous_environment]).validate(),
        Err("command environment names are ambiguous")
    );
}
