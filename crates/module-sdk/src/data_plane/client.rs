use super::{OwnedChunk, abi::*, storage::BorrowedChunk};
use crate::{ModuleResult, NativeBinding, RuntimeClient};
use amaterasu_abi::{STATUS_INTERNAL_ERROR, STATUS_OK, Status};
use std::sync::Arc;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

struct OwnedInput {
    chunk: OwnedChunk,
    segments: Vec<Segment>,
}
// SAFETY: segments point only into immutable boxed slices owned by chunk. The
// Arc is retained/released through callbacks in this producing library.
unsafe impl Send for OwnedInput {}
unsafe impl Sync for OwnedInput {}
unsafe extern "C" fn retain_input(context: *mut c_void) {
    // SAFETY: a live strong reference exists throughout every retain callback.
    unsafe { Arc::<OwnedInput>::increment_strong_count(context.cast()) };
}
unsafe extern "C" fn release_input(context: *mut c_void) {
    // SAFETY: consumes exactly one provider-retained reference in its allocator.
    unsafe { Arc::<OwnedInput>::decrement_strong_count(context.cast()) };
}

/// Safe synchronous borrow supplied to a consumer. The lifetime prevents bytes
/// from escaping receive. A sink must accept everything or nothing: non-OK
/// leaves this chunk queued, so externally visible partial writes need staging.
pub struct ChunkView<'a> {
    pub record_id: u64,
    pub offset: u64,
    pub total_bytes: u64,
    pub segments: Vec<&'a [u8]>,
    pub metadata: &'a [u8],
}

/// Owns one native binding and one authorized channel. Drop retires queued
/// input and all synchronous callbacks through the provider's release barrier.
/// Like NativeBinding, this wrapper is module-thread confined. Native API users
/// may use dedicated threads under its explicit concurrency/lifetime contract.
pub struct TransferClient {
    binding: NativeBinding,
    id: ChannelId,
    description: Description,
}
impl TransferClient {
    /// # Safety
    /// Service must implement TransferApi v1, runtime/provider must outlive self,
    /// and the host must destroy consumers before providers. Expected description
    /// must come from the authorized control negotiation, never guessed formats.
    pub unsafe fn bind(
        runtime: &RuntimeClient,
        service: &str,
        id: ChannelId,
        expected: Description,
    ) -> ModuleResult<Self> {
        // SAFETY: caller establishes service, runtime and provider lifetimes.
        let binding = unsafe {
            runtime.bind_native_typed(
                service,
                super::TRANSFER_CONTRACT,
                INTERFACE_MAJOR,
                INTERFACE_MINOR,
                API_SIZE,
            )?
        };
        let mut output = std::mem::MaybeUninit::<Description>::uninit();
        // SAFETY: exact service table negotiated; output has sufficient storage.
        let api = unsafe { binding.table::<TransferApi>()? };
        let status = unsafe { (api.describe)(binding.context(), id, output.as_mut_ptr()) };
        if status != STATUS_OK {
            return Err(format!("binary channel description rejected: {status}"));
        }
        // SAFETY: successful describe initializes the entire descriptor.
        let description = unsafe { output.assume_init() };
        if description != expected {
            return Err("binary channel negotiation changed".into());
        }
        super::description(
            description.direction,
            description.format,
            description.metadata_schema,
            description.limits,
        )
        .map_err(|_| "invalid binary channel description")?;
        if description.version != 1
            || description.size != size_of::<Description>() as u32
            || description.reserved != 0
        {
            return Err("unsupported binary channel description".into());
        }
        Ok(Self {
            binding,
            id,
            description,
        })
    }
    fn api(&self) -> &TransferApi {
        // SAFETY: bind established this exact layout and owns the live binding.
        unsafe {
            self.binding
                .table::<TransferApi>()
                .expect("validated transfer interface")
        }
    }
    pub fn description(&self) -> Description {
        self.description
    }
    /// Copies bytes into the recipient's bounded queue; no Rust ownership crosses
    /// libraries. Native producers can use submit_leased to avoid this copy.
    pub fn submit(&self, chunk: &OwnedChunk) -> Status {
        if let Err(status) = chunk.validate(self.description.limits) {
            return status;
        }
        let segments = chunk.segments();
        let descriptor = chunk.descriptor(&segments);
        // SAFETY: all descriptors/bytes remain live through this synchronous copy.
        unsafe { (self.api().submit)(self.binding.context(), self.id, &descriptor) }
    }
    /// Transfers owned immutable CPU bytes without copying their payload. The
    /// recipient retains a C lease, not a Rust Arc. On rejection the original
    /// owned chunk is returned for retry. This binding's retirement barrier keeps
    /// the producing library's release callback alive until every reference ends.
    pub fn submit_owned(&self, chunk: OwnedChunk) -> Result<(), (Status, OwnedChunk)> {
        let shape = match chunk.validate(self.description.limits) {
            Ok(shape) => shape,
            Err(status) => return Err((status, chunk)),
        };
        let segments = chunk.segments();
        let input = Arc::new(OwnedInput { chunk, segments });
        let lease = BufferLease {
            context: Arc::as_ptr(&input).cast_mut().cast(),
            retained_bytes: shape.charge,
            retain: retain_input,
            release: release_input,
        };
        let descriptor = Chunk {
            lease: &lease,
            ..input.chunk.descriptor(&input.segments)
        };
        // SAFETY: the Arc owns immutable descriptors/storage, and callbacks run
        // in this library through self's release barrier. Local Arc survives call.
        let status = unsafe { self.submit_leased(&descriptor) };
        if status == STATUS_OK {
            return Ok(());
        }
        match Arc::try_unwrap(input) {
            Ok(input) => Err((status, input.chunk)),
            // Retaining rejected input violates the ABI and cannot certify a
            // safe provider lifetime. Do not return a false ownership guarantee.
            Err(_) => std::process::abort(),
        }
    }
    /// # Safety
    /// Honor Chunk and BufferLease lifetimes, including immutability, thread-safe
    /// retain/release and retained_bytes. The producer's DLL must remain loaded
    /// through cancel or this client's Drop barrier. BUSY retains nothing.
    pub unsafe fn submit_leased(&self, chunk: &Chunk) -> Status {
        // SAFETY: caller owns the submitted descriptor and optional retained lease.
        unsafe { (self.api().submit)(self.binding.context(), self.id, chunk) }
    }
    pub fn receive(&self, callback: impl for<'a> FnMut(ChunkView<'a>) -> Status) -> Status {
        struct Context<F> {
            callback: F,
            limits: Limits,
        }
        unsafe extern "C" fn deliver<F: for<'a> FnMut(ChunkView<'a>) -> Status>(
            context: *mut c_void,
            chunk: *const Chunk,
        ) -> Status {
            catch_unwind(AssertUnwindSafe(|| {
                if chunk.is_null() {
                    return STATUS_INTERNAL_ERROR;
                }
                // SAFETY: synchronous call retains context and provider descriptors.
                let context = unsafe { &mut *context.cast::<Context<F>>() };
                let chunk = unsafe { &*chunk };
                if !chunk.lease.is_null() {
                    return STATUS_INTERNAL_ERROR;
                }
                // SAFETY: TransferApi promises readable immutable callback borrows.
                if let Err(status) = unsafe { BorrowedChunk::new(chunk, context.limits) } {
                    return status;
                }
                let descriptors = unsafe {
                    std::slice::from_raw_parts(chunk.segments, chunk.segment_count as usize)
                };
                let segments = descriptors
                    .iter()
                    .map(|segment| unsafe { std::slice::from_raw_parts(segment.data, segment.len) })
                    .collect();
                let metadata = if chunk.metadata_len == 0 {
                    &[][..]
                } else {
                    unsafe {
                        std::slice::from_raw_parts(chunk.metadata, chunk.metadata_len as usize)
                    }
                };
                (context.callback)(ChunkView {
                    record_id: chunk.record_id,
                    offset: chunk.offset,
                    total_bytes: chunk.total_bytes,
                    segments,
                    metadata,
                })
            }))
            .unwrap_or(STATUS_INTERNAL_ERROR)
        }
        fn sink<F: for<'a> FnMut(ChunkView<'a>) -> Status>(context: &mut Context<F>) -> ChunkSink {
            ChunkSink {
                context: (context as *mut Context<F>).cast(),
                write: deliver::<F>,
            }
        }
        let mut context = Context {
            callback,
            limits: self.description.limits,
        };
        // SAFETY: callback captures cannot escape the synchronous table invocation.
        unsafe { (self.api().receive)(self.binding.context(), self.id, sink(&mut context)) }
    }
    pub fn snapshot(&self) -> Result<Snapshot, Status> {
        self.wait(0, 0)
    }
    pub fn wait(&self, generation: u64, timeout_ms: u32) -> Result<Snapshot, Status> {
        let mut output = Snapshot::default();
        // SAFETY: binding and writable output remain live during this bounded wait.
        let status = unsafe {
            (self.api().wait)(
                self.binding.context(),
                self.id,
                generation,
                timeout_ms,
                &mut output,
            )
        };
        if status == STATUS_OK {
            Ok(output)
        } else {
            Err(status)
        }
    }
    pub fn finish(&self) -> Status {
        // SAFETY: live binding; no borrowed input crosses this call.
        unsafe { (self.api().finish)(self.binding.context(), self.id) }
    }
    pub fn cancel(&self) -> Status {
        // SAFETY: synchronous barrier on this owned channel; no sink is active
        // on this thread. Must not be reentered from this channel's receive sink.
        unsafe { (self.api().cancel)(self.binding.context(), self.id) }
    }
}
