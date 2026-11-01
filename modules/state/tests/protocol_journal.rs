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
        issuer_epoch: "issuer.epoch".into(),
        context: amaterasu_module_sdk::protocol::SessionContext {
            authority_epoch: "authority.epoch".into(),
            effect_owner_epoch: "owner.epoch".into(),
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
        action_approval_sha256: "c".repeat(64),
        cleanup_approval_sha256: "d".repeat(64),
        payload_sha256: "a".repeat(64),
        timeout_ms: 1000,
        parent: None,
    }
}
fn issuer(
    context: &amaterasu_module_sdk::protocol::SessionContext,
) -> amaterasu_module_sdk::effect_authority::Source {
    let LifecycleTarget::Stream {
        host_epoch,
        session_id,
        stream_id,
        generation,
    } = &context.target
    else {
        panic!()
    };
    amaterasu_module_sdk::effect_authority::Source {
        version: 1,
        id: "a".repeat(32),
        generation: *generation,
        host_epoch: host_epoch.clone(),
        session_id: session_id.clone(),
        stream_id: stream_id.clone(),
        stream_generation: *generation,
        device: "device".into(),
        device_generation: 1,
        effect_owner_epoch: context.effect_owner_epoch.clone(),
    }
}
// Storage tests inject an assumed retirement; this is not a production authority.
struct MockRetired;
impl amaterasu_module_sdk::protocol_sequence::IssuerRetirement for MockRetired {
    fn verify_retired(
        &self,
        _: &amaterasu_module_sdk::effect_authority::Source,
        _: &PendingEffect,
    ) -> Result<(), &'static str> {
        Ok(())
    }
    fn verify_cleanup(
        &self,
        _: &amaterasu_module_sdk::effect_authority::Source,
        _: &PendingEffect,
        _: &CleanupReference,
    ) -> Result<(), &'static str> {
        Ok(())
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
    assert_ne!(first.state_operation_id(), stale.state_operation_id());
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

#[test]
fn sequenced_source_runs_two_thousand_effects_without_global_epoch_rotation() {
    use amaterasu_module_sdk::protocol_sequence::{self, Lookup, SequencedSource};
    let path = directory();
    let mut store = TestStateStore::open(path.clone()).unwrap();
    let template = effect("caller", "unused");
    let source = SequencedSource::new(&template.caller, &template.context).unwrap();
    let mut unrelated = template.clone();
    if let LifecycleTarget::Stream { stream_id, .. } = &mut unrelated.context.target {
        *stream_id = "unrelated-live-stream".into();
    }
    let other_source = SequencedSource::new(&unrelated.caller, &unrelated.context).unwrap();
    unrelated.operation_id = other_source.operation_id(1).unwrap();
    let other_read = invoke(&store, &other_source.read_request());
    let other_pending = protocol_sequence::prepare_issue(
        &binding(),
        &other_source,
        &other_read,
        unrelated.clone(),
        issuer(&unrelated.context),
        reference(),
    )
    .unwrap();
    let other_response = invoke(&store, other_pending.request());
    assert!(other_pending.confirmed(other_response).is_ok());
    for sequence in 1..=2000 {
        let read = invoke(&store, &source.read_request());
        assert_eq!(
            protocol_sequence::next_sequence(&source, &read).unwrap(),
            sequence
        );
        let mut item = template.clone();
        item.operation_id = source.operation_id(sequence).unwrap();
        if sequence == 1 {
            let mut forged_issuer = issuer(&item.context);
            forged_issuer.effect_owner_epoch = "other.owner".into();
            assert!(matches!(
                protocol_sequence::prepare_issue(
                    &binding(),
                    &source,
                    &read,
                    item.clone(),
                    forged_issuer,
                    reference()
                ),
                Err("identity_mismatch")
            ));
        }
        let pending = protocol_sequence::prepare_issue(
            &binding(),
            &source,
            &read,
            item.clone(),
            issuer(&item.context),
            reference(),
        )
        .unwrap();
        let result = invoke(&store, pending.request());
        let mut receipt = pending.confirmed(result).unwrap();
        assert_eq!(receipt.reserve(&item), Ok(Reservation::New));
        let current = invoke(&store, &source.read_request());
        if sequence == 1 {
            struct NoEvidence;
            impl protocol_sequence::IssuerRetirement for NoEvidence {}
            assert!(matches!(
                protocol_sequence::prepare_cleanup(
                    &binding(),
                    &source,
                    &current,
                    sequence,
                    &item,
                    &reference(),
                    &NoEvidence
                ),
                Err("issuer_retirement_unproven")
            ));
            assert!(
                matches!(protocol_sequence::lookup(&source, &invoke(&store, &source.read_request()), sequence).unwrap(), Lookup::Existing(entry) if !entry.cleaned)
            );
        }
        let cleanup = protocol_sequence::prepare_cleanup(
            &binding(),
            &source,
            &current,
            sequence,
            &item,
            &reference(),
            &MockRetired,
        )
        .unwrap();
        assert_eq!(invoke(&store, &cleanup)["ok"], true);
        let retired = invoke(&store, &source.read_request());
        assert_eq!(
            protocol_sequence::lookup(&source, &retired, sequence).unwrap(),
            Lookup::Retired
        );
        assert!(retired.to_string().len() < 1000);
        if sequence % 500 == 0 {
            drop(store);
            store = TestStateStore::open(path.clone()).unwrap();
        }
    }
    let read = invoke(&store, &source.read_request());
    assert_eq!(
        protocol_sequence::lookup(&source, &read, 1).unwrap(),
        Lookup::Retired
    );
    assert_eq!(
        protocol_sequence::next_sequence(&source, &read).unwrap(),
        2001
    );
    let other_read = invoke(&store, &other_source.read_request());
    assert!(
        matches!(protocol_sequence::lookup(&other_source, &other_read, 1).unwrap(), Lookup::Existing(entry) if !entry.cleaned && entry.effect == unrelated)
    );
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn sequenced_cleanup_gaps_loss_and_replays_fail_closed() {
    use amaterasu_module_sdk::protocol_sequence::{self, Lookup, SequencedSource};
    let path = directory();
    let mut store = TestStateStore::open(path.clone()).unwrap();
    let template = effect("caller", "unused");
    let source = SequencedSource::new(&template.caller, &template.context).unwrap();
    let mut effects = Vec::new();
    for sequence in 1..=8 {
        let read = invoke(&store, &source.read_request());
        let mut item = template.clone();
        item.operation_id = source.operation_id(sequence).unwrap();
        let pending = protocol_sequence::prepare_issue(
            &binding(),
            &source,
            &read,
            item.clone(),
            issuer(&item.context),
            reference(),
        )
        .unwrap();
        let another = protocol_sequence::prepare_issue(
            &binding(),
            &source,
            &read,
            item.clone(),
            issuer(&item.context),
            reference(),
        )
        .unwrap();
        assert_ne!(pending.state_operation_id(), another.state_operation_id());
        assert_eq!(invoke(&store, pending.request())["ok"], true);
        let stale_response = invoke(&store, another.request());
        assert!(another.confirmed(stale_response).is_err());
        // Lost State reply: a reread identifies an existing effect, not a receipt.
        drop(pending);
        let observed = invoke(&store, &source.read_request());
        assert!(matches!(
            protocol_sequence::lookup(&source, &observed, sequence).unwrap(),
            Lookup::Existing(_)
        ));
        effects.push(item);
    }
    for sequence in [2_u64, 4, 8] {
        let read = invoke(&store, &source.read_request());
        let cleanup = protocol_sequence::prepare_cleanup(
            &binding(),
            &source,
            &read,
            sequence,
            &effects[(sequence - 1) as usize],
            &reference(),
            &MockRetired,
        )
        .unwrap();
        assert_eq!(invoke(&store, &cleanup)["ok"], true); // lose reply
    }
    drop(store);
    store = TestStateStore::open(path.clone()).unwrap();
    let read = invoke(&store, &source.read_request());
    assert_eq!(
        protocol_sequence::next_sequence(&source, &read),
        Err("capacity")
    );
    assert!(
        matches!(protocol_sequence::lookup(&source, &read, 2).unwrap(), Lookup::Existing(entry) if entry.cleaned)
    );
    assert!(
        matches!(protocol_sequence::lookup(&source, &read, 1).unwrap(), Lookup::Existing(entry) if !entry.cleaned)
    );
    let cleanup = protocol_sequence::prepare_cleanup(
        &binding(),
        &source,
        &read,
        1,
        &effects[0],
        &reference(),
        &MockRetired,
    )
    .unwrap();
    assert_eq!(invoke(&store, &cleanup)["ok"], true);
    let read = invoke(&store, &source.read_request());
    assert_eq!(
        protocol_sequence::lookup(&source, &read, 1).unwrap(),
        Lookup::Retired
    );
    assert_eq!(
        protocol_sequence::lookup(&source, &read, 2).unwrap(),
        Lookup::Retired
    );
    assert_eq!(protocol_sequence::next_sequence(&source, &read).unwrap(), 9);
    assert!(matches!(
        protocol_sequence::prepare_cleanup(
            &binding(),
            &source,
            &read,
            1,
            &effects[0],
            &reference(),
            &MockRetired,
        ),
        Err("retired")
    ));
    let mut replay = effects[0].clone();
    assert!(matches!(
        protocol_sequence::prepare_issue(
            &binding(),
            &source,
            &read,
            replay.clone(),
            issuer(&replay.context),
            reference()
        ),
        Err("identity_mismatch")
    ));
    replay.context.target = match replay.context.target {
        LifecycleTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            generation,
        } => LifecycleTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            generation: generation + 1,
        },
        _ => unreachable!(),
    };
    assert!(matches!(
        protocol_sequence::prepare_issue(
            &binding(),
            &source,
            &read,
            replay.clone(),
            issuer(&replay.context),
            reference()
        ),
        Err("identity_mismatch")
    ));
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn sequenced_version_one_without_issuer_fails_closed() {
    use amaterasu_module_sdk::protocol_sequence::{self, SequencedSource};
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let template = effect("caller", "unused");
    let source = SequencedSource::new(&template.caller, &template.context).unwrap();
    let StateRequest::Get { key } = source.read_request() else {
        panic!()
    };
    assert_eq!(
        key,
        "protocol-sequence/v1/047a3e2910de735ac225a616bf6f05349d9456761bf9cec633f0f831bdb053f3"
    );
    let source_hash = key
        .strip_prefix("protocol-sequence/v1/")
        .unwrap()
        .to_string();
    let legacy = StateRequest::Set {
        key,
        value: serde_json::json!({"version":1,"source":source_hash,"retired":0,"issued":0,"entries":[]}),
        expected_revision: Some(0),
    };
    assert_eq!(invoke(&store, &legacy)["ok"], true);
    let read = invoke(&store, &source.read_request());
    assert_eq!(
        protocol_sequence::next_sequence(&source, &read),
        Err("invalid_journal")
    );
    assert_eq!(
        invoke(&store, &source.read_request())["value"]["version"],
        1
    );
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn sequenced_version_two_without_broker_incarnation_fails_closed() {
    use amaterasu_module_sdk::protocol_sequence::{self, SequencedSource};
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let template = effect("caller", "unused");
    let source = SequencedSource::new(&template.caller, &template.context).unwrap();
    let StateRequest::Get { key } = source.read_request() else {
        panic!()
    };
    let source_hash = key
        .strip_prefix("protocol-sequence/v1/")
        .unwrap()
        .to_string();
    let legacy = StateRequest::Set {
        key,
        value: serde_json::json!({"version":2,"source":source_hash,"retired":0,"issued":0,"entries":[]}),
        expected_revision: Some(0),
    };
    assert_eq!(invoke(&store, &legacy)["ok"], true);
    let read = invoke(&store, &source.read_request());
    assert_eq!(
        protocol_sequence::next_sequence(&source, &read),
        Err("invalid_journal")
    );
    assert_eq!(
        invoke(&store, &source.read_request())["value"]["version"],
        2
    );
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn sequenced_version_three_without_approval_fingerprints_fails_closed() {
    use amaterasu_module_sdk::protocol_sequence::{self, SequencedSource};
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let template = effect("caller", "unused");
    let source = SequencedSource::new(&template.caller, &template.context).unwrap();
    let StateRequest::Get { key } = source.read_request() else {
        panic!()
    };
    let source_hash = key
        .strip_prefix("protocol-sequence/v1/")
        .unwrap()
        .to_string();
    let legacy = StateRequest::Set {
        key,
        value: serde_json::json!({"version":3,"source":source_hash,"retired":0,"issued":0,"entries":[]}),
        expected_revision: Some(0),
    };
    assert_eq!(invoke(&store, &legacy)["ok"], true);
    let read = invoke(&store, &source.read_request());
    assert_eq!(
        protocol_sequence::next_sequence(&source, &read),
        Err("invalid_journal")
    );
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}
