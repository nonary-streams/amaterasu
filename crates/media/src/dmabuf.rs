//! GPU frames shared as Linux DMA-BUF descriptors.
//!
//! A `DmaBufFrame` describes one complete presentation already owned by the
//! producer. Every descriptor in it is borrowed only until the synchronous
//! callback or submit call returns. A consumer that keeps the pixels must
//! duplicate the descriptors (`F_DUPFD_CLOEXEC`) before returning, and must
//! finish reading the source promptly: an open DMA-BUF pins the allocation but
//! does not stop a compositor from rendering into it again later.
use amaterasu_abi::{STATUS_INVALID_ARGUMENT, Status};
use std::ffi::c_void;

pub const DMABUF_MAX_PLANES: usize = 4;
/// Largest accepted width or height of a GPU frame.
pub const DMABUF_MAX_DIMENSION: u32 = 8192;
/// `DRM_FORMAT_MOD_INVALID`: the producer did not allocate with an explicit modifier.
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// 8-bit SDR pixels: BT.709 primaries with the sRGB transfer function.
pub const DMABUF_COLOR_SRGB: u32 = 1;
/// 10-bit HDR pixels: BT.2020 primaries with the SMPTE ST 2084 (PQ) transfer.
pub const DMABUF_COLOR_BT2020_PQ: u32 = 2;

/// One memory plane. `fd` is -1 for planes past `DmaBufFrame::plane_count`.
/// `acquire_fence_fd` is a sync_file that signals when the producer's GPU
/// writes are complete, or -1 when the frame is already idle.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaBufPlane {
    pub fd: i32,
    pub acquire_fence_fd: i32,
    pub offset: u32,
    pub pitch: u32,
}

impl DmaBufPlane {
    pub const UNUSED: Self = Self {
        fd: -1,
        acquire_fence_fd: -1,
        offset: 0,
        pitch: 0,
    };
}

/// A borrowed DMA-BUF presentation. `fourcc` and `modifier` use DRM values.
/// Timestamps are microseconds from the producer's stream epoch, as for
/// `CpuFrame`; `sequence` increases once per distinct presentation.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaBufFrame {
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub plane_count: u32,
    pub modifier: u64,
    pub planes: [DmaBufPlane; DMABUF_MAX_PLANES],
    pub color: u32,
    pub reserved: u32,
    pub timestamp_us: u64,
    pub sequence: u64,
}

impl DmaBufFrame {
    /// Checks metadata only. A valid shape does not prove that each descriptor
    /// is open or refers to a buffer large enough for the described layout.
    pub fn validate(&self) -> Result<(), Status> {
        let plane_count = self.plane_count as usize;
        if self.width == 0
            || self.height == 0
            || self.width > DMABUF_MAX_DIMENSION
            || self.height > DMABUF_MAX_DIMENSION
            || self.fourcc == 0
            || plane_count == 0
            || plane_count > DMABUF_MAX_PLANES
            || !matches!(self.color, DMABUF_COLOR_SRGB | DMABUF_COLOR_BT2020_PQ)
            || self.reserved != 0
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let (used, unused) = self.planes.split_at(plane_count);
        let used_valid = used
            .iter()
            .all(|plane| plane.fd >= 0 && plane.acquire_fence_fd >= -1 && plane.pitch != 0);
        let unused_empty = unused.iter().all(|plane| *plane == DmaBufPlane::UNUSED);
        if !used_valid || !unused_empty {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }

    pub fn used_planes(&self) -> &[DmaBufPlane] {
        &self.planes[..(self.plane_count as usize).min(DMABUF_MAX_PLANES)]
    }
}

/// Synchronous DMA-BUF sink. Descriptors are borrowed only through `write`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DmaBufFrameSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const DmaBufFrame, u32) -> Status,
}

impl DmaBufFrameSink {
    /// # Safety
    /// The sink context/callback and every descriptor in `frame` must remain
    /// valid until this call returns. The callback must not keep the borrowed
    /// descriptors; it duplicates any it needs.
    pub unsafe fn send(&self, frame: &DmaBufFrame, freshness: u32) -> Status {
        if let Err(status) = frame.validate() {
            return status;
        }
        if !matches!(
            freshness,
            crate::CAPTURE_FRAME_CACHED_REUSE | crate::CAPTURE_FRAME_FRESH
        ) {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: callback lifetime and descriptor ownership are guaranteed by caller.
        unsafe { (self.write)(self.context, frame, freshness) }
    }
}
