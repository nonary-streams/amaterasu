use super::*;
use crate::backend::Observation;
use crate::scale::SUPPORTED_SCALE_VALUES;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

const ID: &str = "00000000-0000-0000-0000-000000000001";
const CAPTURE_OUTPUT: &str =
    "exact-target:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[derive(Default)]
struct Calls {
    supported_calls: usize,
    creates: Vec<(u64, u64)>,
    removes: Vec<(u64, u64)>,
    feeds: Vec<u64>,
    ambiguous_create: bool,
    remove_failure: bool,
    remove_completes: bool,
    exists: bool,
    pending: bool,
    ready: bool,
    capture_ready: bool,
}
struct Fake(Rc<RefCell<Calls>>);
impl Driver for Fake {
    fn supported(&self) -> bool {
        self.0.borrow_mut().supported_calls += 1;
        true
    }
    fn create(
        &mut self,
        _: &Specification,
        lease: u64,
        display: u64,
        _: &[u8; 32],
    ) -> Result<Binding, &'static str> {
        let mut calls = self.0.borrow_mut();
        calls.creates.push((lease, display));
        calls.exists = true;
        if calls.ambiguous_create {
            return Err("driver_io_timeout");
        }
        Ok(Binding {
            adapter_low: 1,
            adapter_high: 0,
            target: 2,
        })
    }
    fn inspect(
        &mut self,
        _: u64,
        _: u64,
        _: Option<Binding>,
        _: &Specification,
    ) -> Result<Observation, &'static str> {
        let calls = self.0.borrow();
        Ok(Observation {
            exists: calls.exists,
            occupied_by_other_lease: false,
            specification_matches: true,
            lease_display_count: u32::from(calls.exists),
            pending_departure: calls.pending,
            ready: calls.ready.then(|| Ready {
                device_path: "owned-device".into(),
                active: calls.capture_ready,
                capture_output: calls.capture_ready.then(|| CAPTURE_OUTPUT.into()),
                capture_error: (!calls.capture_ready).then_some("capture_target_inactive"),
            }),
        })
    }
    fn feed(&mut self, lease: u64) -> Result<(), &'static str> {
        self.0.borrow_mut().feeds.push(lease);
        Ok(())
    }
    fn reclaim(&mut self, _: u64, _: u64, _: &[u8; 32]) -> Result<(), &'static str> {
        Err("reclaim_unavailable")
    }
    fn remove(&mut self, lease: u64, display: u64) -> Result<(), &'static str> {
        let mut calls = self.0.borrow_mut();
        calls.removes.push((lease, display));
        if calls.remove_failure {
            return Err("remove_failed");
        }
        if calls.remove_completes {
            calls.exists = false;
            calls.pending = false;
        }
        Ok(())
    }
}
fn setup_with(settings: Settings) -> (Manager<Fake>, Rc<RefCell<Calls>>) {
    let calls = Rc::new(RefCell::new(Calls::default()));
    (Manager::new(settings, Fake(calls.clone())), calls)
}
fn setup(enabled: bool) -> (Manager<Fake>, Rc<RefCell<Calls>>) {
    setup_with(Settings {
        enabled,
        ..Settings::default()
    })
}
fn create() -> Operation {
    Operation::Create {
        hdr_enabled: false,
        identity: ID.into(),
        width: 1920,
        height: 1080,
        refresh_millihz: 60000,
        name: "Test display".into(),
        reference_id: None,
        dd_virtual_display_scale: None,
        virtual_display_layout: None,
        expected_topology_revision: None,
        expected_topology_domain_id: None,
    }
}
fn create_with_scale(dd_virtual_display_scale: i32) -> Operation {
    Operation::Create {
        hdr_enabled: false,
        identity: ID.into(),
        width: 1920,
        height: 1080,
        refresh_millihz: 60000,
        name: "Test display".into(),
        reference_id: None,
        dd_virtual_display_scale: Some(dd_virtual_display_scale),
        virtual_display_layout: None,
        expected_topology_revision: None,
        expected_topology_domain_id: None,
    }
}
fn create_with_reference(reference_id: &str) -> Operation {
    Operation::Create {
        hdr_enabled: false,
        identity: ID.into(),
        width: 1920,
        height: 1080,
        refresh_millihz: 60000,
        name: "Test display".into(),
        reference_id: Some(reference_id.into()),
        dd_virtual_display_scale: Some(0),
        virtual_display_layout: None,
        expected_topology_revision: None,
        expected_topology_domain_id: None,
    }
}
fn tick(manager: &mut Manager<Fake>) {
    for monitor in manager.monitors.values_mut() {
        monitor.next_poll = Instant::now();
    }
    manager.tick();
}

#[test]
fn extended_monitor_intent_preserves_peer_topology_and_waits_for_exact_capture_readback() {
    let (mut manager, calls) = setup(true);
    let mut extended = create_with_reference("retained-owner");
    if let Operation::Create {
        virtual_display_layout,
        ..
    } = &mut extended
    {
        *virtual_display_layout = Some(crate::model::VirtualDisplayLayout::Extended);
    }
    assert_eq!(manager.handle("remote-plugin", extended)["ok"], true);
    calls.borrow_mut().ready = true;
    tick(&mut manager);
    let status = || Operation::Status {
        identity: Some(ID.into()),
        reference_id: Some("retained-owner".into()),
    };
    let identity_only = manager.handle("remote-plugin", status());
    assert_eq!(identity_only["monitors"][0]["state"], "ready");
    assert_eq!(identity_only["monitors"][0]["ready"]["active"], false);
    assert!(identity_only["monitors"][0]["capture_output"].is_null());
    calls.borrow_mut().capture_ready = true;
    tick(&mut manager);
    assert_eq!(
        manager.handle("remote-plugin", status())["monitors"][0]["capture_output"],
        CAPTURE_OUTPUT
    );
    calls.borrow_mut().ready = false;
    tick(&mut manager);
    assert!(manager.handle("remote-plugin", status())["monitors"][0]["capture_output"].is_null());

    let mut exclusive = create();
    if let Operation::Create {
        virtual_display_layout,
        ..
    } = &mut exclusive
    {
        *virtual_display_layout = Some(crate::model::VirtualDisplayLayout::Exclusive);
    }
    assert_eq!(
        manager.handle("other", exclusive)["code"],
        "unsupported_topology_domain"
    );
    assert_eq!(calls.borrow().creates.len(), 1);
}

#[test]
fn owner_target_verification_requires_trusted_verifier_and_exact_live_reference() {
    let verify = |owner: &str, reference: &str| Operation::VerifyOwnerTarget {
        owner: owner.into(),
        identity: ID.into(),
        reference_id: reference.into(),
    };
    let (mut denied, _) = setup(true);
    assert_eq!(
        denied.handle("amaterasu.display", verify("remote-plugin", "retained"))["code"],
        "permission_denied"
    );
    let (mut manager, calls) = setup_with(Settings {
        enabled: true,
        target_verifiers: vec!["amaterasu.display".into()],
        ..Settings::default()
    });
    assert_eq!(
        manager.handle("remote-plugin", create_with_reference("retained"))["ok"],
        true
    );
    assert_eq!(
        manager.handle("amaterasu.display", verify("remote-plugin", "retained"))["code"],
        "owned_target_unavailable"
    );
    calls.borrow_mut().ready = true;
    tick(&mut manager);
    let proof = manager.handle("amaterasu.display", verify("remote-plugin", "retained"));
    assert_eq!(proof["ok"], true);
    assert_eq!(proof["owned"], true);
    assert_eq!(proof["target_id"], "owned-device");
    assert_eq!(proof["specification"]["identity"], ID);
    assert_eq!(proof["active"], false);
    for (caller, owner, reference) in [
        ("untrusted", "remote-plugin", "retained"),
        ("remote-plugin", "remote-plugin", "retained"),
        ("amaterasu.display", "other-plugin", "retained"),
        ("amaterasu.display", "remote-plugin", "different"),
    ] {
        let result = manager.handle(caller, verify(owner, reference));
        assert_eq!(result["ok"], false);
        assert!(result.get("target_id").is_none());
    }
    manager.handle(
        "remote-plugin",
        Operation::Remove {
            identity: ID.into(),
            reference_id: Some("retained".into()),
        },
    );
    assert_eq!(
        manager.handle("amaterasu.display", verify("remote-plugin", "retained"))["code"],
        "owned_target_unavailable"
    );
    assert_eq!(calls.borrow().creates.len(), 1);
}

#[test]
fn startup_verifier_policy_is_bounded_explicit_and_unambiguous() {
    assert!(Settings::default().target_verifiers.is_empty());
    let trusted: Settings =
        serde_json::from_value(serde_json::json!({"target_verifiers":["amaterasu.display"]}))
            .unwrap();
    assert_eq!(trusted.target_verifiers, ["amaterasu.display"]);
    for invalid in [
        serde_json::json!({"target_verifiers":[""]}),
        serde_json::json!({"target_verifiers":["amaterasu.display", "amaterasu.display"]}),
        serde_json::json!({"target_verifiers":["untrusted principal"]}),
        serde_json::json!({"target_verifiers":(0..17).map(|index|format!("provider-{index}")).collect::<Vec<_>>()}),
    ] {
        assert!(serde_json::from_value::<Settings>(invalid).is_err());
    }
}

#[derive(Default)]
struct RecoveryCalls {
    lease: Option<u64>,
    display: u64,
    capability: [u8; 32],
    creates: usize,
    reclaims: usize,
    removes: usize,
    feeds: Vec<u64>,
    mismatch: bool,
    lost_reclaim_reply: bool,
    shared_lease_count: u32,
}

struct RecoveryFake(Rc<RefCell<RecoveryCalls>>);

impl Driver for RecoveryFake {
    fn supported(&self) -> bool {
        true
    }
    fn create(
        &mut self,
        _: &Specification,
        _: u64,
        _: u64,
        _: &[u8; 32],
    ) -> Result<Binding, &'static str> {
        self.0.borrow_mut().creates += 1;
        Err("unexpected_create")
    }
    fn inspect(
        &mut self,
        lease: u64,
        display: u64,
        _: Option<Binding>,
        _: &Specification,
    ) -> Result<Observation, &'static str> {
        let calls = self.0.borrow();
        let exists = calls.lease == Some(lease) && calls.display == display;
        Ok(Observation {
            exists,
            occupied_by_other_lease: calls.lease.is_some() && !exists && calls.display == display,
            specification_matches: !exists || !calls.mismatch,
            lease_display_count: if exists {
                calls.shared_lease_count.max(1)
            } else {
                0
            },
            pending_departure: false,
            ready: None,
        })
    }
    fn feed(&mut self, lease: u64) -> Result<(), &'static str> {
        self.0.borrow_mut().feeds.push(lease);
        Ok(())
    }
    fn reclaim(
        &mut self,
        display: u64,
        lease: u64,
        capability: &[u8; 32],
    ) -> Result<(), &'static str> {
        let mut calls = self.0.borrow_mut();
        calls.reclaims += 1;
        if calls.lease.is_none() || calls.display != display || &calls.capability != capability {
            return Err("lease_not_found");
        }
        calls.lease = Some(lease);
        if calls.lost_reclaim_reply {
            Err("driver_io_timeout")
        } else {
            Ok(())
        }
    }
    fn remove(&mut self, lease: u64, display: u64) -> Result<(), &'static str> {
        let mut calls = self.0.borrow_mut();
        calls.removes += 1;
        if calls.lease == Some(lease) && calls.display == display {
            calls.lease = None;
        }
        Ok(())
    }
}

fn recovery_journal() -> (Journal, PathBuf) {
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).unwrap();
    let root = std::env::temp_dir().join(format!(
        "amaterasu-vd-recovery-{:016x}",
        u64::from_le_bytes(nonce)
    ));
    let journal = Journal::open(&root.join("leases.jsonl")).unwrap();
    (journal, root)
}

fn recovery_record(prospective_lease: Option<u64>) -> Record {
    let specification = match create() {
        Operation::Create {
            identity,
            width,
            height,
            refresh_millihz,
            name,
            ..
        } => Specification {
            hdr_enabled: false,
            identity,
            width,
            height,
            refresh_millihz,
            name,
        },
        _ => unreachable!(),
    };
    Record {
        owner: "alice".into(),
        display_id: display_id("alice", ID),
        specification,
        lease: 0x6000_0000_0000_0001,
        prospective_lease,
        capability: [7; 32],
        binding: None,
        references: None,
    }
}

#[test]
fn restart_reclaims_prepared_owned_create_without_replaying_create() {
    let (mut journal, root) = recovery_journal();
    let record = recovery_record(None);
    journal.upsert(record.clone()).unwrap();
    let calls = Rc::new(RefCell::new(RecoveryCalls {
        lease: Some(record.lease),
        display: record.display_id,
        capability: record.capability,
        ..Default::default()
    }));
    let mut manager = Manager::with_journal(
        Settings {
            enabled: true,
            ..Settings::default()
        },
        RecoveryFake(calls.clone()),
        journal,
    )
    .unwrap();
    assert_eq!(calls.borrow().reclaims, 1);
    assert_ne!(
        manager.monitors.values().next().unwrap().lease,
        record.lease
    );
    assert_eq!(
        manager.handle("alice", create())["monitor"]["state"],
        "recovered"
    );
    assert!(
        manager.handle(
            "bob",
            Operation::Status {
                identity: None,
                reference_id: None
            }
        )["monitors"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        manager.handle(
            "bob",
            Operation::Remove {
                identity: ID.into(),
                reference_id: None,
            }
        )["removed"],
        true
    );
    assert_eq!(calls.borrow().creates, 0);
    drop(manager);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn restart_restores_stable_references_and_resumes_released_cleanup() {
    let (mut journal, root) = recovery_journal();
    let mut record = recovery_record(None);
    record.references = Some(BTreeSet::from(["stream-a".into()]));
    journal.upsert(record.clone()).unwrap();
    let calls = Rc::new(RefCell::new(RecoveryCalls {
        lease: Some(record.lease),
        display: record.display_id,
        capability: record.capability,
        ..Default::default()
    }));
    let mut manager = Manager::with_journal(
        Settings {
            enabled: true,
            ..Settings::default()
        },
        RecoveryFake(calls.clone()),
        journal,
    )
    .unwrap();
    let status = manager.handle(
        "alice",
        Operation::Status {
            identity: Some(ID.into()),
            reference_id: Some("stream-a".into()),
        },
    );
    assert_eq!(status["reference_owned"], true);
    assert_eq!(status["monitors"][0]["state"], "recovered");
    assert_eq!(calls.borrow().creates, 0);
    drop(manager);
    std::fs::remove_dir_all(&root).unwrap();

    let (mut journal, root) = recovery_journal();
    let mut record = recovery_record(None);
    record.references = Some(BTreeSet::new());
    journal.upsert(record.clone()).unwrap();
    let calls = Rc::new(RefCell::new(RecoveryCalls {
        lease: Some(record.lease),
        display: record.display_id,
        capability: record.capability,
        ..Default::default()
    }));
    let mut manager = Manager::with_journal(
        Settings {
            enabled: true,
            ..Settings::default()
        },
        RecoveryFake(calls.clone()),
        journal,
    )
    .unwrap();
    assert_eq!(manager.monitors.values().next().unwrap().state, "removing");
    manager.tick();
    assert_eq!(calls.borrow().removes, 1);
    assert!(manager.monitors.is_empty());
    assert_eq!(calls.borrow().creates, 0);
    drop(manager);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn recovered_lease_without_binding_survives_readiness_timeout_and_owner_removes_it() {
    let (mut journal, root) = recovery_journal();
    let record = recovery_record(None);
    journal.upsert(record.clone()).unwrap();
    let calls = Rc::new(RefCell::new(RecoveryCalls {
        lease: Some(record.lease),
        display: record.display_id,
        capability: record.capability,
        ..Default::default()
    }));
    let mut manager = Manager::with_journal(
        Settings {
            enabled: true,
            ..Settings::default()
        },
        RecoveryFake(calls.clone()),
        journal,
    )
    .unwrap();
    let recovered_lease = manager.monitors.values().next().unwrap().lease;
    let monitor = manager.monitors.values_mut().next().unwrap();
    monitor.readiness_started = Instant::now() - READINESS_TIMEOUT - Duration::from_secs(1);
    monitor.next_poll = Instant::now();
    monitor.next_feed = Instant::now();
    manager.tick();
    assert_eq!(manager.monitors.values().next().unwrap().state, "recovered");
    assert_eq!(calls.borrow().feeds, vec![recovered_lease]);
    assert_eq!(calls.borrow().removes, 0);
    assert_eq!(
        manager.handle(
            "alice",
            Operation::Remove {
                identity: ID.into(),
                reference_id: None,
            }
        )["monitor"]["state"],
        "removing"
    );
    manager.monitors.values_mut().next().unwrap().next_poll = Instant::now();
    manager.tick();
    assert!(manager.monitors.is_empty());
    assert_eq!(calls.borrow().removes, 1);
    assert_eq!(manager.journal.as_ref().unwrap().records().count(), 0);
    drop(manager);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn lost_reclaim_reply_adopts_only_the_recorded_new_lease() {
    let (mut journal, root) = recovery_journal();
    let new_lease = 0x6000_0000_0000_0002;
    let record = recovery_record(Some(new_lease));
    journal.upsert(record.clone()).unwrap();
    let calls = Rc::new(RefCell::new(RecoveryCalls {
        lease: Some(new_lease),
        display: record.display_id,
        capability: record.capability,
        ..Default::default()
    }));
    let manager = Manager::with_journal(
        Settings {
            enabled: true,
            ..Settings::default()
        },
        RecoveryFake(calls.clone()),
        journal,
    )
    .unwrap();
    assert_eq!(manager.monitors.values().next().unwrap().lease, new_lease);
    assert_eq!(calls.borrow().reclaims, 0);
    assert_eq!(
        manager
            .journal
            .as_ref()
            .unwrap()
            .records()
            .next()
            .unwrap()
            .prospective_lease,
        None
    );
    drop(manager);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ambiguous_reclaim_reply_remains_prepared_until_exact_readback() {
    let (mut journal, root) = recovery_journal();
    let new_lease = 0x6000_0000_0000_0002;
    let record = recovery_record(Some(new_lease));
    journal.upsert(record.clone()).unwrap();
    let calls = Rc::new(RefCell::new(RecoveryCalls {
        lease: Some(record.lease),
        display: record.display_id,
        capability: record.capability,
        lost_reclaim_reply: true,
        ..Default::default()
    }));
    let mut manager = Manager::with_journal(
        Settings {
            enabled: true,
            ..Settings::default()
        },
        RecoveryFake(calls.clone()),
        journal,
    )
    .unwrap();
    assert_eq!(
        manager.monitors.values().next().unwrap().state,
        "reclaim_pending"
    );
    assert_eq!(calls.borrow().lease, Some(new_lease));
    assert_eq!(
        manager
            .journal
            .as_ref()
            .unwrap()
            .records()
            .next()
            .unwrap()
            .prospective_lease,
        Some(new_lease)
    );
    manager.monitors.values_mut().next().unwrap().next_poll = Instant::now();
    manager.tick();
    assert_eq!(manager.monitors.values().next().unwrap().state, "recovered");
    assert_eq!(manager.monitors.values().next().unwrap().lease, new_lease);
    assert_eq!(calls.borrow().reclaims, 1);
    drop(manager);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_capability_and_specification_mismatch_fail_closed() {
    for mismatch in [false, true] {
        let (mut journal, root) = recovery_journal();
        let record = recovery_record(None);
        journal.upsert(record.clone()).unwrap();
        let calls = Rc::new(RefCell::new(RecoveryCalls {
            lease: Some(record.lease),
            display: record.display_id,
            capability: [9; 32],
            mismatch,
            ..Default::default()
        }));
        let mut manager = Manager::with_journal(
            Settings {
                enabled: true,
                ..Settings::default()
            },
            RecoveryFake(calls.clone()),
            journal,
        )
        .unwrap();
        assert_eq!(calls.borrow().creates, 0);
        assert_eq!(calls.borrow().removes, 0);
        if !mismatch {
            assert_eq!(
                manager.handle(
                    "alice",
                    Operation::Remove {
                        identity: ID.into(),
                        reference_id: None,
                    }
                )["code"],
                "recovery_pending"
            );
        }
        assert_eq!(calls.borrow().removes, 0);
        assert_eq!(manager.handle("alice", create())["ok"], false);
        assert!(manager.journal.as_ref().unwrap().records().next().is_some());
        drop(manager);
        assert_eq!(calls.borrow().removes, 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn shared_or_foreign_lease_never_reclaims_or_clears_ownership() {
    for (lease, prospective, shared_count) in [
        (0x6000_0000_0000_0001, None, 2),
        (0x6000_0000_0000_0002, Some(0x6000_0000_0000_0002), 2),
        (0x6000_0000_0000_0003, Some(0x6000_0000_0000_0002), 1),
    ] {
        let (mut journal, root) = recovery_journal();
        let record = recovery_record(prospective);
        journal.upsert(record.clone()).unwrap();
        let calls = Rc::new(RefCell::new(RecoveryCalls {
            lease: Some(lease),
            display: record.display_id,
            capability: record.capability,
            shared_lease_count: shared_count,
            ..Default::default()
        }));
        let mut manager = Manager::with_journal(
            Settings {
                enabled: true,
                ..Settings::default()
            },
            RecoveryFake(calls.clone()),
            journal,
        )
        .unwrap();
        assert_eq!(
            manager.monitors.values().next().unwrap().state,
            "ownership_conflict"
        );
        assert_eq!(
            manager.handle("alice", create())["code"],
            "ownership_conflict"
        );
        assert_eq!(
            manager.handle(
                "alice",
                Operation::Remove {
                    identity: ID.into(),
                    reference_id: None,
                }
            )["code"],
            "ownership_conflict"
        );
        assert_eq!(calls.borrow().reclaims, 0);
        assert_eq!(calls.borrow().removes, 0);
        assert_eq!(manager.journal.as_ref().unwrap().records().count(), 1);
        drop(manager);
        assert_eq!(calls.borrow().removes, 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}

fn create_payload() -> serde_json::Value {
    json!({
        "action": "create",
        "identity": ID,
        "width": 1920,
        "height": 1080,
        "refresh_millihz": 60000,
        "name": "Test display"
    })
}

#[test]
fn omitted_create_scale_defaults_to_retain_and_preserves_old_payloads() {
    let operation = serde_json::from_value::<Operation>(create_payload()).unwrap();
    let Operation::Create {
        dd_virtual_display_scale,
        ..
    } = &operation
    else {
        panic!("create payload should deserialize as Create");
    };
    // An omitted scale falls back to the module setting, which retains.
    assert_eq!(*dd_virtual_display_scale, None);

    let (mut manager, calls) = setup(true);
    assert_eq!(manager.handle("alice", operation)["ok"], true);
    assert_eq!(calls.borrow().creates.len(), 1);
}

#[test]
fn create_scale_accepts_only_supported_integer_values() {
    for scale in SUPPORTED_SCALE_VALUES {
        let mut payload = create_payload();
        payload["dd_virtual_display_scale"] = json!(scale);
        let operation = serde_json::from_value::<Operation>(payload).unwrap();
        let (mut manager, calls) = setup(true);
        let response = manager.handle("alice", operation);

        if scale == 0 {
            assert_eq!(response["ok"], true);
            assert_eq!(calls.borrow().creates.len(), 1);
        } else {
            assert_eq!(response["code"], "unsupported_scale");
            assert_eq!(calls.borrow().supported_calls, 0);
            assert!(calls.borrow().creates.is_empty());
        }
    }

    for scale in [-2, 1, 99, 126, 550, i32::MIN, i32::MAX] {
        let mut payload = create_payload();
        payload["dd_virtual_display_scale"] = json!(scale);
        let operation = serde_json::from_value::<Operation>(payload).unwrap();
        let (mut manager, calls) = setup(true);
        let response = manager.handle("alice", operation);
        assert_eq!(response["code"], "invalid_request");
        assert_eq!(calls.borrow().supported_calls, 0);
        assert!(calls.borrow().creates.is_empty());
    }
}

#[test]
fn create_scale_rejects_non_integer_json_types_and_out_of_range_integers() {
    for scale in [
        json!(100.0),
        json!(125.5),
        json!("100"),
        json!(true),
        json!(null),
        json!(2_147_483_648_u64),
        json!(-2_147_483_649_i64),
    ] {
        let mut payload = create_payload();
        payload["dd_virtual_display_scale"] = scale;
        assert!(serde_json::from_value::<Operation>(payload).is_err());
    }
}

#[test]
fn valid_nonzero_scale_fails_before_driver_calls() {
    let (mut manager, calls) = setup(true);

    assert_eq!(
        manager.handle("alice", create_with_scale(175))["code"],
        "unsupported_scale"
    );
    assert_eq!(calls.borrow().supported_calls, 0);
    assert!(calls.borrow().creates.is_empty());
}

#[test]
fn disabled_never_touches_driver() {
    let (mut manager, calls) = setup(false);
    assert_eq!(manager.handle("alice", create())["code"], "disabled");
    tick(&mut manager);
    drop(manager);
    assert!(calls.borrow().creates.is_empty());
    assert!(calls.borrow().removes.is_empty());
}

#[test]
fn driver_selection_rejects_sunshine_driver_without_native_effects() {
    let (mut manager, calls) = setup_with(Settings {
        enabled: true,
        dd_activate_virtual_display: false,
        dd_use_sunshine_virtual_display_driver: false,
        ..Settings::default()
    });

    assert_eq!(
        manager.handle("alice", create())["code"],
        "unsupported_driver"
    );
    let status = manager.handle(
        "alice",
        Operation::Status {
            identity: None,
            reference_id: None,
        },
    );
    assert_eq!(status["dd_activate_virtual_display"], false);
    assert_eq!(status["dd_use_sunshine_virtual_display_driver"], false);
    assert_eq!(status["driver"], "unsupported");
    assert_eq!(status["activation_state"], "not_requested");
    assert!(status["activation_error"].is_null());
    assert!(calls.borrow().creates.is_empty());
}

#[test]
fn requested_activation_does_not_change_explicit_create_and_status_reports_gap() {
    let (mut manager, calls) = setup_with(Settings {
        enabled: true,
        dd_activate_virtual_display: true,
        ..Settings::default()
    });

    let response = manager.handle("alice", create());
    assert_eq!(response["ok"], true);
    assert_eq!(response["monitor"]["state"], "creating");
    assert!(response["monitor"]["ready"].is_null());
    assert_eq!(calls.borrow().creates.len(), 1);

    let status = manager.handle(
        "alice",
        Operation::Status {
            identity: None,
            reference_id: None,
        },
    );
    assert_eq!(status["enabled"], true);
    assert_eq!(status["dd_activate_virtual_display"], true);
    assert_eq!(status["activation_supported"], false);
    assert_eq!(status["activation_state"], "unsupported");
    assert_eq!(status["activation_error"], "unsupported_activation");
}

#[test]
fn activation_not_requested_keeps_temporary_create_available() {
    let (mut manager, calls) = setup(true);

    assert_eq!(manager.handle("alice", create())["ok"], true);
    assert_eq!(calls.borrow().creates.len(), 1);
}

#[test]
fn settings_default_to_reference_values_and_accept_stable_names() {
    let defaults = serde_json::from_value::<Settings>(json!({})).unwrap();
    assert!(!defaults.enabled);
    assert!(!defaults.dd_activate_virtual_display);
    assert!(defaults.dd_use_sunshine_virtual_display_driver);

    let configured = serde_json::from_value::<Settings>(json!({
        "enabled": true,
        "dd_activate_virtual_display": true,
        "dd_use_sunshine_virtual_display_driver": false
    }))
    .unwrap();
    assert!(configured.enabled);
    assert!(configured.dd_activate_virtual_display);
    assert!(!configured.dd_use_sunshine_virtual_display_driver);
    assert_eq!(configured.dd_virtual_display_permanent_count, None);
}

#[test]
fn virtual_display_layout_defaults_to_exclusive_and_refuses_unimplemented_variants() {
    use crate::model::VirtualDisplayLayout;
    let defaults = serde_json::from_value::<Settings>(json!({})).unwrap();
    assert_eq!(
        defaults.virtual_display_layout,
        VirtualDisplayLayout::Exclusive
    );
    let extended =
        serde_json::from_value::<Settings>(json!({"virtual_display_layout": "extended"})).unwrap();
    assert_eq!(
        extended.virtual_display_layout,
        VirtualDisplayLayout::Extended
    );
    for unimplemented in [
        "extended_primary",
        "extended_isolated",
        "extended_primary_isolated",
    ] {
        assert!(
            serde_json::from_value::<Settings>(json!({"virtual_display_layout": unimplemented}))
                .is_err(),
            "{unimplemented}"
        );
    }
}

#[test]
fn permanent_monitor_count_preserves_unset_and_explicit_zero() {
    let defaults = serde_json::from_value::<Settings>(json!({})).unwrap();
    assert_eq!(defaults.dd_virtual_display_permanent_count, None);

    let explicit_zero = serde_json::from_value::<Settings>(json!({
        "dd_virtual_display_permanent_count": 0
    }))
    .unwrap();
    assert_eq!(explicit_zero.dd_virtual_display_permanent_count, Some(0));

    let maximum = serde_json::from_value::<Settings>(json!({
        "dd_virtual_display_permanent_count": 4
    }))
    .unwrap();
    assert_eq!(maximum.dd_virtual_display_permanent_count, Some(4));
}

#[test]
fn permanent_monitor_count_accepts_legacy_alias_and_canonical_wins() {
    let alias = serde_json::from_value::<Settings>(json!({
        "dd_vdd_static_monitor_count": 3
    }))
    .unwrap();
    assert_eq!(alias.dd_virtual_display_permanent_count, Some(3));

    let both = serde_json::from_value::<Settings>(json!({
        "dd_virtual_display_permanent_count": 1,
        "dd_vdd_static_monitor_count": 4
    }))
    .unwrap();
    assert_eq!(both.dd_virtual_display_permanent_count, Some(1));

    let canonical_wins_over_invalid_alias = serde_json::from_value::<Settings>(json!({
        "dd_virtual_display_permanent_count": 2,
        "dd_vdd_static_monitor_count": "ignored"
    }))
    .unwrap();
    assert_eq!(
        canonical_wins_over_invalid_alias.dd_virtual_display_permanent_count,
        Some(2)
    );

    let canonical_out_of_range = serde_json::from_value::<Settings>(json!({
        "dd_virtual_display_permanent_count": 5,
        "dd_vdd_static_monitor_count": 2
    }))
    .unwrap();
    assert_eq!(
        canonical_out_of_range.dd_virtual_display_permanent_count,
        Some(0)
    );

    let alias_out_of_range = serde_json::from_value::<Settings>(json!({
        "dd_vdd_static_monitor_count": -1
    }))
    .unwrap();
    assert_eq!(
        alias_out_of_range.dd_virtual_display_permanent_count,
        Some(0)
    );
}

#[test]
fn permanent_monitor_count_rejects_non_integer_values() {
    for invalid in [json!(1.5), json!("2"), json!(null), json!(true)] {
        assert!(
            serde_json::from_value::<Settings>(json!({
                "dd_virtual_display_permanent_count": invalid.clone()
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<Settings>(json!({
                "dd_vdd_static_monitor_count": invalid
            }))
            .is_err()
        );
    }
}

#[test]
fn status_reports_permanent_count_and_unsupported_runtime() {
    let (mut unconfigured_manager, _) = setup_with(Settings::default());
    let unconfigured_status = unconfigured_manager.handle(
        "alice",
        Operation::Status {
            identity: None,
            reference_id: None,
        },
    );
    assert_eq!(unconfigured_status["dd_virtual_display_permanent_count"], 0);
    assert_eq!(
        unconfigured_status["dd_virtual_display_permanent_count_configured"],
        false
    );
    assert_eq!(
        unconfigured_status["permanent_monitor_runtime_supported"],
        false
    );

    let settings = serde_json::from_value::<Settings>(json!({
        "dd_vdd_static_monitor_count": 2
    }))
    .unwrap();
    let (mut manager, _) = setup_with(settings);
    let status = manager.handle(
        "alice",
        Operation::Status {
            identity: None,
            reference_id: None,
        },
    );

    assert_eq!(status["dd_virtual_display_permanent_count"], 2);
    assert_eq!(
        status["dd_virtual_display_permanent_count_configured"],
        true
    );
    assert_eq!(status["permanent_monitor_runtime_supported"], false);
    assert_eq!(status["permanent_monitor_runtime_state"], "unsupported");
}

#[test]
fn module_enabled_setting_is_independent_of_display_activation() {
    let settings = serde_json::from_value::<Settings>(json!({"enabled": true})).unwrap();
    assert!(settings.enabled);
    assert!(!settings.dd_activate_virtual_display);
}
#[test]
fn identity_is_idempotent_and_owned_by_caller() {
    let (mut manager, calls) = setup(true);
    let first = manager.handle("alice", create());
    assert_eq!(first, manager.handle("alice", create()));
    manager.handle(
        "bob",
        Operation::Remove {
            identity: ID.into(),
            reference_id: None,
        },
    );
    assert!(calls.borrow().removes.is_empty());
    assert_eq!(
        manager.handle(
            "bob",
            Operation::Status {
                identity: None,
                reference_id: None
            }
        )["monitors"],
        json!([])
    );
    let second = manager.handle("bob", create());
    assert_ne!(
        first["monitor"]["display_id"],
        second["monitor"]["display_id"]
    );
    assert_eq!(calls.borrow().creates.len(), 2);
    let serialized = first.to_string();
    assert!(!serialized.contains("lease"));
    assert!(!serialized.contains("capability"));
    drop(manager);
    assert_eq!(calls.borrow().creates, calls.borrow().removes);
}

#[test]
fn shared_monitor_lives_until_its_last_stable_reference_is_released() {
    let (mut manager, calls) = setup(true);
    let first = manager.handle("alice", create_with_reference("stream-a"));
    let second = manager.handle("alice", create_with_reference("stream-b"));
    assert_eq!(first["ok"], true);
    assert!(!first.to_string().contains("stream-a"));
    assert_eq!(second["ok"], true);
    assert_eq!(
        second["monitor"]["display_id"],
        first["monitor"]["display_id"]
    );
    assert_eq!(calls.borrow().creates.len(), 1);

    let duplicate = manager.handle("alice", create_with_reference("stream-a"));
    assert_eq!(duplicate, first);
    assert_eq!(calls.borrow().creates.len(), 1);

    let first_status = manager.handle(
        "alice",
        Operation::Status {
            identity: Some(ID.into()),
            reference_id: Some("stream-a".into()),
        },
    );
    assert_eq!(first_status["reference_owned"], true);
    assert!(!first_status.to_string().contains("stream-a"));
    let released_first = manager.handle(
        "alice",
        Operation::Remove {
            identity: ID.into(),
            reference_id: Some("stream-a".into()),
        },
    );
    assert_eq!(released_first["ok"], true);
    assert_eq!(released_first["removed"], false);
    assert_eq!(released_first["reference_released"], true);
    assert!(calls.borrow().removes.is_empty());

    let second_status = manager.handle(
        "alice",
        Operation::Status {
            identity: Some(ID.into()),
            reference_id: Some("stream-b".into()),
        },
    );
    assert_eq!(second_status["reference_owned"], true);
    let released_second = manager.handle(
        "alice",
        Operation::Remove {
            identity: ID.into(),
            reference_id: Some("stream-b".into()),
        },
    );
    assert_eq!(released_second["monitor"]["state"], "removing");
    calls.borrow_mut().remove_completes = true;
    tick(&mut manager);
    assert!(manager.monitors.is_empty());
    assert_eq!(calls.borrow().removes.len(), 1);
}

#[test]
fn lost_shared_reference_reply_releases_only_the_new_reference() {
    let (mut manager, calls) = setup(true);
    manager.handle("alice", create_with_reference("stream-a"));

    let (second, admission) = manager.admit("alice", create_with_reference("stream-b"));
    assert_eq!(second["reference_owned"], true);
    assert!(admission.is_some());
    assert_eq!(calls.borrow().creates.len(), 1);
    manager.completion_lost(admission);

    let first_status = manager.handle(
        "alice",
        Operation::Status {
            identity: Some(ID.into()),
            reference_id: Some("stream-a".into()),
        },
    );
    let second_status = manager.handle(
        "alice",
        Operation::Status {
            identity: Some(ID.into()),
            reference_id: Some("stream-b".into()),
        },
    );
    assert_eq!(first_status["reference_owned"], true);
    assert_eq!(second_status["reference_owned"], false);
    assert_eq!(second_status["monitors"][0]["state"], "creating");
    assert!(calls.borrow().removes.is_empty());
}

#[test]
fn reference_ids_are_bounded_printable_and_caller_scoped() {
    assert!(!crate::model::valid_reference_id(""));
    assert!(crate::model::valid_reference_id(&"a".repeat(128)));
    assert!(!crate::model::valid_reference_id(&"a".repeat(129)));
    assert!(!crate::model::valid_reference_id("stream\nA"));

    let (mut manager, calls) = setup(true);
    manager.handle("alice", create_with_reference("stream-a"));
    let unscoped_status = manager.handle(
        "alice",
        Operation::Status {
            identity: None,
            reference_id: Some("stream-a".into()),
        },
    );
    assert_eq!(unscoped_status["code"], "invalid_request");
    let unauthorized_release = manager.handle(
        "bob",
        Operation::Remove {
            identity: ID.into(),
            reference_id: Some("stream-a".into()),
        },
    );
    assert_eq!(unauthorized_release["removed"], true);
    assert!(calls.borrow().removes.is_empty());
    assert_eq!(manager.monitors.len(), 1);
}
#[test]
fn readiness_and_feeding_track_exact_owned_lease() {
    let (mut manager, calls) = setup(true);
    manager.handle("alice", create());
    calls.borrow_mut().ready = true;
    manager.monitors.values_mut().next().unwrap().next_feed = Instant::now();
    tick(&mut manager);
    assert_eq!(manager.monitors.values().next().unwrap().state, "ready");
    assert_eq!(calls.borrow().feeds, vec![calls.borrow().creates[0].0]);
    tick(&mut manager);
    assert_eq!(calls.borrow().feeds.len(), 1);
    assert_eq!(manager.monitors.values().next().unwrap().state, "ready");
    manager.monitors.values_mut().next().unwrap().next_feed = Instant::now();
    tick(&mut manager);
    assert_eq!(calls.borrow().feeds, vec![calls.borrow().creates[0].0; 2]);
    assert_eq!(manager.monitors.values().next().unwrap().state, "ready");
}
#[test]
fn ambiguous_create_is_not_replayed_and_cleanup_waits_for_departure() {
    let (mut manager, calls) = setup(true);
    calls.borrow_mut().ambiguous_create = true;
    assert_eq!(
        manager.handle("alice", create())["monitor"]["state"],
        "removing"
    );
    manager.handle("alice", create());
    assert_eq!(calls.borrow().creates.len(), 1);
    calls.borrow_mut().exists = false;
    calls.borrow_mut().pending = true;
    for _ in 0..5 {
        tick(&mut manager);
    }
    assert_eq!(calls.borrow().removes.len(), 3);
    assert_eq!(
        manager.monitors.values().next().unwrap().state,
        "cleanup_failed"
    );
    assert!(
        calls
            .borrow()
            .removes
            .iter()
            .all(|entry| *entry == calls.borrow().creates[0])
    );
    calls.borrow_mut().pending = false;
    tick(&mut manager);
    assert!(manager.monitors.is_empty());
    assert!(calls.borrow().feeds.is_empty());
}
#[test]
fn only_owned_explicit_remove_retries_exhausted_exact_cleanup() {
    let (mut manager, calls) = setup(true);
    manager.handle("alice", create());
    let owned = calls.borrow().creates[0];
    calls.borrow_mut().remove_failure = true;
    manager.handle(
        "alice",
        Operation::Remove {
            identity: ID.into(),
            reference_id: None,
        },
    );
    for _ in 0..5 {
        tick(&mut manager);
    }
    assert_eq!(calls.borrow().removes, vec![owned; 3]);
    assert_eq!(
        manager.monitors.values().next().unwrap().state,
        "cleanup_failed"
    );
    assert!(calls.borrow().feeds.is_empty());

    manager.handle(
        "bob",
        Operation::Remove {
            identity: ID.into(),
            reference_id: None,
        },
    );
    manager.handle(
        "alice",
        Operation::Remove {
            identity: "00000000-0000-0000-0000-000000000002".into(),
            reference_id: None,
        },
    );
    tick(&mut manager);
    assert_eq!(calls.borrow().removes, vec![owned; 3]);
    assert_eq!(
        manager.monitors.values().next().unwrap().state,
        "cleanup_failed"
    );

    calls.borrow_mut().remove_failure = false;
    calls.borrow_mut().remove_completes = true;
    manager.handle(
        "alice",
        Operation::Remove {
            identity: ID.into(),
            reference_id: None,
        },
    );
    tick(&mut manager);
    assert_eq!(calls.borrow().removes, vec![owned; 4]);
    assert!(manager.monitors.is_empty());
    assert!(calls.borrow().feeds.is_empty());
}
#[test]
fn unpublished_monitor_expires_into_cleanup_and_identity_can_be_recreated() {
    let (mut manager, calls) = setup(true);
    let first = manager.handle("alice", create());
    manager
        .monitors
        .values_mut()
        .next()
        .unwrap()
        .readiness_started = Instant::now() - READINESS_TIMEOUT;
    tick(&mut manager);
    assert_eq!(
        manager.monitors.values().next().unwrap().error,
        Some("readiness_timeout")
    );
    calls.borrow_mut().exists = false;
    tick(&mut manager);
    let second = manager.handle("alice", create());
    assert_eq!(
        first["monitor"]["display_id"],
        second["monitor"]["display_id"]
    );
    assert_ne!(calls.borrow().creates[0].0, calls.borrow().creates[1].0);
}

#[test]
fn published_identity_loss_clears_stale_path_and_can_recover() {
    let (mut manager, calls) = setup(true);
    manager.handle("alice", create());
    calls.borrow_mut().ready = true;
    tick(&mut manager);
    assert_eq!(manager.monitors.values().next().unwrap().state, "ready");

    calls.borrow_mut().ready = false;
    tick(&mut manager);
    let status = manager.handle(
        "alice",
        Operation::Status {
            identity: None,
            reference_id: None,
        },
    );
    assert_eq!(status["monitors"][0]["state"], "creating");
    assert!(status["monitors"][0]["ready"].is_null());
    assert!(calls.borrow().removes.is_empty());

    calls.borrow_mut().ready = true;
    tick(&mut manager);
    let status = manager.handle(
        "alice",
        Operation::Status {
            identity: None,
            reference_id: None,
        },
    );
    assert_eq!(status["monitors"][0]["state"], "ready");
    assert_eq!(
        status["monitors"][0]["ready"]["device_path"],
        "owned-device"
    );
    assert!(calls.borrow().removes.is_empty());
}

#[test]
fn prolonged_published_identity_loss_cleans_only_the_owned_lease() {
    let (mut manager, calls) = setup(true);
    manager.handle("alice", create());
    calls.borrow_mut().ready = true;
    tick(&mut manager);
    calls.borrow_mut().ready = false;
    tick(&mut manager);
    manager
        .monitors
        .values_mut()
        .next()
        .unwrap()
        .readiness_started = Instant::now() - READINESS_TIMEOUT;
    tick(&mut manager);
    let monitor = manager.monitors.values().next().unwrap();
    assert_eq!(monitor.state, "removing");
    assert_eq!(monitor.error, Some("readiness_timeout"));
    assert!(monitor.ready.is_none());
    tick(&mut manager);
    assert_eq!(calls.borrow().removes, calls.borrow().creates);
}

#[test]
fn departing_lease_revokes_readiness_and_stops_feeding_until_departure() {
    let (mut manager, calls) = setup(true);
    manager.handle("alice", create());
    let owned = calls.borrow().creates[0];
    calls.borrow_mut().ready = true;
    tick(&mut manager);
    assert_eq!(manager.monitors.values().next().unwrap().state, "ready");

    calls.borrow_mut().pending = true;
    manager.monitors.values_mut().next().unwrap().next_feed = Instant::now();
    tick(&mut manager);
    let departing = manager.handle(
        "alice",
        Operation::Status {
            identity: None,
            reference_id: None,
        },
    );
    assert_eq!(departing["monitors"][0]["state"], "removing");
    assert_eq!(departing["monitors"][0]["error"], "lease_departing");
    assert!(departing["monitors"][0]["ready"].is_null());

    tick(&mut manager);
    assert_eq!(calls.borrow().removes, vec![owned]);
    assert!(calls.borrow().feeds.is_empty());
    assert_eq!(manager.monitors.len(), 1);

    calls.borrow_mut().exists = false;
    tick(&mut manager);
    assert_eq!(manager.monitors.len(), 1);
    calls.borrow_mut().pending = false;
    tick(&mut manager);
    assert!(manager.monitors.is_empty());
}

#[test]
fn departing_lease_during_creation_does_not_wait_for_readiness_timeout() {
    let (mut manager, calls) = setup(true);
    manager.handle("alice", create());
    calls.borrow_mut().pending = true;
    tick(&mut manager);

    let departing = manager.handle(
        "alice",
        Operation::Status {
            identity: None,
            reference_id: None,
        },
    );
    assert_eq!(departing["monitors"][0]["state"], "removing");
    assert_eq!(departing["monitors"][0]["error"], "lease_departing");
    assert!(departing["monitors"][0]["ready"].is_null());
    assert_eq!(calls.borrow().creates.len(), 1);
}

#[test]
fn lost_completion_cleans_new_monitor_but_preserves_idempotent_retry() {
    let (mut manager, calls) = setup(true);
    let (_, admission) = manager.admit("alice", create());
    assert!(admission.is_some());
    manager.completion_lost(admission);
    manager.monitors.values_mut().next().unwrap().next_feed = Instant::now();
    tick(&mut manager);
    assert!(calls.borrow().feeds.is_empty());
    assert_eq!(calls.borrow().removes, calls.borrow().creates);
    calls.borrow_mut().exists = false;
    tick(&mut manager);
    assert!(manager.monitors.is_empty());

    let (_, accepted) = manager.admit("alice", create());
    assert!(accepted.is_some());
    let (_, retry) = manager.admit("alice", create());
    assert!(retry.is_none());
    manager.completion_lost(retry);
    assert_eq!(manager.monitors.values().next().unwrap().state, "creating");
    calls.borrow_mut().ready = true;
    manager.monitors.values_mut().next().unwrap().next_feed = Instant::now();
    tick(&mut manager);
    assert_eq!(manager.monitors.values().next().unwrap().state, "ready");
    assert_eq!(calls.borrow().feeds.len(), 1);
}
