use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self(
            std::env::temp_dir().join(format!("amaterasu-state-{}-{sequence}", std::process::id())),
        )
    }

    fn open(&self) -> StateStore {
        StateStore::open(self.0.clone()).unwrap()
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn mutations_survive_restart_and_delete_advances_revision() {
    let directory = TestDirectory::new();
    let store = directory.open();
    assert_eq!(
        store.handle(
            "alice",
            json!({"action":"set", "key":"theme", "value":"dark", "expected_revision":0})
        ),
        json!({"ok":true,"revision":1})
    );
    drop(store);
    let store = directory.open();
    assert_eq!(
        store.handle("alice", json!({"action":"snapshot"})),
        json!({"ok":true,"revision":1,"values":{"theme":"dark"},"next_cursor":null})
    );
    assert_eq!(
        store.handle(
            "alice",
            json!({"action":"delete", "key":"theme", "expected_revision":1})
        ),
        json!({"ok":true,"revision":2,"deleted":true})
    );
    drop(store);
    assert_eq!(
        directory
            .open()
            .handle("alice", json!({"action":"get", "key":"theme"})),
        json!({"ok":true,"revision":2,"found":false,"value":null})
    );
}

#[test]
fn revision_conflicts_leave_persisted_values_untouched() {
    let directory = TestDirectory::new();
    let store = directory.open();
    store.handle("alice", json!({"action":"set","key":"value","value":1}));
    for request in [
        json!({"action":"set","key":"value","value":2,"expected_revision":0}),
        json!({"action":"delete","key":"value","expected_revision":0}),
    ] {
        let response = store.handle("alice", request);
        assert_eq!(response["error"]["code"], "revision_conflict");
        assert_eq!(response["error"]["current_revision"], 1);
    }
    assert_eq!(
        store.handle("alice", json!({"action":"get","key":"value"}))["value"],
        1
    );
}

#[test]
fn caller_namespaces_are_isolated_and_paths_are_encoded() {
    let directory = TestDirectory::new();
    let store = directory.open();
    store.handle(
        "alice",
        json!({"action":"set","key":"../../theme","value":true}),
    );
    assert_eq!(
        store.handle("bob", json!({"action":"get","key":"../../theme"}))["found"],
        false
    );
    let caller = "../../outside";
    assert_eq!(
        store.handle(caller, json!({"action":"set","key":"key","value":1}))["ok"],
        true
    );
    let path = store.namespace_path(caller).unwrap();
    assert!(path.starts_with(&directory.0));
    assert!(
        !path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    );
    let override_attempt = store.handle("bob", json!({"action":"snapshot","namespace":"alice"}));
    assert_eq!(override_attempt["error"]["code"], "invalid_request");
}

#[test]
fn corrupt_namespace_is_never_reset_or_overwritten() {
    let directory = TestDirectory::new();
    let store = directory.open();
    store.handle("alice", json!({"action":"set","key":"key","value":1}));
    let path = store.namespace_path("alice").unwrap();
    std::fs::write(&path, b"broken json").unwrap();
    for request in [
        json!({"action":"snapshot"}),
        json!({"action":"set","key":"key","value":2}),
    ] {
        assert_eq!(
            store.handle("alice", request)["error"]["code"],
            "corrupt_storage"
        );
    }
    assert_eq!(std::fs::read(path).unwrap(), b"broken json");
}

#[test]
fn invalid_and_oversized_mutations_do_not_advance_revision() {
    let directory = TestDirectory::new();
    let store = directory.open();
    assert_eq!(
        store.handle("alice", json!({"action":"set","key":"","value":1}))["error"]["code"],
        "invalid_key"
    );
    assert_eq!(
        store.handle(
            "alice",
            json!({"action":"set","key":"large","value":"x".repeat(MAX_VALUE_BYTES)})
        )["error"]["code"],
        "value_too_large"
    );
    assert_eq!(
        store.handle("alice", json!({"action":"snapshot"}))["revision"],
        0
    );
}

#[test]
fn overlapping_store_instances_are_rejected() {
    let directory = TestDirectory::new();
    let _store = directory.open();
    assert!(StateStore::open(directory.0.clone()).is_err());
}

#[test]
fn null_values_are_distinct_from_missing_keys() {
    let directory = TestDirectory::new();
    let store = directory.open();
    assert_eq!(
        store.handle("alice", json!({"action":"set","key":"null","value":null}))["ok"],
        true
    );
    assert_eq!(
        store.handle("alice", json!({"action":"get","key":"null"}))["found"],
        true
    );
}

fn feedback_bootstrap_claim() -> serde_json::Value {
    json!({
        "version": 1,
        "installation_id": "a".repeat(32),
        "claim": "b".repeat(32),
        "pairing_owner": "pairing",
        "effect_owner": "stream"
    })
}

#[test]
fn feedback_bootstrap_is_host_only_durable_and_one_use() {
    let directory = TestDirectory::new();
    let claim = feedback_bootstrap_claim();
    let store = directory.open();

    let unauthorized = store.handle(
        "pairing",
        json!({"action":"initialize_feedback_host","claim":claim}),
    );
    assert_eq!(unauthorized["error"]["code"], "permission_denied");
    assert!(!directory.0.join("namespaces").exists());

    let initialized = store.handle(
        amaterasu_module_sdk::HOST_LIFECYCLE_CONTROL_PRINCIPAL,
        json!({"action":"initialize_feedback_host","claim":claim}),
    );
    assert_eq!(initialized, json!({"ok":true,"revision":1,"claim":claim}));

    let replay = store.handle(
        amaterasu_module_sdk::HOST_LIFECYCLE_CONTROL_PRINCIPAL,
        json!({"action":"initialize_feedback_host","claim":claim}),
    );
    assert_eq!(replay["error"]["code"], "history_present");
    drop(store);

    let restarted = directory.open();
    assert_eq!(
        restarted.handle(
            "pairing",
            json!({"action":"get","key":amaterasu_module_sdk::state::FEEDBACK_BOOTSTRAP_KEY})
        ),
        json!({
            "ok":true,
            "revision":1,
            "found":true,
            "value":{"claim":claim,"phase":"prepared"}
        })
    );
}

#[test]
fn feedback_bootstrap_refuses_any_preexisting_namespace_history() {
    let directory = TestDirectory::new();
    let store = directory.open();
    assert_eq!(
        store.handle(
            "old-owner",
            json!({"action":"set","key":"history","value":true})
        )["ok"],
        true
    );

    let response = store.handle(
        amaterasu_module_sdk::HOST_LIFECYCLE_CONTROL_PRINCIPAL,
        json!({"action":"initialize_feedback_host","claim":feedback_bootstrap_claim()}),
    );
    assert_eq!(response["error"]["code"], "history_present");
    assert_eq!(
        store.handle("old-owner", json!({"action":"get","key":"history"}))["value"],
        true
    );
}

#[test]
fn namespace_capacity_failure_keeps_previous_revision() {
    let directory = TestDirectory::new();
    let store = directory.open();
    let mut namespace = Namespace {
        values: (0..69)
            .map(|index| (format!("key-{index}"), json!("x".repeat(15_000))))
            .collect(),
        ..Namespace::default()
    };
    let path = store.namespace_path("alice").unwrap();
    persist_mutation(&directory.0, &path, &mut namespace).unwrap();
    let response = store.handle(
        "alice",
        json!({
            "action":"set", "key":"overflow", "value":"x".repeat(15_000)
        }),
    );
    assert_eq!(response["error"]["code"], "namespace_full");
    drop(store);
    let namespace = load_namespace(&path).unwrap();
    assert_eq!(namespace.revision, 1);
    assert_eq!(namespace.values.len(), 69);
}

#[cfg(unix)]
#[test]
fn symlinked_namespace_directory_is_rejected() {
    let directory = TestDirectory::new();
    let outside = TestDirectory::new();
    create_private_directory(&outside.0).unwrap();
    let store = directory.open();
    std::os::unix::fs::symlink(&outside.0, directory.0.join("namespaces")).unwrap();
    let response = store.handle("alice", json!({"action":"set","key":"key","value":1}));
    assert_eq!(response["error"]["code"], "storage_error");
    assert_eq!(std::fs::read_dir(&outside.0).unwrap().count(), 0);
}

#[test]
fn snapshots_traverse_bounded_pages_and_reject_stale_cursors() {
    let directory = TestDirectory::new();
    let store = directory.open();
    for index in 0..7 {
        assert_eq!(
            store.handle(
                "alice",
                json!({"action":"set","key":format!("key-{index}"),"value":"\\\"".repeat(1600)})
            )["ok"],
            true
        );
    }
    let first = store.handle("alice", json!({"action":"snapshot","limit":2}));
    assert_eq!(first["values"].as_object().unwrap().len(), 2);
    let mut collected = BTreeMap::new();
    let mut cursor = Value::Null;
    loop {
        let page = store.handle("alice", json!({"action":"snapshot","cursor":cursor}));
        assert_eq!(page["revision"], 7);
        assert!(snapshot::response_fits(&page));
        for (key, value) in page["values"].as_object().unwrap() {
            assert!(collected.insert(key.clone(), value.clone()).is_none());
        }
        cursor = page["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(collected.len(), 7);
    store.handle("alice", json!({"action":"delete","key":"key-0"}));
    let stale = store.handle(
        "alice",
        json!({"action":"snapshot","cursor":first["next_cursor"]}),
    );
    assert_eq!(stale["error"]["code"], "revision_conflict");
    assert_eq!(stale["error"]["current_revision"], 8);
}

#[test]
fn value_admission_accounts_for_escaped_keys_values_and_envelopes() {
    let directory = TestDirectory::new();
    let store = directory.open();
    let key = "\"".repeat(MAX_KEY_BYTES);
    let mut last_accepted = None;
    for length in 2500..2800 {
        let value = json!("\u{0000}".repeat(length));
        if snapshot::validate_entry(&key, &value).is_ok() {
            last_accepted = Some(value);
        } else {
            assert_eq!(
                store.handle("alice", json!({"action":"set","key":key,"value":value}))["error"]["code"],
                "value_too_large"
            );
            break;
        }
    }
    assert_eq!(
        store.handle("alice", json!({"action":"snapshot"}))["revision"],
        0
    );
    assert_eq!(
        store.handle(
            "alice",
            json!({"action":"set","key":key,"value":last_accepted.unwrap()})
        )["ok"],
        true
    );
    for request in [
        json!({"action":"get","key":key}),
        json!({"action":"snapshot"}),
    ] {
        let response = store.handle("alice", request);
        assert_eq!(response["ok"], true);
        assert!(snapshot::response_fits(&response));
    }
}
