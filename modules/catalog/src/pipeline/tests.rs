use super::*;
use std::collections::BTreeMap;

mod desktop_seed;

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
    Pipeline::new_with_steam_id_export_principals(
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::from([("steam".into(), "management".into())]),
    )
}

fn steam_artwork_importer() -> Pipeline {
    Pipeline::new_with_steam_id_export_principals(
        BTreeSet::new(),
        BTreeMap::new(),
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

fn lutris_artwork_entry(name: &str, game_id: i64, revision: &str) -> Value {
    let mut result = entry(name);
    result["artwork"] = json!({"provider":"lutris","game_id":game_id,"revision":revision});
    result
}

fn lutris_artwork_importer() -> Pipeline {
    Pipeline::new_with_steam_id_export_principals(
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::from([
            ("amaterasu.lutris".into(), "amaterasu.management".into()),
            ("other.provider".into(), "amaterasu.management".into()),
            ("amaterasu.steam".into(), "amaterasu.management".into()),
        ]),
    )
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
    Pipeline::new_with_steam_id_export_principals(
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::from([("amaterasu.playnite".into(), "amaterasu.management".into())]),
    )
}

fn lutris_importer() -> Pipeline {
    Pipeline::new_with_steam_id_export_principals(
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::from([("amaterasu.lutris".into(), "amaterasu.management".into())]),
    )
}

#[test]
fn launch_provenance_comes_only_from_attached_canonical_lutris_marker() {
    let mut backend = Backend::default();
    let mut pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::from(["amaterasu.launch".into()]),
        BTreeMap::from([("amaterasu.nvhttp".into(), "amaterasu.management".into())]),
        BTreeMap::new(),
        BTreeMap::from([("amaterasu.lutris".into(), "amaterasu.management".into())]),
    );
    let begin = backend.run_for(
        &mut pipeline,
        snapshot_begin("lutris-launch", 1),
        "amaterasu.lutris",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(begin["ok"], true, "{begin}");
    let batch = backend.run_for(
        &mut pipeline,
        snapshot_batch(
            "lutris-launch",
            begin["generation"].as_u64().unwrap(),
            1,
            0,
            begin["revision"].as_u64().unwrap(),
            vec![("42", entry("Lutris title"))],
        ),
        "amaterasu.lutris",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(batch["ok"], true, "{batch}");
    let finish = backend.run_for(
        &mut pipeline,
        snapshot_finish(
            "lutris-launch",
            begin["generation"].as_u64().unwrap(),
            1,
            batch["revision"].as_u64().unwrap(),
            1,
            true,
        ),
        "amaterasu.lutris",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(finish["ok"], true, "{finish}");
    let marker_key = "~provider.entry:amaterasu.lutris:42";
    let id = backend.entries[marker_key]["entry_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let resolve = |backend: &mut Backend, pipeline: &mut Pipeline, action: &str| {
        let initiator = if action == "resolve_launch" {
            "amaterasu.management"
        } else {
            "amaterasu.nvhttp"
        };
        backend.run_for(
            pipeline,
            json!({"action":action,"id":id}),
            "amaterasu.launch",
            initiator,
            "amaterasu.management",
        )
    };
    for action in ["resolve_launch", "resolve_exported_launch"] {
        let reply = resolve(&mut backend, &mut pipeline, action);
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(
            reply["provider_provenance"],
            json!({"provider":"amaterasu.lutris","source_id":"42"})
        );
    }
    backend.entries.get_mut(&id).unwrap()["name"] = json!("User edited Lutris title");
    backend.entries.get_mut(&id).unwrap()["tags"] = json!(["lutris"]);
    let detached = resolve(&mut backend, &mut pipeline, "resolve_launch");
    assert_eq!(detached["ok"], true, "{detached}");
    assert!(detached.get("provider_provenance").is_none());

    let mut forged = entry("Forged Lutris title");
    forged["tags"] = json!(["lutris"]);
    let manual_revision = backend.metadata["revision"].clone();
    let manual = backend.run_for(
        &mut pipeline,
        json!({"action":"create","expected_revision":manual_revision,"entry":forged}),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(manual["ok"], true, "{manual}");
    let manual_resolved = backend.run_for(
        &mut pipeline,
        json!({"action":"resolve_launch","id":manual["id"]}),
        "amaterasu.launch",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(manual_resolved["ok"], true, "{manual_resolved}");
    assert!(manual_resolved.get("provider_provenance").is_none());

    let mut noncanonical = backend.entries.remove(marker_key).unwrap();
    noncanonical["source_id"] = json!("042");
    backend
        .entries
        .insert("~provider.entry:amaterasu.lutris:042".into(), noncanonical);
    let malformed = resolve(&mut backend, &mut pipeline, "resolve_launch");
    assert_eq!(
        malformed["error"]["code"], "invalid_state_response",
        "{malformed}"
    );
}

fn playnite_entry(name: &str) -> Value {
    json!({
        "name":name,
        "launch":{"kind":"playnite","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}
    })
}

#[test]
fn playnite_revocation_fences_old_snapshot_and_preserves_unowned_rows() {
    let mut backend = Backend::default();
    let mut playnite = playnite_importer();
    let mut management = Pipeline::default();
    let manual = backend.run_for(
        &mut management,
        json!({"action":"create","entry":entry("Manual"),"expected_revision":0}),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(manual["ok"], true, "{manual}");
    let manual_id = manual["id"].as_str().unwrap().to_owned();
    let mut other_provider = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::from([("other.provider".into(), "amaterasu.management".into())]),
    );
    let other_begin = backend.run_for(
        &mut other_provider,
        snapshot_begin("other-scan", 1),
        "other.provider",
        "amaterasu.management",
        "amaterasu.management",
    );
    let other_batch = backend.run_for(
        &mut other_provider,
        snapshot_batch(
            "other-scan",
            other_begin["generation"].as_u64().unwrap(),
            1,
            0,
            other_begin["revision"].as_u64().unwrap(),
            vec![("other-game", entry("Other provider"))],
        ),
        "other.provider",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(other_batch["ok"], true, "{other_batch}");
    let other_id = backend.entries[&entry_key("other.provider", "other-game")]["entry_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let old_begin = backend.run_for(
        &mut playnite,
        snapshot_begin("old-process", 1),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(old_begin["provider_epoch"], 0);
    let old_generation = old_begin["generation"].as_u64().unwrap();
    let old_batch = backend.run_for(
        &mut playnite,
        snapshot_batch(
            "old-process",
            old_generation,
            1,
            0,
            old_begin["revision"].as_u64().unwrap(),
            vec![
                ("playnite:one", playnite_entry("First")),
                ("playnite:two", playnite_entry("Second")),
            ],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(old_batch["ok"], true, "{old_batch}");
    let first_id = backend.entries[&entry_key("amaterasu.playnite", "playnite:one")]["entry_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_id = backend.entries[&entry_key("amaterasu.playnite", "playnite:two")]["entry_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let detached = backend.run_for(
        &mut management,
        json!({"action":"delete","id":second_id,"expected_revision":old_batch["revision"]}),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(detached["ok"], true, "{detached}");

    let unauthorized = backend.run_for(
        &mut management,
        json!({"action":"provider_snapshot_revoke","revocation_id":"disabled-1","expected_provider_epoch":0}),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(unauthorized["error"]["code"], "permission_denied");
    let revoke = json!({"action":"provider_snapshot_revoke","revocation_id":"disabled-1","expected_provider_epoch":0});
    let revoked = backend.run_for(
        &mut playnite,
        revoke.clone(),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(revoked["provider_epoch"], 1, "{revoked}");
    assert_eq!(
        backend.entries[&state_key("amaterasu.playnite")]["provider_epoch"],
        1
    );
    assert_eq!(
        backend.entries[&state_key("amaterasu.playnite")]["revoked"],
        true
    );
    assert!(backend.entries.contains_key(&manual_id));
    assert!(backend.entries.contains_key(&other_id));
    assert!(backend.entries.contains_key(&first_id));
    assert!(!backend.entries.contains_key(&second_id));

    let replay = backend.run_for(
        &mut playnite,
        revoke,
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(replay["provider_epoch"], 1);
    assert_eq!(replay["replayed"], true);
    let conflicting = backend.run_for(
        &mut playnite,
        json!({"action":"provider_snapshot_revoke","revocation_id":"different-disable","expected_provider_epoch":0}),
        "amaterasu.playnite", "amaterasu.management", "amaterasu.management",
    );
    assert_eq!(conflicting["error"]["code"], "provider_epoch_conflict");

    let mut old_finish = snapshot_finish(
        "old-process",
        old_generation,
        1,
        old_batch["revision"].as_u64().unwrap(),
        1,
        true,
    );
    old_finish["remove_stale"] = json!(true);
    let stale = backend.run_for(
        &mut playnite,
        old_finish,
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(stale["ok"], false, "{stale}");
    assert!(!backend.entries.contains_key(&second_id));
    let old_replay = backend.run_for(
        &mut playnite,
        snapshot_begin("old-process", 1),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(old_replay["error"]["code"], "provider_epoch_conflict");

    let mut new_begin = snapshot_begin("new-process", 2);
    new_begin["provider_epoch"] = json!(1);
    let begun = backend.run_for(
        &mut playnite,
        new_begin,
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(begun["ok"], true, "{begun}");
    let mut new_batch = snapshot_batch(
        "new-process",
        begun["generation"].as_u64().unwrap(),
        2,
        0,
        begun["revision"].as_u64().unwrap(),
        vec![("playnite:one", playnite_entry("First"))],
    );
    new_batch["provider_epoch"] = json!(1);
    let batch = backend.run_for(
        &mut playnite,
        new_batch,
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(batch["ok"], true, "{batch}");
    let mut new_finish = snapshot_finish(
        "new-process",
        begun["generation"].as_u64().unwrap(),
        2,
        batch["revision"].as_u64().unwrap(),
        1,
        true,
    );
    new_finish["provider_epoch"] = json!(1);
    let finished = backend.run_for(
        &mut playnite,
        new_finish,
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(finished["ok"], true, "{finished}");
    assert!(backend.entries.contains_key(&manual_id));
    assert!(backend.entries.contains_key(&other_id));
    assert!(backend.entries.contains_key(&first_id));
    assert!(!backend.entries.contains_key(&second_id));
    assert!(
        !backend
            .entries
            .contains_key(&entry_key("amaterasu.playnite", "playnite:two"))
    );
}

#[test]
fn durable_revoke_wins_over_an_already_dispatched_finish_commit() {
    let mut backend = Backend::default();
    let mut old_pipeline = playnite_importer();
    let first = backend.run_for(
        &mut old_pipeline,
        snapshot_begin("first-scan", 1),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    let first_batch = backend.run_for(
        &mut old_pipeline,
        snapshot_batch(
            "first-scan",
            first["generation"].as_u64().unwrap(),
            1,
            0,
            first["revision"].as_u64().unwrap(),
            vec![("playnite:stale", playnite_entry("Still installed"))],
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    let first_finish = backend.run_for(
        &mut old_pipeline,
        snapshot_finish(
            "first-scan",
            first["generation"].as_u64().unwrap(),
            1,
            first_batch["revision"].as_u64().unwrap(),
            1,
            true,
        ),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(first_finish["ok"], true, "{first_finish}");
    let stale_id = backend.entries[&entry_key("amaterasu.playnite", "playnite:stale")]["entry_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second = backend.run_for(
        &mut old_pipeline,
        snapshot_begin("old-process-empty-scan", 2),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(second["ok"], true, "{second}");
    let finish_request = snapshot_finish(
        "old-process-empty-scan",
        second["generation"].as_u64().unwrap(),
        2,
        second["revision"].as_u64().unwrap(),
        0,
        true,
    );

    let mut incoming = Some(finish_request);
    let mut state_result = None;
    let mut held_commit = None;
    for _ in 0..100 {
        old_pipeline
            .tick(|command| {
                Ok(match command {
                    Command::Poll {} => {
                        incoming
                            .take()
                            .map_or(Response::Pending, |payload| Response::Request {
                                id: 71,
                                caller: "amaterasu.playnite".into(),
                                initiator: "amaterasu.management".into(),
                                service: crate::SERVICE.into(),
                                remaining_ms: 30_000,
                                payload,
                            })
                    }
                    Command::Request { payload, .. } => {
                        if payload["action"] == "collection_commit" {
                            held_commit = Some(payload);
                        } else {
                            state_result = Some(backend.handle(payload));
                        }
                        Response::Accepted { id: 72 }
                    }
                    Command::TakeResult { .. } => Response::Result {
                        payload: state_result.take().expect("scan result"),
                    },
                    command => panic!("unexpected command before held commit: {command:?}"),
                })
            })
            .unwrap();
        if held_commit.is_some() {
            break;
        }
    }
    let held_commit = held_commit.expect("old Finish dispatched a stale cleanup commit");
    assert!(
        held_commit["edits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edit| { edit["key"] == stale_id && edit["value"].is_null() })
    );

    let mut replacement_pipeline = playnite_importer();
    let revoked = backend.run_for(
        &mut replacement_pipeline,
        json!({"action":"provider_snapshot_revoke","revocation_id":"process-replaced","expected_provider_epoch":0}),
        "amaterasu.playnite", "amaterasu.management", "amaterasu.management",
    );
    assert_eq!(revoked["provider_epoch"], 1, "{revoked}");
    assert!(backend.entries.contains_key(&stale_id));

    let mut completion = None;
    old_pipeline
        .tick(|command| {
            Ok(match command {
                Command::TakeResult { .. } => Response::Result {
                    payload: backend.handle(held_commit.clone()),
                },
                Command::Poll {} => Response::Pending,
                Command::Complete { payload, .. } => {
                    completion = Some(payload);
                    Response::Ok
                }
                command => panic!("unexpected command after revoke: {command:?}"),
            })
        })
        .unwrap();
    assert_eq!(completion.unwrap()["error"]["code"], "revision_conflict");
    assert!(backend.entries.contains_key(&stale_id));
    assert_eq!(
        backend.entries[&state_key("amaterasu.playnite")]["provider_epoch"],
        1
    );
    let mut reloaded = playnite_importer();
    let inventory = backend.run_for(
        &mut reloaded,
        snapshot_inventory(None, None, 32),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(inventory["provider_epoch"], 1, "{inventory}");
}

#[test]
fn queued_revoke_precedes_an_older_undispatched_playnite_finish() {
    let mut pipeline = playnite_importer();
    pipeline.queued.push_back(Incoming {
        request_id: 81,
        caller: PLAYNITE_IMPORTER.into(),
        key: "principal/amaterasu.management".into(),
        catalog_owner: None,
        operation: serde_json::from_value(snapshot_finish("old-scan", 1, 1, 1, 0, true)).unwrap(),
        deadline: Instant::now() + Duration::from_secs(30),
    });
    pipeline.receive(&mut |command| {
        Ok(match command {
            Command::Poll {} => Response::Request {
                id: 82,
                caller: PLAYNITE_IMPORTER.into(),
                initiator: "amaterasu.management".into(),
                service: crate::SERVICE.into(),
                remaining_ms: 30_000,
                payload: json!({"action":"provider_snapshot_revoke","revocation_id":"replacement","expected_provider_epoch":0}),
            },
            command => panic!("unexpected command: {command:?}"),
        })
    }).unwrap();
    assert_eq!(pipeline.queued.len(), 2);
    assert!(matches!(
        pipeline.queued[0].operation,
        Operation::ProviderSnapshotRevoke { .. }
    ));
    assert!(matches!(
        pipeline.queued[1].operation,
        Operation::ProviderSnapshotFinish { .. }
    ));
}

#[test]
fn first_boot_revoke_counts_durable_state_once_across_reload_and_begin() {
    let mut backend = Backend::default();
    let mut playnite = playnite_importer();
    let revoked = backend.run_for(
        &mut playnite,
        json!({"action":"provider_snapshot_revoke","revocation_id":"first-disabled","expected_provider_epoch":0}),
        PLAYNITE_IMPORTER, "amaterasu.management", "amaterasu.management",
    );
    assert_eq!(revoked["provider_epoch"], 1, "{revoked}");
    assert_eq!(backend.metadata["provider_count"], 1);
    assert_eq!(backend.entries.len(), 1);

    let mut reloaded = playnite_importer();
    let inventory = backend.run_for(
        &mut reloaded,
        snapshot_inventory(None, None, 32),
        PLAYNITE_IMPORTER,
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(inventory["ok"], true, "{inventory}");
    assert_eq!(inventory["provider_epoch"], 1);
    let mut begin = snapshot_begin("first-enabled-process", 1);
    begin["provider_epoch"] = json!(1);
    let begun = backend.run_for(
        &mut reloaded,
        begin,
        PLAYNITE_IMPORTER,
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(begun["ok"], true, "{begun}");
    assert_eq!(begun["provider_epoch"], 1);
    assert_eq!(backend.metadata["provider_count"], 1);
    assert_eq!(backend.entries.len(), 1);
    assert_eq!(
        backend.entries[&state_key(PLAYNITE_IMPORTER)]["revoked"],
        false
    );
    let mut batch_request = snapshot_batch(
        "first-enabled-process",
        begun["generation"].as_u64().unwrap(),
        1,
        0,
        begun["revision"].as_u64().unwrap(),
        vec![("playnite:first", playnite_entry("First"))],
    );
    batch_request["provider_epoch"] = json!(1);
    let batch = backend.run_for(
        &mut reloaded,
        batch_request,
        PLAYNITE_IMPORTER,
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(batch["ok"], true, "{batch}");
    let mut finish_request = snapshot_finish(
        "first-enabled-process",
        begun["generation"].as_u64().unwrap(),
        1,
        batch["revision"].as_u64().unwrap(),
        1,
        true,
    );
    finish_request["provider_epoch"] = json!(1);
    let finished = backend.run_for(
        &mut reloaded,
        finish_request,
        PLAYNITE_IMPORTER,
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(finished["ok"], true, "{finished}");
    assert_eq!(finished["provider_epoch"], 1);
    assert_eq!(backend.metadata["provider_count"], 2);
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

fn update_provider_config_overrides(
    id: &str,
    expected_revision: u64,
    config_overrides: Value,
) -> Value {
    json!({
        "action":"update_provider_overrides",
        "id":id,
        "expected_revision":expected_revision,
        "config_overrides":config_overrides,
    })
}

fn update_provider_overrides(id: &str, expected_revision: u64, fec_percentage: Value) -> Value {
    update_provider_config_overrides(
        id,
        expected_revision,
        json!({"fec_percentage":fec_percentage}),
    )
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
    assert_eq!(marker.config_overrides.max_bitrate, None);
    assert_eq!(marker.config_overrides.minimum_fps_target, None);
    assert_eq!(marker.config_overrides.video_max_batch_size_kb, None);

    let mut old_marker_value = serde_json::to_value(marker).unwrap();
    old_marker_value["config_overrides"] = json!({"fec_percentage":37});
    let old_marker_with_overrides: Marker = serde_json::from_value(old_marker_value).unwrap();
    assert_eq!(
        old_marker_with_overrides.config_overrides.fec_percentage,
        Some(37)
    );
    assert_eq!(old_marker_with_overrides.config_overrides.max_bitrate, None);
    assert_eq!(
        old_marker_with_overrides
            .config_overrides
            .minimum_fps_target,
        None
    );
    assert_eq!(
        old_marker_with_overrides
            .config_overrides
            .video_max_batch_size_kb,
        None
    );
    assert!(
        old_marker_with_overrides
            .config_overrides
            .validate()
            .is_ok()
    );

    let mut persisted_marker = old_marker_with_overrides;
    persisted_marker.config_overrides.max_bitrate = Some(42_000);
    persisted_marker.config_overrides.minimum_fps_target = Some(59.94);
    persisted_marker.config_overrides.video_max_batch_size_kb = Some(32);
    let stored_marker = marker_value(&persisted_marker);
    let restored_marker: Marker = serde_json::from_value(stored_marker).unwrap();
    assert!(validate_marker(
        &restored_marker,
        "amaterasu.playnite",
        "playnite:10"
    ));
    assert_eq!(restored_marker.config_overrides.max_bitrate, Some(42_000));
    assert_eq!(
        restored_marker.config_overrides.minimum_fps_target,
        Some(59.94)
    );
    assert_eq!(
        restored_marker.config_overrides.video_max_batch_size_kb,
        Some(32)
    );
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
                "prep-cmd": [
                    {
                        "do": "  configure --profile=\"two words\"  ",
                        "undo": " restore --profile=\"two words\" ",
                        "elevated": true
                    }
                ],
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
    assert_eq!(
        stored["prep_cmd"],
        json!([{
            "do":"  configure --profile=\"two words\"  ",
            "undo":" restore --profile=\"two words\" ",
            "elevated":true
        }])
    );
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
    let pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::from(["launch".into()]),
        BTreeMap::new(),
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
    assert!(matches!(operation, Operation::ResolveLaunch { .. }));
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
    let mut pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::new(),
        BTreeMap::from([("consumer".into(), "owner".into())]),
        BTreeMap::new(),
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
fn list_and_search_replies_advertise_override_capabilities_without_changing_exports() {
    let mut backend = Backend::default();
    let mut ordinary_pipeline = Pipeline::default();
    let created = backend.run(
        &mut ordinary_pipeline,
        json!({
            "action":"create",
            "expected_revision":0,
            "entry":{
                "name":"Capability game",
                "launch":{"kind":"application","path":"/Applications/Game.app"}
            }
        }),
    );
    assert_eq!(created["ok"], true, "{created}");

    let expected_capabilities = if cfg!(windows) {
        json!({
            "frame_limiter_provider":["auto","rtss","nvidia-control-panel","none"],
            "rtss_frame_limit_type":["async","front edge sync","back edge sync","nvidia reflex"]
        })
    } else if cfg!(target_os = "linux") {
        json!({
            "frame_limiter_provider":["auto","mangohud","proton","mangohud-proton","none"],
            "rtss_frame_limit_type":[]
        })
    } else {
        json!({
            "frame_limiter_provider":["auto"],
            "rtss_frame_limit_type":[]
        })
    };

    let list = backend.run(&mut ordinary_pipeline, json!({"action":"list"}));
    assert_eq!(list["config_override_capabilities"], expected_capabilities);
    assert_eq!(list["entries"].as_array().unwrap().len(), 1);
    assert_eq!(list["entries"][0]["id"], created["id"]);

    let search = backend.run(
        &mut ordinary_pipeline,
        json!({"action":"search","query":"capability"}),
    );
    assert_eq!(
        search["config_override_capabilities"],
        expected_capabilities
    );
    assert_eq!(search["entries"].as_array().unwrap().len(), 1);
    assert_eq!(search["entries"][0]["id"], created["id"]);

    let mut export_pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::new(),
        BTreeMap::from([("exporter".into(), "caller.one".into())]),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let export = backend.run_for(
        &mut export_pipeline,
        json!({"action":"export_list"}),
        "exporter",
        "unrelated",
        "caller.one",
    );
    assert_eq!(
        export,
        json!({
            "ok":true,
            "revision":1,
            "entries":[{"id":created["id"],"name":"Capability game"}],
            "next_cursor":null
        })
    );
}

#[test]
fn steam_id_export_authority_uses_fixed_owner_and_rejects_payload_spoofing() {
    let mut pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::new(),
        BTreeMap::from([("nvhttp".into(), "management".into())]),
        BTreeMap::from([("consumer".into(), "steam-owner".into())]),
        BTreeMap::new(),
    );
    let mut backend = Backend::default();
    let denied = backend.run_for(
        &mut pipeline,
        json!({"action":"export_steam_ids"}),
        "stranger",
        "consumer",
        "steam-owner",
    );
    assert_eq!(denied["error"]["code"], "permission_denied");

    let spoofed = backend.run_for(
        &mut pipeline,
        json!({"action":"export_steam_ids","owner":"victim"}),
        "consumer",
        "owner",
        "steam-owner",
    );
    assert_eq!(spoofed["error"]["code"], "invalid_request");

    let empty = backend.run_for(
        &mut pipeline,
        json!({"action":"export_steam_ids"}),
        "consumer",
        "unrelated",
        "steam-owner",
    );
    assert_eq!(
        empty,
        json!({"ok":true,"revision":0,"steam_ids":[],"next_cursor":null})
    );
    assert_eq!(backend.revision, 0);

    let export_list_denied = backend.run_for(
        &mut pipeline,
        json!({"action":"export_list"}),
        "consumer",
        "unrelated",
        "steam-owner",
    );
    assert_eq!(export_list_denied["error"]["code"], "permission_denied");

    let generic_export_cannot_export_steam_ids = backend.run_for(
        &mut pipeline,
        json!({"action":"export_steam_ids"}),
        "nvhttp",
        "unrelated",
        "management",
    );
    assert_eq!(
        generic_export_cannot_export_steam_ids["error"]["code"],
        "permission_denied"
    );

    let existing_export_authority_is_unchanged = backend.run_for(
        &mut pipeline,
        json!({"action":"export_list"}),
        "nvhttp",
        "unrelated",
        "management",
    );
    assert_eq!(
        existing_export_authority_is_unchanged["ok"], true,
        "the Steam-ID map must not replace existing export_principals"
    );

    let (key, _, _) = pipeline
        .select_catalog(
            "consumer",
            "unrelated",
            Operation::ExportSteamIds {
                cursor: None,
                limit: None,
            },
        )
        .unwrap();
    assert_eq!(key, "principal/steam-owner");
}

#[test]
fn steam_id_export_pages_only_numeric_ids_and_rejects_stale_or_invalid_cursors() {
    for legacy in [false, true] {
        let mut entries = BTreeMap::new();
        for entry_number in 1..=70 {
            let mut value = entry(&format!("Steam game {entry_number}"));
            value["description"] = "private description".into();
            value["tags"] = json!(["private tag"]);
            if entry_number % 2 == 0 {
                let steam_id = if entry_number == 4 {
                    "2".to_owned()
                } else {
                    entry_number.to_string()
                };
                value["steam_id"] = steam_id.into();
            }
            entries.insert(format!("app-{entry_number:016x}"), value);
        }
        let mut backend = if legacy {
            Backend {
                legacy: Some(json!({
                    "revision":7,
                    "next_id":71,
                    "entries":entries
                })),
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
        let initial_state_revision = backend.revision;
        let mut pipeline = Pipeline::new_with_steam_id_export_principals(
            BTreeSet::new(),
            BTreeMap::new(),
            BTreeMap::from([("consumer".into(), "steam-owner".into())]),
            BTreeMap::new(),
        );
        let mut cursor = Value::Null;
        let mut first_cursor = Value::Null;
        let mut steam_ids = Vec::new();
        loop {
            let page = backend.run_for(
                &mut pipeline,
                json!({"action":"export_steam_ids","limit":32,"cursor":cursor}),
                "consumer",
                "unrelated",
                "steam-owner",
            );
            assert_eq!(page["ok"], true);
            assert_eq!(page["revision"], 7);
            assert_eq!(page.as_object().unwrap().len(), 4);
            assert!(page.get("entries").is_none());
            let page_ids = page["steam_ids"].as_array().unwrap();
            assert!(page_ids.len() <= 32);
            for steam_id in page_ids {
                assert!(steam_id.as_u64().is_some_and(|id| id > 0));
                steam_ids.push(steam_id.as_u64().unwrap());
            }
            cursor = page["next_cursor"].clone();
            if cursor.is_null() {
                break;
            }
            assert_eq!(cursor["revision"], 7);
            assert_eq!(cursor["query"], "steam_ids");
            if first_cursor.is_null() {
                first_cursor = cursor.clone();
            }
        }
        let expected = (2_u64..=70)
            .step_by(2)
            .map(|number| if number == 4 { 2 } else { number })
            .collect::<Vec<_>>();
        assert_eq!(steam_ids, expected);
        assert_eq!(backend.revision, initial_state_revision);

        if legacy {
            backend.legacy.as_mut().unwrap()["revision"] = 8.into();
        } else {
            backend.metadata["revision"] = 8.into();
        }
        let stale = backend.run_for(
            &mut pipeline,
            json!({"action":"export_steam_ids","cursor":first_cursor}),
            "consumer",
            "unrelated",
            "steam-owner",
        );
        assert_eq!(stale["error"]["code"], "revision_conflict");

        let invalid_query = backend.run_for(
            &mut pipeline,
            json!({
                "action":"export_steam_ids",
                "cursor":{"revision":8,"after":"app-0000000000000040","query":""}
            }),
            "consumer",
            "unrelated",
            "steam-owner",
        );
        assert_eq!(invalid_query["error"]["code"], "invalid_cursor");

        let invalid_limit = backend.run_for(
            &mut pipeline,
            json!({"action":"export_steam_ids","limit":33}),
            "consumer",
            "unrelated",
            "steam-owner",
        );
        assert_eq!(invalid_limit["error"]["code"], "invalid_request");
    }
}

#[test]
fn exported_launch_resolution_requires_both_authorities_and_returns_fixed_owner() {
    let pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::from(["launch".into()]),
        BTreeMap::from([("nvhttp".into(), "management".into())]),
        BTreeMap::new(),
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
fn exported_launch_resolution_returns_sparse_revisioned_preflight_rows() {
    let mut pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::from(["launch".into()]),
        BTreeMap::from([("nvhttp".into(), "management".into())]),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let id = "app-0000000000000001";
    let mut backend = Backend {
        revision: 4,
        metadata: json!({"revision":2,"next_id":2}),
        entries: BTreeMap::from([(
            id.into(),
            json!({
                "name":"First game",
                "launch":{"kind":"executable","program":"/games/first"},
                "config_overrides":{"frame_limiter_enable":false,"frame_limiter_fps_limit":0.0}
            }),
        )]),
        ..Default::default()
    };
    let resolve = json!({"action":"resolve_exported_launch","id":id});
    let first = backend.run_for(
        &mut pipeline,
        resolve.clone(),
        "launch",
        "nvhttp",
        "management",
    );
    assert_eq!(first["ok"], true);
    assert_eq!(first["revision"], 2);
    assert_eq!(first["id"], id);
    assert_eq!(first["catalog_owner"], "management");
    assert_eq!(
        first["entry"]["config_overrides"],
        json!({
            "frame_limiter_enable":false,
            "frame_limiter_fps_limit":0.0
        })
    );

    backend.entries.insert(
        id.into(),
        json!({
            "name":"Second game",
            "launch":{"kind":"executable","program":"/games/second"},
            "config_overrides":{"frame_limiter_enable":true}
        }),
    );
    backend.metadata["revision"] = json!(3);
    backend.revision += 1;
    let second = backend.run_for(&mut pipeline, resolve, "launch", "nvhttp", "management");
    assert_eq!(second["revision"], 3);
    assert_eq!(second["entry"]["launch"]["program"], "/games/second");
    assert_eq!(
        second["entry"]["config_overrides"],
        json!({"frame_limiter_enable":true})
    );
    assert_eq!(first["entry"]["launch"]["program"], "/games/first");
    assert_eq!(
        first["entry"]["config_overrides"]["frame_limiter_enable"],
        false
    );
}

#[test]
fn exported_launch_resolution_rejects_malformed_stored_frame_limiter() {
    let mut pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::from(["launch".into()]),
        BTreeMap::from([("nvhttp".into(), "management".into())]),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let id = "app-0000000000000001";
    let mut backend = Backend {
        revision: 1,
        metadata: json!({"revision":1,"next_id":2}),
        entries: BTreeMap::from([(
            id.into(),
            json!({
                "name":"Bad game",
                "launch":{"kind":"executable","program":"/games/bad"},
                "config_overrides":{"frame_limiter_enable":"false"}
            }),
        )]),
        ..Default::default()
    };
    let result = backend.run_for(
        &mut pipeline,
        json!({"action":"resolve_exported_launch","id":id}),
        "launch",
        "nvhttp",
        "management",
    );
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "invalid_state_response");
}

#[test]
fn exported_display_overrides_are_sparse_revisioned_and_strict() {
    let mut pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::from(["launch".into()]),
        BTreeMap::from([("nvhttp".into(), "management".into())]),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let id = "app-0000000000000001";
    let mut backend = Backend {
        revision: 1,
        metadata: json!({"revision":1,"next_id":2}),
        entries: BTreeMap::from([(
            id.into(),
            json!({
                "name":"Display game", "launch":{"kind":"executable","program":"/games/display"},
                "config_overrides":{"dd_resolution_option":"manual","dd_manual_resolution":"2560x1440","dd_hdr_option":null}
            }),
        )]),
        ..Default::default()
    };
    let resolve = json!({"action":"resolve_exported_launch","id":id});
    let first = backend.run_for(
        &mut pipeline,
        resolve.clone(),
        "launch",
        "nvhttp",
        "management",
    );
    assert_eq!(first["revision"], 1);
    assert_eq!(first["catalog_owner"], "management");
    assert_eq!(
        first["entry"]["config_overrides"],
        json!({"dd_resolution_option":"manual","dd_manual_resolution":"2560x1440"})
    );

    backend.entries.get_mut(id).unwrap()["config_overrides"] =
        json!({"dd_resolution_option":"disabled"});
    backend.metadata["revision"] = json!(2);
    backend.revision += 1;
    let second = backend.run_for(
        &mut pipeline,
        resolve.clone(),
        "launch",
        "nvhttp",
        "management",
    );
    assert_eq!(second["revision"], 2);
    assert_eq!(
        second["entry"]["config_overrides"],
        json!({"dd_resolution_option":"disabled"})
    );
    assert_eq!(
        first["entry"]["config_overrides"]["dd_resolution_option"],
        "manual"
    );

    backend.entries.get_mut(id).unwrap()["config_overrides"] =
        json!({"dd_configuration_option":"unknown"});
    assert_eq!(
        backend.run_for(&mut pipeline, resolve, "launch", "nvhttp", "management")["error"]["code"],
        "invalid_state_response"
    );
}

#[cfg(windows)]
#[test]
fn exported_launch_preflight_preserves_sparse_rtss_limit_types() {
    let mut pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::from(["launch".into()]),
        BTreeMap::from([("nvhttp".into(), "management".into())]),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let id = "app-0000000000000001";
    let mut backend = Backend {
        revision: 1,
        metadata: json!({"revision":1,"next_id":2}),
        ..Default::default()
    };
    for override_value in [
        json!({}),
        json!({"rtss_frame_limit_type":null}),
        json!({"rtss_frame_limit_type":"async"}),
        json!({"rtss_frame_limit_type":"front edge sync"}),
        json!({"rtss_frame_limit_type":"back edge sync"}),
        json!({"rtss_frame_limit_type":"nvidia reflex"}),
    ] {
        backend.entries.insert(
            id.into(),
            json!({
                "name":"RTSS game",
                "launch":{"kind":"executable","program":"C:/games/rtss.exe"},
                "config_overrides":override_value
            }),
        );
        let result = backend.run_for(
            &mut pipeline,
            json!({"action":"resolve_exported_launch","id":id}),
            "launch",
            "nvhttp",
            "management",
        );
        assert_eq!(result["ok"], true);
        assert_eq!(
            result["entry"]["config_overrides"]["rtss_frame_limit_type"],
            override_value["rtss_frame_limit_type"]
        );
        assert_eq!(
            result["entry"]["config_overrides"].is_null(),
            override_value["rtss_frame_limit_type"].is_null()
        );
    }
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
        let mut pipeline = Pipeline::new_with_steam_id_export_principals(
            BTreeSet::new(),
            BTreeMap::from([("consumer".into(), "owner".into())]),
            BTreeMap::new(),
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
    assert_eq!(imported["revision"], 1);
    let first_marker_key = "~provider.entry:amaterasu.playnite:playnite:10";
    let first_marker = backend.entries[first_marker_key].clone();
    let first_added_at = first_marker["added_at_unix"].as_i64().unwrap();
    assert_eq!(first_marker["source_flags"], 8);
    assert!(first_marker["config_overrides"]["nvenc_spatial_aq"].is_null());

    let finished = backend.run_for(
        &mut playnite,
        snapshot_finish("marker-time-1", generation, 1, 1, 1, true),
        "amaterasu.playnite",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(finished["ok"], true);
    let seeded = backend.run_for(
        &mut playnite,
        update_provider_config_overrides(
            "app-0000000000000001",
            finished["revision"].as_u64().unwrap(),
            json!({"fec_percentage":37,"nvenc_spatial_aq":false}),
        ),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(seeded["ok"], true);
    assert_eq!(seeded["config_overrides"]["nvenc_spatial_aq"], false);
    assert_eq!(
        backend.entries[first_marker_key]["config_overrides"]["nvenc_spatial_aq"],
        false
    );

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
            seeded["revision"].as_u64().unwrap(),
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
    assert_eq!(
        refreshed_marker["config_overrides"]["nvenc_spatial_aq"],
        false
    );
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
fn resolve_launch_preserves_exact_amd_av1_latency_mode_tokens() {
    let mut backend = Backend::default();
    let mut pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::from(["launch".into()]),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    );

    for (index, mode) in ["auto", "none", "power_saving", "realtime", "lowest"]
        .into_iter()
        .enumerate()
    {
        let mut catalog_entry = entry(&format!("AV1 {mode}"));
        catalog_entry["config_overrides"] = json!({"amd_av1_latency_mode":mode});
        let created = backend.run_for(
            &mut pipeline,
            json!({
                "action":"create",
                "expected_revision":index,
                "entry":catalog_entry
            }),
            "owner",
            "owner",
            "owner",
        );
        assert_eq!(created["ok"], true, "{created}");

        let resolved = backend.run_for(
            &mut pipeline,
            json!({"action":"resolve_launch","id":created["id"]}),
            "launch",
            "owner",
            "owner",
        );
        assert_eq!(resolved["ok"], true, "{resolved}");
        assert_eq!(
            resolved["entry"]["config_overrides"],
            json!({"amd_av1_latency_mode":mode})
        );
    }
}

#[test]
fn dd_hdr_request_override_persists_across_catalog_reload() {
    let mut backend = Backend::default();
    let mut pipeline = Pipeline::default();

    for (index, mode) in ["auto", "force_on", "force_off"].into_iter().enumerate() {
        let mut catalog_entry = entry(&format!("HDR request {mode}"));
        catalog_entry["config_overrides"] = json!({"dd_hdr_request_override":mode});
        let created = backend.run_for(
            &mut pipeline,
            json!({
                "action":"create",
                "expected_revision":index,
                "entry":catalog_entry
            }),
            "owner",
            "owner",
            "owner",
        );
        assert_eq!(created["ok"], true, "{created}");
        assert_eq!(
            backend.entries[created["id"].as_str().unwrap()]["config_overrides"]["dd_hdr_request_override"],
            mode
        );

        let reloaded = backend.run_for(
            &mut Pipeline::default(),
            json!({"action":"get","id":created["id"]}),
            "owner",
            "owner",
            "owner",
        );
        assert_eq!(reloaded["ok"], true, "{reloaded}");
        assert_eq!(
            reloaded["entry"]["config_overrides"]["dd_hdr_request_override"],
            mode
        );
    }
}

#[test]
fn native_pen_touch_override_is_sparse_strict_and_round_trips_through_catalog() {
    let cases = [
        ("omitted", None, None),
        ("null", Some(json!({"native_pen_touch":null})), None),
        (
            "false",
            Some(json!({"native_pen_touch":false})),
            Some(false),
        ),
        ("true", Some(json!({"native_pen_touch":true})), Some(true)),
    ];
    let mut backend = Backend::default();
    let mut pipeline = Pipeline::default();
    let mut revision = 0;

    for (name, config_overrides, expected) in cases {
        let mut catalog_entry = entry(name);
        if let Some(config_overrides) = config_overrides {
            catalog_entry["config_overrides"] = config_overrides;
        }

        let created = backend.run(
            &mut pipeline,
            json!({
                "action":"create",
                "expected_revision":revision,
                "entry":catalog_entry
            }),
        );
        assert_eq!(created["ok"], true, "{created}");
        revision = created["revision"].as_u64().unwrap();

        let id = created["id"].as_str().unwrap();
        let stored_entry = backend.entries[id].clone();
        let reloaded = backend.run(&mut Pipeline::default(), json!({"action":"get","id":id}));
        assert_eq!(reloaded["ok"], true, "{reloaded}");

        if let Some(expected) = expected {
            assert_eq!(
                stored_entry["config_overrides"]["native_pen_touch"],
                expected
            );
            assert_eq!(
                reloaded["entry"]["config_overrides"]["native_pen_touch"],
                expected
            );
        } else {
            assert!(stored_entry.get("config_overrides").is_none());
            assert!(reloaded["entry"].get("config_overrides").is_none());
        }
    }

    for invalid_value in [json!("false"), json!(0), json!({}), json!([])] {
        let mut invalid_entry = entry("Invalid native pen touch override");
        invalid_entry["config_overrides"] = json!({"native_pen_touch":invalid_value});
        assert!(serde_json::from_value::<crate::model::Entry>(invalid_entry).is_err());
    }
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
    configured_entry["exclude_global_prep_cmd"] = json!(true);
    configured_entry["config_overrides"] = json!({
        "fec_percentage": 37,
        "audio_sink_capture_only": false,
        "back_button_timeout": -1,
        "amd_av1_latency_mode": "auto",
    });
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
        backend.entries["app-0000000000000001"]["exclude_global_prep_cmd"],
        true
    );
    assert_eq!(
        backend.entries["app-0000000000000001"]["config_overrides"]["fec_percentage"],
        37
    );
    assert_eq!(
        backend.entries["app-0000000000000001"]["config_overrides"]["audio_sink_capture_only"],
        false
    );
    assert_eq!(
        backend.entries["app-0000000000000001"]["config_overrides"]["back_button_timeout"],
        -1
    );
    assert_eq!(
        backend.entries["app-0000000000000001"]["config_overrides"]["amd_av1_latency_mode"],
        "auto"
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
fn provider_override_preserves_explicit_zero_values_across_playnite_refresh_and_cleanup() {
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
    let config_overrides = json!({
        "fec_percentage":37,
        "max_bitrate":0,
        "minimum_fps_target":0.0,
        "video_max_batch_size_kb":0
    });
    let assert_override_values = |overrides: &Value| {
        assert_eq!(overrides["fec_percentage"], 37);
        assert_eq!(overrides["max_bitrate"], 0);
        assert_eq!(overrides["minimum_fps_target"], json!(0.0));
        assert_eq!(overrides["video_max_batch_size_kb"], 0);
    };

    let updated = backend.run_for(
        &mut pipeline,
        update_provider_config_overrides(id, 1, config_overrides.clone()),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(updated["ok"], true);
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["changed"], true);
    assert_eq!(updated["replayed"], false);
    assert_override_values(&updated["config_overrides"]);
    assert_override_values(&backend.entries[id]["config_overrides"]);
    assert_eq!(backend.entries[id]["launch"]["kind"], "playnite");
    assert_eq!(
        backend.entries[id]["launch"]["game_id"],
        "a1b2c3d4-1234-5678-9abc-0123456789ab"
    );
    let marker_key = "~provider.entry:amaterasu.playnite:playnite:10";
    assert_override_values(&backend.entries[marker_key]["config_overrides"]);
    assert_eq!(backend.entries[marker_key]["detached"], false);

    let repeated = backend.run_for(
        &mut pipeline,
        update_provider_config_overrides(id, 2, config_overrides.clone()),
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
    assert_override_values(&backend.entries[id]["config_overrides"]);
    assert_eq!(
        backend.entries[id]["launch"]["game_id"],
        "a1b2c3d4-1234-5678-9abc-0123456789ab"
    );
    assert_override_values(&backend.entries[marker_key]["config_overrides"]);
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
        update_provider_config_overrides(id, current_revision, config_overrides),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(after_delete["error"]["code"], "provider_entry_not_found");
}

#[test]
fn provider_refresh_rate_option_override_is_strict_and_survives_playnite_refresh() {
    let mut backend = Backend::default();
    let mut pipeline = playnite_importer();
    let finished = import_playnite_entry(
        &mut backend,
        &mut pipeline,
        "playnite-refresh-rate-1",
        1,
        0,
        "playnite:10",
        "Imported title",
    );
    assert_eq!(finished["ok"], true);

    let id = "app-0000000000000001";
    let marker_key = "~provider.entry:amaterasu.playnite:playnite:10";
    for invalid in [json!(1), json!(true), json!([]), json!({})] {
        let rejected = backend.run_for(
            &mut pipeline,
            update_provider_config_overrides(id, 1, json!({"dd_refresh_rate_option":invalid})),
            "amaterasu.management",
            "amaterasu.management",
            "amaterasu.management",
        );
        assert_eq!(rejected["error"]["code"], "invalid_request");
        assert_eq!(backend.metadata["revision"], 1);
        assert!(backend.entries[id].get("config_overrides").is_none());
        assert!(
            backend.entries[marker_key]["config_overrides"]
                .get("dd_refresh_rate_option")
                .is_none()
        );
    }

    let unsupported = backend.run_for(
        &mut pipeline,
        update_provider_config_overrides(id, 1, json!({"dd_refresh_rate_option":"unknown"})),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(unsupported["error"]["code"], "invalid_request");
    assert_eq!(backend.metadata["revision"], 1);

    let mut expected_revision = 1;
    for option in ["auto", "disabled", "manual", "prefer_highest"] {
        let updated = backend.run_for(
            &mut pipeline,
            update_provider_config_overrides(
                id,
                expected_revision,
                json!({"dd_refresh_rate_option":option}),
            ),
            "amaterasu.management",
            "amaterasu.management",
            "amaterasu.management",
        );
        assert_eq!(updated["ok"], true, "{updated}");
        assert_eq!(updated["changed"], true);
        assert_eq!(
            updated["config_overrides"]["dd_refresh_rate_option"],
            option
        );
        assert_eq!(
            backend.entries[id]["config_overrides"]["dd_refresh_rate_option"],
            option
        );
        assert_eq!(backend.entries[marker_key]["version"], 1);
        assert_eq!(
            backend.entries[marker_key]["config_overrides"]["dd_refresh_rate_option"],
            option
        );
        expected_revision = updated["revision"].as_u64().unwrap();
    }

    let refreshed = import_playnite_entry(
        &mut backend,
        &mut pipeline,
        "playnite-refresh-rate-2",
        2,
        expected_revision,
        "playnite:10",
        "Provider renamed title",
    );
    assert_eq!(refreshed["ok"], true, "{refreshed}");
    assert_eq!(backend.entries[id]["name"], "Provider renamed title");
    assert_eq!(
        backend.entries[id]["config_overrides"]["dd_refresh_rate_option"],
        "prefer_highest"
    );
    assert_eq!(backend.entries[marker_key]["version"], 1);
    assert_eq!(
        backend.entries[marker_key]["config_overrides"]["dd_refresh_rate_option"],
        "prefer_highest"
    );
}

#[test]
fn provider_dd_hdr_request_override_survives_playnite_refresh_without_marker_version_bump() {
    let mut backend = Backend::default();
    let mut pipeline = playnite_importer();
    let imported = import_playnite_entry(
        &mut backend,
        &mut pipeline,
        "playnite-dd-hdr-1",
        1,
        0,
        "playnite:10",
        "Imported title",
    );
    assert_eq!(imported["ok"], true);

    let id = "app-0000000000000001";
    let marker_key = "~provider.entry:amaterasu.playnite:playnite:10";
    let updated = backend.run_for(
        &mut pipeline,
        update_provider_config_overrides(id, 1, json!({"dd_hdr_request_override":"force_off"})),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(updated["ok"], true, "{updated}");
    assert_eq!(
        updated["config_overrides"]["dd_hdr_request_override"],
        "force_off"
    );
    assert_eq!(
        backend.entries[id]["config_overrides"]["dd_hdr_request_override"],
        "force_off"
    );
    assert_eq!(backend.entries[marker_key]["version"], 1);
    assert_eq!(
        backend.entries[marker_key]["config_overrides"]["dd_hdr_request_override"],
        "force_off"
    );

    let refreshed = import_playnite_entry(
        &mut backend,
        &mut pipeline,
        "playnite-dd-hdr-2",
        2,
        updated["revision"].as_u64().unwrap(),
        "playnite:10",
        "Provider renamed title",
    );
    assert_eq!(refreshed["ok"], true, "{refreshed}");
    assert_eq!(backend.entries[id]["name"], "Provider renamed title");
    assert_eq!(
        backend.entries[id]["config_overrides"]["dd_hdr_request_override"],
        "force_off"
    );
    assert_eq!(backend.entries[marker_key]["version"], 1);
    assert_eq!(
        backend.entries[marker_key]["config_overrides"]["dd_hdr_request_override"],
        "force_off"
    );
}

#[test]
fn provider_prefer_10bit_sdr_override_survives_playnite_refresh() {
    let mut backend = Backend::default();
    let mut pipeline = playnite_importer();
    let finished = import_playnite_entry(
        &mut backend,
        &mut pipeline,
        "playnite-prefer-10bit-1",
        1,
        0,
        "playnite:10",
        "Imported title",
    );
    assert_eq!(finished["ok"], true);

    let id = "app-0000000000000001";
    let marker_key = "~provider.entry:amaterasu.playnite:playnite:10";
    let mut expected_revision = finished["revision"].as_u64().unwrap();

    for (index, prefer_10bit_sdr) in [true, false].into_iter().enumerate() {
        let updated = backend.run_for(
            &mut pipeline,
            update_provider_config_overrides(
                id,
                expected_revision,
                json!({"prefer_10bit_sdr":prefer_10bit_sdr}),
            ),
            "amaterasu.management",
            "amaterasu.management",
            "amaterasu.management",
        );
        assert_eq!(updated["ok"], true, "{updated}");
        assert_eq!(
            updated["config_overrides"]["prefer_10bit_sdr"],
            prefer_10bit_sdr
        );
        assert_eq!(
            backend.entries[id]["config_overrides"]["prefer_10bit_sdr"],
            prefer_10bit_sdr
        );
        assert_eq!(backend.entries[marker_key]["version"], 1);
        assert_eq!(
            backend.entries[marker_key]["config_overrides"]["prefer_10bit_sdr"],
            prefer_10bit_sdr
        );
        expected_revision = updated["revision"].as_u64().unwrap();

        let source_revision = (index + 2) as u64;
        let refreshed = import_playnite_entry(
            &mut backend,
            &mut pipeline,
            &format!("playnite-prefer-10bit-{}", index + 2),
            source_revision,
            expected_revision,
            "playnite:10",
            "Provider renamed title",
        );
        assert_eq!(refreshed["ok"], true, "{refreshed}");
        assert_eq!(backend.entries[id]["name"], "Provider renamed title");
        assert_eq!(
            backend.entries[id]["config_overrides"]["prefer_10bit_sdr"],
            prefer_10bit_sdr
        );
        assert_eq!(backend.entries[marker_key]["version"], 1);
        assert_eq!(
            backend.entries[marker_key]["config_overrides"]["prefer_10bit_sdr"],
            prefer_10bit_sdr
        );
        expected_revision = refreshed["revision"].as_u64().unwrap();
    }
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
        provider_epoch: 0,
        last_revocation_id: None,
        revoked: false,
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
    let mut pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::new(),
        BTreeMap::new(),
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
fn lutris_artwork_requires_authenticated_matching_importer_and_source() {
    let mut backend = Backend::default();
    let mut pipeline = lutris_artwork_importer();
    let revision = "a".repeat(64);
    for (caller, source_id) in [
        ("other.provider", "42"),
        ("amaterasu.steam", "42"),
        ("amaterasu.lutris", "43"),
        ("amaterasu.lutris", "042"),
        ("amaterasu.lutris", "lutris:42"),
    ] {
        let sync_id = format!("forged-{caller}-{source_id}");
        let begin = backend.run_for(
            &mut pipeline,
            snapshot_begin(&sync_id, 1),
            caller,
            "amaterasu.management",
            "amaterasu.management",
        );
        assert_eq!(begin["ok"], true, "{begin}");
        let rejected = backend.run_for(
            &mut pipeline,
            snapshot_batch(
                &sync_id,
                begin["generation"].as_u64().unwrap(),
                1,
                0,
                begin["revision"].as_u64().unwrap(),
                vec![(source_id, lutris_artwork_entry("Forged", 42, &revision))],
            ),
            caller,
            "amaterasu.management",
            "amaterasu.management",
        );
        assert_eq!(rejected["error"]["code"], "provider_artwork_required");
    }
    assert!(!backend.entries.contains_key("app-0000000000000001"));
}

#[test]
fn lutris_artwork_revision_persists_without_changing_other_rows() {
    let mut backend = Backend::default();
    let mut pipeline = lutris_artwork_importer();
    let revision_a = "a".repeat(64);
    let revision_b = "b".repeat(64);
    let manual = backend.run_for(
        &mut pipeline,
        json!({"action":"create","expected_revision":0,"entry":entry("Manual game")}),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(manual["ok"], true, "{manual}");
    let manual_id = manual["id"].as_str().unwrap().to_owned();
    let manual_before = backend.entries[&manual_id].clone();
    run_steam_artwork_snapshot(
        &mut backend,
        &mut pipeline,
        "steam-unchanged",
        1,
        Some("17"),
    );
    let steam_id = "app-0000000000000002";
    let steam_before = backend.entries[steam_id].clone();

    for (sync_id, source_revision, digest) in [
        ("lutris-artwork-a", 1, &revision_a),
        ("lutris-artwork-b", 2, &revision_b),
    ] {
        let begin = backend.run_for(
            &mut pipeline,
            snapshot_begin(sync_id, source_revision),
            "amaterasu.lutris",
            "amaterasu.management",
            "amaterasu.management",
        );
        assert_eq!(begin["ok"], true, "{begin}");
        let batch = backend.run_for(
            &mut pipeline,
            snapshot_batch(
                sync_id,
                begin["generation"].as_u64().unwrap(),
                source_revision,
                0,
                begin["revision"].as_u64().unwrap(),
                vec![("42", lutris_artwork_entry("Lutris game", 42, digest))],
            ),
            "amaterasu.lutris",
            "amaterasu.management",
            "amaterasu.management",
        );
        assert_eq!(batch["ok"], true, "{batch}");
        let finish = backend.run_for(
            &mut pipeline,
            snapshot_finish(
                sync_id,
                begin["generation"].as_u64().unwrap(),
                source_revision,
                batch["revision"].as_u64().unwrap(),
                1,
                true,
            ),
            "amaterasu.lutris",
            "amaterasu.management",
            "amaterasu.management",
        );
        assert_eq!(finish["ok"], true, "{finish}");
    }

    let lutris_id = "app-0000000000000003";
    assert_eq!(
        backend.entries[lutris_id]["artwork"]["revision"],
        revision_b
    );
    assert_eq!(backend.entries[&manual_id], manual_before);
    assert_eq!(backend.entries[steam_id], steam_before);
    let mut reloaded = lutris_artwork_importer();
    let readback = backend.run_for(
        &mut reloaded,
        json!({"action":"get","id":lutris_id}),
        "amaterasu.management",
        "amaterasu.management",
        "amaterasu.management",
    );
    assert_eq!(readback["ok"], true, "{readback}");
    assert_eq!(readback["entry"]["artwork"]["revision"], revision_b);
}

#[test]
fn steam_runtime_metadata_requires_matching_importer_identity_but_manual_crud_can_set_it() {
    let mut backend = Backend::default();
    let mut pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::new(),
        BTreeMap::new(),
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
