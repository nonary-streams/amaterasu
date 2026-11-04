//! Synchronous GPU processor consumer, shared by built-in and external plugins.
use crate::abi::{STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use amaterasu_media::{
    DmaBufFrameLease, DmaBufLeaseSink, GPU_PROCESSOR_API_V1_2_SIZE, GPU_PROCESSOR_API_V1_3_SIZE,
    GPU_PROCESSOR_CONFIGURATION_INTERFACE_MINOR, GPU_PROCESSOR_INTERFACE_MAJOR,
    GPU_PROCESSOR_MAX_CONFIGURATION_BYTES, GPU_PROCESSOR_UPDATE_INTERFACE_MINOR, GpuProcessorApi,
    GpuProcessorApiV1_2, GpuProcessorApiV1_3, MEDIA_BUSY, MEDIA_UNSUPPORTED,
};
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

/// Configure an advertised v1.2 processor before metadata queries and frames.
/// The provider consumes the bounded JSON synchronously, without retaining it.
/// If the provider rejects the call, this adapter cancels the stream before
/// returning, retiring any resources it may have reserved. The caller must not
/// retry or cancel that stream again. Invalid input or an unsupported interface
/// is rejected before dispatch and does not require cancellation.
///
/// # Safety
/// `api` must be the prefix of a live GPU processor binding, and an advertised
/// v1.2 table must occupy its complete declared size for this call.
pub unsafe fn configure_gpu_stream(
    api: &GpuProcessorApi,
    stream: u64,
    json: &[u8],
) -> Result<(), Status> {
    if json.is_empty()
        || json.len() > GPU_PROCESSOR_MAX_CONFIGURATION_BYTES
        || serde_json::from_slice::<serde_json::Value>(json).is_err()
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let header = api.base.header;
    if header.major != GPU_PROCESSOR_INTERFACE_MAJOR
        || header.minor < GPU_PROCESSOR_CONFIGURATION_INTERFACE_MINOR
        || header.size < GPU_PROCESSOR_API_V1_2_SIZE
    {
        return Err(MEDIA_UNSUPPORTED);
    }
    // SAFETY: caller promises the advertised complete table and live binding.
    let table = unsafe { &*std::ptr::from_ref(api).cast::<GpuProcessorApiV1_2>() };
    let status =
        unsafe { (table.configure_stream)(api.base.context, stream, json.as_ptr(), json.len()) };
    if status == STATUS_OK {
        Ok(())
    } else {
        // SAFETY: a rejected configuration may have reserved provider state;
        // the live binding's cancellation callback synchronously retires it.
        unsafe { (api.cancel_dmabuf_leases)(api.base.context, stream) };
        Err(status)
    }
}

/// Atomically replace settings for a configured stream. A rejected update
/// leaves the stream active with its prior settings; this helper never cancels.
/// Calls must be serialized with frame processing and stream retirement.
///
/// # Safety
/// `api` must be a live binding obtained with the GPU processor's typed
/// contract identity, and its advertised v1.3 table must be complete. Table
/// version and size alone cannot establish interface identity.
pub unsafe fn update_gpu_stream(
    api: &GpuProcessorApi,
    stream: u64,
    json: &[u8],
) -> Result<(), Status> {
    if json.is_empty()
        || json.len() > GPU_PROCESSOR_MAX_CONFIGURATION_BYTES
        || serde_json::from_slice::<serde_json::Value>(json).is_err()
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let header = api.base.header;
    if header.major != GPU_PROCESSOR_INTERFACE_MAJOR
        || header.minor < GPU_PROCESSOR_UPDATE_INTERFACE_MINOR
        || header.size < GPU_PROCESSOR_API_V1_3_SIZE
    {
        return Err(MEDIA_UNSUPPORTED);
    }
    // SAFETY: caller guarantees the advertised complete table and live binding.
    let table = unsafe { &*std::ptr::from_ref(api).cast::<GpuProcessorApiV1_3>() };
    let status =
        unsafe { (table.update_stream)(api.base.context, stream, json.as_ptr(), json.len()) };
    if status == STATUS_OK {
        Ok(())
    } else {
        Err(status)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProcessingOutcome {
    Accepted,
    Busy,
    Failed(Status),
    ProtocolViolation,
}

/// A synchronous reference whose release necessarily occurs before the caller
/// can drop the native binding. This guard intentionally cannot escape a pump.
pub struct ScopedGpuFrame<'a> {
    lease: &'a DmaBufFrameLease,
}

impl<'a> ScopedGpuFrame<'a> {
    /// # Safety
    /// The provider binding and borrowed owner must outlive this guard.
    pub unsafe fn retain(lease: &'a DmaBufFrameLease) -> Result<Self, Status> {
        lease.validate()?;
        // SAFETY: validation checked the callback; caller retains its binding.
        let status = unsafe { lease.retain.unwrap()(lease.context) };
        if status == STATUS_OK {
            Ok(Self { lease })
        } else {
            Err(status)
        }
    }

    pub fn descriptor(&self) -> &DmaBufFrameLease {
        self.lease
    }
}

impl Drop for ScopedGpuFrame<'_> {
    fn drop(&mut self) {
        // SAFETY: one successful retain is released while its binding is live;
        // no asynchronous GPU operation is owned by this synchronous guard.
        unsafe { self.lease.release.unwrap()(self.lease.context, -1) };
    }
}

struct Delivery<'a, F> {
    submit: &'a mut F,
    sequence: u64,
    timestamp_us: u64,
    freshness: u32,
    count: usize,
    result: Status,
    invalid: bool,
}

unsafe extern "C" fn deliver<F>(
    context: *mut c_void,
    lease: *const DmaBufFrameLease,
    freshness: u32,
) -> Status
where
    F: FnMut(&DmaBufFrameLease) -> Status,
{
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: dispatch owns this stack context for the entire synchronous call.
    let delivery = unsafe { &mut *context.cast::<Delivery<'_, F>>() };
    delivery.count += 1;
    if delivery.count != 1 || lease.is_null() {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the trusted provider keeps the descriptor live during its callback.
    let lease = unsafe { &*lease };
    if lease.validate().is_err()
        || lease.frame.sequence != delivery.sequence
        || lease.frame.timestamp_us != delivery.timestamp_us
        || freshness != delivery.freshness
    {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: all provider bindings outlive this dispatch. Retention is local
    // and paired by Drop before returning to the provider.
    let retained = match unsafe { ScopedGpuFrame::retain(lease) } {
        Ok(retained) => retained,
        Err(status) => {
            delivery.result = status;
            return status;
        }
    };
    // External consumer code must not unwind through this extern-C callback.
    delivery.result = catch_unwind(AssertUnwindSafe(|| {
        (delivery.submit)(retained.descriptor())
    }))
    .unwrap_or(crate::abi::STATUS_INTERNAL_ERROR);
    delivery.result
}

/// Processes one GPU frame, allowing only one synchronous output with unchanged
/// capture identity. The downstream encoder retains its own reference before
/// returning if GPU reads will continue asynchronously.
///
/// # Safety
/// Input ownership callbacks and both native bindings must remain live for this
/// call. Neither this function nor its submit callback keeps a lease afterward.
pub unsafe fn process_gpu<F>(
    api: &GpuProcessorApi,
    stream: u64,
    lease: &DmaBufFrameLease,
    freshness: u32,
    mut submit: F,
) -> ProcessingOutcome
where
    F: FnMut(&DmaBufFrameLease) -> Status,
{
    if lease.validate().is_err()
        || !matches!(
            freshness,
            amaterasu_media::CAPTURE_FRAME_FRESH | amaterasu_media::CAPTURE_FRAME_CACHED_REUSE
        )
    {
        return ProcessingOutcome::Failed(STATUS_INVALID_ARGUMENT);
    }
    let mut delivery = Delivery {
        submit: &mut submit,
        sequence: lease.frame.sequence,
        timestamp_us: lease.frame.timestamp_us,
        freshness,
        count: 0,
        result: STATUS_OK,
        invalid: false,
    };
    let sink = DmaBufLeaseSink {
        context: (&mut delivery as *mut Delivery<'_, F>).cast(),
        write: deliver::<F>,
    };
    // SAFETY: lease, sink, callbacks and native bindings are live for the call.
    let status = unsafe { (api.process)(api.base.context, stream, lease, freshness, sink) };
    if delivery.invalid
        || delivery.count > 1
        || (delivery.count == 1 && status != delivery.result)
        || (delivery.count == 0 && status == STATUS_OK)
    {
        return ProcessingOutcome::ProtocolViolation;
    }
    if status == STATUS_OK {
        ProcessingOutcome::Accepted
    } else if status == MEDIA_BUSY {
        ProcessingOutcome::Busy
    } else {
        ProcessingOutcome::Failed(status)
    }
}
