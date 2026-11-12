use super::*;
use crate::{CaptureModule, NativeBackend, SERVICE};
use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status};
use amaterasu_module_sdk::{ManagedModule, RuntimeClient};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn wait_quiescent(module: &mut CaptureModule) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !module.quiesce().unwrap() {
        assert!(Instant::now() < deadline, "capture did not retire");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn wait_closed(controller: &mut Controller<NativeBackend>, caller: &str, id: u64) {
    let receipt = controller.begin_close(caller, id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !receipt.retired() {
        assert!(controller.poll_closing().is_empty());
        assert!(Instant::now() < deadline, "capture did not retire");
        std::thread::sleep(Duration::from_millis(1));
    }
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

unsafe extern "C" fn idle_runtime(_: *mut c_void, _: BorrowedBytes, sink: ReplySink) -> Status {
    let response = serde_json::to_vec(&crate::Response::Pending).unwrap();
    // SAFETY: RuntimeClient provides this live reply sink for the current call.
    unsafe { sink.send(&response) };
    STATUS_OK
}

unsafe extern "C" fn counting_runtime(
    context: *mut c_void,
    _: BorrowedBytes,
    sink: ReplySink,
) -> Status {
    // SAFETY: the fixture retains the counter until after module destruction.
    let polls = unsafe { &*context.cast::<AtomicUsize>() };
    polls.fetch_add(1, Ordering::SeqCst);
    let response = serde_json::to_vec(&crate::Response::Pending).unwrap();
    // SAFETY: RuntimeClient provides this live reply sink for the current call.
    unsafe { sink.send(&response) };
    STATUS_OK
}

struct CancelledCloseRuntime {
    polls: AtomicUsize,
    checks: AtomicUsize,
    completions: AtomicUsize,
}

unsafe extern "C" fn cancelled_close_runtime(
    context: *mut c_void,
    input: BorrowedBytes,
    sink: ReplySink,
) -> Status {
    if context.is_null() {
        return amaterasu_module_sdk::abi::STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the test keeps this counter alive until after module destruction.
    let calls = unsafe { &*context.cast::<CancelledCloseRuntime>() };
    // SAFETY: RuntimeClient supplies command bytes valid for this call.
    let Ok(command) = (unsafe { input.read() }) else {
        return amaterasu_module_sdk::abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<serde_json::Value>(command) else {
        return amaterasu_module_sdk::abi::STATUS_INVALID_ARGUMENT;
    };
    let response = match command["op"].as_str() {
        Some("poll") if calls.polls.fetch_add(1, Ordering::SeqCst) == 0 => {
            crate::Response::Request {
                id: 19,
                caller: "video".into(),
                initiator: "client".into(),
                service: SERVICE.into(),
                payload: serde_json::json!({"action":"close","stream":1}),
                remaining_ms: 1_000,
            }
        }
        Some("poll") => crate::Response::Pending,
        Some("check_request") => {
            calls.checks.fetch_add(1, Ordering::SeqCst);
            crate::Response::Error {
                code: "request_ended".into(),
                message: "request was cancelled".into(),
            }
        }
        Some("complete") => {
            calls.completions.fetch_add(1, Ordering::SeqCst);
            crate::Response::Ok
        }
        _ => crate::Response::Pending,
    };
    let response = serde_json::to_vec(&response).unwrap();
    // SAFETY: RuntimeClient provides a live reply sink for this call.
    unsafe { sink.send(&response) };
    STATUS_OK
}

pub(crate) fn module_with_session(session: Box<dyn CaptureSession>) -> CaptureModule {
    let mut controller = Controller::new(NativeBackend, true);
    controller.streams.insert(
        1,
        Stream {
            owner: "video".into(),
            output: "fixture-monitor".into(),
            session,
            geometry: None,
            touch_surface: None,
            operation_id: Some("owned-open".into()),
            format: crate::format::CaptureFormat::default(),
        },
    );
    let controller = Arc::new(Mutex::new(controller));
    let factory = crate::native::Factory::new(controller.clone());
    let preflight =
        Arc::new(crate::preflight::Provider::new(controller.clone(), NativeBackend).unwrap());
    // SAFETY: both boxed factories and their controller remain owned by the module.
    let preflight_factory = unsafe {
        amaterasu_module_sdk::capture::SourcePreflightFactory::new(&factory.api, preflight.clone())
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
        preflight,
        shutdown_requested: false,
        pending_closes: Vec::new(),
        pending_operations: Vec::new(),
        pending_opens: Vec::new(),
        pending_lookups: Vec::new(),
        pending_enumerations: Vec::new(),
        pending_replies: Vec::new(),
        next_delivery_order: 0,
        poll_diagnostic_reported: false,
        _factory: factory,
        _preflight_factory: preflight_factory,
    }
}

pub(crate) fn add_session(
    module: &mut CaptureModule,
    id: u64,
    owner: &str,
    session: Box<dyn CaptureSession>,
) {
    module.controller.lock().unwrap().streams.insert(
        id,
        Stream {
            owner: owner.into(),
            output: format!("fixture-{id}"),
            session,
            geometry: None,
            touch_surface: None,
            operation_id: Some(format!("open-{id}")),
            format: crate::format::CaptureFormat::default(),
        },
    );
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
    wait_quiescent(&mut module);
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
            source_format: None,
        },
        &mut || panic!("shutdown must reject before admitting native work"),
    );
    assert_eq!(opened["error"]["code"], "capture_disabled");
    assert_eq!(
        controller.lookup("video", "owned-open").unwrap()["stream"],
        1
    );
    wait_closed(&mut controller, "video", 1);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn quiescence_does_not_wait_for_a_native_read_holding_the_controller() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(OwnedSession(drops.clone())));
    let (release, held_read) = hold_controller(&module);
    let started = std::time::Instant::now();
    assert!(!module.quiesce().unwrap());
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    release.send(()).unwrap();
    held_read.join().unwrap();
    wait_quiescent(&mut module);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

struct GatedSession {
    started: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

impl CaptureSession for GatedSession {
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        Ok(None)
    }
}

impl Drop for GatedSession {
    fn drop(&mut self) {
        let _ = self.started.send(());
        let _ = self.release.recv_timeout(Duration::from_secs(5));
    }
}

#[test]
fn quiescence_does_not_block_on_a_stuck_session_destructor() {
    let (started, entered) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let mut module = module_with_session(Box::new(GatedSession {
        started,
        release: gate,
    }));
    assert!(!module.quiesce().unwrap());
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    let start = Instant::now();
    assert!(!module.quiesce().unwrap());
    assert!(start.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    wait_quiescent(&mut module);
}

#[test]
fn exact_close_retains_owner_and_operation_until_session_retires() {
    let (started, entered) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let module = module_with_session(Box::new(GatedSession {
        started,
        release: gate,
    }));
    let mut controller = module.controller.lock().unwrap();
    let receipt = controller.begin_close("video", 1).unwrap();
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(controller.begin_close("other", 1).is_err());
    assert!(!receipt.retired());
    let reply = controller.handle(
        "video",
        Operation::Open {
            output: "fixture-monitor".into(),
            operation_id: Some("owned-open".into()),
            source_format: None,
        },
        &mut || panic!("the retiring operation must not admit a new effect"),
    );
    assert_eq!(reply["error"]["code"], "cleanup_pending");
    assert!(controller.poll_closing().is_empty());
    assert!(!receipt.retired());
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !receipt.retired() {
        assert!(controller.poll_closing().is_empty());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}

struct GatedOpenBackend {
    entered: std::sync::mpsc::Sender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
    drop_entered: std::sync::mpsc::Sender<()>,
    drop_release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl Backend for GatedOpenBackend {
    fn supported(&self) -> bool {
        true
    }
    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        Ok(Vec::new())
    }
    fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        self.entered.send(()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok(Box::new(GatedSession {
            started: self.drop_entered.clone(),
            release: self.drop_release.lock().unwrap().take().unwrap(),
        }))
    }
}

#[test]
fn cancelled_native_open_remains_owned_until_late_session_is_destroyed() {
    let (started, entered) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let (drop_started, drop_entered) = std::sync::mpsc::channel();
    let (drop_release, drop_gate) = std::sync::mpsc::channel();
    let mut controller = Controller::new(
        GatedOpenBackend {
            entered: started,
            release: Mutex::new(gate),
            drop_entered: drop_started,
            drop_release: Mutex::new(Some(drop_gate)),
        },
        true,
    );
    let id = match controller
        .begin_open("video", "fixture".into(), Some("request".into()), None)
        .unwrap()
    {
        OpenStart::Pending(id) => id,
        OpenStart::Ready(_) => panic!("new open must run on its worker"),
    };
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    controller.cancel_open(id);
    assert!(!controller.quiesce().unwrap());
    release.send(()).unwrap();
    controller.test_open_observation_failures = 1;
    assert!(controller.poll_open(id, false).is_err());
    assert!(controller.operation_pending("video", "request"));
    let deadline = Instant::now() + Duration::from_secs(3);
    while controller.poll_open(id, false).unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    drop_entered.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(controller.operation_pending("video", "request"));
    assert!(!controller.quiesce().unwrap());
    drop_release.send(()).unwrap();
    while !controller.quiesce().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[cfg(target_os = "linux")]
struct GatedThreadLocal {
    started: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

#[cfg(target_os = "linux")]
impl Drop for GatedThreadLocal {
    fn drop(&mut self) {
        let _ = self.started.send(());
        let _ = self.release.recv_timeout(Duration::from_secs(5));
    }
}

#[cfg(target_os = "linux")]
thread_local! {
    static RETIREMENT_TLS: std::cell::RefCell<Option<GatedThreadLocal>> = const { std::cell::RefCell::new(None) };
}

#[cfg(target_os = "linux")]
struct TlsSession(Option<GatedThreadLocal>);

#[cfg(target_os = "linux")]
impl CaptureSession for TlsSession {
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        Ok(None)
    }
}

#[cfg(target_os = "linux")]
impl Drop for TlsSession {
    fn drop(&mut self) {
        RETIREMENT_TLS.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn quiescence_waits_for_native_thread_termination_after_session_drop() {
    let (started, entered) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let mut module = module_with_session(Box::new(TlsSession(Some(GatedThreadLocal {
        started,
        release: gate,
    }))));
    assert!(!module.quiesce().unwrap());
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    let start = Instant::now();
    assert!(!module.quiesce().unwrap());
    assert!(start.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    wait_quiescent(&mut module);
}

#[test]
fn tick_can_poll_without_waiting_for_a_native_read_owning_the_controller() {
    let polls = AtomicUsize::new(0);
    let mut module = module_with_session(Box::new(OwnedSession(Arc::new(AtomicUsize::new(0)))));
    // SAFETY: polls stays alive until the module has been dropped below.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::from_ref(&polls).cast_mut().cast(),
            invoke: counting_runtime,
        })
    };
    let (release, held_read) = hold_controller(&module);
    let started = std::time::Instant::now();
    module.tick().unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    release.send(()).unwrap();
    held_read.join().unwrap();
    module.tick().unwrap();
    assert_eq!(polls.load(Ordering::SeqCst), 2);
    drop(module);
}

#[test]
fn cancelled_close_does_not_retire_the_owned_capture_stream() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(OwnedSession(drops.clone())));
    let calls = CancelledCloseRuntime {
        polls: AtomicUsize::new(0),
        checks: AtomicUsize::new(0),
        completions: AtomicUsize::new(0),
    };
    // SAFETY: calls remains live until the module is dropped below.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::from_ref(&calls).cast_mut().cast(),
            invoke: cancelled_close_runtime,
        })
    };

    module.tick().unwrap();

    assert_eq!(calls.checks.load(Ordering::SeqCst), 1);
    assert_eq!(calls.completions.load(Ordering::SeqCst), 0);
    assert!(module.controller.lock().unwrap().streams.contains_key(&1));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(module);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn busy_shutdown_announcement_preserves_admission_fence_for_later_ticks() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module = module_with_session(Box::new(OwnedSession(drops.clone())));
    let controller = module.controller.clone();
    let (release, held_read) = hold_controller(&module);
    let started = std::time::Instant::now();
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
            source_format: None,
        },
        &mut || panic!("deferred shutdown admission must precede native allocation"),
    );
    assert_eq!(opened["error"]["code"], "capture_disabled");
    assert_eq!(drops.load(Ordering::SeqCst), 0);
}
