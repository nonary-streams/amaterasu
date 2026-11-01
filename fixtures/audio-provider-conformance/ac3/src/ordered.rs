//! Synthetic ordered AC-3-labelled replacement; these are not Dolby samples.
use amaterasu_abi::*;
use amaterasu_media::*;
use std::{ffi::c_void, mem::size_of};

#[repr(C)]
struct Provider {
    api: OrderedAudioEncoderApiV2,
    channels: u32,
    input: Option<AudioInputSelectionV2>,
    pending_timestamp: Option<u64>,
    buffered_frames: u32,
    buffered_timestamp: Option<u64>,
    sequence: u64,
    draining: bool,
    ended: bool,
}
fn layout(channels: u32) -> AudioChannelLayoutV2 {
    AudioChannelLayoutV2 {
        size: size_of::<AudioChannelLayoutV2>() as u32,
        version: AUDIO_ORDERED_VERSION,
        channels,
        reserved: 0,
        mask: if channels == 2 { 3 } else { 63 },
        lanes: if channels == 2 {
            [1, 2, 0, 0, 0, 0, 0, 0]
        } else {
            [1, 2, 4, 8, 16, 32, 0, 0]
        },
    }
}
unsafe extern "C" fn enumerate(
    context: *mut c_void,
    stream: u64,
    sink: AudioFormatOfferSinkV2,
    generation: *mut u64,
) -> Status {
    if context.is_null() || stream != 19 || generation.is_null() {
        return MEDIA_UNSUPPORTED;
    }
    let channels = unsafe { (*context.cast::<Provider>()).channels };
    let offer = AudioFormatOfferV2 {
        id: 11,
        format: AudioFormatDescriptorV2 {
            size: size_of::<AudioFormatDescriptorV2>() as u32,
            version: AUDIO_ORDERED_VERSION,
            format: AudioFormatDescriptor {
                size: size_of::<AudioFormatDescriptor>() as u32,
                version: AUDIO_FORMAT_VERSION,
                namespace: [0; 16],
                codec: AUDIO_CODEC_AC3,
                framing: AUDIO_FRAMING_AC3_SYNCFRAME,
                sample_rate: 48_000,
                channels,
                channel_mask: if channels == 2 { 3 } else { 63 },
                frame_samples: 1536,
                reserved: 0,
                config: BorrowedBytes::new(&[]),
            },
            layout: layout(channels),
            fixed_packet_bytes: 4,
            reserved: 0,
        },
    };
    unsafe {
        *generation = 101;
        (sink.write)(sink.context, &offer)
    }
}
unsafe extern "C" fn configure(
    context: *mut c_void,
    stream: u64,
    generation: u64,
    id: u64,
    input: *const AudioInputSelectionV2,
) -> Status {
    if context.is_null() || input.is_null() || (stream, generation, id) != (19, 101, 11) {
        return MEDIA_UNSUPPORTED;
    }
    let input = unsafe { *input };
    let channels = unsafe { (*context.cast::<Provider>()).channels };
    if input.layout != layout(channels) || input.stream != 20 || input.generation != 1 {
        return MEDIA_UNSUPPORTED;
    }
    unsafe { (*context.cast::<Provider>()).input = Some(input) };
    STATUS_OK
}
unsafe extern "C" fn submit(
    context: *mut c_void,
    stream: u64,
    frame: *const OrderedAudioFrameV2,
) -> Status {
    if context.is_null() || frame.is_null() || stream != 19 {
        return STATUS_INVALID_ARGUMENT;
    }
    let provider = unsafe { &mut *context.cast::<Provider>() };
    let frame = unsafe { &*frame };
    if provider
        .input
        .as_ref()
        .is_none_or(|input| input.validate_frame(frame).is_err())
        || provider.draining
    {
        return MEDIA_UNSUPPORTED;
    }
    if provider.pending_timestamp.is_some() {
        return MEDIA_BUSY;
    }
    let start = provider
        .buffered_timestamp
        .unwrap_or(frame.frame.timestamp_us);
    if provider.buffered_frames > 0
        && start + u64::from(provider.buffered_frames) * 1_000_000 / 48_000
            != frame.frame.timestamp_us
    {
        return MEDIA_UNSUPPORTED;
    }
    let total = provider.buffered_frames + frame.frame.frames;
    if total >= 3072 {
        return MEDIA_UNSUPPORTED;
    }
    if total >= 1536 {
        provider.pending_timestamp = Some(start);
        provider.buffered_frames = total - 1536;
        provider.buffered_timestamp = (provider.buffered_frames != 0).then_some(start + 32_000);
    } else {
        provider.buffered_frames = total;
        provider.buffered_timestamp = Some(start);
    }
    STATUS_OK
}
unsafe extern "C" fn receive(
    context: *mut c_void,
    stream: u64,
    sink: NegotiatedAudioPacketSink,
) -> Status {
    if context.is_null() || stream != 19 {
        return STATUS_INVALID_ARGUMENT;
    }
    let provider = unsafe { &mut *context.cast::<Provider>() };
    let (flags, frames, timestamp, bytes): (_, _, _, &[u8]) = match provider.pending_timestamp {
        Some(timestamp) => (0, 1536, timestamp, &[0xa1, 0x11, 0x33, 0x44]),
        None if provider.draining && provider.buffered_frames != 0 => {
            provider.pending_timestamp = provider.buffered_timestamp.take();
            provider.buffered_frames = 0;
            (
                0,
                1536,
                provider.pending_timestamp.unwrap(),
                &[0xa1, 0x11, 0x33, 0x44],
            )
        }
        None if provider.draining && !provider.ended => (AUDIO_PACKET_END_OF_STREAM, 0, 0, &[]),
        None => return MEDIA_PENDING,
    };
    let packet = NegotiatedAudioPacket {
        stream,
        generation: 101,
        offer_id: 11,
        flags,
        frames,
        timestamp_us: timestamp,
        sequence: provider.sequence,
        data: BorrowedBytes::new(bytes),
    };
    let status = unsafe { (sink.write)(sink.context, &packet) };
    if status == STATUS_OK {
        if flags == AUDIO_PACKET_END_OF_STREAM {
            provider.ended = true;
        } else {
            provider.pending_timestamp = None;
            provider.sequence += 1;
        }
    }
    status
}
unsafe extern "C" fn drain(context: *mut c_void, stream: u64) -> Status {
    if context.is_null() || stream != 19 {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe {
        (*context.cast::<Provider>()).draining = true;
    }
    STATUS_OK
}
#[unsafe(no_mangle)]
pub extern "C" fn fixture_create_v2() -> *mut OrderedAudioEncoderApiV2 {
    fixture_create_v2_channels(2)
}
#[unsafe(no_mangle)]
pub extern "C" fn fixture_create_v2_channels(channels: u32) -> *mut OrderedAudioEncoderApiV2 {
    if !matches!(channels, 2 | 6) {
        return std::ptr::null_mut();
    }
    let mut provider = Box::new(Provider {
        api: OrderedAudioEncoderApiV2 {
            base: NativeInterface {
                header: AbiHeader {
                    size: size_of::<OrderedAudioEncoderApiV2>() as u32,
                    major: 2,
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
        channels,
        input: None,
        pending_timestamp: None,
        buffered_frames: 0,
        buffered_timestamp: None,
        sequence: 0,
        draining: false,
        ended: false,
    });
    provider.api.base.context = (&mut *provider as *mut Provider).cast();
    Box::into_raw(provider).cast()
}
/// # Safety
/// Pointer was returned by fixture_create_v2 and all bindings have retired.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_destroy_v2(pointer: *mut OrderedAudioEncoderApiV2) {
    if !pointer.is_null() {
        drop(unsafe { Box::from_raw(pointer.cast::<Provider>()) });
    }
}
