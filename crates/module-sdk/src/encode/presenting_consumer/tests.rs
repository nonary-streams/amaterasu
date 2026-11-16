use super::*;
use std::cell::RefCell;

#[derive(Default)]
struct Producer {
    retained: Cell<usize>,
}
unsafe extern "C" fn retain_source(context: *mut c_void) -> Status {
    let producer = unsafe { &*context.cast::<Producer>() };
    producer.retained.set(producer.retained.get() + 1);
    STATUS_OK
}
unsafe extern "C" fn release_source(context: *mut c_void, _: *const FrameDescriptor) {
    let producer = unsafe { &*context.cast::<Producer>() };
    producer.retained.set(producer.retained.get() - 1);
}

/// A plugin encoder that retains every accepted frame past the submit call.
#[derive(Default)]
struct Consumer {
    held: RefCell<Vec<CaptureResourceFrame>>,
    configured: Cell<u64>,
    cancelled: Cell<u64>,
}
unsafe extern "C" fn consumer_enumerate(
    _: *mut c_void,
    _: u64,
    _: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    unsafe { *generation = 4 };
    STATUS_OK
}
unsafe extern "C" fn consumer_configure(
    context: *mut c_void,
    _: u64,
    _: u64,
    offer: u64,
    _: u64,
    _: *const CaptureResourceOffer,
) -> Status {
    unsafe { &*context.cast::<Consumer>() }
        .configured
        .set(offer);
    STATUS_OK
}
unsafe extern "C" fn consumer_submit(
    context: *mut c_void,
    _: u64,
    frame: *const CaptureResourceFrame,
    _: u32,
) -> Status {
    let consumer = unsafe { &*context.cast::<Consumer>() };
    let frame = unsafe { *frame };
    let status = unsafe { (frame.retain)(frame.context) };
    if status == STATUS_OK {
        consumer.held.borrow_mut().push(frame);
    }
    status
}
unsafe extern "C" fn consumer_cancel(context: *mut c_void, stream: u64) {
    unsafe { &*context.cast::<Consumer>() }
        .cancelled
        .set(stream);
}
fn consumer_api(consumer: &Consumer) -> FrameResourceConsumerApi {
    FrameResourceConsumerApi {
        base: NativeInterface {
            header: AbiHeader::of::<FrameResourceConsumerApi>(),
            context: std::ptr::from_ref(consumer).cast_mut().cast(),
            release: None,
        },
        enumerate_resources: consumer_enumerate,
        configure: consumer_configure,
        submit: consumer_submit,
        cancel: consumer_cancel,
    }
}
fn payload() -> DmaBufFrame {
    let mut planes = [DmaBufPlane::UNUSED; DMABUF_MAX_PLANES];
    planes[0] = DmaBufPlane {
        fd: 0,
        acquire_fence_fd: -1,
        offset: 0,
        pitch: 8,
    };
    DmaBufFrame {
        width: 2,
        height: 2,
        fourcc: u32::from_le_bytes(*b"AB30"),
        plane_count: 1,
        modifier: 0,
        planes,
        color: DMABUF_COLOR_SRGB,
        reserved: 0,
        timestamp_us: 37,
        sequence: 91,
    }
}
/// A processor output: capture-clock identity, before encoder presentation.
fn processed(producer: &Producer, payload: &DmaBufFrame, kind: u32) -> CaptureResourceFrame {
    CaptureResourceFrame {
        offer_generation: 3,
        offer_id: 5,
        timestamp_us: 37,
        capture_pacing_timestamp_us: 36,
        sequence: 91,
        width: 2,
        height: 2,
        freshness: CAPTURE_FRAME_FRESH,
        reserved: 0,
        color: FrameColorMetadata {
            version: 1,
            size: size_of::<FrameColorMetadata>() as u32,
            primaries: FRAME_PRIMARIES_BT2020,
            transfer: FRAME_TRANSFER_PQ,
            matrix: FRAME_MATRIX_RGB,
            range: FRAME_RANGE_FULL,
        },
        clock: FrameClockMetadata {
            version: 1,
            size: size_of::<FrameClockMetadata>() as u32,
            domain: FRAME_CLOCK_HOST_MONOTONIC,
            reserved: 0,
            epoch_id: [3; 16],
        },
        pacing_clock: Default::default(),
        resource: FrameDescriptor {
            kind,
            version: 1,
            size: size_of::<DmaBufFrame>() as u32,
            reserved: 0,
            namespace: [0; 16],
            data: std::ptr::from_ref(payload).cast(),
        },
        acquire_sync: FrameDescriptor {
            kind: FRAME_SYNC_NONE,
            version: 0,
            size: 0,
            reserved: 0,
            namespace: [0; 16],
            data: std::ptr::null(),
        },
        context: std::ptr::from_ref(producer).cast_mut().cast(),
        retain: retain_source,
        release: release_source,
    }
}
const TIMELINE: HostVideoPresentation = HostVideoPresentation {
    timestamp_us: 5_000,
    epoch_id: [9; 16],
    sequence: 1,
};

#[test]
fn retained_processed_frame_reaches_the_consumer_on_the_video_timeline() {
    let producer = Producer::default();
    let consumer = Consumer::default();
    let inner = consumer_api(&consumer);
    let mut adapter = unsafe { PresentingResourceConsumer::new(&inner) }.unwrap();
    adapter.set_presentation(TIMELINE);
    let api = adapter.api();
    let pixels = payload();
    let frame = processed(&producer, &pixels, FRAME_RESOURCE_DMABUF);
    let status = unsafe { (api.submit)(api.base.context, 7, &frame, 0) };
    assert_eq!(status, STATUS_OK);
    // The consumer still holds the frame after submit returned.
    assert_eq!(producer.retained.get(), 1);
    let held = consumer.held.borrow()[0];
    assert_eq!(held.timestamp_us, TIMELINE.timestamp_us);
    assert_eq!(held.sequence, TIMELINE.sequence);
    assert_eq!(held.clock.epoch_id, TIMELINE.epoch_id);
    assert_eq!(held.clock.domain, FRAME_CLOCK_HOST_MONOTONIC);
    // Processing color and pixels pass through; the source clock becomes pacing data.
    assert_eq!(held.color.transfer, FRAME_TRANSFER_PQ);
    assert_eq!(held.capture_pacing_timestamp_us, frame.timestamp_us);
    let gpu = unsafe { &*held.resource.data.cast::<DmaBufFrame>() };
    assert_eq!((gpu.timestamp_us, gpu.planes), (5_000, pixels.planes));
    unsafe { (held.release)(held.context, &frame.acquire_sync) };
    assert_eq!(producer.retained.get(), 0);
}

#[test]
fn negotiation_and_cancellation_reach_the_wrapped_consumer() {
    let consumer = Consumer::default();
    let inner = consumer_api(&consumer);
    let adapter = unsafe { PresentingResourceConsumer::new(&inner) }.unwrap();
    let api = adapter.api();
    let mut generation = 0;
    unsafe extern "C" fn ignore_offer(_: *mut c_void, _: *const CaptureResourceOffer) -> Status {
        STATUS_OK
    }
    let sink = CaptureResourceOfferSink {
        context: std::ptr::null_mut(),
        write: ignore_offer,
    };
    assert_eq!(
        unsafe { (api.enumerate_resources)(api.base.context, 7, sink, &mut generation) },
        STATUS_OK
    );
    assert_eq!(generation, 4);
    assert_eq!(
        unsafe { (api.configure)(api.base.context, 7, 4, 12, 1, std::ptr::null()) },
        STATUS_OK
    );
    assert_eq!(consumer.configured.get(), 12);
    unsafe { (api.cancel)(api.base.context, 7) };
    assert_eq!(consumer.cancelled.get(), 7);
}

#[test]
fn frames_without_a_timeline_or_dmabuf_storage_are_declined() {
    let producer = Producer::default();
    let consumer = Consumer::default();
    let inner = consumer_api(&consumer);
    let mut adapter = unsafe { PresentingResourceConsumer::new(&inner) }.unwrap();
    let pixels = payload();
    let frame = processed(&producer, &pixels, FRAME_RESOURCE_DMABUF);
    let api = std::ptr::from_ref(adapter.api());
    let submit =
        |frame: &CaptureResourceFrame| unsafe { ((*api).submit)((*api).base.context, 7, frame, 0) };
    assert_eq!(submit(&frame), MEDIA_UNSUPPORTED);
    adapter.set_presentation(TIMELINE);
    let cpu = processed(&producer, &pixels, FRAME_RESOURCE_DMABUF + 100);
    assert_eq!(submit(&cpu), MEDIA_UNSUPPORTED);
    assert!(consumer.held.borrow().is_empty());
    assert_eq!(producer.retained.get(), 0);
}
