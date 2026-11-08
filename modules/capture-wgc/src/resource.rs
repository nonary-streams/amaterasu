//! Native-table adaptation uses the same public CPU-source SDK as replacements.
//! Raw WGC FP16 pixels stay owned until the selected delivery range is converted.
use crate::{backend::Backend, controller::Controller, format::CaptureFormat, model::CaptureError};
use amaterasu_media::*;
use amaterasu_module_sdk::{
    abi::Status,
    capture::high_depth::{RgbEncoding, RgbSurface, YuvRange, p010_color},
    capture::{CpuSourceDescription, CpuSourceFormat, CpuSourcePresentation, CpuSourceProvider},
};
use std::sync::{Arc, Mutex, MutexGuard};

pub struct Provider<B: Backend> {
    controller: Arc<Mutex<Controller<B>>>,
}
impl<B: Backend> Provider<B> {
    pub fn new(controller: Arc<Mutex<Controller<B>>>) -> Self {
        Self { controller }
    }
    fn controller(&self) -> Result<MutexGuard<'_, Controller<B>>, Status> {
        match self.controller.try_lock() {
            Ok(controller) => Ok(controller),
            Err(std::sync::TryLockError::WouldBlock) => Err(MEDIA_BUSY),
            Err(_) => Err(MEDIA_BACKEND_ERROR),
        }
    }
}

fn formats(format: CaptureFormat) -> Vec<CpuSourceFormat> {
    if format.ten_bit {
        [YuvRange::Limited, YuvRange::Full]
            .into_iter()
            .map(|range| CpuSourceFormat {
                pixel_format: PIXEL_P010,
                color: p010_color(format.hdr, range),
            })
            .collect()
    } else {
        vec![CpuSourceFormat {
            pixel_format: PIXEL_BGRA8,
            color: sdr_color(format),
        }]
    }
}
fn same_format(left: &CpuSourceFormat, right: &CpuSourceFormat) -> bool {
    left.pixel_format == right.pixel_format
        && [
            left.color.version,
            left.color.size,
            left.color.primaries,
            left.color.transfer,
            left.color.matrix,
            left.color.range,
        ] == [
            right.color.version,
            right.color.size,
            right.color.primaries,
            right.color.transfer,
            right.color.matrix,
            right.color.range,
        ]
}
fn status(error: CaptureError) -> Status {
    match error.code {
        "not_found" | "access_lost" | "device_lost" => MEDIA_CLOSED,
        "source_pending" => MEDIA_PENDING,
        "unsupported_platform"
        | "unsupported_format"
        | "geometry_unavailable"
        | "source_color_unavailable"
        | "source_range_mismatch" => MEDIA_UNSUPPORTED,
        _ => MEDIA_BACKEND_ERROR,
    }
}
impl<B: Backend> CpuSourceProvider for Provider<B> {
    fn describe(&self, caller: &str, stream: u64) -> Result<CpuSourceDescription, Status> {
        let mut controller = self.controller()?;
        let (format, info) = controller.source_info(caller, stream).map_err(status)?;
        // Revalidate the Open geometry before publishing resource offers or
        // claiming per-presentation provenance, including after conversion.
        controller.geometry(caller, stream).map_err(status)?;
        format
            .validate_dimensions(info.width, info.height)
            .map_err(status)?;
        Ok(CpuSourceDescription {
            generation: stream,
            width: info.width,
            height: info.height,
            formats: formats(format),
            // Scan-out provenance establishes HDR enablement; no mastering
            // metadata is fabricated from a display setting or encoder request.
            hdr: Some(HdrMetadata {
                enabled: u32::from(format.hdr),
                ..HdrMetadata::default()
            }),
            authoritative_color: format.color_proof,
        })
    }
    fn read(
        &self,
        caller: &str,
        stream: u64,
        requested: &CpuSourceFormat,
    ) -> Result<Option<CpuSourcePresentation>, Status> {
        let mut controller = self.controller()?;
        let (format, _) = controller.source_info(caller, stream).map_err(status)?;
        if !formats(format)
            .iter()
            .any(|candidate| same_format(candidate, requested))
        {
            return Err(MEDIA_UNSUPPORTED);
        }
        let Some(delivery) = controller
            .read_with_freshness(caller, stream, 0)
            .map_err(status)?
        else {
            return Ok(None);
        };
        // Frame owns its pixels independently of the controller/worker. The
        // public factory invokes foreign sinks after this lock is released.
        drop(controller);
        let frame = delivery.frame;
        let (stride, bytes, color) = if format.ten_bit {
            let range = match requested.color.range {
                FRAME_RANGE_FULL => YuvRange::Full,
                FRAME_RANGE_LIMITED => YuvRange::Limited,
                _ => return Err(MEDIA_UNSUPPORTED),
            };
            let source = RgbSurface {
                width: frame.width,
                height: frame.height,
                stride: frame.stride,
                encoding: RgbEncoding::Rgba16FloatScRgb,
                bytes: frame.bytes,
            };
            let pixels = source.to_p010(format.hdr, range)?;
            (pixels.stride, pixels.bytes, pixels.color)
        } else {
            (frame.stride, frame.bytes, sdr_color(format))
        };
        Ok(Some(CpuSourcePresentation {
            receipt: None,
            width: frame.width,
            height: frame.height,
            stride,
            pixel_format: requested.pixel_format,
            color,
            bytes: bytes.into(),
            timestamp_us: frame.timestamp_us,
            capture_pacing_timestamp_us: frame.capture_pacing_timestamp_us,
            sequence: frame.sequence,
            freshness: if delivery.is_fresh {
                CAPTURE_FRAME_FRESH
            } else {
                CAPTURE_FRAME_CACHED_REUSE
            },
        }))
    }
    fn delivery_failed(&self, caller: &str, stream: u64, _status: Status) {
        if let Ok(mut controller) = self.controller.try_lock() {
            let _ = controller.discard_cached_frame(caller, stream);
        }
    }
    fn geometry(&self, caller: &str, stream: u64) -> Result<CaptureGeometry, Status> {
        self.controller()?
            .geometry(caller, stream)
            .map_err(status)?
            .ok_or(MEDIA_UNSUPPORTED)
    }
}

#[cfg(test)]
mod tests;

fn sdr_color(format: CaptureFormat) -> FrameColorMetadata {
    if !format.color_proof {
        return FrameColorMetadata::default();
    }
    FrameColorMetadata {
        version: 1,
        size: size_of::<FrameColorMetadata>() as u32,
        primaries: FRAME_PRIMARIES_BT709,
        transfer: FRAME_TRANSFER_SRGB,
        matrix: FRAME_MATRIX_RGB,
        range: FRAME_RANGE_FULL,
    }
}
