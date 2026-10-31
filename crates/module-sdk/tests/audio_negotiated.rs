//! Two independent synthetic encoder implementations, exercised by one unchanged
//! SDK consumer. Bytes are fixtures, not valid AC-3/E-AC-3 bitstreams.
use amaterasu_media::*;
use amaterasu_module_sdk::{
    abi::*,
    audio::{self, AudioFormat, AudioNegotiationError},
};
use std::{ffi::c_void, mem::size_of};

fn format(codec: u32, framing: u32) -> AudioFormat {
    AudioFormat {
        namespace: [0; 16],
        codec,
        framing,
        sample_rate: 48_000,
        channels: 6,
        channel_mask: 0x3f,
        frame_samples: 1536,
        config: vec![1, 2, 3],
    }
}
struct Provider {
    selected: bool,
    reject_stale: bool,
    pending: bool,
    offers: u32,
    codec: u32,
    framing: u32,
}
unsafe extern "C" fn enumerate<const VARIANT: u32>(
    ctx: *mut c_void,
    _: u64,
    sink: AudioFormatOfferSink,
    generation: *mut u64,
) -> Status {
    let p = unsafe { &mut *ctx.cast::<Provider>() };
    p.offers += 1;
    let format = format(p.codec, p.framing);
    let offer = AudioFormatOffer {
        id: u64::from(VARIANT),
        format: format.descriptor(),
    };
    unsafe {
        *generation = 7;
        (sink.write)(sink.context, &offer)
    }
}
unsafe extern "C" fn configure<const VARIANT: u32>(
    ctx: *mut c_void,
    _: u64,
    generation: u64,
    id: u64,
) -> Status {
    let p = unsafe { &mut *ctx.cast::<Provider>() };
    if p.reject_stale || generation != 7 || id != u64::from(VARIANT) {
        return MEDIA_UNSUPPORTED;
    }
    p.selected = true;
    STATUS_OK
}
unsafe extern "C" fn submit(_: *mut c_void, _: u64, _: *const AudioFrame) -> Status {
    STATUS_OK
}
unsafe extern "C" fn receive<const VARIANT: u32>(
    ctx: *mut c_void,
    _: u64,
    sink: NegotiatedAudioPacketSink,
) -> Status {
    let p = unsafe { &mut *ctx.cast::<Provider>() };
    if !p.selected || !p.pending {
        return MEDIA_PENDING;
    }
    let data = [VARIANT as u8, 0x42];
    let packet = NegotiatedAudioPacket {
        stream: 1,
        generation: 7,
        offer_id: u64::from(VARIANT),
        flags: 0,
        frames: 1536,
        timestamp_us: 42,
        sequence: 1,
        data: BorrowedBytes::new(&data),
    };
    let status = unsafe { (sink.write)(sink.context, &packet) };
    if status == STATUS_OK {
        p.pending = false;
    }
    status
}
unsafe extern "C" fn drain(_: *mut c_void, _: u64) -> Status {
    STATUS_OK
}
fn table<const VARIANT: u32>(provider: &mut Provider) -> NegotiatedAudioEncoderApi {
    NegotiatedAudioEncoderApi {
        base: NativeInterface {
            header: AbiHeader {
                size: size_of::<NegotiatedAudioEncoderApi>() as u32,
                major: 1,
                minor: 0,
            },
            context: (provider as *mut Provider).cast(),
            release: None,
        },
        enumerate: enumerate::<VARIANT>,
        configure: configure::<VARIANT>,
        submit,
        receive: receive::<VARIANT>,
        drain,
    }
}
struct Receiver {
    selection: audio::AudioSelection,
    busy: bool,
    bytes: Vec<u8>,
}
unsafe extern "C" fn accept(ctx: *mut c_void, packet: *const NegotiatedAudioPacket) -> Status {
    let receiver = unsafe { &mut *ctx.cast::<Receiver>() };
    let packet = unsafe { &*packet };
    let bytes = match unsafe { receiver.selection.packet_bytes(packet) } {
        Ok(bytes) => bytes,
        Err(status) => return status,
    };
    if receiver.busy {
        return MEDIA_BUSY;
    }
    receiver.bytes.extend_from_slice(bytes);
    STATUS_OK
}
fn run_consumer(api: &NegotiatedAudioEncoderApi, expected: AudioFormat, variant: u8) {
    // SAFETY: fixture table and context remain live and use synchronous callbacks.
    let selection = unsafe { audio::negotiate(api, 1, &[expected]) }.unwrap();
    let mut receiver = Receiver {
        selection,
        busy: true,
        bytes: vec![],
    };
    let sink = NegotiatedAudioPacketSink {
        context: (&mut receiver as *mut Receiver).cast(),
        write: accept,
    };
    assert_eq!(
        unsafe { (api.receive)(api.base.context, 1, sink) },
        MEDIA_BUSY
    );
    receiver.busy = false;
    assert_eq!(
        unsafe { (api.receive)(api.base.context, 1, sink) },
        STATUS_OK
    );
    assert_eq!(receiver.bytes, [variant, 0x42]);
    assert_eq!(
        unsafe { (api.receive)(api.base.context, 1, sink) },
        MEDIA_PENDING
    );
}
#[test]
fn unchanged_consumer_negotiates_two_independent_encoders_and_retries() {
    let mut a = Provider {
        selected: false,
        reject_stale: false,
        pending: true,
        offers: 0,
        codec: AUDIO_CODEC_AC3,
        framing: AUDIO_FRAMING_AC3_SYNCFRAME,
    };
    let mut b = Provider {
        selected: false,
        reject_stale: false,
        pending: true,
        offers: 0,
        codec: AUDIO_CODEC_EAC3,
        framing: AUDIO_FRAMING_EAC3_SYNCFRAME,
    };
    let a_api = table::<1>(&mut a);
    let b_api = table::<2>(&mut b);
    run_consumer(
        &a_api,
        format(AUDIO_CODEC_AC3, AUDIO_FRAMING_AC3_SYNCFRAME),
        1,
    );
    run_consumer(
        &b_api,
        format(AUDIO_CODEC_EAC3, AUDIO_FRAMING_EAC3_SYNCFRAME),
        2,
    );
    assert_eq!((a.offers, b.offers), (1, 1));
}
#[test]
fn no_unoffered_fallback_or_stale_selection_and_packets_are_bound_to_offer() {
    let mut p = Provider {
        selected: false,
        reject_stale: false,
        pending: true,
        offers: 0,
        codec: AUDIO_CODEC_AC3,
        framing: AUDIO_FRAMING_AC3_SYNCFRAME,
    };
    let api = table::<1>(&mut p);
    let ac3 = format(AUDIO_CODEC_AC3, AUDIO_FRAMING_AC3_SYNCFRAME);
    let eac3 = format(AUDIO_CODEC_EAC3, AUDIO_FRAMING_EAC3_SYNCFRAME);
    assert_eq!(
        unsafe { audio::negotiate(&api, 1, &[eac3]) },
        Err(AudioNegotiationError::NoCompatibleFormat)
    );
    assert!(!p.selected);
    p.reject_stale = true;
    std::hint::black_box(&p);
    assert_eq!(
        unsafe { audio::negotiate(&api, 1, std::slice::from_ref(&ac3)) },
        Err(AudioNegotiationError::Stale)
    );
    assert!(!p.selected);
    let data = [0x42];
    let packet = NegotiatedAudioPacket {
        stream: 1,
        generation: 7,
        offer_id: 2,
        flags: 0,
        frames: 1536,
        timestamp_us: 0,
        sequence: 1,
        data: BorrowedBytes::new(&data),
    };
    assert_eq!(
        packet.validate_for(1, 7, 1, &ac3.descriptor()),
        Err(STATUS_INVALID_ARGUMENT)
    );
    assert_eq!(
        packet.validate_for(1, 8, 2, &ac3.descriptor()),
        Err(STATUS_INVALID_ARGUMENT)
    );
    assert_eq!(
        NegotiatedAudioPacket {
            frames: 2,
            ..packet
        }
        .validate_for(1, 7, 2, &ac3.descriptor()),
        Err(STATUS_INVALID_ARGUMENT)
    );
}

#[test]
fn negotiated_audio_bounds_layout_and_preserves_eos_rules() {
    let mut supported = format(AUDIO_CODEC_AC3, AUDIO_FRAMING_AC3_SYNCFRAME);
    let mut descriptor = supported.descriptor();
    assert!(descriptor.validate().is_ok());
    descriptor.channel_mask = 3;
    assert_eq!(descriptor.validate(), Err(STATUS_INVALID_ARGUMENT));
    supported.config = vec![0; AUDIO_FORMAT_MAX_CONFIG_BYTES + 1];
    assert_eq!(
        supported.descriptor().validate(),
        Err(STATUS_INVALID_ARGUMENT)
    );
    let valid = format(AUDIO_CODEC_EAC3, AUDIO_FRAMING_EAC3_SYNCFRAME);
    let eos = NegotiatedAudioPacket {
        stream: 1,
        generation: 7,
        offer_id: 2,
        flags: AUDIO_PACKET_END_OF_STREAM,
        frames: 0,
        timestamp_us: 123,
        sequence: 2,
        data: BorrowedBytes::new(&[]),
    };
    assert!(eos.validate_for(1, 7, 2, &valid.descriptor()).is_ok());
    assert_eq!(
        NegotiatedAudioPacket { stream: 2, ..eos }.validate_for(1, 7, 2, &valid.descriptor()),
        Err(STATUS_INVALID_ARGUMENT)
    );
    assert_eq!(
        NegotiatedAudioPacket {
            flags: AUDIO_PACKET_END_OF_STREAM | AUDIO_PACKET_DISCONTINUITY,
            ..eos
        }
        .validate_for(1, 7, 2, &valid.descriptor()),
        Err(STATUS_INVALID_ARGUMENT)
    );
    #[cfg(target_pointer_width = "64")]
    {
        assert_eq!(size_of::<AudioFormatDescriptor>(), 72);
        assert_eq!(size_of::<AudioFormatOffer>(), 80);
        assert_eq!(size_of::<NegotiatedAudioPacket>(), 64);
        assert_eq!(size_of::<NegotiatedAudioEncoderApi>(), 72);
    }
}
