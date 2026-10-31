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
            authority_epoch: "authority.epoch".into(),
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

fn rotation(
    previous: &str,
    next: &str,
) -> amaterasu_module_sdk::protocol_authority::ConfirmedEpochRotation {
    use amaterasu_module_sdk::protocol_authority::ConfirmedEpochRotation;
    ConfirmedEpochRotation::from_authority_responses(
        previous,
        &serde_json::json!({"ok":true,"epoch":previous,"owner_epoch":"owner.old","fence_generation":1,"drain_required":false,"in_flight":0}),
        &serde_json::json!({"ok":true,"epoch":next}),
        &serde_json::json!({"ok":true,"epoch":next,"owner_epoch":"owner.new","fence_generation":0,"drain_required":false,"in_flight":0}),
        "owner.new",
    ).unwrap()
}

#[test]
fn retired_authority_epochs_reuse_bounded_capacity_and_reject_old_replays_after_reopen() {
    let path = directory();
    let mut store = TestStateStore::open(path.clone()).unwrap();
    for n in 0..20 {
        let epoch = if n == 0 {
            "authority.epoch".to_string()
        } else {
            format!("authority.epoch.{n}")
        };
        let next = format!("authority.epoch.{}", n + 1);
        let mut entry = effect("caller", "same.operation");
        entry.context.authority_epoch = epoch.clone();
        let ReservationPlan::Write(write) = begin(&store, entry.clone()) else {
            panic!("new epoch must accept work")
        };
        let response = invoke(&store, write.request());
        assert!(write.confirmed(response).is_ok());
        let read = invoke(&store, &protocol_state::read_request());
        let proof = rotation(&epoch, &next);
        assert!(matches!(
            protocol_state::prepare_epoch_retirement(&binding(), &read, &proof),
            Err("cleanup_unconfirmed")
        ));
        if n == 0 {
            drop(store);
            store = TestStateStore::open(path.clone()).unwrap();
            let reopened = invoke(&store, &protocol_state::read_request());
            assert!(matches!(
                protocol_state::prepare_epoch_retirement(&binding(), &reopened, &proof),
                Err("cleanup_unconfirmed")
            ));
        }
        // An unsubmitted or lost cleanup reply cannot be assumed successful.
        let cleanup =
            protocol_state::prepare_cleanup(&binding(), &read, &entry, &reference()).unwrap();
        assert!(matches!(
            protocol_state::prepare_epoch_retirement(
                &binding(),
                &invoke(&store, &protocol_state::read_request()),
                &proof
            ),
            Err("cleanup_unconfirmed")
        ));
        assert_eq!(invoke(&store, &cleanup)["ok"], true); // deliberately discard reply
        let read = invoke(&store, &protocol_state::read_request());
        assert!(protocol_state::entries(&read).unwrap()[0].cleaned);
        let retire = protocol_state::prepare_epoch_retirement(&binding(), &read, &proof).unwrap();
        assert_eq!(invoke(&store, &retire)["ok"], true); // lost CAS reply
        drop(store);
        store = TestStateStore::open(path.clone()).unwrap();
        let reopened = invoke(&store, &protocol_state::read_request());
        assert_eq!(
            protocol_state::journal_epoch(&reopened).unwrap().as_deref(),
            Some(next.as_str())
        );
        assert!(protocol_state::entries(&reopened).unwrap().is_empty());
        assert!(matches!(
            protocol_state::prepare_reservation(&binding(), &reopened, entry, reference()),
            Err("stale_authority_epoch")
        ));
    }
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn old_writer_cannot_publish_after_cleanup_and_epoch_rotation() {
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let first = effect("caller", "first");
    let ReservationPlan::Write(write) = begin(&store, first.clone()) else {
        panic!()
    };
    let response = invoke(&store, write.request());
    assert!(write.confirmed(response).is_ok());
    let old_read = invoke(&store, &protocol_state::read_request());
    let ReservationPlan::Write(delayed) = protocol_state::prepare_reservation(
        &binding(),
        &old_read,
        effect("caller", "delayed"),
        reference(),
    )
    .unwrap() else {
        panic!()
    };
    let cleanup =
        protocol_state::prepare_cleanup(&binding(), &old_read, &first, &reference()).unwrap();
    assert_eq!(invoke(&store, &cleanup)["ok"], true);
    let read = invoke(&store, &protocol_state::read_request());
    let retire = protocol_state::prepare_epoch_retirement(
        &binding(),
        &read,
        &rotation("authority.epoch", "next"),
    )
    .unwrap();
    assert_eq!(invoke(&store, &retire)["ok"], true);
    let stale = invoke(&store, delayed.request());
    assert!(delayed.confirmed(stale).is_err());
    assert_eq!(
        protocol_state::journal_epoch(&invoke(&store, &protocol_state::read_request()))
            .unwrap()
            .as_deref(),
        Some("next")
    );
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn unconfirmed_authority_retirement_cannot_produce_rotation_evidence() {
    use amaterasu_module_sdk::protocol_authority::ConfirmedEpochRotation;
    let recovered = serde_json::json!({"ok":true,"epoch":"new"});
    let new = serde_json::json!({"ok":true,"epoch":"new","owner_epoch":"owner","fence_generation":0,"drain_required":false,"in_flight":0});
    for old in [
        serde_json::json!({"ok":true,"epoch":"old","fence_generation":0,"drain_required":false,"in_flight":0}),
        serde_json::json!({"ok":true,"epoch":"old","fence_generation":1,"drain_required":true,"in_flight":0}),
        serde_json::json!({"ok":true,"epoch":"old","fence_generation":1,"drain_required":false,"in_flight":1}),
    ] {
        assert!(
            ConfirmedEpochRotation::from_authority_responses(
                "old", &old, &recovered, &new, "owner"
            )
            .is_err()
        );
    }
}

#[test]
fn legacy_journal_is_not_silently_reset_or_compacted() {
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let legacy = StateRequest::Set {
        key: "protocol-obligations/v1".into(),
        value: serde_json::json!({"version":1,"entries":[]}),
        expected_revision: Some(0),
    };
    assert_eq!(invoke(&store, &legacy)["ok"], true);
    let read = invoke(&store, &protocol_state::read_request());
    assert_eq!(protocol_state::journal_epoch(&read), Err("invalid_journal"));
    assert!(matches!(
        protocol_state::prepare_epoch_retirement(&binding(), &read, &rotation("old", "new")),
        Err("invalid_journal")
    ));
    assert_eq!(
        invoke(&store, &protocol_state::read_request())["value"]["version"],
        1
    );
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}
