//! Optional timing queries on already authenticated, typed native bindings.
use crate::abi::{NativeInterface, STATUS_OK, Status};
use amaterasu_media::*;
use std::{
    ffi::c_void,
    mem::{MaybeUninit, size_of},
};

type Query = unsafe extern "C" fn(*mut c_void, u64, *mut EncodeTimingSnapshotV1) -> Status;

fn supports(base: &NativeInterface, minor: u32, size: usize) -> bool {
    base.header.major == INTERFACE_MAJOR
        && base.header.minor >= minor
        && base.header.size as usize >= size
}

unsafe fn read(
    base: &NativeInterface,
    session: u64,
    query: Query,
) -> Result<Option<EncodeTimingSnapshotV1>, Status> {
    let mut output = MaybeUninit::uninit();
    // SAFETY: caller retains this typed provider and its session during the call;
    // output is writable and read only when the provider reports initialization.
    match unsafe { query(base.context, session, output.as_mut_ptr()) } {
        STATUS_OK => {
            let snapshot = unsafe { output.assume_init() };
            snapshot.validate()?;
            Ok(Some(snapshot))
        }
        MEDIA_UNSUPPORTED | MEDIA_PENDING | MEDIA_BUSY => Ok(None),
        status => Err(status),
    }
}

/// # Safety
/// `api` belongs to a live ENCODER typed binding obeying its advertised layout;
/// its caller owns `session`. Retain the binding through this synchronous call.
pub unsafe fn encoder_timing(
    api: &EncoderApi,
    session: u64,
) -> Result<Option<EncodeTimingSnapshotV1>, Status> {
    if !supports(
        &api.base,
        ENCODER_TIMING_INTERFACE_MINOR,
        size_of::<EncoderApiV1_3>(),
    ) {
        return Ok(None);
    }
    let table = unsafe { &*(api as *const EncoderApi).cast::<EncoderApiV1_3>() };
    unsafe { read(&api.base, session, table.encode_timing) }
}

/// # Safety
/// The live ENCODER_PROVIDER typed binding advertises a valid table, and its
/// authenticated caller owns `session`. No reference is retained after return.
pub unsafe fn provider_encode_timing(
    api: &EncoderProviderApi,
    session: u64,
) -> Result<Option<EncodeTimingSnapshotV1>, Status> {
    if !supports(
        &api.base,
        ENCODER_PROVIDER_TIMING_INTERFACE_MINOR,
        size_of::<EncoderProviderApiV1_1>(),
    ) {
        return Ok(None);
    }
    let table = unsafe { &*(api as *const EncoderProviderApi).cast::<EncoderProviderApiV1_1>() };
    unsafe { read(&api.base, session, table.encode_timing) }
}

/// # Safety
/// `api` is from the selected live video typed binding and the caller owns the
/// stream. The full table must obey its advertised version, size and lifetime.
pub unsafe fn video_encode_timing(
    api: &VideoApi,
    stream: u64,
) -> Result<Option<EncodeTimingSnapshotV1>, Status> {
    if !supports(
        &api.base,
        VIDEO_TIMING_INTERFACE_MINOR,
        size_of::<VideoApiV1_4>(),
    ) {
        return Ok(None);
    }
    let table = unsafe { &*(api as *const VideoApi).cast::<VideoApiV1_4>() };
    unsafe { read(&api.base, stream, table.encode_timing) }
}

/// Reject a backwards or contradictory cumulative update within one epoch.
pub fn timing_follows(current: EncodeTimingSnapshotV1, previous: EncodeTimingSnapshotV1) -> bool {
    if current.validate().is_err() || previous.validate().is_err() {
        return false;
    }
    if current.epoch != previous.epoch {
        return true;
    }
    let (Some(count), Some(total)) = (
        current
            .completed_frames
            .checked_sub(previous.completed_frames),
        current.total_us.checked_sub(previous.total_us),
    ) else {
        return false;
    };
    if count == 0 {
        return current == previous;
    }
    (previous.completed_frames == 0
        || (current.min_us <= previous.min_us && current.max_us >= previous.max_us))
        && u128::from(total) <= u128::from(count) * u128::from(MAX_ENCODE_LATENCY_US)
}
