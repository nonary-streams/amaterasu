//! Optional per-presentation host timing reported by a capture provider.
use crate::{CaptureApiV1_11, INTERFACE_MAJOR};
use amaterasu_abi::{AbiHeader, Status};
use std::{ffi::c_void, mem::size_of};

/// Capture API v1.12 adds `frame_timing`.
pub const CAPTURE_FRAME_TIMING_INTERFACE_MINOR: u32 = 12;

/// Host timing of one presentation, keyed by the stream `sequence` that a
/// delivered `DmaBufFrame` (or resource frame) for it carries.
///
/// `processing_started_host_us` is the host monotonic clock in microseconds
/// (`host_monotonic_micros`: CLOCK_MONOTONIC on Linux, the performance counter
/// on Windows) at which the provider began handling that presentation, before
/// it fetched, exported or copied the frame. Zero means unknown.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureFrameTimingV1 {
    pub sequence: u64,
    pub processing_started_host_us: u64,
}

/// Additive v1.12 tail, preserving the complete v1.11 capture prefix.
///
/// `frame_timing` reports the timing of one recent presentation of a stream the
/// bound caller owns. It never blocks, delivers no frame and starts no GPU work.
/// STATUS_OK initializes the output with the requested sequence; any other
/// status leaves it unspecified. MEDIA_PENDING means no record for that
/// sequence (not yet stored, never stored or already evicted from the
/// provider's bounded history); MEDIA_UNSUPPORTED means this stream supplies no
/// timing. Consumers must treat every failure as "unknown" and keep their own
/// measurement rather than fail the stream.
#[repr(C)]
pub struct CaptureApiV1_12 {
    pub base: CaptureApiV1_11,
    pub frame_timing:
        unsafe extern "C" fn(*mut c_void, u64, u64, *mut CaptureFrameTimingV1) -> Status,
}

pub const CAPTURE_API_V1_12_SIZE: u32 = size_of::<CaptureApiV1_12>() as u32;

/// The table advertises v1.12 and is large enough to hold `frame_timing`.
pub fn capture_frame_timing_available(header: AbiHeader) -> bool {
    header.major == INTERFACE_MAJOR
        && header.minor >= CAPTURE_FRAME_TIMING_INTERFACE_MINOR
        && header.size >= CAPTURE_API_V1_12_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    #[test]
    fn frame_timing_tail_preserves_the_v1_11_prefix() {
        assert_eq!(size_of::<CaptureFrameTimingV1>(), 16);
        assert_eq!(
            offset_of!(CaptureFrameTimingV1, processing_started_host_us),
            8
        );
        assert_eq!(offset_of!(CaptureApiV1_12, base), 0);
        assert_eq!(
            offset_of!(CaptureApiV1_12, frame_timing),
            size_of::<CaptureApiV1_11>()
        );
    }

    #[test]
    fn availability_requires_major_minor_and_size() {
        let header = |major, minor, size| AbiHeader { size, major, minor };
        assert!(capture_frame_timing_available(header(
            INTERFACE_MAJOR,
            12,
            CAPTURE_API_V1_12_SIZE
        )));
        assert!(!capture_frame_timing_available(header(
            INTERFACE_MAJOR,
            11,
            CAPTURE_API_V1_12_SIZE
        )));
        assert!(!capture_frame_timing_available(header(
            INTERFACE_MAJOR,
            12,
            size_of::<CaptureApiV1_11>() as u32
        )));
        assert!(!capture_frame_timing_available(header(
            INTERFACE_MAJOR + 1,
            12,
            CAPTURE_API_V1_12_SIZE
        )));
    }
}
