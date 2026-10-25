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
}
impl Compositor for FakeCompositor {
    fn snapshot(&mut self, changing: &str) -> Result<Option<super::super::layout::Layout>, String> {
        super::super::layout::Layout::capture(&serde_json::json!({"outputs":[{"name":"Virtual-1","connected":true,"enabled":true,"currentModeId":"1","modes":[{"id":"1","size":{"width":3840,"height":2160},"refreshRate":60}],"pos":{"x":0,"y":0},"scale":2.5,"rotation":1,"hdr":false,"wcg":true,"priority":1}]}), changing).map(Some)
    }
    fn configure(
        &mut self,
        _: &str,
        _: &Specification,
        _: Option<&super::super::layout::Layout>,
    ) -> Result<(), String> {
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
        compositor: FakeCompositor { fail: false },
        connectors: FakeConnectors::named(1..=8),
        stopped: Default::default(),
    };
    (root, controller)
}
fn specification(identity: &str) -> Specification {
    Specification {
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
        .create("stream-session".into(), specification(FIRST))
        .unwrap();
    let second = controller
        .create("stream-session".into(), specification(SECOND))
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
            .create("stream-session".into(), specification(FIRST))
            .is_err()
    );
    assert_eq!(controller.journal.records.len(), 1);
    let broker = controller.broker.clone();
    drop(controller);
    *broker.reject_removal.lock().unwrap() = false;
    let mut restarted = Controller {
        journal: Journal::open(&root).unwrap(),
        broker,
        compositor: FakeCompositor { fail: false },
        connectors: FakeConnectors::named(1..=8),
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
            .create("stream-session".into(), specification(FIRST))
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
        .create("stream-session".into(), specification(FIRST))
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
        compositor: FakeCompositor { fail: false },
        connectors,
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
            .create("stream-session".into(), specification(FIRST))
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
            .create("stream-session".into(), specification(FIRST))
            .err()
            .as_deref(),
        Some("no free virtual display connector")
    );
    assert!(controller.journal.records.is_empty());
    assert!(controller.broker.effects.lock().unwrap().is_empty());
    drop(controller);
    fs::remove_dir_all(root).unwrap();
}
