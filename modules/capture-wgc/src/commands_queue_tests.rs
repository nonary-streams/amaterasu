use super::*;
use std::time::Duration;

struct HeldEnumeration {
    entered: std::sync::mpsc::Sender<()>,
    gate: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl Backend for HeldEnumeration {
    fn supported(&self) -> bool {
        true
    }

    fn enumerate(&self) -> CaptureResult<Vec<crate::model::Output>> {
        self.entered.send(()).unwrap();
        self.gate
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok(Vec::new())
    }

    fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        panic!("the enumeration fixture must not open capture")
    }
}

#[test]
fn cancelled_enumeration_remains_owned_until_the_native_worker_exits() {
    let (entered, started) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let controller = Arc::new(Mutex::new(Controller::new(
        HeldEnumeration {
            entered,
            gate: Mutex::new(gate),
        },
        true,
    )));
    let mut module = module_with_controller(Controller::new(NativeBackend::new(None), true));
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    broker.lock().unwrap().active.insert(40);
    let mut pending = vec![crate::commands::PendingCommand::new(
        40,
        "video".into(),
        Operation::Enumerate {
            offset: None,
            limit: None,
        },
    )];
    crate::commands::poll_requests(&module.runtime, &controller, false, &mut pending).unwrap();
    started.recv_timeout(Duration::from_secs(1)).unwrap();
    broker.lock().unwrap().active.remove(&40);
    let before = Instant::now();
    crate::commands::poll_requests(&module.runtime, &controller, true, &mut pending).unwrap();
    assert!(before.elapsed() < Duration::from_secs(1));
    assert_eq!(pending.len(), 1);
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !pending.is_empty() {
        assert!(Instant::now() < deadline);
        crate::commands::poll_requests(&module.runtime, &controller, true, &mut pending).unwrap();
        std::thread::yield_now();
    }
    assert!(broker.lock().unwrap().completed.is_empty());
}

#[test]
fn broker_status_does_not_block_close_behind_a_busy_controller() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(OwnedSession(drops.clone())));
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    {
        let mut broker = broker.lock().unwrap();
        broker.active.insert(40);
        broker.requests.push_back(crate::Response::Request {
            id: 40,
            caller: "video".into(),
            initiator: "video".into(),
            service: crate::SERVICE.into(),
            payload: json!({"action":"status"}),
            remaining_ms: 30_000,
        });
        broker.close(41, "video", 1);
    }
    let (release, held) = hold_controller(&module);
    let started = Instant::now();
    module.tick().unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(broker.lock().unwrap().requests.is_empty());
    assert!(broker.lock().unwrap().completed.is_empty());
    assert_eq!(module.pending_commands.len(), 1);
    assert_eq!(module.pending_closes.len(), 1);
    release.send(()).unwrap();
    held.join().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while broker.lock().unwrap().completed.len() != 2 {
        assert!(Instant::now() < deadline);
        module.tick().unwrap();
        std::thread::yield_now();
    }
    let broker = broker.lock().unwrap();
    assert!(broker.completed.iter().any(|(request, _)| *request == 40));
    assert!(
        broker
            .completed
            .iter()
            .any(|(request, reply)| *request == 41 && reply == &json!({"ok":true}))
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn status_waiters_are_bounded_and_cancelled_requests_release_capacity() {
    let mut module = module_with_controller(Controller::new(NativeBackend::new(None), true));
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    {
        let mut broker = broker.lock().unwrap();
        for id in 1..=33 {
            broker.active.insert(id);
            broker.requests.push_back(crate::Response::Request {
                id,
                caller: "video".into(),
                initiator: "video".into(),
                service: crate::SERVICE.into(),
                payload: json!({"action":"status"}),
                remaining_ms: 30_000,
            });
        }
    }
    let (release, held) = hold_controller(&module);
    for _ in 0..5 {
        module.tick().unwrap();
    }
    assert_eq!(
        module.pending_commands.len(),
        crate::commands::MAX_PENDING_COMMANDS
    );
    assert!(
        broker
            .lock()
            .unwrap()
            .completed
            .iter()
            .any(|(id, reply)| *id == 33 && reply["error"]["code"] == "resource_exhausted")
    );
    broker.lock().unwrap().active.remove(&1);
    module.tick().unwrap();
    assert_eq!(
        module.pending_commands.len(),
        crate::commands::MAX_PENDING_COMMANDS - 1
    );
    release.send(()).unwrap();
    held.join().unwrap();
    module.tick().unwrap();
    assert!(module.pending_commands.is_empty());
}

#[test]
fn status_remains_available_to_consumers_after_shutdown_announcement() {
    let mut module = module_with_controller(Controller::new(NativeBackend::new(None), true));
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    module.prepare_shutdown(1_000).unwrap();
    {
        let mut broker = broker.lock().unwrap();
        broker.active.insert(40);
        broker.requests.push_back(crate::Response::Request {
            id: 40,
            caller: "video".into(),
            initiator: "video".into(),
            service: crate::SERVICE.into(),
            payload: json!({"action":"status"}),
            remaining_ms: 1_000,
        });
    }
    module.tick().unwrap();
    let broker = broker.lock().unwrap();
    assert_eq!(broker.completed.len(), 1);
    assert_eq!(broker.completed[0].0, 40);
    assert_eq!(broker.completed[0].1["ok"], true);
}

#[test]
fn tick_after_quiescence_does_not_start_new_enumeration() {
    let mut module = module_with_controller(Controller::new(NativeBackend::new(None), true));
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    assert!(module.quiesce().unwrap());
    {
        let mut broker = broker.lock().unwrap();
        broker.active.insert(40);
        broker.requests.push_back(crate::Response::Request {
            id: 40,
            caller: "video".into(),
            initiator: "video".into(),
            service: crate::SERVICE.into(),
            payload: json!({"action":"enumerate"}),
            remaining_ms: 1_000,
        });
    }
    module.tick().unwrap();
    assert!(module.pending_commands.is_empty());
    assert!(module.quiesce().unwrap());
    let broker = broker.lock().unwrap();
    assert_eq!(broker.completed.len(), 1);
    assert_eq!(broker.completed[0].0, 40);
    assert_eq!(broker.completed[0].1["error"]["code"], "capture_disabled");
}

#[test]
fn read_waiter_failure_does_not_starve_an_owned_close() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(OwnedSession(drops.clone())));
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    {
        let mut broker = broker.lock().unwrap();
        broker.activity_failures = 1;
        broker.active.insert(40);
        broker.requests.push_back(crate::Response::Request {
            id: 40,
            caller: "video".into(),
            initiator: "video".into(),
            service: crate::SERVICE.into(),
            payload: json!({"action":"status"}),
            remaining_ms: 1_000,
        });
        broker.close(41, "video", 1);
    }
    assert!(module.tick().unwrap_err().contains("activity check failed"));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(
        broker
            .lock()
            .unwrap()
            .completed
            .iter()
            .any(|(id, reply)| *id == 41 && reply == &json!({"ok":true}))
    );
    assert_eq!(module.pending_commands.len(), 1);
    module.tick().unwrap();
    assert!(
        broker
            .lock()
            .unwrap()
            .completed
            .iter()
            .any(|(id, _)| *id == 40)
    );
}

#[test]
fn failed_existing_read_waiter_does_not_block_new_close() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(OwnedSession(drops.clone())));
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    {
        let mut broker = broker.lock().unwrap();
        broker.active.insert(40);
        broker.requests.push_back(crate::Response::Request {
            id: 40,
            caller: "video".into(),
            initiator: "video".into(),
            service: crate::SERVICE.into(),
            payload: json!({"action":"status"}),
            remaining_ms: 1_000,
        });
    }
    let (release, held) = hold_controller(&module);
    module.tick().unwrap();
    release.send(()).unwrap();
    held.join().unwrap();
    {
        let mut broker = broker.lock().unwrap();
        broker.activity_failures = 1;
        broker.close(41, "video", 1);
    }
    assert!(module.tick().unwrap_err().contains("activity check failed"));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(module.pending_commands.len(), 1);
    assert!(
        broker
            .lock()
            .unwrap()
            .completed
            .iter()
            .any(|(id, reply)| *id == 41 && reply == &json!({"ok":true}))
    );
    module.tick().unwrap();
    assert!(
        broker
            .lock()
            .unwrap()
            .completed
            .iter()
            .any(|(id, _)| *id == 40)
    );
}
