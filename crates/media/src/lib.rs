//! Draft media interfaces. Buffers are borrowed for one synchronous callback.
//! No Rust allocator, collection, or reference crosses a module boundary.
use amaterasu_abi::{NativeInterface, STATUS_INVALID_ARGUMENT, Status};
use std::{ffi::c_void, mem::size_of};

mod audio;
pub use audio::*;
mod audio_encoder;
pub use audio_encoder::*;
mod control;
pub use control::*;
mod clock;
pub use clock::*;
mod color;
pub use color::*;
mod dmabuf;
pub use dmabuf::*;
mod codec;
pub use codec::*;
mod geometry;
pub use geometry::*;
mod transport;
pub use transport::*;

pub const INTERFACE_MAJOR: u32 = 1;
pub const INTERFACE_MINOR: u32 = 0;
/// Capture API v1.1 adds physical desktop geometry.
pub const CAPTURE_INTERFACE_MINOR: u32 = 1;
/// Capture API v1.2 adds per-delivery source freshness metadata.
pub const CAPTURE_FRESHNESS_INTERFACE_MINOR: u32 = 2;
/// Capture API v1.3 adds a separate WGC source timestamp to fresh deliveries.
pub const CAPTURE_PACING_TIMESTAMP_INTERFACE_MINOR: u32 = 3;
/// Capture API v1.4 adds borrowed DMA-BUF (GPU) frame delivery.
pub const CAPTURE_DMABUF_INTERFACE_MINOR: u32 = 4;
/// Capture API v1.5 adds a cheap query of the newest completed presentation.
pub const CAPTURE_PRESENTATION_INTERFACE_MINOR: u32 = 5;
/// Encoder API v1.1 adds DMA-BUF frame submission.
pub const ENCODER_DMABUF_INTERFACE_MINOR: u32 = 1;
pub const CAPTURE_FRAME_CACHED_REUSE: u32 = 0;
pub const CAPTURE_FRAME_FRESH: u32 = 1;
pub const PIXEL_BGRA8: u32 = 1;
pub const PIXEL_NV12: u32 = 2;
pub const CODEC_H264: u32 = 1;
pub const CODEC_HEVC: u32 = 2;
pub const CODEC_AV1: u32 = 3;
pub const FLAG_KEYFRAME: u32 = 1;
pub const FLAG_END_OF_STREAM: u32 = 2;
/// Last fragment of one complete coded picture. Earlier fragments share its
/// sequence and timestamp. This is separate from end-of-stream.
pub const FLAG_END_OF_ACCESS_UNIT: u32 = 4;
pub const MEDIA_PENDING: Status = 10;
pub const MEDIA_BUSY: Status = 11;
pub const MEDIA_CLOSED: Status = 12;
pub const MEDIA_UNSUPPORTED: Status = 13;
pub const MEDIA_BACKEND_ERROR: Status = 14;
pub const MAX_FRAME_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_PACKET_BYTES: usize = 16 * 1024 * 1024;

/// BGRA uses one plane; NV12 uses contiguous Y then interleaved UV planes with
/// the same stride. Timestamps are microseconds from the producer's stream epoch.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CpuFrame {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub stride: u32,
    pub data: *const u8,
    pub len: usize,
    pub timestamp_us: u64,
    pub sequence: u64,
}

impl CpuFrame {
    /// Checks metadata only. A valid shape does not establish pointer validity.
    pub fn validate(&self) -> Result<(), Status> {
        let expected = frame_size(self.width, self.height, self.format, self.stride)?;
        if self.data.is_null() || self.len != expected {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }

    /// # Safety
    /// The allocation must contain `len` initialized bytes, remain alive for the
    /// returned borrow, and not be mutated while that borrow exists.
    pub unsafe fn bytes(&self) -> Result<&[u8], Status> {
        self.validate()?;
        // SAFETY: the caller supplies the allocation and its borrowing guarantee.
        Ok(unsafe { std::slice::from_raw_parts(self.data, self.len) })
    }
}

pub fn frame_size(width: u32, height: u32, format: u32, stride: u32) -> Result<usize, Status> {
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let (minimum_stride, rows) = match format {
        PIXEL_BGRA8 => (width.checked_mul(4), Some(height)),
        PIXEL_NV12 if width.is_multiple_of(2) && height.is_multiple_of(2) => {
            (Some(width), height.checked_add(height / 2))
        }
        _ => return Err(STATUS_INVALID_ARGUMENT),
    };
    if stride < minimum_stride.ok_or(STATUS_INVALID_ARGUMENT)? {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let size = (stride as usize)
        .checked_mul(rows.ok_or(STATUS_INVALID_ARGUMENT)? as usize)
        .filter(|size| *size <= MAX_FRAME_BYTES)
        .ok_or(STATUS_INVALID_ARGUMENT)?;
    Ok(size)
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const CpuFrame) -> Status,
}

impl FrameSink {
    /// # Safety
    /// The sink context/callback and frame allocation must remain valid until
    /// this call returns. The callback must not retain the borrowed frame bytes.
    pub unsafe fn send(&self, frame: &CpuFrame) -> Status {
        if let Err(status) = frame.validate() {
            return status;
        }
        // SAFETY: callback lifetime and frame ownership are guaranteed by caller.
        unsafe { (self.write)(self.context, frame) }
    }
}

/// H.264 and HEVC packets use Annex B start codes, not length prefixes. AV1
/// packets are low-overhead OBU streams, each access unit one temporal unit
/// beginning with a temporal delimiter. An encoder
/// may emit several packets for one submitted sequence. The final fragment sets
/// FLAG_END_OF_ACCESS_UNIT. EOS carries no bytes and cannot terminate a partial AU.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncodedPacket {
    pub codec: u32,
    pub flags: u32,
    pub data: *const u8,
    pub len: usize,
    pub timestamp_us: u64,
    pub sequence: u64,
}

impl EncodedPacket {
    pub fn validate(&self) -> Result<(), Status> {
        if !matches!(self.codec, CODEC_H264 | CODEC_HEVC | CODEC_AV1)
            || self.flags & !(FLAG_KEYFRAME | FLAG_END_OF_STREAM | FLAG_END_OF_ACCESS_UNIT) != 0
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if self.flags & FLAG_END_OF_STREAM != 0 {
            if self.len != 0 || self.flags != FLAG_END_OF_STREAM {
                return Err(STATUS_INVALID_ARGUMENT);
            }
        } else if self.len == 0 || self.len > MAX_PACKET_BYTES || self.data.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }

    /// # Safety
    /// A nonempty packet must point to `len` initialized bytes that outlive the
    /// returned borrow. No writer may mutate those bytes during this borrow.
    pub unsafe fn bytes(&self) -> Result<&[u8], Status> {
        self.validate()?;
        if self.len == 0 {
            return Ok(&[]);
        }
        // SAFETY: nonempty allocation validity and lifetime are caller obligations.
        Ok(unsafe { std::slice::from_raw_parts(self.data, self.len) })
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PacketSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const EncodedPacket) -> Status,
}

impl PacketSink {
    /// # Safety
    /// Callback/context and packet data must remain valid for this call. The
    /// callback must copy anything it needs after returning.
    pub unsafe fn send(&self, packet: &EncodedPacket) -> Status {
        if let Err(status) = packet.validate() {
            return status;
        }
        // SAFETY: the caller guarantees callback and packet lifetimes.
        unsafe { (self.write)(self.context, packet) }
    }
}

#[repr(C)]
pub struct CaptureApi {
    pub base: NativeInterface,
    /// At most one frame callback; MEDIA_PENDING means no frame is ready.
    pub read_frame: unsafe extern "C" fn(*mut c_void, u64, FrameSink) -> Status,
    /// Returns physical desktop geometry for the currently owned capture stream.
    pub geometry: unsafe extern "C" fn(*mut c_void, u64, *mut CaptureGeometry) -> Status,
}

/// Additive v1.2 capture table. Bind the legacy prefix first, then read this
/// tail only when both the advertised minor and table size cover v1.2.
#[repr(C)]
pub struct CaptureApiV1_2 {
    pub base: NativeInterface,
    pub read_frame: unsafe extern "C" fn(*mut c_void, u64, FrameSink) -> Status,
    pub geometry: unsafe extern "C" fn(*mut c_void, u64, *mut CaptureGeometry) -> Status,
    /// Reports whether this delivery came from a newly captured source frame
    /// or reused cached pixels. Freshness describes source acquisition only;
    /// it does not guarantee encoder acceptance.
    pub read_frame_with_freshness:
        unsafe extern "C" fn(*mut c_void, u64, FreshFrameSinkV1_2) -> Status,
}

/// Additive v1.3 table. Its first four fields preserve the complete v1.2 table
/// layout; the new callback carries source pacing time separately from the
/// timestamp that providers may rewrite for encoder submission.
#[repr(C)]
pub struct CaptureApiV1_3 {
    pub base: NativeInterface,
    pub read_frame: unsafe extern "C" fn(*mut c_void, u64, FrameSink) -> Status,
    pub geometry: unsafe extern "C" fn(*mut c_void, u64, *mut CaptureGeometry) -> Status,
    pub read_frame_with_freshness:
        unsafe extern "C" fn(*mut c_void, u64, FreshFrameSinkV1_2) -> Status,
    pub read_frame_with_capture_timestamp:
        unsafe extern "C" fn(*mut c_void, u64, FreshFrameSinkV1_3) -> Status,
}

/// Additive v1.4 table. Its first five fields preserve the complete v1.3
/// layout. Providers whose frames live in GPU memory deliver them as borrowed
/// DMA-BUF descriptors; MEDIA_UNSUPPORTED means this stream has no GPU frames
/// and the consumer should use a CPU callback instead.
#[repr(C)]
pub struct CaptureApiV1_4 {
    pub base: NativeInterface,
    pub read_frame: unsafe extern "C" fn(*mut c_void, u64, FrameSink) -> Status,
    pub geometry: unsafe extern "C" fn(*mut c_void, u64, *mut CaptureGeometry) -> Status,
    pub read_frame_with_freshness:
        unsafe extern "C" fn(*mut c_void, u64, FreshFrameSinkV1_2) -> Status,
    pub read_frame_with_capture_timestamp:
        unsafe extern "C" fn(*mut c_void, u64, FreshFrameSinkV1_3) -> Status,
    pub read_dmabuf_frame: unsafe extern "C" fn(*mut c_void, u64, DmaBufFrameSink) -> Status,
}

/// Additive v1.5 table. Its first six fields preserve the complete v1.4
/// layout. `latest_presentation` reports the newest completed presentation
/// without delivering a frame, so a consumer can push each new presentation
/// as soon as it lands instead of sampling on a fixed timer. It never blocks.
#[repr(C)]
pub struct CaptureApiV1_5 {
    pub base: NativeInterface,
    pub read_frame: unsafe extern "C" fn(*mut c_void, u64, FrameSink) -> Status,
    pub geometry: unsafe extern "C" fn(*mut c_void, u64, *mut CaptureGeometry) -> Status,
    pub read_frame_with_freshness:
        unsafe extern "C" fn(*mut c_void, u64, FreshFrameSinkV1_2) -> Status,
    pub read_frame_with_capture_timestamp:
        unsafe extern "C" fn(*mut c_void, u64, FreshFrameSinkV1_3) -> Status,
    pub read_dmabuf_frame: unsafe extern "C" fn(*mut c_void, u64, DmaBufFrameSink) -> Status,
    pub latest_presentation:
        unsafe extern "C" fn(*mut c_void, u64, *mut PresentationStatus) -> Status,
}

/// Newest completed presentation of one capture stream. `sequence` is the
/// value a later `DmaBufFrame` for that presentation carries; zero means no
/// presentation has completed yet. `timestamp_us` uses the stream epoch.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentationStatus {
    pub sequence: u64,
    pub timestamp_us: u64,
}

/// The v1.0 prefix retained so consumers can bind older capture providers
/// without reading the optional v1.1 geometry callback past their table.
#[repr(C)]
pub struct CaptureApiPrefix {
    pub base: NativeInterface,
    pub read_frame: unsafe extern "C" fn(*mut c_void, u64, FrameSink) -> Status,
}

pub const CAPTURE_API_V1_0_SIZE: u32 = size_of::<CaptureApiPrefix>() as u32;
pub const CAPTURE_API_V1_1_SIZE: u32 = size_of::<CaptureApi>() as u32;
pub const CAPTURE_API_V1_2_SIZE: u32 = size_of::<CaptureApiV1_2>() as u32;
pub const CAPTURE_API_V1_3_SIZE: u32 = size_of::<CaptureApiV1_3>() as u32;
pub const CAPTURE_API_V1_4_SIZE: u32 = size_of::<CaptureApiV1_4>() as u32;
pub const CAPTURE_API_V1_5_SIZE: u32 = size_of::<CaptureApiV1_5>() as u32;

/// Receives one borrowed frame and its source freshness in a single callback.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FreshFrameSinkV1_2 {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const CpuFrame, u32) -> Status,
}

impl FreshFrameSinkV1_2 {
    /// # Safety
    /// The sink context/callback and frame allocation must remain valid until
    /// this call returns. The callback must not retain borrowed frame bytes.
    pub unsafe fn send(&self, frame: &CpuFrame, freshness: u32) -> Status {
        if let Err(status) = frame.validate() {
            return status;
        }
        if !matches!(freshness, CAPTURE_FRAME_CACHED_REUSE | CAPTURE_FRAME_FRESH) {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: callback lifetime and frame ownership are guaranteed by caller.
        unsafe { (self.write)(self.context, frame, freshness) }
    }
}

/// Receives one borrowed v1.3 frame together with source freshness. The
/// capture pacing timestamp has the same microsecond stream epoch as the
/// source timestamp that WGC recorded when it acquired the frame.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CaptureFrameV1_3 {
    pub frame: CpuFrame,
    pub capture_pacing_timestamp_us: u64,
}

/// Synchronous v1.3 frame sink. The frame bytes and descriptor are borrowed
/// only through `write`; consumers must copy any data retained afterward.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FreshFrameSinkV1_3 {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const CaptureFrameV1_3, u32) -> Status,
}

impl FreshFrameSinkV1_3 {
    /// # Safety
    /// The sink context/callback and frame allocation must remain valid until
    /// this call returns. The callback must not retain borrowed frame bytes.
    pub unsafe fn send(&self, frame: &CaptureFrameV1_3, freshness: u32) -> Status {
        if let Err(status) = frame.frame.validate() {
            return status;
        }
        if !matches!(freshness, CAPTURE_FRAME_CACHED_REUSE | CAPTURE_FRAME_FRESH) {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: callback lifetime and frame ownership are guaranteed by caller.
        unsafe { (self.write)(self.context, frame, freshness) }
    }
}

#[repr(C)]
pub struct EncoderApi {
    pub base: NativeInterface,
    /// STATUS_OK means copied/accepted. MEDIA_BUSY accepts nothing; retry later.
    pub submit: unsafe extern "C" fn(*mut c_void, u64, *const CpuFrame, u32) -> Status,
    /// At most one packet callback. A rejected sink must not consume its packet.
    pub receive: unsafe extern "C" fn(*mut c_void, u64, PacketSink) -> Status,
    /// Stops accepting input and flushes queued packets before one EOS marker.
    pub drain: unsafe extern "C" fn(*mut c_void, u64) -> Status,
}

/// Additive v1.1 encoder table. The v1.0 prefix is unchanged. `submit_dmabuf`
/// follows `submit`'s contract: STATUS_OK means the encoder has duplicated
/// what it needs from the borrowed descriptors, MEDIA_BUSY accepts nothing,
/// and MEDIA_UNSUPPORTED means this stream's encoder cannot import the
/// frame's format or modifier. Fourth argument: nonzero forces an IDR frame.
#[repr(C)]
pub struct EncoderApiV1_1 {
    pub base: NativeInterface,
    pub submit: unsafe extern "C" fn(*mut c_void, u64, *const CpuFrame, u32) -> Status,
    pub receive: unsafe extern "C" fn(*mut c_void, u64, PacketSink) -> Status,
    pub drain: unsafe extern "C" fn(*mut c_void, u64) -> Status,
    pub submit_dmabuf: unsafe extern "C" fn(*mut c_void, u64, *const DmaBufFrame, u32) -> Status,
}

pub const ENCODER_API_V1_0_SIZE: u32 = size_of::<EncoderApi>() as u32;
pub const ENCODER_API_V1_1_SIZE: u32 = size_of::<EncoderApiV1_1>() as u32;

/// Encoded output from an owned capture-to-encode stream. Configuration and
/// lifecycle remain on the control service; consumers only receive packets here.
#[repr(C)]
pub struct VideoApi {
    pub base: NativeInterface,
    pub receive: unsafe extern "C" fn(*mut c_void, u64, PacketSink) -> Status,
}

/// v1.1 tail: authoritative current capture geometry for the same owned stream.
/// The v1.0 receive prefix remains unchanged. STATUS_OK initializes the output;
/// MEDIA_UNSUPPORTED means the source has no authoritative desktop mapping.
#[repr(C)]
pub struct VideoApiV1_1 {
    pub base: NativeInterface,
    pub receive: unsafe extern "C" fn(*mut c_void, u64, PacketSink) -> Status,
    pub geometry: unsafe extern "C" fn(*mut c_void, u64, *mut CaptureGeometry) -> Status,
}

#[cfg(test)]
mod tests;
