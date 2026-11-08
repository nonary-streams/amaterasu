use super::*;
use crate::model::{Operation, ResourceCaptureClock};
use amaterasu_media::{CaptureApi, MEDIA_CLOSED, MEDIA_PENDING, PIXEL_NV12};

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
    setup_output("black:64x48@60:nv12")
}

fn setup_output(output: &str) -> (SharedController, Box<Factory>, u64) {
    let controller = Arc::new(Mutex::new(Controller::new(true)));
    let (reply, _) = controller
        .lock()
        .unwrap()
        .handle(
            "video",
            Operation::Open {
                output: output.into(),
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
    format: u32,
}

unsafe extern "C" fn copy_frame(context: *mut c_void, frame: *const CpuFrame) -> Status {
    // SAFETY: the test sink and borrowed frame are retained through this callback.
    let sink = unsafe { &mut *context.cast::<Captured>() };
    let frame = unsafe { &*frame };
    assert_eq!(frame.validate(), Ok(()));
    let expected_format = if sink.format == 0 {
        PIXEL_NV12
    } else {
        sink.format
    };
    let expected_stride = if expected_format == amaterasu_media::PIXEL_P010 {
        128
    } else {
        64
    };
    assert_eq!(
        (frame.width, frame.height, frame.format, frame.stride),
        (64, 48, expected_format, expected_stride)
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
    assert_eq!(
        resource.offer_id,
        if state.format == amaterasu_media::PIXEL_P010 {
            2
        } else {
            1
        }
    );
    assert_eq!(resource.resource.kind, FRAME_RESOURCE_CPU);
    // SAFETY: this negotiated CPU schema points to a live CpuFrame.
    let frame = unsafe { &*resource.resource.data.cast::<CpuFrame>() };
    assert_eq!(frame.timestamp_us, resource.timestamp_us);
    assert_eq!(frame.sequence, resource.sequence);
    // SAFETY: borrowed-only retain callback is valid during this resource callback.
    assert_eq!(unsafe { (resource.retain)(resource.context) }, STATUS_OK);
    let none = FrameDescriptor {
        kind: FRAME_SYNC_NONE,
        version: 0,
        size: 0,
        reserved: 0,
        namespace: [0; 16],
        data: std::ptr::null(),
    };
    unsafe { (resource.release)(resource.context, &none) };
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
    assert_eq!(offer.ownership, FRAME_OWNERSHIP_RETAINABLE);
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
        (caps.format, 64, 64, 48, 48)
    );
    assert!(matches!(
        caps.format,
        PIXEL_NV12 | amaterasu_media::PIXEL_P010 | amaterasu_media::PIXEL_BGRA8
    ));
    let selected = crate::model::resource::ResourceMode::find(offer.id).unwrap();
    assert_eq!(offer.resource.version, selected.schema());
    if let Some(expected) = selected.color {
        assert_eq!(
            offer.resource.size,
            size_of::<CpuResourceCapabilitiesV2>() as u32
        );
        let actual = unsafe { &*offer.resource.data.cast::<CpuResourceCapabilitiesV2>() };
        assert_eq!(
            (
                actual.color.primaries,
                actual.color.transfer,
                actual.color.matrix,
                actual.color.range
            ),
            (
                expected.primaries,
                expected.transfer,
                expected.matrix,
                expected.range
            )
        );
    }
    STATUS_OK
}

unsafe extern "C" fn inspect_known_black(
    context: *mut c_void,
    resource: *const CaptureResourceFrame,
) -> Status {
    let expected = unsafe { &*context.cast::<crate::model::resource::ResourceMode>() };
    let resource = unsafe { &*resource };
    assert_eq!(resource.offer_id, expected.id);
    assert_eq!(resource.resource.version, 2);
    let color = expected.color.unwrap();
    assert_eq!(
        (
            resource.color.primaries,
            resource.color.transfer,
            resource.color.matrix,
            resource.color.range
        ),
        (color.primaries, color.transfer, color.matrix, color.range)
    );
    let cpu = unsafe { &*resource.resource.data.cast::<CpuFrame>() };
    assert_eq!(cpu.format, expected.format);
    let bytes = unsafe { cpu.bytes() }.unwrap();
    let luma = (cpu.width * cpu.height) as usize;
    match cpu.format {
        amaterasu_media::PIXEL_P010 => {
            assert!(
                bytes[..luma * 2]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .all(|sample| u16::from_le_bytes(*sample)
                        == if expected.full_range { 0 } else { 64 << 6 })
            );
            assert!(
                bytes[luma * 2..]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .all(|sample| u16::from_le_bytes(*sample) == 512 << 6)
            );
        }
        amaterasu_media::PIXEL_NV12 => {
            assert!(bytes[..luma].iter().all(|value| *value == 16));
            assert!(bytes[luma..].iter().all(|value| *value == 128));
        }
        amaterasu_media::PIXEL_BGRA8 => assert!(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [0, 0, 0, 255])
        ),
        _ => panic!("unsupported generated format"),
    }
    STATUS_OK
}

#[test]
fn explicit_color_offers_deliver_real_sdr_full_limited_and_hdr_black_codes() {
    for expected in crate::model::resource::MODES
        .into_iter()
        .filter(|mode| mode.color.is_some())
    {
        let (_controller, factory, id) = setup();
        let owner = OwnedBinding::new(&factory, "video");
        let api = owner.api();
        let context = api.base.base.base.context;
        assert_eq!(
            unsafe { (api.select_resource)(context, id, id, expected.id) },
            STATUS_OK
        );
        let sink = CaptureResourceFrameSink {
            context: std::ptr::from_ref(&expected).cast_mut().cast(),
            write: inspect_known_black,
        };
        assert_eq!(
            unsafe { (api.read_resource_frame)(context, id, sink) },
            STATUS_OK
        );
    }
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
    assert_eq!(offers.calls, 7);
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

#[test]
fn p010_native_cpu_delivery_preserves_neutral_codes_and_true_pixel_format() {
    let (_controller, factory, id) = setup_output("black:64x48@60:p010");
    let owner = OwnedBinding::new(&factory, "video");
    let api = owner.api();
    let mut frames = Captured {
        format: amaterasu_media::PIXEL_P010,
        ..Default::default()
    };
    let sink = CaptureResourceFrameSink {
        context: (&mut frames as *mut Captured).cast(),
        write: inspect_resource,
    };
    // SAFETY: retained binding and synchronous resource callback remain live.
    assert_eq!(
        unsafe { (api.select_resource)(api.base.base.base.context, id, id, 2) },
        STATUS_OK
    );
    assert_eq!(
        unsafe { (api.read_resource_frame)(api.base.base.base.context, id, sink) },
        STATUS_OK
    );
    let bytes = &frames.frames[0].2;
    assert_eq!(bytes.len(), 64 * 48 * 3);
    assert!(
        bytes[..64 * 48 * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .all(|sample| u16::from_le_bytes([sample[0], sample[1]]) == 64 << 6)
    );
    assert!(
        bytes[64 * 48 * 2..]
            .as_chunks::<2>()
            .0
            .iter()
            .all(|sample| u16::from_le_bytes([sample[0], sample[1]]) == 512 << 6)
    );
}
