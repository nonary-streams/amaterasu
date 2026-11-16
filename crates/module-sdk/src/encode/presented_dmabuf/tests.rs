use super::*;
use std::cell::Cell;

#[derive(Default)]
struct Producer {
    retained: Cell<usize>,
    reject: Cell<bool>,
    releases: Cell<usize>,
    completion: Cell<u32>,
}
unsafe extern "C" fn retain_source(context: *mut c_void) -> Status {
    let producer = unsafe { &*context.cast::<Producer>() };
    if producer.reject.get() {
        return MEDIA_BUSY;
    }
    producer.retained.set(producer.retained.get() + 1);
    STATUS_OK
}
unsafe extern "C" fn release_source(context: *mut c_void, completion: *const FrameDescriptor) {
    let producer = unsafe { &*context.cast::<Producer>() };
    producer.retained.set(producer.retained.get() - 1);
    producer.releases.set(producer.releases.get() + 1);
    producer.completion.set(unsafe { (*completion).kind });
}
fn none() -> FrameDescriptor {
    FrameDescriptor {
        kind: FRAME_SYNC_NONE,
        version: 0,
        size: 0,
        reserved: 0,
        namespace: [0; 16],
        data: std::ptr::null(),
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
        fourcc: u32::from_le_bytes(*b"XR24"),
        plane_count: 1,
        modifier: 0,
        planes,
        color: DMABUF_COLOR_SRGB,
        reserved: 0,
        timestamp_us: 37,
        sequence: 91,
    }
}
fn source(producer: &Producer, payload: &DmaBufFrame) -> CaptureResourceFrame {
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
            primaries: FRAME_PRIMARIES_BT709,
            transfer: FRAME_TRANSFER_SRGB,
            matrix: FRAME_MATRIX_RGB,
            range: FRAME_RANGE_FULL,
        },
        clock: FrameClockMetadata {
            version: 1,
            size: size_of::<FrameClockMetadata>() as u32,
            domain: FRAME_CLOCK_STREAM_RELATIVE,
            ..Default::default()
        },
        pacing_clock: Default::default(),
        resource: FrameDescriptor {
            kind: FRAME_RESOURCE_DMABUF,
            version: 1,
            size: size_of::<DmaBufFrame>() as u32,
            reserved: 0,
            namespace: [0; 16],
            data: std::ptr::from_ref(payload).cast(),
        },
        acquire_sync: none(),
        context: std::ptr::from_ref(producer).cast_mut().cast(),
        retain: retain_source,
        release: release_source,
    }
}

#[test]
fn two_encoder_consumers_share_capture_storage_with_independent_presentations() {
    let producer = Producer::default();
    let pixels = payload();
    let captured = source(&producer, &pixels);
    let first = unsafe {
        PresentedDmaBufResource::new(
            &captured,
            HostVideoPresentation {
                timestamp_us: 1100,
                epoch_id: [7; 16],
                sequence: 1,
            },
        )
    }
    .unwrap();
    let second = unsafe {
        PresentedDmaBufResource::new(
            &captured,
            HostVideoPresentation {
                timestamp_us: 2200,
                epoch_id: [8; 16],
                sequence: 1,
            },
        )
    }
    .unwrap();
    // Consumer A retains asynchronously; consumer B borrows synchronously.
    let retained = *first.descriptor();
    assert_eq!(unsafe { (retained.retain)(retained.context) }, STATUS_OK);
    let borrowed = second.descriptor();
    assert_eq!(borrowed.timestamp_us, 2200);
    assert_eq!(borrowed.clock.epoch_id, [8; 16]);
    assert_eq!(
        unsafe { &*borrowed.resource.data.cast::<DmaBufFrame>() }.planes,
        pixels.planes
    );
    drop(second);
    drop(first);
    assert_eq!(producer.retained.get(), 1);
    // Both the adapted header and original descriptor storage remain valid.
    let gpu = unsafe { &*retained.resource.data.cast::<DmaBufFrame>() };
    assert_eq!(gpu.timestamp_us, 1100);
    assert_eq!(gpu.planes, pixels.planes);
    assert_eq!(gpu.sequence, 1);
    assert_eq!(retained.capture_pacing_timestamp_us, 37);
    assert_eq!(retained.pacing_clock.domain, FRAME_CLOCK_STREAM_RELATIVE);
    let fields = |color: FrameColorMetadata| {
        (
            color.version,
            color.size,
            color.primaries,
            color.transfer,
            color.matrix,
            color.range,
        )
    };
    assert_eq!(fields(retained.color), fields(captured.color));
    assert_eq!(captured.timestamp_us, 37);
    assert_eq!(pixels.timestamp_us, 37);
    unsafe { (retained.release)(retained.context, &none()) };
    assert_eq!(producer.retained.get(), 0);
    assert_eq!(producer.releases.get(), 1);
    assert_eq!(producer.completion.get(), FRAME_SYNC_NONE);
}

#[test]
fn cached_capture_can_be_presented_again_without_changing_the_producer_sequence() {
    let producer = Producer::default();
    let pixels = payload();
    let mut captured = source(&producer, &pixels);
    captured.freshness = CAPTURE_FRAME_CACHED_REUSE;
    for sequence in [1, 2] {
        let view = unsafe {
            PresentedDmaBufResource::new(
                &captured,
                HostVideoPresentation {
                    timestamp_us: 1100 + sequence * 100,
                    epoch_id: [7; 16],
                    sequence,
                },
            )
        }
        .unwrap();
        let descriptor = view.descriptor();
        assert_eq!(descriptor.sequence, sequence);
        assert_eq!(descriptor.capture_pacing_timestamp_us, 37);
        let gpu = unsafe { &*descriptor.resource.data.cast::<DmaBufFrame>() };
        assert_eq!(gpu.sequence, sequence);
        assert_eq!(gpu.timestamp_us, descriptor.timestamp_us);
        assert_eq!(gpu.planes, pixels.planes);
    }
    assert_eq!(captured.sequence, 91);
    assert_eq!(pixels.sequence, 91);
    assert_eq!(producer.retained.get(), 0);
}

#[test]
fn existing_pacing_clock_is_preserved_and_completions_reach_the_producer() {
    let producer = Producer::default();
    let pixels = payload();
    let mut captured = source(&producer, &pixels);
    captured.pacing_clock = FrameClockMetadata {
        version: 1,
        size: size_of::<FrameClockMetadata>() as u32,
        domain: FRAME_CLOCK_HOST_MONOTONIC,
        epoch_id: [9; 16],
        reserved: 0,
    };
    let view = unsafe {
        PresentedDmaBufResource::new(
            &captured,
            HostVideoPresentation {
                timestamp_us: 1100,
                epoch_id: [7; 16],
                sequence: 1,
            },
        )
    }
    .unwrap();
    assert_eq!(view.descriptor().capture_pacing_timestamp_us, 36);
    assert_eq!(view.descriptor().pacing_clock.epoch_id, [9; 16]);
    let retained = *view.descriptor();
    assert_eq!(unsafe { (retained.retain)(retained.context) }, STATUS_OK);
    drop(view);
    let completion = FrameDescriptor {
        kind: FRAME_SYNC_SYNC_FILE,
        version: 1,
        size: size_of::<FrameSyncFile>() as u32,
        ..none()
    };
    unsafe { (retained.release)(retained.context, &completion) };
    assert_eq!(producer.completion.get(), FRAME_SYNC_SYNC_FILE);
    assert_eq!(producer.retained.get(), 0);
}

#[test]
fn bad_headers_and_rejected_retains_do_not_take_capture_ownership() {
    let producer = Producer::default();
    let pixels = payload();
    let captured = source(&producer, &pixels);
    let presentation = HostVideoPresentation {
        timestamp_us: 1100,
        epoch_id: [7; 16],
        sequence: 1,
    };
    let mut bad = captured;
    bad.sequence += 1;
    assert!(unsafe { PresentedDmaBufResource::new(&bad, presentation) }.is_err());
    assert!(
        unsafe {
            PresentedDmaBufResource::new(
                &captured,
                HostVideoPresentation {
                    epoch_id: [0; 16],
                    ..presentation
                },
            )
        }
        .is_err()
    );
    let view = unsafe { PresentedDmaBufResource::new(&captured, presentation) }.unwrap();
    producer.reject.set(true);
    assert_eq!(
        unsafe { (view.descriptor().retain)(view.descriptor().context) },
        MEDIA_BUSY
    );
    drop(view);
    assert_eq!(producer.retained.get(), 0);
    assert_eq!(producer.releases.get(), 0);
}
