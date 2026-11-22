//! Nonblocking CPU frame fitting. The selected provider owns its worker and pixels.
use crate::{CpuFrame, FrameSink};
use amaterasu_abi::{NativeInterface, Status};
use std::ffi::c_void;

pub const CPU_FRAME_FIT_MAJOR: u32 = 1;
pub const CPU_FRAME_FIT_MINOR: u32 = 0;

/// Target image size and SDR matrix/range for a provider that returns NV12.
/// The output may be BGRA if the provider leaves color conversion to the encoder.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CpuFrameFitTarget {
    pub width: u32,
    pub height: u32,
    pub bt709: u32,
    pub full_range: u32,
}

impl CpuFrameFitTarget {
    pub fn valid(self) -> bool {
        (1..=8192).contains(&self.width)
            && (1..=8192).contains(&self.height)
            && self.bt709 <= 1
            && self.full_range <= 1
    }
}

/// At most one accepted job per stream. Submit copies borrowed input on OK
/// and accepts nothing on BUSY. UNSUPPORTED explicitly attests the original
/// frame already satisfies the target. Receive delivers at most one synchronous
/// callback, retains the output on BUSY and returns PENDING until ready.
/// Cancel fences pending output for that stream without waiting on its worker.
/// Calls on a binding are serialized and may not reenter it from a callback.
#[repr(C)]
pub struct CpuFrameFitApi {
    pub base: NativeInterface,
    pub submit:
        unsafe extern "C" fn(*mut c_void, u64, *const CpuFrame, CpuFrameFitTarget) -> Status,
    pub receive: unsafe extern "C" fn(*mut c_void, u64, FrameSink) -> Status,
    pub cancel: unsafe extern "C" fn(*mut c_void, u64),
}
