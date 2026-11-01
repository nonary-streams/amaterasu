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
            StateRequest::InitializeFeedbackHost { .. } => {
                unreachable!("the caller-scoped key/value exercise does not initialize a host")
            }
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

// Exercise delivered and cancelled broker requests against the real State
// store. A provider may execute a request after its broker result has ended.
#[test]
fn broker_cancellation_requires_a_same_namespace_cas_before_abort() {
    use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig};
    use amaterasu_runtime::Broker;

    const STATE: &str = "amaterasu.state/1";
    const CONFIGURATION: &str = "amaterasu.host-configuration";
    const PAIRING: &str = "amaterasu.pairing";
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-state-cancel-fence-{}",
        amaterasu_storage::random_identifier().unwrap()
    ));
    let store = TestStateStore::open(directory.clone()).unwrap();
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "state".into(),
        Grants {
            provide: [STATE.into()].into(),
            ..Default::default()
        },
    );
    for caller in [CONFIGURATION, PAIRING] {
        config.principals.insert(
            caller.into(),
            Grants {
                call: [STATE.into()].into(),
                ..Default::default()
            },
        );
    }
    let mut broker = Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch(
            "state",
            Command::Register {
                service: STATE.into()
            }
        ),
        Response::Ok
    ));

    let request = |payload: Value, operation: &str| Command::Request {
        service: STATE.into(),
        payload,
        timeout_ms: 30_000,
        operation_id: operation.into(),
        parent: None,
    };
    let call = |broker: &mut Broker, caller: &str, payload: Value, operation: &str| {
        let Response::Accepted { id } = broker.dispatch(caller, request(payload, operation)) else {
            panic!("state request not accepted")
        };
        let Response::Request {
            id: delivered,
            caller: authenticated_caller,
            payload,
            ..
        } = broker.dispatch("state", Command::Poll {})
        else {
            panic!("state request not delivered")
        };
        assert_eq!(id, delivered);
        let result = store.handle(&authenticated_caller, payload);
        assert!(matches!(
            broker.dispatch(
                "state",
                Command::Complete {
                    request: id,
                    payload: result.clone()
                }
            ),
            Response::Ok
        ));
        assert!(
            matches!(broker.dispatch(caller, Command::TakeResult { request: id }), Response::Result { payload } if payload == result)
        );
        assert!(matches!(
            broker.dispatch(caller, Command::ReleaseResult { request: id }),
            Response::Ok
        ));
        result
    };
    let original = json!({"identity":"old"});
    let pending = json!({"action":"set","key":"configuration","expected_revision":0,"value":{"identity":"new"}});
    let Response::Accepted { id } =
        broker.dispatch(CONFIGURATION, request(pending, "delayed-write"))
    else {
        panic!("write not accepted")
    };
    let Response::Request {
        id: delivered,
        caller,
        payload,
        ..
    } = broker.dispatch("state", Command::Poll {})
    else {
        panic!("write not delivered")
    };
    assert_eq!(id, delivered);
    assert!(matches!(
        broker.dispatch(CONFIGURATION, Command::Cancel { request: id }),
        Response::Ok
    ));

    // An unrelated namespace cannot fence the delivered configuration write.
    assert_eq!(
        call(
            &mut broker,
            PAIRING,
            json!({"action":"set","key":"fence","value":true,"expected_revision":0}),
            "other-namespace"
        )["revision"],
        1
    );
    assert_eq!(
        call(
            &mut broker,
            CONFIGURATION,
            json!({"action":"get","key":"configuration"}),
            "read-before-fence"
        )["revision"],
        0
    );
    // This successful CAS advances the *same* namespace, so the earlier write
    // can no longer succeed even though the State provider received it already.
    assert_eq!(
        call(
            &mut broker,
            CONFIGURATION,
            json!({"action":"set","key":"configuration","value":original,"expected_revision":0}),
            "abort-fence"
        )["revision"],
        1
    );
    let late = store.handle(&caller, payload);
    assert_eq!(late["error"]["code"], "revision_conflict");
    assert!(
        matches!(broker.dispatch("state", Command::Complete { request: id, payload: late }), Response::Error { code, .. } if code == "request_ended")
    );
    drop(store);
    let store = TestStateStore::open(directory.clone()).unwrap();
    let recovered = store.handle(CONFIGURATION, json!({"action":"get","key":"configuration"}));
    assert_eq!(recovered["revision"], 1);
    assert_eq!(recovered["value"], original);

    // The opposite ordering proves cancellation alone is not an abort: State
    // can persist after cancellation and the attempted abort CAS must conflict.
    let Response::Accepted { id } = broker.dispatch(CONFIGURATION, request(json!({"action":"set","key":"configuration","expected_revision":1,"value":{"identity":"new"}}), "cancelled-write-wins")) else { panic!("write not accepted") };
    let Response::Request {
        caller, payload, ..
    } = broker.dispatch("state", Command::Poll {})
    else {
        panic!("write not delivered")
    };
    assert!(matches!(
        broker.dispatch(CONFIGURATION, Command::Cancel { request: id }),
        Response::Ok
    ));
    let persisted = store.handle(&caller, payload);
    assert_eq!(persisted["revision"], 2);
    assert!(
        matches!(broker.dispatch("state", Command::Complete { request: id, payload: persisted }), Response::Error { code, .. } if code == "request_ended")
    );
    assert_eq!(
        store.handle(
            CONFIGURATION,
            json!({"action":"set","key":"configuration","value":original,"expected_revision":1})
        )["error"]["code"],
        "revision_conflict"
    );
    assert_eq!(
        store.handle(CONFIGURATION, json!({"action":"get","key":"configuration"}))["value"]["identity"],
        "new"
    );
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}
