//! Optional provider-owned input-to-ready timing. All counters are scoped to one
//! session epoch; they never imply device-only time or client delivery.
use crate::{EncoderApiV1_2, EncoderProviderApi, VideoApiV1_3};
use amaterasu_abi::{STATUS_INVALID_ARGUMENT, Status};
use serde::Serialize;
use std::{ffi::c_void, mem::size_of};

pub const ENCODER_TIMING_INTERFACE_MINOR: u32 = 3;
pub const ENCODER_PROVIDER_TIMING_INTERFACE_MINOR: u32 = 1;
pub const VIDEO_TIMING_INTERFACE_MINOR: u32 = 4;
pub const MAX_ENCODE_LATENCY_US: u64 = 60_000_000;

/// v1 cumulative measurements from provider input submission to complete owned
/// coded output becoming ready. Includes provider conversion, queueing and codec
/// work; excludes waiting for a consumer to poll, sink retries, transport,
/// capture and presentation. Providers timestamp readiness when output is made,
/// never at a later receive callback. Unmatched or unmeasured frames add nothing.
/// `epoch` is nonzero, stable for a session, and changes before any counter reset.
/// Zero completed_frames means no measured output, not zero latency. Counters
/// must remain exact: stop adding samples before overflow, never wrap/saturate.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct EncodeTimingSnapshotV1 {
    pub version: u32,
    pub size: u32,
    pub epoch: u64,
    pub completed_frames: u64,
    pub total_us: u64,
    pub min_us: u64,
    pub max_us: u64,
}

impl EncodeTimingSnapshotV1 {
    pub fn empty(epoch: u64) -> Self {
        Self {
            version: 1,
            size: size_of::<Self>() as u32,
            epoch,
            completed_frames: 0,
            total_us: 0,
            min_us: 0,
            max_us: 0,
        }
    }

    pub fn validate(&self) -> Result<(), Status> {
        let valid = self.version == 1
            && self.size >= size_of::<Self>() as u32
            && self.epoch != 0
            && self.max_us <= MAX_ENCODE_LATENCY_US
            && if self.completed_frames == 0 {
                self.total_us == 0 && self.min_us == 0 && self.max_us == 0
            } else {
                self.min_us <= self.max_us
                    && u128::from(self.total_us)
                        >= u128::from(self.completed_frames) * u128::from(self.min_us)
                    && u128::from(self.total_us)
                        <= u128::from(self.completed_frames) * u128::from(self.max_us)
            };
        valid.then_some(()).ok_or(STATUS_INVALID_ARGUMENT)
    }

    /// Adds an actual completed-frame measurement. A zero duration is valid.
    /// Rejected samples leave the exact cumulative snapshot unchanged.
    pub fn record(&mut self, elapsed_us: u64) -> bool {
        if elapsed_us > MAX_ENCODE_LATENCY_US {
            return false;
        }
        let (Some(count), Some(total)) = (
            self.completed_frames.checked_add(1),
            self.total_us.checked_add(elapsed_us),
        ) else {
            return false;
        };
        self.min_us = if self.completed_frames == 0 {
            elapsed_us
        } else {
            self.min_us.min(elapsed_us)
        };
        self.max_us = self.max_us.max(elapsed_us);
        self.completed_frames = count;
        self.total_us = total;
        true
    }

    /// Interval mean in milliseconds. Empty, reset, or invalid intervals remain
    /// unavailable. Bounds also reject inconsistent deltas from a replacement.
    pub fn mean_ms_since(self, previous: Self) -> Option<f64> {
        self.validate().ok()?;
        previous.validate().ok()?;
        if self.epoch != previous.epoch {
            return None;
        }
        let count = self
            .completed_frames
            .checked_sub(previous.completed_frames)?;
        let total = self.total_us.checked_sub(previous.total_us)?;
        if count == 0 || u128::from(total) > u128::from(count) * u128::from(MAX_ENCODE_LATENCY_US) {
            return None;
        }
        Some(total as f64 / count as f64 / 1000.0)
    }
}

/// Optional additive tables. Reads initialize the full v1 output only on OK;
/// BUSY/PENDING/UNSUPPORTED provide no sample and consume no output or work.
/// They authorize the session against the binding's caller just like receive.
#[repr(C)]
pub struct EncoderApiV1_3 {
    pub base: EncoderApiV1_2,
    pub encode_timing:
        unsafe extern "C" fn(*mut c_void, u64, *mut EncodeTimingSnapshotV1) -> Status,
}
#[repr(C)]
pub struct EncoderProviderApiV1_1 {
    pub base: EncoderProviderApi,
    pub encode_timing:
        unsafe extern "C" fn(*mut c_void, u64, *mut EncodeTimingSnapshotV1) -> Status,
}
#[repr(C)]
pub struct VideoApiV1_4 {
    pub base: VideoApiV1_3,
    pub encode_timing:
        unsafe extern "C" fn(*mut c_void, u64, *mut EncodeTimingSnapshotV1) -> Status,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn means_preserve_unknown_zero_resets_and_exact_bounds() {
        let empty = EncodeTimingSnapshotV1::empty(1);
        assert_eq!(empty.mean_ms_since(empty), None);
        let mut current = empty;
        assert!(current.record(0));
        assert_eq!(current.mean_ms_since(empty), Some(0.0));
        assert!(current.record(4000));
        assert_eq!(current.mean_ms_since(empty), Some(2.0));
        assert_eq!(
            current.mean_ms_since(EncodeTimingSnapshotV1::empty(2)),
            None
        );
        let before = current;
        assert!(!current.record(MAX_ENCODE_LATENCY_US + 1));
        assert_eq!(current, before);
        assert!(
            EncodeTimingSnapshotV1 {
                total_us: 1,
                ..empty
            }
            .validate()
            .is_err()
        );
        assert!(
            EncodeTimingSnapshotV1 {
                total_us: MAX_ENCODE_LATENCY_US + 1,
                min_us: 0,
                max_us: MAX_ENCODE_LATENCY_US,
                completed_frames: 1,
                ..empty
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn table_extensions_preserve_the_legacy_prefixes() {
        use std::mem::offset_of;
        assert_eq!(
            offset_of!(EncoderApiV1_3, encode_timing),
            size_of::<EncoderApiV1_2>()
        );
        assert_eq!(
            offset_of!(EncoderProviderApiV1_1, encode_timing),
            size_of::<EncoderProviderApi>()
        );
        assert_eq!(
            offset_of!(VideoApiV1_4, encode_timing),
            size_of::<VideoApiV1_3>()
        );
        assert_eq!(size_of::<EncodeTimingSnapshotV1>(), 48);
    }
}
