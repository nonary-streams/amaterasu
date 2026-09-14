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
