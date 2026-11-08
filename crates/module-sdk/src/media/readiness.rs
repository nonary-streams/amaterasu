//! Advisory admission checks on already authenticated encoder bindings.
use crate::abi::{STATUS_OK, Status};
use amaterasu_media::{
    ENCODER_GPU_READINESS_INTERFACE_MINOR, EncoderApi, EncoderApiV1_4, INTERFACE_MAJOR, MEDIA_BUSY,
    MEDIA_PENDING, MEDIA_UNSUPPORTED,
};
use std::mem::size_of;

/// Returns an advisory readiness observation, or None for older providers.
/// An affirmative observation reserves nothing; handle BUSY on submission too.
///
/// # Safety
/// `api` must come from a live typed encoder binding owned by this caller, and
/// the binding and session must remain live through the synchronous query.
pub unsafe fn encoder_gpu_input_ready(
    api: &EncoderApi,
    session: u64,
) -> Result<Option<bool>, Status> {
    if api.base.header.major != INTERFACE_MAJOR
        || api.base.header.minor < ENCODER_GPU_READINESS_INTERFACE_MINOR
        || (api.base.header.size as usize) < size_of::<EncoderApiV1_4>()
    {
        return Ok(None);
    }
    // SAFETY: the caller guarantees a typed encoder table, and both its minor
    // and complete tail size were checked before reading this callback.
    let table = unsafe { &*(api as *const EncoderApi).cast::<EncoderApiV1_4>() };
    match unsafe { (table.gpu_input_ready)(api.base.context, session) } {
        STATUS_OK => Ok(Some(true)),
        MEDIA_BUSY | MEDIA_PENDING => Ok(Some(false)),
        MEDIA_UNSUPPORTED => Ok(None),
        status => Err(status),
    }
}
