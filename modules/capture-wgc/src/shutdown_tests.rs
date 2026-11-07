use super::*;
use crate::{CaptureModule, NativeBackend};
use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status};
use amaterasu_module_sdk::{ManagedModule, RuntimeClient};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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

unsafe extern "C" fn idle_runtime(_: *mut c_void, _: BorrowedBytes, sink: ReplySink) -> Status {
    let response = serde_json::to_vec(&crate::Response::Pending).unwrap();
    // SAFETY: RuntimeClient provides this live reply sink for the current call.
    unsafe { sink.send(&response) };
    STATUS_OK
}

fn module_with_session(session: Box<dyn CaptureSession>) -> CaptureModule {
    let mut controller = Controller::new(NativeBackend::new(None), true);
    controller.streams.insert(
        1,
        Stream {
            capture_format: crate::format::CaptureFormat::default(),
            owner: "video".into(),
            output: "fixture-monitor".into(),
            session,
            geometry: None,
            touch_surface: None,
            operation_id: Some("owned-open".into()),
            constant_rate: false,
            opened_at: Instant::now(),
            last_frame: None,
            next_sequence: 0,
            last_timestamp_us: 0,
            close_receipt: None,
        },
    );
    let controller = Arc::new(Mutex::new(controller));
    let factory = amaterasu_module_sdk::capture::CpuSourceFactory::new(Arc::new(
        crate::resource::Provider::new(controller.clone()),
    ));
    // SAFETY: both boxed factories and their controller remain owned by the module.
    let preflight_factory = unsafe {
        amaterasu_module_sdk::capture::SourcePreflightFactory::new(
            &factory.api,
            Arc::new(crate::preflight::Provider::new(controller.clone())),
        )
    };
    // SAFETY: this stateless endpoint is valid for the entire test; shutdown
    // must not need runtime traffic or native capture initialization.
    let runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::null_mut(),
            invoke: idle_runtime,
        })
    };
    CaptureModule {
        runtime,
        controller,
        shutdown_requested: false,
        pending_closes: Vec::new(),
        _factory: factory,
        _preflight_factory: preflight_factory,
    }
}

fn hold_controller(
    module: &CaptureModule,
) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let controller = module.controller.clone();
    let (held, locked) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let _read = controller.lock().unwrap();
        held.send(()).unwrap();
        let _ = gate.recv_timeout(std::time::Duration::from_secs(5));
    });
    locked
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
    (release, thread)
}

#[test]
fn module_quiescence_retires_owned_capture_before_dependencies_can_shutdown() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(OwnedSession(drops.clone())));
    assert!(module.quiesce().unwrap());
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "ready must prove capture resources were retired"
    );
    assert!(module.controller.lock().unwrap().streams.is_empty());
    assert!(module.quiesce().unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn shutdown_announcement_closes_admission_without_retiring_consumers_capture() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(OwnedSession(drops.clone())));
    module.prepare_shutdown(1_000).unwrap();
    module.prepare_shutdown(0).unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let mut controller = module.controller.lock().unwrap();
    assert!(controller.read("video", 1, 0).unwrap().is_none());
    let opened = controller.handle(
        "new-owner",
        Operation::Open {
            output: "fixture-monitor".into(),
            operation_id: Some("new-open".into()),
            constant_rate: false,
            source_format: None,
        },
        &mut || panic!("shutdown must reject before admitting native work"),
    );
    assert_eq!(opened["error"]["code"], "capture_disabled");
    assert_eq!(
        controller.lookup("video", "owned-open").unwrap()["stream"],
        1
    );
    controller.close("video", 1).unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn quiescence_does_not_wait_for_a_native_read_holding_the_controller() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(OwnedSession(drops.clone())));
    let (release, held_read) = hold_controller(&module);
    let started = Instant::now();
    assert!(!module.quiesce().unwrap());
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    release.send(()).unwrap();
    held_read.join().unwrap();
    assert!(module.quiesce().unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn busy_shutdown_announcement_preserves_admission_fence_for_later_ticks() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(OwnedSession(drops.clone())));
    let controller = module.controller.clone();
    let (release, held_read) = hold_controller(&module);
    let started = Instant::now();
    module.prepare_shutdown(1_000).unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    release.send(()).unwrap();
    held_read.join().unwrap();
    module.tick().unwrap();
    let mut controller = controller.lock().unwrap();
    assert!(controller.read("video", 1, 0).unwrap().is_none());
    let opened = controller.handle(
        "new-owner",
        Operation::Open {
            output: "fixture-monitor".into(),
            operation_id: Some("new-open".into()),
            constant_rate: false,
            source_format: None,
        },
        &mut || panic!("deferred shutdown admission must precede native allocation"),
    );
    assert_eq!(opened["error"]["code"], "capture_disabled");
    assert_eq!(drops.load(Ordering::SeqCst), 0);
}

struct HeldSource {
    poll_entered: std::sync::mpsc::Sender<()>,
    poll_gate: std::sync::mpsc::Receiver<()>,
    drop_entered: std::sync::mpsc::Sender<()>,
    drop_gate: std::sync::mpsc::Receiver<()>,
    drops: Arc<AtomicUsize>,
}

impl crate::worker::FrameSource for HeldSource {
    fn poll(&mut self) -> CaptureResult<Option<Frame>> {
        self.poll_entered.send(()).unwrap();
        // A failing implementation must eventually release the test thread.
        let _ = self
            .poll_gate
            .recv_timeout(std::time::Duration::from_secs(5));
        Ok(None)
    }
}

impl Drop for HeldSource {
    fn drop(&mut self) {
        let _ = self.drop_entered.send(());
        let _ = self
            .drop_gate
            .recv_timeout(std::time::Duration::from_secs(5));
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn module_quiescence_keeps_dependencies_alive_until_worker_destruction_finishes() {
    use std::sync::mpsc;
    use std::time::Duration;
    let (poll_entered, polled) = mpsc::channel();
    let (poll_gate, poll_wait) = mpsc::channel();
    let (drop_entered, dropping) = mpsc::channel();
    let (drop_gate, drop_wait) = mpsc::channel();
    let drops = Arc::new(AtomicUsize::new(0));
    let worker_drops = drops.clone();
    let mut worker = crate::worker::Worker::start(move || {
        Ok(HeldSource {
            poll_entered,
            poll_gate: poll_wait,
            drop_entered,
            drop_gate: drop_wait,
            drops: worker_drops,
        })
    })
    .unwrap();
    assert!(worker.read(0).unwrap().is_none());
    polled.recv_timeout(Duration::from_secs(1)).unwrap();
    let mut module = module_with_session(Box::new(worker));
    let started = Instant::now();
    assert!(!module.quiesce().unwrap());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(module.controller.lock().unwrap().streams.len(), 1);

    poll_gate.send(()).unwrap();
    dropping.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        !module.quiesce().unwrap(),
        "a running destructor still owns native resources"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop_gate.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !module.quiesce().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(module.controller.lock().unwrap().streams.is_empty());
}

struct PanickingSource;

impl crate::worker::FrameSource for PanickingSource {
    fn poll(&mut self) -> CaptureResult<Option<Frame>> {
        panic!("capture fixture poll failed");
    }
}

#[test]
fn worker_panic_cannot_become_successful_module_retirement_on_retry() {
    let mut worker = crate::worker::Worker::start(|| Ok(PanickingSource)).unwrap();
    assert_eq!(worker.read(1_000).unwrap_err().code, "access_lost");
    let mut module = module_with_session(Box::new(worker));
    let deadline = Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match module.quiesce() {
            Ok(false) => {
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            Err(error) => {
                assert!(error.contains("panicked"));
                break;
            }
            Ok(true) => panic!("panicked retirement must not acknowledge success"),
        }
    }
    assert!(module.quiesce().unwrap_err().contains("panicked"));
    assert_eq!(module.controller.lock().unwrap().streams.len(), 1);
}

#[derive(Default)]
struct Broker {
    requests: std::collections::VecDeque<crate::Response>,
    active: std::collections::BTreeSet<u64>,
    completed: Vec<(u64, serde_json::Value)>,
    completion_attempts: Vec<u64>,
    completion_failures: usize,
    activity_failures: usize,
    session_drops: Option<Arc<AtomicUsize>>,
    retirement_counts_at_completion: Vec<usize>,
}

impl Broker {
    fn close(&mut self, request: u64, caller: &str, stream: u64) {
        self.active.insert(request);
        self.requests.push_back(crate::Response::Request {
            id: request,
            caller: caller.into(),
            initiator: caller.into(),
            service: crate::SERVICE.into(),
            payload: json!({"action":"close", "stream":stream}),
            remaining_ms: 30_000,
        });
    }
}

unsafe extern "C" fn broker_runtime(
    context: *mut c_void,
    input: BorrowedBytes,
    sink: ReplySink,
) -> Status {
    // SAFETY: each test retains this boxed broker through all module calls.
    let broker = unsafe { &*context.cast::<Mutex<Broker>>() };
    // SAFETY: RuntimeClient retains these input bytes through this call.
    let command: crate::Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let mut broker = broker.lock().unwrap();
    let response = match command {
        crate::Command::Poll {} => broker
            .requests
            .pop_front()
            .unwrap_or(crate::Response::Pending),
        crate::Command::CheckRequest { .. } if broker.activity_failures != 0 => {
            broker.activity_failures -= 1;
            crate::Response::Error {
                code: "fixture_check_failed".into(),
                message: "activity is unknown".into(),
            }
        }
        crate::Command::CheckRequest { request } if broker.active.contains(&request) => {
            crate::Response::Ok
        }
        crate::Command::Complete { request, payload } => {
            broker.completion_attempts.push(request);
            if broker.completion_failures != 0 {
                broker.completion_failures -= 1;
                crate::Response::Error {
                    code: "fixture_completion_failed".into(),
                    message: "retry this exact completion".into(),
                }
            } else if broker.active.remove(&request) {
                if let Some(drops) = &broker.session_drops {
                    let retired = drops.load(Ordering::SeqCst);
                    broker.retirement_counts_at_completion.push(retired);
                }
                broker.completed.push((request, payload));
                crate::Response::Ok
            } else {
                crate::Response::Error {
                    code: "request_ended".into(),
                    message: "fixture request is no longer active".into(),
                }
            }
        }
        crate::Command::CheckRequest { .. } => crate::Response::Error {
            code: "request_ended".into(),
            message: "fixture request is no longer active".into(),
        },
        _ => panic!("unexpected capture broker command"),
    };
    // SAFETY: RuntimeClient provides this live reply sink for the current call.
    unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

struct ObservedSession {
    worker: crate::worker::Worker,
    drops: Arc<AtomicUsize>,
    access_lost: bool,
}

impl CaptureSession for ObservedSession {
    fn source_info(&mut self) -> CaptureResult<Option<crate::format::SourceInfo>> {
        Ok(Some(crate::format::SourceInfo {
            width: 1,
            height: 1,
            encoding: crate::format::SourceEncoding::Bgra8,
            desktop_hdr: false,
        }))
    }

    fn read(&mut self, wait_ms: u32) -> CaptureResult<Option<Frame>> {
        if self.access_lost {
            Err(CaptureError::new(
                "access_lost",
                "fixture capture item closed",
            ))
        } else {
            self.worker.read(wait_ms)
        }
    }

    fn quiesce(&mut self) -> CaptureResult<bool> {
        self.worker.quiesce()
    }
}

impl Drop for ObservedSession {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn bind_broker(module: &mut CaptureModule, broker: &mut Mutex<Broker>) {
    // SAFETY: tests retain the boxed broker until after all runtime calls.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::from_mut(broker).cast(),
            invoke: broker_runtime,
        })
    };
}

#[test]
fn broker_close_retains_its_request_until_worker_cleanup_retires_without_blocking_tick() {
    use std::sync::mpsc;
    use std::time::Duration;
    let (poll_entered, polled) = mpsc::channel();
    let (poll_gate, poll_wait) = mpsc::channel();
    let (drop_entered, dropping) = mpsc::channel();
    let (drop_gate, drop_wait) = mpsc::channel();
    let drops = Arc::new(AtomicUsize::new(0));
    let worker_drops = drops.clone();
    let mut worker = crate::worker::Worker::start(move || {
        Ok(HeldSource {
            poll_entered,
            poll_gate: poll_wait,
            drop_entered,
            drop_gate: drop_wait,
            drops: worker_drops,
        })
    })
    .unwrap();
    assert!(worker.read(0).unwrap().is_none());
    polled.recv_timeout(Duration::from_secs(1)).unwrap();
    let session_drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(ObservedSession {
        worker,
        drops: session_drops.clone(),
        access_lost: false,
    }));
    let mut broker = Box::new(Mutex::new(Broker::default()));
    broker.lock().unwrap().session_drops = Some(session_drops.clone());
    bind_broker(&mut module, &mut broker);
    broker.lock().unwrap().close(41, "video", 1);
    let started = Instant::now();
    module.tick().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a real broker Close must not join a held capture worker on the host tick"
    );
    assert!(broker.lock().unwrap().completed.is_empty());
    assert_eq!(module.controller.lock().unwrap().streams.len(), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(session_drops.load(Ordering::SeqCst), 0);
    {
        let mut controller = module.controller.lock().unwrap();
        assert_eq!(
            controller.read("video", 1, 0).unwrap_err().code,
            "access_lost"
        );
        assert_eq!(
            controller.lookup("video", "owned-open").unwrap()["stream"],
            1
        );
        let replay = controller.handle(
            "video",
            Operation::Open {
                output: "fixture-monitor".into(),
                operation_id: Some("owned-open".into()),
                constant_rate: false,
                source_format: None,
            },
            &mut || panic!("a retiring capture must not reopen"),
        );
        assert_eq!(replay["error"]["code"], "cleanup_pending");
    }
    poll_gate.send(()).unwrap();
    dropping.recv_timeout(Duration::from_secs(1)).unwrap();
    module.tick().unwrap();
    assert!(broker.lock().unwrap().completed.is_empty());
    drop_gate.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while broker.lock().unwrap().completed.is_empty() {
        assert!(Instant::now() < deadline);
        module.tick().unwrap();
        std::thread::yield_now();
    }
    assert_eq!(broker.lock().unwrap().completed, [(41, json!({"ok":true}))]);
    assert_eq!(broker.lock().unwrap().retirement_counts_at_completion, [1]);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(module.controller.lock().unwrap().streams.is_empty());
}

struct ControlledSession {
    ready: Arc<std::sync::atomic::AtomicBool>,
    polls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
    retire_after: usize,
}

impl CaptureSession for ControlledSession {
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        Ok(None)
    }

    fn quiesce(&mut self) -> CaptureResult<bool> {
        let polls = self.polls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(self.ready.load(Ordering::SeqCst) || polls >= self.retire_after)
    }
}

impl Drop for ControlledSession {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn controlled_module(
    retire_after: usize,
) -> (
    CaptureModule,
    Arc<std::sync::atomic::AtomicBool>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let polls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let module = module_with_session(Box::new(ControlledSession {
        ready: ready.clone(),
        polls: polls.clone(),
        drops: drops.clone(),
        retire_after,
    }));
    (module, ready, polls, drops)
}

#[test]
fn cancelled_close_waiter_does_not_cancel_native_cleanup_or_restart_a_retry() {
    let (mut module, ready, _, drops) = controlled_module(usize::MAX);
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    broker.lock().unwrap().close(41, "video", 1);
    module.tick().unwrap();
    let receipt = module.pending_closes[0].receipt.clone().unwrap();
    broker.lock().unwrap().active.remove(&41);
    broker.lock().unwrap().close(42, "video", 1);
    module.tick().unwrap();
    assert_eq!(module.pending_closes.len(), 1);
    assert_eq!(module.pending_closes[0].request, Some(42));
    assert!(Arc::ptr_eq(
        &receipt.0,
        &module.pending_closes[0].receipt.as_ref().unwrap().0
    ));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    ready.store(true, Ordering::SeqCst);
    module.tick().unwrap();
    module.tick().unwrap();
    assert_eq!(broker.lock().unwrap().completed, [(42, json!({"ok":true}))]);
    assert_eq!(broker.lock().unwrap().completion_attempts, [42]);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(module.pending_closes.is_empty());
}

#[test]
fn accepted_close_cancelled_while_controller_busy_keeps_its_cleanup_target() {
    let (mut module, ready, polls, drops) = controlled_module(usize::MAX);
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    let (release, held_read) = hold_controller(&module);
    broker.lock().unwrap().close(41, "video", 1);
    module.tick().unwrap();
    assert!(module.pending_closes[0].admitted);
    broker.lock().unwrap().active.remove(&41);
    module.tick().unwrap();
    assert_eq!(module.pending_closes[0].request, None);
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    release.send(()).unwrap();
    held_read.join().unwrap();
    module.tick().unwrap();
    assert!(
        module.pending_closes.is_empty(),
        "the stream now owns orphan cleanup"
    );
    assert_eq!(module.controller.lock().unwrap().streams.len(), 1);
    assert!(polls.load(Ordering::SeqCst) > 0);
    ready.store(true, Ordering::SeqCst);
    module.tick().unwrap();
    assert!(module.controller.lock().unwrap().streams.is_empty());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(broker.lock().unwrap().completion_attempts.is_empty());
}

#[test]
fn expired_unadmitted_close_does_not_retire_an_active_capture() {
    let (mut module, _, polls, drops) = controlled_module(usize::MAX);
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    broker.lock().unwrap().close(41, "video", 1);
    broker.lock().unwrap().active.remove(&41);
    module.tick().unwrap();
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert!(
        module
            .controller
            .lock()
            .unwrap()
            .read("video", 1, 0)
            .unwrap()
            .is_none()
    );
    assert!(module.pending_closes.is_empty());
}

#[test]
fn failed_activity_check_does_not_admit_native_cleanup() {
    let (mut module, ready, polls, drops) = controlled_module(usize::MAX);
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    broker.lock().unwrap().activity_failures = 1;
    broker.lock().unwrap().close(41, "video", 1);
    assert!(module.tick().unwrap_err().contains("activity check failed"));
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert!(!module.pending_closes[0].admitted);
    assert!(broker.lock().unwrap().completed.is_empty());
    module.tick().unwrap();
    assert!(module.pending_closes[0].admitted);
    ready.store(true, Ordering::SeqCst);
    module.tick().unwrap();
    assert_eq!(broker.lock().unwrap().completed, [(41, json!({"ok":true}))]);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn foreign_close_does_not_admit_retirement_or_observe_owner_failure() {
    let (mut module, _, polls, drops) = controlled_module(usize::MAX);
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    broker.lock().unwrap().close(41, "other-owner", 1);
    module.tick().unwrap();
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(
        broker.lock().unwrap().completed[0].1["error"]["code"],
        "not_found"
    );
    assert!(
        module
            .controller
            .lock()
            .unwrap()
            .read("video", 1, 0)
            .unwrap()
            .is_none()
    );
}

#[test]
fn global_quiescence_preserves_exact_close_proof_after_removing_the_native_session() {
    let (mut module, _, polls, drops) = controlled_module(3);
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    broker.lock().unwrap().close(41, "video", 1);
    module.tick().unwrap();
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert!(module.quiesce().unwrap());
    assert_eq!(polls.load(Ordering::SeqCst), 3);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(broker.lock().unwrap().completed, [(41, json!({"ok":true}))]);
    assert!(module.pending_closes.is_empty());
}

#[test]
fn completion_retry_keeps_the_retired_receipt_without_restarting_cleanup() {
    let (mut module, ready, polls, drops) = controlled_module(usize::MAX);
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    broker.lock().unwrap().close(41, "video", 1);
    module.tick().unwrap();
    ready.store(true, Ordering::SeqCst);
    broker.lock().unwrap().completion_failures = 1;
    assert!(module.tick().unwrap_err().contains("completion failed"));
    let retired_polls = polls.load(Ordering::SeqCst);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(module.pending_closes.len(), 1);
    module.tick().unwrap();
    assert_eq!(polls.load(Ordering::SeqCst), retired_polls);
    assert_eq!(broker.lock().unwrap().completion_attempts, [41, 41]);
    assert_eq!(broker.lock().unwrap().completed, [(41, json!({"ok":true}))]);
    assert!(module.pending_closes.is_empty());
}

#[test]
fn close_waiter_capacity_is_bounded_and_cancellation_preserves_owned_cleanup() {
    let (mut module, ready, _, drops) = controlled_module(usize::MAX);
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    for request in 0..=crate::MAX_PENDING_CLOSES as u64 {
        broker.lock().unwrap().close(request + 100, "video", 1);
    }
    for _ in 0..5 {
        module.tick().unwrap();
    }
    assert_eq!(module.pending_closes.len(), crate::MAX_PENDING_CLOSES);
    assert_eq!(broker.lock().unwrap().completed.len(), 1);
    assert_eq!(
        broker.lock().unwrap().completed[0].1["error"]["code"],
        "resource_exhausted"
    );
    broker.lock().unwrap().active.clear();
    module.tick().unwrap();
    assert!(module.pending_closes.is_empty());
    assert_eq!(module.controller.lock().unwrap().streams.len(), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    ready.store(true, Ordering::SeqCst);
    module.tick().unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(module.controller.lock().unwrap().streams.is_empty());
}

#[test]
fn broker_close_reports_worker_panic_once_without_claiming_native_retirement() {
    let mut worker = crate::worker::Worker::start(|| Ok(PanickingSource)).unwrap();
    assert_eq!(worker.read(1_000).unwrap_err().code, "access_lost");
    let mut module = module_with_session(Box::new(worker));
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    broker.lock().unwrap().close(41, "video", 1);
    let deadline = Instant::now() + std::time::Duration::from_secs(2);
    while broker.lock().unwrap().completed.is_empty() {
        assert!(Instant::now() < deadline);
        let _ = module.tick();
        std::thread::yield_now();
    }
    assert_eq!(broker.lock().unwrap().completed[0].0, 41);
    assert_eq!(
        broker.lock().unwrap().completed[0].1["error"]["code"],
        "backend_error"
    );
    assert_eq!(broker.lock().unwrap().completion_attempts, [41]);
    assert_eq!(module.controller.lock().unwrap().streams.len(), 1);
    assert!(module.pending_closes.is_empty());
    assert!(module.quiesce().unwrap_err().contains("panicked"));
}

#[test]
fn failed_worker_retirement_keeps_status_and_explicit_close_retries_usable() {
    let mut worker = crate::worker::Worker::start(|| Ok(PanickingSource)).unwrap();
    assert_eq!(worker.read(1_000).unwrap_err().code, "access_lost");
    let mut module = module_with_session(Box::new(worker));
    let mut broker = Box::new(Mutex::new(Broker::default()));
    bind_broker(&mut module, &mut broker);
    broker.lock().unwrap().close(41, "video", 1);
    let deadline = Instant::now() + std::time::Duration::from_secs(2);
    while broker.lock().unwrap().completed.is_empty() {
        assert!(Instant::now() < deadline);
        let _ = module.tick();
        std::thread::yield_now();
    }
    {
        let mut broker = broker.lock().unwrap();
        broker.close(42, "video", 1);
        broker.active.insert(43);
        broker.requests.push_back(crate::Response::Request {
            id: 43,
            caller: "video".into(),
            initiator: "video".into(),
            service: crate::SERVICE.into(),
            payload: json!({"action":"status"}),
            remaining_ms: 30_000,
        });
    }
    module.tick().unwrap();
    let broker = broker.lock().unwrap();
    assert_eq!(
        broker.completed.iter().find(|(id, _)| *id == 42).unwrap().1["error"]["code"],
        "backend_error"
    );
    assert_eq!(
        broker.completed.iter().find(|(id, _)| *id == 43).unwrap().1["ok"],
        true
    );
    assert_eq!(module.controller.lock().unwrap().streams.len(), 1);
}

unsafe extern "C" fn unexpected_frame(
    _: *mut c_void,
    _: *const amaterasu_media::CpuFrame,
) -> Status {
    amaterasu_media::MEDIA_BACKEND_ERROR
}

#[test]
fn native_terminal_error_returns_before_a_held_source_destructor_and_keeps_join_owned() {
    use std::sync::mpsc;
    use std::time::Duration;
    let (poll_entered, polled) = mpsc::channel();
    let (poll_gate, poll_wait) = mpsc::channel();
    let (drop_entered, dropping) = mpsc::channel();
    let (drop_gate, drop_wait) = mpsc::channel();
    let source_drops = Arc::new(AtomicUsize::new(0));
    let worker_drops = source_drops.clone();
    let mut worker = crate::worker::Worker::start(move || {
        Ok(HeldSource {
            poll_entered,
            poll_gate: poll_wait,
            drop_entered,
            drop_gate: drop_wait,
            drops: worker_drops,
        })
    })
    .unwrap();
    assert!(worker.read(0).unwrap().is_none());
    polled.recv_timeout(Duration::from_secs(1)).unwrap();
    let session_drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(ObservedSession {
        worker,
        drops: session_drops.clone(),
        access_lost: true,
    }));
    let query = amaterasu_module_sdk::abi::NativeInterfaceQuery {
        header: AbiHeader::of::<amaterasu_module_sdk::abi::NativeInterfaceQuery>(),
        interface_major: 1,
        min_minor: 0,
        min_size: amaterasu_media::CAPTURE_API_V1_0_SIZE,
    };
    let mut table = std::ptr::null();
    // SAFETY: the boxed production SDK factory, owner/query, and binding table
    // remain live throughout this synchronous callback and its single release.
    let status = unsafe {
        assert_eq!(
            module._factory.api.bind.unwrap()(
                module._factory.api.context,
                BorrowedBytes::new(b"video"),
                &query,
                &mut table
            ),
            STATUS_OK
        );
        let api = &*table.cast::<amaterasu_media::CaptureApi>();
        let started = Instant::now();
        let status = (api.read_frame)(
            api.base.context,
            1,
            amaterasu_media::FrameSink {
                context: std::ptr::null_mut(),
                write: unexpected_frame,
            },
        );
        api.base.release.unwrap()(api.base.context);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a terminal native callback must not join a held worker"
        );
        status
    };
    assert_eq!(status, amaterasu_media::MEDIA_CLOSED);
    assert_eq!(session_drops.load(Ordering::SeqCst), 0);
    assert_eq!(module.controller.lock().unwrap().streams.len(), 1);
    poll_gate.send(()).unwrap();
    dropping.recv_timeout(Duration::from_secs(1)).unwrap();
    module.tick().unwrap();
    assert_eq!(source_drops.load(Ordering::SeqCst), 0);
    assert_eq!(session_drops.load(Ordering::SeqCst), 0);
    drop_gate.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !module.controller.lock().unwrap().streams.is_empty() {
        assert!(Instant::now() < deadline);
        module.tick().unwrap();
        std::thread::yield_now();
    }
    assert_eq!(source_drops.load(Ordering::SeqCst), 1);
    assert_eq!(session_drops.load(Ordering::SeqCst), 1);
}
