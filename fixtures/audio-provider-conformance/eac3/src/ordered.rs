//! Independently implemented synthetic ordered E-AC-3 provider, not a codec.
use amaterasu_abi::*;
use amaterasu_media::*;
use std::{ffi::c_void, mem::size_of};

#[repr(C)]
struct Encoder {
    table: OrderedAudioEncoderApiV2,
    channels: u32,
    generation: u64,
    fixed_bytes: u32,
    busy_once: bool,
    busy_count: u32,
    pending_discontinuity: bool,
    selection: Option<AudioInputSelectionV2>,
    output: Option<(u64, u64)>,
    buffered_frames: u32,
    buffered_timestamp: Option<u64>,
    serial: u64,
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
unsafe extern "C" fn offers(
    context: *mut c_void,
    id: u64,
    sink: AudioFormatOfferSinkV2,
    generation: *mut u64,
) -> Status {
    if context.is_null() || id != 19 || generation.is_null() {
        return MEDIA_UNSUPPORTED;
    }
    let provider = unsafe { &*context.cast::<Encoder>() };
    let channels = provider.channels;
    let desc = AudioFormatDescriptorV2 {
        size: size_of::<AudioFormatDescriptorV2>() as u32,
        version: AUDIO_ORDERED_VERSION,
        format: AudioFormatDescriptor {
            size: size_of::<AudioFormatDescriptor>() as u32,
            version: AUDIO_FORMAT_VERSION,
            namespace: [0; 16],
            codec: AUDIO_CODEC_EAC3,
            framing: AUDIO_FRAMING_EAC3_SYNCFRAME,
            sample_rate: 48_000,
            channels,
            channel_mask: if channels == 2 { 3 } else { 63 },
            frame_samples: 1536,
            reserved: 0,
            config: BorrowedBytes::new(&[]),
        },
        layout: layout(channels),
        fixed_packet_bytes: provider.fixed_bytes,
        reserved: 0,
    };
    unsafe {
        *generation = provider.generation;
        (sink.write)(
            sink.context,
            &AudioFormatOfferV2 {
                id: 22,
                format: desc,
            },
        )
    }
}
unsafe extern "C" fn select(
    context: *mut c_void,
    stream: u64,
    generation: u64,
    id: u64,
    selection: *const AudioInputSelectionV2,
) -> Status {
    if context.is_null()
        || selection.is_null()
        || (stream, generation, id) != (19, unsafe { (*context.cast::<Encoder>()).generation }, 22)
    {
        return MEDIA_UNSUPPORTED;
    }
    let selected = unsafe { *selection };
    let channels = unsafe { (*context.cast::<Encoder>()).channels };
    if selected.layout != layout(channels) || selected.stream != 20 || selected.generation != 1 {
        return MEDIA_UNSUPPORTED;
    }
    unsafe { (*context.cast::<Encoder>()).selection = Some(selected) };
    STATUS_OK
}
unsafe extern "C" fn accept(
    context: *mut c_void,
    stream: u64,
    frame: *const OrderedAudioFrameV2,
) -> Status {
    if context.is_null() || frame.is_null() || stream != 19 {
        return STATUS_INVALID_ARGUMENT;
    }
    let encoder = unsafe { &mut *context.cast::<Encoder>() };
    if encoder.busy_once {
        encoder.busy_once = false;
        encoder.busy_count += 1;
        return MEDIA_BUSY;
    }
    let frame = unsafe { &*frame };
    if encoder.draining
        || encoder
            .selection
            .as_ref()
            .is_none_or(|selection| selection.validate_frame(frame).is_err())
    {
        return MEDIA_UNSUPPORTED;
    }
    if encoder.output.is_some() {
        return MEDIA_BUSY;
    }
    if frame.frame.flags & AUDIO_DISCONTINUITY != 0 {
        if encoder.buffered_frames != 0 {
            return MEDIA_UNSUPPORTED;
        }
        encoder.pending_discontinuity = true;
    }
    let start = encoder
        .buffered_timestamp
        .unwrap_or(frame.frame.timestamp_us);
    if encoder.buffered_frames > 0
        && start + u64::from(encoder.buffered_frames) * 1_000_000 / 48_000
            != frame.frame.timestamp_us
    {
        return MEDIA_UNSUPPORTED;
    }
    let total = encoder.buffered_frames + frame.frame.frames;
    if total >= 3072 {
        return MEDIA_UNSUPPORTED;
    }
    if total >= 1536 {
        encoder.output = Some((start, encoder.serial));
        encoder.serial += 1;
        encoder.buffered_frames = total - 1536;
        encoder.buffered_timestamp = (encoder.buffered_frames != 0).then_some(start + 32_000);
    } else {
        encoder.buffered_frames = total;
        encoder.buffered_timestamp = Some(start);
    }
    STATUS_OK
}
unsafe extern "C" fn take(
    context: *mut c_void,
    stream: u64,
    sink: NegotiatedAudioPacketSink,
) -> Status {
    if context.is_null() || stream != 19 {
        return STATUS_INVALID_ARGUMENT;
    }
    let encoder = unsafe { &mut *context.cast::<Encoder>() };
    let (flags, frames, timestamp, sequence, payload): (_, _, _, _, &[u8]) = match encoder.output {
        Some((timestamp, sequence)) => (
            if encoder.pending_discontinuity {
                AUDIO_PACKET_DISCONTINUITY
            } else {
                0
            },
            1536,
            timestamp,
            sequence,
            &[0xe2, 0x22, 0x66, 0x77],
        ),
        None if encoder.draining && encoder.buffered_frames != 0 => {
            let timestamp = encoder.buffered_timestamp.take().unwrap();
            let sequence = encoder.serial;
            encoder.serial += 1;
            encoder.buffered_frames = 0;
            encoder.output = Some((timestamp, sequence));
            (0, 1536, timestamp, sequence, &[0xe2, 0x22, 0x66, 0x77])
        }
        None if encoder.draining && !encoder.ended => {
            (AUDIO_PACKET_END_OF_STREAM, 0, 0, encoder.serial, &[])
        }
        None => return MEDIA_PENDING,
    };
    let packet = NegotiatedAudioPacket {
        stream,
        generation: encoder.generation,
        offer_id: 22,
        flags,
        frames,
        timestamp_us: timestamp,
        sequence,
        data: BorrowedBytes::new(payload),
    };
    let result = unsafe { (sink.write)(sink.context, &packet) };
    if result == STATUS_OK {
        if flags == AUDIO_PACKET_END_OF_STREAM {
            encoder.ended = true;
        } else {
            encoder.output = None;
            encoder.pending_discontinuity = false;
        }
    }
    result
}
unsafe extern "C" fn finish(context: *mut c_void, stream: u64) -> Status {
    if context.is_null() || stream != 19 {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe {
        (*context.cast::<Encoder>()).draining = true;
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
    let mut encoder = Box::new(Encoder {
        table: OrderedAudioEncoderApiV2 {
            base: NativeInterface {
                header: AbiHeader {
                    size: size_of::<OrderedAudioEncoderApiV2>() as u32,
                    major: 2,
                    minor: 0,
                },
                context: std::ptr::null_mut(),
                release: None,
            },
            enumerate: offers,
            configure: select,
            submit: accept,
            receive: take,
            drain: finish,
        },
        channels,
        generation: 202,
        fixed_bytes: 4,
        busy_once: false,
        busy_count: 0,
        pending_discontinuity: false,
        selection: None,
        output: None,
        buffered_frames: 0,
        buffered_timestamp: None,
        serial: 0,
        draining: false,
        ended: false,
    });
    encoder.table.base.context = (&mut *encoder as *mut Encoder).cast();
    Box::into_raw(encoder).cast()
}
/// # Safety
/// Pointer comes from fixture_create_v2 and no provider calls remain.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_destroy_v2(pointer: *mut OrderedAudioEncoderApiV2) {
    if !pointer.is_null() {
        drop(unsafe { Box::from_raw(pointer.cast::<Encoder>()) });
    }
}

/// # Safety
/// The fixture table must be exclusively owned and not bound during mutation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_set_offer_v2(
    pointer: *mut OrderedAudioEncoderApiV2,
    generation: u64,
    fixed_bytes: u32,
) {
    let provider = unsafe { &mut *pointer.cast::<Encoder>() };
    provider.generation = generation;
    provider.fixed_bytes = fixed_bytes;
}

/// # Safety
/// The fixture table is live and no native calls are in progress.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_busy_once_v2(pointer: *mut OrderedAudioEncoderApiV2) {
    unsafe {
        (*pointer.cast::<Encoder>()).busy_once = true;
    }
}
/// # Safety
/// The fixture table is live and no native calls are in progress.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_busy_count_v2(pointer: *mut OrderedAudioEncoderApiV2) -> u32 {
    unsafe { (*pointer.cast::<Encoder>()).busy_count }
}
