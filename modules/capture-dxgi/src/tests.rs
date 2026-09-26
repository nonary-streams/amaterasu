use super::*;
use crate::backend::{Backend, CaptureSession, TouchSurface};
use crate::model::{CaptureResult, Frame, Output};
use amaterasu_media::*;
use amaterasu_module_sdk::abi::*;
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
    setup_with_frames_and_geometry(enabled, VecDeque::new(), geometries, touch_surfaces)
}

fn setup_with_frames_and_geometry(
    enabled: bool,
    events: VecDeque<CaptureResult<Option<Frame>>>,
    geometries: VecDeque<CaptureResult<Option<CaptureGeometry>>>,
    touch_surfaces: VecDeque<CaptureResult<Option<TouchSurface>>>,
) -> (Arc<Mutex<Controller<FakeBackend>>>, Arc<AtomicUsize>) {
    let drops = Arc::new(AtomicUsize::new(0));
    let backend = FakeBackend {
        events: Arc::new(Mutex::new(events)),
        geometries: Arc::new(Mutex::new(geometries)),
        touch_surfaces: Arc::new(Mutex::new(touch_surfaces)),
        drops: drops.clone(),
    };
    (
        Arc::new(Mutex::new(Controller::new(backend, enabled))),
        drops,
    )
}

fn sample_geometry(left: i32) -> CaptureGeometry {
    CaptureGeometry::new(
        DesktopRect::new(left, 0, 1920, 1080).unwrap(),
        DesktopRect::new(left, 0, 3840, 1080).unwrap(),
    )
    .unwrap()
}

fn sample_frame() -> Frame {
    Frame {
        width: 1,
        height: 1,
        stride: 4,
        timestamp_us: 1,
        sequence: 1,
        bytes: vec![1, 2, 3, 4],
    }
}

#[test]
fn frame_read_rechecks_published_geometry_before_delivering_pixels() {
    let initial = sample_geometry(-1920);
    let moved = sample_geometry(0);
    for (current, expected_status, expected_bytes, closes) in [
        (Ok(Some(moved)), MEDIA_CLOSED, Vec::new(), true),
        (Ok(None), MEDIA_CLOSED, Vec::new(), true),
        (Ok(Some(initial)), STATUS_OK, sample_frame().bytes, false),
        (
            Err(CaptureError::new("access_lost", "provider lost output")),
            MEDIA_CLOSED,
            Vec::new(),
            true,
        ),
        (
            Err(CaptureError::new(
                "geometry_unavailable",
                "topology unavailable",
            )),
            MEDIA_CLOSED,
            Vec::new(),
            true,
        ),
    ] {
        let (controller, drops) = setup_with_frames_and_geometry(
            true,
            VecDeque::from([Ok(Some(sample_frame()))]),
            VecDeque::from([Ok(Some(initial)), current]),
            VecDeque::new(),
        );
        let stream = open(&mut controller.lock().unwrap(), "owner");
        let factory = native::Factory::new(controller.clone());
        let binding = TestBinding::new(&factory, "owner");
        let mut bytes = Vec::new();
        assert_eq!(binding.read(stream, &mut bytes), expected_status);
        assert_eq!(bytes, expected_bytes);
        assert_eq!(drops.load(Ordering::SeqCst), usize::from(closes));
        if closes {
            assert_eq!(binding.read(stream, &mut bytes), MEDIA_CLOSED);
            let status = controller
                .lock()
                .unwrap()
                .handle("owner", Operation::Status, &mut || Ok(()));
            assert!(status["streams"].as_array().unwrap().is_empty());
        }
    }
}

#[test]
fn frame_read_without_advertised_geometry_keeps_capture_available() {
    let (controller, drops) = setup_with_frames_and_geometry(
        true,
        VecDeque::from([Ok(Some(sample_frame()))]),
        VecDeque::from([Err(CaptureError::new(
            "geometry_unavailable",
            "topology unavailable",
        ))]),
        VecDeque::new(),
    );
    let stream = open(&mut controller.lock().unwrap(), "owner");
    let factory = native::Factory::new(controller);
    let binding = TestBinding::new(&factory, "owner");
    let mut bytes = Vec::new();
    assert_eq!(binding.read(stream, &mut bytes), STATUS_OK);
    assert_eq!(bytes, sample_frame().bytes);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
}

#[test]
fn touch_surface_is_published_only_when_provider_proves_valid_mapping() {
    let geometry = CaptureGeometry::new(
        DesktopRect::new(-1920, 0, 1920, 1080).unwrap(),
        DesktopRect::new(-1920, 0, 3840, 1080).unwrap(),
    )
    .unwrap();
    let moved = CaptureGeometry::new(
        DesktopRect::new(0, 0, 1920, 1080).unwrap(),
        DesktopRect::new(0, 0, 3840, 1080).unwrap(),
    )
    .unwrap();
    let valid = TouchSurface {
        width: 1920,
        height: 1080,
        logical_width: 1920,
        logical_height: 1080,
    };
    let cases = [
        (
            vec![Ok(Some(geometry)), Ok(Some(geometry))],
            Ok(Some(valid)),
            true,
            None,
        ),
        (vec![Ok(Some(geometry))], Ok(None), false, None),
        (vec![Ok(None)], Ok(Some(valid)), false, None),
        (
            vec![Err(CaptureError::new(
                "geometry_unavailable",
                "topology unavailable",
            ))],
            Ok(Some(valid)),
            false,
            None,
        ),
        (
            vec![Ok(Some(geometry))],
            Err(CaptureError::new(
                "geometry_unavailable",
                "topology unavailable",
            )),
            false,
            None,
        ),
        (
            vec![Ok(Some(geometry))],
            Ok(Some(TouchSurface {
                width: 1280,
                ..valid
            })),
            false,
            None,
        ),
        (
            vec![Ok(Some(geometry)), Ok(Some(moved))],
            Ok(Some(valid)),
            false,
            None,
        ),
        (
            vec![Ok(Some(geometry)), Ok(None)],
            Ok(Some(valid)),
            false,
            None,
        ),
        (
            vec![
                Ok(Some(geometry)),
                Err(CaptureError::new(
                    "geometry_unavailable",
                    "topology unavailable",
                )),
            ],
            Ok(Some(valid)),
            false,
            None,
        ),
        (
            vec![Ok(Some(geometry))],
            Ok(Some(TouchSurface {
                logical_width: 0,
                ..valid
            })),
            false,
            Some("geometry_unavailable"),
        ),
    ];
    for (geometries, provider, present, error) in cases {
        let (controller, drops) =
            setup_with_surfaces(true, VecDeque::from(geometries), VecDeque::from([provider]));
        let mut controller = controller.lock().unwrap();
        let request = || Operation::Open {
            output: "fake".into(),
            operation_id: Some("touch".into()),
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
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            continue;
        }
        assert_eq!(reply["ok"], true);
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
        let stream = reply["stream"].as_u64().unwrap();
        assert!(controller.read("owner", stream, 0).unwrap().is_none());
        controller.close("owner", stream).unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}

fn open(controller: &mut Controller<FakeBackend>, owner: &str) -> u64 {
    let reply = controller.handle(
        owner,
        Operation::Open {
            operation_id: None,
            output: "fake-output".into(),
        },
        &mut || Ok(()),
    );
    assert_eq!(reply["ok"], true);
    reply["stream"].as_u64().unwrap()
}

struct TestBinding(*const CaptureApi);

impl TestBinding {
    fn new(factory: &native::Factory<FakeBackend>, caller: &str) -> Self {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: INTERFACE_MAJOR,
            min_minor: CAPTURE_INTERFACE_MINOR,
            min_size: size_of::<CaptureApi>() as u32,
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
                output: "fake".into()
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
            sequence: 1,
            bytes: vec![1, 2, 3, 4, 5, 6, 7, 8],
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
fn advertised_geometry_becoming_unavailable_closes_only_its_owner_stream() {
    let initial = sample_geometry(-1920);
    for unavailable in [
        Ok(None),
        Err(CaptureError::new(
            "geometry_unavailable",
            "topology unavailable",
        )),
    ] {
        let (controller, drops) =
            setup_with_geometry(true, VecDeque::from([Ok(Some(initial)), unavailable]));
        let id = open(&mut controller.lock().unwrap(), "owner");
        let mut controller = controller.lock().unwrap();
        assert_eq!(
            controller.geometry("foreign", id).unwrap_err().code,
            "not_found"
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(
            controller.geometry("owner", id).unwrap_err().code,
            "access_lost"
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            controller.geometry("owner", id).unwrap_err().code,
            "not_found"
        );
        assert_eq!(
            controller.read("owner", id, 0).err().unwrap().code,
            "not_found"
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(
            controller.handle("owner", Operation::Status, &mut || Ok(()))["streams"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn operation_lookup_recovers_owned_stream_without_marking_replays_as_new() {
    let (controller, drops) = setup(true);
    let mut controller = controller.lock().unwrap();
    let request = || Operation::Open {
        output: "fake".into(),
        operation_id: Some("start-1".into()),
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
        output: "different".into(),
        operation_id: Some("start-1".into()),
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
    for (major, size) in [
        (INTERFACE_MAJOR + 1, size_of::<CaptureApi>() as u32),
        (INTERFACE_MAJOR, size_of::<CaptureApi>() as u32 + 1),
    ] {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: major,
            min_minor: 0,
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
fn identical_operation_ids_are_scoped_to_each_owner() {
    let (controller, _) = setup(true);
    let mut controller = controller.lock().unwrap();
    let request = || Operation::Open {
        output: "fake-output".into(),
        operation_id: Some("shared-operation-name".into()),
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
        sequence: 1,
        bytes: vec![1, 2, 3, 4],
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

#[cfg(windows)]
#[test]
#[ignore = "requires an attached Windows display and desktop-duplication access"]
fn native_enumeration_and_open_report_the_same_physical_output_rectangle() {
    let backend = crate::backend::NativeBackend;
    let output = backend
        .enumerate()
        .unwrap()
        .into_iter()
        .find(|output| output.attached && output.rotation == 1)
        .expect("test requires an attached unrotated DXGI output");
    let mut session = backend.open(&output.id).unwrap();
    let geometry = session
        .geometry()
        .unwrap()
        .expect("verified DXGI output geometry");

    assert_eq!(geometry.output.left, output.left);
    assert_eq!(geometry.output.top, output.top);
    assert_eq!(geometry.output.width, output.width);
    assert_eq!(geometry.output.height, output.height);
}
