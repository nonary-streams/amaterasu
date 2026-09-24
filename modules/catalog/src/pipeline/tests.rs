use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Default)]
struct CleanupPolicy {
    remove_stale: bool,
    remove_uninstalled: bool,
    remove_expired: bool,
    remove_selection: bool,
}

#[derive(Default)]
struct Backend {
    revision: u64,
    metadata: Value,
    entries: BTreeMap<String, Value>,
    legacy: Option<Value>,
    fail: bool,
    lose_commit_ack_once: bool,
}
impl Backend {
    fn handle(&mut self, request: Value) -> Value {
        match request["action"].as_str().unwrap() {
            "get" => {
                json!({"ok":true,"revision":0,"found":self.legacy.is_some(),"value":self.legacy})
            }
            "collection_get" => {
                json!({"ok":true,"revision":self.revision,"found":self.revision != 0,"metadata":self.metadata,"count":self.entries.len(),"value":request["key"].as_str().and_then(|id| self.entries.get(id))})
            }
            "collection_commit" => {
                if self.fail {
                    return error("storage_error", "injected write failure");
                }
                if request["expected_revision"] != self.revision {
                    return error("revision_conflict", "concurrent write");
                }
                for edit in request["edits"].as_array().unwrap() {
                    let key = edit["key"].as_str().unwrap();
                    if edit["value"].is_null() {
                        self.entries.remove(key);
                    } else {
                        self.entries.insert(key.into(), edit["value"].clone());
                    }
                }
                self.metadata = request["metadata"].clone();
                self.revision += 1;
                json!({"ok":true,"revision":self.revision})
            }
            "collection_page" => {
                if request["revision"] != self.revision {
                    return error("revision_conflict", "stale collection");
                }
                let after = request["after"].as_str().unwrap_or("");
                let mut entries = Vec::new();
                let mut bytes = 0;
                let mut more = false;
                for (key, value) in self.entries.iter().filter(|(key, _)| key.as_str() > after) {
                    let row = json!({"key":key,"value":value});
                    let size = serde_json::to_vec(&row).unwrap().len();
                    if entries.len() == request["limit"].as_u64().unwrap() as usize
                        || bytes + size > 14_000
                    {
                        more = true;
                        break;
                    }
                    bytes += size;
                    entries.push(row);
                }
                let next = if more {
                    entries.last().map(|entry| entry["key"].clone())
                } else {
                    None
                };
                json!({"ok":true,"revision":self.revision,"entries":entries,"next":next})
            }
            action => panic!("unknown action {action}"),
        }
    }
    fn run(&mut self, pipeline: &mut Pipeline, payload: Value) -> Value {
        self.run_for(pipeline, payload, "caller.one", "original", "caller.one")
    }
    fn run_for(
        &mut self,
        pipeline: &mut Pipeline,
        payload: Value,
        caller: &str,
        initiator: &str,
        owner: &str,
    ) -> Value {
        let mut incoming = Some(payload);
        let mut result = None;
        let mut state_result = None;
        let mut operations = std::collections::BTreeSet::new();
        for _ in 0..1000 {
            pipeline
                .tick(|command| {
                    assert!(serde_json::to_vec(&command).unwrap().len() <= 16_384);
                    Ok(match command {
                        Command::Poll {} => {
                            incoming
                                .take()
                                .map_or(Response::Pending, |payload| Response::Request {
                                    id: 7,
                                    caller: caller.into(),
                                    initiator: initiator.into(),
                                    service: crate::SERVICE.into(),
                                    remaining_ms: 30_000,
                                    payload,
                                })
                        }
                        Command::Request {
                            payload,
                            parent,
                            operation_id,
                            ..
                        } => {
                            assert_eq!(parent, Some(7));
                            assert!(operations.insert(operation_id));
                            assert_eq!(
                                payload
                                    .get("collection")
                                    .or_else(|| payload.get("key"))
                                    .unwrap(),
                                format!("principal/{owner}").as_str()
                            );
                            let is_commit = payload["action"] == "collection_commit";
                            let result = self.handle(payload);
                            state_result = Some(if is_commit && self.lose_commit_ack_once {
                                self.lose_commit_ack_once = false;
                                error("deadline_exceeded", "injected lost commit acknowledgement")
                            } else {
                                result
                            });
                            Response::Accepted { id: 20 }
                        }
                        Command::TakeResult { .. } => {
                            let response = Response::Result {
                                payload: state_result.take().unwrap(),
                            };
                            assert!(serde_json::to_vec(&response).unwrap().len() <= 16_384);
                            response
                        }
                        Command::Complete { payload, .. } => {
                            result = Some(payload);
                            Response::Ok
                        }
                        command => panic!("unexpected command: {command:?}"),
                    })
                })
                .unwrap();
            if let Some(result) = result {
                return result;
            }
        }
        panic!("pipeline stalled")
    }
}
fn entry(name: &str) -> Value {
    json!({"name":name,"launch":{"kind":"application","path":"/Applications/Game.app"}})
}

fn steam_importer() -> Pipeline {
    Pipeline::new(
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::from([("steam".into(), "management".into())]),
    )
}

fn steam_artwork_importer() -> Pipeline {
    Pipeline::new(
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::from([("amaterasu.steam".into(), "amaterasu.management".into())]),
    )
}

fn steam_artwork_entry(name: &str, revision: Option<&str>) -> Value {
    let mut result = entry(name);
    if let Some(revision) = revision {
        result["artwork"] = json!({"provider":"steam","app_id":42,"revision":revision});
    }
    result
}

fn steam_launch_entry(name: &str, app_id: &str, install_dir: Option<&str>) -> Value {
    let mut result = entry(name);
    result["steam_id"] = json!(app_id);
    if let Some(install_dir) = install_dir {
        result["steam_install_dir"] = json!(install_dir);
    }
    result
}

fn run_steam_artwork_snapshot(
    backend: &mut Backend,
    pipeline: &mut Pipeline,
    sync_id: &str,
    source_revision: u64,
    artwork_revision: Option<&str>,
) {
    let begin = backend.run_for(
        pipeline,
        snapshot_begin(sync_id, source_revision),
        "amaterasu.steam",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(begin["ok"], true, "{begin}");
    let generation = begin["generation"].as_u64().unwrap();
    let batch = backend.run_for(
        pipeline,
        snapshot_batch(
            sync_id,
            generation,
            source_revision,
            0,
            begin["revision"].as_u64().unwrap(),
            vec![(
                "steam:42",
                steam_artwork_entry("Provider game", artwork_revision),
            )],
        ),
        "amaterasu.steam",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(batch["ok"], true, "{batch}");
    let finish = backend.run_for(
        pipeline,
        snapshot_finish(
            sync_id,
            generation,
            source_revision,
            batch["revision"].as_u64().unwrap(),
            1,
            true,
        ),
        "amaterasu.steam",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(finish["ok"], true, "{finish}");
}

fn playnite_importer() -> Pipeline {
    Pipeline::new(
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::from([("amaterasu.playnite".into(), "amaterasu.management".into())]),
    )
}

fn lutris_importer() -> Pipeline {
    Pipeline::new(
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::from([("amaterasu.lutris".into(), "amaterasu.management".into())]),
    )
}

fn playnite_entry(name: &str) -> Value {
    json!({
        "name":name,
        "launch":{"kind":"playnite","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}
    })
}

#[test]
fn playnite_fullscreen_synthetic_entry_is_fixed_cas_owned_and_survives_snapshots() {
    let mut backend = Backend::default();
    let mut playnite = playnite_importer();
    let created = backend.run_for(
        &mut playnite,
        json!({"action":"provider_playnite_fullscreen_entry","enabled":true}),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["enabled"], true);
    assert_eq!(created["changed"], true);
    assert_eq!(created["revision"], 1);
    let source_key = "~provider.entry:amaterasu.playnite:fullscreen-entry";
    let entry_id = backend.entries[source_key]["entry_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        backend.entries[&entry_id],
        json!({
            "name":"Playnite (Fullscreen)",
            "description":"",
            "tags":[],
            "launch":{"kind":"playnite_fullscreen","exit_timeout_seconds":10}
        })
    );
    assert_eq!(backend.entries[source_key]["source_id"], "fullscreen-entry");

    let replay = backend.run_for(
        &mut playnite,
        json!({"action":"provider_playnite_fullscreen_entry","enabled":true}),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(replay["ok"], true, "{replay}");
    assert_eq!(replay["changed"], false);
    assert_eq!(backend.metadata["revision"], 1);

    let mut manual = Pipeline::default();
    for action in ["update", "delete"] {
        let payload = if action == "update" {
            json!({
                "action":"update",
                "id":entry_id,
                "expected_revision":1,
                "entry":{"name":"Overwrite","launch":{"kind":"playnite_fullscreen"}}
            })
        } else {
            json!({"action":"delete","id":entry_id,"expected_revision":1})
        };
        let rejected = backend.run_for(
            &mut manual,
            payload,
            "amaterasu.management",
            "amaterasu.management",
            "amaterasu.management",
        );
        assert_eq!(rejected["error"]["code"], "provider_entry_managed");
        assert_eq!(backend.entries[&entry_id]["name"], "Playnite (Fullscreen)");
    }

    let unauthorized = backend.run_for(
        &mut manual,
        json!({"action":"provider_playnite_fullscreen_entry","enabled":false}),
        "amaterasu.steam",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(unauthorized["error"]["code"], "permission_denied");
    assert!(
        serde_json::from_value::<Operation>(json!({
            "action":"provider_playnite_fullscreen_entry",
            "enabled":true,
            "owner":"amaterasu.steam"
        }))
        .is_err()
    );

    let begin = backend.run_for(
        &mut playnite,
        snapshot_begin("ordinary-refresh", 1),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(begin["ok"], true, "{begin}");
    let finished = backend.run_for(
        &mut playnite,
        snapshot_finish(
            "ordinary-refresh",
            begin["generation"].as_u64().unwrap(),
            1,
            begin["revision"].as_u64().unwrap(),
            0,
            true,
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(finished["ok"], true, "{finished}");
    assert!(backend.entries.contains_key(source_key));
    assert!(backend.entries.contains_key(&entry_id));

    let removed = backend.run_for(
        &mut playnite,
        json!({"action":"provider_playnite_fullscreen_entry","enabled":false}),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(removed["ok"], true, "{removed}");
    assert_eq!(removed["enabled"], false);
    assert_eq!(removed["changed"], true);
    assert!(!backend.entries.contains_key(source_key));
    assert!(!backend.entries.contains_key(&entry_id));

    let manual_fullscreen = backend.run_for(
        &mut manual,
        json!({
            "action":"create",
            "expected_revision":removed["revision"],
            "entry":{"name":"Manual Fullscreen","launch":{"kind":"playnite_fullscreen","exit_timeout_seconds":0}}
        }),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(manual_fullscreen["ok"], true, "{manual_fullscreen}");
    assert_eq!(
        backend.entries[manual_fullscreen["id"].as_str().unwrap()]["launch"]["kind"],
        "playnite_fullscreen"
    );
}

#[test]
fn playnite_snapshot_cannot_claim_the_reserved_fullscreen_source_or_launch_kind() {
    let mut backend = Backend::default();
    let mut playnite = playnite_importer();
    let begin = backend.run_for(
        &mut playnite,
        snapshot_begin("forged-fullscreen", 1),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    let generation = begin["generation"].as_u64().unwrap();
    let forged_source = backend.run_for(
        &mut playnite,
        snapshot_batch(
            "forged-fullscreen",
            generation,
            1,
            0,
            begin["revision"].as_u64().unwrap(),
            vec![(
                "fullscreen-entry",
                json!({"name":"Forged","launch":{"kind":"playnite_fullscreen"}}),
            )],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(forged_source["ok"], false);
    assert_eq!(forged_source["error"]["code"], "duplicate_source_id");

    let forged_kind = backend.run_for(
        &mut playnite,
        snapshot_batch(
            "forged-fullscreen",
            generation,
            1,
            0,
            begin["revision"].as_u64().unwrap(),
            vec![(
                "playnite:manual",
                json!({"name":"Forged","launch":{"kind":"playnite_fullscreen"}}),
            )],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(forged_kind["ok"], false);
    assert_eq!(forged_kind["error"]["code"], "invalid_entry");
    assert_eq!(backend.metadata["revision"], 0);
    assert_eq!(backend.entries.len(), 1, "only provider state may exist");
}

fn snapshot_begin(sync_id: &str, source_revision: u64) -> Value {
    json!({
        "action":"provider_snapshot_begin",
        "sync_id":sync_id,
        "source_revision":source_revision,
    })
}

fn snapshot_begin_at(
    sync_id: &str,
    source_revision: u64,
    expected_collection_revision: u64,
) -> Value {
    json!({
        "action":"provider_snapshot_begin",
        "sync_id":sync_id,
        "source_revision":source_revision,
        "expected_collection_revision":expected_collection_revision,
    })
}

fn snapshot_inventory(
    after: Option<&str>,
    expected_collection_revision: Option<u64>,
    limit: usize,
) -> Value {
    json!({
        "action":"provider_snapshot_inventory",
        "after":after,
        "expected_collection_revision":expected_collection_revision,
        "limit":limit,
    })
}

fn snapshot_batch(
    sync_id: &str,
    generation: u64,
    source_revision: u64,
    sequence: u64,
    expected_revision: u64,
    items: Vec<(&str, Value)>,
) -> Value {
    snapshot_batch_with_removals(
        sync_id,
        generation,
        source_revision,
        sequence,
        expected_revision,
        items,
        Vec::new(),
    )
}

fn snapshot_batch_with_removals(
    sync_id: &str,
    generation: u64,
    source_revision: u64,
    sequence: u64,
    expected_revision: u64,
    items: Vec<(&str, Value)>,
    remove_source_ids: Vec<&str>,
) -> Value {
    snapshot_batch_with_intents(
        sync_id,
        generation,
        source_revision,
        sequence,
        expected_revision,
        items,
        (remove_source_ids, Vec::new(), Vec::new()),
    )
}

fn snapshot_batch_with_intents(
    sync_id: &str,
    generation: u64,
    source_revision: u64,
    sequence: u64,
    expected_revision: u64,
    items: Vec<(&str, Value)>,
    (remove_source_ids, expire_source_ids, selection_remove_source_ids): (
        Vec<&str>,
        Vec<&str>,
        Vec<&str>,
    ),
) -> Value {
    json!({
        "action":"provider_snapshot_batch",
        "sync_id":sync_id,
        "generation":generation,
        "source_revision":source_revision,
        "sequence":sequence,
        "expected_revision":expected_revision,
        "entries":items.into_iter().map(|(source_id,entry)| json!({"source_id":source_id,"entry":entry})).collect::<Vec<_>>(),
        "remove_source_ids":remove_source_ids,
        "expire_source_ids":expire_source_ids,
        "selection_remove_source_ids":selection_remove_source_ids,
    })
}

fn snapshot_finish(
    sync_id: &str,
    generation: u64,
    source_revision: u64,
    expected_revision: u64,
    next_sequence: u64,
    complete: bool,
) -> Value {
    snapshot_finish_policy(
        sync_id,
        generation,
        source_revision,
        expected_revision,
        next_sequence,
        complete,
        true,
    )
}

fn snapshot_finish_policy(
    sync_id: &str,
    generation: u64,
    source_revision: u64,
    expected_revision: u64,
    next_sequence: u64,
    complete: bool,
    remove_stale: bool,
) -> Value {
    snapshot_finish_with_policies(
        sync_id,
        generation,
        source_revision,
        expected_revision,
        next_sequence,
        complete,
        CleanupPolicy {
            remove_stale,
            remove_uninstalled: false,
            ..CleanupPolicy::default()
        },
    )
}

fn snapshot_finish_with_policies(
    sync_id: &str,
    generation: u64,
    source_revision: u64,
    expected_revision: u64,
    next_sequence: u64,
    complete: bool,
    cleanup_policy: CleanupPolicy,
) -> Value {
    json!({
        "action":"provider_snapshot_finish",
        "sync_id":sync_id,
        "generation":generation,
        "source_revision":source_revision,
        "expected_revision":expected_revision,
        "next_sequence":next_sequence,
        "complete":complete,
        "remove_stale":cleanup_policy.remove_stale,
        "remove_uninstalled":cleanup_policy.remove_uninstalled,
        "remove_expired":cleanup_policy.remove_expired,
        "remove_selection":cleanup_policy.remove_selection,
    })
}

fn import_playnite_entry(
    backend: &mut Backend,
    pipeline: &mut Pipeline,
    sync_id: &str,
    source_revision: u64,
    expected_revision: u64,
    source_id: &str,
    name: &str,
) -> Value {
    import_playnite_entries(
        backend,
        pipeline,
        sync_id,
        source_revision,
        expected_revision,
        vec![(source_id, name)],
    )
}

fn import_playnite_entries(
    backend: &mut Backend,
    pipeline: &mut Pipeline,
    sync_id: &str,
    source_revision: u64,
    expected_revision: u64,
    items: Vec<(&str, &str)>,
) -> Value {
    let begin = backend.run_for(
        pipeline,
        snapshot_begin(sync_id, source_revision),
        "amaterasu.playnite",
        "management",
        "amaterasu.management",
    );
    assert_eq!(begin["ok"], true);
    let generation = begin["generation"].as_u64().unwrap();
    let batch = backend.run_for(
        pipeline,
        snapshot_batch(
            sync_id,
            generation,
            source_revision,
            0,
            expected_revision,
            items
                .into_iter()
                .map(|(source_id, name)| (source_id, playnite_entry(name)))
                .collect(),
        ),
        "amaterasu.playnite",
        "management",
        "amaterasu.management",
    );
    assert_eq!(batch["ok"], true);
    backend.run_for(
        pipeline,
        snapshot_finish(
            sync_id,
            generation,
            source_revision,
            batch["revision"].as_u64().unwrap(),
            1,
            true,
        ),
        "amaterasu.playnite",
        "management",
        "amaterasu.management",
    )
}

fn update_provider_overrides(id: &str, expected_revision: u64, fec_percentage: Value) -> Value {
    json!({
        "action":"update_provider_overrides",
        "id":id,
        "expected_revision":expected_revision,
        "config_overrides":{"fec_percentage":fec_percentage},
    })
}

#[test]
fn old_provider_snapshot_state_defaults_to_legacy_cleanup_policy() {
    let mut value = serde_json::to_value(SnapshotState::initial()).unwrap();
    value.as_object_mut().unwrap().remove("finish_remove_stale");
    value
        .as_object_mut()
        .unwrap()
        .remove("finish_remove_expired");
    value
        .as_object_mut()
        .unwrap()
        .remove("finish_remove_selection");
    let restored: SnapshotState = serde_json::from_value(value).unwrap();
    assert!(restored.finish_remove_stale);
    assert!(!restored.finish_remove_uninstalled);
    assert!(!restored.finish_remove_expired);
    assert!(!restored.finish_remove_selection);
}

#[test]
fn old_provider_markers_default_to_no_uninstalled_tombstone() {
    let marker: Marker = serde_json::from_value(json!({
        "version":1,
        "caller":"amaterasu.playnite",
        "source_id":"playnite:10",
        "entry_id":"app-0000000000000001",
        "generation":1,
        "fingerprint":"a".repeat(64),
        "detached":false
    }))
    .unwrap();

    assert_eq!(marker.remove_generation, None);
    assert_eq!(marker.added_at_unix, None);
    assert_eq!(marker.source_flags, 0);
    assert_eq!(marker.remove_reason, None);
    assert!(marker.config_overrides.validate().is_ok());
}

#[test]
fn snapshot_finish_policy_is_boolean_and_defaults_to_legacy_removal() {
    let mut request = json!({
        "action":"provider_snapshot_finish",
        "sync_id":"scan-1",
        "generation":1,
        "source_revision":1,
        "expected_revision":0,
        "next_sequence":0,
        "complete":true
    });
    let operation: Operation = serde_json::from_value(request.clone()).unwrap();
    assert!(matches!(
        operation,
        Operation::ProviderSnapshotFinish {
            remove_stale: true,
            remove_uninstalled: false,
            remove_expired: false,
            remove_selection: false,
            ..
        }
    ));

    request["remove_stale"] = json!(false);
    let operation: Operation = serde_json::from_value(request.clone()).unwrap();
    assert!(matches!(
        operation,
        Operation::ProviderSnapshotFinish {
            remove_stale: false,
            remove_uninstalled: false,
            ..
        }
    ));

    request["remove_uninstalled"] = json!(true);
    let operation: Operation = serde_json::from_value(request.clone()).unwrap();
    assert!(matches!(
        operation,
        Operation::ProviderSnapshotFinish {
            remove_stale: false,
            remove_uninstalled: true,
            remove_expired: false,
            remove_selection: false,
            ..
        }
    ));

    request["remove_expired"] = json!(true);
    request["remove_selection"] = json!(true);
    assert!(matches!(
        serde_json::from_value::<Operation>(request.clone()).unwrap(),
        Operation::ProviderSnapshotFinish {
            remove_uninstalled: true,
            remove_expired: true,
            remove_selection: true,
            ..
        }
    ));

    request["remove_stale"] = json!("false");
    assert!(serde_json::from_value::<Operation>(request).is_err());
}

#[test]
fn preparation_and_undo_are_canonically_persisted_across_reload() {
    let executable = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut backend = Backend::default();
    let created = backend.run(
        &mut Pipeline::default(),
        json!({
            "action": "create",
            "expected_revision": 0,
            "entry": {
                "name": "Prepared game",
                "launch": {
                    "kind": "executable",
                    "program": "/games/prepared",
                    "preparation": [
                        {
                            "run": {"executable": executable.clone()},
                            "undo": {"executable": executable.clone(), "args": ["restore"]}
                        },
                        {"run": {"executable": executable.clone(), "timeout_ms": 30_000}}
                    ]
                }
            }
        }),
    );
    assert_eq!(created["ok"], true);

    let canonical = json!([
        {
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
                "environment": {},
                "cwd": null,
                "timeout_ms": 10_000
            }
        },
        {
            "run": {
                "executable": executable.clone(),
                "args": [],
                "environment": {},
                "cwd": null,
                "timeout_ms": 30_000
            },
            "undo": null
        }
    ]);
    let stored = backend.entries["app-0000000000000001"].clone();
    assert_eq!(stored["launch"]["preparation"], canonical);
    assert_eq!(created["entry"], stored);

    let reloaded = backend.run(
        &mut Pipeline::default(),
        json!({"action":"get","id":"app-0000000000000001"}),
    );
    assert_eq!(reloaded["entry"], stored);
    assert_eq!(reloaded["entry"]["launch"]["preparation"], canonical);
}

#[test]
fn ten_thousand_entries_use_single_entry_commits_and_bounded_pages() {
    let mut backend = Backend::default();
    let mut pipeline = Pipeline::default();
    for index in 0..10_000 {
        let result = backend.run(&mut pipeline, json!({"action":"create","expected_revision":index,"entry":entry(&format!("Game {index}"))}));
        assert_eq!(result["revision"], index + 1);
    }
    // A module restart has no catalog cache to recover.
    let mut pipeline = Pipeline::default();
    let mut cursor = Value::Null;
    let mut count = 0;
    loop {
        let page = backend.run(
            &mut pipeline,
            json!({"action":"list","limit":32,"cursor":cursor}),
        );
        count += page["entries"].as_array().unwrap().len();
        assert!(serde_json::to_vec(&page).unwrap().len() < 16_384);
        cursor = page["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(count, 10_000);
    let page = backend.run(&mut pipeline, json!({"action":"list","limit":1}));
    let updated = backend.run(&mut pipeline, json!({"action":"update","id":"app-0000000000000001","expected_revision":10_000,"entry":entry("Changed")}));
    assert_eq!(updated["revision"], 10_001);
    let stale = backend.run(
        &mut pipeline,
        json!({"action":"list","cursor":page["next_cursor"]}),
    );
    assert_eq!(stale["error"]["code"], "revision_conflict");
}
#[test]
fn legacy_migration_preserves_ids_and_failed_writes_do_not_succeed() {
    let mut backend = Backend {
        legacy: Some(
            json!({"revision":9,"next_id":42,"entries":{"app-0000000000000007":entry("Legacy")}}),
        ),
        ..Default::default()
    };
    let mut pipeline = Pipeline::default();
    let first = backend.run(
        &mut pipeline,
        json!({"action":"get","id":"app-0000000000000007"}),
    );
    assert_eq!(first["revision"], 9);
    assert_eq!(first["entry"]["name"], "Legacy");
    backend.fail = true;
    let failure = backend.run(
        &mut pipeline,
        json!({"action":"create","expected_revision":9,"entry":entry("New")}),
    );
    assert_eq!(failure["error"]["code"], "storage_error");
    assert_eq!(backend.metadata["revision"], 9);
    backend.fail = false;
    let created = backend.run(
        &mut Pipeline::default(),
        json!({"action":"create","expected_revision":9,"entry":entry("New")}),
    );
    assert_eq!(created["id"], "app-000000000000002a");
}
#[test]
fn sparse_search_has_bounded_work_and_advancing_empty_pages() {
    let mut backend = Backend {
        revision: 1,
        metadata: json!({"revision":1,"next_id":1002}),
        ..Default::default()
    };
    for index in 1..=1001 {
        backend.entries.insert(
            format!("app-{index:016x}"),
            entry(if index == 1001 { "Target" } else { "Other" }),
        );
    }
    let mut cursor = Value::Null;
    let mut found = 0;
    let mut pages = 0;
    loop {
        let result = backend.run(
            &mut Pipeline::default(),
            json!({"action":"search","query":"target","cursor":cursor}),
        );
        found += result["entries"].as_array().unwrap().len();
        pages += 1;
        cursor = result["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(found, 1);
    assert_eq!(pages, 4);
}
#[test]
fn expired_work_never_reaches_state_and_queue_is_bounded() {
    let mut pipeline = Pipeline::default();
    let mut completed = false;
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::Poll {} => Response::Request {
                    id: 7,
                    caller: "caller".into(),
                    initiator: "caller".into(),
                    service: crate::SERVICE.into(),
                    remaining_ms: 0,
                    payload: json!({"action":"list"}),
                },
                Command::Complete { payload, .. } => {
                    assert_eq!(payload["error"]["code"], "deadline_exceeded");
                    completed = true;
                    Response::Ok
                }
                other => panic!("expired work: {other:?}"),
            })
        })
        .unwrap();
    assert!(completed);
    for _ in 0..100 {
        pipeline
            .tick(|command| {
                Ok(match command {
                    Command::Poll {} => Response::Request {
                        id: 7,
                        caller: "caller".into(),
                        initiator: "caller".into(),
                        service: crate::SERVICE.into(),
                        remaining_ms: 30_000,
                        payload: json!({"action":"list"}),
                    },
                    Command::Request { .. } => Response::Accepted { id: 20 },
                    Command::TakeResult { .. } => Response::Pending,
                    other => panic!("unexpected: {other:?}"),
                })
            })
            .unwrap();
    }
    assert_eq!(pipeline.queued.len(), MAX_QUEUED);
}

#[test]
fn metadata_heavy_pages_and_single_entries_fit_transport() {
    let mut backend = Backend::default();
    let mut pipeline = Pipeline::default();
    let heavy = json!({"name":"Heavy", "description":"x".repeat(1000), "launch":{"kind":"executable","program":"/game","arguments":vec!["x".repeat(1000); 10]}});
    for revision in 0..3 {
        let result = backend.run(
            &mut pipeline,
            json!({"action":"create","expected_revision":revision,"entry":heavy}),
        );
        assert_eq!(result["ok"], true);
    }
    let mut cursor = Value::Null;
    for _ in 0..3 {
        let page = backend.run(
            &mut pipeline,
            json!({"action":"list","cursor":cursor,"limit":32}),
        );
        assert_eq!(page["entries"].as_array().unwrap().len(), 1);
        cursor = page["next_cursor"].clone();
    }
    assert!(cursor.is_null());
    let oversized = json!({"name":"Too large","launch":{"kind":"executable","program":"/game","arguments":vec!["x".repeat(1000); 12]}});
    let failure = backend.run(
        &mut pipeline,
        json!({"action":"create","expected_revision":3,"entry":oversized}),
    );
    assert_eq!(failure["error"]["code"], "invalid_entry");
    assert_eq!(backend.metadata["revision"], 3);
}
#[test]
fn cancelled_or_malformed_state_replies_complete_without_stalling() {
    for response in [
        Response::error("cancelled", "parent ended"),
        Response::Result {
            payload: json!({"ok":true}),
        },
    ] {
        let mut pipeline = Pipeline {
            pending: Some(Pending {
                context: Context {
                    request_id: 7,
                    key: "principal/caller".into(),
                    catalog_owner: None,
                    importer: None,
                    provider_count: 0,
                    deadline: Instant::now(),
                },
                state_request_id: 20,
                stage: Stage::Loading(Operation::List {
                    cursor: None,
                    limit: None,
                }),
            }),
            ..Default::default()
        };
        let mut completed = false;
        pipeline
            .tick(|command| {
                Ok(match command {
                    Command::TakeResult { .. } => response.clone(),
                    Command::Complete { payload, .. } => {
                        assert_eq!(payload["ok"], false);
                        completed = true;
                        Response::error("request_ended", "cancelled")
                    }
                    Command::Poll {} => Response::Pending,
                    other => panic!("unexpected: {other:?}"),
                })
            })
            .unwrap();
        assert!(completed);
        assert!(pipeline.pending.is_none());
    }
}

#[test]
fn delegated_launch_lookup_uses_bound_initiator_and_requires_startup_authority() {
    let pipeline = Pipeline::new(
        BTreeSet::from(["launch".into()]),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let resolve = || Operation::ResolveLaunch {
        id: "app-0000000000000001".into(),
    };
    assert!(
        pipeline
            .select_catalog("stranger", "victim", resolve())
            .is_err()
    );
    let (key, catalog_owner, operation) = pipeline
        .select_catalog("launch", "original.client", resolve())
        .unwrap();
    assert_eq!(key, "principal/original.client");
    assert_eq!(catalog_owner, None);
    assert!(matches!(operation, Operation::Get { .. }));
    let (key, _, _) = pipeline
        .select_catalog(
            "launch",
            "original.client",
            Operation::Get {
                id: "app-0000000000000001".into(),
            },
        )
        .unwrap();
    assert_eq!(key, "principal/launch");
    assert!(
        serde_json::from_value::<Operation>(
            json!({"action":"resolve_launch","id":"app-0000000000000001","owner":"victim"})
        )
        .is_err()
    );
}

#[test]
fn export_authority_uses_fixed_owner_and_rejects_payload_spoofing() {
    let mut pipeline = Pipeline::new(
        BTreeSet::new(),
        BTreeMap::from([("consumer".into(), "owner".into())]),
        BTreeMap::new(),
    );
    let mut backend = Backend::default();
    let denied = backend.run_for(
        &mut pipeline,
        json!({"action":"export_list"}),
        "stranger",
        "consumer",
        "unused",
    );
    assert_eq!(denied["error"]["code"], "permission_denied");
    for field in ["owner", "query", "initiator"] {
        let mut request = json!({"action":"export_list"});
        request[field] = "victim".into();
        let denied = backend.run_for(&mut pipeline, request, "consumer", "owner", "unused");
        assert_eq!(denied["error"]["code"], "invalid_request");
    }
    let empty = backend.run_for(
        &mut pipeline,
        json!({"action":"export_list"}),
        "consumer",
        "unrelated",
        "owner",
    );
    assert_eq!(
        empty,
        json!({"ok":true,"revision":0,"entries":[],"next_cursor":null})
    );
    assert_eq!(backend.revision, 0);
    // Ordinary requests from the same export consumer still use its own namespace.
    let (key, _, _) = pipeline
        .select_catalog(
            "consumer",
            "owner",
            Operation::List {
                cursor: None,
                limit: None,
            },
        )
        .unwrap();
    assert_eq!(key, "principal/consumer");
}

#[test]
fn exported_launch_resolution_requires_both_authorities_and_returns_fixed_owner() {
    let pipeline = Pipeline::new(
        BTreeSet::from(["launch".into()]),
        BTreeMap::from([("nvhttp".into(), "management".into())]),
        BTreeMap::new(),
    );
    let resolve = || Operation::ResolveExportedLaunch {
        id: "app-0000000000000001".into(),
    };
    assert!(
        pipeline
            .select_catalog("stranger", "nvhttp", resolve())
            .is_err()
    );
    assert!(
        pipeline
            .select_catalog("launch", "foreign", resolve())
            .is_err()
    );
    let (key, owner, operation) = pipeline
        .select_catalog("launch", "nvhttp", resolve())
        .unwrap();
    assert_eq!(key, "principal/management");
    assert_eq!(owner.as_deref(), Some("management"));
    assert!(matches!(operation, Operation::ResolveExportedLaunch { .. }));
    assert!(
        serde_json::from_value::<Operation>(json!({
            "action":"resolve_exported_launch",
            "id":"app-0000000000000001",
            "owner":"victim"
        }))
        .is_err()
    );

    let entry = json!({
        "name":"Managed game",
        "launch":{"kind":"executable","program":"/games/managed","working_directory":null}
    });
    let mut backend = Backend {
        revision: 4,
        metadata: json!({"revision":2,"next_id":2}),
        entries: BTreeMap::from([("app-0000000000000001".into(), entry)]),
        ..Default::default()
    };
    let mut pipeline = pipeline;
    let result = backend.run_for(
        &mut pipeline,
        json!({"action":"resolve_exported_launch","id":"app-0000000000000001"}),
        "launch",
        "nvhttp",
        "management",
    );
    assert_eq!(result["ok"], true);
    assert_eq!(result["catalog_owner"], "management");
    assert_eq!(result["entry"]["name"], "Managed game");

    let mut legacy_backend = Backend {
        legacy: Some(json!({
            "revision":2,
            "next_id":2,
            "entries":{"app-0000000000000001":{
                "name":"Legacy managed game",
                "launch":{"kind":"executable","program":"/games/legacy","working_directory":null}
            }}
        })),
        ..Default::default()
    };
    let legacy = legacy_backend.run_for(
        &mut pipeline,
        json!({"action":"resolve_exported_launch","id":"app-0000000000000001"}),
        "launch",
        "nvhttp",
        "management",
    );
    assert_eq!(legacy["catalog_owner"], "management");
    assert_eq!(legacy["entry"]["name"], "Legacy managed game");
    assert_eq!(legacy_backend.revision, 0);
}

#[test]
fn export_pages_are_sanitized_revision_bound_and_read_only() {
    for legacy in [false, true] {
        let entries: BTreeMap<String, Value> = (1..=70).map(|id| {
            (format!("app-{id:016x}"), json!({"name":format!("Game {id}"),"description":"private", "tags":["private"], "launch":{"kind":"executable","program":"/private/game","arguments":["secret"]}}))
        }).collect();
        let mut backend = if legacy {
            Backend {
                legacy: Some(json!({"revision":7,"next_id":71,"entries":entries})),
                ..Default::default()
            }
        } else {
            Backend {
                revision: 3,
                metadata: json!({"revision":7,"next_id":71}),
                entries,
                ..Default::default()
            }
        };
        let initial_revision = backend.revision;
        let mut pipeline = Pipeline::new(
            BTreeSet::new(),
            BTreeMap::from([("consumer".into(), "owner".into())]),
            BTreeMap::new(),
        );
        let mut cursor = Value::Null;
        let mut first_cursor = Value::Null;
        let mut ids = Vec::new();
        loop {
            let reply = backend.run_for(
                &mut pipeline,
                json!({"action":"export_list","limit":32,"cursor":cursor}),
                "consumer",
                "unrelated",
                "owner",
            );
            assert_eq!(reply["revision"], 7);
            for row in reply["entries"].as_array().unwrap() {
                assert_eq!(row.as_object().unwrap().len(), 2);
                assert!(row["name"].as_str().unwrap().starts_with("Game "));
                ids.push(row["id"].as_str().unwrap().to_owned());
            }
            cursor = reply["next_cursor"].clone();
            if cursor.is_null() {
                break;
            }
            assert_eq!(cursor["after"], ids.last().unwrap().as_str());
            assert_eq!(cursor["query"], "");
            if first_cursor.is_null() {
                first_cursor = cursor.clone();
            }
        }
        assert_eq!(ids.len(), 70);
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            backend.revision, initial_revision,
            "export performed a write"
        );
        if legacy {
            backend.legacy.as_mut().unwrap()["revision"] = 8.into();
        } else {
            backend.metadata["revision"] = 8.into();
        }
        let stale = backend.run_for(
            &mut pipeline,
            json!({"action":"export_list","cursor":first_cursor}),
            "consumer",
            "unrelated",
            "owner",
        );
        assert_eq!(stale["error"]["code"], "revision_conflict");
        let invalid = backend.run_for(
            &mut pipeline,
            json!({"action":"export_list","limit":33}),
            "consumer",
            "unrelated",
            "owner",
        );
        assert_eq!(invalid["error"]["code"], "invalid_request");
    }
}

#[test]
fn provider_snapshots_are_caller_bound_replay_safe_and_delete_only_after_complete_finish() {
    let mut backend = Backend::default();
    let mut pipeline = steam_importer();

    let denied = backend.run_for(
        &mut pipeline,
        snapshot_begin("scan-1", 7),
        "untrusted",
        "management",
        "management",
    );
    assert_eq!(denied["error"]["code"], "permission_denied");

    let mut spoofed = snapshot_begin("scan-1", 7);
    spoofed["owner"] = "victim".into();
    let malformed = backend.run_for(&mut pipeline, spoofed, "steam", "management", "management");
    assert_eq!(malformed["error"]["code"], "invalid_request");

    let begin = backend.run_for(
        &mut pipeline,
        snapshot_begin("scan-1", 7),
        "steam",
        "management",
        "management",
    );
    assert_eq!(begin["ok"], true);
    assert_eq!(begin["generation"], 1);
    assert_eq!(begin["revision"], 0);

    let batch = snapshot_batch("scan-1", 1, 7, 0, 0, vec![("steam:10", entry("Imported"))]);
    backend.lose_commit_ack_once = true;
    let unknown = backend.run_for(
        &mut pipeline,
        batch.clone(),
        "steam",
        "management",
        "management",
    );
    assert_eq!(unknown["error"]["code"], "deadline_exceeded");
    assert_eq!(backend.metadata["revision"], 1);
    assert!(backend.entries.contains_key("app-0000000000000001"));

    let altered_replay = backend.run_for(
        &mut pipeline,
        snapshot_batch(
            "scan-1",
            1,
            7,
            0,
            0,
            vec![("steam:10", entry("Different payload"))],
        ),
        "steam",
        "management",
        "management",
    );
    assert_eq!(altered_replay["error"]["code"], "sequence_conflict");

    let replay = backend.run_for(&mut pipeline, batch, "steam", "management", "management");
    assert_eq!(replay["ok"], true, "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["revision"], 1);
    assert_eq!(backend.metadata["revision"], 1);
    assert_eq!(backend.entries["app-0000000000000001"]["name"], "Imported");

    let visible = backend.run_for(
        &mut pipeline,
        json!({"action":"list","limit":32}),
        "management",
        "management",
        "management",
    );
    assert_eq!(visible["entries"].as_array().unwrap().len(), 1);
    assert_eq!(visible["entries"][0]["entry"]["name"], "Imported");
    assert!(visible["next_cursor"].is_null());

    let stale_revision = backend.run_for(
        &mut pipeline,
        snapshot_batch("scan-1", 1, 7, 1, 0, vec![("steam:11", entry("Stale"))]),
        "steam",
        "management",
        "management",
    );
    assert_eq!(stale_revision["error"]["code"], "revision_conflict");

    let gap = backend.run_for(
        &mut pipeline,
        snapshot_batch("scan-1", 1, 7, 2, 1, vec![("steam:11", entry("Gap"))]),
        "steam",
        "management",
        "management",
    );
    assert_eq!(gap["error"]["code"], "sequence_conflict");

    let incomplete = backend.run_for(
        &mut pipeline,
        snapshot_finish("scan-1", 1, 7, 1, 1, false),
        "steam",
        "management",
        "management",
    );
    assert_eq!(incomplete["error"]["code"], "incomplete_snapshot");
    assert!(backend.entries.contains_key("app-0000000000000001"));

    let finish = backend.run_for(
        &mut pipeline,
        snapshot_finish("scan-1", 1, 7, 1, 1, true),
        "steam",
        "management",
        "management",
    );
    assert_eq!(finish["ok"], true);
    assert_eq!(finish["cleanup_pending"], false);
    assert!(backend.entries.contains_key("app-0000000000000001"));

    let next = backend.run_for(
        &mut pipeline,
        snapshot_begin("scan-2", 8),
        "steam",
        "management",
        "management",
    );
    assert_eq!(next["generation"], 2);
    let empty_complete = backend.run_for(
        &mut pipeline,
        snapshot_finish("scan-2", 2, 8, 1, 0, true),
        "steam",
        "management",
        "management",
    );
    assert_eq!(empty_complete["ok"], true);
    assert!(!backend.entries.contains_key("app-0000000000000001"));
    assert_eq!(backend.entries.len(), 1, "the provider state row remains");
    assert_eq!(backend.metadata["provider_count"], 1);
}

#[test]
fn provider_uninstalled_tombstones_are_authorized_bounded_and_unique() {
    let mut backend = Backend::default();
    let mut pipeline = playnite_importer();
    let imported = import_playnite_entry(
        &mut backend,
        &mut pipeline,
        "playnite-scan-1",
        1,
        0,
        "playnite:10",
        "Imported",
    );
    assert_eq!(imported["ok"], true);

    let unauthorized = backend.run_for(
        &mut pipeline,
        snapshot_batch_with_removals(
            "playnite-scan-1",
            1,
            1,
            1,
            1,
            Vec::new(),
            vec!["playnite:10"],
        ),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(unauthorized["error"]["code"], "permission_denied");

    let begin = backend.run_for(
        &mut pipeline,
        snapshot_begin("playnite-scan-2", 2),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    let generation = begin["generation"].as_u64().unwrap();
    assert_eq!(generation, 2);

    let overlap = backend.run_for(
        &mut pipeline,
        snapshot_batch_with_removals(
            "playnite-scan-2",
            generation,
            2,
            0,
            1,
            vec![("playnite:10", playnite_entry("Imported"))],
            vec!["playnite:10"],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(overlap["error"]["code"], "invalid_request");

    let duplicates = backend.run_for(
        &mut pipeline,
        snapshot_batch_with_removals(
            "playnite-scan-2",
            generation,
            2,
            0,
            1,
            Vec::new(),
            vec!["playnite:10", "playnite:10"],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(duplicates["error"]["code"], "invalid_request");

    let too_many = backend.run_for(
        &mut pipeline,
        snapshot_batch_with_removals(
            "playnite-scan-2",
            generation,
            2,
            0,
            1,
            Vec::new(),
            vec![
                "playnite:a",
                "playnite:b",
                "playnite:c",
                "playnite:d",
                "playnite:e",
                "playnite:f",
                "playnite:g",
                "playnite:h",
                "playnite:i",
            ],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(too_many["error"]["code"], "invalid_request");

    let tombstone = backend.run_for(
        &mut pipeline,
        snapshot_batch_with_removals(
            "playnite-scan-2",
            generation,
            2,
            0,
            1,
            Vec::new(),
            vec!["playnite:10"],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(tombstone["ok"], true);

    let cross_batch_duplicate = backend.run_for(
        &mut pipeline,
        snapshot_batch(
            "playnite-scan-2",
            generation,
            2,
            1,
            1,
            vec![("playnite:10", playnite_entry("Duplicate"))],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(
        cross_batch_duplicate["error"]["code"],
        "duplicate_source_id"
    );
    assert_eq!(backend.entries["app-0000000000000001"]["name"], "Imported");
}

#[test]
fn provider_inventory_is_fixed_owner_paginated_and_collection_revision_bound() {
    let mut backend = Backend::default();
    let mut playnite = playnite_importer();
    let imported = import_playnite_entries(
        &mut backend,
        &mut playnite,
        "inventory-seed",
        1,
        0,
        vec![("playnite:10", "First"), ("playnite:11", "Second")],
    );
    assert_eq!(imported["ok"], true);

    let mut steam = steam_importer();
    let steam_begin = backend.run_for(
        &mut steam,
        snapshot_begin("inventory-steam", 1),
        "steam",
        "management",
        "management",
    );
    let steam_batch = backend.run_for(
        &mut steam,
        snapshot_batch(
            "inventory-steam",
            steam_begin["generation"].as_u64().unwrap(),
            1,
            0,
            1,
            vec![("steam:42", entry("Steam row"))],
        ),
        "steam",
        "management",
        "management",
    );
    assert_eq!(steam_batch["ok"], true);
    let steam_finish = backend.run_for(
        &mut steam,
        snapshot_finish(
            "inventory-steam",
            steam_begin["generation"].as_u64().unwrap(),
            1,
            steam_batch["revision"].as_u64().unwrap(),
            1,
            true,
        ),
        "steam",
        "management",
        "management",
    );
    assert_eq!(steam_finish["ok"], true);

    let denied = backend.run_for(
        &mut playnite,
        snapshot_inventory(None, None, 1),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(denied["error"]["code"], "permission_denied");

    let mut retargeted = snapshot_inventory(None, None, 1);
    retargeted["owner"] = json!("principal/victim");
    let rejected = backend.run_for(
        &mut playnite,
        retargeted,
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(rejected["error"]["code"], "invalid_request");

    let first = backend.run_for(
        &mut playnite,
        snapshot_inventory(None, None, 1),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["total_count"], 2);
    assert_eq!(first["entries"][0]["source_id"], "playnite:10");
    assert_eq!(first["entries"][0]["attached"], true);
    assert!(first["entries"][0].get("entry_name").is_none());
    assert!(first["entries"][0]["added_at_unix"].as_i64().unwrap() > 0);
    assert!(first["next_after"].is_string());
    let collection_revision = first["collection_revision"].as_u64().unwrap();

    let second = backend.run_for(
        &mut playnite,
        snapshot_inventory(first["next_after"].as_str(), Some(collection_revision), 1),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(second["entries"][0]["source_id"], "playnite:11");
    assert!(second["next_after"].is_null());

    let manual = backend.run_for(
        &mut Pipeline::default(),
        json!({
            "action":"create",
            "expected_revision":2,
            "entry":{"name":"Manual","launch":{"kind":"application","path":"/Applications/Manual.app"}}
        }),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(manual["ok"], true);

    let stale_page = backend.run_for(
        &mut playnite,
        snapshot_inventory(first["next_after"].as_str(), Some(collection_revision), 1),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(stale_page["error"]["code"], "revision_conflict");
    assert!(
        stale_page["error"]["current_collection_revision"]
            .as_u64()
            .unwrap()
            > collection_revision
    );

    let stale_begin = backend.run_for(
        &mut playnite,
        snapshot_begin_at("stale-inventory", 2, collection_revision),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(stale_begin["error"]["code"], "revision_conflict");
}

#[test]
fn provider_inventory_names_are_fixed_owner_and_attached_row_only() {
    let mut backend = Backend::default();
    let mut lutris = lutris_importer();
    let lutris_begin = backend.run_for(
        &mut lutris,
        snapshot_begin("lutris-inventory-seed", 1),
        "amaterasu.lutris",
        "amaterasu.management",
        "amaterasu.management",
    );
    let lutris_batch = backend.run_for(
        &mut lutris,
        snapshot_batch(
            "lutris-inventory-seed",
            lutris_begin["generation"].as_u64().unwrap(),
            1,
            0,
            lutris_begin["revision"].as_u64().unwrap(),
            vec![("10", entry("Lutris-owned title"))],
        ),
        "amaterasu.lutris",
        "amaterasu.management",
        "amaterasu.management",
    );
    let lutris_finish = backend.run_for(
        &mut lutris,
        snapshot_finish(
            "lutris-inventory-seed",
            lutris_begin["generation"].as_u64().unwrap(),
            1,
            lutris_batch["revision"].as_u64().unwrap(),
            1,
            true,
        ),
        "amaterasu.lutris",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(lutris_finish["ok"], true, "{lutris_finish}");

    let mut playnite = playnite_importer();
    let playnite_expected_revision = backend.metadata["revision"].as_u64().unwrap();
    let playnite_import = import_playnite_entries(
        &mut backend,
        &mut playnite,
        "lutris-inventory-other-provider",
        1,
        playnite_expected_revision,
        vec![("playnite:1", "Playnite-owned title")],
    );
    assert_eq!(playnite_import["ok"], true);
    let manual_expected_revision = backend.metadata["revision"].as_u64().unwrap();
    let manual = backend.run_for(
        &mut Pipeline::default(),
        json!({
            "action":"create",
            "expected_revision":manual_expected_revision,
            "entry":entry("Manual title"),
        }),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(manual["ok"], true, "{manual}");

    let mut inventory_request = snapshot_inventory(None, None, 32);
    inventory_request["include_entry_names"] = json!(true);
    let inventory = backend.run_for(
        &mut lutris,
        inventory_request,
        "amaterasu.lutris",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(inventory["ok"], true, "{inventory}");
    assert_eq!(inventory["total_count"], 1);
    assert_eq!(inventory["entries"][0]["source_id"], "10");
    assert_eq!(inventory["entries"][0]["attached"], true);
    assert_eq!(inventory["entries"][0]["entry_name"], "Lutris-owned title");

    let marker_key = "~provider.entry:amaterasu.lutris:10";
    let entry_id = backend.entries[marker_key]["entry_id"]
        .as_str()
        .unwrap()
        .to_owned();
    backend.entries.get_mut(&entry_id).unwrap()["name"] = json!("User-edited title");
    let mut detached_request = snapshot_inventory(None, None, 32);
    detached_request["include_entry_names"] = json!(true);
    let detached = backend.run_for(
        &mut lutris,
        detached_request,
        "amaterasu.lutris",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(detached["ok"], true, "{detached}");
    assert_eq!(detached["entries"][0]["attached"], false);
    assert!(detached["entries"][0]["entry_name"].is_null());
}

#[test]
fn playnite_marker_keeps_first_import_time_and_legacy_source_flags_on_refresh() {
    let mut backend = Backend::default();
    let mut playnite = playnite_importer();
    let mut begin = backend.run_for(
        &mut playnite,
        snapshot_begin("marker-time-1", 1),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    let generation = begin["generation"].as_u64().unwrap();
    let mut batch = snapshot_batch(
        "marker-time-1",
        generation,
        1,
        0,
        0,
        vec![("playnite:10", playnite_entry("Persistent metadata"))],
    );
    batch["entries"][0]["source_flags"] = json!(8);
    let imported = backend.run_for(
        &mut playnite,
        batch,
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(imported["ok"], true);
    let first_marker_key = "~provider.entry:amaterasu.playnite:playnite:10";
    let first_marker = backend.entries[first_marker_key].clone();
    let first_added_at = first_marker["added_at_unix"].as_i64().unwrap();
    assert_eq!(first_marker["source_flags"], 8);

    begin = backend.run_for(
        &mut playnite,
        snapshot_begin("marker-time-2", 2),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    let generation = begin["generation"].as_u64().unwrap();
    // Simulate an older importer that does not yet send source_flags.
    let refresh = backend.run_for(
        &mut playnite,
        snapshot_batch(
            "marker-time-2",
            generation,
            2,
            0,
            1,
            vec![("playnite:10", playnite_entry("Persistent metadata"))],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(refresh["ok"], true);
    let refreshed_marker = &backend.entries[first_marker_key];
    assert_eq!(refreshed_marker["added_at_unix"], first_added_at);
    assert_eq!(refreshed_marker["source_flags"], 8);
}

#[test]
fn provider_removal_reasons_are_gated_and_bound_to_finish_replay() {
    let mut backend = Backend::default();
    let mut playnite = playnite_importer();
    let imported = import_playnite_entries(
        &mut backend,
        &mut playnite,
        "cleanup-seed",
        1,
        0,
        vec![
            ("playnite:10", "Uninstalled"),
            ("playnite:11", "Expired"),
            ("playnite:12", "Selection"),
        ],
    );
    assert_eq!(imported["ok"], true);
    let uninstalled_marker = "~provider.entry:amaterasu.playnite:playnite:10";
    let expired_marker = "~provider.entry:amaterasu.playnite:playnite:11";
    let selection_marker = "~provider.entry:amaterasu.playnite:playnite:12";
    backend.entries.get_mut(uninstalled_marker).unwrap()["source_flags"] = json!(8);
    backend.entries.get_mut(expired_marker).unwrap()["source_flags"] = json!(1);
    backend.entries.get_mut(selection_marker).unwrap()["source_flags"] = json!(8);
    backend.entries.get_mut(expired_marker).unwrap()["added_at_unix"] = json!(1_700_000_000);
    backend.revision += 1;

    let begin = backend.run_for(
        &mut playnite,
        snapshot_begin("cleanup-policy", 2),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    let generation = begin["generation"].as_u64().unwrap();
    let batch = backend.run_for(
        &mut playnite,
        snapshot_batch_with_intents(
            "cleanup-policy",
            generation,
            2,
            0,
            1,
            Vec::new(),
            (
                vec!["playnite:10"],
                vec!["playnite:11"],
                vec!["playnite:12"],
            ),
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(batch["ok"], true, "{batch}");

    let finish = backend.run_for(
        &mut playnite,
        snapshot_finish_with_policies(
            "cleanup-policy",
            generation,
            2,
            batch["revision"].as_u64().unwrap(),
            1,
            true,
            CleanupPolicy {
                remove_stale: false,
                remove_uninstalled: true,
                remove_expired: true,
                remove_selection: false,
            },
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(finish["ok"], true, "{finish}");
    assert!(!backend.entries.contains_key("app-0000000000000001"));
    assert!(!backend.entries.contains_key("app-0000000000000002"));
    assert!(backend.entries.contains_key("app-0000000000000003"));
    assert!(backend.entries.contains_key(selection_marker));

    let flipped_retry = backend.run_for(
        &mut playnite,
        snapshot_finish_with_policies(
            "cleanup-policy",
            generation,
            2,
            batch["revision"].as_u64().unwrap(),
            1,
            true,
            CleanupPolicy {
                remove_stale: false,
                remove_uninstalled: true,
                remove_expired: true,
                remove_selection: true,
            },
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(flipped_retry["error"]["code"], "snapshot_conflict");
}

#[test]
fn provider_batches_reject_cross_reason_duplicates_and_malformed_lists() {
    let mut backend = Backend::default();
    let mut playnite = playnite_importer();
    let begin = backend.run_for(
        &mut playnite,
        snapshot_begin("invalid-intents", 1),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    let generation = begin["generation"].as_u64().unwrap();
    let duplicate = backend.run_for(
        &mut playnite,
        snapshot_batch_with_intents(
            "invalid-intents",
            generation,
            1,
            0,
            0,
            Vec::new(),
            (Vec::new(), vec!["playnite:10"], vec!["playnite:10"]),
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(duplicate["error"]["code"], "invalid_request");

    let mut malformed = snapshot_batch("invalid-intents", generation, 1, 0, 0, Vec::new());
    malformed["expire_source_ids"] = json!("playnite:10");
    let malformed = backend.run_for(
        &mut playnite,
        malformed,
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(malformed["error"]["code"], "invalid_request");
    assert_eq!(
        backend.entries.len(),
        1,
        "only provider state was committed"
    );
}

#[test]
fn playnite_uninstalled_cleanup_is_explicit_gated_and_scoped_to_unchanged_rows() {
    let mut backend = Backend::default();
    let mut playnite = playnite_importer();
    let imported = import_playnite_entries(
        &mut backend,
        &mut playnite,
        "playnite-scan-1",
        1,
        0,
        vec![
            ("playnite:10", "Removed later"),
            ("playnite:11", "Retained"),
        ],
    );
    assert_eq!(imported["ok"], true);

    let overrides = backend.run_for(
        &mut playnite,
        update_provider_overrides("app-0000000000000002", 1, json!(37)),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(overrides["ok"], true);

    let mut steam = steam_importer();
    let steam_begin = backend.run_for(
        &mut steam,
        snapshot_begin("steam-scan-1", 1),
        "steam",
        "management",
        "management",
    );
    let steam_batch = backend.run_for(
        &mut steam,
        snapshot_batch(
            "steam-scan-1",
            steam_begin["generation"].as_u64().unwrap(),
            1,
            0,
            2,
            vec![("steam:42", entry("Steam entry"))],
        ),
        "steam",
        "management",
        "management",
    );
    assert_eq!(steam_batch["ok"], true);
    let steam_finish = backend.run_for(
        &mut steam,
        snapshot_finish_with_policies(
            "steam-scan-1",
            steam_begin["generation"].as_u64().unwrap(),
            1,
            steam_batch["revision"].as_u64().unwrap(),
            1,
            true,
            CleanupPolicy {
                remove_stale: false,
                remove_uninstalled: false,
                ..CleanupPolicy::default()
            },
        ),
        "steam",
        "management",
        "management",
    );
    assert_eq!(steam_finish["ok"], true);

    let mut manual = Pipeline::default();
    let manual_entry = backend.run_for(
        &mut manual,
        json!({
            "action":"create",
            "expected_revision":3,
            "entry":{"name":"Manual entry","launch":{"kind":"application","path":"/Applications/Manual.app"}}
        }),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(manual_entry["ok"], true);
    assert_eq!(manual_entry["id"], "app-0000000000000004");

    let generation_two = backend.run_for(
        &mut playnite,
        snapshot_begin("playnite-scan-2", 2),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    let generation = generation_two["generation"].as_u64().unwrap();
    let batch = backend.run_for(
        &mut playnite,
        snapshot_batch_with_removals(
            "playnite-scan-2",
            generation,
            2,
            0,
            4,
            vec![("playnite:11", playnite_entry("Retained after refresh"))],
            vec!["playnite:10"],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(batch["ok"], true);
    assert_eq!(batch["revision"], 5);

    // Manual refreshes keep the explicit source fact in storage but do not
    // apply the membership deletion policy when auto_sync is disabled.
    let retained = backend.run_for(
        &mut playnite,
        snapshot_finish_with_policies(
            "playnite-scan-2",
            generation,
            2,
            5,
            1,
            true,
            CleanupPolicy {
                remove_stale: false,
                remove_uninstalled: false,
                ..CleanupPolicy::default()
            },
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(retained["ok"], true);
    assert!(backend.entries.contains_key("app-0000000000000001"));
    assert_eq!(
        backend.entries["app-0000000000000002"]["config_overrides"]["fec_percentage"],
        37
    );
    assert_eq!(
        backend.entries["app-0000000000000002"]["name"],
        "Retained after refresh"
    );

    let generation_three = backend.run_for(
        &mut playnite,
        snapshot_begin("playnite-scan-3", 3),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    let generation = generation_three["generation"].as_u64().unwrap();
    let tombstone = backend.run_for(
        &mut playnite,
        snapshot_batch_with_removals(
            "playnite-scan-3",
            generation,
            3,
            0,
            5,
            Vec::new(),
            vec!["playnite:10"],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(tombstone["ok"], true);

    let incomplete = backend.run_for(
        &mut playnite,
        snapshot_finish_with_policies(
            "playnite-scan-3",
            generation,
            3,
            5,
            1,
            false,
            CleanupPolicy {
                remove_stale: false,
                remove_uninstalled: true,
                ..CleanupPolicy::default()
            },
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(incomplete["error"]["code"], "incomplete_snapshot");
    assert!(backend.entries.contains_key("app-0000000000000001"));

    let finish_with_removal = snapshot_finish_with_policies(
        "playnite-scan-3",
        generation,
        3,
        5,
        1,
        true,
        CleanupPolicy {
            remove_stale: false,
            remove_uninstalled: true,
            ..CleanupPolicy::default()
        },
    );
    backend.lose_commit_ack_once = true;
    let uncertain = backend.run_for(
        &mut playnite,
        finish_with_removal.clone(),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(uncertain["error"]["code"], "deadline_exceeded");
    assert!(!backend.entries.contains_key("app-0000000000000001"));

    let cleaned = backend.run_for(
        &mut playnite,
        finish_with_removal,
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(cleaned["ok"], true);
    assert!(!backend.entries.contains_key("app-0000000000000001"));
    assert!(
        !backend
            .entries
            .contains_key("~provider.entry:amaterasu.playnite:playnite:10")
    );
    assert!(backend.entries.contains_key("app-0000000000000002"));
    assert!(backend.entries.contains_key("app-0000000000000003"));
    assert!(backend.entries.contains_key("app-0000000000000004"));
    assert_eq!(
        backend.entries["app-0000000000000002"]["config_overrides"]["fec_percentage"],
        37
    );
    assert_eq!(backend.metadata["provider_count"], 4);

    let replay_with_changed_policy = backend.run_for(
        &mut playnite,
        snapshot_finish_with_policies(
            "playnite-scan-3",
            generation,
            3,
            5,
            1,
            true,
            CleanupPolicy {
                remove_stale: false,
                remove_uninstalled: false,
                ..CleanupPolicy::default()
            },
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(
        replay_with_changed_policy["error"]["code"],
        "snapshot_conflict"
    );
}

#[test]
fn playnite_tombstone_cleanup_conflict_does_not_delete_an_entry() {
    let mut backend = Backend::default();
    let mut playnite = playnite_importer();
    let imported = import_playnite_entry(
        &mut backend,
        &mut playnite,
        "playnite-scan-1",
        1,
        0,
        "playnite:10",
        "Imported",
    );
    assert_eq!(imported["ok"], true);

    let begin = backend.run_for(
        &mut playnite,
        snapshot_begin("playnite-scan-2", 2),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    let generation = begin["generation"].as_u64().unwrap();
    let tombstone = backend.run_for(
        &mut playnite,
        snapshot_batch_with_removals(
            "playnite-scan-2",
            generation,
            2,
            0,
            1,
            Vec::new(),
            vec!["playnite:10"],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(tombstone["ok"], true);

    let concurrent = backend.run_for(
        &mut Pipeline::default(),
        json!({
            "action":"create",
            "expected_revision":1,
            "entry":{"name":"Concurrent manual row","launch":{"kind":"application","path":"/Applications/Concurrent.app"}}
        }),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(concurrent["ok"], true);

    let conflict = backend.run_for(
        &mut playnite,
        snapshot_finish_with_policies(
            "playnite-scan-2",
            generation,
            2,
            1,
            1,
            true,
            CleanupPolicy {
                remove_stale: false,
                remove_uninstalled: true,
                ..CleanupPolicy::default()
            },
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(conflict["error"]["code"], "revision_conflict");
    assert!(backend.entries.contains_key("app-0000000000000001"));
    assert!(backend.entries.contains_key("app-0000000000000002"));
}

#[test]
fn empty_or_rejected_playnite_snapshot_has_no_cleanup_effect() {
    let mut backend = Backend::default();
    let mut playnite = playnite_importer();
    let imported = import_playnite_entry(
        &mut backend,
        &mut playnite,
        "playnite-scan-1",
        1,
        0,
        "playnite:10",
        "Imported",
    );
    assert_eq!(imported["ok"], true);

    let begin = backend.run_for(
        &mut playnite,
        snapshot_begin("playnite-scan-2", 2),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    let generation = begin["generation"].as_u64().unwrap();
    let rejected_batch = backend.run_for(
        &mut playnite,
        snapshot_batch_with_removals(
            "playnite-scan-2",
            generation,
            2,
            0,
            1,
            Vec::new(),
            vec!["playnite:10", "playnite:10"],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(rejected_batch["error"]["code"], "invalid_request");

    let empty_complete = backend.run_for(
        &mut playnite,
        snapshot_finish_with_policies(
            "playnite-scan-2",
            generation,
            2,
            1,
            0,
            true,
            CleanupPolicy {
                remove_stale: false,
                remove_uninstalled: true,
                ..CleanupPolicy::default()
            },
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(empty_complete["ok"], true);
    assert!(backend.entries.contains_key("app-0000000000000001"));
    assert!(
        backend
            .entries
            .contains_key("~provider.entry:amaterasu.playnite:playnite:10")
    );
}

#[test]
fn provider_snapshot_preserves_manual_config_overrides_across_later_refreshes() {
    let mut backend = Backend::default();
    let mut pipeline = steam_importer();
    let begin = backend.run_for(
        &mut pipeline,
        snapshot_begin("scan-1", 1),
        "steam",
        "management",
        "management",
    );
    let imported = backend.run_for(
        &mut pipeline,
        snapshot_batch(
            "scan-1",
            begin["generation"].as_u64().unwrap(),
            1,
            0,
            0,
            vec![("steam:10", entry("Imported"))],
        ),
        "steam",
        "management",
        "management",
    );
    assert_eq!(imported["revision"], 1);

    let mut configured_entry = entry("Hand edited");
    configured_entry["config_overrides"] =
        json!({"fec_percentage":37,"audio_sink_capture_only":false});
    let manual = backend.run_for(
        &mut pipeline,
        json!({"action":"update","id":"app-0000000000000001","expected_revision":1,"entry":configured_entry}),
        "management",
        "management",
        "management",
    );
    assert_eq!(manual["ok"], true);
    assert_eq!(manual["revision"], 2);

    let restarted = backend.run_for(
        &mut pipeline,
        snapshot_begin("scan-2", 2),
        "steam",
        "management",
        "management",
    );
    let imported_again = backend.run_for(
        &mut pipeline,
        snapshot_batch(
            "scan-2",
            restarted["generation"].as_u64().unwrap(),
            2,
            0,
            2,
            vec![("steam:10", entry("Provider changed name"))],
        ),
        "steam",
        "management",
        "management",
    );
    assert_eq!(imported_again["revision"], 2);
    assert_eq!(
        backend.entries["app-0000000000000001"]["name"],
        "Hand edited"
    );
    assert_eq!(
        backend.entries["app-0000000000000001"]["config_overrides"]["fec_percentage"],
        37
    );
    assert_eq!(
        backend.entries["app-0000000000000001"]["config_overrides"]["audio_sink_capture_only"],
        false
    );
    let marker = backend.entries["~provider.entry:steam:steam:10"].clone();
    assert_eq!(marker["detached"], true);

    let finished = backend.run_for(
        &mut pipeline,
        snapshot_finish("scan-2", 2, 2, 2, 1, true),
        "steam",
        "management",
        "management",
    );
    assert_eq!(finished["ok"], true);

    let third = backend.run_for(
        &mut pipeline,
        snapshot_begin("scan-3", 3),
        "steam",
        "management",
        "management",
    );
    let cleaned = backend.run_for(
        &mut pipeline,
        snapshot_finish("scan-3", 3, 3, 2, 0, true),
        "steam",
        "management",
        "management",
    );
    assert_eq!(third["generation"], 3);
    assert_eq!(cleaned["ok"], true);
    assert_eq!(
        backend.entries["app-0000000000000001"]["name"],
        "Hand edited"
    );
    assert!(
        !backend
            .entries
            .contains_key("~provider.entry:steam:steam:10")
    );
}

#[test]
fn generic_management_update_cannot_replace_provider_owned_playnite_identity() {
    let mut backend = Backend::default();
    let mut pipeline = playnite_importer();
    let imported = import_playnite_entry(
        &mut backend,
        &mut pipeline,
        "playnite-scan-1",
        1,
        0,
        "playnite:10",
        "Imported title",
    );
    assert_eq!(imported["ok"], true);
    let id = "app-0000000000000001";
    let original = backend.entries[id].clone();
    let revision = backend.metadata["revision"].as_u64().unwrap();

    let stale = backend.run_for(
        &mut pipeline,
        json!({
            "action":"update",
            "id":id,
            "expected_revision":revision - 1,
            "entry":entry("Forged executable replacement")
        }),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(stale["error"]["code"], "revision_conflict");

    let update = backend.run_for(
        &mut pipeline,
        json!({
            "action":"update",
            "id":id,
            "expected_revision":revision,
            "entry":entry("Forged executable replacement")
        }),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(update["error"]["code"], "provider_entry_managed");
    assert_eq!(backend.metadata["revision"], revision);
    assert_eq!(backend.entries[id], original);
    assert_eq!(
        backend.entries[id]["launch"]["game_id"],
        "a1b2c3d4-1234-5678-9abc-0123456789ab"
    );
}

#[test]
fn provider_override_updates_only_fec_and_survives_playnite_refresh_and_cleanup() {
    let mut backend = Backend::default();
    let mut pipeline = playnite_importer();
    let finished = import_playnite_entry(
        &mut backend,
        &mut pipeline,
        "playnite-scan-1",
        1,
        0,
        "playnite:10",
        "Imported title",
    );
    assert_eq!(finished["ok"], true);
    assert_eq!(backend.metadata["revision"], 1);
    let id = "app-0000000000000001";

    let updated = backend.run_for(
        &mut pipeline,
        update_provider_overrides(id, 1, json!(37)),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(updated["ok"], true);
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["changed"], true);
    assert_eq!(updated["replayed"], false);
    assert_eq!(updated["config_overrides"]["fec_percentage"], 37);
    assert_eq!(
        backend.entries[id]["config_overrides"]["fec_percentage"],
        37
    );
    assert_eq!(backend.entries[id]["launch"]["kind"], "playnite");
    assert_eq!(
        backend.entries[id]["launch"]["game_id"],
        "a1b2c3d4-1234-5678-9abc-0123456789ab"
    );
    let marker_key = "~provider.entry:amaterasu.playnite:playnite:10";
    assert_eq!(
        backend.entries[marker_key]["config_overrides"]["fec_percentage"],
        37
    );
    assert_eq!(backend.entries[marker_key]["detached"], false);

    let repeated = backend.run_for(
        &mut pipeline,
        update_provider_overrides(id, 2, json!(37)),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(repeated["ok"], true);
    assert_eq!(repeated["revision"], 2);
    assert_eq!(repeated["changed"], false);
    assert_eq!(repeated["replayed"], false);

    let refreshed = import_playnite_entry(
        &mut backend,
        &mut pipeline,
        "playnite-scan-2",
        2,
        2,
        "playnite:10",
        "Provider renamed title",
    );
    assert_eq!(refreshed["ok"], true);
    assert_eq!(backend.entries[id]["name"], "Provider renamed title");
    assert_eq!(
        backend.entries[id]["config_overrides"]["fec_percentage"],
        37
    );
    assert_eq!(
        backend.entries[id]["launch"]["game_id"],
        "a1b2c3d4-1234-5678-9abc-0123456789ab"
    );
    assert_eq!(backend.entries[marker_key]["detached"], false);

    let begin = backend.run_for(
        &mut pipeline,
        snapshot_begin("playnite-scan-3", 3),
        "amaterasu.playnite",
        "management",
        "amaterasu.management",
    );
    let removed = backend.run_for(
        &mut pipeline,
        snapshot_finish(
            "playnite-scan-3",
            begin["generation"].as_u64().unwrap(),
            3,
            backend.metadata["revision"].as_u64().unwrap(),
            0,
            true,
        ),
        "amaterasu.playnite",
        "management",
        "amaterasu.management",
    );
    assert_eq!(removed["ok"], true);
    assert!(!backend.entries.contains_key(id));
    assert!(!backend.entries.contains_key(marker_key));
    let current_revision = backend.metadata["revision"].as_u64().unwrap();
    let after_delete = backend.run_for(
        &mut pipeline,
        update_provider_overrides(id, current_revision, json!(37)),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(after_delete["error"]["code"], "provider_entry_not_found");
}

#[test]
fn provider_override_enforces_management_and_fixed_playnite_identity() {
    let mut backend = Backend::default();
    let mut pipeline = playnite_importer();
    let finished = import_playnite_entry(
        &mut backend,
        &mut pipeline,
        "playnite-scan-1",
        1,
        0,
        "playnite:10",
        "Imported title",
    );
    assert_eq!(finished["ok"], true);

    let denied = backend.run_for(
        &mut pipeline,
        update_provider_overrides("app-0000000000000001", 1, json!(37)),
        "attacker",
        "attacker",
        "attacker",
    );
    assert_eq!(denied["error"]["code"], "permission_denied");

    let arbitrary_owner = backend.run_for(
        &mut pipeline,
        json!({
            "action":"update_provider_overrides",
            "id":"app-0000000000000001",
            "owner":"principal/someone-else",
            "expected_revision":1,
            "config_overrides":{"fec_percentage":37}
        }),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(arbitrary_owner["error"]["code"], "invalid_request");

    let stale = backend.run_for(
        &mut pipeline,
        update_provider_overrides("app-0000000000000001", 0, json!(37)),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(stale["error"]["code"], "revision_conflict");
    assert_eq!(stale["error"]["current_revision"], 1);

    for invalid_fec in [json!(0), json!(256)] {
        let invalid = backend.run_for(
            &mut pipeline,
            update_provider_overrides("app-0000000000000001", 1, invalid_fec),
            "amaterasu.management",
            "amaterasu.management",
            "amaterasu.management",
        );
        assert_eq!(invalid["error"]["code"], "invalid_request");
    }

    let mut manual_pipeline = Pipeline::default();
    let manual = backend.run_for(
        &mut manual_pipeline,
        json!({
            "action":"create",
            "expected_revision":1,
            "entry":{"name":"Manual title","launch":{"kind":"application","path":"/Applications/Manual.app"}}
        }),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(manual["ok"], true);
    let manual_id = manual["id"].as_str().unwrap();
    let forged_manual = backend.run_for(
        &mut pipeline,
        update_provider_overrides(manual_id, 2, json!(37)),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(forged_manual["error"]["code"], "provider_entry_not_found");
    assert!(backend.entries[manual_id]["config_overrides"].is_null());
}

#[test]
fn provider_override_rejects_entries_marked_for_another_provider() {
    let id = "app-0000000000000001";
    let source_id = "steam:10";
    let imported_entry: crate::model::Entry = serde_json::from_value(entry("Steam title")).unwrap();
    let marker = Marker {
        version: 1,
        caller: "amaterasu.steam".into(),
        source_id: source_id.into(),
        entry_id: id.into(),
        generation: 1,
        fingerprint: provider_entry_fingerprint("amaterasu.steam", &imported_entry),
        config_overrides: ConfigOverrides::default(),
        detached: false,
        remove_generation: None,
        added_at_unix: Some(1_700_000_000),
        source_flags: 8,
        remove_reason: None,
    };
    let playnite_state = SnapshotState {
        version: 1,
        generation: 1,
        sync_id: "playnite-scan".into(),
        source_revision: 1,
        next_sequence: 0,
        last_batch: None,
        finished: true,
        cleanup_pending: false,
        finish_expected_revision: Some(1),
        finish_remove_stale: true,
        finish_remove_uninstalled: false,
        finish_remove_expired: false,
        finish_remove_selection: false,
    };
    let mut backend = Backend {
        revision: 1,
        metadata: json!({"revision":1,"next_id":2,"provider_count":2}),
        ..Backend::default()
    };
    backend
        .entries
        .insert(id.into(), serde_json::to_value(imported_entry).unwrap());
    backend.entries.insert(
        entry_key("amaterasu.steam", source_id),
        marker_value(&marker),
    );
    backend.entries.insert(
        state_key("amaterasu.playnite"),
        serde_json::to_value(playnite_state).unwrap(),
    );
    let mut pipeline = playnite_importer();

    let denied = backend.run_for(
        &mut pipeline,
        update_provider_overrides(id, 1, json!(37)),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(denied["error"]["code"], "provider_entry_not_found");
    assert!(backend.entries[id]["config_overrides"].is_null());
    assert_eq!(backend.revision, 1);
}

#[test]
fn provider_override_recovers_lost_commit_ack_as_an_idempotent_replay() {
    let mut backend = Backend::default();
    let mut pipeline = playnite_importer();
    let finished = import_playnite_entry(
        &mut backend,
        &mut pipeline,
        "playnite-scan-1",
        1,
        0,
        "playnite:10",
        "Imported title",
    );
    assert_eq!(finished["ok"], true);

    backend.lose_commit_ack_once = true;
    let uncertain = backend.run_for(
        &mut pipeline,
        update_provider_overrides("app-0000000000000001", 1, json!(37)),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(uncertain["error"]["code"], "deadline_exceeded");
    assert_eq!(backend.metadata["revision"], 2);
    assert_eq!(
        backend.entries["app-0000000000000001"]["config_overrides"]["fec_percentage"],
        37
    );

    let replay = backend.run_for(
        &mut pipeline,
        update_provider_overrides("app-0000000000000001", 1, json!(37)),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(replay["ok"], true, "{replay}");
    assert_eq!(replay["revision"], 2);
    assert_eq!(replay["changed"], false);
    assert_eq!(replay["replayed"], true);
    assert_eq!(backend.metadata["revision"], 2);
}

#[test]
fn only_the_fixed_playnite_importer_can_write_typed_playnite_launch_targets() {
    let mut backend = Backend::default();
    let mut steam = steam_importer();
    let begin = backend.run_for(
        &mut steam,
        snapshot_begin("scan-steam", 1),
        "steam",
        "management",
        "management",
    );
    let denied = backend.run_for(
        &mut steam,
        snapshot_batch(
            "scan-steam",
            begin["generation"].as_u64().unwrap(),
            1,
            0,
            0,
            vec![("steam:1", playnite_entry("Forged target"))],
        ),
        "steam",
        "management",
        "management",
    );
    assert_eq!(denied["error"]["code"], "invalid_entry");
    assert!(!backend.entries.contains_key("app-0000000000000001"));

    let mut playnite = playnite_importer();
    let begin = backend.run_for(
        &mut playnite,
        snapshot_begin("scan-playnite", 1),
        "amaterasu.playnite",
        "management",
        "amaterasu.management",
    );
    let accepted = backend.run_for(
        &mut playnite,
        snapshot_batch(
            "scan-playnite",
            begin["generation"].as_u64().unwrap(),
            1,
            0,
            0,
            vec![(
                "a1b2c3d4-1234-5678-9abc-0123456789ab",
                playnite_entry("Trusted target"),
            )],
        ),
        "amaterasu.playnite",
        "management",
        "amaterasu.management",
    );
    assert_eq!(accepted["ok"], true, "{accepted}");
    assert_eq!(
        backend.entries["app-0000000000000001"]["launch"]["kind"],
        "playnite"
    );
}

#[test]
fn finish_policy_retains_then_cleans_only_unchanged_provider_entries_and_replays_stably() {
    let mut backend = Backend::default();
    let mut pipeline = steam_importer();
    let first = backend.run_for(
        &mut pipeline,
        snapshot_begin("scan-1", 1),
        "steam",
        "management",
        "management",
    );
    let imported = backend.run_for(
        &mut pipeline,
        snapshot_batch(
            "scan-1",
            first["generation"].as_u64().unwrap(),
            1,
            0,
            0,
            vec![("steam:10", entry("Managed"))],
        ),
        "steam",
        "management",
        "management",
    );
    assert_eq!(imported["revision"], 1);
    let first_finished = backend.run_for(
        &mut pipeline,
        snapshot_finish("scan-1", 1, 1, 1, 1, true),
        "steam",
        "management",
        "management",
    );
    assert_eq!(first_finished["remove_stale"], true);

    let second = backend.run_for(
        &mut pipeline,
        snapshot_begin("scan-2", 2),
        "steam",
        "management",
        "management",
    );
    let retained_finish = snapshot_finish_policy(
        "scan-2",
        second["generation"].as_u64().unwrap(),
        2,
        1,
        0,
        true,
        false,
    );
    backend.lose_commit_ack_once = true;
    let ambiguous = backend.run_for(
        &mut pipeline,
        retained_finish.clone(),
        "steam",
        "management",
        "management",
    );
    assert_eq!(ambiguous["error"]["code"], "deadline_exceeded");
    assert!(backend.entries.contains_key("app-0000000000000001"));
    assert!(
        backend
            .entries
            .contains_key("~provider.entry:steam:steam:10")
    );

    let mut restarted_pipeline = steam_importer();
    let replay = backend.run_for(
        &mut restarted_pipeline,
        retained_finish,
        "steam",
        "management",
        "management",
    );
    assert_eq!(replay["ok"], true);
    assert_eq!(replay["remove_stale"], false);
    let flipped_replay = backend.run_for(
        &mut restarted_pipeline,
        snapshot_finish_policy("scan-2", 2, 2, 1, 0, true, true),
        "steam",
        "management",
        "management",
    );
    assert_eq!(flipped_replay["error"]["code"], "snapshot_conflict");

    let third = backend.run_for(
        &mut restarted_pipeline,
        snapshot_begin("scan-3", 3),
        "steam",
        "management",
        "management",
    );
    let cleaned = backend.run_for(
        &mut restarted_pipeline,
        snapshot_finish_policy(
            "scan-3",
            third["generation"].as_u64().unwrap(),
            3,
            1,
            0,
            true,
            true,
        ),
        "steam",
        "management",
        "management",
    );
    assert_eq!(cleaned["ok"], true);
    assert_eq!(cleaned["remove_stale"], true);
    assert!(!backend.entries.contains_key("app-0000000000000001"));
    assert!(
        !backend
            .entries
            .contains_key("~provider.entry:steam:steam:10")
    );
}

#[test]
fn steam_artwork_is_limited_to_the_authenticated_importer_and_matching_source_id() {
    let mut backend = Backend::default();
    let mut pipeline = Pipeline::new(
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::from([
            ("amaterasu.steam".into(), "amaterasu.management".into()),
            ("other.provider".into(), "amaterasu.management".into()),
        ]),
    );

    let other_begin = backend.run_for(
        &mut pipeline,
        snapshot_begin("other-artwork", 1),
        "other.provider",
        "amaterasu.management",
        "amaterasu.management",
    );
    let other_result = backend.run_for(
        &mut pipeline,
        snapshot_batch(
            "other-artwork",
            other_begin["generation"].as_u64().unwrap(),
            1,
            0,
            0,
            vec![("other:42", steam_artwork_entry("Other provider", Some("7")))],
        ),
        "other.provider",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(other_result["error"]["code"], "provider_artwork_required");

    let steam_begin = backend.run_for(
        &mut pipeline,
        snapshot_begin("wrong-source-artwork", 2),
        "amaterasu.steam",
        "amaterasu.management",
        "amaterasu.management",
    );
    let wrong_source = backend.run_for(
        &mut pipeline,
        snapshot_batch(
            "wrong-source-artwork",
            steam_begin["generation"].as_u64().unwrap(),
            2,
            0,
            0,
            vec![("steam:43", steam_artwork_entry("Wrong source", Some("7")))],
        ),
        "amaterasu.steam",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(wrong_source["error"]["code"], "provider_artwork_required");
    assert!(!backend.entries.contains_key("app-0000000000000001"));
}

#[test]
fn steam_runtime_metadata_requires_matching_importer_identity_but_manual_crud_can_set_it() {
    let mut backend = Backend::default();
    let mut pipeline = Pipeline::new(
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::from([
            ("amaterasu.steam".into(), "amaterasu.management".into()),
            ("other.provider".into(), "amaterasu.management".into()),
        ]),
    );

    let other_begin = backend.run_for(
        &mut pipeline,
        snapshot_begin("other-steam-metadata", 1),
        "other.provider",
        "amaterasu.management",
        "amaterasu.management",
    );
    let forged = backend.run_for(
        &mut pipeline,
        snapshot_batch(
            "other-steam-metadata",
            other_begin["generation"].as_u64().unwrap(),
            1,
            0,
            0,
            vec![(
                "other:42",
                steam_launch_entry("Forged", "42", Some("/games/x")),
            )],
        ),
        "other.provider",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(forged["error"]["code"], "provider_identity_required");

    let steam_begin = backend.run_for(
        &mut pipeline,
        snapshot_begin("mismatched-steam-metadata", 2),
        "amaterasu.steam",
        "amaterasu.management",
        "amaterasu.management",
    );
    let mismatched = backend.run_for(
        &mut pipeline,
        snapshot_batch(
            "mismatched-steam-metadata",
            steam_begin["generation"].as_u64().unwrap(),
            2,
            0,
            0,
            vec![("steam:43", steam_launch_entry("Mismatched", "42", None))],
        ),
        "amaterasu.steam",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(mismatched["error"]["code"], "provider_identity_required");

    let manual = backend.run_for(
        &mut Pipeline::default(),
        json!({
            "action":"create",
            "expected_revision":0,
            "entry":{
                "name":"Manual Steam entry",
                "steam_id":"42",
                "steam_install_dir":"/games/space game",
                "launch":{"kind":"application","path":"/Applications/Game.app"}
            }
        }),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(manual["ok"], true, "{manual}");
    assert_eq!(
        backend.entries[manual["id"].as_str().unwrap()]["steam_id"],
        "42"
    );
    assert_eq!(
        backend.entries[manual["id"].as_str().unwrap()]["steam_install_dir"],
        "/games/space game"
    );

    let updated = backend.run_for(
        &mut Pipeline::default(),
        json!({
            "action":"update",
            "id":manual["id"],
            "expected_revision":manual["revision"],
            "entry":{
                "name":"Updated Steam entry",
                "steam_id":"42",
                "steam_install_dir":"/games/renamed",
                "launch":{"kind":"application","path":"/Applications/Game.app"}
            }
        }),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(updated["ok"], true, "{updated}");
    assert_eq!(
        backend.entries[manual["id"].as_str().unwrap()]["steam_install_dir"],
        "/games/renamed"
    );
}

#[test]
fn steam_artwork_refreshes_and_clears_for_attached_rows_but_not_detached_rows() {
    let mut backend = Backend::default();
    let mut pipeline = steam_artwork_importer();

    run_steam_artwork_snapshot(&mut backend, &mut pipeline, "artwork-1", 1, Some("101"));
    let entry_id = "app-0000000000000001";
    assert_eq!(backend.entries[entry_id]["artwork"]["provider"], "steam");
    assert_eq!(backend.entries[entry_id]["artwork"]["app_id"], 42);
    assert_eq!(backend.entries[entry_id]["artwork"]["revision"], "101");

    run_steam_artwork_snapshot(&mut backend, &mut pipeline, "artwork-2", 2, Some("202"));
    assert_eq!(backend.entries[entry_id]["artwork"]["revision"], "202");

    run_steam_artwork_snapshot(&mut backend, &mut pipeline, "artwork-3", 3, None);
    assert!(backend.entries[entry_id].get("artwork").is_none());
    let marker_key = "~provider.entry:amaterasu.steam:steam:42";
    assert_eq!(backend.entries[marker_key]["detached"], false);

    let expected_revision = backend.metadata["revision"].as_u64().unwrap();
    let manual = backend.run_for(
        &mut pipeline,
        json!({
            "action":"update",
            "id":entry_id,
            "expected_revision":expected_revision,
            "entry":steam_artwork_entry("User edited", None)
        }),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(manual["ok"], true, "{manual}");

    run_steam_artwork_snapshot(&mut backend, &mut pipeline, "artwork-4", 4, Some("404"));
    assert_eq!(backend.entries[entry_id]["name"], "User edited");
    assert!(backend.entries[entry_id].get("artwork").is_none());
    assert_eq!(backend.entries[marker_key]["detached"], true);
}

#[test]
fn retained_manually_edited_entry_loses_marker_but_survives_later_cleanup() {
    let mut backend = Backend::default();
    let mut pipeline = steam_importer();
    let begin = backend.run_for(
        &mut pipeline,
        snapshot_begin("scan-1", 1),
        "steam",
        "management",
        "management",
    );
    backend.run_for(
        &mut pipeline,
        snapshot_batch(
            "scan-1",
            begin["generation"].as_u64().unwrap(),
            1,
            0,
            0,
            vec![("steam:10", entry("Managed"))],
        ),
        "steam",
        "management",
        "management",
    );
    backend.run_for(
        &mut pipeline,
        snapshot_finish("scan-1", 1, 1, 1, 1, true),
        "steam",
        "management",
        "management",
    );
    backend.run_for(
        &mut pipeline,
        json!({"action":"update","id":"app-0000000000000001","expected_revision":1,"entry":entry("Hand edited")} ),
        "management",
        "management",
        "management",
    );

    let second = backend.run_for(
        &mut pipeline,
        snapshot_begin("scan-2", 2),
        "steam",
        "management",
        "management",
    );
    let retained = backend.run_for(
        &mut pipeline,
        snapshot_finish_policy(
            "scan-2",
            second["generation"].as_u64().unwrap(),
            2,
            2,
            0,
            true,
            false,
        ),
        "steam",
        "management",
        "management",
    );
    assert_eq!(retained["ok"], true);
    assert_eq!(
        backend.entries["app-0000000000000001"]["name"],
        "Hand edited"
    );
    assert!(
        backend
            .entries
            .contains_key("~provider.entry:steam:steam:10")
    );

    let third = backend.run_for(
        &mut pipeline,
        snapshot_begin("scan-3", 3),
        "steam",
        "management",
        "management",
    );
    let cleaned = backend.run_for(
        &mut pipeline,
        snapshot_finish_policy(
            "scan-3",
            third["generation"].as_u64().unwrap(),
            3,
            2,
            0,
            true,
            true,
        ),
        "steam",
        "management",
        "management",
    );
    assert_eq!(cleaned["ok"], true);
    assert_eq!(
        backend.entries["app-0000000000000001"]["name"],
        "Hand edited"
    );
    assert!(
        !backend
            .entries
            .contains_key("~provider.entry:steam:steam:10")
    );
}
