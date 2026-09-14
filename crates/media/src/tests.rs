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
    assert_eq!(std::mem::size_of::<EncoderApi>(), 56);
    assert_eq!(std::mem::size_of::<VideoApi>(), 40);
}
