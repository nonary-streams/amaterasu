use super::*;
use crate::model::{Operation, ResourceCaptureClock};
use amaterasu_media::{CaptureApi, MEDIA_CLOSED, MEDIA_PENDING};

struct OwnedBinding(*const CaptureApiV1_8);

impl OwnedBinding {
    fn new(factory: &Factory, caller: &str) -> Self {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: 1,
            min_minor: 8,
            min_size: size_of::<CaptureApiV1_8>() as u32,
        };
        let mut interface = std::ptr::null();
        // SAFETY: factory, query, caller and result slot live throughout bind.
        assert_eq!(
            unsafe {
                factory.api.bind.unwrap()(
                    factory.api.context,
                    BorrowedBytes::new(caller.as_bytes()),
                    &query,
                    &mut interface,
                )
            },
            STATUS_OK
        );
        Self(interface.cast())
    }

    fn api(&self) -> &CaptureApiV1_8 {
        // SAFETY: this wrapper uniquely owns the binding until Drop.
        unsafe { &*self.0 }
    }
}

impl Drop for OwnedBinding {
    fn drop(&mut self) {
        let base = &self.api().base.base.base;
        // SAFETY: the wrapper releases its unique binding after callbacks finish.
        unsafe { base.release.unwrap()(base.context) };
    }
}

fn setup() -> (SharedController, Box<Factory>, u64) {
    let controller = Arc::new(Mutex::new(Controller::new(true)));
    let (reply, _) = controller
        .lock()
        .unwrap()
        .handle(
            "video",
            Operation::Open {
                output: "black:64x48@60:nv12".into(),
                operation_id: Some("native".into()),
                resource_capture_clock: Some(ResourceCaptureClock {
                    domain: "host_monotonic".into(),
                    epoch_id: "11111111111111111111111111111111".into(),
                }),
                resource_callback_contract: Some("cross_service_safe".into()),
            },
            host_monotonic_micros(),
            &mut || Ok(()),
        )
        .unwrap();
    let factory = Factory::new(controller.clone());
    (controller, factory, reply["stream"].as_u64().unwrap())
}

#[derive(Default)]
struct Captured {
    status: Status,
    frames: Vec<(u64, u64, Vec<u8>)>,
    resource_calls: usize,
    controller: Option<SharedController>,
}

unsafe extern "C" fn copy_frame(context: *mut c_void, frame: *const CpuFrame) -> Status {
    // SAFETY: the test sink and borrowed frame are retained through this callback.
    let sink = unsafe { &mut *context.cast::<Captured>() };
    let frame = unsafe { &*frame };
    assert_eq!(frame.validate(), Ok(()));
    assert_eq!(
        (frame.width, frame.height, frame.format, frame.stride),
        (64, 48, PIXEL_NV12, 64)
    );
    if let Some(controller) = &sink.controller {
        assert!(
            controller.try_lock().is_ok(),
            "foreign sink must run outside provider lock"
        );
    }
    // SAFETY: provider guarantees immutable initialized bytes through the callback.
    sink.frames.push((
        frame.sequence,
        frame.timestamp_us,
        unsafe { frame.bytes() }.unwrap().to_vec(),
    ));
    sink.status
}

unsafe extern "C" fn inspect_resource(
    context: *mut c_void,
    resource: *const CaptureResourceFrame,
) -> Status {
    // SAFETY: the test owns the sink and the provider lends a valid resource.
    let state = unsafe { &mut *context.cast::<Captured>() };
    let resource = unsafe { &*resource };
    state.resource_calls += 1;
    assert_eq!(resource.clock.domain, FRAME_CLOCK_HOST_MONOTONIC);
    assert_eq!(resource.clock.epoch_id, [0x11; 16]);
    assert_eq!(resource.offer_id, 1);
    assert_eq!(resource.resource.kind, FRAME_RESOURCE_CPU);
    // SAFETY: this negotiated CPU schema points to a live CpuFrame.
    let frame = unsafe { &*resource.resource.data.cast::<CpuFrame>() };
    assert_eq!(frame.timestamp_us, resource.timestamp_us);
    assert_eq!(frame.sequence, resource.sequence);
    // SAFETY: borrowed-only retain callback is valid during this resource callback.
    assert_eq!(
        unsafe { (resource.retain)(resource.context) },
        MEDIA_UNSUPPORTED
    );
    // SAFETY: the same test-owned sink and frame remain valid for nested synchronous copying.
    unsafe { copy_frame(context, frame) }
}

#[derive(Default)]
struct Offers {
    calls: usize,
}

unsafe extern "C" fn inspect_offer(
    context: *mut c_void,
    offer: *const CaptureResourceOffer,
) -> Status {
    // SAFETY: test owns this sink; provider borrows the initialized offer.
    let state = unsafe { &mut *context.cast::<Offers>() };
    let offer = unsafe { &*offer };
    state.calls += 1;
    assert_eq!(offer.ownership, FRAME_OWNERSHIP_BORROWED);
    assert_eq!(offer.accessibility, FRAME_ACCESS_CPU);
    // SAFETY: negotiated CPU capability schema points at initialized capabilities.
    let caps = unsafe { &*offer.resource.data.cast::<CpuResourceCapabilities>() };
    assert_eq!(
        (
            caps.format,
            caps.min_width,
            caps.max_width,
            caps.min_height,
            caps.max_height
        ),
        (PIXEL_NV12, 64, 64, 48, 48)
    );
    STATUS_OK
}

#[test]
fn unchanged_cpu_consumer_reads_black_through_legacy_prefix_and_owner_scope() {
    let (controller, factory, id) = setup();
    let owner = OwnedBinding::new(&factory, "video");
    let other = OwnedBinding::new(&factory, "foreign");
    let mut sink = Captured {
        status: MEDIA_BUSY,
        controller: Some(controller.clone()),
        ..Default::default()
    };
    let callback = FrameSink {
        context: (&mut sink as *mut Captured).cast(),
        write: copy_frame,
    };
    // SAFETY: CaptureApi is the stable ABI prefix of the retained 1.8 binding.
    let legacy = unsafe { &*owner.0.cast::<CaptureApi>() };
    let foreign = unsafe { &*other.0.cast::<CaptureApi>() };
    // SAFETY: both bindings and the synchronous test sink remain live.
    assert_eq!(
        unsafe { (foreign.read_frame)(foreign.base.context, id, callback) },
        MEDIA_CLOSED
    );
    assert!(sink.frames.is_empty());
    assert_eq!(
        unsafe { (legacy.read_frame)(legacy.base.context, id, callback) },
        MEDIA_BUSY
    );
    sink.status = STATUS_OK;
    assert_eq!(
        unsafe { (legacy.read_frame)(legacy.base.context, id, callback) },
        STATUS_OK
    );
    assert_eq!(sink.frames[0], sink.frames[1]);
    assert!(sink.frames[0].2[..64 * 48].iter().all(|byte| *byte == 16));
    assert!(sink.frames[0].2[64 * 48..].iter().all(|byte| *byte == 128));
    let last = unsafe { (legacy.read_frame)(legacy.base.context, id, callback) };
    assert!(matches!(last, MEDIA_PENDING | STATUS_OK));
    controller
        .lock()
        .unwrap()
        .close("video", id, host_monotonic_micros())
        .unwrap();
    assert_eq!(
        unsafe { (legacy.read_frame)(legacy.base.context, id, callback) },
        MEDIA_CLOSED
    );
}

#[test]
fn generic_cpu_offer_requires_exact_stream_generation_then_preserves_clock_and_pixels() {
    let (_controller, factory, id) = setup();
    let owner = OwnedBinding::new(&factory, "video");
    let api = owner.api();
    let context = api.base.base.base.context;
    let mut frames = Captured::default();
    let sink = CaptureResourceFrameSink {
        context: (&mut frames as *mut Captured).cast(),
        write: inspect_resource,
    };
    // SAFETY: retained binding and synchronous resource sink stay live.
    assert_eq!(
        unsafe { (api.read_resource_frame)(context, id, sink) },
        MEDIA_UNSUPPORTED
    );
    let mut offers = Offers::default();
    let mut generation = 0;
    assert_eq!(
        unsafe {
            (api.enumerate_resources)(
                context,
                id,
                CaptureResourceOfferSink {
                    context: (&mut offers as *mut Offers).cast(),
                    write: inspect_offer,
                },
                &mut generation,
            )
        },
        STATUS_OK
    );
    assert_eq!(generation, id);
    assert_eq!(offers.calls, 1);
    assert_eq!(
        unsafe { (api.select_resource)(context, id, generation + 1, 1) },
        MEDIA_UNSUPPORTED
    );
    assert_eq!(
        unsafe { (api.select_resource)(context, id, generation, 1) },
        STATUS_OK
    );
    assert_eq!(
        unsafe { (api.read_resource_frame)(context, id, sink) },
        STATUS_OK
    );
    assert_eq!(frames.resource_calls, 1);
    assert_eq!(frames.frames.len(), 1);
}

#[test]
fn malformed_native_query_is_rejected_without_allocating_a_binding() {
    let (controller, factory, _) = setup();
    let count = Arc::strong_count(&controller);
    for (major, minor, size) in [
        (2, 0, 1),
        (1, 9, 1),
        (1, 8, size_of::<CaptureApiV1_8>() as u32 + 1),
    ] {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: major,
            min_minor: minor,
            min_size: size,
        };
        let mut interface = std::ptr::dangling();
        // SAFETY: all query and output allocations are aligned and test-owned.
        assert_eq!(
            unsafe {
                factory.api.bind.unwrap()(
                    factory.api.context,
                    BorrowedBytes::new(b"video"),
                    &query,
                    &mut interface,
                )
            },
            MEDIA_UNSUPPORTED
        );
        assert!(interface.is_null());
        assert_eq!(Arc::strong_count(&controller), count);
    }
}
