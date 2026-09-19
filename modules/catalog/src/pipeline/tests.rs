use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
struct Backend {
    revision: u64,
    metadata: Value,
    entries: BTreeMap<String, Value>,
    legacy: Option<Value>,
    fail: bool,
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
                            state_result = Some(self.handle(payload));
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
    let pipeline = Pipeline::new(BTreeSet::from(["launch".into()]), BTreeMap::new());
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
