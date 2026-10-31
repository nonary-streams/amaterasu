#![cfg(feature = "test-support")]
use amaterasu_module_sdk::{
    extension_api::{LifecycleTarget, MethodRef},
    protocol::{ObligationStore, PendingEffect, Reservation},
    protocol_state::{
        self, CleanupReference, DURABLE_CAS_CAPABILITY, DurableStateBinding, ReservationPlan,
    },
    state::StateRequest,
};
use amaterasu_state::test_support::TestStateStore;
use serde_json::Value;
use std::path::PathBuf;

fn directory() -> PathBuf {
    std::env::temp_dir().join(format!(
        "protocol-state-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
fn effect(caller: &str, operation: &str) -> PendingEffect {
    let method = MethodRef {
        service: "service/1".into(),
        method: "receive".into(),
        interface_major: 1,
        interface_minor: 0,
        schema_revision: 1,
    };
    let mut cleanup = method.clone();
    cleanup.method = "cleanup".into();
    PendingEffect {
        caller: caller.into(),
        operation_id: operation.into(),
        context: amaterasu_module_sdk::protocol::SessionContext {
            target: LifecycleTarget::Stream {
                host_epoch: "epoch".into(),
                session_id: "session".into(),
                stream_id: "stream".into(),
                generation: 1,
            },
            peer_ref: "peer".into(),
        },
        message_type: 0x553a,
        capability: "capability".into(),
        provider: "provider".into(),
        method,
        cleanup,
        payload_sha256: "a".repeat(64),
        timeout_ms: 1000,
        parent: None,
    }
}
fn reference() -> CleanupReference {
    CleanupReference::new("b".repeat(64)).unwrap()
}
fn binding() -> DurableStateBinding {
    DurableStateBinding::new("owner", "state", DURABLE_CAS_CAPABILITY).unwrap()
}
fn invoke(store: &TestStateStore, request: &StateRequest) -> Value {
    store.handle("owner", request.payload())
}
fn begin(store: &TestStateStore, effect: PendingEffect) -> ReservationPlan {
    protocol_state::prepare_reservation(
        &binding(),
        &invoke(store, &protocol_state::read_request()),
        effect,
        reference(),
    )
    .unwrap()
}

#[test]
fn durable_cas_restart_lost_reply_and_tombstone() {
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let one = effect("caller.one", "same");
    let ReservationPlan::Write(write) = begin(&store, one.clone()) else {
        panic!()
    };
    // Committed write with lost reply: no proof is issued and re-reading must
    // discover the existing obligation, not grant another dispatch.
    assert_eq!(invoke(&store, write.request())["ok"], true);
    drop(write);
    assert!(matches!(
        begin(&store, one.clone()),
        ReservationPlan::Existing
    ));
    let two = effect("caller.two", "same");
    let ReservationPlan::Write(write) = begin(&store, two.clone()) else {
        panic!()
    };
    let response = invoke(&store, write.request());
    let mut ready = write.confirmed(response).unwrap();
    assert_eq!(ready.reserve(&two), Ok(Reservation::New));
    assert!(ready.reserve(&two).is_err());
    drop(store);
    let store = TestStateStore::open(path.clone()).unwrap();
    let read = invoke(&store, &protocol_state::read_request());
    assert_eq!(protocol_state::entries(&read).unwrap().len(), 2);
    let cleanup = protocol_state::prepare_cleanup(&binding(), &read, &one, &reference()).unwrap();
    assert_eq!(invoke(&store, &cleanup)["ok"], true);
    let read = invoke(&store, &protocol_state::read_request());
    assert!(protocol_state::entries(&read).unwrap()[0].cleaned);
    assert!(matches!(begin(&store, one), ReservationPlan::Existing));
    assert!(!read.to_string().contains("certificate_pem"));
    assert!(!read.to_string().contains("ticket"));
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn stale_cas_capacity_and_durability_selection_fail_closed() {
    assert!(DurableStateBinding::new("owner", "state", "memory").is_err());
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let read = invoke(&store, &protocol_state::read_request());
    let ReservationPlan::Write(first) = protocol_state::prepare_reservation(
        &binding(),
        &read,
        effect("caller.one", "one"),
        reference(),
    )
    .unwrap() else {
        panic!()
    };
    let ReservationPlan::Write(stale) = protocol_state::prepare_reservation(
        &binding(),
        &read,
        effect("caller.two", "two"),
        reference(),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(invoke(&store, first.request())["ok"], true);
    let rejected = invoke(&store, stale.request());
    assert!(stale.confirmed(rejected).is_err());
    for n in 2..=8 {
        let ReservationPlan::Write(write) = begin(&store, effect("caller", &format!("op{n}")))
        else {
            panic!()
        };
        let result = invoke(&store, write.request());
        assert!(write.confirmed(result).is_ok());
    }
    let read = invoke(&store, &protocol_state::read_request());
    assert!(matches!(
        protocol_state::prepare_reservation(
            &binding(),
            &read,
            effect("caller", "overflow"),
            reference()
        ),
        Err("capacity")
    ));
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}
