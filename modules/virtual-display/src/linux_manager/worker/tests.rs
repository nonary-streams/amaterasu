use super::*;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct FakeBroker {
    outputs: Arc<Mutex<BTreeMap<String, bool>>>,
    effects: Arc<Mutex<Vec<(String, bool)>>>,
    journal: PathBuf,
    reject_removal: Arc<Mutex<bool>>,
    reject_connection: Arc<Mutex<bool>>,
}
impl Broker for FakeBroker {
    fn connected(&self, output: &str) -> Result<bool, String> {
        Ok(*self.outputs.lock().unwrap().get(output).unwrap_or(&false))
    }
    fn set_connected(&self, output: &str, connected: bool) -> Result<(), String> {
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(&self.journal).unwrap()).unwrap();
        assert!(
            document["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["output"] == output),
            "native effect preceded durable ownership intent"
        );
        assert!(
            document["records"]
                .as_array()
                .unwrap()
                .iter()
                .find(|record| record["output"] == output)
                .unwrap()["layout"]
                .is_object(),
            "hotplug effect preceded the durable existing-layout snapshot"
        );
        if !connected && *self.reject_removal.lock().unwrap() {
            return Err("broker unavailable".into());
        }
        if connected && *self.reject_connection.lock().unwrap() {
            return Err("connector owned by another uid".into());
        }
        self.effects
            .lock()
            .unwrap()
            .push((output.into(), connected));
        self.outputs
            .lock()
            .unwrap()
            .insert(output.into(), connected);
        Ok(())
    }
}
/// Connector names the fake vibeshine card exposes, or a discovery failure.
#[derive(Clone)]
struct FakeConnectors(Arc<Mutex<Result<Vec<String>, String>>>);
impl FakeConnectors {
    fn named(range: std::ops::RangeInclusive<u32>) -> Self {
        Self(Arc::new(Mutex::new(Ok(range
            .map(|index| format!("Virtual-{index}"))
            .collect()))))
    }
}
impl ConnectorInventory for FakeConnectors {
    fn connectors(&self) -> Result<Vec<String>, String> {
        self.0.lock().unwrap().clone()
    }
}
struct FakeCompositor {
    fail: bool,
    fail_restore: bool,
    rejected_configuration_layout: Option<serde_json::Value>,
    snapshot_x: i64,
    fail_restore_after: Option<usize>,
    guard_available: bool,
    parked_observable: bool,
    placements: Vec<(String, Placement)>,
    restored: Vec<bool>,
    mode_journal: Option<PathBuf>,
    setup_journal: Option<PathBuf>,
}
impl FakeCompositor {
    fn new() -> Self {
        Self {
            fail: false,
            fail_restore: false,
            rejected_configuration_layout: None,
            snapshot_x: 0,
            fail_restore_after: None,
            guard_available: true,
            parked_observable: true,
            placements: Vec::new(),
            restored: Vec::new(),
            mode_journal: None,
            setup_journal: None,
        }
    }
}
impl Compositor for FakeCompositor {
    fn mode_only_baseline(
        &mut self,
        _: &str,
        _: &Specification,
        _: &Specification,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({"state":{"preserved":true},"old_mode":"1","desired_mode":"2"}))
    }
    fn apply_mode_only(
        &mut self,
        _: &str,
        _: &Specification,
        baseline: &serde_json::Value,
    ) -> Result<(), String> {
        let path = self.mode_journal.as_ref().ok_or("missing test journal")?;
        let document: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            document["records"][0]["transition"]["preservation_baseline"],
            *baseline
        );
        assert_eq!(document["records"][0]["transition"]["completed"], false);
        if self.fail {
            Err("uncertain mode effect".into())
        } else {
            Ok(())
        }
    }
    fn snapshot(&mut self, changing: &str) -> Result<Option<super::super::layout::Layout>, String> {
        super::super::layout::Layout::capture(&serde_json::json!({"outputs":[{"name":"Virtual-1","connected":true,"enabled":true,"currentModeId":"1","modes":[{"id":"1","size":{"width":3840,"height":2160},"refreshRate":60}],"pos":{"x":self.snapshot_x,"y":0},"scale":2.5,"rotation":1,"hdr":false,"wcg":true,"priority":1}]}), changing).map(Some)
    }
    fn restore(&mut self, layout: Option<&super::super::layout::Layout>) -> Result<(), String> {
        self.restored.push(layout.is_some());
        if self.fail_restore || self.fail_restore_after == Some(self.restored.len()) {
            Err("restore failed".into())
        } else {
            Ok(())
        }
    }
    fn verify_removal_guard(&mut self, _: &str) -> Result<bool, String> {
        Ok(self.guard_available)
    }
    fn verify_parked(&mut self, _: &str) -> Result<(), String> {
        if self.parked_observable {
            Ok(())
        } else {
            Err("owned output not observable".into())
        }
    }
    fn configure(
        &mut self,
        output: &str,
        _: &Specification,
        layout: Option<&super::super::layout::Layout>,
        placement: &Placement,
        _: ScaleSetting,
    ) -> Result<(), String> {
        if let Some(path) = &self.setup_journal {
            let document: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            let record = document["records"]
                .as_array()
                .unwrap()
                .iter()
                .find(|record| record["output"] == output)
                .unwrap();
            assert!(
                record["setup_layouts"]
                    .as_array()
                    .is_some_and(|layouts| !layouts.is_empty()),
                "current desktop snapshot must be durable before configuration"
            );
        }
        self.placements.push((output.into(), placement.clone()));
        if self.fail
            || self
                .rejected_configuration_layout
                .as_ref()
                .is_some_and(|rejected| {
                    layout
                        .and_then(|value| serde_json::to_value(value).ok())
                        .as_ref()
                        == Some(rejected)
                })
        {
            Err("compositor failed".into())
        } else {
            Ok(())
        }
    }
}
fn fixture() -> (
    PathBuf,
    Controller<FakeBroker, FakeCompositor, FakeConnectors>,
) {
    let root = std::env::temp_dir().join(format!(
        "amaterasu-owned-display-{}",
        amaterasu_storage::random_identifier().unwrap()
    ));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let broker = FakeBroker {
        outputs: Arc::new(Mutex::new(BTreeMap::from([("Virtual-1".into(), true)]))),
        effects: Default::default(),
        journal: root.join("connectors.json"),
        reject_removal: Default::default(),
        reject_connection: Default::default(),
    };
    let controller = Controller {
        journal: Journal::open(&root).unwrap(),
        broker,
        compositor: FakeCompositor::new(),
        connectors: FakeConnectors::named(1..=8),
        layout: VirtualDisplayLayout::Extended,
        stopped: Default::default(),
    };
    (root, controller)
}
fn specification(identity: &str) -> Specification {
    Specification {
        hdr_enabled: false,
        identity: identity.into(),
        width: 1920,
        height: 1080,
        refresh_millihz: 60000,
        name: "Amaterasu".into(),
    }
}
const FIRST: &str = "11111111-1111-4111-8111-111111111111";
const SECOND: &str = "22222222-2222-4222-8222-222222222222";
#[test]
fn mode_change_retains_the_same_compositor_and_registry_global() {
    use super::super::{observer::Observation, same_output_identity};
    let before = Observation {
        global: 4,
        width: 1920,
        height: 1080,
        refresh: 60000,
        generation: 1,
        compositor_id: [1; 32],
    };
    let mut after = before;
    after.width = 1280;
    after.height = 720;
    after.generation = 2;
    assert!(same_output_identity(before, after));
    after.global = 5;
    assert!(!same_output_identity(before, after));
    after.global = 4;
    after.compositor_id = [2; 32];
    assert!(!same_output_identity(before, after));
}
#[test]
fn mode_only_allows_staged_owned_reference_but_legacy_requires_one() {
    let refs = std::collections::BTreeSet::from(["session".into(), "stream".into()]);
    assert!(super::super::references_admit_reconfigure(
        &refs, "stream", true
    ));
    assert!(!super::super::references_admit_reconfigure(
        &refs, "other", true
    ));
    assert!(!super::super::references_admit_reconfigure(
        &refs, "stream", false
    ));
}
#[test]
fn reconfigure_persists_old_and_new_modes_and_retains_uncertain_intent() {
    let (root, mut controller) = fixture();
    let old = specification(FIRST);
    let record = controller
        .create_with_references(
            "owner".into(),
            old.clone(),
            ScaleSetting::Retain,
            Some(std::collections::BTreeSet::from(["ref-1".into()])),
            VirtualDisplayLayout::Extended,
        )
        .unwrap();
    let mut desired = old.clone();
    desired.width = 1280;
    let transition = super::super::Transition {
        operation_id: "resize-1".into(),
        reference_id: "ref-1".into(),
        borrow_id: "a".repeat(64),
        old: old.clone(),
        desired: desired.clone(),
        completed: false,
        mode_only: false,
        preservation_baseline: None,
    };
    controller.compositor.fail = true;
    assert!(
        controller
            .reconfigure(
                &record.key(),
                transition.clone(),
                ScaleSetting::Retain,
                VirtualDisplayLayout::Extended
            )
            .is_err()
    );
    let document: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("connectors.json")).unwrap()).unwrap();
    assert_eq!(document["records"][0]["specification"]["width"], 1920);
    assert_eq!(document["records"][0]["transition"]["old"]["width"], 1920);
    assert_eq!(
        document["records"][0]["transition"]["desired"]["width"],
        1280
    );
    assert_eq!(document["records"][0]["transition"]["completed"], false);
    controller.compositor.fail = false;
    assert!(
        controller
            .reconfigure(
                &record.key(),
                transition,
                ScaleSetting::Retain,
                VirtualDisplayLayout::Extended
            )
            .is_err()
    );
    let mut pending = controller.journal.records[0].clone();
    pending.references = Some(Default::default());
    controller.journal.update(pending).unwrap();
    drop(controller);
    let reopened = Journal::open(&root).unwrap();
    assert!(reopened.records[0].references.as_ref().unwrap().is_empty());
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn mode_only_persists_baseline_before_effect_and_retains_uncertain_transition() {
    let (root, mut controller) = fixture();
    let old = specification(FIRST);
    let record = controller
        .create_with_references(
            "owner".into(),
            old.clone(),
            ScaleSetting::Retain,
            Some(std::collections::BTreeSet::from(["ref-1".into()])),
            VirtualDisplayLayout::Extended,
        )
        .unwrap();
    let mut desired = old.clone();
    desired.width = 1280;
    controller.compositor.mode_journal = Some(root.join("connectors.json"));
    controller.compositor.fail = true;
    let transition = super::super::Transition {
        operation_id: "mode-only-1".into(),
        reference_id: "ref-1".into(),
        borrow_id: "a".repeat(64),
        old,
        desired,
        completed: false,
        mode_only: true,
        preservation_baseline: None,
    };
    assert!(
        controller
            .reconfigure(
                &record.key(),
                transition,
                ScaleSetting::Retain,
                VirtualDisplayLayout::Extended
            )
            .is_err()
    );
    let saved = controller.journal.records[0].transition.as_ref().unwrap();
    assert!(saved.mode_only && !saved.completed && saved.preservation_baseline.is_some());
    drop(controller);
    assert!(Journal::open(&root).unwrap().records[0].valid());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn reconfigure_records_confirmed_mode_before_success() {
    let (root, mut controller) = fixture();
    let old = specification(FIRST);
    let record = controller
        .create_with_references(
            "owner".into(),
            old.clone(),
            ScaleSetting::Retain,
            Some(std::collections::BTreeSet::from(["ref-1".into()])),
            VirtualDisplayLayout::Extended,
        )
        .unwrap();
    let mut desired = old.clone();
    desired.width = 1280;
    let transition = super::super::Transition {
        operation_id: "resize-1".into(),
        reference_id: "ref-1".into(),
        borrow_id: "a".repeat(64),
        old,
        desired,
        completed: false,
        mode_only: false,
        preservation_baseline: None,
    };
    let mut result = controller
        .reconfigure(
            &record.key(),
            transition,
            ScaleSetting::Retain,
            VirtualDisplayLayout::Extended,
        )
        .unwrap();
    assert_eq!(result.specification.width, 1280);
    let document: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("connectors.json")).unwrap()).unwrap();
    assert_eq!(document["records"][0]["specification"]["width"], 1280);
    assert_eq!(document["records"][0]["transition"]["completed"], true);
    // Releasing the final reference is persisted before native removal. The
    // completed receipt remains valid even if the process stops in that gap.
    result.references = Some(Default::default());
    controller.journal.update(result.clone()).unwrap();
    assert!(result.valid());
    controller.remove(&result.key()).unwrap();
    assert!(controller.journal.records.is_empty());
    drop(controller);
    let reopened = Journal::open(&root).unwrap();
    assert!(reopened.records.is_empty());
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn pending_reconfiguration_keeps_old_borrow_fenced_until_worker_settles() {
    use super::super::{Borrow, Manager, Reconfiguration, Transition, observer::Observation};
    let mut manager =
        Manager::start(serde_json::from_value(serde_json::json!({"enabled":false})).unwrap())
            .unwrap();
    let key = ("owner".to_string(), FIRST.to_string());
    let old = specification(FIRST);
    let borrow_id = "a".repeat(64);
    manager.borrows.insert(
        borrow_id.clone(),
        Borrow {
            key: key.clone(),
            reference_id: "ref-1".into(),
            valid: false,
            observation: Observation {
                global: 1,
                width: 1920,
                height: 1080,
                refresh: 60000,
                generation: 1,
                compositor_id: [1; 32],
            },
        },
    );
    manager.reconfigurations.insert(
        key.clone(),
        Reconfiguration {
            transition: Transition {
                operation_id: "resize-1".into(),
                reference_id: "ref-1".into(),
                borrow_id: borrow_id.clone(),
                old: old.clone(),
                desired: old,
                completed: false,
                mode_only: false,
                preservation_baseline: None,
            },
            state: "pending",
            result: None,
            domain_id: "domain".into(),
            revision: 1,
            verification: None,
        },
    );
    let unknown =
        manager.reconfigure_status("other", "resize-1", FIRST, "ref-1", &borrow_id, "domain", 1);
    assert_eq!(unknown["code"], "recovery_required");
    let unknown =
        manager.reconfigure_status("owner", "missing", FIRST, "ref-1", &borrow_id, "domain", 1);
    assert_eq!(unknown["code"], "recovery_required");
    assert_eq!(manager.reconfigurations.len(), 1);
    assert!(
        manager.sender.is_none(),
        "lookup must not enqueue native work"
    );
    manager.domain_id = "domain".into();
    assert_eq!(
        manager.reconfigure_status("owner", "resize-1", FIRST, "ref-1", &borrow_id, "domain", 1)["state"],
        "pending"
    );
    assert_eq!(
        manager.reconfigure_mode_only_status(
            "owner", "resize-1", FIRST, "ref-1", &borrow_id, "domain", 1
        )["code"],
        "recovery_required"
    );
    assert_eq!(
        manager.release_borrow("owner", FIRST, "ref-1", &borrow_id)["code"],
        "reconfigure_pending"
    );
    assert_eq!(manager.borrows.len(), 1);
    assert_eq!(manager.domain()["pending_mutations"], 1);
    manager
        .reconfigurations
        .get_mut(&key)
        .unwrap()
        .transition
        .mode_only = true;
    manager.reconfigurations.get_mut(&key).unwrap().verification =
        Some(super::super::Verification::Pending);
    assert!(manager.poll_mode_only_verification(&key).unwrap().is_none());
    manager.reconfigurations.get_mut(&key).unwrap().verification =
        Some(super::super::Verification::Finished {
            result: Ok(serde_json::json!({"readback":{"scale":2}})),
            observed_at: std::time::Instant::now(),
        });
    assert_eq!(
        manager.poll_mode_only_verification(&key).unwrap().unwrap()["readback"]["scale"],
        2
    );
    manager.reconfigurations.get_mut(&key).unwrap().verification =
        Some(super::super::Verification::Finished {
            result: Ok(serde_json::json!({"readback":{"scale":3}})),
            observed_at: std::time::Instant::now() - std::time::Duration::from_secs(2),
        });
    assert!(
        manager.poll_mode_only_verification(&key).is_err(),
        "expired proof must not be reused"
    );
    let mut cached = serde_json::json!({"ok":true,"preservation_readback":{"scale":1}});
    let fresh = manager.apply_fresh_mode_only_readback(
        &key,
        &mut cached,
        Ok(serde_json::json!({"readback":{"scale":2},"before_digest":"a","after_digest":"a"})),
    );
    assert_eq!(fresh["preservation_readback"]["scale"], 2);
    let failed =
        manager.apply_fresh_mode_only_readback(&key, &mut cached, Err("desktop changed".into()));
    assert_eq!(failed["code"], "recovery_required");
    assert_eq!(manager.reconfigurations[&key].state, "recovery_required");
    assert!(!manager.borrows[&borrow_id].valid);
    assert_eq!(manager.domain()["pending_mutations"], 1);
    assert_eq!(
        manager.release_borrow("owner", FIRST, "ref-1", &borrow_id)["released"],
        true
    );
    assert!(manager.borrows.is_empty());
}
#[test]
fn clients_get_distinct_free_connectors_and_cleanup_preserves_vibeshines_display() {
    let (root, mut controller) = fixture();
    let first = controller
        .create(
            "stream-session".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
        )
        .unwrap();
    let second = controller
        .create(
            "stream-session".into(),
            specification(SECOND),
            ScaleSetting::Automatic,
        )
        .unwrap();
    assert_eq!(first.output, "Virtual-2");
    assert_eq!(second.output, "Virtual-3");
    controller.remove(&first.key()).unwrap();
    assert!(controller.broker.connected("Virtual-1").unwrap());
    assert!(!controller.broker.connected("Virtual-2").unwrap());
    assert!(controller.broker.connected("Virtual-3").unwrap());
    assert!(
        !controller
            .broker
            .effects
            .lock()
            .unwrap()
            .iter()
            .any(|(output, _)| output == "Virtual-1")
    );
    controller.remove(&second.key()).unwrap();
    assert!(controller.journal.records.is_empty());
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn interrupted_cleanup_retains_intent_and_restart_only_recovers_owned_connectors() {
    let (root, mut controller) = fixture();
    controller.compositor.fail = true;
    *controller.broker.reject_removal.lock().unwrap() = true;
    assert!(
        controller
            .create(
                "stream-session".into(),
                specification(FIRST),
                ScaleSetting::Automatic
            )
            .is_err()
    );
    assert_eq!(controller.journal.records.len(), 1);
    let broker = controller.broker.clone();
    drop(controller);
    *broker.reject_removal.lock().unwrap() = false;
    let mut restarted = Controller {
        journal: Journal::open(&root).unwrap(),
        broker,
        compositor: FakeCompositor::new(),
        connectors: FakeConnectors::named(1..=8),
        layout: VirtualDisplayLayout::Extended,
        stopped: Default::default(),
    };
    assert!(restarted.recover().is_none());
    assert!(restarted.journal.records.is_empty());
    assert!(restarted.broker.connected("Virtual-1").unwrap());
    assert!(!restarted.broker.connected("Virtual-2").unwrap());
    drop(restarted);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn journal_excludes_other_hosts_and_rejects_corrupt_ownership() {
    let (root, controller) = fixture();
    assert!(Journal::open(&root).is_err());
    drop(controller);
    amaterasu_storage::write_atomically(&root.join("connectors.json"),br#"{"schema":1,"records":[{"owner":"a","specification":{"identity":"11111111-1111-4111-8111-111111111111","width":1920,"height":1080,"refresh_millihz":60000,"name":"test"},"output":"HDMI-A-1"}]}"#).unwrap();
    assert!(Journal::open(&root).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ownership_journal_never_persists_more_records_than_restart_can_read() {
    let (root, mut controller) = fixture();
    let first = controller
        .create_with_references(
            "owner".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    for index in 1..8 {
        let mut record = first.clone();
        record.specification.identity = format!("00000000-0000-4000-8000-{index:012x}");
        record.output = format!("Virtual-{}", index + 2);
        controller.journal.insert(record).unwrap();
    }
    let before = fs::read(root.join("connectors.json")).unwrap();
    let mut overflow = first;
    overflow.specification.identity = "00000000-0000-4000-8000-000000000009".into();
    overflow.output = "Virtual-10".into();
    assert!(controller.journal.insert(overflow).is_err());
    assert_eq!(fs::read(root.join("connectors.json")).unwrap(), before);
    drop(controller);
    let reopened = Journal::open(&root).unwrap();
    assert_eq!(reopened.records.len(), 8);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn shutdown_never_admits_a_new_connector_effect() {
    let (root, mut controller) = fixture();
    controller
        .stopped
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(
        controller
            .create(
                "stream-session".into(),
                specification(FIRST),
                ScaleSetting::Automatic
            )
            .is_err()
    );
    assert!(controller.journal.records.is_empty());
    assert!(controller.broker.effects.lock().unwrap().is_empty());
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reloaded_pool_allocates_the_connectors_that_exist_past_virtual_8() {
    let (root, mut controller) = fixture();
    controller.connectors = FakeConnectors::named(9..=16);
    controller
        .broker
        .outputs
        .lock()
        .unwrap()
        .insert("Virtual-9".into(), true);
    let first = controller
        .create(
            "stream-session".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
        )
        .unwrap();
    assert_eq!(first.output, "Virtual-10");
    assert!(controller.broker.connected("Virtual-10").unwrap());
    assert!(
        !controller
            .broker
            .effects
            .lock()
            .unwrap()
            .iter()
            .any(|(output, _)| output == "Virtual-1" || output == "Virtual-9")
    );
    let broker = controller.broker.clone();
    let connectors = controller.connectors.clone();
    drop(controller);

    // The journaled Virtual-10 record remains valid ownership after restart.
    let mut restarted = Controller {
        journal: Journal::open(&root).unwrap(),
        broker,
        compositor: FakeCompositor::new(),
        connectors,
        layout: VirtualDisplayLayout::Extended,
        stopped: Default::default(),
    };
    assert_eq!(restarted.journal.records[0].output, "Virtual-10");
    assert!(restarted.recover().is_none());
    assert!(!restarted.broker.connected("Virtual-10").unwrap());
    assert!(restarted.broker.connected("Virtual-9").unwrap());
    drop(restarted);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn connector_discovery_failure_or_exhaustion_admits_no_effect() {
    let (root, mut controller) = fixture();
    *controller.connectors.0.lock().unwrap() = Err("DRM sysfs is unavailable".into());
    assert_eq!(
        controller
            .create(
                "stream-session".into(),
                specification(FIRST),
                ScaleSetting::Automatic
            )
            .err()
            .as_deref(),
        Some("DRM sysfs is unavailable")
    );
    controller.connectors = FakeConnectors::named(9..=9);
    controller
        .broker
        .outputs
        .lock()
        .unwrap()
        .insert("Virtual-9".into(), true);
    assert_eq!(
        controller
            .create(
                "stream-session".into(),
                specification(FIRST),
                ScaleSetting::Automatic
            )
            .err()
            .as_deref(),
        Some("no free virtual display connector")
    );
    assert!(controller.journal.records.is_empty());
    assert!(controller.broker.effects.lock().unwrap().is_empty());
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exclusive_streams_keep_peer_outputs_and_restore_the_pre_stream_desktop() {
    let (root, mut controller) = fixture();
    controller.layout = VirtualDisplayLayout::Exclusive;
    let first = controller
        .create(
            "stream-session".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
        )
        .unwrap();
    let second = controller
        .create(
            "stream-session".into(),
            specification(SECOND),
            ScaleSetting::Automatic,
        )
        .unwrap();
    assert_eq!(
        controller.compositor.placements,
        [
            ("Virtual-2".into(), Placement::Exclusive { keep: vec![] }),
            (
                "Virtual-3".into(),
                Placement::Exclusive {
                    keep: vec!["Virtual-2".into()]
                }
            ),
        ]
    );
    // The snapshot taken before the effect stays journaled, so removal
    // re-enables the displays this stream turned off.
    assert!(
        controller
            .journal
            .records
            .iter()
            .all(|record| record.layout.is_some())
    );
    controller.remove(&first.key()).unwrap();
    controller.remove(&second.key()).unwrap();
    assert_eq!(controller.compositor.restored, [true, true, true, true]);
    assert!(controller.journal.records.is_empty());
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn per_request_layout_is_used_without_changing_the_controller_default() {
    let (root, mut controller) = fixture();
    let first = controller
        .create_with_references(
            "session".into(),
            specification(FIRST),
            ScaleSetting::Retain,
            None,
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    let second = controller
        .create(
            "session".into(),
            specification(SECOND),
            ScaleSetting::Retain,
        )
        .unwrap();
    assert_eq!(
        controller.compositor.placements,
        [
            ("Virtual-2".into(), Placement::Exclusive { keep: vec![] }),
            ("Virtual-3".into(), Placement::Extended),
        ]
    );
    controller.remove(&second.key()).unwrap();
    controller.remove(&first.key()).unwrap();
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn extended_streams_restore_the_layout_current_at_removal() {
    let (root, mut controller) = fixture();
    let record = controller
        .create(
            "stream-session".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
        )
        .unwrap();
    assert_eq!(
        controller.compositor.placements,
        [("Virtual-2".into(), Placement::Extended)]
    );
    assert!(controller.journal.records[0].layout.is_none());
    controller.remove(&record.key()).unwrap();
    // Removal snapshots afresh, so the restore still carries a layout.
    assert_eq!(controller.compositor.restored, [true, true]);
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn removal_preserves_connector_and_journal_until_a_survivor_is_available() {
    let (root, mut controller) = fixture();
    let record = controller
        .create_with_references(
            "stream-session".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Extended,
        )
        .unwrap();
    controller.compositor.guard_available = false;
    assert!(controller.remove(&record.key()).unwrap());
    assert!(controller.broker.connected(&record.output).unwrap());
    assert_eq!(controller.journal.records.len(), 1);
    assert!(controller.journal.records[0].parked);
    assert!(
        !controller
            .broker
            .effects
            .lock()
            .unwrap()
            .iter()
            .any(|(name, connected)| name == &record.output && !connected)
    );

    let reused = controller
        .create_with_references(
            "new-session".into(),
            specification(SECOND),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Extended,
        )
        .unwrap();
    assert_eq!(reused.output, record.output);
    assert!(!controller.journal.records[0].parked);
    controller.compositor.guard_available = true;
    controller.remove(&reused.key()).unwrap();
    assert!(!controller.broker.connected(&reused.output).unwrap());
    assert!(controller.journal.records.is_empty());
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_restore_parks_the_released_display_for_a_new_stream() {
    let (root, mut controller) = fixture();
    let old = controller
        .create_with_references(
            "owner".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    controller.compositor.fail_restore = true;
    assert!(controller.remove(&old.key()).unwrap());
    assert_eq!(
        controller.journal.records[0].park_reason.as_deref(),
        Some("restore_failed")
    );
    assert!(controller.broker.connected(&old.output).unwrap());
    controller.compositor.fail_restore = false;
    let next = controller
        .create_with_references(
            "owner".into(),
            specification(SECOND),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    assert_eq!(next.output, old.output);
    assert!(!controller.remove(&next.key()).unwrap());
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn extended_reuse_retains_the_failed_restoration_baseline() {
    let (root, mut controller) = fixture();
    let old = controller
        .create_with_references(
            "owner".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    controller.compositor.fail_restore = true;
    assert!(controller.remove(&old.key()).unwrap());
    let baseline = serde_json::to_value(&controller.journal.records[0].layout).unwrap();
    // The old desktop still cannot be restored. Preparing the new stream
    // must not reapply that failed baseline as part of configuration.
    controller.compositor.rejected_configuration_layout = baseline.clone().into();
    controller.compositor.snapshot_x = 200;
    controller.compositor.setup_journal = Some(root.join("connectors.json"));
    let next = controller
        .create_with_references(
            "owner".into(),
            specification(SECOND),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Extended,
        )
        .unwrap();
    assert_eq!(serde_json::to_value(&next.layout).unwrap(), baseline);
    assert!(next.setup_layouts.is_empty());
    controller.compositor.fail_restore = false;
    controller.remove(&next.key()).unwrap();
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_reuse_preserves_each_unrestored_setup_across_the_next_stream() {
    let (root, mut controller) = fixture();
    let old = controller
        .create_with_references(
            "owner".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    controller.compositor.fail_restore = true;
    assert!(controller.remove(&old.key()).unwrap());
    let baseline = serde_json::to_value(&controller.journal.records[0].layout).unwrap();
    controller.compositor.snapshot_x = 200;
    controller.compositor.fail = true;
    assert!(
        controller
            .create_with_references(
                "owner".into(),
                specification(SECOND),
                ScaleSetting::Automatic,
                Some(Default::default()),
                VirtualDisplayLayout::Extended,
            )
            .is_err()
    );
    assert!(controller.journal.records[0].parked);
    assert_eq!(controller.journal.records[0].setup_layouts.len(), 1);
    let pending = serde_json::to_value(&controller.journal.records[0].setup_layouts[0]).unwrap();
    controller.compositor.fail = false;
    controller.compositor.snapshot_x = 400;
    let next = controller
        .create_with_references(
            "owner".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Extended,
        )
        .unwrap();
    assert_eq!(serde_json::to_value(&next.layout).unwrap(), baseline);
    assert_eq!(next.setup_layouts.len(), 1);
    assert_eq!(
        serde_json::to_value(&next.setup_layouts[0]).unwrap(),
        pending
    );
    controller.compositor.fail_restore = false;
    controller.remove(&next.key()).unwrap();
    assert!(controller.journal.records.is_empty());
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_restore_without_output_proof_keeps_cleanup_unresolved() {
    let (root, mut controller) = fixture();
    let record = controller
        .create_with_references(
            "owner".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    controller.compositor.fail_restore = true;
    controller.compositor.parked_observable = false;
    assert!(controller.remove(&record.key()).is_err());
    assert!(!controller.journal.records[0].parked);
    assert!(controller.broker.connected(&record.output).unwrap());
    controller.compositor.fail_restore = false;
    controller.compositor.parked_observable = true;
    controller.remove(&record.key()).unwrap();
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_post_disconnect_restore_keeps_recoverable_reservation() {
    let (root, mut controller) = fixture();
    let record = controller
        .create_with_references(
            "owner".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    controller.compositor.fail_restore_after = Some(2);
    assert!(controller.remove(&record.key()).unwrap());
    assert!(!controller.broker.connected(&record.output).unwrap());
    assert!(controller.journal.records[0].parked);
    let next = controller
        .create_with_references(
            "owner".into(),
            specification(SECOND),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    assert_eq!(next.output, record.output);
    assert!(controller.broker.connected(&next.output).unwrap());
    controller.remove(&next.key()).unwrap();
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn parked_connector_is_recovered_and_reused_after_restart() {
    let (root, mut controller) = fixture();
    let old = controller
        .create_with_references(
            "owner".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    controller.compositor.guard_available = false;
    assert!(controller.remove(&old.key()).unwrap());
    let broker = controller.broker.clone();
    drop(controller);
    let mut compositor = FakeCompositor::new();
    compositor.guard_available = false;
    let mut restarted = Controller {
        journal: Journal::open(&root).unwrap(),
        broker,
        compositor,
        connectors: FakeConnectors::named(1..=8),
        layout: VirtualDisplayLayout::Exclusive,
        stopped: Default::default(),
    };
    assert!(restarted.recover().is_none());
    assert!(restarted.journal.records[0].parked);
    let next = restarted
        .create_with_references(
            "owner".into(),
            specification(SECOND),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    assert_eq!(next.output, old.output);
    assert_eq!(restarted.journal.records.len(), 1);
    restarted.compositor.guard_available = true;
    restarted.remove(&next.key()).unwrap();
    drop(restarted);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn vanished_parked_connector_reuses_its_recovery_intent_on_a_new_pool() {
    let (root, mut controller) = fixture();
    let old = controller
        .create_with_references(
            "owner".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    controller.compositor.guard_available = false;
    assert!(controller.remove(&old.key()).unwrap());
    let baseline = serde_json::to_value(&controller.journal.records[0].layout).unwrap();
    controller
        .broker
        .outputs
        .lock()
        .unwrap()
        .insert(old.output.clone(), false);
    controller.connectors = FakeConnectors::named(3..=10);
    let next = controller
        .create_with_references(
            "owner".into(),
            specification(SECOND),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Extended,
        )
        .unwrap();
    assert_eq!(next.output, "Virtual-3");
    assert_eq!(serde_json::to_value(&next.layout).unwrap(), baseline);
    assert_eq!(controller.journal.records.len(), 1);
    controller.compositor.guard_available = true;
    controller.remove(&next.key()).unwrap();
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn parked_connector_is_not_adopted_without_broker_ownership() {
    let (root, mut controller) = fixture();
    let old = controller
        .create_with_references(
            "owner".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
            Some(Default::default()),
            VirtualDisplayLayout::Exclusive,
        )
        .unwrap();
    controller.compositor.guard_available = false;
    assert!(controller.remove(&old.key()).unwrap());
    let before = controller.compositor.placements.len();
    *controller.broker.reject_connection.lock().unwrap() = true;
    assert!(
        controller
            .create_with_references(
                "owner".into(),
                specification(SECOND),
                ScaleSetting::Automatic,
                Some(std::collections::BTreeSet::from(["next-ref".into()])),
                VirtualDisplayLayout::Exclusive,
            )
            .is_err()
    );
    assert_eq!(controller.compositor.placements.len(), before);
    assert!(
        controller
            .journal
            .records
            .iter()
            .any(|record| record.specification.identity == SECOND)
    );
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn parked_status_is_explicit_and_does_not_claim_connector_removal() {
    use super::super::{Manager, Monitor};
    let (sender, receiver) = std::sync::mpsc::sync_channel(2);
    let key = ("owner".to_owned(), FIRST.to_owned());
    let mut manager = Manager {
        settings: serde_json::from_value(serde_json::json!({"enabled":true})).unwrap(),
        monitors: BTreeMap::from([(
            key.clone(),
            Monitor {
                specification: specification(FIRST),
                state: "removing",
                output: Some("Virtual-2".into()),
                error: None,
                references: Some(Default::default()),
                layout: VirtualDisplayLayout::Extended,
                scale: ScaleSetting::Automatic,
            },
        )]),
        parked: Default::default(),
        ready: true,
        recovery_error: None,
        sender: None,
        receiver: Some(receiver),
        worker: None,
        cleanup_pending: std::collections::BTreeSet::from([key.clone()]),
        observer: None,
        borrows: Default::default(),
        reconfigurations: Default::default(),
        domain_id: "provider_session:test".into(),
        topology_revision: 1,
        pending_mutations: 1,
        stopped: Default::default(),
    };
    sender
        .send(Event::Removed {
            key,
            result: Ok(true),
        })
        .unwrap();
    manager.tick();
    let status = manager.status("owner", Some(FIRST.into()), Some("old-ref".into()));
    assert_eq!(status["parked"], true);
    assert_eq!(status["reference_owned"], false);
    assert_eq!(status["monitors"], serde_json::json!([]));
    assert_eq!(status["parked_outputs"][0]["identity"], FIRST);
    let removal = manager.remove("owner", FIRST, Some("old-ref"));
    assert_eq!(removal["parked"], true);
    assert_eq!(removal["removed"], false);
    // A new reference to the same identity supersedes the stale parked
    // observation while the create worker is still in flight.
    manager.monitors.insert(
        ("owner".into(), FIRST.into()),
        Monitor {
            specification: specification(FIRST),
            state: "creating",
            output: None,
            error: None,
            references: Some(std::collections::BTreeSet::from(["new-ref".into()])),
            layout: VirtualDisplayLayout::Extended,
            scale: ScaleSetting::Automatic,
        },
    );
    let active = manager.status("owner", Some(FIRST.into()), Some("new-ref".into()));
    assert_eq!(active["parked"], false);
    assert_eq!(active["reference_owned"], true);
    assert_ne!(
        manager.remove("owner", FIRST, Some("new-ref"))["parked"],
        true
    );
}

#[test]
fn manager_releases_parks_and_reuses_output_through_public_operations() {
    use super::super::Manager;
    let (root, mut controller) = fixture();
    controller.compositor.guard_available = false;
    let broker = controller.broker.clone();
    let (sender, jobs) = std::sync::mpsc::sync_channel(16);
    let (events, receiver) = std::sync::mpsc::sync_channel(16);
    let stopped = controller.stopped.clone();
    let worker_stopped = stopped.clone();
    let worker = std::thread::spawn(move || {
        run(
            controller.journal,
            controller.broker,
            controller.compositor,
            controller.connectors,
            controller.layout,
            jobs,
            events,
            worker_stopped,
        )
    });
    let mut manager = Manager {
        settings: serde_json::from_value(serde_json::json!({"enabled":true})).unwrap(),
        monitors: Default::default(),
        parked: Default::default(),
        ready: false,
        recovery_error: None,
        sender: Some(sender),
        receiver: Some(receiver),
        worker: Some(worker),
        cleanup_pending: Default::default(),
        observer: None,
        borrows: Default::default(),
        reconfigurations: Default::default(),
        domain_id: "provider_session:test".into(),
        topology_revision: 0,
        pending_mutations: 0,
        stopped,
    };
    let wait = |manager: &mut Manager, ready: &dyn Fn(&Manager) -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            manager.tick();
            if ready(manager) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("display worker did not settle");
    };
    wait(&mut manager, &|manager| manager.ready);
    let create = |identity: &str, reference: &str| crate::model::Operation::Create {
        hdr_enabled: false,
        identity: identity.into(),
        width: 1920,
        height: 1080,
        refresh_millihz: 60000,
        name: "Amaterasu".into(),
        reference_id: Some(reference.into()),
        dd_virtual_display_scale: None,
        virtual_display_layout: None,
        expected_topology_revision: None,
        expected_topology_domain_id: None,
    };
    assert_eq!(
        manager.admit("session", create(FIRST, "first-ref")).0["ok"],
        true
    );
    wait(&mut manager, &|manager| manager.pending_mutations == 0);
    let output = manager.monitors[&("session".into(), FIRST.into())]
        .output
        .clone()
        .unwrap();
    assert_eq!(
        manager.remove("session", FIRST, Some("first-ref"))["monitor"]["state"],
        "removing"
    );
    wait(&mut manager, &|manager| manager.pending_mutations == 0);
    let parked = manager.status("session", Some(FIRST.into()), Some("first-ref".into()));
    assert_eq!(parked["parked"], true);
    assert_eq!(parked["reference_owned"], false);
    assert!(broker.connected(&output).unwrap());
    assert_eq!(
        manager.admit("session", create(SECOND, "second-ref")).0["ok"],
        true
    );
    wait(&mut manager, &|manager| manager.pending_mutations == 0);
    let next = manager.status("session", Some(SECOND.into()), Some("second-ref".into()));
    assert_eq!(next["monitors"][0]["output"], output);
    assert_eq!(next["reference_owned"], true);
    assert_eq!(
        manager.status("session", None, None)["parked_outputs"],
        serde_json::json!([])
    );
    drop(manager);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn linux_shared_references_persist_and_release_only_the_last_connector_owner() {
    use super::super::{Manager, Monitor};
    let (root, controller) = fixture();
    let broker = controller.broker.clone();
    let (sender, jobs) = std::sync::mpsc::sync_channel(16);
    let (events, receiver) = std::sync::mpsc::sync_channel(16);
    let stopped = controller.stopped.clone();
    let worker_stopped = stopped.clone();
    let worker = std::thread::spawn(move || {
        run(
            controller.journal,
            controller.broker,
            controller.compositor,
            controller.connectors,
            controller.layout,
            jobs,
            events,
            worker_stopped,
        );
    });
    let mut manager = Manager {
        settings: serde_json::from_value(serde_json::json!({"enabled":true})).unwrap(),
        monitors: Default::default(),
        parked: Default::default(),
        ready: true,
        recovery_error: None,
        sender: Some(sender),
        receiver: Some(receiver),
        worker: Some(worker),
        cleanup_pending: Default::default(),
        observer: None,
        borrows: Default::default(),
        reconfigurations: Default::default(),
        domain_id: "test-domain".into(),
        topology_revision: 0,
        pending_mutations: 0,
        stopped,
    };
    let create = |reference: Option<&str>| crate::model::Operation::Create {
        hdr_enabled: false,
        identity: FIRST.into(),
        width: 1920,
        height: 1080,
        refresh_millihz: 60000,
        name: "Amaterasu".into(),
        reference_id: reference.map(str::to_owned),
        dd_virtual_display_scale: None,
        virtual_display_layout: None,
        expected_topology_revision: None,
        expected_topology_domain_id: None,
    };
    assert_eq!(
        manager.admit("session", create(Some("stream-a"))).0["ok"],
        true
    );
    let (response, second_admission) = manager.admit("session", create(Some("stream-b")));
    assert_eq!(response["reference_owned"], true);
    assert!(second_admission.is_some());
    let document: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("connectors.json")).unwrap()).unwrap();
    assert_eq!(
        document["records"][0]["references"],
        serde_json::json!(["stream-a", "stream-b"])
    );
    let duplicate = manager.admit("session", create(Some("stream-b")));
    assert!(
        duplicate.1.is_none(),
        "duplicate create cannot release an existing reference on completion loss"
    );
    assert_eq!(
        manager.admit("session", create(Some("bad\nreference"))).0["code"],
        "invalid_request"
    );
    // A failed atomic replacement must not grant a reference only in memory.
    let journal_path = root.join("connectors.json");
    let backup_path = root.join("connectors-backup.json");
    fs::rename(&journal_path, &backup_path).unwrap();
    fs::create_dir(&journal_path).unwrap();
    assert_eq!(
        manager.admit("session", create(Some("stream-c"))).0["code"],
        "journal_unavailable"
    );
    assert_eq!(
        manager.status("session", Some(FIRST.into()), Some("stream-c".into()))["reference_owned"],
        false
    );
    assert_eq!(
        manager.remove("session", FIRST, Some("stream-a"))["code"],
        "journal_unavailable"
    );
    assert_eq!(
        manager.status("session", Some(FIRST.into()), Some("stream-a".into()))["reference_owned"],
        true
    );
    fs::remove_dir(&journal_path).unwrap();
    fs::rename(&backup_path, &journal_path).unwrap();
    assert_eq!(
        manager.admit("session", create(None)).0["code"],
        "reference_conflict"
    );
    assert_eq!(
        manager.status("other", Some(FIRST.into()), Some("stream-a".into()))["reference_owned"],
        false
    );
    assert_eq!(
        manager.status("session", None, Some("stream-a".into()))["ok"],
        false
    );
    let domain = manager.domain();
    assert_eq!(domain["scope"], "provider_session");
    let revision = manager.topology_revision;
    let request = |layout: &str, expected: u64| {
        serde_json::from_value(serde_json::json!({
            "action":"create", "identity":FIRST, "width":1920, "height":1080,
            "refresh_millihz":60000, "name":"Amaterasu", "reference_id":"stream-a",
            "virtual_display_layout":layout, "expected_topology_domain_id":"test-domain", "expected_topology_revision":expected
        }))
        .unwrap()
    };
    assert_eq!(
        manager
            .admit("session", request("exclusive", revision + 1))
            .0["code"],
        "topology_changed"
    );
    let mut partial = serde_json::json!({"action":"create", "identity":FIRST, "width":1920, "height":1080, "refresh_millihz":60000, "name":"Amaterasu", "expected_topology_revision":revision});
    assert_eq!(
        manager
            .admit("session", serde_json::from_value(partial.clone()).unwrap())
            .0["code"],
        "invalid_request"
    );
    partial["expected_topology_domain_id"] = "provider_session:old-epoch".into();
    assert_eq!(
        manager
            .admit("session", serde_json::from_value(partial).unwrap())
            .0["code"],
        "topology_changed"
    );
    assert_eq!(
        manager.admit("session", request("extended", revision)).0["code"],
        "identity_conflict"
    );
    manager.pending_mutations = 1;
    let pending_borrow = crate::model::Operation::Borrow {
        identity: FIRST.into(),
        reference_id: "stream-a".into(),
    };
    assert_eq!(
        manager.admit("session", pending_borrow).0["code"],
        "topology_mutating"
    );
    manager.pending_mutations = 0;
    // A borrow pins its owner reference and cannot be released by another caller.
    manager.borrows.insert(
        "borrow-token".into(),
        super::super::Borrow {
            key: ("session".into(), FIRST.into()),
            reference_id: "stream-a".into(),
            observation: super::super::observer::Observation {
                global: 4,
                width: 1920,
                height: 1080,
                refresh: 60000,
                generation: 1,
                compositor_id: [1; 32],
            },
            valid: true,
        },
    );
    assert_eq!(
        manager.remove("session", FIRST, Some("stream-a"))["code"],
        "display_borrowed"
    );
    let other_create: crate::model::Operation = serde_json::from_value(serde_json::json!({
        "action":"create", "identity":SECOND, "width":1920, "height":1080,
        "refresh_millihz":60000, "name":"Amaterasu", "reference_id":"stream-other",
        "virtual_display_layout":"extended", "expected_topology_domain_id":"test-domain", "expected_topology_revision":manager.topology_revision
    }))
    .unwrap();
    assert_eq!(
        manager.admit("session", other_create).0["code"],
        "topology_borrowed"
    );
    manager.monitors.insert(
        ("other-owner".into(), SECOND.into()),
        Monitor {
            specification: specification(SECOND),
            state: "ready",
            output: Some("Virtual-3".into()),
            error: None,
            references: Some(std::collections::BTreeSet::from(["other-ref".into()])),
            layout: VirtualDisplayLayout::Extended,
            scale: ScaleSetting::Retain,
        },
    );
    assert_eq!(
        manager.remove("other-owner", SECOND, Some("other-ref"))["code"],
        "topology_borrowed"
    );
    manager
        .monitors
        .remove(&("other-owner".into(), SECOND.into()));

    // Failed revalidation does not drop the reference fence, and cannot
    // become valid again if an equivalent output later appears.
    assert_eq!(
        manager.validate_borrow("session", FIRST, "stream-a", "borrow-token")["code"],
        "borrow_invalid"
    );
    assert_eq!(
        manager.remove("session", FIRST, Some("stream-a"))["code"],
        "display_borrowed"
    );
    assert!(!manager.borrows["borrow-token"].valid);
    manager.invalidate_observer();
    assert_eq!(
        manager.remove("session", FIRST, Some("stream-a"))["code"],
        "display_borrowed"
    );
    assert_eq!(
        manager.release_borrow("other", FIRST, "stream-a", "borrow-token")["code"],
        "borrow_invalid"
    );
    assert_eq!(
        manager.release_borrow("session", FIRST, "stream-a", "borrow-token")["released"],
        true
    );
    let response = manager.remove("session", FIRST, Some("stream-a"));
    assert_eq!(response["reference_released"], true);
    assert!(broker.connected("Virtual-2").unwrap());
    assert_eq!(
        manager.status("session", Some(FIRST.into()), Some("stream-b".into()))["reference_owned"],
        true
    );
    // Completion loss releases just the newly admitted reference.
    manager.completion_lost(second_admission);
    // A second actor command is a barrier after the queued native removal.
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    assert!(manager.enqueue(Work::References {
        key: ("absent".into(), FIRST.into()),
        references: Default::default(),
        reply
    }));
    assert!(result.recv().unwrap().is_err());
    manager.tick();
    assert!(!broker.connected("Virtual-2").unwrap());
    assert!(broker.connected("Virtual-1").unwrap());
    assert!(manager.monitors.is_empty());
    // A legacy record remains valid without reference metadata.
    let legacy = Monitor {
        specification: specification(FIRST),
        state: "ready",
        output: Some("Virtual-2".into()),
        error: None,
        references: None,
        layout: VirtualDisplayLayout::Exclusive,
        scale: ScaleSetting::Retain,
    };
    manager
        .monitors
        .insert(("session".into(), FIRST.into()), legacy);
    assert_eq!(
        manager.admit("session", create(Some("stream-c"))).0["code"],
        "reference_conflict"
    );
    assert_eq!(manager.admit("session", create(None)).0["ok"], true);
    drop(manager);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reference_ownership_survives_journal_reload_and_restart_cleanup() {
    let (root, mut controller) = fixture();
    let record = controller
        .create_with_references(
            "session".into(),
            specification(FIRST),
            ScaleSetting::Automatic,
            Some(std::collections::BTreeSet::from([
                "stream-a".into(),
                "stream-b".into(),
            ])),
            VirtualDisplayLayout::Extended,
        )
        .unwrap();
    let broker = controller.broker.clone();
    drop(controller);
    let journal = Journal::open(&root).unwrap();
    assert_eq!(journal.records[0].references, record.references);
    let mut restarted = Controller {
        journal,
        broker,
        compositor: FakeCompositor::new(),
        connectors: FakeConnectors::named(1..=8),
        layout: VirtualDisplayLayout::Extended,
        stopped: Default::default(),
    };
    // Linux restart reconciles stale connectors; references never revive a stream.
    assert!(restarted.recover().is_none());
    assert!(!restarted.broker.connected(&record.output).unwrap());
    drop(restarted);
    fs::remove_dir_all(root).unwrap();
}
