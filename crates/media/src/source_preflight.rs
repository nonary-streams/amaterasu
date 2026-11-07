//! Optional read-only source admission on an authenticated capture binding.
use crate::CaptureApiV1_10;
use amaterasu_abi::{BorrowedBytes, ReplySink, Status};
use std::ffi::c_void;

pub const CAPTURE_SOURCE_PREFLIGHT_INTERFACE_MINOR: u32 = 11;

/// Additive capture v1.11 tail, preserving every earlier capture prefix.
/// The JSON request/reply follow the Source Preflight SDK contract. A query
/// observes capability and current readiness without opening a capture source,
/// creating/mutating a display, granting permission, or reserving native resources.
/// It creates no lease and never guarantees that a later open will succeed.
/// Input bytes and reply sink are borrowed only for the synchronous callback;
/// one bounded reply must be copied before the callback returns. Providers must
/// authenticate the binding's caller and may return BUSY without a reply.
#[repr(C)]
pub struct CaptureApiV1_11 {
    pub base: CaptureApiV1_10,
    pub source_preflight: unsafe extern "C" fn(*mut c_void, BorrowedBytes, ReplySink) -> Status,
}

pub const CAPTURE_API_V1_11_SIZE: u32 = std::mem::size_of::<CaptureApiV1_11>() as u32;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_preflight_preserves_the_complete_existing_capture_prefix() {
        assert_eq!(std::mem::offset_of!(CaptureApiV1_11, base), 0);
        assert_eq!(
            std::mem::offset_of!(CaptureApiV1_11, source_preflight),
            std::mem::size_of::<CaptureApiV1_10>()
        );
        assert_eq!(
            CAPTURE_API_V1_11_SIZE as usize,
            std::mem::size_of::<CaptureApiV1_10>() + std::mem::size_of::<usize>()
        );
    }
}
