use super::*;
use crate::backend::{Backend, CaptureSession};
use crate::model::{CaptureResult, Frame, Output};
use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status};
use std::collections::{BTreeSet, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Broker {
    requests: VecDeque<Response>,
    active: BTreeSet<u64>,
    failing_checks: BTreeSet<u64>,
    completed: Vec<(u64, Value)>,
    logs: Vec<String>,
}

impl Broker {
    fn enqueue(&mut self, id: u64, payload: Value) {
        self.active.insert(id);
        self.requests.push_back(Response::Request {
            id,
            caller: "video".into(),
            initiator: "video".into(),
            service: SERVICE.into(),
            payload,
            remaining_ms: 30_000,
        });
    }
}

unsafe extern "C" fn broker_runtime(
    context: *mut c_void,
    input: BorrowedBytes,
    sink: ReplySink,
) -> Status {
    // SAFETY: Harness retains this allocation until after module destruction.
    let broker = unsafe { &*context.cast::<Mutex<Broker>>() };
    // SAFETY: the host provides valid input for the duration of this call.
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let mut broker = broker.lock().unwrap();
    let response = match command {
        Command::Poll {} => broker.requests.pop_front().unwrap_or(Response::Pending),
        Command::CheckRequest { request } if broker.failing_checks.contains(&request) => {
            Response::Error {
                code: "unavailable".into(),
                message: "injected activity check failure".into(),
            }
        }
        Command::CheckRequest { request } if broker.active.contains(&request) => Response::Ok,
        Command::CheckRequest { .. } => Response::Error {
            code: "request_ended".into(),
            message: "request ended".into(),
        },
        Command::Complete { request, payload } if broker.active.remove(&request) => {
            broker.completed.push((request, payload));
            Response::Ok
        }
        Command::Complete { .. } => Response::Error {
            code: "request_ended".into(),
            message: "request ended".into(),
        },
        Command::Log { message, .. } => {
            broker.logs.push(message);
            Response::Ok
        }
        _ => panic!("unexpected broker command"),
    };
    // SAFETY: the reply sink is live during this invocation.
    unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

struct OwnedSession(Arc<AtomicUsize>);

impl CaptureSession for OwnedSession {
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        Ok(None)
    }
}

impl Drop for OwnedSession {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct HeldBackend {
    entered: mpsc::Sender<()>,
    release: Arc<Mutex<mpsc::Receiver<()>>>,
}

impl Backend for HeldBackend {
    fn supported(&self) -> bool {
        true
    }

    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        self.entered.send(()).unwrap();
        let _ = self
            .release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10));
        Ok(Vec::new())
    }

    fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        unreachable!("enumeration must not open a capture session")
    }
}

struct Harness {
    module: CaptureModule,
    broker: Box<Mutex<Broker>>,
    session_drops: Arc<AtomicUsize>,
}

impl Harness {
    fn new() -> Self {
        let session_drops = Arc::new(AtomicUsize::new(0));
        let mut module = crate::controller::shutdown_tests::module_with_session(Box::new(
            OwnedSession(session_drops.clone()),
        ));
        let mut broker = Box::new(Mutex::new(Broker::default()));
        // SAFETY: the boxed broker remains alive until the module is dropped.
        module.runtime = unsafe {
            RuntimeClient::from_endpoint(HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: std::ptr::from_mut(&mut *broker).cast(),
                invoke: broker_runtime,
            })
        };
        Self {
            module,
            broker,
            session_drops,
        }
    }

    fn start_held_enumeration(&mut self, request: u64) -> mpsc::Sender<()> {
        let (entered, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let gate = Arc::new(Mutex::new(gate));
        self.broker.lock().unwrap().active.insert(request);
        self.module
            .pending_enumerations
            .push(enumeration::PendingEnumeration::new(request, None, None));
        enumeration::poll_requests(
            &self.module.runtime,
            false,
            &mut self.module.pending_enumerations,
            || HeldBackend {
                entered: entered.clone(),
                release: gate.clone(),
            },
        )
        .unwrap();
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        release
    }
}

#[test]
fn failed_enumeration_activity_check_does_not_starve_new_close() {
    let mut harness = Harness::new();
    let release = harness.start_held_enumeration(40);
    {
        let mut broker = harness.broker.lock().unwrap();
        broker.failing_checks.insert(40);
        broker.enqueue(41, json!({"action": "close", "stream": 1}));
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let started = Instant::now();
        harness.module.tick().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1), "tick blocked");
        if harness
            .broker
            .lock()
            .unwrap()
            .completed
            .iter()
            .any(|(id, payload)| *id == 41 && payload == &json!({"ok": true}))
        {
            break;
        }
        assert!(Instant::now() < deadline, "new Close was starved");
        std::thread::yield_now();
    }
    assert_eq!(harness.session_drops.load(Ordering::SeqCst), 1);
    assert_eq!(harness.module.pending_enumerations.len(), 1);
    assert_eq!(harness.broker.lock().unwrap().logs.len(), 1);
    {
        let mut broker = harness.broker.lock().unwrap();
        broker.failing_checks.remove(&40);
        broker.active.remove(&40);
    }
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !harness.module.quiesce().unwrap() {
        assert!(Instant::now() < deadline, "enumeration did not retire");
        std::thread::yield_now();
    }
}

#[test]
fn unsupported_backend_returns_without_starting_an_enumeration_worker() {
    struct UnsupportedBackend;

    impl Backend for UnsupportedBackend {
        fn supported(&self) -> bool {
            false
        }

        fn enumerate(&self) -> CaptureResult<Vec<Output>> {
            panic!("unsupported capture must not start enumeration")
        }

        fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
            panic!("unsupported capture must not open a session")
        }
    }

    let mut harness = Harness::new();
    harness.broker.lock().unwrap().active.insert(60);
    harness
        .module
        .pending_enumerations
        .push(enumeration::PendingEnumeration::new(60, None, None));
    enumeration::poll_requests(
        &harness.module.runtime,
        false,
        &mut harness.module.pending_enumerations,
        || UnsupportedBackend,
    )
    .unwrap();
    assert!(harness.module.pending_enumerations.is_empty());
    let broker = harness.broker.lock().unwrap();
    assert_eq!(broker.completed.len(), 1);
    assert_eq!(broker.completed[0].0, 60);
    assert_eq!(
        broker.completed[0].1["error"]["code"],
        "unsupported_platform"
    );
}

#[test]
fn cancelled_enumeration_keeps_its_worker_until_quiescent() {
    let mut harness = Harness::new();
    let release = harness.start_held_enumeration(50);
    harness.broker.lock().unwrap().active.remove(&50);
    harness.module.tick().unwrap();
    assert_eq!(harness.module.pending_enumerations.len(), 1);
    let started = Instant::now();
    assert!(!harness.module.quiesce().unwrap());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "quiesce blocked"
    );
    assert_eq!(harness.module.pending_enumerations.len(), 1);
    assert!(harness.broker.lock().unwrap().completed.is_empty());
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !harness.module.quiesce().unwrap() {
        assert!(
            Instant::now() < deadline,
            "cancelled enumeration did not retire"
        );
        std::thread::yield_now();
    }
    assert!(harness.module.pending_enumerations.is_empty());
    assert!(harness.broker.lock().unwrap().completed.is_empty());
}
