use super::*;
use amaterasu_abi::STATUS_OK;

#[test]
fn frame_shapes_reject_overflow_invalid_planes_and_excessive_memory() {
    assert_eq!(frame_size(1920, 1080, PIXEL_BGRA8, 7680), Ok(8_294_400));
    assert_eq!(frame_size(1920, 1080, PIXEL_NV12, 1920), Ok(3_110_400));
    for shape in [
        (0, 1080, PIXEL_BGRA8, 7680),
        (8193, 1, PIXEL_BGRA8, 32772),
        (1920, 1080, PIXEL_BGRA8, 7679),
        (1919, 1080, PIXEL_NV12, 1920),
        (1920, 1079, PIXEL_NV12, 1920),
        (1920, 1080, 99, 7680),
        (8192, 8192, PIXEL_BGRA8, u32::MAX),
    ] {
        assert_eq!(
            frame_size(shape.0, shape.1, shape.2, shape.3),
            Err(STATUS_INVALID_ARGUMENT)
        );
    }
}

#[test]
fn callback_copies_borrowed_frame_and_propagates_backpressure() {
    let pixels = vec![7; 16];
    let mut received = Vec::<u8>::new();
    let frame = CpuFrame {
        width: 2,
        height: 2,
        format: PIXEL_BGRA8,
        stride: 8,
        data: pixels.as_ptr(),
        len: pixels.len(),
        timestamp_us: 100,
        sequence: 1,
    };
    unsafe extern "C" fn copy(context: *mut c_void, frame: *const CpuFrame) -> Status {
        // SAFETY: test supplies a live frame and exclusive destination for the callback.
        let (target, frame) = unsafe { (&mut *context.cast::<Vec<u8>>(), &*frame) };
        // SAFETY: pixels outlive the synchronous callback and have no writer.
        target.extend_from_slice(unsafe { frame.bytes().unwrap() });
        MEDIA_BUSY
    }
    let sink = FrameSink {
        context: (&mut received as *mut Vec<u8>).cast(),
        write: copy,
    };
    // SAFETY: local context, function, and pixels remain valid throughout the call.
    assert_eq!(unsafe { sink.send(&frame) }, MEDIA_BUSY);
    drop(pixels);
    assert_eq!(received, vec![7; 16]);
}

#[test]
fn packet_validation_distinguishes_payloads_from_end_of_stream() {
    let data = [0, 0, 0, 1, 0x65];
    let packet = EncodedPacket {
        codec: CODEC_H264,
        flags: FLAG_KEYFRAME,
        data: data.as_ptr(),
        len: data.len(),
        timestamp_us: 0,
        sequence: 1,
    };
    assert!(packet.validate().is_ok());
    assert!(
        EncodedPacket {
            codec: CODEC_HEVC,
            ..packet
        }
        .validate()
        .is_ok()
    );
    assert!(
        EncodedPacket {
            codec: CODEC_AV1 + 1,
            ..packet
        }
        .validate()
        .is_err()
    );
    assert!(
        EncodedPacket {
            codec: CODEC_AV1,
            ..packet
        }
        .validate()
        .is_ok()
    );
    assert!(EncodedPacket { len: 0, ..packet }.validate().is_err());
    assert!(
        EncodedPacket {
            flags: 128,
            ..packet
        }
        .validate()
        .is_err()
    );
    assert!(
        EncodedPacket {
            len: MAX_PACKET_BYTES + 1,
            ..packet
        }
        .validate()
        .is_err()
    );
    let eos = EncodedPacket {
        flags: FLAG_END_OF_STREAM,
        data: std::ptr::null(),
        len: 0,
        ..packet
    };
    // SAFETY: empty EOS has no allocation to borrow.
    assert_eq!(unsafe { eos.bytes().unwrap() }, &[] as &[u8]);
    assert!(
        EncodedPacket {
            flags: FLAG_END_OF_ACCESS_UNIT,
            ..packet
        }
        .validate()
        .is_ok()
    );
    assert!(
        EncodedPacket {
            flags: FLAG_KEYFRAME | FLAG_END_OF_ACCESS_UNIT,
            ..packet
        }
        .validate()
        .is_ok()
    );
    assert!(
        EncodedPacket {
            flags: FLAG_END_OF_STREAM | FLAG_END_OF_ACCESS_UNIT,
            ..eos
        }
        .validate()
        .is_err()
    );
    assert!(
        EncodedPacket {
            flags: FLAG_END_OF_STREAM | FLAG_KEYFRAME,
            ..eos
        }
        .validate()
        .is_err()
    );
}

#[test]
fn hevc_payload_and_eos_are_valid_while_unknown_video_codecs_fail_closed() {
    let data = [0, 0, 0, 1, 0x26, 0x01, 0x80];
    let packet = EncodedPacket {
        codec: CODEC_HEVC,
        flags: FLAG_KEYFRAME | FLAG_END_OF_ACCESS_UNIT,
        data: data.as_ptr(),
        len: data.len(),
        timestamp_us: 0,
        sequence: 1,
    };
    assert!(packet.validate().is_ok());

    let eos = EncodedPacket {
        flags: FLAG_END_OF_STREAM,
        data: std::ptr::null(),
        len: 0,
        ..packet
    };
    // SAFETY: empty EOS has no allocation to borrow.
    assert_eq!(unsafe { eos.bytes().unwrap() }, &[] as &[u8]);

    assert!(
        EncodedPacket {
            codec: u32::MAX,
            ..packet
        }
        .validate()
        .is_err()
    );
    assert!(
        EncodedPacket {
            codec: u32::MAX,
            ..eos
        }
        .validate()
        .is_err()
    );
}

#[test]
fn invalid_frame_never_invokes_sink() {
    unsafe extern "C" fn never(_: *mut c_void, _: *const CpuFrame) -> Status {
        STATUS_OK
    }
    let frame = CpuFrame {
        width: 2,
        height: 2,
        format: PIXEL_BGRA8,
        stride: 8,
        data: std::ptr::null(),
        len: 16,
        timestamp_us: 0,
        sequence: 0,
    };
    let sink = FrameSink {
        context: std::ptr::null_mut(),
        write: never,
    };
    // SAFETY: callback needs no context and the invalid data is never dereferenced.
    assert_eq!(unsafe { sink.send(&frame) }, STATUS_INVALID_ARGUMENT);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn media_records_have_explicit_64_bit_c_layouts() {
    assert_eq!(std::mem::size_of::<CpuFrame>(), 48);
    assert_eq!(std::mem::offset_of!(CpuFrame, data), 16);
    assert_eq!(std::mem::offset_of!(CpuFrame, timestamp_us), 32);
    assert_eq!(std::mem::size_of::<EncodedPacket>(), 40);
    assert_eq!(std::mem::offset_of!(EncodedPacket, data), 8);
    assert_eq!(std::mem::offset_of!(CaptureApi, base), 0);
    assert_eq!(std::mem::offset_of!(EncoderApi, base), 0);
    assert_eq!(std::mem::offset_of!(VideoApi, base), 0);
    assert_eq!(std::mem::size_of::<CaptureApiPrefix>(), 40);
    assert_eq!(CAPTURE_API_V1_0_SIZE, 40);
    assert_eq!(std::mem::size_of::<CaptureApi>(), 48);
    assert_eq!(CAPTURE_API_V1_1_SIZE, 48);
    assert_eq!(std::mem::size_of::<CaptureApiV1_2>(), 56);
    assert_eq!(CAPTURE_API_V1_2_SIZE, 56);
    assert_eq!(
        std::mem::offset_of!(CaptureApiV1_2, read_frame_with_freshness),
        48
    );
    assert_eq!(std::mem::size_of::<CaptureFrameV1_3>(), 56);
    assert_eq!(std::mem::offset_of!(CaptureFrameV1_3, frame), 0);
    assert_eq!(
        std::mem::offset_of!(CaptureFrameV1_3, capture_pacing_timestamp_us),
        48
    );
    assert_eq!(std::mem::size_of::<CaptureApiV1_3>(), 64);
    assert_eq!(CAPTURE_API_V1_3_SIZE, 64);
    assert_eq!(std::mem::offset_of!(CaptureApiV1_3, base), 0);
    assert_eq!(
        std::mem::offset_of!(CaptureApiV1_3, read_frame_with_freshness),
        48
    );
    assert_eq!(
        std::mem::offset_of!(CaptureApiV1_3, read_frame_with_capture_timestamp),
        56
    );
    assert_eq!(std::mem::size_of::<EncoderApi>(), 56);
    assert_eq!(std::mem::size_of::<VideoApi>(), 40);
}

#[test]
fn freshness_sink_accepts_only_defined_provenance_values() {
    unsafe extern "C" fn record(
        context: *mut c_void,
        _: *const CpuFrame,
        freshness: u32,
    ) -> Status {
        // SAFETY: the test passes a writable u32 for this synchronous callback.
        unsafe { context.cast::<u32>().write(freshness) };
        STATUS_OK
    }

    let bytes = [0u8; 4];
    let frame = CpuFrame {
        width: 1,
        height: 1,
        format: PIXEL_BGRA8,
        stride: 4,
        data: bytes.as_ptr(),
        len: bytes.len(),
        timestamp_us: 0,
        sequence: 1,
    };
    let mut observed = u32::MAX;
    let sink = FreshFrameSinkV1_2 {
        context: (&mut observed as *mut u32).cast(),
        write: record,
    };

    // SAFETY: frame bytes and callback context both outlive the synchronous send.
    assert_eq!(unsafe { sink.send(&frame, CAPTURE_FRAME_FRESH) }, STATUS_OK);
    assert_eq!(observed, CAPTURE_FRAME_FRESH);
    // SAFETY: same synchronous callback contract as above.
    assert_eq!(
        unsafe { sink.send(&frame, CAPTURE_FRAME_CACHED_REUSE) },
        STATUS_OK
    );
    assert_eq!(observed, CAPTURE_FRAME_CACHED_REUSE);
    // SAFETY: invalid freshness is rejected before the callback is called.
    assert_eq!(unsafe { sink.send(&frame, 2) }, STATUS_INVALID_ARGUMENT);
    assert_eq!(observed, CAPTURE_FRAME_CACHED_REUSE);
}

#[test]
fn pacing_timestamp_sink_preserves_source_time_and_validates_freshness() {
    unsafe extern "C" fn record(
        context: *mut c_void,
        frame: *const CaptureFrameV1_3,
        freshness: u32,
    ) -> Status {
        if frame.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: test sends a live frame and a writable two-value result.
        let frame = unsafe { &*frame };
        unsafe {
            let observed = context.cast::<[u64; 2]>();
            (*observed)[0] = frame.capture_pacing_timestamp_us;
            (*observed)[1] = u64::from(freshness);
        }
        STATUS_OK
    }

    let bytes = [0u8; 4];
    let frame = CaptureFrameV1_3 {
        frame: CpuFrame {
            width: 1,
            height: 1,
            format: PIXEL_BGRA8,
            stride: 4,
            data: bytes.as_ptr(),
            len: bytes.len(),
            timestamp_us: 900,
            sequence: 5,
        },
        capture_pacing_timestamp_us: 123,
    };
    let mut observed = [u64::MAX; 2];
    let sink = FreshFrameSinkV1_3 {
        context: (&mut observed as *mut [u64; 2]).cast(),
        write: record,
    };

    // SAFETY: bytes, result storage, and callback context live through send.
    assert_eq!(unsafe { sink.send(&frame, CAPTURE_FRAME_FRESH) }, STATUS_OK);
    assert_eq!(observed, [123, u64::from(CAPTURE_FRAME_FRESH)]);
    // SAFETY: invalid freshness is rejected before entering the callback.
    assert_eq!(unsafe { sink.send(&frame, 2) }, STATUS_INVALID_ARGUMENT);
    assert_eq!(observed, [123, u64::from(CAPTURE_FRAME_FRESH)]);
}

#[test]
fn video_geometry_tail_preserves_every_legacy_prefix_offset() {
    use std::mem::{offset_of, size_of};
    assert_eq!(offset_of!(VideoApiV1_1, base), offset_of!(VideoApi, base));
    assert_eq!(
        offset_of!(VideoApiV1_1, receive),
        offset_of!(VideoApi, receive)
    );
    assert_eq!(offset_of!(VideoApiV1_1, geometry), size_of::<VideoApi>());
}

fn dmabuf_frame() -> DmaBufFrame {
    let mut planes = [DmaBufPlane::UNUSED; DMABUF_MAX_PLANES];
    planes[0] = DmaBufPlane {
        fd: 7,
        acquire_fence_fd: -1,
        offset: 0,
        pitch: 7680,
    };
    DmaBufFrame {
        width: 1920,
        height: 1080,
        fourcc: u32::from_le_bytes(*b"XR24"),
        plane_count: 1,
        modifier: DRM_FORMAT_MOD_INVALID,
        planes,
        color: DMABUF_COLOR_SRGB,
        reserved: 0,
        timestamp_us: 10,
        sequence: 1,
    }
}

#[test]
fn dmabuf_validation_requires_exact_used_and_unused_planes() {
    let frame = dmabuf_frame();
    assert!(frame.validate().is_ok());
    assert_eq!(frame.used_planes().len(), 1);
    let mut missing_fd = frame;
    missing_fd.planes[0].fd = -1;
    assert!(missing_fd.validate().is_err());
    let mut stray_plane = frame;
    stray_plane.planes[1].fd = 9;
    assert!(stray_plane.validate().is_err());
    let mut zero_pitch = frame;
    zero_pitch.planes[0].pitch = 0;
    assert!(zero_pitch.validate().is_err());
    for invalid in [
        DmaBufFrame { width: 0, ..frame },
        DmaBufFrame {
            height: DMABUF_MAX_DIMENSION + 1,
            ..frame
        },
        DmaBufFrame { fourcc: 0, ..frame },
        DmaBufFrame {
            plane_count: 0,
            ..frame
        },
        DmaBufFrame {
            plane_count: 5,
            ..frame
        },
        DmaBufFrame { color: 0, ..frame },
        DmaBufFrame {
            reserved: 1,
            ..frame
        },
    ] {
        assert!(invalid.validate().is_err());
    }
    let hdr = DmaBufFrame {
        color: DMABUF_COLOR_BT2020_PQ,
        ..frame
    };
    assert!(hdr.validate().is_ok());
}

#[test]
fn dmabuf_sink_rejects_invalid_frames_and_freshness_before_callback() {
    unsafe extern "C" fn write(
        context: *mut c_void,
        frame: *const DmaBufFrame,
        fresh: u32,
    ) -> Status {
        // SAFETY: the test passes a live counter and a frame valid for this call.
        unsafe {
            *(context as *mut u64) += (*frame).sequence + u64::from(fresh);
        }
        STATUS_OK
    }
    let mut calls = 0u64;
    let sink = DmaBufFrameSink {
        context: (&mut calls as *mut u64).cast(),
        write,
    };
    let frame = dmabuf_frame();
    // SAFETY: context and frame outlive these synchronous calls.
    unsafe {
        assert_eq!(sink.send(&frame, CAPTURE_FRAME_FRESH), STATUS_OK);
        assert_ne!(sink.send(&frame, 2), STATUS_OK);
        assert_ne!(
            sink.send(&DmaBufFrame { width: 0, ..frame }, CAPTURE_FRAME_FRESH),
            STATUS_OK
        );
    }
    assert_eq!(calls, 2);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn dmabuf_and_gpu_table_layouts_are_stable() {
    use std::mem::{offset_of, size_of};
    assert_eq!(size_of::<DmaBufPlane>(), 16);
    assert_eq!(size_of::<DmaBufFrame>(), 112);
    assert_eq!(offset_of!(DmaBufFrame, modifier), 16);
    assert_eq!(offset_of!(DmaBufFrame, planes), 24);
    assert_eq!(offset_of!(DmaBufFrame, color), 88);
    assert_eq!(offset_of!(DmaBufFrame, timestamp_us), 96);
    assert_eq!(size_of::<DmaBufFrameSink>(), 16);
    assert_eq!(CAPTURE_API_V1_4_SIZE, 72);
    assert_eq!(offset_of!(CaptureApiV1_4, read_dmabuf_frame), 64);
    assert_eq!(CAPTURE_API_V1_5_SIZE, 80);
    assert_eq!(offset_of!(CaptureApiV1_5, latest_presentation), 72);
    assert_eq!(size_of::<PresentationStatus>(), 16);
    assert_eq!(ENCODER_API_V1_0_SIZE, 56);
    assert_eq!(ENCODER_API_V1_1_SIZE, 64);
    assert_eq!(offset_of!(EncoderApiV1_1, submit_dmabuf), 56);
}
