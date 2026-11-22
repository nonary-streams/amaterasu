//! GameStream wire validation and full transport reservation.
use super::*;

const SHORT_FRAME_HEADER_BYTES: usize = 8;
const NV_HEADER_BYTES: u32 = 16;
const RTP_HEADER_BYTES: u32 = 16;
const MAX_BLOCK_DATAGRAMS: u32 = 255;
const MAX_BLOCK_DATA_SHARDS: u32 = 1023;

impl VideoFramePlan {
    /// Legacy Rust helper for plans using the GameStream packet layout.
    ///
    /// # Safety
    /// Both nonempty plan allocations must be live, initialized and immutable.
    pub unsafe fn validate(
        &self,
        limits: &VideoFramingLimits,
    ) -> Result<VideoPlanReservation, Status> {
        // SAFETY: the caller provides both live buffers.
        unsafe { self.validate_gamestream(limits) }
    }

    /// Validate GameStream shard packing and FEC and reserve full protected
    /// datagram sizes before changing packet, frame or nonce state.
    ///
    /// # Safety
    /// Both nonempty plan allocations must be live, initialized and immutable.
    pub unsafe fn validate_gamestream(
        &self,
        limits: &VideoFramingLimits,
    ) -> Result<VideoPlanReservation, Status> {
        // SAFETY: the caller provides both live buffers.
        let counts = unsafe { self.validate_shape(limits) }?;
        if !limits.packet_size.is_multiple_of(16)
            || (self.framed_bytes.len + SHORT_FRAME_HEADER_BYTES)
                .div_ceil((limits.packet_size - NV_HEADER_BYTES) as usize)
                != counts.data_shards as usize
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let packet_bytes = limits.packet_size + RTP_HEADER_BYTES;
        let mut field_work = 0u64;
        for block in &self.blocks[..self.block_count as usize] {
            if block.data_shards > MAX_BLOCK_DATA_SHARDS
                || (block.data_shards * block.percentage).div_ceil(100) != block.parity_shards
                || (block.parity_shards != 0
                    && block.data_shards + block.parity_shards > MAX_BLOCK_DATAGRAMS)
            {
                return Err(STATUS_INVALID_ARGUMENT);
            }
            field_work += u64::from(block.data_shards)
                * u64::from(block.parity_shards)
                * u64::from(packet_bytes);
        }
        if field_work > VIDEO_FRAME_MAX_FIELD_WORK {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if limits.min_fec_percentage != 0 && self.blocks[0].parity_shards == 0 {
            let largest =
                largest_protected_data(limits.min_fec_percentage, limits.min_parity_shards)
                    .ok_or(STATUS_INVALID_ARGUMENT)?;
            if counts.data_shards <= largest * limits.max_fec_blocks {
                return Err(STATUS_INVALID_ARGUMENT);
            }
        }
        let datagrams = u64::from(counts.datagrams);
        let queue_bytes = datagrams * u64::from(packet_bytes + limits.crypto_overhead);
        let wire_bytes = queue_bytes + datagrams * u64::from(limits.network_overhead);
        if queue_bytes > u64::from(limits.max_queue_bytes) || wire_bytes > limits.max_wire_bytes {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(VideoPlanReservation {
            data_shards: counts.data_shards,
            datagrams: counts.datagrams,
            queue_bytes,
            wire_bytes,
        })
    }
}

/// Maximum data shards that fit one protected GameStream block under the
/// minimum trusted policy, considering every representable wire percentage.
pub(super) fn largest_protected_data(min_percentage: u32, min_parity: u32) -> Option<u32> {
    (1u32..MAX_BLOCK_DATAGRAMS).rev().find(|data| {
        let rate_for_minimum = if min_parity == 0 {
            0
        } else {
            ((min_parity - 1) * 100 / data) + 1
        };
        let rate = min_percentage.max(rate_for_minimum);
        rate <= 255 && data + (data * rate).div_ceil(100) <= MAX_BLOCK_DATAGRAMS
    })
}
