//! Per-stream frame delivery shared between the controller and native reads.
//!
//! A read asks the stream's helper for the newest presentation, lends its
//! descriptors to exactly one sink callback, and closes them afterward.
use crate::admission::Admission;
use amaterasu_media::{
    CAPTURE_FRAME_CACHED_REUSE, CAPTURE_FRAME_FRESH, DMABUF_COLOR_SRGB, DMABUF_MAX_DIMENSION,
    DmaBufFrame, DmaBufPlane, MEDIA_BACKEND_ERROR, MEDIA_BUSY, MEDIA_CLOSED, MEDIA_PENDING,
    MEDIA_UNSUPPORTED,
};
use amaterasu_module_sdk::abi::{STATUS_OK, Status};
use amaterasu_vkms_frame_helper::{
    client::{ClientError, ExportedFrame, FrameExport},
    protocol::Failure,
};
use std::{
    os::fd::AsRawFd,
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
};

/// Exports frames for one CRTC. The production source is a helper process.
pub trait FrameSource: Send {
    fn frame(&mut self, crtc_id: u32) -> Result<FrameExport, ClientError>;
}

/// Why a stream stopped delivering; reported in status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamFault {
    /// The helper exited, timed out or broke protocol.
    HelperFailed,
    /// The driver refused CAP_SYS_ADMIN.
    PermissionDenied,
    /// The output disappeared or lost its CRTC.
    OutputLost(&'static str),
}

impl StreamFault {
    pub fn code(self) -> &'static str {
        match self {
            Self::HelperFailed => "frame_helper_failed",
            Self::PermissionDenied => "permission_denied",
            Self::OutputLost(code) => code,
        }
    }
}

pub struct StreamFrames {
    pub admission: Arc<Admission>,
    source: Mutex<Box<dyn FrameSource>>,
    crtc_id: AtomicU32,
    /// CLOCK_MONOTONIC nanoseconds at stream open: the stream epoch.
    epoch_ns: u64,
    last_timestamp_us: AtomicU64,
    /// Sequence of the last presentation a sink accepted; 0 before any.
    last_delivered_sequence: AtomicU64,
    delivered: AtomicU64,
    resolve_requested: AtomicBool,
    fault: Mutex<Option<StreamFault>>,
}

/// One frame lent to a sink. Dropping it closes every descriptor.
pub struct Lent {
    pub frame: DmaBufFrame,
    pub freshness: u32,
    _descriptors: ExportedFrame,
}

impl StreamFrames {
    pub fn new(source: Box<dyn FrameSource>, crtc_id: u32, epoch_ns: u64) -> Self {
        Self {
            admission: Arc::new(Admission::default()),
            source: Mutex::new(source),
            crtc_id: AtomicU32::new(crtc_id),
            epoch_ns,
            last_timestamp_us: AtomicU64::new(0),
            last_delivered_sequence: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            resolve_requested: AtomicBool::new(false),
            fault: Mutex::new(None),
        }
    }

    pub fn crtc_id(&self) -> u32 {
        self.crtc_id.load(Ordering::SeqCst)
    }

    pub fn retarget(&self, crtc_id: u32) {
        self.crtc_id.store(crtc_id, Ordering::SeqCst);
    }

    pub fn delivered(&self) -> u64 {
        self.delivered.load(Ordering::Relaxed)
    }

    /// Takes a pending request to re-resolve the output's CRTC.
    pub fn take_resolve_request(&self) -> bool {
        self.resolve_requested.swap(false, Ordering::SeqCst)
    }

    pub fn fault(&self) -> Option<StreamFault> {
        *self
            .fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn set_fault(&self, fault: StreamFault) {
        let mut current = self
            .fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        current.get_or_insert(fault);
    }

    /// Exports the newest presentation for one sink callback.
    pub fn lend(&self) -> Result<Lent, Status> {
        if let Some(fault) = self.fault() {
            return Err(match fault {
                StreamFault::OutputLost(_) => MEDIA_CLOSED,
                _ => MEDIA_BACKEND_ERROR,
            });
        }
        let exported = {
            let mut source = match self.source.try_lock() {
                Ok(source) => source,
                Err(TryLockError::WouldBlock) => return Err(MEDIA_BUSY),
                Err(TryLockError::Poisoned(_)) => return Err(MEDIA_BACKEND_ERROR),
            };
            source.frame(self.crtc_id())
        };
        match exported {
            Ok(FrameExport::Empty) => Err(MEDIA_PENDING),
            Ok(FrameExport::Ready(exported)) => self.describe(exported),
            Err(error) => Err(self.export_failure(&error)),
        }
    }

    /// Records the sink's verdict. Only an accepted presentation counts as
    /// delivered, so a rejected fresh frame stays fresh on the next read.
    pub fn finish(&self, lent: Lent, status: Status) -> Status {
        if status == STATUS_OK {
            self.last_delivered_sequence
                .fetch_max(lent.frame.sequence, Ordering::SeqCst);
            self.delivered.fetch_add(1, Ordering::Relaxed);
        }
        drop(lent);
        status
    }

    fn export_failure(&self, error: &ClientError) -> Status {
        match error {
            ClientError::Refused {
                failure: Failure::CrtcNotFound,
                ..
            } => {
                // The output may have moved to another CRTC; the controller
                // re-resolves it on its next refresh.
                self.resolve_requested.store(true, Ordering::SeqCst);
                MEDIA_PENDING
            }
            ClientError::Refused {
                failure: Failure::Busy | Failure::Interrupted,
                ..
            } => MEDIA_BUSY,
            ClientError::Refused {
                failure: Failure::PermissionDenied,
                ..
            } => {
                self.set_fault(StreamFault::PermissionDenied);
                MEDIA_BACKEND_ERROR
            }
            ClientError::Refused { .. } => MEDIA_BACKEND_ERROR,
            _ => {
                self.set_fault(StreamFault::HelperFailed);
                MEDIA_BACKEND_ERROR
            }
        }
    }

    fn describe(&self, exported: ExportedFrame) -> Result<Lent, Status> {
        let descriptor = exported.descriptor;
        if descriptor.width > DMABUF_MAX_DIMENSION || descriptor.height > DMABUF_MAX_DIMENSION {
            return Err(MEDIA_UNSUPPORTED);
        }
        let mut planes = [DmaBufPlane::UNUSED; amaterasu_media::DMABUF_MAX_PLANES];
        for (plane, buffer) in exported.dma_bufs.iter().enumerate() {
            planes[plane] = DmaBufPlane {
                fd: buffer.as_raw_fd(),
                acquire_fence_fd: exported.fences[plane]
                    .as_ref()
                    .map_or(-1, AsRawFd::as_raw_fd),
                offset: descriptor.offsets[plane],
                pitch: descriptor.pitches[plane],
            };
        }
        let freshness = if descriptor.sequence > self.last_delivered_sequence.load(Ordering::SeqCst)
        {
            CAPTURE_FRAME_FRESH
        } else {
            CAPTURE_FRAME_CACHED_REUSE
        };
        let frame = DmaBufFrame {
            width: descriptor.width,
            height: descriptor.height,
            fourcc: descriptor.fourcc,
            plane_count: descriptor.plane_count,
            modifier: descriptor.modifier,
            planes,
            color: DMABUF_COLOR_SRGB,
            reserved: 0,
            timestamp_us: self.stream_timestamp_us(descriptor.timestamp_ns),
            sequence: descriptor.sequence,
        };
        Ok(Lent {
            frame,
            freshness,
            _descriptors: exported,
        })
    }

    /// Converts a driver CLOCK_MONOTONIC presentation time to microseconds
    /// since stream open. Presentations completed before the stream opened
    /// map to zero, and the result never decreases.
    fn stream_timestamp_us(&self, presentation_ns: u64) -> u64 {
        let elapsed_us = presentation_ns.saturating_sub(self.epoch_ns) / 1_000;
        let previous = self
            .last_timestamp_us
            .fetch_max(elapsed_us, Ordering::SeqCst);
        previous.max(elapsed_us)
    }
}

#[cfg(test)]
mod tests;
