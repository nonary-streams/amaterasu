//! Versioned, provider-neutral encoded video units and bounded framing plans.
//!
//! These tables do not expose sockets, keys, RTP sequence numbers or nonce
//! ownership. All pointed-to bytes are borrowed only during the synchronous
//! call; BUSY consumes nothing. Native providers are trusted code.
use crate::{
    EncoderFormatId, FRAME_CLOCK_STREAM_RELATIVE, FrameClockMetadata, MAX_VIDEO_ACCESS_UNIT_BYTES,
};
use amaterasu_abi::{BorrowedBytes, NativeInterface, STATUS_INVALID_ARGUMENT, Status};
use std::{ffi::c_void, mem::size_of};

pub const VIDEO_UNIT_VERSION: u32 = 1;
/// FEC policy fields extend the v1 limits prefix and require framing API v2.
pub const VIDEO_FRAMING_LIMITS_VERSION: u32 = 2;
pub const VIDEO_FRAMING_INTERFACE_MAJOR: u32 = 2;
pub const VIDEO_FRAMING_INTERFACE_MINOR: u32 = 1;
pub const VIDEO_UNIT_KEYFRAME: u32 = 1;
pub const VIDEO_UNIT_DISCONTINUITY: u32 = 2;
pub const VIDEO_UNIT_END_OF_STREAM: u32 = 4;
pub const VIDEO_FRAME_MAX_BLOCKS: usize = 4;
pub const VIDEO_FRAME_MAX_DATAGRAMS: u32 = 2048;
pub const VIDEO_FRAME_MAX_FIELD_WORK: u64 = 256 * 1024 * 1024;
pub const VIDEO_FRAME_RECORD_START: u32 = 1;
pub const VIDEO_FRAME_CRITICAL_PREFIX: u32 = 2;
/// Trusted policy may omit parity only when the frame cannot fit protected
/// blocks within its negotiated block limit. This is not a provider preference.
pub const VIDEO_FRAME_ALLOW_LARGE_UNPROTECTED: u32 = 1;

/// One complete access unit, bound to one selected source and configuration.
/// Bytes are immutable and borrowed only through the sink/submit call. This
/// version carries no frame metadata and never authorizes metadata insertion.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NegotiatedVideoUnit {
    pub size: u32,
    pub version: u32,
    pub stream: u64,
    pub generation: u64,
    pub offer_id: u64,
    pub codec: EncoderFormatId,
    pub framing: EncoderFormatId,
    pub access_unit_id: u64,
    pub submission_id: u64,
    pub source_id: u64,
    pub config_generation: u64,
    pub timestamp_us: u64,
    pub clock: FrameClockMetadata,
    pub flags: u32,
    pub reserved: u32,
    pub data: BorrowedBytes,
}

impl NegotiatedVideoUnit {
    /// Structural validity is not proof of the selected profile or provider.
    pub fn validate_for(
        &self,
        stream: u64,
        generation: u64,
        offer_id: u64,
        codec: EncoderFormatId,
        framing: EncoderFormatId,
    ) -> Result<(), Status> {
        let same = |a: EncoderFormatId, b: EncoderFormatId| {
            a.namespace_id == b.namespace_id && a.value == b.value && a.schema == b.schema
        };
        let valid_id =
            |id: EncoderFormatId| id.namespace_id != 0 && id.value != 0 && id.schema != 0;
        if self.size as usize != size_of::<Self>()
            || self.version != VIDEO_UNIT_VERSION
            || stream == 0
            || generation == 0
            || offer_id == 0
            || self.stream != stream
            || self.generation != generation
            || self.offer_id != offer_id
            || !valid_id(codec)
            || !valid_id(framing)
            || !same(self.codec, codec)
            || !same(self.framing, framing)
            || self.config_generation == 0
            || self.source_id == 0
            || self.clock.version != 1
            || self.clock.size as usize != size_of::<FrameClockMetadata>()
            || self.clock.domain != FRAME_CLOCK_STREAM_RELATIVE
            || self.clock.reserved != 0
            || self.clock.epoch_id == [0; 16]
            || self.reserved != 0
            || self.flags
                & !(VIDEO_UNIT_KEYFRAME | VIDEO_UNIT_DISCONTINUITY | VIDEO_UNIT_END_OF_STREAM)
                != 0
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if self.flags == VIDEO_UNIT_END_OF_STREAM {
            if self.data.len != 0 {
                return Err(STATUS_INVALID_ARGUMENT);
            }
        } else if self.flags & VIDEO_UNIT_END_OF_STREAM != 0
            || self.access_unit_id == 0
            || self.data.len == 0
            || self.data.len > MAX_VIDEO_ACCESS_UNIT_BYTES
            || self.data.ptr.is_null()
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NegotiatedVideoUnitSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const NegotiatedVideoUnit) -> Status,
}

#[repr(C)]
pub struct NegotiatedVideoApi {
    pub base: NativeInterface,
    pub receive: unsafe extern "C" fn(*mut c_void, u64, NegotiatedVideoUnitSink) -> Status,
}

#[repr(C)]
pub struct NegotiatedVideoTransportApi {
    pub base: NativeInterface,
    pub submit: unsafe extern "C" fn(*mut c_void, u64, *const NegotiatedVideoUnit) -> Status,
    pub drain: unsafe extern "C" fn(*mut c_void, u64) -> Status,
}

/// One contiguous block in data-shard order. Percentage is the wire value;
/// parity_shards must equal ceil(data_shards * percentage / 100).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VideoFecBlock {
    pub first_data_shard: u32,
    pub data_shards: u32,
    pub parity_shards: u32,
    pub percentage: u32,
}

/// Host-supplied bounds from trusted session composition and peer negotiation.
/// packet_size is NV header plus payload, excluding the 16-byte RTP header.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoFramingLimits {
    pub size: u32,
    pub version: u32,
    pub packet_size: u32,
    pub crypto_overhead: u32,
    pub network_overhead: u32,
    pub allowed_extensions: u32,
    pub max_datagrams: u32,
    pub max_queue_bytes: u32,
    pub min_parity_shards: u32,
    pub max_fec_percentage: u32,
    pub max_wire_bytes: u64,
    /// V2 tail; the preceding V1 field offsets remain unchanged.
    pub min_fec_percentage: u32,
    pub max_fec_blocks: u32,
    pub fec_policy_flags: u32,
    pub reserved: u32,
}

/// Authenticated video-source selection supplied to the transport once, before
/// any packet or nonce state changes. Metadata-required sessions are not
/// supported by this version.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VideoPlanSelection {
    pub size: u32,
    pub version: u32,
    pub stream: u64,
    pub generation: u64,
    pub offer_id: u64,
    pub source_id: u64,
    pub config_generation: u64,
    pub codec: EncoderFormatId,
    pub framing: EncoderFormatId,
    pub clock_epoch: [u8; 16],
    pub clock_origin_us: u64,
    pub metadata_required: u32,
    pub reserved: u32,
}

/// Plan transport is a distinct typed family from unit-only transport. Prepare
/// outputs host-controlled limits; it never accepts FEC policy from the caller.
#[repr(C)]
pub struct NegotiatedVideoPlanTransportApi {
    pub base: NativeInterface,
    pub probe: unsafe extern "C" fn(
        *mut c_void,
        u64,
        *const VideoPlanSelection,
        *mut VideoFramingLimits,
    ) -> Status,
    pub prepare: unsafe extern "C" fn(
        *mut c_void,
        u64,
        *const VideoPlanSelection,
        *mut VideoFramingLimits,
    ) -> Status,
    pub submit: unsafe extern "C" fn(
        *mut c_void,
        u64,
        *const NegotiatedVideoUnit,
        *const VideoFramePlan,
    ) -> Status,
    pub drain: unsafe extern "C" fn(*mut c_void, u64) -> Status,
}

/// Framed bytes exclude the eight-byte short header. record_starts has one byte
/// per data shard (0 or 1), or is empty. Pointers are synchronous borrows.
/// Providers may reorder records or add padding within framed_bytes. The host
/// owns transport headers, FEC execution, sequence, encryption and delivery.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VideoFramePlan {
    pub size: u32,
    pub version: u32,
    pub framed_bytes: BorrowedBytes,
    pub record_starts: BorrowedBytes,
    pub critical_prefix_shards: u32,
    pub block_count: u32,
    pub blocks: [VideoFecBlock; VIDEO_FRAME_MAX_BLOCKS],
}

#[derive(Debug, PartialEq, Eq)]
pub struct VideoPlanReservation {
    pub data_shards: u32,
    pub datagrams: u32,
    pub queue_bytes: u64,
    pub wire_bytes: u64,
}

impl VideoFramePlan {
    /// Validate all provider-supplied values and produce a conservative queue
    /// reservation before sequence, frame-index or nonce mutation.
    ///
    /// # Safety
    /// Both nonempty borrowed allocations must be initialized and immutable
    /// until this function returns. Structural checks cannot prove pointer validity.
    pub unsafe fn validate(
        &self,
        limits: &VideoFramingLimits,
    ) -> Result<VideoPlanReservation, Status> {
        let invalid = || STATUS_INVALID_ARGUMENT;
        if self.size as usize != size_of::<Self>()
            || self.version != 1
            || limits.size as usize != size_of::<VideoFramingLimits>()
            || limits.version != VIDEO_FRAMING_LIMITS_VERSION
            || !(256..=1392).contains(&limits.packet_size)
            || !limits.packet_size.is_multiple_of(16)
            || limits.crypto_overhead > 256
            || limits.network_overhead > 128
            || limits.allowed_extensions & !(VIDEO_FRAME_RECORD_START | VIDEO_FRAME_CRITICAL_PREFIX)
                != 0
            || limits.max_datagrams == 0
            || limits.max_datagrams > VIDEO_FRAME_MAX_DATAGRAMS
            || limits.max_queue_bytes == 0
            || limits.min_parity_shards > 254
            || limits.min_fec_percentage > limits.max_fec_percentage
            || limits.max_fec_percentage > 255
            || !(1..=VIDEO_FRAME_MAX_BLOCKS as u32).contains(&limits.max_fec_blocks)
            || limits.fec_policy_flags & !VIDEO_FRAME_ALLOW_LARGE_UNPROTECTED != 0
            || limits.reserved != 0
            || (limits.min_fec_percentage == 0 && limits.min_parity_shards != 0)
            || limits.max_wire_bytes == 0
            || self.framed_bytes.len == 0
            || self.framed_bytes.len > MAX_VIDEO_ACCESS_UNIT_BYTES
            || self.framed_bytes.ptr.is_null()
            || self.block_count == 0
            || self.block_count > limits.max_fec_blocks
        {
            return Err(invalid());
        }
        let shard_payload = (limits.packet_size - 16) as usize;
        let data_count = self
            .framed_bytes
            .len
            .checked_add(8)
            .ok_or_else(invalid)?
            .div_ceil(shard_payload);
        let data_count = u32::try_from(data_count).map_err(|_| invalid())?;
        if data_count > limits.max_datagrams
            || self.critical_prefix_shards > data_count
            || (self.critical_prefix_shards != 0
                && limits.allowed_extensions & VIDEO_FRAME_CRITICAL_PREFIX == 0)
        {
            return Err(invalid());
        }
        if self.record_starts.len != 0 {
            if limits.allowed_extensions & VIDEO_FRAME_RECORD_START == 0
                || self.record_starts.len != data_count as usize
                || self.record_starts.ptr.is_null()
            {
                return Err(invalid());
            }
            // SAFETY: caller guarantees the record-start allocation is live and immutable.
            let flags = unsafe {
                std::slice::from_raw_parts(self.record_starts.ptr, self.record_starts.len)
            };
            if flags.iter().any(|flag| *flag > 1) {
                return Err(invalid());
            }
        }
        let mut offset = 0u32;
        let mut datagrams = 0u32;
        let mut field_work = 0u64;
        let mut has_unprotected = false;
        let mut has_protected = false;
        for (index, block) in self.blocks.iter().enumerate() {
            if index >= self.block_count as usize {
                if block.first_data_shard != 0
                    || block.data_shards != 0
                    || block.parity_shards != 0
                    || block.percentage != 0
                {
                    return Err(invalid());
                }
                continue;
            }
            if block.first_data_shard != offset
                || block.data_shards == 0
                || block.data_shards > 1023
                || block.percentage > limits.max_fec_percentage
            {
                return Err(invalid());
            }
            has_unprotected |= block.parity_shards == 0;
            has_protected |= block.parity_shards != 0;
            if block.parity_shards != 0 && block.percentage < limits.min_fec_percentage {
                return Err(invalid());
            }
            let expected = u64::from(block.data_shards)
                .checked_mul(u64::from(block.percentage))
                .ok_or_else(invalid)?
                .div_ceil(100);
            if expected != u64::from(block.parity_shards)
                || (block.parity_shards > 0 && block.parity_shards < limits.min_parity_shards)
                || (block.parity_shards > 0
                    && block
                        .data_shards
                        .checked_add(block.parity_shards)
                        .ok_or_else(invalid)?
                        > 255)
            {
                return Err(invalid());
            }
            offset = offset.checked_add(block.data_shards).ok_or_else(invalid)?;
            datagrams = datagrams
                .checked_add(block.data_shards)
                .and_then(|x| x.checked_add(block.parity_shards))
                .ok_or_else(invalid)?;
            field_work = field_work
                .checked_add(
                    u64::from(block.data_shards)
                        * u64::from(block.parity_shards)
                        * u64::from(limits.packet_size + 16),
                )
                .ok_or_else(invalid)?;
        }
        if offset != data_count
            || datagrams > limits.max_datagrams
            || field_work > VIDEO_FRAME_MAX_FIELD_WORK
        {
            return Err(invalid());
        }
        if limits.min_fec_percentage != 0 && has_unprotected {
            let largest =
                largest_protected_data(limits.min_fec_percentage, limits.min_parity_shards)
                    .ok_or_else(invalid)?;
            let capacity = largest
                .checked_mul(limits.max_fec_blocks)
                .ok_or_else(invalid)?;
            if limits.fec_policy_flags & VIDEO_FRAME_ALLOW_LARGE_UNPROTECTED == 0
                || has_protected
                || data_count <= capacity
            {
                return Err(invalid());
            }
        }
        let queue_bytes =
            u64::from(datagrams) * u64::from(limits.packet_size + 16 + limits.crypto_overhead);
        let wire_bytes = queue_bytes + u64::from(datagrams) * u64::from(limits.network_overhead);
        if queue_bytes > u64::from(limits.max_queue_bytes) || wire_bytes > limits.max_wire_bytes {
            return Err(invalid());
        }
        Ok(VideoPlanReservation {
            data_shards: data_count,
            datagrams,
            queue_bytes,
            wire_bytes,
        })
    }
}

/// Maximum data shards that fit one protected 255-shard block under the
/// minimum trusted policy, considering every representable wire percentage.
fn largest_protected_data(min_percentage: u32, min_parity: u32) -> Option<u32> {
    (1u32..255).rev().find(|data| {
        let rate_for_minimum = if min_parity == 0 {
            0
        } else {
            ((min_parity - 1) * 100 / data) + 1
        };
        let rate = min_percentage.max(rate_for_minimum);
        rate <= 255 && data + (data * rate).div_ceil(100) <= 255
    })
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VideoFramePlanSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const VideoFramePlan) -> Status,
}

/// `plan` is synchronous and retry-safe: it must not commit policy state or
/// retain a pointer. It forwards a rejected sink status; BUSY consumes nothing.
#[repr(C)]
pub struct VideoFramingApi {
    pub base: NativeInterface,
    pub plan: unsafe extern "C" fn(
        *mut c_void,
        u64,
        *const NegotiatedVideoUnit,
        *const VideoFramingLimits,
        VideoFramePlanSink,
    ) -> Status,
}

/// Optional v2.1 capability check. Success applies to the supplied identities
/// and host limits only; it is not evidence of peer or hardware support.
#[repr(C)]
pub struct VideoFramingApiV2_1 {
    pub base: VideoFramingApi,
    pub probe: unsafe extern "C" fn(
        *mut c_void,
        *const EncoderFormatId,
        *const EncoderFormatId,
        *const VideoFramingLimits,
    ) -> Status,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FRAME_CLOCK_STREAM_RELATIVE;
    use amaterasu_abi::BorrowedBytes;

    fn id(value: u64) -> EncoderFormatId {
        EncoderFormatId {
            namespace_id: 42,
            value,
            schema: 1,
        }
    }
    fn limits() -> VideoFramingLimits {
        VideoFramingLimits {
            size: size_of::<VideoFramingLimits>() as u32,
            version: VIDEO_FRAMING_LIMITS_VERSION,
            packet_size: 1024,
            crypto_overhead: 32,
            network_overhead: 28,
            allowed_extensions: VIDEO_FRAME_RECORD_START | VIDEO_FRAME_CRITICAL_PREFIX,
            max_datagrams: VIDEO_FRAME_MAX_DATAGRAMS,
            max_queue_bytes: 8 * 1024 * 1024,
            min_parity_shards: 1,
            min_fec_percentage: 50,
            max_fec_percentage: 100,
            max_fec_blocks: 4,
            fec_policy_flags: 0,
            reserved: 0,
            max_wire_bytes: 8 * 1024 * 1024,
        }
    }
    fn unit(bytes: &[u8]) -> NegotiatedVideoUnit {
        NegotiatedVideoUnit {
            size: size_of::<NegotiatedVideoUnit>() as u32,
            version: 1,
            stream: 5,
            generation: 7,
            offer_id: 9,
            codec: id(3),
            framing: id(4),
            access_unit_id: 1,
            submission_id: 2,
            source_id: 6,
            config_generation: 8,
            timestamp_us: 42,
            clock: FrameClockMetadata {
                version: 1,
                size: size_of::<FrameClockMetadata>() as u32,
                domain: FRAME_CLOCK_STREAM_RELATIVE,
                reserved: 0,
                epoch_id: [1; 16],
            },
            flags: VIDEO_UNIT_KEYFRAME,
            reserved: 0,
            data: BorrowedBytes::new(bytes),
        }
    }
    fn plan<'a>(data: &'a [u8], starts: &'a [u8]) -> VideoFramePlan {
        let mut blocks = [VideoFecBlock::default(); VIDEO_FRAME_MAX_BLOCKS];
        blocks[0] = VideoFecBlock {
            first_data_shard: 0,
            data_shards: 2,
            parity_shards: 1,
            percentage: 50,
        };
        VideoFramePlan {
            size: size_of::<VideoFramePlan>() as u32,
            version: 1,
            framed_bytes: BorrowedBytes::new(data),
            record_starts: BorrowedBytes::new(starts),
            critical_prefix_shards: 1,
            block_count: 1,
            blocks,
        }
    }
    #[test]
    fn unit_identity_epoch_and_eos_are_checked() {
        let bytes = [1, 2];
        let mut value = unit(&bytes);
        assert!(value.validate_for(5, 7, 9, id(3), id(4)).is_ok());
        assert!(value.validate_for(5, 7, 9, id(3), id(5)).is_err());
        assert!(value.validate_for(5, 8, 9, id(3), id(4)).is_err());
        value.clock.epoch_id = [0; 16];
        assert!(value.validate_for(5, 7, 9, id(3), id(4)).is_err());
        value = unit(&bytes);
        value.flags = VIDEO_UNIT_END_OF_STREAM;
        assert!(value.validate_for(5, 7, 9, id(3), id(4)).is_err());
        value.data = BorrowedBytes::new(&[]);
        assert!(value.validate_for(5, 7, 9, id(3), id(4)).is_ok());
    }
    #[test]
    fn plan_reserves_wire_and_crypto_before_admission() {
        let data = vec![7; 1200];
        let starts = [1, 0];
        let plan = plan(&data, &starts);
        let reservation = unsafe { plan.validate(&limits()) }.unwrap();
        assert_eq!(
            reservation,
            VideoPlanReservation {
                data_shards: 2,
                datagrams: 3,
                queue_bytes: 3216,
                wire_bytes: 3300
            }
        );
        let mut bounded = limits();
        bounded.max_wire_bytes = reservation.wire_bytes - 1;
        assert!(unsafe { plan.validate(&bounded) }.is_err());
        bounded = limits();
        bounded.max_queue_bytes = reservation.queue_bytes as u32 - 1;
        assert!(unsafe { plan.validate(&bounded) }.is_err());
    }
    #[test]
    fn plan_rejects_unnegotiated_flags_holes_and_bad_parity() {
        let data = vec![7; 1200];
        let starts = [1, 0];
        let mut plan = plan(&data, &starts);
        let mut bounded = limits();
        bounded.allowed_extensions = 0;
        assert!(unsafe { plan.validate(&bounded) }.is_err());
        plan.blocks[0].first_data_shard = 1;
        assert!(unsafe { plan.validate(&limits()) }.is_err());
        plan.blocks[0].first_data_shard = 0;
        plan.blocks[0].parity_shards = 2;
        assert!(unsafe { plan.validate(&limits()) }.is_err());
        plan.blocks[0].parity_shards = 1;
        plan.blocks[1].data_shards = 1;
        assert!(unsafe { plan.validate(&limits()) }.is_err());
        plan.blocks[1].data_shards = 0;
        let invalid_starts = [1, 2];
        plan.record_starts = BorrowedBytes::new(&invalid_starts);
        assert!(unsafe { plan.validate(&limits()) }.is_err());
    }
    #[test]
    fn fec_policy_rejects_zero_and_below_minimum_protection() {
        let data = vec![7; 9 * 1008 - 7];
        let mut plan = plan(&data, &[]);
        plan.critical_prefix_shards = 0;
        plan.blocks[0].data_shards = 10;
        let mut policy = limits();
        policy.min_fec_percentage = 20;
        policy.min_parity_shards = 2;
        policy.max_fec_percentage = 255;
        plan.blocks[0].percentage = 0;
        plan.blocks[0].parity_shards = 0;
        assert!(unsafe { plan.validate(&policy) }.is_err());
        plan.blocks[0].percentage = 15;
        plan.blocks[0].parity_shards = 2;
        assert!(unsafe { plan.validate(&policy) }.is_err());
        plan.blocks[0].percentage = 20;
        plan.blocks[0].parity_shards = 2;
        assert!(unsafe { plan.validate(&policy) }.is_ok());
        let small = vec![1; 1200];
        plan.framed_bytes = BorrowedBytes::new(&small);
        plan.blocks[0].data_shards = 2;
        plan.blocks[0].percentage = 20;
        plan.blocks[0].parity_shards = 1;
        assert!(unsafe { plan.validate(&policy) }.is_err());
    }

    #[test]
    fn only_explicit_large_frame_fallback_can_omit_parity() {
        let small = vec![1; 1200];
        let large = vec![1; 899 * 1008 - 7];
        let mut policy = limits();
        policy.min_fec_percentage = 20;
        policy.min_parity_shards = 2;
        policy.max_fec_percentage = 255;
        let mut plan = plan(&large, &[]);
        plan.critical_prefix_shards = 0;
        plan.blocks[0] = VideoFecBlock {
            first_data_shard: 0,
            data_shards: 900,
            parity_shards: 0,
            percentage: 0,
        };
        assert!(unsafe { plan.validate(&policy) }.is_err());
        policy.fec_policy_flags = VIDEO_FRAME_ALLOW_LARGE_UNPROTECTED;
        assert!(unsafe { plan.validate(&policy) }.is_ok());
        plan.framed_bytes = BorrowedBytes::new(&small);
        plan.blocks[0].data_shards = 2;
        assert!(unsafe { plan.validate(&policy) }.is_err());
        plan.framed_bytes = BorrowedBytes::new(&large);
        plan.blocks[0] = VideoFecBlock {
            first_data_shard: 0,
            data_shards: 2,
            parity_shards: 2,
            percentage: 100,
        };
        plan.blocks[1] = VideoFecBlock {
            first_data_shard: 2,
            data_shards: 898,
            parity_shards: 0,
            percentage: 0,
        };
        plan.block_count = 2;
        assert!(unsafe { plan.validate(&policy) }.is_err());
        plan.blocks[0].data_shards = u32::MAX;
        assert!(unsafe { plan.validate(&policy) }.is_err());
        policy.max_fec_blocks = 0;
        assert!(unsafe { plan.validate(&policy) }.is_err());
    }

    #[test]
    fn fallback_capacity_matches_exhaustive_wire_rates() {
        for min_percentage in [1, 20, 100, 255] {
            for min_parity in [0, 1, 2, 100, 254] {
                let expected = (1u32..255).rev().find(|data| {
                    (min_percentage..=255).any(|percentage| {
                        let parity = (data * percentage).div_ceil(100);
                        parity >= min_parity && data + parity <= 255
                    })
                });
                assert_eq!(largest_protected_data(min_percentage, min_parity), expected);
            }
        }
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn framing_tables_match_public_c_layout() {
        assert_eq!(size_of::<VideoPlanSelection>(), 128);
        assert_eq!(std::mem::offset_of!(VideoPlanSelection, codec), 48);
        assert_eq!(std::mem::offset_of!(VideoPlanSelection, clock_epoch), 96);
        assert_eq!(
            std::mem::offset_of!(VideoPlanSelection, metadata_required),
            120
        );
        assert_eq!(size_of::<NegotiatedVideoPlanTransportApi>(), 64);
        assert_eq!(
            std::mem::offset_of!(NegotiatedVideoPlanTransportApi, submit),
            48
        );
        assert_eq!(size_of::<VideoFramingApiV2_1>(), 48);
        assert_eq!(std::mem::offset_of!(VideoFramingApiV2_1, probe), 40);
        assert_eq!(size_of::<NegotiatedVideoUnit>(), 176);
        assert_eq!(std::mem::offset_of!(NegotiatedVideoUnit, clock), 120);
        assert_eq!(std::mem::offset_of!(NegotiatedVideoUnit, data), 160);
        assert_eq!(size_of::<VideoFecBlock>(), 16);
        assert_eq!(size_of::<VideoFramingLimits>(), 64);
        assert_eq!(std::mem::offset_of!(VideoFramingLimits, max_wire_bytes), 40);
        assert_eq!(
            std::mem::offset_of!(VideoFramingLimits, min_fec_percentage),
            48
        );
        assert_eq!(size_of::<VideoFramePlan>(), 112);
        assert_eq!(size_of::<VideoFramingApi>(), 40);
        assert_eq!(size_of::<NegotiatedVideoApi>(), 40);
        assert_eq!(size_of::<NegotiatedVideoTransportApi>(), 48);
    }
}
