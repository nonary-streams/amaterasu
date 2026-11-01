//! Synthetic AC-3-labelled provider. The test bytes are not an encoded stream.
use amaterasu_abi::*;
use amaterasu_media::*;
use std::{ffi::c_void, mem::size_of};
mod ordered;

#[repr(C)]
struct Provider {
    table: NegotiatedAudioEncoderApi,
    selected: bool,
    pending: bool,
}
unsafe extern "C" fn enumerate(
    _: *mut c_void,
    _: u64,
    sink: AudioFormatOfferSink,
    generation: *mut u64,
) -> Status {
    let format = AudioFormatDescriptor {
        size: size_of::<AudioFormatDescriptor>() as u32,
        version: AUDIO_FORMAT_VERSION,
        namespace: [0; 16],
        codec: AUDIO_CODEC_AC3,
        framing: AUDIO_FRAMING_AC3_SYNCFRAME,
        sample_rate: 48_000,
        channels: 6,
        channel_mask: 0x3f,
        frame_samples: 1536,
        reserved: 0,
        config: BorrowedBytes::new(&[]),
    };
    let offer = AudioFormatOffer { id: 11, format };
    unsafe {
        *generation = 101;
        (sink.write)(sink.context, &offer)
    }
}
unsafe extern "C" fn configure(
    ctx: *mut c_void,
    stream: u64,
    generation: u64,
    offer: u64,
) -> Status {
    let provider = unsafe { &mut *ctx.cast::<Provider>() };
    if stream != 19 || generation != 101 || offer != 11 {
        return MEDIA_UNSUPPORTED;
    }
    provider.selected = true;
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
    let provider = unsafe { &mut *ctx.cast::<Provider>() };
    if !provider.selected || !provider.pending {
        return MEDIA_PENDING;
    }
    let bytes = [0xa1, 0x11];
    let packet = NegotiatedAudioPacket {
        stream,
        generation: 101,
        offer_id: 11,
        flags: 0,
        frames: 1536,
        timestamp_us: 0,
        sequence: 1,
        data: BorrowedBytes::new(&bytes),
    };
    let result = unsafe { (sink.write)(sink.context, &packet) };
    if result == STATUS_OK {
        provider.pending = false;
    }
    result
}
unsafe extern "C" fn drain(_: *mut c_void, _: u64) -> Status {
    STATUS_OK
}
#[unsafe(no_mangle)]
pub extern "C" fn fixture_create() -> *mut NegotiatedAudioEncoderApi {
    let mut provider = Box::new(Provider {
        table: NegotiatedAudioEncoderApi {
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
        selected: false,
        pending: true,
    });
    provider.table.base.context = (&mut *provider as *mut Provider).cast();
    Box::into_raw(provider).cast()
}
/// # Safety
/// `table` must be an unfreed pointer returned by `fixture_create` with no calls in flight.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_destroy(table: *mut NegotiatedAudioEncoderApi) {
    if !table.is_null() {
        drop(unsafe { Box::from_raw(table.cast::<Provider>()) });
    }
}
