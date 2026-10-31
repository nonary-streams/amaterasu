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
    placements: Vec<(String, Placement)>,
    restored: Vec<bool>,
}
impl FakeCompositor {
    fn new() -> Self {
        Self {
            fail: false,
            placements: Vec::new(),
            restored: Vec::new(),
        }
    }
}
impl Compositor for FakeCompositor {
    fn snapshot(&mut self, changing: &str) -> Result<Option<super::super::layout::Layout>, String> {
        super::super::layout::Layout::capture(&serde_json::json!({"outputs":[{"name":"Virtual-1","connected":true,"enabled":true,"currentModeId":"1","modes":[{"id":"1","size":{"width":3840,"height":2160},"refreshRate":60}],"pos":{"x":0,"y":0},"scale":2.5,"rotation":1,"hdr":false,"wcg":true,"priority":1}]}), changing).map(Some)
    }
    fn restore(&mut self, layout: Option<&super::super::layout::Layout>) -> Result<(), String> {
        self.restored.push(layout.is_some());
        Ok(())
    }
    fn configure(
        &mut self,
        output: &str,
        _: &Specification,
        _: Option<&super::super::layout::Layout>,
        placement: &Placement,
        _: ScaleSetting,
    ) -> Result<(), String> {
        self.placements.push((output.into(), placement.clone()));
        if self.fail {
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
    assert_eq!(controller.compositor.restored, [true, true]);
    assert!(controller.journal.records.is_empty());
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
    assert_eq!(controller.compositor.restored, [true]);
    drop(controller);
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
        ready: true,
        recovery_error: None,
        sender: Some(sender),
        receiver: Some(receiver),
        worker: Some(worker),
        cleanup_pending: Default::default(),
        observer: None,
        borrows: Default::default(),
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
