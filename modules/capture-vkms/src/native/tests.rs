use super::*;
use crate::{
    model::Operation,
    test_support::{FakeBackend, Scripted, SourceScript, settings},
};
use amaterasu_media::{
    CAPTURE_FRAME_CACHED_REUSE, CAPTURE_FRAME_FRESH, CpuFrame, DmaBufFrame, MEDIA_PENDING,
};

struct TestBinding(*const CaptureApiV1_4);

impl TestBinding {
    fn bind(factory: &Factory<FakeBackend>, caller: &[u8], min_minor: u32) -> Result<Self, Status> {
        let mut interface = std::ptr::null();
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: INTERFACE_MAJOR,
            min_minor,
            min_size: CAPTURE_API_V1_4_SIZE,
        };
        // SAFETY: stable factory, caller bytes, query and output slot last through the call.
        let status = unsafe {
            factory.api.bind.unwrap()(
                factory.api.context,
                BorrowedBytes::new(caller),
                &query,
                &mut interface,
            )
        };
        if status == STATUS_OK {
            Ok(Self(interface.cast()))
        } else {
            assert!(interface.is_null());
            Err(status)
        }
    }

    fn api(&self) -> &CaptureApiV1_4 {
        // SAFETY: this wrapper uniquely owns the live boxed binding until Drop.
        unsafe { &*self.0 }
    }

    fn read(&self, stream: u64, sink: &mut Sink) -> Status {
        let api = self.api();
        // SAFETY: the binding and the exclusive sink outlive the synchronous call.
        unsafe {
            (api.read_dmabuf_frame)(
                api.base.context,
                stream,
                DmaBufFrameSink {
                    context: (sink as *mut Sink).cast(),
                    write: record,
                },
            )
        }
    }
}

impl Drop for TestBinding {
    fn drop(&mut self) {
        let api = self.api();
        // SAFETY: releases this binding once after all calls finished.
        unsafe { api.base.release.unwrap()(api.base.context) };
    }
}

struct Sink {
    status: Status,
    deliveries: Vec<(DmaBufFrame, u32, bool)>,
    script: Arc<SourceScript>,
}

/// Records each delivery and whether its descriptors were still open.
unsafe extern "C" fn record(
    context: *mut c_void,
    frame: *const DmaBufFrame,
    freshness: u32,
) -> Status {
    // SAFETY: the test passes an exclusive Sink and a frame valid for this call.
    let (sink, frame) = unsafe { (&mut *context.cast::<Sink>(), *frame) };
    let open = !sink.script.all_lent_descriptors_closed();
    sink.deliveries.push((frame, freshness, open));
    sink.status
}

fn setup() -> (Box<Factory<FakeBackend>>, Arc<SourceScript>, u64) {
    let backend = FakeBackend::new();
    let script = backend.script.clone();
    let mut controller = Controller::new(backend, settings());
    let id = controller
        .handle(
            "amaterasu.video",
            Operation::Open {
                output: "kwin:Virtual-9".into(),
                operation_id: Some("video".into()),
                constant_rate: None,
            },
            0,
        )
        .unwrap()
        .created
        .unwrap();
    let factory = Factory::new(Arc::new(Mutex::new(controller)));
    (factory, script, id)
}

#[test]
fn the_table_advertises_v1_4_and_rejects_newer_queries() {
    let (factory, _, _) = setup();
    let binding = TestBinding::bind(&factory, b"amaterasu.video", 4).unwrap();
    let header = binding.api().base.header;
    assert_eq!(header.minor, CAPTURE_DMABUF_INTERFACE_MINOR);
    assert_eq!(header.size, CAPTURE_API_V1_4_SIZE);
    assert_eq!(
        TestBinding::bind(&factory, b"amaterasu.video", 5).err(),
        Some(MEDIA_UNSUPPORTED)
    );
    assert_eq!(
        TestBinding::bind(&factory, b"", 4).err(),
        Some(STATUS_INVALID_ARGUMENT)
    );
}

#[test]
fn cpu_frame_callbacks_are_unsupported() {
    let (factory, _, id) = setup();
    let binding = TestBinding::bind(&factory, b"amaterasu.video", 1).unwrap();
    let api = binding.api();
    unsafe extern "C" fn never(_: *mut c_void, _: *const CpuFrame) -> Status {
        panic!("no CPU frame is delivered")
    }
    // SAFETY: the binding is live and the sink is never called.
    let status = unsafe {
        (api.read_frame)(
            api.base.context,
            id,
            FrameSink {
                context: std::ptr::null_mut(),
                write: never,
            },
        )
    };
    assert_eq!(status, MEDIA_UNSUPPORTED);
}

#[test]
fn geometry_reports_the_owned_output() {
    let (factory, _, id) = setup();
    let binding = TestBinding::bind(&factory, b"amaterasu.video", 1).unwrap();
    let api = binding.api();
    let mut geometry = std::mem::MaybeUninit::<CaptureGeometry>::uninit();
    // SAFETY: the binding is live and `geometry` is a writable aligned slot.
    let status = unsafe { (api.geometry)(api.base.context, id, geometry.as_mut_ptr()) };
    assert_eq!(status, STATUS_OK);
    // SAFETY: STATUS_OK means the provider initialized the slot.
    let geometry = unsafe { geometry.assume_init() };
    assert!(geometry.validate().is_ok());
    assert_eq!(
        (geometry.output.width, geometry.output.height),
        (1920, 1080)
    );

    let other = TestBinding::bind(&factory, b"other.module", 1).unwrap();
    let api = other.api();
    let mut slot = std::mem::MaybeUninit::<CaptureGeometry>::uninit();
    // SAFETY: as above.
    let status = unsafe { (api.geometry)(api.base.context, id, slot.as_mut_ptr()) };
    assert_eq!(status, MEDIA_CLOSED);
}

#[test]
fn dmabuf_reads_lend_descriptors_for_one_callback_with_freshness() {
    let (factory, script, id) = setup();
    let binding = TestBinding::bind(&factory, b"amaterasu.video", 4).unwrap();
    let mut sink = Sink {
        status: STATUS_OK,
        deliveries: Vec::new(),
        script: script.clone(),
    };
    script.push(Scripted::Empty);
    assert_eq!(binding.read(id, &mut sink), MEDIA_PENDING);
    assert!(sink.deliveries.is_empty());

    script.push(Scripted::FencedFrame { sequence: 7 });
    script.push(Scripted::FencedFrame { sequence: 7 });
    script.push(Scripted::FencedFrame { sequence: 8 });
    for _ in 0..3 {
        assert_eq!(binding.read(id, &mut sink), STATUS_OK);
        assert!(script.all_lent_descriptors_closed());
    }
    let freshness: Vec<u32> = sink.deliveries.iter().map(|delivery| delivery.1).collect();
    assert_eq!(
        freshness,
        vec![
            CAPTURE_FRAME_FRESH,
            CAPTURE_FRAME_CACHED_REUSE,
            CAPTURE_FRAME_FRESH
        ]
    );
    assert!(sink.deliveries.iter().all(|delivery| delivery.2));
    assert_eq!(sink.deliveries[2].0.sequence, 8);
}

#[test]
fn a_sink_rejection_is_returned_and_still_closes_descriptors() {
    let (factory, script, id) = setup();
    let binding = TestBinding::bind(&factory, b"amaterasu.video", 4).unwrap();
    let mut sink = Sink {
        status: MEDIA_BUSY,
        deliveries: Vec::new(),
        script: script.clone(),
    };
    script.push(Scripted::FencedFrame { sequence: 3 });
    assert_eq!(binding.read(id, &mut sink), MEDIA_BUSY);
    assert!(script.all_lent_descriptors_closed());
}

#[test]
fn other_callers_and_closed_streams_cannot_read() {
    let (factory, script, id) = setup();
    let mut sink = Sink {
        status: STATUS_OK,
        deliveries: Vec::new(),
        script,
    };
    let other = TestBinding::bind(&factory, b"other.module", 4).unwrap();
    assert_eq!(other.read(id, &mut sink), MEDIA_CLOSED);

    let owner = TestBinding::bind(&factory, b"amaterasu.video", 4).unwrap();
    factory
        .controller
        .lock()
        .unwrap()
        .close("amaterasu.video", id)
        .unwrap();
    assert_eq!(owner.read(id, &mut sink), MEDIA_CLOSED);
    assert!(sink.deliveries.is_empty());
}

#[test]
fn a_busy_controller_is_media_busy() {
    let (factory, script, id) = setup();
    let binding = TestBinding::bind(&factory, b"amaterasu.video", 4).unwrap();
    let mut sink = Sink {
        status: STATUS_OK,
        deliveries: Vec::new(),
        script,
    };
    let _held = factory.controller.lock().unwrap();
    assert_eq!(binding.read(id, &mut sink), MEDIA_BUSY);
}
