use super::*;
use crate::backend::{Backend, CaptureSession, TouchSurface};
use crate::model::{CaptureResult, Frame, Output};
use amaterasu_media::*;
use amaterasu_module_sdk::abi::*;
use serde_json::json;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

struct FakeBackend {
    events: Arc<Mutex<VecDeque<CaptureResult<Option<Frame>>>>>,
    geometries: Arc<Mutex<VecDeque<CaptureResult<Option<CaptureGeometry>>>>>,
    touch_surfaces: Arc<Mutex<VecDeque<CaptureResult<Option<TouchSurface>>>>>,
    drops: Arc<AtomicUsize>,
}

struct FakeSession {
    events: Arc<Mutex<VecDeque<CaptureResult<Option<Frame>>>>>,
    geometries: Arc<Mutex<VecDeque<CaptureResult<Option<CaptureGeometry>>>>>,
    touch_surfaces: Arc<Mutex<VecDeque<CaptureResult<Option<TouchSurface>>>>>,
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
        Ok(Box::new(FakeSession {
            events: self.events.clone(),
            geometries: self.geometries.clone(),
            touch_surfaces: self.touch_surfaces.clone(),
            drops: self.drops.clone(),
        }))
    }
}

impl CaptureSession for FakeSession {
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        self.events.lock().unwrap().pop_front().unwrap_or(Ok(None))
    }

    fn geometry(&mut self) -> CaptureResult<Option<CaptureGeometry>> {
        self.geometries
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(None))
    }

    fn touch_surface(&mut self) -> CaptureResult<Option<TouchSurface>> {
        self.touch_surfaces
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(None))
    }
}

impl Drop for FakeSession {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn setup(enabled: bool) -> (Arc<Mutex<Controller<FakeBackend>>>, Arc<AtomicUsize>) {
    setup_with_geometry(enabled, VecDeque::new())
}

fn setup_with_geometry(
    enabled: bool,
    geometries: VecDeque<CaptureResult<Option<CaptureGeometry>>>,
) -> (Arc<Mutex<Controller<FakeBackend>>>, Arc<AtomicUsize>) {
    setup_with_surfaces(enabled, geometries, VecDeque::new())
}

fn setup_with_surfaces(
    enabled: bool,
    geometries: VecDeque<CaptureResult<Option<CaptureGeometry>>>,
    touch_surfaces: VecDeque<CaptureResult<Option<TouchSurface>>>,
) -> (Arc<Mutex<Controller<FakeBackend>>>, Arc<AtomicUsize>) {
    let drops = Arc::new(AtomicUsize::new(0));
    let backend = FakeBackend {
        events: Arc::new(Mutex::new(VecDeque::new())),
        geometries: Arc::new(Mutex::new(geometries)),
        touch_surfaces: Arc::new(Mutex::new(touch_surfaces)),
        drops: drops.clone(),
    };
    (
        Arc::new(Mutex::new(Controller::new(backend, enabled))),
        drops,
    )
}

#[test]
fn touch_surface_is_published_only_when_provider_proves_valid_mapping() {
    let valid = TouchSurface {
        width: 1920,
        height: 1080,
        logical_width: 1920,
        logical_height: 1080,
    };
    let cases = [
        (Ok(Some(valid)), true, None),
        (Ok(None), false, None),
        (
            Err(CaptureError::new(
                "geometry_unavailable",
                "topology unavailable",
            )),
            false,
            None,
        ),
        (
            Ok(Some(TouchSurface {
                logical_width: 0,
                ..valid
            })),
            false,
            Some("geometry_unavailable"),
        ),
    ];
    for (provider, present, error) in cases {
        let (controller, _) =
            setup_with_surfaces(true, VecDeque::new(), VecDeque::from([provider]));
        let mut controller = controller.lock().unwrap();
        let request = || Operation::Open {
            output: "fake".into(),
            operation_id: Some("touch".into()),
            constant_rate: false,
        };
        let reply = controller.handle("owner", request(), &mut || Ok(()));
        if let Some(error) = error {
            assert_eq!(reply["error"]["code"], error);
            assert_eq!(
                controller.handle("owner", Operation::Status, &mut || Ok(()))["streams"]
                    .as_array()
                    .unwrap()
                    .len(),
                0
            );
            continue;
        }
        assert_eq!(reply["touch_surface"].is_object(), present);
        if present {
            assert_eq!(reply["touch_surface"]["logical_width"], 1920);
        }
        assert_eq!(controller.handle("owner", request(), &mut || Ok(())), reply);
        assert_eq!(
            controller.handle(
                "owner",
                Operation::Lookup {
                    operation_id: "touch".into()
                },
                &mut || Ok(())
            ),
            reply
        );
        let status = controller.handle("owner", Operation::Status, &mut || Ok(()));
        assert_eq!(
            status["streams"][0]["touch_surface"],
            reply["touch_surface"]
        );
    }
}

fn open(controller: &mut Controller<FakeBackend>, owner: &str) -> u64 {
    let reply = controller.handle(
        owner,
        Operation::Open {
            operation_id: None,
            output: "fake-output".into(),
            constant_rate: false,
        },
        &mut || Ok(()),
    );
    assert_eq!(reply["ok"], true);
    reply["stream"].as_u64().unwrap()
}

fn open_constant_rate(controller: &mut Controller<FakeBackend>, owner: &str) -> u64 {
    let reply = controller.handle(
        owner,
        Operation::Open {
            operation_id: None,
            output: "fake-output".into(),
            constant_rate: true,
        },
        &mut || Ok(()),
    );
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["constant_rate"], true);
    reply["stream"].as_u64().unwrap()
}

#[test]
fn constant_rate_open_is_opt_in_and_deserializes_explicitly() {
    let default = serde_json::from_value::<Operation>(json!({
        "action": "open",
        "output": "fake-output"
    }))
    .unwrap();
    assert!(matches!(
        default,
        Operation::Open {
            constant_rate: false,
            ..
        }
    ));

    let enabled = serde_json::from_value::<Operation>(json!({
        "action": "open",
        "output": "fake-output",
        "constant_rate": true
    }))
    .unwrap();
    assert!(matches!(
        enabled,
        Operation::Open {
            constant_rate: true,
            ..
        }
    ));
}

fn frame(timestamp_us: u64, sequence: u64, bytes: &[u8]) -> Frame {
    Frame {
        width: 2,
        height: 1,
        stride: 8,
        timestamp_us,
        capture_pacing_timestamp_us: timestamp_us,
        sequence,
        bytes: Arc::new(bytes.to_vec()),
    }
}

struct TestBinding(*const CaptureApi);

impl TestBinding {
    fn new(factory: &native::Factory<FakeBackend>, caller: &str) -> Self {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: INTERFACE_MAJOR,
            min_minor: CAPTURE_PACING_TIMESTAMP_INTERFACE_MINOR,
            min_size: CAPTURE_API_V1_3_SIZE,
        };
        let mut result = std::ptr::null();
        // SAFETY: test supplies live factory, query, caller and output storage.
        let status = unsafe {
            factory.api.bind.unwrap()(
                factory.api.context,
                BorrowedBytes::new(caller.as_bytes()),
                &query,
                &mut result,
            )
        };
        assert_eq!(status, STATUS_OK);
        assert!(!result.is_null());
        Self(result.cast())
    }

    fn read(&self, id: u64, bytes: &mut Vec<u8>) -> Status {
        // SAFETY: binding owns the returned table; sink context remains alive
        // for the synchronous call and copies bytes before returning.
        unsafe {
            let api = &*self.0;
            (api.read_frame)(
                api.base.context,
                id,
                FrameSink {
                    context: (bytes as *mut Vec<u8>).cast(),
                    write: copy_frame,
                },
            )
        }
    }

    fn read_rejected(&self, id: u64) -> Status {
        // SAFETY: binding owns the table and reject_frame is synchronous.
        unsafe {
            let api = &*self.0;
            (api.read_frame)(
                api.base.context,
                id,
                FrameSink {
                    context: std::ptr::null_mut(),
                    write: reject_frame,
                },
            )
        }
    }

    fn read_freshness(&self, id: u64, values: &mut Vec<u32>) -> Status {
        // SAFETY: capture-wgc v1.3 retains the v1.2 table prefix; the vector
        // lives through the synchronous freshness callback and copies metadata.
        unsafe {
            let api = &*self.0.cast::<CaptureApiV1_2>();
            (api.read_frame_with_freshness)(
                api.base.context,
                id,
                FreshFrameSinkV1_2 {
                    context: (values as *mut Vec<u32>).cast(),
                    write: record_freshness,
                },
            )
        }
    }

    fn read_capture_timestamp(&self, id: u64, values: &mut Vec<(u64, u64, u64, u32)>) -> Status {
        // SAFETY: capture-wgc binds the v1.3 table; the vector and callback
        // context remain live until its synchronous delivery callback returns.
        unsafe {
            let api = &*self.0.cast::<CaptureApiV1_3>();
            (api.read_frame_with_capture_timestamp)(
                api.base.context,
                id,
                FreshFrameSinkV1_3 {
                    context: (values as *mut Vec<(u64, u64, u64, u32)>).cast(),
                    write: record_capture_timestamp,
                },
            )
        }
    }

    fn geometry(&self, id: u64) -> (Status, Option<CaptureGeometry>) {
        let mut geometry = std::mem::MaybeUninit::uninit();
        // SAFETY: binding owns the table and geometry is writable for this call.
        let status = unsafe {
            let api = &*self.0;
            (api.geometry)(api.base.context, id, geometry.as_mut_ptr())
        };
        let geometry = if status == STATUS_OK {
            // SAFETY: a successful callback initializes the entire geometry.
            Some(unsafe { geometry.assume_init() })
        } else {
            None
        };
        (status, geometry)
    }
}

impl Drop for TestBinding {
    fn drop(&mut self) {
        // SAFETY: one release of this owned binding, after every read finished.
        unsafe {
            let api = &*self.0;
            api.base.release.unwrap()(api.base.context);
        }
    }
}

unsafe extern "C" fn copy_frame(context: *mut c_void, frame: *const CpuFrame) -> Status {
    // SAFETY: read_frame guarantees frame and bytes through this callback;
    // TestBinding::read passes an exclusively borrowed Vec as context.
    unsafe {
        let frame = &*frame;
        assert_eq!(frame.stride, frame.width * 4);
        (&mut *context.cast::<Vec<u8>>()).extend_from_slice(frame.bytes().unwrap());
    }
    STATUS_OK
}

unsafe extern "C" fn reject_frame(_: *mut c_void, _: *const CpuFrame) -> Status {
    STATUS_INVALID_ARGUMENT
}

unsafe extern "C" fn record_freshness(
    context: *mut c_void,
    _: *const CpuFrame,
    freshness: u32,
) -> Status {
    // SAFETY: TestBinding::read_freshness passes a live Vec through the call.
    unsafe { (&mut *context.cast::<Vec<u32>>()).push(freshness) };
    STATUS_OK
}

unsafe extern "C" fn record_capture_timestamp(
    context: *mut c_void,
    frame: *const CaptureFrameV1_3,
    freshness: u32,
) -> Status {
    if frame.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: TestBinding passes a live vector for this synchronous callback.
    let frame = unsafe { &*frame };
    unsafe {
        (&mut *context.cast::<Vec<(u64, u64, u64, u32)>>()).push((
            frame.frame.timestamp_us,
            frame.frame.sequence,
            frame.capture_pacing_timestamp_us,
            freshness,
        ));
    }
    STATUS_OK
}

#[test]
fn native_binding_enforces_owner_and_releases_its_state_reference_once() {
    let (controller, drops) = setup(true);
    let stream = open(&mut controller.lock().unwrap(), "owner");
    let factory = native::Factory::new(controller.clone());
    let before = Arc::strong_count(&controller);
    let other = TestBinding::new(&factory, "other");
    let owner = TestBinding::new(&factory, "owner");
    assert_eq!(Arc::strong_count(&controller), before + 2);
    let mut bytes = Vec::new();
    assert_eq!(other.read(stream, &mut bytes), MEDIA_CLOSED);
    assert_eq!(owner.read(stream, &mut bytes), MEDIA_PENDING);
    assert!(bytes.is_empty());
    drop(other);
    drop(owner);
    assert_eq!(Arc::strong_count(&controller), before);
    controller.lock().unwrap().close("owner", stream).unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn disabled_and_cancelled_opens_do_not_leave_resources() {
    let (controller, drops) = setup(false);
    let result = controller.lock().unwrap().handle(
        "owner",
        Operation::Open {
            operation_id: None,
            output: "fake".into(),
            constant_rate: false,
        },
        &mut || Ok(()),
    );
    assert_eq!(result["error"]["code"], "capture_disabled");
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let (controller, drops) = setup(true);
    let mut calls = 0;
    let result = controller.lock().unwrap().handle(
        "owner",
        Operation::Open {
            operation_id: None,
            output: "fake".into(),
            constant_rate: false,
        },
        &mut || {
            calls += 1;
            if calls == 2 {
                Err(CaptureError::new("cancelled", "expired while opening"))
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(result["error"]["code"], "cancelled");
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn stream_capacity_owner_close_and_shutdown_are_enforced() {
    let (controller, drops) = setup(true);
    let mut controller = controller.lock().unwrap();
    let first = open(&mut controller, "owner");
    for _ in 1..model::MAX_STREAMS {
        open(&mut controller, "owner");
    }
    assert_eq!(
        controller.handle(
            "owner",
            Operation::Open {
                operation_id: None,
                output: "fake".into(),
                constant_rate: false,
            },
            &mut || Ok(())
        )["error"]["code"],
        "resource_exhausted"
    );
    assert!(controller.close("other", first).is_err());
    assert!(controller.read("owner", first, 17).is_err());
    controller.shutdown();
    assert_eq!(drops.load(Ordering::SeqCst), model::MAX_STREAMS);
    assert!(controller.read("owner", first, 0).is_err());
}

#[test]
fn row_pitch_copy_excludes_padding_and_checks_bounds() {
    let pixels = [1, 2, 3, 4, 99, 99, 99, 99, 5, 6, 7, 8];
    // SAFETY: both four-byte rows exist at eight-byte spacing in pixels.
    let tight = unsafe { model::copy_bgra_rows(1, 2, 8, pixels.as_ptr()) }.unwrap();
    assert_eq!(tight, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(model::mapped_length(2, 3, 4).is_err());
    assert!(model::frame_layout(0, 1).is_err());
    assert!(model::frame_layout(u32::MAX, 1).is_err());
    assert!(model::frame_layout(8192, 8192).is_err());
}

#[test]
fn native_callback_delivers_pixels_then_reports_pending_and_device_loss() {
    let events = Arc::new(Mutex::new(VecDeque::from([
        Ok(Some(Frame {
            width: 2,
            height: 1,
            stride: 8,
            timestamp_us: 1,
            capture_pacing_timestamp_us: 17,
            sequence: 1,
            bytes: Arc::new(vec![1, 2, 3, 4, 5, 6, 7, 8]),
        })),
        Ok(None),
        Err(CaptureError::new("access_lost", "test desktop switch")),
    ])));
    let drops = Arc::new(AtomicUsize::new(0));
    let controller = Arc::new(Mutex::new(Controller::new(
        FakeBackend {
            events,
            geometries: Arc::new(Mutex::new(VecDeque::new())),
            touch_surfaces: Arc::new(Mutex::new(VecDeque::new())),
            drops: drops.clone(),
        },
        true,
    )));
    let stream = open(&mut controller.lock().unwrap(), "owner");
    let factory = native::Factory::new(controller.clone());
    let binding = TestBinding::new(&factory, "owner");
    let mut bytes = Vec::new();
    {
        let _locked = controller.lock().unwrap();
        assert_eq!(binding.read(stream, &mut bytes), MEDIA_BUSY);
    }
    assert_eq!(binding.read(stream, &mut bytes), STATUS_OK);
    assert_eq!(bytes, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(binding.read(stream, &mut bytes), MEDIA_PENDING);
    assert_eq!(binding.read(stream, &mut bytes), MEDIA_CLOSED);
    assert_eq!(binding.read(stream, &mut bytes), MEDIA_CLOSED);
    assert_eq!(bytes.len(), 8);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn native_geometry_is_owner_scoped_and_change_closes_the_stream() {
    let initial = CaptureGeometry::new(
        DesktopRect::new(-1920, 0, 1920, 1080).unwrap(),
        DesktopRect::new(-1920, 0, 4480, 1440).unwrap(),
    )
    .unwrap();
    let changed = CaptureGeometry::new(
        DesktopRect::new(-1600, 0, 1600, 900).unwrap(),
        DesktopRect::new(-1600, 0, 4160, 1440).unwrap(),
    )
    .unwrap();
    let (controller, _) = setup_with_geometry(
        true,
        VecDeque::from([Ok(Some(initial)), Ok(Some(initial)), Ok(Some(changed))]),
    );
    let id = open(&mut controller.lock().unwrap(), "owner");
    let factory = native::Factory::new(controller);
    let owner = TestBinding::new(&factory, "owner");
    let foreign = TestBinding::new(&factory, "foreign");
    assert_eq!(foreign.geometry(id), (MEDIA_CLOSED, None));
    assert_eq!(owner.geometry(id), (STATUS_OK, Some(initial)));
    assert_eq!(owner.geometry(id), (MEDIA_CLOSED, None));
    assert_eq!(owner.geometry(id), (MEDIA_CLOSED, None));
}

#[test]
fn operation_lookup_recovers_owned_stream_without_marking_replays_as_new() {
    let (controller, drops) = setup(true);
    let mut controller = controller.lock().unwrap();
    let request = || Operation::Open {
        output: "fake".into(),
        operation_id: Some("start-1".into()),
        constant_rate: false,
    };
    let opened = controller.handle("owner", request(), &mut || Ok(()));
    let id = opened["stream"].as_u64().unwrap();
    assert_eq!(controller.take_opened_stream(), Some(id));
    assert_eq!(
        controller.handle("owner", request(), &mut || Ok(())),
        opened
    );
    // A lost reply to an idempotent replay must not close the existing stream.
    assert_eq!(controller.take_opened_stream(), None);
    let lookup = || Operation::Lookup {
        operation_id: "start-1".into(),
    };
    assert_eq!(controller.handle("owner", lookup(), &mut || Ok(())), opened);
    assert_eq!(controller.take_opened_stream(), None);
    assert_eq!(
        controller.handle("other", lookup(), &mut || Ok(()))["error"]["code"],
        "not_found"
    );
    let conflict = Operation::Open {
        output: "fake".into(),
        operation_id: Some("start-1".into()),
        constant_rate: true,
    };
    assert_eq!(
        controller.handle("owner", conflict, &mut || Ok(()))["error"]["code"],
        "operation_conflict"
    );
    controller.close("owner", id).unwrap();
    assert_eq!(
        controller.handle("owner", lookup(), &mut || Ok(()))["error"]["code"],
        "not_found"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn native_factory_rejects_incompatible_table_without_allocating_a_binding() {
    let (controller, _) = setup(true);
    let factory = native::Factory::new(controller.clone());
    let before = Arc::strong_count(&controller);
    for (major, minor, size) in [
        (
            INTERFACE_MAJOR + 1,
            CAPTURE_FRESHNESS_INTERFACE_MINOR,
            CAPTURE_API_V1_2_SIZE,
        ),
        (
            INTERFACE_MAJOR,
            CAPTURE_PACING_TIMESTAMP_INTERFACE_MINOR,
            CAPTURE_API_V1_3_SIZE + 1,
        ),
        (
            INTERFACE_MAJOR,
            CAPTURE_PACING_TIMESTAMP_INTERFACE_MINOR + 1,
            CAPTURE_API_V1_3_SIZE,
        ),
    ] {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: major,
            min_minor: minor,
            min_size: size,
        };
        let mut result = std::ptr::null();
        // SAFETY: all pointers refer to live test-owned values for this call.
        let status = unsafe {
            factory.api.bind.unwrap()(
                factory.api.context,
                BorrowedBytes::new(b"owner"),
                &query,
                &mut result,
            )
        };
        assert_eq!(status, MEDIA_UNSUPPORTED);
        assert!(result.is_null());
        assert_eq!(Arc::strong_count(&controller), before);
    }
}

#[test]
fn native_factory_accepts_v1_0_through_v1_3_minimum_queries() {
    let (controller, _) = setup(true);
    let factory = native::Factory::new(controller.clone());
    let expected_reference_count = Arc::strong_count(&controller);

    for (minor, size) in [
        (0, CAPTURE_API_V1_0_SIZE),
        (CAPTURE_INTERFACE_MINOR, CAPTURE_API_V1_1_SIZE),
        (CAPTURE_FRESHNESS_INTERFACE_MINOR, CAPTURE_API_V1_2_SIZE),
        (
            CAPTURE_PACING_TIMESTAMP_INTERFACE_MINOR,
            CAPTURE_API_V1_3_SIZE,
        ),
    ] {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: INTERFACE_MAJOR,
            min_minor: minor,
            min_size: size,
        };
        let mut result = std::ptr::null();
        // SAFETY: factory, caller, query, and result storage remain live through bind.
        let status = unsafe {
            factory.api.bind.unwrap()(
                factory.api.context,
                BorrowedBytes::new(b"owner"),
                &query,
                &mut result,
            )
        };
        assert_eq!(status, STATUS_OK);
        assert!(!result.is_null());
        // SAFETY: successful bind transfers one live v1.3 table, released exactly once here.
        unsafe {
            let api = &*result;
            assert_eq!(api.header.minor, CAPTURE_PACING_TIMESTAMP_INTERFACE_MINOR);
            assert_eq!(api.header.size, CAPTURE_API_V1_3_SIZE);
            (api.release.expect("owned test binding"))(api.context);
        }
        assert_eq!(Arc::strong_count(&controller), expected_reference_count);
    }
}

#[test]
fn identical_operation_ids_are_scoped_to_each_owner() {
    let (controller, _) = setup(true);
    let mut controller = controller.lock().unwrap();
    let request = || Operation::Open {
        output: "fake-output".into(),
        operation_id: Some("shared-operation-name".into()),
        constant_rate: false,
    };
    let first = controller.handle("first", request(), &mut || Ok(()));
    let second = controller.handle("second", request(), &mut || Ok(()));
    assert_ne!(first["stream"], second["stream"]);
    for (caller, expected) in [("first", &first), ("second", &second)] {
        let lookup = controller.handle(
            caller,
            Operation::Lookup {
                operation_id: "shared-operation-name".into(),
            },
            &mut || Ok(()),
        );
        assert_eq!(&lookup, expected);
    }
    controller
        .close("first", first["stream"].as_u64().unwrap())
        .unwrap();
    let lookup = controller.handle(
        "second",
        Operation::Lookup {
            operation_id: "shared-operation-name".into(),
        },
        &mut || Ok(()),
    );
    assert_eq!(lookup, second);
}

struct ClosingSink {
    controller: Arc<Mutex<Controller<FakeBackend>>>,
    stream: u64,
    observed: Vec<u8>,
}

unsafe extern "C" fn close_during_frame(context: *mut c_void, frame: *const CpuFrame) -> Status {
    // SAFETY: the test lends ClosingSink exclusively through the synchronous
    // callback, and the provider keeps frame storage alive until return.
    let sink = unsafe { &mut *context.cast::<ClosingSink>() };
    let Ok(mut controller) = sink.controller.try_lock() else {
        return MEDIA_BUSY;
    };
    if controller.close("owner", sink.stream).is_err() {
        return MEDIA_CLOSED;
    }
    drop(controller);
    // SAFETY: closing the native stream must not invalidate this callback's
    // separately owned frame; it remains borrowed until the callback returns.
    let bytes = match unsafe { (&*frame).bytes() } {
        Ok(bytes) => bytes,
        Err(status) => return status,
    };
    sink.observed.extend_from_slice(bytes);
    STATUS_OK
}

#[test]
fn callback_can_close_stream_without_invalidating_borrowed_pixels() {
    let events = Arc::new(Mutex::new(VecDeque::from([Ok(Some(Frame {
        width: 1,
        height: 1,
        stride: 4,
        timestamp_us: 1,
        capture_pacing_timestamp_us: 23,
        sequence: 1,
        bytes: Arc::new(vec![1, 2, 3, 4]),
    }))])));
    let drops = Arc::new(AtomicUsize::new(0));
    let controller = Arc::new(Mutex::new(Controller::new(
        FakeBackend {
            events,
            geometries: Arc::new(Mutex::new(VecDeque::new())),
            touch_surfaces: Arc::new(Mutex::new(VecDeque::new())),
            drops: drops.clone(),
        },
        true,
    )));
    let stream = open(&mut controller.lock().unwrap(), "owner");
    let factory = native::Factory::new(controller.clone());
    let binding = TestBinding::new(&factory, "owner");
    let mut sink = ClosingSink {
        controller,
        stream,
        observed: Vec::new(),
    };
    // SAFETY: the test owns binding and callback context until read returns.
    let result = unsafe {
        let api = &*binding.0;
        (api.read_frame)(
            api.base.context,
            stream,
            FrameSink {
                context: (&mut sink as *mut ClosingSink).cast(),
                write: close_during_frame,
            },
        )
    };
    assert_eq!(result, STATUS_OK);
    assert_eq!(sink.observed, [1, 2, 3, 4]);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(binding.read(stream, &mut Vec::new()), MEDIA_CLOSED);
}

#[test]
fn rejected_native_delivery_clears_the_constant_rate_cache() {
    let events = Arc::new(Mutex::new(VecDeque::from([
        Ok(Some(frame(1, 1, &[1, 2, 3, 4, 5, 6, 7, 8]))),
        Ok(None),
    ])));
    let controller = Arc::new(Mutex::new(Controller::new(
        FakeBackend {
            events,
            geometries: Arc::new(Mutex::new(VecDeque::new())),
            touch_surfaces: Arc::new(Mutex::new(VecDeque::new())),
            drops: Arc::new(AtomicUsize::new(0)),
        },
        true,
    )));
    let stream = open_constant_rate(&mut controller.lock().unwrap(), "owner");
    let factory = native::Factory::new(controller);
    let binding = TestBinding::new(&factory, "owner");

    assert_eq!(binding.read_rejected(stream), STATUS_INVALID_ARGUMENT);
    assert_eq!(binding.read(stream, &mut Vec::new()), MEDIA_PENDING);
}

#[test]
fn constant_rate_repeats_owned_pixels_with_monotonic_metadata_and_stops_after_error() {
    let events = VecDeque::from([
        Ok(None),
        Ok(Some(frame(7, 4, &[1, 2, 3, 4, 5, 6, 7, 8]))),
        Ok(None),
        Ok(None),
        Err(CaptureError::new("backend_error", "temporary read failure")),
        Ok(None),
    ]);
    let drops = Arc::new(AtomicUsize::new(0));
    let backend = FakeBackend {
        events: Arc::new(Mutex::new(events)),
        geometries: Arc::new(Mutex::new(VecDeque::new())),
        touch_surfaces: Arc::new(Mutex::new(VecDeque::new())),
        drops,
    };
    let mut controller = Controller::new(backend, true);
    let stream = open_constant_rate(&mut controller, "owner");

    assert!(controller.read("owner", stream, 0).unwrap().is_none());
    let first = controller.read("owner", stream, 0).unwrap().unwrap();
    assert_eq!(first.sequence, 1);
    assert_eq!(first.capture_pacing_timestamp_us, 7);
    let second = controller.read("owner", stream, 0).unwrap().unwrap();
    let third = controller.read("owner", stream, 0).unwrap().unwrap();
    assert!(first.timestamp_us < second.timestamp_us);
    assert!(second.timestamp_us < third.timestamp_us);
    assert_eq!((first.sequence, second.sequence, third.sequence), (1, 2, 3));
    assert_eq!(
        (
            first.capture_pacing_timestamp_us,
            second.capture_pacing_timestamp_us,
            third.capture_pacing_timestamp_us
        ),
        (7, 7, 7)
    );
    assert_eq!(first.bytes.as_ref(), second.bytes.as_ref());
    assert_eq!(second.bytes.as_ref(), third.bytes.as_ref());
    assert!(Arc::ptr_eq(&first.bytes, &second.bytes));

    assert_eq!(
        controller.read("owner", stream, 0).unwrap_err().code,
        "backend_error"
    );
    assert!(controller.read("owner", stream, 0).unwrap().is_none());
    controller.close("owner", stream).unwrap();
    assert_eq!(
        controller.read("owner", stream, 0).unwrap_err().code,
        "not_found"
    );
}

#[test]
fn closed_capture_drops_constant_rate_cache_and_requires_a_new_stream() {
    let events = Arc::new(Mutex::new(VecDeque::from([
        Ok(Some(frame(7, 4, &[1, 2, 3, 4, 5, 6, 7, 8]))),
        Err(CaptureError::new("access_lost", "capture item closed")),
        Ok(None),
    ])));
    let drops = Arc::new(AtomicUsize::new(0));
    let mut controller = Controller::new(
        FakeBackend {
            events: events.clone(),
            geometries: Arc::new(Mutex::new(VecDeque::new())),
            drops: drops.clone(),
        },
        true,
    );
    let stream = open_constant_rate(&mut controller, "owner");

    assert!(controller.read("owner", stream, 0).unwrap().is_some());
    assert_eq!(
        controller.read("owner", stream, 0).unwrap_err().code,
        "access_lost"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        controller.read("owner", stream, 0).unwrap_err().code,
        "not_found"
    );
    assert_eq!(events.lock().unwrap().len(), 1);
}

#[test]
fn v1_3_capture_callback_preserves_acquisition_time_across_timestamp_rewrite_and_reuse() {
    let mut acquired = frame(0, 4, &[1, 2, 3, 4, 5, 6, 7, 8]);
    acquired.capture_pacing_timestamp_us = 123;
    let events = VecDeque::from([Ok(Some(acquired)), Ok(None)]);
    let controller = Arc::new(Mutex::new(Controller::new(
        FakeBackend {
            events: Arc::new(Mutex::new(events)),
            geometries: Arc::new(Mutex::new(VecDeque::new())),
            touch_surfaces: Arc::new(Mutex::new(VecDeque::new())),
            drops: Arc::new(AtomicUsize::new(0)),
        },
        true,
    )));
    let stream = open_constant_rate(&mut controller.lock().unwrap(), "owner");
    let factory = native::Factory::new(controller);
    let binding = TestBinding::new(&factory, "owner");
    // SAFETY: the factory accepted the v1.3 query and the binding retains this table.
    let advertised = unsafe { &*binding.0.cast::<CaptureApiV1_3>() };
    assert_eq!(
        advertised.base.header.minor,
        CAPTURE_PACING_TIMESTAMP_INTERFACE_MINOR
    );
    assert_eq!(advertised.base.header.size, CAPTURE_API_V1_3_SIZE);
    let mut deliveries = Vec::new();

    assert_eq!(
        binding.read_capture_timestamp(stream, &mut deliveries),
        STATUS_OK
    );
    assert_eq!(
        binding.read_capture_timestamp(stream, &mut deliveries),
        STATUS_OK
    );
    assert_eq!(deliveries.len(), 2);
    assert!(deliveries[0].0 > 0);
    assert_eq!([deliveries[0].1, deliveries[1].1], [1, 2]);
    assert!(deliveries[0].0 < deliveries[1].0);
    assert_eq!(deliveries[0].2, 123);
    assert_eq!(deliveries[1].2, 123);
    assert_eq!(
        [deliveries[0].3, deliveries[1].3],
        [CAPTURE_FRAME_FRESH, CAPTURE_FRAME_CACHED_REUSE]
    );
}

#[test]
fn v1_2_prefix_callback_remains_usable_from_the_v1_3_table() {
    let events = VecDeque::from([Ok(Some(frame(7, 4, &[1, 2, 3, 4, 5, 6, 7, 8]))), Ok(None)]);
    let controller = Arc::new(Mutex::new(Controller::new(
        FakeBackend {
            events: Arc::new(Mutex::new(events)),
            geometries: Arc::new(Mutex::new(VecDeque::new())),
            touch_surfaces: Arc::new(Mutex::new(VecDeque::new())),
            drops: Arc::new(AtomicUsize::new(0)),
        },
        true,
    )));
    let stream = open_constant_rate(&mut controller.lock().unwrap(), "owner");
    let factory = native::Factory::new(controller);
    let binding = TestBinding::new(&factory, "owner");
    let mut freshness = Vec::new();

    assert_eq!(binding.read_freshness(stream, &mut freshness), STATUS_OK);
    assert_eq!(binding.read_freshness(stream, &mut freshness), STATUS_OK);
    assert_eq!(freshness, [CAPTURE_FRAME_FRESH, CAPTURE_FRAME_CACHED_REUSE]);
}

#[test]
fn default_wgc_mode_does_not_repeat_the_last_frame() {
    let events = VecDeque::from([Ok(Some(frame(7, 4, &[1, 2, 3, 4, 5, 6, 7, 8]))), Ok(None)]);
    let backend = FakeBackend {
        events: Arc::new(Mutex::new(events)),
        geometries: Arc::new(Mutex::new(VecDeque::new())),
        touch_surfaces: Arc::new(Mutex::new(VecDeque::new())),
        drops: Arc::new(AtomicUsize::new(0)),
    };
    let mut controller = Controller::new(backend, true);
    let stream = open(&mut controller, "owner");

    let first = controller.read("owner", stream, 0).unwrap().unwrap();
    assert_eq!((first.timestamp_us, first.sequence), (7, 4));
    assert!(controller.read("owner", stream, 0).unwrap().is_none());
}

#[cfg(windows)]
#[test]
#[ignore = "requires an attached Windows display and Windows Graphics Capture access"]
fn native_enumeration_and_open_report_the_same_physical_output_rectangle() {
    let backend = crate::backend::NativeBackend::new(None);
    let output = backend
        .enumerate()
        .unwrap()
        .into_iter()
        .find(|output| output.attached)
        .expect("test requires an attached WGC output");
    let mut session = backend.open(&output.id).unwrap();
    let geometry = session
        .geometry()
        .unwrap()
        .expect("verified WGC output geometry");

    assert_eq!(geometry.output.left, output.left);
    assert_eq!(geometry.output.top, output.top);
    assert_eq!(geometry.output.width, output.width);
    assert_eq!(geometry.output.height, output.height);
}
