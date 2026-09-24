use super::*;
use crate::backend::{Backend, CaptureSession};
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
    drops: Arc<AtomicUsize>,
}

struct FakeSession {
    events: Arc<Mutex<VecDeque<CaptureResult<Option<Frame>>>>>,
    geometries: Arc<Mutex<VecDeque<CaptureResult<Option<CaptureGeometry>>>>>,
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
    let drops = Arc::new(AtomicUsize::new(0));
    let backend = FakeBackend {
        events: Arc::new(Mutex::new(VecDeque::new())),
        geometries: Arc::new(Mutex::new(geometries)),
        drops: drops.clone(),
    };
    (
        Arc::new(Mutex::new(Controller::new(backend, enabled))),
        drops,
    )
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
        sequence: 1,
        bytes: Arc::new(vec![1, 2, 3, 4]),
    }))])));
    let drops = Arc::new(AtomicUsize::new(0));
    let controller = Arc::new(Mutex::new(Controller::new(
        FakeBackend {
            events,
            geometries: Arc::new(Mutex::new(VecDeque::new())),
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
        drops,
    };
    let mut controller = Controller::new(backend, true);
    let stream = open_constant_rate(&mut controller, "owner");

    assert!(controller.read("owner", stream, 0).unwrap().is_none());
    let first = controller.read("owner", stream, 0).unwrap().unwrap();
    assert_eq!(first.sequence, 1);
    let second = controller.read("owner", stream, 0).unwrap().unwrap();
    let third = controller.read("owner", stream, 0).unwrap().unwrap();
    assert!(first.timestamp_us < second.timestamp_us);
    assert!(second.timestamp_us < third.timestamp_us);
    assert_eq!((first.sequence, second.sequence, third.sequence), (1, 2, 3));
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
fn default_wgc_mode_does_not_repeat_the_last_frame() {
    let events = VecDeque::from([Ok(Some(frame(7, 4, &[1, 2, 3, 4, 5, 6, 7, 8]))), Ok(None)]);
    let backend = FakeBackend {
        events: Arc::new(Mutex::new(events)),
        geometries: Arc::new(Mutex::new(VecDeque::new())),
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
