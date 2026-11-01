#![cfg(feature = "test-support")]
use amaterasu_module_sdk::{
    ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema,
    broker_effect_fence::{EffectScope, EffectSource, GuardedEffectIdentity, SourceSlotLease},
    extension_api::MethodRef,
    protocol_slot_state::{self, Entry, ProviderIncarnation, RecordedEffectMethod},
    protocol_state::CleanupReference,
    state::StateRequest,
};
use amaterasu_state::test_support::TestStateStore;
use serde_json::Value;

fn scope() -> EffectScope {
    EffectScope {
        authority_epoch: "authority".into(),
        issuer_epoch: "issuer".into(),
        source: EffectSource {
            version: 1,
            id: "a".repeat(32),
            generation: 1,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            stream_generation: 1,
            device: "device".into(),
            device_generation: 1,
            effect_owner_epoch: "owner".into(),
        },
    }
}
fn invoke(store: &TestStateStore, request: &StateRequest) -> Value {
    store.handle("pairing", request.payload())
}
// Storage fixture only; production needs actual broker and worker receipts.
struct TestRetired;
struct NoProof;
impl protocol_slot_state::EffectRetirement for NoProof {}
impl protocol_slot_state::SourceRetirement for NoProof {}
impl protocol_slot_state::BrokerClose for NoProof {}
impl protocol_slot_state::BrokerCompaction for NoProof {}
impl protocol_slot_state::OldWorkersRetired for NoProof {}
impl protocol_slot_state::OldWorkersRetired for TestRetired {
    fn verify(
        &self,
        _: &protocol_slot_state::Claim,
        _: &SourceSlotLease,
    ) -> Result<(), &'static str> {
        Ok(())
    }
}
impl protocol_slot_state::EffectRetirement for TestRetired {
    fn verify(&self, _: &SourceSlotLease, _: &Entry) -> Result<(), &'static str> {
        Ok(())
    }
}
impl protocol_slot_state::SourceRetirement for TestRetired {
    fn verify(&self, _: &protocol_slot_state::Claim) -> Result<(), &'static str> {
        Ok(())
    }
}
impl protocol_slot_state::BrokerClose for TestRetired {
    fn verify(&self, _: &protocol_slot_state::Claim) -> Result<(), &'static str> {
        Ok(())
    }
}
impl protocol_slot_state::BrokerCompaction for TestRetired {
    fn verify(&self, _: &SourceSlotLease, _: u64) -> Result<(), &'static str> {
        Ok(())
    }
}
fn entry(scope: &EffectScope, sequence: u64, lease: &SourceSlotLease) -> Entry {
    let method = ApiMethod {
        service: "example.effects/1".into(),
        method: "apply".into(),
        interface_major: 1,
        interface_minor: 0,
        request: ApiSchema {
            required: Default::default(),
            optional: Default::default(),
            allow_extra: false,
        },
        response: ApiSchema {
            required: Default::default(),
            optional: Default::default(),
            allow_extra: false,
        },
        privilege: ApiPrivilege::User,
    };
    let mut cleanup = method.clone();
    cleanup.method = "cleanup".into();
    Entry {
        issued_lease: lease.clone(),
        provider_incarnation: ProviderIncarnation {
            version: 1,
            id: "e".repeat(32),
        },
        identity: GuardedEffectIdentity {
            scope: scope.clone(),
            sequence,
            capability: "example.effect".into(),
            provider: "provider".into(),
            service: "example.effects/1".into(),
            method: "apply".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
            payload_sha256: "c".repeat(64),
            timeout_ms: 1000,
            operation_id: format!("effect.{sequence}"),
            parent: None,
        },
        selected_method: RecordedEffectMethod {
            version: 1,
            provider: "provider".into(),
            method,
            provider_incarnation: ProviderIncarnation {
                version: 1,
                id: "e".repeat(32),
            },
        },
        cleanup_method: MethodRef {
            service: "example.effects/1".into(),
            method: "cleanup".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        },
        cleanup_approval: ApiMethodApproval {
            provider: "provider".into(),
            callers: ["issuer".into()].into(),
            method: cleanup,
        },
        cleanup: CleanupReference::new("d".repeat(64)).unwrap(),
        cleanup_confirmed: false,
    }
}
#[test]
fn durable_slot_claim_and_issue_survive_lost_replies_and_restart() {
    let path = std::env::temp_dir().join(format!(
        "pairing-slot-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = TestStateStore::open(path.clone()).unwrap();
    let scope = scope();
    let slot = scope.source.slot().unwrap();
    let mut lease = SourceSlotLease {
        slot,
        incarnation: 1,
        broker_epoch: "broker".into(),
        token: "opaque".into(),
    };
    let original_lease = lease.clone();
    let lookup = protocol_slot_state::read_request(slot).unwrap();
    let initial = invoke(&store, &lookup);
    let claim =
        protocol_slot_state::prepare_claim(&initial, lease.clone(), scope.clone(), "issuer".into())
            .unwrap();
    let duplicate =
        protocol_slot_state::prepare_claim(&initial, lease.clone(), scope.clone(), "issuer".into())
            .unwrap();
    assert_ne!(claim.state_operation_id(), duplicate.state_operation_id());
    let response = invoke(&store, claim.request());
    claim.confirmed(response).unwrap();
    let duplicate_response = invoke(&store, duplicate.request());
    assert!(duplicate.confirmed(duplicate_response).is_err());
    let current = invoke(&store, &lookup);
    assert!(
        protocol_slot_state::prepare_claim(&current, lease.clone(), scope.clone(), "issuer".into())
            .is_err()
    );
    let first = protocol_slot_state::prepare_issue(
        &current,
        slot,
        &lease,
        entry(&scope, 1, &original_lease),
    )
    .unwrap();
    assert_eq!(invoke(&store, first.request())["ok"], true);
    // Deliberately lose the write reply; a reread discovers the identity but is
    // not itself a broker arm, dispatch or worker-retirement receipt.
    drop(first);
    drop(store);
    let store = TestStateStore::open(path.clone()).unwrap();
    let current = invoke(&store, &lookup);
    let (_, slot_state) = protocol_slot_state::read(slot, &current).unwrap();
    let active = slot_state.active.unwrap();
    assert_eq!(active.issued_through, 1);
    assert_eq!(active.entries, vec![entry(&scope, 1, &original_lease)]);
    assert_eq!(active.entries[0].selected_method.provider, "provider");
    assert_eq!(active.entries[0].cleanup_approval.provider, "provider");
    let mut legacy = current.clone();
    legacy["value"]["active"]["entries"][0]
        .as_object_mut()
        .unwrap()
        .remove("provider_incarnation");
    assert!(protocol_slot_state::read(slot, &legacy).is_err());
    let mut unknown_version = current.clone();
    unknown_version["value"]["active"]["entries"][0]["provider_incarnation"]["version"] =
        serde_json::json!(2);
    assert!(protocol_slot_state::read(slot, &unknown_version).is_err());
    assert!(
        protocol_slot_state::prepare_issue(
            &current,
            slot,
            &lease,
            entry(&scope, 1, &original_lease)
        )
        .is_err()
    );
    let mut wrong = scope.clone();
    wrong.issuer_epoch = "other".into();
    assert!(
        protocol_slot_state::prepare_issue(
            &current,
            slot,
            &lease,
            entry(&wrong, 2, &original_lease)
        )
        .is_err()
    );
    let mut wrong_provider = entry(&scope, 2, &original_lease);
    wrong_provider.selected_method.provider = "other".into();
    assert!(protocol_slot_state::prepare_issue(&current, slot, &lease, wrong_provider).is_err());
    let mut wrong_cleanup = entry(&scope, 2, &original_lease);
    wrong_cleanup.cleanup_approval.callers.clear();
    assert!(protocol_slot_state::prepare_issue(&current, slot, &lease, wrong_cleanup).is_err());
    let mut already_cleaned = entry(&scope, 2, &original_lease);
    already_cleaned.cleanup_confirmed = true;
    assert!(protocol_slot_state::prepare_issue(&current, slot, &lease, already_cleaned).is_err());
    assert!(!current.to_string().contains("payload_base64"));
    let new_lease = SourceSlotLease {
        broker_epoch: "new-broker".into(),
        token: "new-token".into(),
        ..lease.clone()
    };
    assert!(
        protocol_slot_state::prepare_recovery_lease(
            &current,
            slot,
            &lease,
            new_lease.clone(),
            &NoProof
        )
        .is_err()
    );
    let recovery = protocol_slot_state::prepare_recovery_lease(
        &current,
        slot,
        &lease,
        new_lease.clone(),
        &TestRetired,
    )
    .unwrap();
    let result = invoke(&store, recovery.request());
    recovery.confirmed(result).unwrap();
    lease = new_lease;
    let current = invoke(&store, &lookup);
    let (_, slot_state) = protocol_slot_state::read(slot, &current).unwrap();
    assert_eq!(
        slot_state.active.unwrap().entries[0].issued_lease,
        original_lease
    );
    assert!(
        protocol_slot_state::prepare_retire_effect(
            &current,
            slot,
            &lease,
            &entry(&scope, 1, &original_lease),
            &NoProof
        )
        .is_err()
    );
    let retire = protocol_slot_state::prepare_retire_effect(
        &current,
        slot,
        &lease,
        &entry(&scope, 1, &original_lease),
        &TestRetired,
    )
    .unwrap();
    let result = invoke(&store, retire.request());
    retire.confirmed(result).unwrap();
    drop(store);
    let store = TestStateStore::open(path.clone()).unwrap();
    let current = invoke(&store, &lookup);
    let (_, slot_state) = protocol_slot_state::read(slot, &current).unwrap();
    let retained = slot_state.active.unwrap();
    assert_eq!(retained.retired_through, 0);
    assert!(retained.entries[0].cleanup_confirmed);
    assert_eq!(retained.entries[0].selected_method.provider, "provider");
    assert!(protocol_slot_state::prepare_compacted(&current, slot, &lease, 1, &NoProof).is_err());
    let compact =
        protocol_slot_state::prepare_compacted(&current, slot, &lease, 1, &TestRetired).unwrap();
    let result = invoke(&store, compact.request());
    compact.confirmed(result).unwrap();
    let current = invoke(&store, &lookup);
    assert!(protocol_slot_state::prepare_retire_slot(&current, slot, &lease, &NoProof).is_err());
    let retire =
        protocol_slot_state::prepare_retire_slot(&current, slot, &lease, &TestRetired).unwrap();
    assert_eq!(invoke(&store, retire.request())["ok"], true);
    drop(retire); // Lost reply after the durable write.
    drop(store);
    let store = TestStateStore::open(path.clone()).unwrap();
    let current = invoke(&store, &lookup);
    let (_, slot_state) = protocol_slot_state::read(slot, &current).unwrap();
    assert_eq!(slot_state.retired_incarnation, 1);
    assert_eq!(slot_state.pending_close.as_ref().unwrap().lease, lease);
    let responses: Vec<_> = (0..64)
        .map(|index| {
            (
                index,
                invoke(&store, &protocol_slot_state::read_request(index).unwrap()),
            )
        })
        .collect();
    let snapshot = protocol_slot_state::recovery_snapshot(&responses).unwrap();
    assert_eq!(snapshot.retired_incarnations[usize::from(slot)], 1);
    assert_eq!(snapshot.pending_close[0].lease, lease);
    let mut inconsistent = responses.clone();
    inconsistent[0].1["revision"] = serde_json::json!(snapshot.revision + 1);
    assert!(protocol_slot_state::recovery_snapshot(&inconsistent).is_err());
    let next = SourceSlotLease {
        incarnation: 2,
        token: "next".into(),
        ..lease.clone()
    };
    assert!(
        protocol_slot_state::prepare_claim(&current, next.clone(), scope.clone(), "issuer".into())
            .is_err()
    );
    assert!(protocol_slot_state::prepare_clear_closed(&current, slot, &NoProof).is_err());
    let clear = protocol_slot_state::prepare_clear_closed(&current, slot, &TestRetired).unwrap();
    let response = invoke(&store, clear.request());
    clear.confirmed(response).unwrap();
    let current = invoke(&store, &lookup);
    assert!(
        protocol_slot_state::prepare_claim(&current, next, scope.clone(), "issuer".into()).is_ok()
    );
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}
