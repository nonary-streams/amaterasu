use crate::{abi::*, media::BorrowedCpuFrame};
use amaterasu_media::*;

#[derive(Clone, Copy)]
pub struct CpuProcessingOffer {
    pub id: u64,
    pub capabilities: CpuResourceCapabilitiesV2,
}
#[derive(Clone)]
pub struct CpuProcessingDescription {
    pub generation: u64,
    pub inputs: Vec<CpuProcessingOffer>,
    pub outputs: Vec<CpuProcessingOffer>,
}
/// Synchronous immutable input. Its selected tuple is proven independently of
/// the output offer; the processor must transform pixels when those differ.
pub struct CpuResourceInput<'a> {
    pub pixels: BorrowedCpuFrame<'a>,
    pub color: FrameColorMetadata,
    pub freshness: u32,
}
pub(super) fn tuple(color: FrameColorMetadata) -> [u32; 6] {
    [
        color.version,
        color.size,
        color.primaries,
        color.transfer,
        color.matrix,
        color.range,
    ]
}
pub(super) fn known(caps: CpuResourceCapabilitiesV2) -> bool {
    let base = caps.base;
    let color = caps.color;
    if color.version != 1
        || color.size != size_of::<FrameColorMetadata>() as u32
        || !matches!(color.range, FRAME_RANGE_FULL | FRAME_RANGE_LIMITED)
        || base.min_width == 0
        || base.min_height == 0
        || base.min_width > base.max_width
        || base.min_height > base.max_height
        || base.max_width > 8192
        || base.max_height > 8192
    {
        return false;
    }
    match base.format {
        PIXEL_BGRA8 => {
            color.primaries == FRAME_PRIMARIES_BT709
                && color.transfer == FRAME_TRANSFER_SRGB
                && color.matrix == FRAME_MATRIX_RGB
                && color.range == FRAME_RANGE_FULL
        }
        PIXEL_NV12 | PIXEL_P010 => {
            (color.primaries == FRAME_PRIMARIES_BT709
                && color.transfer == FRAME_TRANSFER_BT709
                && color.matrix == FRAME_MATRIX_BT709)
                || (base.format == PIXEL_P010
                    && color.primaries == FRAME_PRIMARIES_BT2020
                    && color.transfer == FRAME_TRANSFER_PQ
                    && color.matrix == FRAME_MATRIX_BT2020_NCL)
        }
        _ => false,
    }
}
pub(super) fn compatible(
    producer: CpuResourceCapabilitiesV2,
    consumer: CpuResourceCapabilitiesV2,
) -> bool {
    known(producer)
        && known(consumer)
        && producer.base.format == consumer.base.format
        && tuple(producer.color) == tuple(consumer.color)
        && producer.base.min_width >= consumer.base.min_width
        && producer.base.max_width <= consumer.base.max_width
        && producer.base.min_height >= consumer.base.min_height
        && producer.base.max_height <= consumer.base.max_height
}
pub(super) fn none(d: &FrameDescriptor) -> bool {
    d.kind == FRAME_SYNC_NONE
        && d.version == 0
        && d.size == 0
        && d.reserved == 0
        && d.namespace == [0; 16]
        && d.data.is_null()
}
// Match the base resource contract: v0 is absent, v1 has a complete envelope
// with reserved zero, and unknown versions remain opaque. No domain is used
// for timing comparisons here, including UNKNOWN or future domain values.
fn valid_clock(clock: FrameClockMetadata) -> bool {
    match clock.version {
        0 => {
            clock.size == 0 && clock.domain == 0 && clock.reserved == 0 && clock.epoch_id == [0; 16]
        }
        1 => clock.size == size_of::<FrameClockMetadata>() as u32 && clock.reserved == 0,
        _ => true,
    }
}
pub(super) fn validate_description(description: &CpuProcessingDescription) -> Result<(), Status> {
    if description.generation == 0 {
        return Err(MEDIA_BACKEND_ERROR);
    }
    for offers in [&description.inputs, &description.outputs] {
        if offers.is_empty() || offers.len() > super::MAX_PROCESSING_OFFERS {
            return Err(MEDIA_BACKEND_ERROR);
        }
        let mut ids = std::collections::BTreeSet::new();
        for offer in offers {
            if offer.id == 0 || !ids.insert(offer.id) || !known(offer.capabilities) {
                return Err(MEDIA_BACKEND_ERROR);
            }
        }
    }
    Ok(())
}
pub(super) unsafe fn read_offer(
    offer: &CaptureResourceOffer,
) -> Result<CpuProcessingOffer, Status> {
    let descriptor = offer.resource;
    if offer.id == 0
        || offer.reserved != 0
        || offer.accessibility & FRAME_ACCESS_CPU == 0
        || offer.accessibility & !(FRAME_ACCESS_CPU | FRAME_ACCESS_GPU) != 0
        || !matches!(
            offer.ownership,
            FRAME_OWNERSHIP_BORROWED | FRAME_OWNERSHIP_RETAINABLE
        )
        || descriptor.kind != FRAME_RESOURCE_CPU
        || descriptor.version != 2
        || descriptor.size != size_of::<CpuResourceCapabilitiesV2>() as u32
        || descriptor.reserved != 0
        || descriptor.namespace != [0; 16]
        || descriptor.data.is_null()
        || !none(&offer.acquire_sync)
        || !none(&offer.completion_sync)
    {
        return Err(MEDIA_UNSUPPORTED);
    }
    // SAFETY: native enumeration lends its complete standard-v2 capabilities.
    let capabilities = unsafe {
        descriptor
            .data
            .cast::<CpuResourceCapabilitiesV2>()
            .read_unaligned()
    };
    if !known(capabilities) {
        return Err(MEDIA_UNSUPPORTED);
    }
    Ok(CpuProcessingOffer {
        id: offer.id,
        capabilities,
    })
}
pub(super) unsafe fn input<'a>(
    frame: &'a CaptureResourceFrame,
    caps: CpuResourceCapabilitiesV2,
) -> Result<CpuResourceInput<'a>, Status> {
    let d = frame.resource;
    if frame.reserved != 0
        || !valid_clock(frame.clock)
        || !valid_clock(frame.pacing_clock)
        || frame.sequence == 0
        || !matches!(
            frame.freshness,
            CAPTURE_FRAME_FRESH | CAPTURE_FRAME_CACHED_REUSE
        )
        || tuple(frame.color) != tuple(caps.color)
        || d.kind != FRAME_RESOURCE_CPU
        || d.version != 2
        || d.size < size_of::<CpuFrame>() as u32
        || d.reserved != 0
        || d.namespace != [0; 16]
        || d.data.is_null()
        || !(d.data as usize).is_multiple_of(align_of::<CpuFrame>())
        || !none(&frame.acquire_sync)
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    // SAFETY: the caller guarantees the immutable native descriptor/pixel borrow.
    let cpu = unsafe { &*d.data.cast::<CpuFrame>() };
    let base = caps.base;
    if cpu.width != frame.width
        || cpu.height != frame.height
        || cpu.timestamp_us != frame.timestamp_us
        || cpu.sequence != frame.sequence
        || cpu.format != base.format
        || cpu.width < base.min_width
        || cpu.width > base.max_width
        || cpu.height < base.min_height
        || cpu.height > base.max_height
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    Ok(CpuResourceInput {
        pixels: unsafe { BorrowedCpuFrame::from_descriptor(cpu)? },
        color: frame.color,
        freshness: frame.freshness,
    })
}
pub(super) fn with_offer<T>(
    offer: CpuProcessingOffer,
    call: impl FnOnce(&CaptureResourceOffer) -> T,
) -> T {
    let none = FrameDescriptor {
        kind: FRAME_SYNC_NONE,
        version: 0,
        size: 0,
        reserved: 0,
        namespace: [0; 16],
        data: std::ptr::null(),
    };
    call(&CaptureResourceOffer {
        id: offer.id,
        ownership: FRAME_OWNERSHIP_RETAINABLE,
        accessibility: FRAME_ACCESS_CPU,
        reserved: 0,
        resource: FrameDescriptor {
            kind: FRAME_RESOURCE_CPU,
            version: 2,
            size: size_of::<CpuResourceCapabilitiesV2>() as u32,
            reserved: 0,
            namespace: [0; 16],
            data: std::ptr::from_ref(&offer.capabilities).cast(),
        },
        acquire_sync: none,
        completion_sync: none,
    })
}
