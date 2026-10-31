//! One unchanged consumer artifact for both independently built providers.
use amaterasu_media::*;
use amaterasu_module_sdk::{
    abi::*,
    audio::{self, AudioFormat, AudioSelection},
};
use std::ffi::c_void;

struct Sink {
    selection: AudioSelection,
    busy: bool,
    result: Vec<u8>,
}
unsafe extern "C" fn receive(context: *mut c_void, packet: *const NegotiatedAudioPacket) -> Status {
    if context.is_null() || packet.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let state = unsafe { &mut *context.cast::<Sink>() };
    let packet = unsafe { &*packet };
    let Ok(bytes) = (unsafe { state.selection.packet_bytes(packet) }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    if state.busy {
        return MEDIA_BUSY;
    }
    state.result.extend_from_slice(bytes);
    STATUS_OK
}
/// # Safety
/// `api` must point to a live, trusted negotiated audio encoder table for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_consume(
    api: *const NegotiatedAudioEncoderApi,
    codec: u32,
    framing: u32,
    expected: u8,
) -> i32 {
    if api.is_null() {
        return 1;
    }
    let api = unsafe { &*api };
    let format = AudioFormat {
        namespace: [0; 16],
        codec,
        framing,
        sample_rate: 48_000,
        channels: 6,
        channel_mask: 0x3f,
        frame_samples: 1536,
        config: Vec::new(),
    };
    let Ok(selection) = (unsafe { audio::negotiate(api, 19, &[format]) }) else {
        return 2;
    };
    let mut state = Sink {
        selection,
        busy: true,
        result: Vec::new(),
    };
    let sink = NegotiatedAudioPacketSink {
        context: (&mut state as *mut Sink).cast(),
        write: receive,
    };
    if unsafe { (api.receive)(api.base.context, 19, sink) } != MEDIA_BUSY {
        return 3;
    }
    state.busy = false;
    if unsafe { (api.receive)(api.base.context, 19, sink) } != STATUS_OK {
        return 4;
    }
    if state.result.first().copied() != Some(expected) || state.result.len() != 2 {
        return 5;
    }
    if unsafe { (api.receive)(api.base.context, 19, sink) } != MEDIA_PENDING {
        return 6;
    }
    0
}
