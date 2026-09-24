use super::*;
use std::collections::BTreeMap;

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

fn snapshot_begin(sync_id: &str, source_revision: u64) -> Value {
    json!({
        "action":"provider_snapshot_begin",
        "sync_id":sync_id,
        "source_revision":source_revision,
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
    json!({
        "action":"provider_snapshot_batch",
        "sync_id":sync_id,
        "generation":generation,
        "source_revision":source_revision,
        "sequence":sequence,
        "expected_revision":expected_revision,
        "entries":items.into_iter().map(|(source_id,entry)| json!({"source_id":source_id,"entry":entry})).collect::<Vec<_>>(),
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
    json!({
        "action":"provider_snapshot_finish",
        "sync_id":sync_id,
        "generation":generation,
        "source_revision":source_revision,
        "expected_revision":expected_revision,
        "next_sequence":next_sequence,
        "complete":complete,
        "remove_stale":remove_stale,
    })
}

#[test]
fn old_provider_snapshot_state_defaults_to_removal_enabled() {
    let mut value = serde_json::to_value(SnapshotState::initial()).unwrap();
    value.as_object_mut().unwrap().remove("finish_remove_stale");
    let restored: SnapshotState = serde_json::from_value(value).unwrap();
    assert!(restored.finish_remove_stale);
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
            ..
        }
    ));

    request["remove_stale"] = json!(false);
    let operation: Operation = serde_json::from_value(request.clone()).unwrap();
    assert!(matches!(
        operation,
        Operation::ProviderSnapshotFinish {
            remove_stale: false,
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
fn provider_snapshot_detaches_manual_edits_and_preserves_them_on_later_cleanup() {
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

    let manual = backend.run_for(
        &mut pipeline,
        json!({"action":"update","id":"app-0000000000000001","expected_revision":1,"entry":entry("Hand edited")}),
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
