//! Independently implemented synthetic E-AC-3-labelled provider; bytes are fixtures.
use amaterasu_abi::*;
use amaterasu_media::*;
use std::{ffi::c_void, mem::size_of};
mod ordered;

#[repr(C)]
struct Encoder {
    api: NegotiatedAudioEncoderApi,
    phase: u8,
}
unsafe extern "C" fn enumerate(
    _: *mut c_void,
    _: u64,
    sink: AudioFormatOfferSink,
    generation: *mut u64,
) -> Status {
    let descriptor = AudioFormatDescriptor {
        size: size_of::<AudioFormatDescriptor>() as u32,
        version: AUDIO_FORMAT_VERSION,
        namespace: [0; 16],
        codec: AUDIO_CODEC_EAC3,
        framing: AUDIO_FRAMING_EAC3_SYNCFRAME,
        sample_rate: 48_000,
        channels: 6,
        channel_mask: 0x3f,
        frame_samples: 1536,
        reserved: 0,
        config: BorrowedBytes::new(&[]),
    };
    unsafe {
        *generation = 202;
        (sink.write)(
            sink.context,
            &AudioFormatOffer {
                id: 22,
                format: descriptor,
            },
        )
    }
}
unsafe extern "C" fn configure(ctx: *mut c_void, stream: u64, generation: u64, id: u64) -> Status {
    if (stream, generation, id) != (19, 202, 22) {
        return MEDIA_UNSUPPORTED;
    }
    unsafe {
        (*ctx.cast::<Encoder>()).phase = 1;
    }
    STATUS_OK
}
unsafe extern "C" fn submit(_: *mut c_void, _: u64, _: *const AudioFrame) -> Status {
    STATUS_OK
}
unsafe extern "C" fn receive(
    ctx: *mut c_void,
    stream: u64,
    sink: NegotiatedAudioPacketSink,
) -> Status {
    let encoder = unsafe { &mut *ctx.cast::<Encoder>() };
    if encoder.phase != 1 {
        return MEDIA_PENDING;
    }
    let sample = [0xe2, 0x22];
    let packet = NegotiatedAudioPacket {
        stream,
        generation: 202,
        offer_id: 22,
        flags: 0,
        frames: 1536,
        timestamp_us: 0,
        sequence: 1,
        data: BorrowedBytes::new(&sample),
    };
    match unsafe { (sink.write)(sink.context, &packet) } {
        STATUS_OK => {
            encoder.phase = 2;
            STATUS_OK
        }
        other => other,
    }
}
unsafe extern "C" fn drain(_: *mut c_void, _: u64) -> Status {
    STATUS_OK
}
#[unsafe(no_mangle)]
pub extern "C" fn fixture_create() -> *mut NegotiatedAudioEncoderApi {
    let mut encoder = Box::new(Encoder {
        api: NegotiatedAudioEncoderApi {
            base: NativeInterface {
                header: AbiHeader {
                    size: size_of::<NegotiatedAudioEncoderApi>() as u32,
                    major: 1,
                    minor: 0,
                },
                context: std::ptr::null_mut(),
                release: None,
            },
            enumerate,
            configure,
            submit,
            receive,
            drain,
        },
        phase: 0,
    });
    encoder.api.base.context = (&mut *encoder as *mut Encoder).cast();
    Box::into_raw(encoder).cast()
}
/// # Safety
/// `table` must be an unfreed pointer returned by `fixture_create` with no calls in flight.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_destroy(table: *mut NegotiatedAudioEncoderApi) {
    if !table.is_null() {
        drop(unsafe { Box::from_raw(table.cast::<Encoder>()) });
    }
}
