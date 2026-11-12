//! Exercise a held metadata query through the actual Capture v1.11 SDK table.
use super::*;
use crate::backend::{Backend, CaptureSession};
use crate::model::{CaptureResult, Frame, Operation, Output};
use amaterasu_media::{CAPTURE_API_V1_11_SIZE, CaptureApiV1_11};
use amaterasu_module_sdk::{
    abi::{
        AbiHeader, BorrowedBytes, NativeInterface, NativeInterfaceQuery, ReplySink, STATUS_OK,
        Status,
    },
    capture::{CpuSourceFactory, SourceMode, SourcePreflightFactory},
};
use std::ffi::c_void;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

struct BackendFixture;
struct SessionFixture;
impl Backend for BackendFixture {
    fn supported(&self) -> bool {
        true
    }
    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        Ok(Vec::new())
    }
    fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        Ok(Box::new(SessionFixture))
    }
}
impl CaptureSession for SessionFixture {
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        Ok(None)
    }
}

#[derive(Default)]
struct SinkState {
    in_callback: AtomicBool,
    calls: AtomicUsize,
    late_calls: AtomicUsize,
}
unsafe extern "C" fn sink_write(context: *mut c_void, _: BorrowedBytes) {
    // SAFETY: fixture retains this sink through worker retirement, including
    // after the native call, to detect an invalid deferred callback safely.
    let sink = unsafe { &*context.cast::<SinkState>() };
    sink.calls.fetch_add(1, Ordering::SeqCst);
    if !sink.in_callback.load(Ordering::SeqCst) {
        sink.late_calls.fetch_add(1, Ordering::SeqCst);
    }
}

struct Binding(*const NativeInterface);
impl Binding {
    fn new(factory: &amaterasu_module_sdk::abi::NativeServiceFactory) -> Self {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: 1,
            min_minor: 11,
            min_size: CAPTURE_API_V1_11_SIZE,
        };
        let mut output = std::ptr::null();
        // SAFETY: the boxed factories, caller and query remain live through bind.
        let status = unsafe {
            factory.bind.unwrap()(
                factory.context,
                BorrowedBytes::new(b"owner"),
                &query,
                &mut output,
            )
        };
        assert_eq!(status, STATUS_OK);
        assert!(!output.is_null());
        Self(output)
    }
    fn query(&self, request: &SourcePreflightRequest, sink: &SinkState) -> Status {
        let bytes = serde_json::to_vec(request).unwrap();
        // SAFETY: this binding is the complete v1.11 table and remains live;
        // the request and sink are retained for the entire synchronous call.
        let api = unsafe { &*self.0.cast::<CaptureApiV1_11>() };
        sink.in_callback.store(true, Ordering::SeqCst);
        let status = unsafe {
            (api.source_preflight)(
                api.base.base.base.base.base.context,
                BorrowedBytes::new(&bytes),
                ReplySink {
                    context: (sink as *const SinkState).cast_mut().cast(),
                    write: sink_write,
                },
            )
        };
        sink.in_callback.store(false, Ordering::SeqCst);
        status
    }
}
impl Drop for Binding {
    fn drop(&mut self) {
        // SAFETY: the fixture owns this binding uniquely after native callbacks return.
        let base = unsafe { &*self.0 };
        unsafe { base.release.unwrap()(base.context) };
    }
}

fn request() -> SourcePreflightRequest {
    SourcePreflightRequest {
        source_class: SourceClass::ManagedOutput,
        route: None,
        mode: SourceMode {
            width: 1920,
            height: 1080,
            refresh_millihz: 60_000,
            hdr: false,
            bit_depth: None,
        },
    }
}

#[test]
fn held_metadata_does_not_hold_controller_or_keep_a_borrowed_sink() {
    let controller = Arc::new(Mutex::new(Controller::new(BackendFixture, true)));
    let stream = controller.lock().unwrap().handle(
        "owner",
        Operation::Open {
            output: "display".into(),
            operation_id: None,
            constant_rate: false,
            source_format: None,
        },
        &mut || Ok(()),
    )["stream"]
        .as_u64()
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let provider = Arc::new(
        Provider::with_observer(controller.clone(), move |caller, request| {
            assert_eq!(caller, "owner");
            entered_tx.send(()).unwrap();
            // Bounded even if an assertion in the caller fails, so Drop cannot hang.
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
            Ok(ready(request))
        })
        .unwrap(),
    );
    let inner = CpuSourceFactory::new(Arc::new(crate::resource::Provider::new(controller.clone())));
    // SAFETY: CpuSourceFactory binds metadata only; both factories and every
    // binding outlive their callbacks and the retained provider.
    let factory = unsafe { SourcePreflightFactory::new(&inner.api, provider.clone()) };
    let binding = Binding::new(&factory.api);
    let sink = SinkState::default();
    let request = request();

    let start = Instant::now();
    assert_eq!(binding.query(&request, &sink), MEDIA_BUSY);
    assert!(start.elapsed() < Duration::from_secs(1));
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker entered metadata query");
    let start = Instant::now();
    assert_eq!(binding.query(&request, &sink), MEDIA_BUSY);
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(sink.calls.load(Ordering::SeqCst), 0);

    let mut guard = controller
        .try_lock()
        .expect("metadata does not hold controller lock");
    guard
        .close("owner", stream)
        .expect("close is available during metadata");
    assert!(guard.quiesce().unwrap());
    drop(guard);
    assert!(!provider.quiesce().unwrap(), "held worker is not retired");
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !provider.quiesce().unwrap() {
        assert!(Instant::now() < deadline, "worker did not retire");
        std::thread::yield_now();
    }
    assert_eq!(
        sink.calls.load(Ordering::SeqCst),
        0,
        "late metadata is discarded"
    );
    assert_eq!(
        sink.late_calls.load(Ordering::SeqCst),
        0,
        "sink never outlives its call"
    );
    drop(binding);
}

#[test]
fn shutdown_discards_a_late_observation_even_if_controller_remains_enabled() {
    let controller = Arc::new(Mutex::new(Controller::new(BackendFixture, true)));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let provider = Arc::new(
        Provider::with_observer(controller.clone(), move |_, request| {
            entered_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
            Ok(ready(request))
        })
        .unwrap(),
    );
    let inner = CpuSourceFactory::new(Arc::new(crate::resource::Provider::new(controller)));
    // SAFETY: the inner factory allocates only binding metadata and outlives
    // this wrapper and its unique binding.
    let factory = unsafe { SourcePreflightFactory::new(&inner.api, provider.clone()) };
    let binding = Binding::new(&factory.api);
    let sink = SinkState::default();
    let request = request();
    assert_eq!(binding.query(&request, &sink), MEDIA_BUSY);
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    provider.stop_admission().unwrap();
    assert!(!provider.quiesce().unwrap());
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !provider.quiesce().unwrap() {
        assert!(Instant::now() < deadline, "worker did not retire");
        std::thread::yield_now();
    }
    assert_ne!(binding.query(&request, &sink), STATUS_OK);
    assert_eq!(sink.calls.load(Ordering::SeqCst), 0);
    assert_eq!(sink.late_calls.load(Ordering::SeqCst), 0);
}
