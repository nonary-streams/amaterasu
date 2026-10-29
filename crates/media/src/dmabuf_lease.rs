//! Explicit lifetime for immutable DMA-BUF presentations.
use crate::{CAPTURE_FRAME_CACHED_REUSE, CAPTURE_FRAME_FRESH, DmaBufFrame};
use amaterasu_abi::{NativeInterface, STATUS_INVALID_ARGUMENT, Status};
use std::{ffi::c_void, mem::size_of};

/// One immutable presentation with an explicit provider-owned lifetime.
///
/// The descriptor is borrowed during a capture/processor callback. `retain`
/// acquires one additional reference on success; BUSY or any other error acquires
/// nothing. Providers must impose a finite retained-frame/reference limit and
/// return BUSY when full. References pin both storage and its immutable contents,
/// not merely the DMA-BUF allocation. A duplicated scanout fd alone cannot
/// provide this guarantee. Consumers still wait on every acquire fence before
/// reading pixels; acquire-fence and plane descriptors remain provider-owned.
///
/// `release` consumes exactly one retained reference. A nonnegative completion
/// fence fd is transferred to the provider, which waits for that fence before
/// reusing storage and closes it exactly once. `-1` asserts all consumer GPU work
/// has completed. An invalid/failed fence must quarantine storage rather than
/// permit reuse. Release cannot fail: ownership always transfers, including on
/// shutdown. The provider, callbacks, and native binding must outlive all retained
/// references and pending fences. No callback may unwind across the ABI.
///
/// Callbacks run on the owning module thread; this ABI does not promise that
/// retain/release or frame access may run concurrently on other threads.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DmaBufFrameLease {
    pub frame: DmaBufFrame,
    pub context: *mut c_void,
    pub retain: Option<unsafe extern "C" fn(*mut c_void) -> Status>,
    pub release: Option<unsafe extern "C" fn(*mut c_void, i32)>,
}

impl DmaBufFrameLease {
    /// Checks metadata and the presence of lifetime callbacks, never pointer or
    /// descriptor validity. The caller remains responsible for provider trust.
    pub fn validate(&self) -> Result<(), Status> {
        self.frame.validate()?;
        if self.context.is_null() || self.retain.is_none() || self.release.is_none() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }
}

/// Synchronous delivery of a borrowed lease. The recipient explicitly retains
/// the lease if it needs the pixels beyond this call. Sink return status never
/// implicitly transfers a reference, on success or failure.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DmaBufLeaseSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const DmaBufFrameLease, u32) -> Status,
}

impl DmaBufLeaseSink {
    /// # Safety
    /// The sink and provider, frame storage, and callbacks must be live and
    /// obey the lease contract. The provider owns its initial reference for the
    /// entire call and releases it only after `send` returns.
    pub unsafe fn send(&self, lease: &DmaBufFrameLease, freshness: u32) -> Status {
        if let Err(status) = lease.validate() {
            return status;
        }
        if !matches!(freshness, CAPTURE_FRAME_CACHED_REUSE | CAPTURE_FRAME_FRESH) {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller guarantees sink and lease lifetime through this call.
        unsafe { (self.write)(self.context, lease, freshness) }
    }
}

pub const GPU_PROCESSOR_INTERFACE_MAJOR: u32 = 1;
pub const GPU_PROCESSOR_INTERFACE_MINOR: u32 = 0;

/// Optional synchronous GPU transform. It receives a borrowed input lease and
/// delivers one borrowed output lease through the sink on success, preserving
/// sequence, timestamp, and the supplied source freshness. The processor owns
/// the output during the callback; the sink explicitly retains any output
/// needed afterward. It propagates a
/// rejected sink status unchanged; BUSY before delivery consumes nothing.
///
/// All submitted GPU operations must obey acquire/completion fences. Processing
/// may change layout and pixels, but cannot mutate the immutable input. Before
/// returning, the processor finishes every read from the borrowed input or
/// explicitly retains a separate reference and releases it after GPU completion
/// (or with its own completion fence). An output with pending GPU writes carries
/// valid acquire fences covering those writes. Releasing the caller's input
/// reference does not retire the processor's independently retained GPU work.
/// No sink callback may occur after `process` returns. An asynchronous processor
/// should
/// retain inputs privately and expose a separate scheduling API; this interface
/// promises no asynchronous callback or unbounded internal queue.
#[repr(C)]
pub struct GpuProcessorApi {
    pub base: NativeInterface,
    pub process: unsafe extern "C" fn(
        *mut c_void,
        u64,
        *const DmaBufFrameLease,
        u32,
        DmaBufLeaseSink,
    ) -> Status,
    /// Stops this processing stream and synchronously retires every retained
    /// input/output reference and completion fence before returning. May block
    /// on GPU/vendor work; source bindings remain live throughout this call.
    /// No more work may be submitted for this stream after cancellation.
    pub cancel_dmabuf_leases: unsafe extern "C" fn(*mut c_void, u64),
}

pub const GPU_PROCESSOR_API_V1_0_SIZE: u32 = size_of::<GpuProcessorApi>() as u32;
