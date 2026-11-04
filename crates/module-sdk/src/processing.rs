//! Host-invoked GPU effects. The SDK owns ABI dispatch and delivery; effects
//! implement frame processing and synchronous retirement, without driving video.
use crate::{ModuleResult, RuntimeClient, abi::*, media::native_contracts};
use amaterasu_media::*;
use std::{
    cell::RefCell,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};
mod consumer;
pub use consumer::{
    ProcessingOutcome, ScopedGpuFrame, configure_gpu_stream, process_gpu, update_gpu_stream,
};

/// A synchronous borrow. Plane descriptors and pixels belong to the input
/// provider and must not be closed, modified, or used after `process` returns.
pub struct GpuFrame<'a> {
    lease: &'a DmaBufFrameLease,
    freshness: u32,
}

impl GpuFrame<'_> {
    pub fn frame(&self) -> &DmaBufFrame {
        &self.lease.frame
    }
    pub fn freshness(&self) -> u32 {
        self.freshness
    }
}

/// One provider-owned output reference. The SDK delivers it, then releases its
/// initial reference. Consumers explicitly retain their own references.
pub struct GpuOutputFrame {
    lease: DmaBufFrameLease,
}

impl GpuOutputFrame {
    /// # Safety
    /// Transfers exactly one live reference. Callbacks and storage must obey
    /// the GPU lease contract and outlive all references and completion fences.
    /// Output writes must already be complete or covered by acquire fences.
    /// The provider must remain loaded until consumers retire their references.
    pub unsafe fn from_owned_lease(lease: DmaBufFrameLease) -> Result<Self, Status> {
        // Even an invalid descriptor transfers its reference, so release it on
        // validation failure when the mandatory callback is present.
        if let Err(status) = lease.validate() {
            if let Some(release) = lease.release {
                // SAFETY: constructor's ownership contract transfers a live reference.
                unsafe { release(lease.context, -1) };
            }
            return Err(status);
        }
        Ok(Self { lease })
    }
}

impl Drop for GpuOutputFrame {
    fn drop(&mut self) {
        // SAFETY: construction transferred one reference; delivery only borrows it.
        unsafe { self.lease.release.expect("validated output")(self.lease.context, -1) };
    }
}

/// An effect is invoked by the selected consumer. Finish every borrowed-input
/// access before returning. No asynchronous callback or unbounded queue is
/// supplied by this interface. The binding is local to its owning module thread.
pub trait GpuFrameProcessor: 'static {
    /// Configure one stream before metadata and frames. JSON is synchronously
    /// borrowed; providers copy any settings needed after this call. Providers
    /// enforce their stream admission, replacement and retirement policy.
    fn configure_stream(&mut self, _stream: u64, _json: &[u8]) -> Result<(), Status> {
        Err(MEDIA_UNSUPPORTED)
    }
    /// Atomically replace the active settings of a configured stream. Rejection
    /// must leave its prior settings usable. The negotiated output format and
    /// HDR metadata must remain unchanged. Copy settings needed after return.
    fn update_stream(&mut self, _stream: u64, _json: &[u8]) -> Result<(), Status> {
        Err(MEDIA_UNSUPPORTED)
    }
    fn process(&mut self, stream: u64, input: GpuFrame<'_>) -> Result<GpuOutputFrame, Status>;
    /// Retire this stream's GPU work and references synchronously. Downstream
    /// consumers must have retired their retained output references first.
    fn cancel(&mut self, stream: u64);
    /// Retire every stream before the binding or plugin can be destroyed.
    fn shutdown(&mut self);
    /// Known output metadata; None preserves the older unspecified behavior.
    fn hdr_metadata(&self, _stream: u64) -> Option<HdrMetadata> {
        None
    }
}

#[repr(C)]
struct Binding<P> {
    api: GpuProcessorApiV1_3,
    processor: RefCell<P>,
}

/// Transfers an effect to a runtime-owned table. The runtime releases it once,
/// after ending calls and retiring downstream leases. Useful for external
/// providers as well as built-ins; no effect-specific runtime support is needed.
pub fn into_gpu_binding<P: GpuFrameProcessor>(processor: P) -> *const GpuProcessorApi {
    let mut binding = Box::new(Binding {
        api: GpuProcessorApiV1_3 {
            base: GpuProcessorApiV1_2 {
                base: GpuProcessorApiV1_1 {
                    base: GpuProcessorApi {
                        base: NativeInterface {
                            header: AbiHeader {
                                major: GPU_PROCESSOR_INTERFACE_MAJOR,
                                minor: GPU_PROCESSOR_UPDATE_INTERFACE_MINOR,
                                size: GPU_PROCESSOR_API_V1_3_SIZE,
                            },
                            context: std::ptr::null_mut(),
                            release: Some(release::<P>),
                        },
                        process: process::<P>,
                        cancel_dmabuf_leases: cancel::<P>,
                    },
                    hdr_metadata: metadata::<P>,
                },
                configure_stream: configure::<P>,
            },
            update_stream: update::<P>,
        },
        processor: RefCell::new(processor),
    });
    binding.api.base.base.base.base.context = (&mut *binding as *mut Binding<P>).cast();
    Box::into_raw(binding).cast()
}

unsafe extern "C" fn update<P: GpuFrameProcessor>(
    context: *mut c_void,
    stream: u64,
    json: *const u8,
    len: usize,
) -> Status {
    guard(|| {
        if context.is_null()
            || json.is_null()
            || len == 0
            || len > GPU_PROCESSOR_MAX_CONFIGURATION_BYTES
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller supplies readable borrowed bytes for this synchronous call.
        let json = unsafe { std::slice::from_raw_parts(json, len) };
        if serde_json::from_slice::<serde_json::Value>(json).is_err() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the binding is live and points to the SDK-owned context.
        let binding = unsafe { &*context.cast::<Binding<P>>() };
        let Ok(mut processor) = binding.processor.try_borrow_mut() else {
            return MEDIA_BUSY;
        };
        match processor.update_stream(stream, json) {
            Ok(()) => STATUS_OK,
            Err(status) if status != STATUS_OK => status,
            Err(_) => STATUS_INTERNAL_ERROR,
        }
    })
}

unsafe extern "C" fn configure<P: GpuFrameProcessor>(
    context: *mut c_void,
    stream: u64,
    json: *const u8,
    len: usize,
) -> Status {
    guard(|| {
        if context.is_null()
            || json.is_null()
            || len == 0
            || len > GPU_PROCESSOR_MAX_CONFIGURATION_BYTES
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller supplies readable borrowed bytes for this synchronous call.
        let json = unsafe { std::slice::from_raw_parts(json, len) };
        if serde_json::from_slice::<serde_json::Value>(json).is_err() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the binding is live and points to the SDK-owned context.
        let binding = unsafe { &*context.cast::<Binding<P>>() };
        let Ok(mut processor) = binding.processor.try_borrow_mut() else {
            return MEDIA_BUSY;
        };
        match processor.configure_stream(stream, json) {
            Ok(()) => STATUS_OK,
            Err(status) if status != STATUS_OK => status,
            Err(_) => STATUS_INTERNAL_ERROR,
        }
    })
}

unsafe extern "C" fn metadata<P: GpuFrameProcessor>(
    context: *mut c_void,
    stream: u64,
    output: *mut HdrMetadata,
) -> Status {
    guard(|| {
        if context.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: native binding and writable output remain live for this call.
        let binding = unsafe { &*context.cast::<Binding<P>>() };
        let Ok(processor) = binding.processor.try_borrow() else {
            return MEDIA_BUSY;
        };
        match processor.hdr_metadata(stream) {
            Some(metadata) if metadata.enabled <= 1 && metadata.reserved == 0 => {
                unsafe {
                    *output = metadata;
                }
                STATUS_OK
            }
            Some(_) => MEDIA_BACKEND_ERROR,
            None => MEDIA_UNSUPPORTED,
        }
    })
}

fn guard(operation: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(STATUS_INTERNAL_ERROR)
}

fn barrier(operation: impl FnOnce()) {
    // Returning from a failed retirement would let the runtime unload code
    // still referenced by GPU work. A void cancellation cannot report failure.
    if catch_unwind(AssertUnwindSafe(operation)).is_err() {
        std::process::abort();
    }
}

unsafe extern "C" fn process<P: GpuFrameProcessor>(
    context: *mut c_void,
    stream: u64,
    input: *const DmaBufFrameLease,
    freshness: u32,
    sink: DmaBufLeaseSink,
) -> Status {
    guard(|| {
        if context.is_null()
            || input.is_null()
            || !matches!(freshness, CAPTURE_FRAME_FRESH | CAPTURE_FRAME_CACHED_REUSE)
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the native caller supplies a live table context and descriptor
        // for this synchronous call. No borrowed pointer survives the invocation.
        let (binding, input) = unsafe { (&*context.cast::<Binding<P>>(), &*input) };
        if let Err(status) = input.validate() {
            return status;
        }
        let Ok(mut processor) = binding.processor.try_borrow_mut() else {
            return MEDIA_BUSY;
        };
        let output = match processor.process(
            stream,
            GpuFrame {
                lease: input,
                freshness,
            },
        ) {
            Ok(output) => output,
            Err(status) => return status,
        };
        if output.lease.frame.sequence != input.frame.sequence
            || output.lease.frame.timestamp_us != input.frame.timestamp_us
        {
            return MEDIA_BACKEND_ERROR;
        }
        // SAFETY: the output owns its reference throughout delivery. The sink
        // may retain a separate reference; its rejected status passes unchanged.
        unsafe { sink.send(&output.lease, freshness) }
    })
}

unsafe extern "C" fn cancel<P: GpuFrameProcessor>(context: *mut c_void, stream: u64) {
    barrier(|| {
        // SAFETY: a bound native table supplies its live context until release.
        let binding = unsafe { &*context.cast::<Binding<P>>() };
        binding.processor.borrow_mut().cancel(stream);
    });
}

unsafe extern "C" fn release<P: GpuFrameProcessor>(context: *mut c_void) {
    barrier(|| {
        // SAFETY: the runtime transfers its one binding back, after ending calls.
        let binding = unsafe { Box::from_raw(context.cast::<Binding<P>>()) };
        binding.processor.borrow_mut().shutdown();
    });
}

/// A factory creates independent state for each authenticated consumer binding.
/// The principal comes from the runtime, never from a frame or plugin setting.
pub struct GpuProcessorFactory<F> {
    create: F,
}

impl<F, P> GpuProcessorFactory<F>
where
    F: Fn(&str) -> Result<P, Status> + Send + Sync,
    P: GpuFrameProcessor,
{
    pub fn new(create: F) -> Box<Self> {
        Box::new(Self { create })
    }

    /// # Safety
    /// Keep this boxed factory and the provider library alive until every
    /// consumer binding is released. Signed dependencies establish this order.
    pub unsafe fn register(&self, runtime: &RuntimeClient, service: &str) -> ModuleResult<()> {
        let factory = NativeServiceFactory {
            header: AbiHeader::of::<NativeServiceFactory>(),
            context: std::ptr::from_ref(self).cast_mut().cast(),
            bind: Some(bind::<F, P>),
        };
        // SAFETY: caller establishes the factory and library lifetime.
        unsafe { runtime.register_native_typed(service, native_contracts::GPU_PROCESSOR, &factory) }
    }
}

unsafe extern "C" fn bind<F, P>(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status
where
    F: Fn(&str) -> Result<P, Status> + Send + Sync,
    P: GpuFrameProcessor,
{
    guard(|| {
        if output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime provides writable output and readable query during bind.
        unsafe {
            *output = std::ptr::null();
        }
        if context.is_null() || query.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let query = unsafe { &*query };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != GPU_PROCESSOR_INTERFACE_MAJOR
            || query.min_minor > GPU_PROCESSOR_UPDATE_INTERFACE_MINOR
            || query.min_size > GPU_PROCESSOR_API_V1_3_SIZE
        {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: runtime owns the authenticated principal for this call.
        let principal = match unsafe { principal.read() }
            .ok()
            .and_then(|p| std::str::from_utf8(p).ok())
        {
            Some(principal) if !principal.is_empty() => principal,
            _ => return STATUS_INVALID_ARGUMENT,
        };
        // SAFETY: registration retains this boxed factory through all binds.
        let factory = unsafe { &*context.cast::<GpuProcessorFactory<F>>() };
        match (factory.create)(principal) {
            Ok(processor) => {
                // SAFETY: output transfers one SDK-owned binding to the runtime.
                unsafe {
                    *output = into_gpu_binding(processor).cast();
                }
                STATUS_OK
            }
            Err(status) => status,
        }
    })
}

#[cfg(test)]
mod tests;
