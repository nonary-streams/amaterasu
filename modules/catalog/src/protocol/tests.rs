use super::*;

#[test]
fn output_defaults_preserve_exact_text_and_validate_utf8_bounds() {
    let legacy = entry("Legacy output");
    assert!(legacy.output.is_none());
    assert!(
        serde_json::to_value(&legacy)
            .unwrap()
            .get("output")
            .is_none()
    );

    for output in [
        "",
        "null",
        " logs/$(APP).log ",
        "C:\\Logs\\game.log",
        &"é".repeat(512),
    ] {
        let mut configured = serde_json::to_value(&legacy).unwrap();
        configured["output"] = json!(output);
        let configured: Entry = serde_json::from_value(configured).unwrap();
        configured.validate().unwrap();
        assert_eq!(configured.output.as_deref(), Some(output));
        assert_eq!(serde_json::to_value(configured).unwrap()["output"], output);
    }

    let mut null_output = serde_json::to_value(&legacy).unwrap();
    null_output["output"] = Value::Null;
    let null_output: Entry = serde_json::from_value(null_output).unwrap();
    assert!(null_output.output.is_none());
    assert!(
        serde_json::to_value(null_output)
            .unwrap()
            .get("output")
            .is_none()
    );

    for output in [
        "x".repeat(1_025),
        "é".repeat(513),
        "log\0path".into(),
        "log\npath".into(),
        "log\u{0085}path".into(),
    ] {
        let mut invalid = legacy.clone();
        invalid.output = Some(output);
        assert!(invalid.validate().is_err());
    }
    for output in [json!(true), json!(42), json!([]), json!({})] {
        let mut invalid = serde_json::to_value(&legacy).unwrap();
        invalid["output"] = output;
        assert!(serde_json::from_value::<Entry>(invalid).is_err());
    }
}

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

fn playnite_entry() -> Entry {
    serde_json::from_value(json!({
        "name":"Playnite game",
        "launch":{"kind":"playnite","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}
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
fn sparse_fec_override_is_typed_defaulted_and_persisted_with_the_entry() {
    let legacy: Entry = serde_json::from_value(json!({
        "name":"Legacy entry",
        "launch":{"kind":"executable","program":"/games/legacy"}
    }))
    .unwrap();
    assert!(legacy.config_overrides.is_empty());

    let configured: Entry = serde_json::from_value(json!({
        "name":"Configured entry",
        "launch":{"kind":"executable","program":"/games/configured"},
        "config_overrides":{"fec_percentage":37,"controller":false,"gamepad":"auto","keyboard":true,"mouse":false}
    }))
    .unwrap();
    configured.validate().unwrap();
    assert_eq!(configured.config_overrides.fec_percentage, Some(37));
    let persisted = serde_json::to_value(configured).unwrap();
    assert_eq!(persisted["config_overrides"]["fec_percentage"], 37);
    assert_eq!(persisted["config_overrides"]["controller"], false);
    assert_eq!(persisted["config_overrides"]["gamepad"], "auto");
    assert_eq!(persisted["config_overrides"]["keyboard"], true);
    assert_eq!(persisted["config_overrides"]["mouse"], false);

    let invalid_percentage: Entry = serde_json::from_value(json!({
        "name":"Invalid entry",
        "launch":{"kind":"executable","program":"/games/invalid"},
        "config_overrides":{"fec_percentage":0}
    }))
    .unwrap();
    assert!(invalid_percentage.validate().is_err());
    assert!(
        serde_json::from_value::<Entry>(json!({
            "name":"Invalid entry",
            "launch":{"kind":"executable","program":"/games/invalid"},
            "config_overrides":{"fec_percentage":30,"unknown_key":1}
        }))
        .is_err()
    );
}

#[test]
fn global_preparation_exclusion_defaults_aliases_and_persists_with_entry() {
    let legacy: Entry = serde_json::from_value(json!({
        "name":"Legacy entry",
        "launch":{"kind":"executable","program":"/games/legacy"}
    }))
    .unwrap();
    assert!(!legacy.exclude_global_prep_cmd);
    assert_eq!(
        serde_json::to_value(&legacy)
            .unwrap()
            .get("exclude_global_prep_cmd"),
        None
    );

    for field in ["exclude_global_prep_cmd", "exclude-global-prep-cmd"] {
        let mut value = json!({
            "name":"Prepared exclusion",
            "launch":{"kind":"executable","program":"/games/excluded"}
        });
        value[field] = json!(true);
        let excluded: Entry = serde_json::from_value(value).unwrap();
        assert!(excluded.exclude_global_prep_cmd);
        let persisted = serde_json::to_value(excluded).unwrap();
        assert_eq!(persisted["exclude_global_prep_cmd"], true);
    }

    assert!(
        serde_json::from_value::<Entry>(json!({
            "name":"Ambiguous exclusion",
            "exclude_global_prep_cmd":false,
            "exclude-global-prep-cmd":true,
            "launch":{"kind":"executable","program":"/games/game"}
        }))
        .is_err()
    );

    let excluded: Entry = serde_json::from_value(json!({
        "name":"Persisted exclusion",
        "exclude_global_prep_cmd":true,
        "launch":{"kind":"executable","program":"/games/excluded"}
    }))
    .unwrap();
    let Change::Write { document, .. } = apply(
        Operation::Create {
            entry: excluded,
            expected_revision: 0,
        },
        None,
    )
    .unwrap() else {
        panic!()
    };
    let reloaded: Document =
        serde_json::from_value(serde_json::to_value(document).unwrap()).unwrap();
    assert!(reloaded.entries["app-0000000000000001"].exclude_global_prep_cmd);
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
fn manual_mutations_cannot_forge_provider_owned_playnite_launch_targets() {
    let create_error = apply(
        Operation::Create {
            entry: playnite_entry(),
            expected_revision: 0,
        },
        None,
    )
    .err()
    .unwrap();
    assert_eq!(create_error["error"]["code"], "provider_launch_required");

    let document = create(Document::default(), "Ordinary game");
    let update_error = apply(
        Operation::Update {
            id: "app-0000000000000001".into(),
            entry: playnite_entry(),
            expected_revision: 1,
        },
        Some(document.clone()),
    )
    .err()
    .unwrap();
    assert_eq!(update_error["error"]["code"], "provider_launch_required");
    assert_eq!(document.entries.len(), 1);
}

#[test]
fn manual_fullscreen_launch_targets_are_editable_but_cannot_claim_provider_metadata() {
    let fullscreen: Entry = serde_json::from_value(json!({
        "name":"Playnite Fullscreen",
        "launch":{"kind":"playnite_fullscreen","exit_timeout_seconds":0}
    }))
    .unwrap();
    let Change::Write { document, reply } = apply(
        Operation::Create {
            entry: fullscreen,
            expected_revision: 0,
        },
        None,
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(reply["id"], "app-0000000000000001");
    assert_eq!(reply["entry"]["launch"]["kind"], "playnite_fullscreen");

    let edited: Entry = serde_json::from_value(json!({
        "name":"Fullscreen with timeout",
        "launch":{"kind":"playnite_fullscreen","exit_timeout_seconds":300}
    }))
    .unwrap();
    let Change::Write { document, .. } = apply(
        Operation::Update {
            id: "app-0000000000000001".into(),
            entry: edited,
            expected_revision: document.revision,
        },
        Some(document),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        document.entries["app-0000000000000001"].name,
        "Fullscreen with timeout"
    );
    assert!(document.validate().is_ok());

    assert!(serde_json::from_value::<Operation>(json!({
        "action":"create",
        "expected_revision":0,
        "source_id":"fullscreen-entry",
        "entry":{"name":"Forged","provider_source":"amaterasu.playnite","launch":{"kind":"playnite_fullscreen"}}
    }))
    .is_err());
}

#[test]
fn manual_mutations_cannot_forge_provider_owned_steam_artwork() {
    let mut artwork_entry = entry("Steam artwork");
    artwork_entry.artwork = Some(crate::model::ArtworkReference::Steam {
        app_id: 42,
        revision: "123".into(),
    });

    let create_error = apply(
        Operation::Create {
            entry: artwork_entry.clone(),
            expected_revision: 0,
        },
        None,
    )
    .err()
    .unwrap();
    assert_eq!(create_error["error"]["code"], "provider_artwork_required");

    let document = create(Document::default(), "Ordinary game");
    let update_error = apply(
        Operation::Update {
            id: "app-0000000000000001".into(),
            entry: artwork_entry,
            expected_revision: 1,
        },
        Some(document.clone()),
    )
    .err()
    .unwrap();
    assert_eq!(update_error["error"]["code"], "provider_artwork_required");
    assert_eq!(document.entries.len(), 1);
    assert!(document.entries["app-0000000000000001"].artwork.is_none());
}

#[test]
fn manual_mutations_cannot_forge_lutris_artwork() {
    let mut artwork_entry = entry("Lutris artwork");
    artwork_entry.artwork = Some(crate::model::ArtworkReference::Lutris {
        game_id: 42,
        revision: "a".repeat(64),
    });
    let create_error = apply(
        Operation::Create {
            entry: artwork_entry.clone(),
            expected_revision: 0,
        },
        None,
    )
    .err()
    .unwrap();
    assert_eq!(create_error["error"]["code"], "provider_artwork_required");

    let document = create(Document::default(), "Ordinary game");
    let update_error = apply(
        Operation::Update {
            id: "app-0000000000000001".into(),
            entry: artwork_entry,
            expected_revision: 1,
        },
        Some(document.clone()),
    )
    .err()
    .unwrap();
    assert_eq!(update_error["error"]["code"], "provider_artwork_required");
    assert!(document.entries["app-0000000000000001"].artwork.is_none());
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
fn frame_limiter_overrides_are_sparse_normalized_and_platform_validated() {
    let missing: Entry = serde_json::from_value(json!({
        "name":"Inherited limiter settings",
        "launch":{"kind":"executable","program":"/games/inherited"}
    }))
    .unwrap();
    assert!(missing.config_overrides.is_empty());

    let null_values: Entry = serde_json::from_value(json!({
        "name":"Null limiter settings",
        "launch":{"kind":"executable","program":"/games/null"},
        "config_overrides":{
            "frame_limiter_enable":null,
            "frame_limiter_fps_limit":null,
            "frame_limiter_provider":null,
            "frame_limiter_disable_vsync":null,
            "frame_limiter_auto_virtual_framegen":null,
            "rtss_frame_limit_type":null,
            "ds4_back_as_touchpad_click":null
        }
    }))
    .unwrap();
    assert!(null_values.config_overrides.is_empty());
    assert_eq!(
        serde_json::to_value(null_values).unwrap()["config_overrides"],
        json!(null)
    );

    let mut configured: Entry = serde_json::from_value(json!({
        "name":"Configured limiter settings",
        "launch":{"kind":"executable","program":"/games/configured"},
        "config_overrides":{
            "frame_limiter_enable":false,
            "frame_limiter_fps_limit":0.0,
            "frame_limiter_provider":"auto",
            "frame_limiter_disable_vsync":false,
            "frame_limiter_auto_virtual_framegen":true,
            "ds4_back_as_touchpad_click":false
        }
    }))
    .unwrap();
    configured.validate().unwrap();
    assert_eq!(
        configured
            .config_overrides
            .frame_limiter_auto_virtual_framegen
            .as_deref(),
        Some("enabled")
    );
    let persisted = serde_json::to_value(&configured).unwrap();
    assert_eq!(
        persisted["config_overrides"],
        json!({
            "frame_limiter_enable":false,
            "frame_limiter_fps_limit":0.0,
            "frame_limiter_provider":"auto",
            "frame_limiter_disable_vsync":false,
            "frame_limiter_auto_virtual_framegen":"enabled",
            "ds4_back_as_touchpad_click":false
        })
    );

    for fps_limit in [0.0, 1.234, 1000.0] {
        configured.config_overrides.frame_limiter_fps_limit = Some(fps_limit);
        configured.validate().unwrap();
    }
    for fps_limit in [-0.001, 1.2345, 1000.001, f64::NAN, f64::INFINITY] {
        configured.config_overrides.frame_limiter_fps_limit = Some(fps_limit);
        assert!(configured.validate().is_err());
    }

    let platform_provider = if cfg!(windows) {
        "nvidia-control-panel"
    } else if cfg!(target_os = "linux") {
        "proton"
    } else {
        "auto"
    };
    configured.config_overrides.frame_limiter_fps_limit = Some(60.0);
    configured.config_overrides.frame_limiter_provider = Some(platform_provider.into());
    configured.validate().unwrap();
    configured.config_overrides.frame_limiter_provider = Some("unsupported".into());
    assert!(configured.validate().is_err());

    for (legacy, canonical) in [
        (json!(true), "enabled"),
        (json!(false), "disabled"),
        (json!("true"), "enabled"),
        (json!("false"), "disabled"),
    ] {
        let parsed: Entry = serde_json::from_value(json!({
            "name":"Normalized virtual frame generation",
            "launch":{"kind":"executable","program":"/games/configured"},
            "config_overrides":{"frame_limiter_auto_virtual_framegen":legacy}
        }))
        .unwrap();
        assert_eq!(
            parsed
                .config_overrides
                .frame_limiter_auto_virtual_framegen
                .as_deref(),
            Some(canonical)
        );
    }
    assert!(
        serde_json::from_value::<Entry>(json!({
            "name":"Invalid virtual frame generation",
            "launch":{"kind":"executable","program":"/games/configured"},
            "config_overrides":{"frame_limiter_auto_virtual_framegen":"true-ish"}
        }))
        .is_err()
    );

    configured.config_overrides.rtss_frame_limit_type = Some("nvidia reflex".into());
    configured.config_overrides.frame_limiter_provider = Some(platform_provider.into());
    assert_eq!(configured.validate().is_ok(), cfg!(windows));
}

#[test]
fn raw_preparation_alias_preserves_command_text_and_uses_bounded_defaults() {
    let entry: Entry = serde_json::from_value(json!({
        "name":"Legacy prepared game",
        "prep-cmd":[{
            "do":"  configure --profile=\"two words\"  ",
            "undo":" restore --profile=\"two words\" ",
            "elevated":true
        }],
        "launch":{"kind":"executable","program":"/games/prepared"}
    }))
    .unwrap();
    entry.validate().unwrap();
    assert_eq!(
        entry.prep_cmd[0].do_command,
        "  configure --profile=\"two words\"  "
    );
    assert_eq!(entry.prep_cmd[0].undo, " restore --profile=\"two words\" ");
    assert!(entry.prep_cmd[0].elevated);
    assert_eq!(
        serde_json::to_value(&entry).unwrap()["prep_cmd"],
        json!([{
            "do":"  configure --profile=\"two words\"  ",
            "undo":" restore --profile=\"two words\" ",
            "elevated":true
        }])
    );

    let defaulted: Entry = serde_json::from_value(json!({
        "name":"Defaulted prep",
        "prep_cmd":[{}],
        "launch":{"kind":"executable","program":"/games/prepared"}
    }))
    .unwrap();
    assert_eq!(defaulted.prep_cmd[0].do_command, "");
    assert_eq!(defaulted.prep_cmd[0].undo, "");
    assert!(!defaulted.prep_cmd[0].elevated);
    defaulted.validate().unwrap();

    let oversized: Entry = serde_json::from_value(json!({
        "name":"Oversized prep",
        "prep_cmd":[{"do":"x".repeat(2049)}],
        "launch":{"kind":"executable","program":"/games/prepared"}
    }))
    .unwrap();
    assert!(oversized.validate().is_err());

    let nul_command: Entry = serde_json::from_value(json!({
        "name":"NUL prep",
        "prep_cmd":[{"do":"tool\0argument"}],
        "launch":{"kind":"executable","program":"/games/prepared"}
    }))
    .unwrap();
    assert!(nul_command.validate().is_err());

    let combined: Entry = serde_json::from_value(json!({
        "name":"Too many prep steps",
        "prep_cmd":[{"do":"raw"}],
        "launch":{
            "kind":"executable",
            "program":command_path(),
            "preparation":[
                {"run":{"executable":command_path()}},
                {"run":{"executable":command_path()}},
                {"run":{"executable":command_path()}},
                {"run":{"executable":command_path()}},
                {"run":{"executable":command_path()}},
                {"run":{"executable":command_path()}},
                {"run":{"executable":command_path()}},
                {"run":{"executable":command_path()}}
            ]
        }
    }))
    .unwrap();
    assert!(combined.validate().is_err());

    assert!(
        serde_json::from_value::<Entry>(json!({
            "name":"Ambiguous prep aliases",
            "prep_cmd":[],
            "prep-cmd":[],
            "launch":{"kind":"executable","program":"/games/prepared"}
        }))
        .is_err()
    );
}

#[test]
fn playnite_targets_can_store_structured_application_preparation() {
    let mut entry = playnite_entry();
    entry.launch = serde_json::from_value(json!({
        "kind":"playnite",
        "game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab",
        "preparation":[{"run":{"executable":command_path()}}]
    }))
    .unwrap();
    entry.validate().unwrap();
    assert_eq!(
        serde_json::to_value(entry).unwrap()["launch"]["preparation"][0]["run"]["timeout_ms"],
        10_000
    );
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
fn typed_detached_commands_persist_exact_settings_and_reject_invalid_inputs() {
    let entry: Entry = serde_json::from_value(json!({
        "name": "Typed detached lifecycle",
        "launch": {
            "kind": "executable",
            "program": "/games/example/game",
            "detached": [
                "/games/example/legacy --quiet",
                {
                    "command": "/games/example/overlay --launch",
                    "environment": {"OVERLAY_MODE": "quiet"},
                    "working_directory": "/games/example",
                    "stdout": "/games/example/overlay.out",
                    "stderr": "/games/example/overlay.err"
                }
            ]
        }
    }))
    .unwrap();
    entry.validate().unwrap();
    let encoded = serde_json::to_value(&entry).unwrap();
    assert_eq!(
        encoded["launch"]["detached"][0],
        "/games/example/legacy --quiet"
    );
    assert_eq!(encoded["launch"]["detached"][1]["cwd"], "/games/example");
    assert!(
        encoded["launch"]["detached"][1]
            .get("working_directory")
            .is_none()
    );
    assert_eq!(
        encoded["launch"]["detached"][1]["environment"]["OVERLAY_MODE"],
        "quiet"
    );
    assert_eq!(
        serde_json::to_value(serde_json::from_value::<Entry>(encoded.clone()).unwrap()).unwrap(),
        encoded
    );

    for detached in [
        json!({"command":"overlay","cwd":"relative"}),
        json!({"command":"overlay","stdout":"relative.out"}),
        json!({"command":"overlay","environment":{"A-B":"bad"}}),
        json!({"command":"overlay","unknown":true}),
        json!({"command":""}),
    ] {
        let entry = serde_json::from_value::<Entry>(json!({
            "name":"Invalid typed detached",
            "launch":{"kind":"executable","program":"/games/example/game","detached":[detached]}
        }));
        assert!(entry.is_err() || entry.unwrap().validate().is_err());
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
