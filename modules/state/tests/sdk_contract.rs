use amaterasu_module_sdk::state::{self, StateLookup, StateMutation, StateRequest};
use amaterasu_state::test_support::TestStateStore;
use serde_json::{Value, json};
use std::collections::BTreeMap;

// The same consumer works with each provider and observes CAS and caller scope.
fn exercise(mut call: impl FnMut(&str, Value) -> Value) {
    let set = StateRequest::Set {
        key: "key".into(),
        value: json!(null),
        expected_revision: Some(0),
    };
    let saved: StateMutation = state::decode_response(call("alice", set.payload()))
        .unwrap()
        .unwrap();
    assert_eq!(saved.revision, 1);
    let lookup = StateRequest::Get { key: "key".into() };
    let own: StateLookup = state::decode_response(call("alice", lookup.payload()))
        .unwrap()
        .unwrap();
    assert!(own.found);
    assert_eq!(own.value, Value::Null);
    let other: StateLookup = state::decode_response(call("bob", lookup.payload()))
        .unwrap()
        .unwrap();
    assert!(!other.found);
    let failure = state::decode_response::<StateMutation>(call("alice", set.payload()))
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.code, "revision_conflict");
    assert_eq!(failure.current_revision, Some(1));
}

#[test]
fn key_value_consumer_works_with_real_and_independent_providers() {
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-sdk-state-{}",
        amaterasu_storage::random_identifier().unwrap()
    ));
    let store = TestStateStore::open(directory.clone()).unwrap();
    exercise(|caller, payload| store.handle(caller, payload));
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();

    let mut namespaces: BTreeMap<String, (u64, BTreeMap<String, Value>)> = BTreeMap::new();
    exercise(|caller, payload| {
        let operation: StateRequest = serde_json::from_value(payload).unwrap();
        let (revision, values) = namespaces.entry(caller.into()).or_default();
        match operation {
            StateRequest::Get { key } => {
                json!({"ok":true,"revision":revision,"found":values.contains_key(&key),"value":values.get(&key)})
            }
            StateRequest::Set {
                key,
                value,
                expected_revision,
            } => {
                if expected_revision.is_some_and(|expected| expected != *revision) {
                    json!({"ok":false,"error":{"code":"revision_conflict","message":"revision differs","current_revision":revision}})
                } else {
                    values.insert(key, value);
                    *revision += 1;
                    json!({"ok":true,"revision":revision})
                }
            }
            StateRequest::Delete { .. } => unreachable!(),
        }
    });
}

#[test]
fn payload_cannot_select_another_caller_and_bad_responses_fail() {
    assert!(
        serde_json::from_value::<StateRequest>(json!({"action":"get","key":"key","caller":"bob"}))
            .is_err()
    );
    assert!(state::decode_response::<StateLookup>(json!({"ok":true,"found":true})).is_err());
    assert!(state::decode_response::<StateLookup>(json!({"ok":false})).is_err());
}
