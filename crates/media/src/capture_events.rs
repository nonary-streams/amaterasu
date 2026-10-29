//! Optional capture notifications. Pixels continue through the existing frame APIs.
use crate::{CaptureApiV1_6, INTERFACE_MAJOR};
use amaterasu_abi::{BorrowedBytes, Status};
use std::ffi::c_void;

pub const CAPTURE_EVENTS_INTERFACE_MINOR: u32 = 7;
pub const CAPTURE_SWAP_ACCEPTED: u32 = 1;
pub const CAPTURE_SWAP_REJECTED: u32 = 2;

/// Borrowed for `request_swap` only. Empty provider asks the consumer to choose;
/// empty output leaves source matching to the consumer. Neither grants access.
/// IDs are nonzero and unique within a capture stream. Provider aliases and
/// reason codes contain at most 64 ASCII letters/digits/`_`/`-`/`.`; output IDs
/// are UTF-8, at most 512 bytes, without control characters.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CaptureSwapRequest {
    pub id: u64,
    pub preferred_provider: BorrowedBytes,
    pub output: BorrowedBytes,
    pub reason: BorrowedBytes,
}

/// Consumer-owned callbacks, valid from successful subscribe until unsubscribe
/// returns or control close completes. Callbacks may run on capture threads,
/// concurrently with the consumer. They must be bounded, contain panics, and
/// only record a notification and wake the consumer: never call a provider or
/// encode inline. They must not retain request pointers.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CaptureEventSink {
    pub context: *mut c_void,
    pub frame_ready: unsafe extern "C" fn(*mut c_void),
    /// OK records the request; BUSY records nothing, permitting an identical retry.
    pub request_swap: unsafe extern "C" fn(*mut c_void, *const CaptureSwapRequest) -> Status,
    /// Terminal capture failure, separate from an advisory swap request. Reason
    /// is a borrowed bounded identifier. No new publications follow; frames
    /// already acquired by consumers keep their normal completion lifetime.
    pub stopped: unsafe extern "C" fn(*mut c_void, BorrowedBytes),
}

/// Additive v1.7, preserving the complete v1.6 capture prefix. At most one
/// subscription per stream; ownership follows the authenticated bound caller.
/// Subscribe must announce an already available frame/pending swap, closing the
/// registration race. Notifications may coalesce: consumers acquire the latest
/// frame using the existing CPU/DMA-BUF callbacks, on the module thread.
#[repr(C)]
pub struct CaptureApiV1_7 {
    pub base: CaptureApiV1_6,
    pub subscribe_events: unsafe extern "C" fn(*mut c_void, u64, CaptureEventSink) -> Status,
    /// Lifetime barrier: stop callbacks and wait for admitted notifications to
    /// finish before returning. Idempotent even after control close. Must also
    /// run when releasing a binding. Never call it from an event callback.
    pub unsubscribe_events: unsafe extern "C" fn(*mut c_void, u64),
    /// ACCEPTED means the coordinator accepted the transition, not that another
    /// provider has already opened. Rejection leaves capture running. `reason`
    /// is a borrowed bounded reason code. Only the exact pending ID may settle.
    pub resolve_swap: unsafe extern "C" fn(*mut c_void, u64, u64, u32, BorrowedBytes) -> Status,
}

pub const CAPTURE_API_V1_7_SIZE: u32 = size_of::<CaptureApiV1_7>() as u32;

pub fn capture_events_available(header: amaterasu_abi::AbiHeader) -> bool {
    header.major == INTERFACE_MAJOR
        && header.minor >= CAPTURE_EVENTS_INTERFACE_MINOR
        && header.size >= CAPTURE_API_V1_7_SIZE
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    #[test]
    fn event_tail_matches_c_and_preserves_every_legacy_capture_field() {
        assert_eq!(size_of::<CaptureEventSink>(), 32);
        assert_eq!(size_of::<CaptureSwapRequest>(), 56);
        assert_eq!(CAPTURE_API_V1_7_SIZE, 112);
        assert_eq!(std::mem::offset_of!(CaptureApiV1_7, base), 0);
        assert_eq!(
            std::mem::offset_of!(CaptureApiV1_7, subscribe_events),
            size_of::<CaptureApiV1_6>()
        );
        assert_eq!(std::mem::offset_of!(CaptureApiV1_7, unsubscribe_events), 96);
        assert_eq!(std::mem::offset_of!(CaptureApiV1_7, resolve_swap), 104);
    }
}
