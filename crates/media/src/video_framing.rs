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

mod gamestream;

pub const VIDEO_UNIT_VERSION: u32 = 1;
/// FEC policy fields extend the v1 limits prefix and require framing API v2.
pub const VIDEO_FRAMING_LIMITS_VERSION: u32 = 2;
pub const VIDEO_FRAMING_INTERFACE_MAJOR: u32 = 2;
pub const VIDEO_FRAMING_INTERFACE_MINOR: u32 = 2;
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

/// One contiguous block in data-shard order. The selected transport defines
/// how the wire percentage maps to parity shards for its FEC scheme.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VideoFecBlock {
    pub first_data_shard: u32,
    pub data_shards: u32,
    pub parity_shards: u32,
    pub percentage: u32,
}

/// Host-supplied bounds from trusted session composition and peer negotiation.
/// The selected transport defines its packet layout within the negotiated
/// packet size. For GameStream this includes the NV header and excludes RTP.
/// Crypto and network overhead are provider-supplied bytes per datagram.
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

/// Version 2 carries the exact bounded negotiated wire identity inline.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VideoPlanSelectionV2 {
    pub base: VideoPlanSelection,
    pub payload_type: u32,
    pub encoding_name_len: u32,
    pub encoding_name: [u8; 32],
}

impl VideoPlanSelectionV2 {
    /// Returns the exact wire name only for canonical, bounded descriptors.
    pub fn wire_name(&self) -> Option<&[u8]> {
        let len = usize::try_from(self.encoding_name_len).ok()?;
        if self.base.size as usize != size_of::<Self>()
            || self.base.version != 2
            || !(99..=127).contains(&self.payload_type)
            || !(1..=32).contains(&len)
            || self.encoding_name[len..].iter().any(|byte| *byte != 0)
            || !self.encoding_name[..len]
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'-')
        {
            return None;
        }
        Some(&self.encoding_name[..len])
    }

    /// Produce the internal v1 descriptor after validating the v2 wire fields.
    pub fn base_v1(&self) -> Option<VideoPlanSelection> {
        self.wire_name()?;
        let mut base = self.base;
        base.size = size_of::<VideoPlanSelection>() as u32;
        base.version = 1;
        Some(base)
    }
}

#[repr(C)]
pub struct NegotiatedVideoPlanTransportApiV2 {
    pub base: NativeInterface,
    pub probe: unsafe extern "C" fn(
        *mut c_void,
        u64,
        *const VideoPlanSelectionV2,
        *mut VideoFramingLimits,
    ) -> Status,
    pub prepare: unsafe extern "C" fn(
        *mut c_void,
        u64,
        *const VideoPlanSelectionV2,
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

/// Bytes in the agreed framing, before transport headers and encryption.
/// For GameStream they also exclude the eight-byte short frame header.
/// `record_starts` has one byte per data shard (0 or 1), or is empty.
/// The transport owns packet layout, FEC execution, sequence, encryption and
/// delivery.
/// Pointers are synchronous borrows.
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
pub struct VideoPlanCounts {
    pub data_shards: u32,
    pub datagrams: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub struct VideoPlanReservation {
    pub data_shards: u32,
    pub datagrams: u32,
    pub queue_bytes: u64,
    pub wire_bytes: u64,
}

impl VideoFramePlan {
    /// Check the common ABI shape and negotiated bounds before copying or
    /// forwarding a plan. The selected transport verifies its actual packet
    /// layout, FEC work and complete queue and wire reservations at admission.
    ///
    /// # Safety
    /// Both nonempty borrowed allocations must be initialized and immutable
    /// until this function returns. Structural checks cannot prove pointer validity.
    pub unsafe fn validate_shape(
        &self,
        limits: &VideoFramingLimits,
    ) -> Result<VideoPlanCounts, Status> {
        if self.size as usize != size_of::<Self>()
            || self.version != 1
            || limits.size as usize != size_of::<VideoFramingLimits>()
            || limits.version != VIDEO_FRAMING_LIMITS_VERSION
            || !(256..=1392).contains(&limits.packet_size)
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
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let counts = self.validate_blocks(limits)?;
        let bytes = self.framed_bytes.len as u64;
        let datagrams = u64::from(counts.datagrams);
        let min_queue = bytes + datagrams * u64::from(limits.crypto_overhead);
        let min_wire = min_queue + datagrams * u64::from(limits.network_overhead);
        if bytes > u64::from(counts.data_shards) * u64::from(limits.packet_size)
            || min_queue > u64::from(limits.max_queue_bytes)
            || min_wire > limits.max_wire_bytes
            || self.critical_prefix_shards > counts.data_shards
            || (self.critical_prefix_shards != 0
                && limits.allowed_extensions & VIDEO_FRAME_CRITICAL_PREFIX == 0)
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if self.record_starts.len != 0 {
            if limits.allowed_extensions & VIDEO_FRAME_RECORD_START == 0
                || self.record_starts.len != counts.data_shards as usize
                || self.record_starts.ptr.is_null()
            {
                return Err(STATUS_INVALID_ARGUMENT);
            }
            // SAFETY: caller guarantees the record-start allocation is live and immutable.
            let flags = unsafe {
                std::slice::from_raw_parts(self.record_starts.ptr, self.record_starts.len)
            };
            if flags.iter().any(|flag| *flag > 1) {
                return Err(STATUS_INVALID_ARGUMENT);
            }
        }
        Ok(counts)
    }

    fn validate_blocks(&self, limits: &VideoFramingLimits) -> Result<VideoPlanCounts, Status> {
        let mut data_shards = 0u32;
        let mut datagrams = 0u32;
        let mut has_unprotected = false;
        let mut has_protected = false;
        let (active, unused) = self.blocks.split_at(self.block_count as usize);
        for block in active {
            if block.first_data_shard != data_shards
                || block.data_shards == 0
                || block.data_shards > limits.max_datagrams
                || block.parity_shards > limits.max_datagrams
                || block.percentage > limits.max_fec_percentage
                || (block.parity_shards == 0) != (block.percentage == 0)
            {
                return Err(STATUS_INVALID_ARGUMENT);
            }
            if block.parity_shards == 0 {
                has_unprotected = true;
            } else {
                has_protected = true;
                if block.percentage < limits.min_fec_percentage
                    || block.parity_shards < limits.min_parity_shards
                {
                    return Err(STATUS_INVALID_ARGUMENT);
                }
            }
            data_shards += block.data_shards;
            datagrams += block.data_shards + block.parity_shards;
        }
        if datagrams > limits.max_datagrams
            || unused.iter().any(|block| {
                block.first_data_shard != 0
                    || block.data_shards != 0
                    || block.parity_shards != 0
                    || block.percentage != 0
            })
            || (limits.min_fec_percentage != 0
                && has_unprotected
                && (limits.fec_policy_flags & VIDEO_FRAME_ALLOW_LARGE_UNPROTECTED == 0
                    || has_protected))
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(VideoPlanCounts {
            data_shards,
            datagrams,
        })
    }
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

/// Pure, bounded conversion of a negotiated total link budget to encoder
/// payload bits per second. This includes the provider's framing overhead.
/// Pass three zero scalars to query budgeting capability for these descriptors.
#[repr(C)]
pub struct VideoFramingApiV2_2 {
    pub base: VideoFramingApiV2_1,
    pub encoder_bitrate: unsafe extern "C" fn(
        *mut c_void,
        *const EncoderFormatId,
        *const EncoderFormatId,
        *const VideoFramingLimits,
        u32, // wire kilobits per second
        u32, // frames per second
        u32, // audio payload bits per second
        *mut u32,
    ) -> Status,
}

#[cfg(test)]
mod tests {
    use super::{gamestream::largest_protected_data, *};
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
    fn shape_enforces_common_bounds_without_gamestream_packing() {
        let data = [7; 8];
        let starts = [1, 0];
        let plan = plan(&data, &starts);
        let mut bounds = limits();
        bounds.max_queue_bytes = 104;
        bounds.max_wire_bytes = 188;
        assert_eq!(
            unsafe { plan.validate_shape(&bounds) }.unwrap(),
            VideoPlanCounts {
                data_shards: 2,
                datagrams: 3,
            }
        );
        assert!(unsafe { plan.validate_gamestream(&bounds) }.is_err());

        let mut constrained = bounds;
        constrained.max_datagrams = 2;
        assert!(unsafe { plan.validate_shape(&constrained) }.is_err());
        constrained = bounds;
        constrained.max_queue_bytes -= 1;
        assert!(unsafe { plan.validate_shape(&constrained) }.is_err());
        constrained = bounds;
        constrained.max_wire_bytes -= 1;
        assert!(unsafe { plan.validate_shape(&constrained) }.is_err());
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
        assert_eq!(size_of::<VideoPlanSelectionV2>(), 168);
        assert_eq!(
            std::mem::offset_of!(VideoPlanSelectionV2, payload_type),
            128
        );
        assert_eq!(
            std::mem::offset_of!(VideoPlanSelectionV2, encoding_name),
            136
        );
        assert_eq!(size_of::<NegotiatedVideoPlanTransportApiV2>(), 64);
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
