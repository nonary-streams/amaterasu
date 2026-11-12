use crate::backend::{Backend, CaptureSession, TouchSurface};
use crate::controller::{Controller, OpenStart};
use crate::model::{CaptureError, CaptureResult, Frame, Operation, Output};
use crate::opening::{PendingLookup, PendingOpen, poll_requests};
use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status};
use amaterasu_module_sdk::{Command, Response, RuntimeClient};
use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

struct Session(Arc<AtomicUsize>);
impl CaptureSession for Session {
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        Ok(None)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct FakeBackend {
    entered: mpsc::Sender<()>,
    gate: Mutex<mpsc::Receiver<()>>,
    opens: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}
impl Backend for FakeBackend {
    fn supported(&self) -> bool {
        true
    }
    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        Ok(Vec::new())
    }
    fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.entered.send(()).unwrap();
        self.gate
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok(Box::new(Session(self.drops.clone())))
    }
}

#[derive(Default)]
struct Broker {
    active: BTreeSet<u64>,
    expire_on_complete: BTreeSet<u64>,
    uncertain_complete: BTreeSet<u64>,
    unknown_activity: BTreeSet<u64>,
    completed: Vec<(u64, Value)>,
    delivered_uncertain: Vec<(u64, Value)>,
}

unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, sink: ReplySink) -> Status {
    // SAFETY: the harness keeps the stable broker allocation alive through every call.
    let broker = unsafe { &*context.cast::<Mutex<Broker>>() };
    // SAFETY: RuntimeClient keeps the input alive throughout this synchronous call.
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let mut broker = broker.lock().unwrap();
    let response = match command {
        Command::CheckRequest { request } if broker.unknown_activity.remove(&request) => {
            Response::Error {
                code: "unavailable".into(),
                message: "unknown activity".into(),
            }
        }
        Command::CheckRequest { request } if broker.active.contains(&request) => Response::Ok,
        Command::CheckRequest { .. } => Response::Error {
            code: "request_ended".into(),
            message: "ended".into(),
        },
        Command::Complete { request, payload } => {
            if broker.expire_on_complete.remove(&request) {
                broker.active.remove(&request);
            }
            if broker.uncertain_complete.remove(&request) {
                broker.active.remove(&request);
                broker.delivered_uncertain.push((request, payload));
                Response::Error {
                    code: "unavailable".into(),
                    message: "completion outcome unknown".into(),
                }
            } else if broker.active.remove(&request) {
                broker.completed.push((request, payload));
                Response::Ok
            } else {
                Response::Error {
                    code: "request_ended".into(),
                    message: "ended".into(),
                }
            }
        }
        _ => panic!("unexpected command: {command:?}"),
    };
    // SAFETY: the reply sink is valid for the current runtime invocation.
    unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

struct Harness {
    controller: Arc<Mutex<Controller<FakeBackend>>>,
    broker: Box<Mutex<Broker>>,
    runtime: RuntimeClient,
    opens: Vec<PendingOpen>,
    lookups: Vec<PendingLookup>,
    release: Option<mpsc::Sender<()>>,
    entered: mpsc::Receiver<()>,
    count: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}
impl Harness {
    fn new() -> Self {
        let (started, entered) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let count = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let controller = Arc::new(Mutex::new(Controller::new(
            FakeBackend {
                entered: started,
                gate: Mutex::new(gate),
                opens: count.clone(),
                drops: drops.clone(),
            },
            true,
        )));
        let mut broker = Box::new(Mutex::new(Broker::default()));
        // SAFETY: broker is boxed and outlives the runtime and all invocations.
        let runtime = unsafe {
            RuntimeClient::from_endpoint(HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: std::ptr::from_mut(&mut *broker).cast(),
                invoke,
            })
        };
        Self {
            controller,
            broker,
            runtime,
            opens: Vec::new(),
            lookups: Vec::new(),
            release: Some(release),
            entered,
            count,
            drops,
        }
    }
    fn request(&mut self, id: u64) {
        self.broker.lock().unwrap().active.insert(id);
        self.opens.push(PendingOpen::new(
            id,
            "video".into(),
            "display".into(),
            Some("same-op".into()),
            None,
        ));
    }
    fn poll(&mut self) -> Result<(), String> {
        poll_requests(
            &self.runtime,
            &self.controller,
            false,
            &mut self.opens,
            &mut self.lookups,
        )
    }
    fn start(&mut self) {
        self.request(1);
        self.poll().unwrap();
        self.entered.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    fn release(&mut self) {
        self.release.take().unwrap().send(()).unwrap();
    }
    fn wait_for(&mut self, id: u64) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self
            .broker
            .lock()
            .unwrap()
            .completed
            .iter()
            .any(|(request, _)| *request == id)
        {
            self.poll().unwrap();
            assert!(Instant::now() < deadline, "request did not complete");
            std::thread::yield_now();
        }
    }
}

#[test]
fn completion_ending_late_hands_the_same_open_to_replay_in_both_queue_orders() {
    for replay_first in [false, true] {
        let mut h = Harness::new();
        h.start();
        h.request(2);
        if replay_first {
            h.opens.swap(0, 1);
        }
        h.broker.lock().unwrap().expire_on_complete.insert(1);
        h.release();
        h.wait_for(2);
        let broker = h.broker.lock().unwrap();
        assert_eq!(broker.completed.len(), 1);
        assert_eq!(broker.completed[0].1["ok"], true);
        assert_eq!(h.count.load(Ordering::SeqCst), 1);
        assert_eq!(h.drops.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn unknown_replay_activity_defers_abandonment_until_it_can_inherit() {
    let mut h = Harness::new();
    h.start();
    h.request(2);
    {
        let mut broker = h.broker.lock().unwrap();
        broker.active.remove(&1);
        broker.unknown_activity.insert(2);
    }
    assert!(h.poll().unwrap_err().contains("activity check failed"));
    assert_eq!(h.count.load(Ordering::SeqCst), 1);
    assert_eq!(h.drops.load(Ordering::SeqCst), 0);
    h.release();
    h.wait_for(2);
    assert_eq!(h.count.load(Ordering::SeqCst), 1);
    assert_eq!(h.drops.load(Ordering::SeqCst), 0);
}

#[test]
fn unknown_activity_for_one_open_does_not_starve_an_independent_open() {
    let mut h = Harness::new();
    h.start();
    {
        let mut broker = h.broker.lock().unwrap();
        broker.active.insert(2);
        broker.unknown_activity.insert(1);
    }
    h.opens.push(PendingOpen::new(
        2,
        "video".into(),
        "other-display".into(),
        Some("independent-op".into()),
        None,
    ));
    assert!(h.poll().unwrap_err().contains("activity check failed"));
    h.entered.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(h.count.load(Ordering::SeqCst), 2);
    let release = h.release.as_ref().unwrap();
    release.send(()).unwrap();
    release.send(()).unwrap();
    h.wait_for(2);
}

#[test]
fn uncertain_completion_preserves_possibly_delivered_stream_for_lookup() {
    let mut h = Harness::new();
    h.start();
    h.broker.lock().unwrap().uncertain_complete.insert(1);
    h.release();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let result = h.poll();
        if !h.broker.lock().unwrap().delivered_uncertain.is_empty() {
            assert!(result.unwrap_err().contains("completion failed"));
            break;
        }
        result.unwrap();
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    h.broker.lock().unwrap().active.insert(3);
    h.lookups
        .push(PendingLookup::new(3, "video".into(), "same-op".into()));
    h.wait_for(3);
    let broker = h.broker.lock().unwrap();
    assert_eq!(
        broker.completed[0].1["stream"],
        broker.delivered_uncertain[0].1["stream"]
    );
    assert_eq!(h.drops.load(Ordering::SeqCst), 0);
    assert_eq!(h.count.load(Ordering::SeqCst), 1);
}

#[test]
fn shutdown_accepts_a_delivered_stream_already_retired_by_the_consumer() {
    let mut h = Harness::new();
    h.start();
    h.broker.lock().unwrap().uncertain_complete.insert(1);
    h.release();
    let deadline = Instant::now() + Duration::from_secs(3);
    let stream = loop {
        let result = h.poll();
        let stream = h
            .broker
            .lock()
            .unwrap()
            .delivered_uncertain
            .first()
            .and_then(|(_, value)| value["stream"].as_u64());
        if let Some(stream) = stream {
            assert!(result.is_err());
            break stream;
        }
        result.unwrap();
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    };
    let receipt = h
        .controller
        .lock()
        .unwrap()
        .begin_close("video", stream)
        .unwrap();
    while !receipt.retired() {
        assert!(h.controller.lock().unwrap().poll_closing().is_empty());
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    poll_requests(
        &h.runtime,
        &h.controller,
        true,
        &mut h.opens,
        &mut h.lookups,
    )
    .unwrap();
    assert!(h.opens.is_empty());
    assert!(h.controller.lock().unwrap().quiesce().unwrap());
    assert_eq!(h.drops.load(Ordering::SeqCst), 1);
}

#[derive(Clone, Copy)]
enum FailedInitialQuery {
    Geometry,
    TouchSurface,
}

struct FailedQuerySession {
    query: FailedInitialQuery,
    destructor_entered: mpsc::Sender<()>,
    release_destructor: mpsc::Receiver<()>,
    drops: Arc<AtomicUsize>,
}

impl CaptureSession for FailedQuerySession {
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        Ok(None)
    }

    fn geometry(&mut self) -> CaptureResult<Option<amaterasu_media::CaptureGeometry>> {
        if matches!(self.query, FailedInitialQuery::Geometry) {
            return Err(CaptureError::new(
                "backend_error",
                "initial geometry failed",
            ));
        }
        let rect = amaterasu_media::DesktopRect::new(0, 0, 1920, 1080).unwrap();
        Ok(Some(
            amaterasu_media::CaptureGeometry::new(rect, rect).unwrap(),
        ))
    }

    fn touch_surface(&mut self) -> CaptureResult<Option<TouchSurface>> {
        Err(CaptureError::new(
            "backend_error",
            "initial touch surface failed",
        ))
    }
}

impl Drop for FailedQuerySession {
    fn drop(&mut self) {
        let _ = self.destructor_entered.send(());
        let _ = self.release_destructor.recv_timeout(Duration::from_secs(5));
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

struct FailedQueryBackend {
    query: FailedInitialQuery,
    destructor_entered: mpsc::Sender<()>,
    release_destructor: Mutex<Option<mpsc::Receiver<()>>>,
    opens: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

impl Backend for FailedQueryBackend {
    fn supported(&self) -> bool {
        true
    }
    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        Ok(Vec::new())
    }
    fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FailedQuerySession {
            query: self.query,
            destructor_entered: self.destructor_entered.clone(),
            release_destructor: self.release_destructor.lock().unwrap().take().unwrap(),
            drops: self.drops.clone(),
        }))
    }
}

#[test]
fn failed_initial_queries_keep_the_opened_session_owned_until_its_destructor_exits() {
    for query in [
        FailedInitialQuery::Geometry,
        FailedInitialQuery::TouchSurface,
    ] {
        let (entered, destructor_entered) = mpsc::channel();
        let (release, release_destructor) = mpsc::channel();
        let opens = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let mut controller = Controller::new(
            FailedQueryBackend {
                query,
                destructor_entered: entered,
                release_destructor: Mutex::new(Some(release_destructor)),
                opens: opens.clone(),
                drops: drops.clone(),
            },
            true,
        );
        let OpenStart::Pending(token) = controller
            .begin_open("video", "display".into(), Some("failed-open".into()), None)
            .unwrap()
        else {
            panic!("expected asynchronous native open")
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        let (reply, stream) = loop {
            let started = Instant::now();
            let result = controller.poll_open(token, true).unwrap();
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "startup polling blocked"
            );
            if let Some(result) = result {
                break result;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(reply["error"]["code"], "backend_error");
        assert!(stream.is_none());
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        destructor_entered
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(controller.operation_pending("video", "failed-open"));
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let started = Instant::now();
        assert!(!controller.quiesce().unwrap());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "quiesce blocked on the destructor"
        );
        assert_eq!(
            controller.handle("video", Operation::Status, &mut || Ok(()))["ok"],
            true
        );
        assert!(controller.operation_pending("video", "failed-open"));
        release.send(()).unwrap();
        while !controller.quiesce().unwrap() {
            assert!(
                Instant::now() < deadline,
                "failed-open session did not retire"
            );
            std::thread::yield_now();
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(!controller.operation_pending("video", "failed-open"));
    }
}
