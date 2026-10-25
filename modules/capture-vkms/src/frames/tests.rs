use super::*;
use crate::test_support::{FakeSource, Scripted, SourceScript};

const EPOCH_NS: u64 = 10_000_000;

fn stream() -> (StreamFrames, Arc<SourceScript>) {
    let script = Arc::new(SourceScript::default());
    let frames = StreamFrames::new(Box::new(FakeSource(script.clone())), 41, EPOCH_NS);
    (frames, script)
}

fn deliver(frames: &StreamFrames, status: Status) -> Result<(DmaBufFrame, u32), Status> {
    let lent = frames.lend()?;
    let observed = (lent.frame, lent.freshness);
    assert_eq!(frames.finish(lent, status), status);
    Ok(observed)
}

#[test]
fn a_new_sequence_is_fresh_and_a_repeat_is_cached_reuse() {
    let (frames, script) = stream();
    script.push(Scripted::Frame {
        sequence: 5,
        timestamp_ns: EPOCH_NS + 2_000_000,
    });
    script.push(Scripted::Frame {
        sequence: 5,
        timestamp_ns: EPOCH_NS + 2_000_000,
    });
    script.push(Scripted::Frame {
        sequence: 6,
        timestamp_ns: EPOCH_NS + 18_000_000,
    });
    let (first, freshness) = deliver(&frames, STATUS_OK).unwrap();
    assert_eq!(freshness, CAPTURE_FRAME_FRESH);
    assert_eq!(first.sequence, 5);
    assert_eq!(first.timestamp_us, 2_000);
    assert_eq!(
        deliver(&frames, STATUS_OK).unwrap().1,
        CAPTURE_FRAME_CACHED_REUSE
    );
    let (third, freshness) = deliver(&frames, STATUS_OK).unwrap();
    assert_eq!(freshness, CAPTURE_FRAME_FRESH);
    assert_eq!(third.timestamp_us, 18_000);
    assert_eq!(frames.delivered(), 3);
}

#[test]
fn a_rejected_fresh_frame_stays_fresh() {
    let (frames, script) = stream();
    for _ in 0..2 {
        script.push(Scripted::Frame {
            sequence: 9,
            timestamp_ns: EPOCH_NS,
        });
    }
    assert_eq!(deliver(&frames, MEDIA_BUSY).unwrap().1, CAPTURE_FRAME_FRESH);
    assert_eq!(deliver(&frames, STATUS_OK).unwrap().1, CAPTURE_FRAME_FRESH);
    assert_eq!(frames.delivered(), 1);
}

#[test]
fn timestamps_start_at_stream_open_and_never_decrease() {
    let (frames, script) = stream();
    script.push(Scripted::Frame {
        sequence: 1,
        timestamp_ns: EPOCH_NS - 5_000_000,
    });
    script.push(Scripted::Frame {
        sequence: 2,
        timestamp_ns: EPOCH_NS + 7_000_000,
    });
    script.push(Scripted::Frame {
        sequence: 3,
        timestamp_ns: EPOCH_NS + 6_000_000,
    });
    let stamps: Vec<u64> = (0..3)
        .map(|_| deliver(&frames, STATUS_OK).unwrap().0.timestamp_us)
        .collect();
    assert_eq!(stamps, vec![0, 7_000, 7_000]);
}

#[test]
fn the_lent_frame_describes_the_export_and_closes_after_finish() {
    let (frames, script) = stream();
    script.push(Scripted::FencedFrame { sequence: 4 });
    let lent = frames.lend().unwrap();
    let frame = lent.frame;
    assert!(frame.validate().is_ok());
    assert_eq!(frame.width, 1920);
    assert_eq!(frame.fourcc, u32::from_le_bytes(*b"XR24"));
    assert_eq!(frame.modifier, 0x0100_0000_0000_0004);
    assert_eq!(frame.color, DMABUF_COLOR_SRGB);
    assert_eq!(frame.planes[0].pitch, 7680);
    assert!(frame.planes[0].fd >= 0);
    assert!(frame.planes[0].acquire_fence_fd >= 0);
    assert_eq!(frame.planes[1], DmaBufPlane::UNUSED);
    assert!(!script.all_lent_descriptors_closed());
    frames.finish(lent, STATUS_OK);
    assert!(script.all_lent_descriptors_closed());
}

#[test]
fn reads_request_the_current_crtc() {
    let (frames, script) = stream();
    let _ = frames.lend();
    frames.retarget(77);
    let _ = frames.lend();
    assert_eq!(*script.requested_crtcs.lock().unwrap(), vec![41, 77]);
}

#[test]
fn export_failures_map_to_media_statuses() {
    let (frames, script) = stream();
    script.push(Scripted::Empty);
    assert_eq!(frames.lend().err(), Some(MEDIA_PENDING));

    script.push(Scripted::Refuse(Failure::Busy));
    assert_eq!(frames.lend().err(), Some(MEDIA_BUSY));

    script.push(Scripted::Refuse(Failure::CrtcNotFound));
    assert_eq!(frames.lend().err(), Some(MEDIA_PENDING));
    assert!(frames.take_resolve_request());
    assert!(!frames.take_resolve_request());
    assert_eq!(frames.fault(), None);

    script.push(Scripted::Refuse(Failure::PermissionDenied));
    assert_eq!(frames.lend().err(), Some(MEDIA_BACKEND_ERROR));
    assert_eq!(frames.fault(), Some(StreamFault::PermissionDenied));
    // A faulted stream does not ask the helper again.
    let requests = script.requested_crtcs.lock().unwrap().len();
    assert_eq!(frames.lend().err(), Some(MEDIA_BACKEND_ERROR));
    assert_eq!(script.requested_crtcs.lock().unwrap().len(), requests);
}

#[test]
fn a_lost_helper_or_output_ends_delivery() {
    let (frames, script) = stream();
    script.push(Scripted::HelperGone);
    assert_eq!(frames.lend().err(), Some(MEDIA_BACKEND_ERROR));
    assert_eq!(
        frames.fault().map(StreamFault::code),
        Some("frame_helper_failed")
    );

    let (frames, _) = stream();
    frames.set_fault(StreamFault::OutputLost("output_disconnected"));
    assert_eq!(frames.lend().err(), Some(MEDIA_CLOSED));
    // The first fault is kept.
    frames.set_fault(StreamFault::HelperFailed);
    assert_eq!(
        frames.fault(),
        Some(StreamFault::OutputLost("output_disconnected"))
    );
}
