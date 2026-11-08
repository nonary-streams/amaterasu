//! Encoder table skeleton. Implement the hooks with an owned, bounded queue,
//! then register a factory with `audio::register_encoder` during module startup.
use amaterasu_media::{
    AudioEncoderApi, AudioFrame, AudioUnitSink, MEDIA_PENDING, MEDIA_UNSUPPORTED,
};
use amaterasu_module_sdk::abi::{AbiHeader, NativeInterface, Status};
use std::ffi::c_void;

unsafe extern "C" fn submit(_: *mut c_void, _: u64, _: *const AudioFrame) -> Status {
    // Copy input if retained. BUSY must leave the complete frame unaccepted.
    MEDIA_UNSUPPORTED
}

unsafe extern "C" fn receive(_: *mut c_void, _: u64, _: AudioUnitSink) -> Status {
    // Offer one queued unit through AudioOutput; retain it until accepted.
    MEDIA_PENDING
}

unsafe extern "C" fn drain(_: *mut c_void, _: u64) -> Status {
    // Stop accepting input, flush the queue, then deliver an end unit.
    MEDIA_UNSUPPORTED
}

fn main() {
    let table = AudioEncoderApi {
        base: NativeInterface {
            header: AbiHeader::of::<AudioEncoderApi>(),
            context: std::ptr::null_mut(),
            release: None,
        },
        submit,
        receive,
        drain,
    };
    assert_eq!(table.base.header.major, 1);
}
