use super::*;
use crate::backend::Observation;
use std::cell::RefCell;
use std::rc::Rc;

const ID: &str = "00000000-0000-0000-0000-000000000001";
#[derive(Default)]
struct Calls {
    creates: Vec<(u64, u64)>,
    removes: Vec<(u64, u64)>,
    feeds: Vec<u64>,
    ambiguous_create: bool,
    exists: bool,
    pending: bool,
    ready: bool,
}
struct Fake(Rc<RefCell<Calls>>);
impl Driver for Fake {
    fn supported(&self) -> bool {
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
    fn inspect(&mut self, _: u64, _: u64, _: Option<Binding>) -> Result<Observation, &'static str> {
        let calls = self.0.borrow();
        Ok(Observation {
            exists: calls.exists,
            pending_departure: calls.pending,
            ready: calls.ready.then(|| Ready {
                device_path: "owned-device".into(),
                active: false,
            }),
        })
    }
    fn feed(&mut self, lease: u64) -> Result<(), &'static str> {
        self.0.borrow_mut().feeds.push(lease);
        Ok(())
    }
    fn remove(&mut self, lease: u64, display: u64) -> Result<(), &'static str> {
        self.0.borrow_mut().removes.push((lease, display));
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
        identity: ID.into(),
        width: 1920,
        height: 1080,
        refresh_millihz: 60000,
        name: "Test display".into(),
    }
}
fn tick(manager: &mut Manager<Fake>) {
    for monitor in manager.monitors.values_mut() {
        monitor.next_poll = Instant::now();
    }
    manager.tick();
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
    });

    assert_eq!(
        manager.handle("alice", create())["code"],
        "unsupported_driver"
    );
    let status = manager.handle("alice", Operation::Status { identity: None });
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

    let status = manager.handle("alice", Operation::Status { identity: None });
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
        },
    );
    assert!(calls.borrow().removes.is_empty());
    assert_eq!(
        manager.handle("bob", Operation::Status { identity: None })["monitors"],
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
fn readiness_and_feeding_track_exact_owned_lease() {
    let (mut manager, calls) = setup(true);
    manager.handle("alice", create());
    calls.borrow_mut().ready = true;
    manager.monitors.values_mut().next().unwrap().next_feed = Instant::now();
    tick(&mut manager);
    assert_eq!(manager.monitors.values().next().unwrap().state, "ready");
    assert_eq!(calls.borrow().feeds, vec![calls.borrow().creates[0].0]);
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
fn unpublished_monitor_expires_into_cleanup_and_identity_can_be_recreated() {
    let (mut manager, calls) = setup(true);
    let first = manager.handle("alice", create());
    manager.monitors.values_mut().next().unwrap().created = Instant::now() - READINESS_TIMEOUT;
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
