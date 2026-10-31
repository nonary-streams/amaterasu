use super::*;
use crate::store::StateStore;

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "amaterasu-collection-{}",
            amaterasu_storage::random_identifier().unwrap()
        )))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn entry(index: usize) -> Value {
    json!({"name":format!("Game {index}"),"description":"manual library","launch":{"kind":"application","path":"/Applications/Game.app"}})
}
fn bounded_handle(store: &StateStore, caller: &str, payload: Value) -> Value {
    assert!(
        serde_json::to_vec(&amaterasu_module_sdk::Command::Request {
            service: "amaterasu.state/1".into(),
            payload: payload.clone(),
            timeout_ms: 30_000,
            operation_id: "test".into(),
            parent: Some(7)
        })
        .unwrap()
        .len()
            < 16_384
    );
    let result = store.handle(caller, payload);
    assert!(
        serde_json::to_vec(&amaterasu_module_sdk::Response::Result {
            payload: result.clone()
        })
        .unwrap()
        .len()
            < 16_384
    );
    result
}
#[test]
fn ten_thousand_durable_entries_survive_restart_with_bounded_transport_and_isolation() {
    let directory = Directory::new();
    let store = StateStore::open(directory.0.clone()).unwrap();
    for batch in 0..200 {
        let edits: Vec<_> = (batch * 50 + 1..=batch * 50 + 50)
            .map(|index| json!({"key":format!("app-{index:016x}"),"value":entry(index)}))
            .collect();
        let result = bounded_handle(
            &store,
            "catalog",
            json!({"action":"collection_commit","collection":"principal/alice","expected_revision":batch,"metadata":{"revision":batch + 1,"next_id":batch * 50 + 51},"edits":edits}),
        );
        assert_eq!(result["revision"], batch + 1, "{result}");
    }
    drop(store);
    let store = StateStore::open(directory.0.clone()).unwrap();
    let metadata = bounded_handle(
        &store,
        "catalog",
        json!({"action":"collection_get","collection":"principal/alice"}),
    );
    assert_eq!(metadata["count"], 10_000);
    let mut after = Value::Null;
    let mut count = 0;
    loop {
        let page = bounded_handle(
            &store,
            "catalog",
            json!({"action":"collection_page","collection":"principal/alice","revision":200,"after":after,"limit":128}),
        );
        let rows = page["entries"].as_array().unwrap();
        for row in rows {
            count += 1;
            assert_eq!(row["key"], format!("app-{count:016x}"));
        }
        after = page["next"].clone();
        if after.is_null() {
            break;
        }
    }
    assert_eq!(count, 10_000);
    for (caller, collection) in [("other", "principal/alice"), ("catalog", "principal/bob")] {
        assert_eq!(
            store.handle(
                caller,
                json!({"action":"collection_get","collection":collection})
            )["found"],
            false
        );
    }
    let conflict = store.handle("catalog", json!({"action":"collection_commit","collection":"principal/alice","expected_revision":199,"metadata":{},"edits":[]}));
    assert_eq!(conflict["error"]["code"], "revision_conflict");
    assert_eq!(conflict["error"]["current_revision"], 200);
    let stale = store.handle(
        "catalog",
        json!({"action":"collection_page","collection":"principal/alice","revision":199,"limit":1}),
    );
    assert_eq!(stale["error"]["code"], "revision_conflict");
}
#[test]
fn interrupted_commit_keeps_old_manifest_and_retry_reclaims_orphan_shards() {
    let directory = Directory::new();
    amaterasu_storage::create_private_directory(&directory.0).unwrap();
    let namespace = directory.0.join("namespace").join("state.json");
    let open = || Collection::open(&directory.0, &namespace, "apps").unwrap();
    open()
        .commit(
            0,
            json!({"revision":1}),
            vec![Edit {
                key: "app-0000000000000001".into(),
                value: Some(entry(1)),
            }],
        )
        .unwrap();
    let mut interrupted = open();
    interrupted.fail_before_manifest = true;
    let orphan = interrupted.shard_path(2, 0);
    assert!(
        interrupted
            .commit(
                1,
                json!({"revision":2}),
                vec![Edit {
                    key: "app-0000000000000002".into(),
                    value: Some(entry(2))
                }]
            )
            .is_err()
    );
    assert!(orphan.exists());
    let reloaded = open();
    assert_eq!(
        reloaded.get(Some("app-0000000000000001")).unwrap()["value"],
        entry(1)
    );
    assert!(reloaded.get(Some("app-0000000000000002")).unwrap()["value"].is_null());
    assert_eq!(reloaded.get(None).unwrap()["revision"], 1);
    reloaded.commit(1, json!({"revision":2}), vec![]).unwrap();
    assert!(!orphan.exists());
    assert_eq!(open().get(None).unwrap()["count"], 1);
}
#[test]
fn oversized_entry_is_rejected_before_commit_and_large_pages_fit_transport() {
    let directory = Directory::new();
    let store = StateStore::open(directory.0.clone()).unwrap();
    let oversized = store.handle("catalog", json!({"action":"collection_commit","collection":"apps","expected_revision":0,"metadata":{},"edits":[{"key":"one","value":"x".repeat(12_000)}]}));
    assert_eq!(oversized["error"]["code"], "value_too_large");
    for revision in 0..3 {
        assert_eq!(
            bounded_handle(
                &store,
                "catalog",
                json!({"action":"collection_commit","collection":"apps","expected_revision":revision,"metadata":{},"edits":[{"key":format!("{revision}"),"value":"x".repeat(11_800)}]})
            )["revision"],
            revision + 1
        );
    }
    let page = bounded_handle(
        &store,
        "catalog",
        json!({"action":"collection_page","collection":"apps","revision":3,"limit":128}),
    );
    assert_eq!(page["entries"].as_array().unwrap().len(), 1);
    assert!(page["next"].is_string());
}
#[cfg(unix)]
#[test]
fn collection_parent_symlinks_cannot_escape_namespace() {
    let directory = Directory::new();
    let outside = Directory::new();
    std::fs::create_dir_all(directory.0.join("namespace")).unwrap();
    std::fs::create_dir_all(&outside.0).unwrap();
    std::os::unix::fs::symlink(&outside.0, directory.0.join("namespace/collections")).unwrap();
    assert!(
        Collection::open(
            &directory.0,
            &directory.0.join("namespace/state.json"),
            "apps"
        )
        .is_err()
    );
}

#[test]
fn indexed_pages_touch_only_relevant_leaves_and_deletion_preserves_ranges() {
    let directory = Directory::new();
    amaterasu_storage::create_private_directory(&directory.0).unwrap();
    let namespace = directory.0.join("namespace/state.json");
    let open = || Collection::open(&directory.0, &namespace, "apps").unwrap();
    for batch in 0..20 {
        open()
            .commit(
                batch,
                json!({}),
                (batch * 100..batch * 100 + 100)
                    .map(|index| Edit {
                        key: format!("key-{index:08}"),
                        value: Some(json!({"name":format!("Game {index}")})),
                    })
                    .collect(),
            )
            .unwrap();
    }
    let collection = open();
    assert!(collection.manifest.shards.len() > 8);
    let page = collection.page(20, Some("key-00001000"), 128).unwrap();
    assert_eq!(page["entries"][0]["key"], "key-00001001");
    assert!(collection.loaded_shards.get() <= 2);
    drop(collection);
    // Removing a complete leaf drops its range; neighboring leaves continue to
    // own subsequent inserts, including keys below the previous first key.
    let collection = open();
    let bound = collection.manifest.shards.keys().nth(3).unwrap().clone();
    let keys: Vec<_> = collection.load(&bound).unwrap().keys().cloned().collect();
    let mut revision = 20;
    for chunk in keys.chunks(128) {
        open()
            .commit(
                revision,
                json!({}),
                chunk
                    .iter()
                    .map(|key| Edit {
                        key: key.clone(),
                        value: None,
                    })
                    .collect(),
            )
            .unwrap();
        revision += 1;
    }
    assert!(!open().manifest.shards.contains_key(&bound));
    open()
        .commit(
            revision,
            json!({}),
            vec![
                Edit {
                    key: "aaa".into(),
                    value: Some(json!("first")),
                },
                Edit {
                    key: bound.clone(),
                    value: Some(json!("reinserted")),
                },
            ],
        )
        .unwrap();
    assert_eq!(open().get(Some("aaa")).unwrap()["value"], "first");
    assert_eq!(open().get(Some(&bound)).unwrap()["value"], "reinserted");
}

#[test]
fn corrupt_manifest_counts_and_leaf_counts_return_errors_without_mutating() {
    let directory = Directory::new();
    amaterasu_storage::create_private_directory(&directory.0).unwrap();
    let namespace = directory.0.join("namespace/state.json");
    Collection::open(&directory.0, &namespace, "apps")
        .unwrap()
        .commit(
            0,
            json!({}),
            vec![Edit {
                key: "one".into(),
                value: Some(json!(1)),
            }],
        )
        .unwrap();
    let collection = Collection::open(&directory.0, &namespace, "apps").unwrap();
    let path = collection.directory.join("manifest.json");
    let valid = std::fs::read(&path).unwrap();
    let mut manifest: Value = serde_json::from_slice(&valid).unwrap();
    manifest["count"] = json!(0);
    amaterasu_storage::write_json_atomically(&path, &manifest).unwrap();
    assert!(Collection::open(&directory.0, &namespace, "apps").is_err());
    std::fs::write(&path, &valid).unwrap();
    let leaf = collection.shard_path(1, 0);
    amaterasu_storage::write_json_atomically(&leaf, &json!({"one":1,"two":2})).unwrap();
    let loaded = Collection::open(&directory.0, &namespace, "apps").unwrap();
    let failure = loaded
        .commit(
            1,
            json!({}),
            vec![Edit {
                key: "one".into(),
                value: None,
            }],
        )
        .unwrap_err()
        .payload();
    assert_eq!(failure["error"]["code"], "corrupt_storage");
    assert_eq!(std::fs::read(&path).unwrap(), valid);
}

#[test]
fn durable_draft_publishes_many_edits_atomically_and_replays_after_restart() {
    let directory = Directory::new();
    let store = StateStore::open(directory.0.clone()).unwrap();
    let call = |payload| bounded_handle(&store, "catalog", payload);
    let begin = json!({"action":"collection_draft_begin","collection":"apps","draft_id":"draft_1","expected_revision":0});
    assert_eq!(call(begin.clone())["next_sequence"], 0);
    for sequence in 0..10 {
        let edits: Vec<_> = (0..50)
            .map(|n| json!({"key":format!("app-{:016x}",sequence * 50 + n),"value":entry(n)}))
            .collect();
        let append = json!({"action":"collection_draft_append","collection":"apps","draft_id":"draft_1","sequence":sequence,"edits":edits});
        assert_eq!(call(append.clone())["next_sequence"], sequence + 1);
        assert_eq!(call(append)["replayed"], true);
        assert_eq!(
            call(json!({"action":"collection_get","collection":"apps"}))["count"],
            0
        );
    }
    drop(store);
    let store = StateStore::open(directory.0.clone()).unwrap();
    assert_eq!(
        bounded_handle(&store, "catalog", begin)["next_sequence"],
        10
    );
    let publish = json!({"action":"collection_draft_publish","collection":"apps","draft_id":"draft_1","expected_revision":0,"metadata":{"revision":1}});
    assert_eq!(
        bounded_handle(&store, "catalog", publish.clone())["revision"],
        1
    );
    drop(store);
    let store = StateStore::open(directory.0.clone()).unwrap();
    let replay = bounded_handle(&store, "catalog", publish);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["revision"], 1);
    assert_eq!(
        bounded_handle(
            &store,
            "catalog",
            json!({"action":"collection_get","collection":"apps"})
        )["count"],
        500
    );
}

#[test]
fn drafts_are_namespace_bound_conflict_safe_and_abortable() {
    let directory = Directory::new();
    let store = StateStore::open(directory.0.clone()).unwrap();
    let begin = json!({"action":"collection_draft_begin","collection":"apps","draft_id":"draft_2","expected_revision":0});
    assert_eq!(store.handle("first", begin.clone())["ok"], true);
    let append = json!({"action":"collection_draft_append","collection":"apps","draft_id":"draft_2","sequence":0,"edits":[{"key":"one","value":1}]});
    assert_eq!(store.handle("first", append)["ok"], true);
    let changed = json!({"action":"collection_draft_append","collection":"apps","draft_id":"draft_2","sequence":0,"edits":[{"key":"one","value":2}]});
    assert_eq!(
        store.handle("first", changed)["error"]["code"],
        "sequence_conflict"
    );
    let wrong_collection =
        json!({"action":"collection_draft_abort","collection":"other","draft_id":"draft_2"});
    assert_eq!(
        store.handle("first", wrong_collection)["error"]["code"],
        "draft_conflict"
    );
    assert_eq!(store.handle("second", begin)["ok"], true);
    assert_eq!(store.handle("first", json!({"action":"collection_commit","collection":"apps","expected_revision":0,"metadata":{},"edits":[{"key":"manual","value":3}]}))["ok"], true);
    let publish = json!({"action":"collection_draft_publish","collection":"apps","draft_id":"draft_2","expected_revision":0,"metadata":{}});
    assert_eq!(
        store.handle("first", publish)["error"]["code"],
        "revision_conflict"
    );
    let found = store.handle(
        "first",
        json!({"action":"collection_get","collection":"apps","key":"manual"}),
    );
    assert_eq!(found["value"], 3);
    let abort = json!({"action":"collection_draft_abort","collection":"apps","draft_id":"draft_2"});
    assert_eq!(store.handle("first", abort.clone())["ok"], true);
    assert_eq!(store.handle("first", abort)["ok"], true);
    assert_eq!(store.handle("second", json!({"action":"collection_draft_begin","collection":"apps","draft_id":"draft_2","expected_revision":0}))["next_sequence"], 0);
}

#[test]
fn published_receipt_replay_never_removes_reused_id_in_another_collection() {
    let directory = Directory::new();
    let store = StateStore::open(directory.0.clone()).unwrap();
    let begin = |collection| json!({"action":"collection_draft_begin","collection":collection,"draft_id":"shared","expected_revision":0});
    let publish = json!({"action":"collection_draft_publish","collection":"first","draft_id":"shared","expected_revision":0,"metadata":{}});
    assert_eq!(store.handle("catalog", begin("first"))["ok"], true);
    assert_eq!(store.handle("catalog", publish.clone())["ok"], true);
    assert_eq!(store.handle("catalog", begin("second"))["ok"], true);
    assert_eq!(store.handle("catalog", publish)["replayed"], true);
    assert_eq!(store.handle("catalog", json!({"action":"collection_draft_append","collection":"second","draft_id":"shared","sequence":0,"edits":[{"key":"safe","value":1}]}))["ok"], true);
}

#[test]
fn draft_limits_and_cross_chunk_duplicate_keys_fail_before_publication() {
    let directory = Directory::new();
    let store = StateStore::open(directory.0.clone()).unwrap();
    for n in 0..16 {
        let result = store.handle("catalog", json!({"action":"collection_draft_begin","collection":"apps","draft_id":format!("draft-{n}"),"expected_revision":0}));
        assert_eq!(result["ok"], true, "{result}");
    }
    let full = store.handle("catalog", json!({"action":"collection_draft_begin","collection":"apps","draft_id":"overflow","expected_revision":0}));
    assert_eq!(full["error"]["code"], "resource_exhausted");
    for sequence in 0..2 {
        let result = store.handle("catalog", json!({"action":"collection_draft_append","collection":"apps","draft_id":"draft-0","sequence":sequence,"edits":[{"key":"same","value":sequence}]}));
        assert_eq!(result["ok"], true);
    }
    let rejected = store.handle("catalog", json!({"action":"collection_draft_publish","collection":"apps","draft_id":"draft-0","expected_revision":0,"metadata":{}}));
    assert_eq!(rejected["error"]["code"], "invalid_request");
    assert_eq!(
        store.handle(
            "catalog",
            json!({"action":"collection_get","collection":"apps"})
        )["revision"],
        0
    );
    assert_eq!(
        store.handle(
            "catalog",
            json!({"action":"collection_draft_abort","collection":"apps","draft_id":"draft-0"})
        )["ok"],
        true
    );
    assert_eq!(store.handle("catalog", json!({"action":"collection_draft_begin","collection":"apps","draft_id":"available","expected_revision":0}))["ok"], true);
}

#[test]
fn restart_recovers_chunk_persisted_before_draft_cursor_update() {
    let directory = Directory::new();
    let store = StateStore::open(directory.0.clone()).unwrap();
    let begin = json!({"action":"collection_draft_begin","collection":"apps","draft_id":"crash","expected_revision":0});
    assert_eq!(store.handle("catalog", begin)["ok"], true);
    drop(store);
    let path = directory
        .0
        .join("namespaces/636174616c6f67/drafts/crash/chunk-0000000000000000.json");
    let chunk = json!([{"key":"one","value":1}]);
    amaterasu_storage::write_atomically(&path, &serde_json::to_vec(&chunk).unwrap()).unwrap();
    let store = StateStore::open(directory.0.clone()).unwrap();
    let changed = json!({"action":"collection_draft_append","collection":"apps","draft_id":"crash","sequence":0,"edits":[{"key":"one","value":2}]});
    assert_eq!(
        store.handle("catalog", changed)["error"]["code"],
        "sequence_conflict"
    );
    let same = json!({"action":"collection_draft_append","collection":"apps","draft_id":"crash","sequence":0,"edits":chunk});
    assert_eq!(store.handle("catalog", same)["next_sequence"], 1);
    let publish = json!({"action":"collection_draft_publish","collection":"apps","draft_id":"crash","expected_revision":0,"metadata":{}});
    assert_eq!(store.handle("catalog", publish)["ok"], true);
    assert_eq!(
        store.handle(
            "catalog",
            json!({"action":"collection_get","collection":"apps","key":"one"})
        )["value"],
        1
    );
}

#[test]
fn stale_drafts_are_reclaimed_while_recent_and_resumed_drafts_survive() {
    use std::time::{Duration, SystemTime};
    let directory = Directory::new();
    let store = StateStore::open(directory.0.clone()).unwrap();
    let begin = |id| json!({"action":"collection_draft_begin","collection":"apps","draft_id":id,"expected_revision":0});
    for id in ["stale", "resume", "recent"] {
        assert_eq!(store.handle("catalog", begin(id))["ok"], true);
    }
    let root = directory.0.join("namespaces/636174616c6f67/drafts");
    let old = SystemTime::now()
        .checked_sub(Duration::from_secs(25 * 3600))
        .unwrap();
    for id in ["stale", "resume"] {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(root.join(id).join("draft.json"))
            .unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
    }
    assert_eq!(store.handle("catalog", begin("resume"))["ok"], true);
    assert_eq!(store.handle("catalog", begin("new"))["ok"], true);
    assert!(!root.join("stale").exists());
    assert!(root.join("resume").exists());
    assert!(root.join("recent").exists());
}

#[cfg(unix)]
#[test]
fn stale_reclamation_retains_malformed_and_symlinked_entries() {
    use std::time::{Duration, SystemTime};
    let directory = Directory::new();
    let store = StateStore::open(directory.0.clone()).unwrap();
    let begin = |id| json!({"action":"collection_draft_begin","collection":"apps","draft_id":id,"expected_revision":0});
    assert_eq!(store.handle("catalog", begin("malformed"))["ok"], true);
    let base = directory.0.join("namespaces/636174616c6f67/drafts");
    let state_path = base.join("malformed/draft.json");
    std::fs::write(&state_path, b"invalid").unwrap();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&state_path)
        .unwrap();
    file.set_times(
        std::fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(25 * 3600)),
    )
    .unwrap();
    let outside = directory.0.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, base.join("linked")).unwrap();
    assert_eq!(store.handle("catalog", begin("new"))["ok"], true);
    assert!(state_path.exists());
    assert!(base.join("linked").is_symlink());
    assert!(outside.exists());
}

#[test]
fn draft_content_identity_fences_resumes_across_restart() {
    let directory = Directory::new();
    let store = StateStore::open(directory.0.clone()).unwrap();
    let begin = json!({"action":"collection_draft_begin","collection":"apps","draft_id":"bound","expected_revision":0,"content_id":"a".repeat(64)});
    assert_eq!(store.handle("catalog", begin.clone())["next_sequence"], 0);
    assert_eq!(store.handle("catalog", json!({"action":"collection_draft_append","collection":"apps","draft_id":"bound","sequence":0,"edits":[{"key":"one","value":1}]}))["next_sequence"], 1);
    drop(store);
    let store = StateStore::open(directory.0.clone()).unwrap();
    let mut changed = begin.clone();
    changed["content_id"] = json!("b".repeat(64));
    assert_eq!(
        store.handle("catalog", changed)["error"]["code"],
        "draft_conflict"
    );
    let mut omitted = begin.clone();
    omitted.as_object_mut().unwrap().remove("content_id");
    assert_eq!(
        store.handle("catalog", omitted)["error"]["code"],
        "draft_conflict"
    );
    assert_eq!(store.handle("catalog", begin)["next_sequence"], 1);
    let invalid = json!({"action":"collection_draft_begin","collection":"apps","draft_id":"invalid","expected_revision":0,"content_id":"X".repeat(64)});
    assert_eq!(
        store.handle("catalog", invalid)["error"]["code"],
        "invalid_request"
    );
    let legacy = json!({"action":"collection_draft_begin","collection":"apps","draft_id":"legacy","expected_revision":0});
    assert_eq!(store.handle("catalog", legacy.clone())["next_sequence"], 0);
    assert_eq!(store.handle("catalog", legacy)["next_sequence"], 0);
}
