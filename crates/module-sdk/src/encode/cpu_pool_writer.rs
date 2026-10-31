//! Synchronous CPU capture copies into explicitly negotiated encoder-owned storage.
//!
//! This helper does not retain capture frames: all reads finish inside the
//! capture callback. The caller owns capture negotiation, transaction options,
//! pool retirement and the lifetime guard required by `InputPool`.
use crate::{
    abi::{STATUS_INVALID_ARGUMENT, STATUS_OK, Status},
    encode::pool::{InputLease, InputPool},
};
use amaterasu_media::*;
use std::{ffi::c_void, mem::size_of, ptr};

const MAX_OFFERS: usize = 64;

/// Negotiated CPU target capabilities and the configured pool generation.
#[derive(Clone, Copy)]
pub struct CpuPoolSelection {
    generation: u64,
    capabilities: EncoderInputCpuWritableCapabilities,
    width: u32,
    height: u32,
}

impl CpuPoolSelection {
    /// Pool generation, needed to query retirement after admission stops.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Capabilities of the selected writable CPU offer.
    pub fn capabilities(&self) -> EncoderInputCpuWritableCapabilities {
        self.capabilities
    }
}

struct Offers {
    ids: Vec<u64>,
    chosen: Option<(u64, EncoderInputCpuWritableCapabilities)>,
    width: u32,
    height: u32,
    format: u32,
    invalid: bool,
}

fn none_descriptor() -> FrameDescriptor {
    FrameDescriptor {
        kind: FRAME_SYNC_NONE,
        version: 0,
        size: 0,
        reserved: 0,
        namespace: [0; 16],
        data: ptr::null(),
    }
}

fn none(value: &FrameDescriptor) -> bool {
    value.kind == FRAME_SYNC_NONE
        && value.version == 0
        && value.size == 0
        && value.reserved == 0
        && value.namespace == [0; 16]
        && value.data.is_null()
}

unsafe extern "C" fn collect_offer(
    context: *mut c_void,
    offer: *const CaptureResourceOffer,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: enumeration owns this synchronous callback context.
    let state = unsafe { &mut *context.cast::<Offers>() };
    if offer.is_null() {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: provider supplies a readable offer for the callback.
    let offer = unsafe { &*offer };
    if offer.id == 0
        || offer.reserved != 0
        || state.ids.len() >= MAX_OFFERS
        || state.ids.contains(&offer.id)
        || offer.accessibility & !(FRAME_ACCESS_CPU | FRAME_ACCESS_GPU) != 0
    {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    state.ids.push(offer.id);
    let resource = &offer.resource;
    if state.chosen.is_some()
        || offer.accessibility != FRAME_ACCESS_CPU
        || !matches!(
            offer.ownership,
            FRAME_OWNERSHIP_BORROWED | FRAME_OWNERSHIP_RETAINABLE
        )
        || resource.kind != ENCODER_INPUT_RESOURCE_CPU_WRITABLE
        || resource.version != 1
        || resource.namespace != [0; 16]
        || resource.reserved != 0
        || resource.size != size_of::<EncoderInputCpuWritableCapabilities>() as u32
        || resource.data.is_null()
        || !none(&offer.acquire_sync)
        || !none(&offer.completion_sync)
    {
        return STATUS_OK;
    }
    // SAFETY: negotiated standard offer descriptor supplies the readable payload.
    let caps = unsafe {
        resource
            .data
            .cast::<EncoderInputCpuWritableCapabilities>()
            .read_unaligned()
    };
    if encoder_input_cpu_writable_capabilities_valid(&caps)
        && caps.format == state.format
        && (caps.min_width..=caps.max_width).contains(&state.width)
        && (caps.min_height..=caps.max_height).contains(&state.height)
    {
        state.chosen = Some((offer.id, caps));
    }
    STATUS_OK
}

/// Enumerates and configures a compatible host-addressable target. A native
/// BUSY/PENDING status remains distinguishable from an incompatible offer.
/// The caller must independently select a CPU capture offer with the same shape
/// and format, and must retire every successful generation.
///
/// # Safety
/// The pool provider must obey its ABI and the writable CPU schema, including
/// callback lifetime, exclusive ownership, and valid payload pointers.
pub unsafe fn configure<G>(
    pool: &InputPool<G>,
    width: u32,
    height: u32,
    format: u32,
    target_count: u32,
) -> Result<CpuPoolSelection, Status> {
    let mut offers = Offers {
        ids: Vec::new(),
        chosen: None,
        width,
        height,
        format,
        invalid: false,
    };
    let sink = CaptureResourceOfferSink {
        context: (&mut offers as *mut Offers).cast(),
        write: collect_offer,
    };
    // SAFETY: context is live for the synchronous enumeration.
    let offer_generation = unsafe { pool.enumerate(sink) }?;
    if offers.invalid {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let (offer_id, capabilities) = offers.chosen.ok_or(MEDIA_UNSUPPORTED)?;
    let config = EncoderInputPoolConfig {
        version: 1,
        size: size_of::<EncoderInputPoolConfig>() as u32,
        offer_generation,
        offer_id,
        width,
        height,
        target_count,
        reserved: 0,
    };
    let generation = pool.configure(&config)?;
    Ok(CpuPoolSelection {
        generation,
        capabilities,
        width,
        height,
    })
}

/// When abandonment fails the lease remains live and must be explicitly retried.
/// Retaining it also retains the pool's lifetime guard; dropping it poisons the pool.
pub enum WriteError<G> {
    /// The operation failed without leaving an outstanding lease.
    Status(Status),
    /// Abandonment failed; retain and retry this exclusive lease.
    Abandon {
        /// Native abandonment failure.
        status: Status,
        /// Lease still owned by the caller.
        lease: InputLease<G>,
    },
}

fn valid_color(color: &FrameColorMetadata) -> bool {
    (color.version == 0
        && color.size == 0
        && color.primaries == 0
        && color.transfer == 0
        && color.matrix == 0
        && color.range == 0)
        || (color.version == 1 && color.size == size_of::<FrameColorMetadata>() as u32)
}

fn valid_clock(clock: &FrameClockMetadata) -> bool {
    if clock.version == 0 {
        return clock.size == 0
            && clock.domain == 0
            && clock.reserved == 0
            && clock.epoch_id == [0; 16];
    }
    clock.version == 1
        && clock.size == size_of::<FrameClockMetadata>() as u32
        && clock.reserved == 0
        && matches!(
            clock.domain,
            FRAME_CLOCK_UNKNOWN | FRAME_CLOCK_STREAM_RELATIVE | FRAME_CLOCK_HOST_MONOTONIC
        )
}

fn abandon<G>(lease: InputLease<G>, original: Status) -> WriteError<G> {
    // SAFETY: writes are synchronous and no access remains outstanding.
    match unsafe { lease.abandon(&none_descriptor()) } {
        Ok(()) => WriteError::Status(original),
        Err((status, lease)) => WriteError::Abandon { status, lease },
    }
}

/// Copy one borrowed CPU capture frame and submit the exclusive target. The
/// caller supplies the selected capture identity and any prepared control ID.
/// Busy acquires create no lease. On rejected commit, the helper abandons it;
/// on failed abandon it returns the still-live lease to the caller.
///
/// # Safety
/// The capture frame and pixel allocation must be live and immutable for this
/// call. The configured pool provider must honor the writable target schema,
/// including pointer validity and exclusive access. Both bindings and their
/// dependencies must satisfy the `InputPool` lifetime and threading contract.
pub unsafe fn write<G>(
    pool: &InputPool<G>,
    selection: &CpuPoolSelection,
    frame: &CaptureResourceFrame,
    capture_generation: u64,
    capture_offer: u64,
    control_id: u64,
) -> Result<(), WriteError<G>> {
    let source = &frame.resource;
    if frame.offer_generation != capture_generation
        || capture_generation == 0
        || frame.offer_id != capture_offer
        || capture_offer == 0
        || frame.reserved != 0
        || !matches!(
            frame.freshness,
            CAPTURE_FRAME_FRESH | CAPTURE_FRAME_CACHED_REUSE
        )
        || !valid_color(&frame.color)
        || !valid_clock(&frame.clock)
        || !valid_clock(&frame.pacing_clock)
        || !none(&frame.acquire_sync)
        || source.kind != FRAME_RESOURCE_CPU
        || source.version != 1
        || source.reserved != 0
        || source.namespace != [0; 16]
        || source.size < size_of::<CpuFrame>() as u32
        || source.data.is_null()
        || (control_id != 0 && control_id != frame.sequence)
    {
        return Err(WriteError::Status(STATUS_INVALID_ARGUMENT));
    }
    // SAFETY: capture schema supplies the readable CpuFrame payload.
    let cpu = unsafe { source.data.cast::<CpuFrame>().read_unaligned() };
    if cpu.validate().is_err()
        || cpu.width != frame.width
        || cpu.height != frame.height
        || cpu.width != selection.width
        || cpu.height != selection.height
        || cpu.format != selection.capabilities.format
        || cpu.sequence != frame.sequence
        || cpu.timestamp_us != frame.timestamp_us
    {
        return Err(WriteError::Status(STATUS_INVALID_ARGUMENT));
    }
    let lease = pool.acquire().map_err(WriteError::Status)?;
    let target = lease.target();
    let desc = &target.resource;
    if target.generation != selection.generation
        || !none(&target.acquire_sync)
        || desc.kind != ENCODER_INPUT_RESOURCE_CPU_WRITABLE
        || desc.version != 1
        || desc.reserved != 0
        || desc.namespace != [0; 16]
        || desc.size != size_of::<EncoderInputCpuWritableFrame>() as u32
        || desc.data.is_null()
    {
        return Err(abandon(lease, STATUS_INVALID_ARGUMENT));
    }
    // SAFETY: the selected writable CPU schema supplies this target payload.
    let output = unsafe {
        desc.data
            .cast::<EncoderInputCpuWritableFrame>()
            .read_unaligned()
    };
    if !encoder_input_cpu_writable_valid(&output, &selection.capabilities)
        || output.width != cpu.width
        || output.height != cpu.height
        || output.format != cpu.format
    {
        return Err(abandon(lease, STATUS_INVALID_ARGUMENT));
    }
    let source_start = cpu.data as usize;
    let target_start = output.data as usize;
    let Some(source_end) = source_start.checked_add(cpu.len) else {
        return Err(abandon(lease, STATUS_INVALID_ARGUMENT));
    };
    let Some(target_end) = target_start.checked_add(output.len) else {
        return Err(abandon(lease, STATUS_INVALID_ARGUMENT));
    };
    if source_start < target_end && target_start < source_end {
        return Err(abandon(lease, STATUS_INVALID_ARGUMENT));
    }
    let row_bytes = match cpu.format {
        PIXEL_BGRA8 => cpu.width as usize * 4,
        PIXEL_NV12 => cpu.width as usize,
        _ => return Err(abandon(lease, STATUS_INVALID_ARGUMENT)),
    };
    let rows = if cpu.format == PIXEL_NV12 {
        cpu.height as usize + cpu.height as usize / 2
    } else {
        cpu.height as usize
    };
    for row in 0..rows {
        // SAFETY: validated layouts cover every row, both allocations remain
        // live, and the complete ranges were checked for overlap above.
        unsafe {
            ptr::copy_nonoverlapping(
                cpu.data.add(row * cpu.stride as usize),
                output.data.add(row * output.stride as usize),
                row_bytes,
            )
        };
    }
    let submission = EncoderInputSubmission {
        version: 1,
        size: size_of::<EncoderInputSubmission>() as u32,
        generation: target.generation,
        target_id: target.target_id,
        lease_id: target.lease_id,
        control_id,
        timestamp_us: frame.timestamp_us,
        sequence: frame.sequence,
        color: frame.color,
        clock: frame.clock,
        completion_sync: none_descriptor(),
    };
    // SAFETY: synchronous copy is complete, and metadata and identity match this lease.
    match unsafe { lease.commit(&submission) } {
        Ok(()) => Ok(()),
        Err((status, lease)) => Err(abandon(lease, status)),
    }
}

#[cfg(test)]
#[path = "cpu_pool_writer/tests.rs"]
mod tests;
