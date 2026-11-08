use crate::abi::{STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use amaterasu_media::*;
fn validate(api: &FrameProcessorApi, stream: u64, json: &[u8]) -> Result<(), Status> {
    let header = api.base.base.base.header;
    if header.major != FRAME_PROCESSOR_INTERFACE_MAJOR
        || header.minor != FRAME_PROCESSOR_INTERFACE_MINOR
        || header.size < FRAME_PROCESSOR_API_SIZE
    {
        return Err(MEDIA_UNSUPPORTED);
    }
    if stream == 0
        || json.is_empty()
        || json.len() > FRAME_PROCESSOR_MAX_CONFIGURATION_BYTES
        || serde_json::from_slice::<serde_json::Value>(json).is_err()
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    Ok(())
}
/// # Safety
/// The exact typed processor binding and stream owner remain live throughout
/// this call and any cancellation needed after rejected configuration.
pub unsafe fn configure_frame_stream(
    api: &FrameProcessorApi,
    stream: u64,
    json: &[u8],
) -> Result<(), Status> {
    validate(api, stream, json)?;
    let context = api.base.base.base.context;
    // SAFETY: caller supplies the typed live binding and borrowed settings.
    let status = unsafe { (api.configure_stream)(context, stream, json.as_ptr(), json.len()) };
    if status == STATUS_OK {
        Ok(())
    } else {
        unsafe { (api.base.base.cancel)(context, stream) };
        Err(status)
    }
}
/// # Safety
/// The exact typed binding, configured stream and bytes remain live through
/// this serialized call. A rejected update leaves its previous settings active.
pub unsafe fn update_frame_stream(
    api: &FrameProcessorApi,
    stream: u64,
    json: &[u8],
) -> Result<(), Status> {
    validate(api, stream, json)?;
    let status = unsafe {
        (api.update_stream)(
            api.base.base.base.context,
            stream,
            json.as_ptr(),
            json.len(),
        )
    };
    if status == STATUS_OK {
        Ok(())
    } else {
        Err(status)
    }
}
