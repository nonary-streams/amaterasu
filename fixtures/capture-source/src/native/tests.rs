use super::*;
use crate::model::{FRAME_COUNT, OUTPUT, Operation, frame_bytes};
use amaterasu_media::{CAPTURE_INTERFACE_MINOR, CaptureApi};

struct TestBinding(*const CaptureApi);

impl TestBinding {
    fn new(factory: &Factory, caller: &[u8]) -> Self {
        let query = query(
            INTERFACE_MAJOR,
            CAPTURE_INTERFACE_MINOR,
            size_of::<CaptureApi>() as u32,
        );
        let mut interface = std::ptr::null();
        // SAFETY: all values are initialized test-owned allocations lasting through bind.
        let status = unsafe {
            factory.api.bind.unwrap()(
                factory.api.context,
                BorrowedBytes::new(caller),
                &query,
                &mut interface,
            )
        };
        assert_eq!(status, STATUS_OK);
        Self(interface.cast())
    }

    fn api(&self) -> &CaptureApi {
        // SAFETY: this wrapper owns the binding until its unique Drop release.
        unsafe { &*self.0 }
    }

    fn read(&self, stream: u64, sink: &mut CopiedFrames) -> Status {
        let callback = FrameSink {
            context: (sink as *mut CopiedFrames).cast(),
            write: copy_frame,
        };
        let api = self.api();
        // SAFETY: wrapper and synchronous test sink live through the call.
        unsafe { (api.read_frame)(api.base.context, stream, callback) }
    }
}

impl Drop for TestBinding {
    fn drop(&mut self) {
        let api = self.api();
        // SAFETY: this is the unique release, after every wrapper call has ended.
        unsafe { api.base.release.unwrap()(api.base.context) };
    }
}

fn query(major: u32, minor: u32, size: u32) -> NativeInterfaceQuery {
    NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: major,
        min_minor: minor,
        min_size: size,
    }
}

fn setup() -> (SharedController, Box<Factory>, u64) {
    let controller = Arc::new(Mutex::new(Controller::default()));
    let id = controller
        .lock()
        .unwrap()
        .handle(
            "amaterasu.video",
            Operation::Open {
                output: OUTPUT.into(),
                operation_id: Some("capture-operation".into()),
            },
        )
        .unwrap()
        .1
        .unwrap();
    let factory = Factory::new(controller.clone());
    (controller, factory, id)
}

#[derive(Default)]
struct CopiedFrames {
    status: Status,
    frames: Vec<(u64, u64, Vec<u8>)>,
    controller: Option<SharedController>,
}

unsafe extern "C" fn copy_frame(context: *mut c_void, frame: *const CpuFrame) -> Status {
    // SAFETY: read passes a live exclusive test sink and valid borrowed frame.
    let sink = unsafe { &mut *context.cast::<CopiedFrames>() };
    let frame = unsafe { &*frame };
    assert_eq!(frame.validate(), Ok(()));
    assert_eq!(
        (frame.width, frame.height, frame.format, frame.stride),
        (WIDTH, HEIGHT, PIXEL_NV12, WIDTH)
    );
    if let Some(controller) = &sink.controller {
        assert!(
            controller.try_lock().is_ok(),
            "foreign sink runs without controller lock"
        );
    }
    // SAFETY: initialized immutable pixel bytes remain live throughout this callback.
    sink.frames.push((
        frame.sequence,
        frame.timestamp_us,
        unsafe { frame.bytes() }.unwrap().to_vec(),
    ));
    sink.status
}

#[test]
fn native_query_rejects_unknown_version_oversized_table_and_invalid_caller() {
    let (controller, factory, _) = setup();
    let before = Arc::strong_count(&controller);
    let size = size_of::<CaptureApi>() as u32;
    for query in [
        query(INTERFACE_MAJOR + 1, 0, size),
        query(INTERFACE_MAJOR, 9, size),
        query(INTERFACE_MAJOR, 1, size_of::<CaptureApiV1_8>() as u32 + 1),
        NativeInterfaceQuery {
            header: AbiHeader {
                size: 0,
                major: 1,
                minor: 0,
            },
            ..query(1, 1, size)
        },
    ] {
        let mut interface = std::ptr::dangling();
        // SAFETY: test query and output are live, aligned stack allocations.
        assert_eq!(
            unsafe {
                factory.api.bind.unwrap()(
                    factory.api.context,
                    BorrowedBytes::new(b"owner"),
                    &query,
                    &mut interface,
                )
            },
            MEDIA_UNSUPPORTED
        );
        assert!(interface.is_null());
        assert_eq!(Arc::strong_count(&controller), before);
    }
    for caller in [b"".as_slice(), b"\xff".as_slice()] {
        let mut interface = std::ptr::null();
        // SAFETY: test-owned bytes, query and output remain live through bind.
        assert_eq!(
            unsafe {
                factory.api.bind.unwrap()(
                    factory.api.context,
                    BorrowedBytes::new(caller),
                    &query(1, 1, size),
                    &mut interface,
                )
            },
            STATUS_INVALID_ARGUMENT
        );
        assert!(interface.is_null());
    }
    let legacy = query(1, 0, amaterasu_media::CAPTURE_API_V1_0_SIZE);
    let mut interface = std::ptr::null();
    // SAFETY: test values live through bind and successful result has unique release below.
    assert_eq!(
        unsafe {
            factory.api.bind.unwrap()(
                factory.api.context,
                BorrowedBytes::new(b"owner"),
                &legacy,
                &mut interface,
            )
        },
        STATUS_OK
    );
    drop(TestBinding(interface.cast()));
    assert_eq!(Arc::strong_count(&controller), before);
}

#[test]
fn native_ownership_geometry_and_closed_ids_are_enforced() {
    let (controller, factory, id) = setup();
    let owner = TestBinding::new(&factory, b"amaterasu.video");
    let wrong = TestBinding::new(&factory, b"amaterasu.nvhttp");
    let mut sink = CopiedFrames::default();
    assert_eq!(wrong.read(id, &mut sink), MEDIA_CLOSED);
    assert!(sink.frames.is_empty());
    let mut actual = fixture_geometry();
    let api = owner.api();
    // SAFETY: test retains binding and supplies a writable aligned geometry slot.
    assert_eq!(
        unsafe { (api.geometry)(api.base.context, id, &mut actual) },
        STATUS_OK
    );
    assert_eq!(actual, fixture_geometry());
    let wrong_api = wrong.api();
    // SAFETY: wrong binding is live and geometry output is a valid test allocation.
    assert_eq!(
        unsafe { (wrong_api.geometry)(wrong_api.base.context, id, &mut actual) },
        MEDIA_CLOSED
    );
    let locked = controller.lock().unwrap();
    assert_eq!(owner.read(id, &mut sink), MEDIA_BUSY);
    drop(locked);
    controller
        .lock()
        .unwrap()
        .close("amaterasu.video", id)
        .unwrap();
    assert_eq!(owner.read(id, &mut sink), MEDIA_CLOSED);
    // SAFETY: closed IDs still use a live native binding and valid output slot.
    assert_eq!(
        unsafe { (api.geometry)(api.base.context, id, &mut actual) },
        MEDIA_CLOSED
    );
}

#[test]
fn nv12_callbacks_retry_rejections_and_copied_pixels_outlive_the_binding() {
    let (controller, factory, id) = setup();
    let binding = TestBinding::new(&factory, b"amaterasu.video");
    let mut sink = CopiedFrames {
        status: MEDIA_BUSY,
        controller: Some(controller.clone()),
        ..Default::default()
    };
    assert_eq!(binding.read(id, &mut sink), MEDIA_BUSY);
    assert_eq!(controller.lock().unwrap().metrics()["frames"], 0);
    sink.status = STATUS_OK;
    assert_eq!(binding.read(id, &mut sink), STATUS_OK);
    assert_eq!(sink.frames[0], sink.frames[1]);
    assert_eq!(sink.frames[0].0, 0);
    assert_eq!(
        sink.frames[0].2.len(),
        WIDTH as usize * HEIGHT as usize * 3 / 2
    );
    for row in sink.frames[0].2[..WIDTH as usize * HEIGHT as usize]
        .as_chunks::<{ WIDTH as usize }>()
        .0
    {
        assert!(row[..WIDTH as usize / 2].iter().all(|&luma| luma == 220));
        assert!(row[WIDTH as usize / 2..].iter().all(|&luma| luma == 32));
    }
    assert!(
        sink.frames[0].2[WIDTH as usize * HEIGHT as usize..]
            .iter()
            .all(|&chroma| chroma == 128)
    );
    for sequence in 1..FRAME_COUNT {
        assert_eq!(binding.read(id, &mut sink), STATUS_OK);
        let copied = sink.frames.last().unwrap();
        assert_eq!(copied.0, sequence);
        assert_eq!(copied.1, sequence * 1_000_000 / FPS);
        assert_eq!(copied.2, frame_bytes(sequence));
    }
    assert_eq!(binding.read(id, &mut sink), amaterasu_media::MEDIA_PENDING);
    assert_eq!(sink.frames.len(), FRAME_COUNT as usize + 1);
    assert_ne!(sink.frames[1].2, sink.frames[2].2);
    let first_copy = sink.frames[1].2.clone();
    drop(binding);
    controller.lock().unwrap().shutdown();
    drop(factory);
    assert_eq!(sink.frames[1].2, first_copy);
    assert_eq!(controller.lock().unwrap().metrics()["frames"], FRAME_COUNT);
}

struct ReentrantSink<'a> {
    binding: &'a TestBinding,
    controller: SharedController,
    stream: u64,
    nested_status: Status,
}

unsafe extern "C" fn reentrant_sink(context: *mut c_void, _frame: *const CpuFrame) -> Status {
    // SAFETY: the outer test passes a live exclusive sink for this synchronous call.
    let sink = unsafe { &mut *context.cast::<ReentrantSink<'_>>() };
    sink.nested_status = sink.binding.read(sink.stream, &mut CopiedFrames::default());
    sink.controller
        .lock()
        .unwrap()
        .close("amaterasu.video", sink.stream)
        .unwrap();
    STATUS_OK
}

#[test]
fn reentrant_callback_cannot_consume_twice_and_can_close_without_deadlock() {
    let (controller, factory, id) = setup();
    let binding = TestBinding::new(&factory, b"amaterasu.video");
    let mut sink = ReentrantSink {
        binding: &binding,
        controller: controller.clone(),
        stream: id,
        nested_status: STATUS_OK,
    };
    let api = binding.api();
    // SAFETY: binding and nested test sink live through the synchronous callback.
    assert_eq!(
        unsafe {
            (api.read_frame)(
                api.base.context,
                id,
                FrameSink {
                    context: (&mut sink as *mut ReentrantSink<'_>).cast(),
                    write: reentrant_sink,
                },
            )
        },
        STATUS_OK
    );
    assert_eq!(sink.nested_status, MEDIA_BUSY);
    assert_eq!(binding.read(id, &mut CopiedFrames::default()), MEDIA_CLOSED);
    let metrics = controller.lock().unwrap().metrics();
    assert_eq!(metrics["frames"], 1);
    assert_eq!(metrics["closed"], 1);
    assert_eq!(metrics["active"], 0);
}

#[test]
fn binding_copies_borrowed_caller_and_rejected_sink_errors_do_not_advance() {
    let (controller, factory, id) = setup();
    let mut borrowed_caller = b"amaterasu.video".to_vec();
    let binding = TestBinding::new(&factory, &borrowed_caller);
    borrowed_caller.fill(b'x');
    drop(borrowed_caller);
    let mut sink = CopiedFrames {
        status: STATUS_INVALID_ARGUMENT,
        ..Default::default()
    };
    assert_eq!(binding.read(id, &mut sink), STATUS_INVALID_ARGUMENT);
    sink.status = STATUS_OK;
    assert_eq!(binding.read(id, &mut sink), STATUS_OK);
    assert_eq!(sink.frames[0], sink.frames[1]);
    assert_eq!(controller.lock().unwrap().metrics()["frames"], 1);
}

#[derive(Default)]
struct ResourceProbe {
    offer: Option<(u64, u32, u32, u32, u32)>,
    retained: Option<CaptureResourceFrame>,
    status: Status,
    sequences: Vec<u64>,
}

unsafe extern "C" fn inspect_offer(
    context: *mut c_void,
    offer: *const CaptureResourceOffer,
) -> Status {
    // SAFETY: the test passes a live probe and the provider supplies a borrowed offer.
    let probe = unsafe { &mut *context.cast::<ResourceProbe>() };
    let offer = unsafe { &*offer };
    assert_eq!(offer.resource.kind, FRAME_RESOURCE_CPU);
    assert_eq!(offer.resource.version, 1);
    assert_eq!(offer.resource.namespace, [0; 16]);
    assert_eq!(
        offer.resource.size as usize,
        size_of::<CpuResourceCapabilities>()
    );
    assert_eq!(offer.ownership, FRAME_OWNERSHIP_RETAINABLE);
    assert_eq!(offer.accessibility, FRAME_ACCESS_CPU);
    assert_eq!(offer.acquire_sync.kind, FRAME_SYNC_NONE);
    assert_eq!(offer.completion_sync.kind, FRAME_SYNC_NONE);
    // SAFETY: the standard CPU offer provides this exact capability payload during the call.
    let capabilities = unsafe { &*offer.resource.data.cast::<CpuResourceCapabilities>() };
    probe.offer = Some((
        offer.id,
        capabilities.format,
        capabilities.min_width,
        capabilities.max_width,
        capabilities.max_height,
    ));
    probe.status
}

unsafe extern "C" fn inspect_resource(
    context: *mut c_void,
    frame: *const CaptureResourceFrame,
) -> Status {
    // SAFETY: probe and borrowed frame are valid for the synchronous callback.
    let probe = unsafe { &mut *context.cast::<ResourceProbe>() };
    let frame = unsafe { &*frame };
    assert_eq!((frame.offer_generation, frame.offer_id), (1, 1));
    assert_eq!(
        (frame.resource.kind, frame.resource.version),
        (FRAME_RESOURCE_CPU, 1)
    );
    assert_eq!(frame.acquire_sync.kind, FRAME_SYNC_NONE);
    assert_eq!((frame.color.version, frame.color.size), (0, 0));
    assert_eq!((frame.clock.version, frame.clock.size), (0, 0));
    assert_eq!(
        (frame.pacing_clock.version, frame.pacing_clock.size),
        (0, 0)
    );
    // SAFETY: the CPU frame descriptor is valid and immutable during this call.
    let cpu = unsafe { &*frame.resource.data.cast::<CpuFrame>() };
    assert_eq!(cpu.validate(), Ok(()));
    assert_eq!(cpu.sequence, frame.sequence);
    probe.sequences.push(frame.sequence);
    if probe.status == STATUS_OK && probe.retained.is_none() {
        // SAFETY: the provider holds its reference during this callback.
        assert_eq!(unsafe { (frame.retain)(frame.context) }, STATUS_OK);
        probe.retained = Some(*frame);
    }
    probe.status
}

#[test]
fn resource_negotiation_is_owned_and_leases_keep_accepted_pixels_immutable() {
    let (controller, factory, id) = setup();
    let owner = TestBinding::new(&factory, b"amaterasu.video");
    let wrong = TestBinding::new(&factory, b"amaterasu.nvhttp");
    // SAFETY: the provider advertises v1.8 and the binding is live throughout this test.
    let api = unsafe { &*owner.0.cast::<CaptureApiV1_8>() };
    let wrong_api = unsafe { &*wrong.0.cast::<CaptureApiV1_8>() };
    let context = api.base.base.base.context;
    let mut probe = ResourceProbe::default();
    let sink = CaptureResourceFrameSink {
        context: (&mut probe as *mut ResourceProbe).cast(),
        write: inspect_resource,
    };
    let offers = CaptureResourceOfferSink {
        context: (&mut probe as *mut ResourceProbe).cast(),
        write: inspect_offer,
    };
    let mut generation = 0;
    // SAFETY: all bindings, sinks and outputs are live for each synchronous call.
    unsafe {
        assert_eq!(
            (api.read_resource_frame)(context, id, sink),
            MEDIA_UNSUPPORTED
        );
        assert_eq!(
            (wrong_api.enumerate_resources)(
                wrong_api.base.base.base.context,
                id,
                offers,
                &mut generation
            ),
            MEDIA_CLOSED
        );
        assert_eq!(
            (wrong_api.select_resource)(wrong_api.base.base.base.context, id, 1, 1),
            MEDIA_CLOSED
        );
        assert_eq!(
            (api.enumerate_resources)(context, id, offers, &mut generation),
            STATUS_OK
        );
        assert_eq!(generation, 1);
        assert_eq!(probe.offer, Some((1, PIXEL_NV12, WIDTH, WIDTH, HEIGHT)));
        assert_eq!((api.select_resource)(context, id, 2, 1), MEDIA_UNSUPPORTED);
        assert_eq!((api.select_resource)(context, id, 1, 1), STATUS_OK);
        assert_eq!((api.select_resource)(context, id, 1, 99), MEDIA_UNSUPPORTED);
        probe.status = MEDIA_BUSY;
        assert_eq!((api.read_resource_frame)(context, id, sink), MEDIA_BUSY);
        probe.status = STATUS_OK;
        assert_eq!((api.read_resource_frame)(context, id, sink), STATUS_OK);
        assert_eq!((api.read_resource_frame)(context, id, sink), STATUS_OK);
    }
    assert_eq!(probe.sequences, [0, 0, 1]);
    let retained = probe.retained.take().unwrap();
    controller
        .lock()
        .unwrap()
        .close("amaterasu.video", id)
        .unwrap();
    // SAFETY: retained reference pins both the descriptor and immutable frame storage.
    let cpu = unsafe { &*retained.resource.data.cast::<CpuFrame>() };
    assert_eq!(unsafe { cpu.bytes() }.unwrap(), frame_bytes(0));
    // SAFETY: this consumes the successful retain after all reads have completed.
    unsafe { (retained.release)(retained.context, &none_descriptor()) };
    // SAFETY: binding and sink remain live; closed stream cannot deliver another frame.
    assert_eq!(
        unsafe { (api.read_resource_frame)(context, id, sink) },
        MEDIA_CLOSED
    );
    assert_eq!(controller.lock().unwrap().metrics()["frames"], 2);
}
