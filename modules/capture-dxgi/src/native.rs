//! The provider's caller-scoped controller behind the public CPU capture SDK.
use crate::{backend::Backend, controller::Controller};
use amaterasu_media::*;
use amaterasu_module_sdk::{
    abi::{NativeServiceFactory, Status},
    capture::{
        CpuSourceDescription, CpuSourceFactory, CpuSourceFormat, CpuSourcePresentation,
        CpuSourceProvider,
        high_depth::{YuvRange, p010_color},
    },
};
use std::sync::{Arc, Mutex};
type SharedController<B> = Arc<Mutex<Controller<B>>>;
pub struct Factory<B: Backend> {
    pub api: NativeServiceFactory,
    _source: Box<CpuSourceFactory<Bridge<B>>>,
}
impl<B: Backend + 'static> Factory<B> {
    pub fn retirement_ready(&self) -> Result<bool, Status> {
        self._source.retirement_ready()
    }
    pub fn new(controller: SharedController<B>) -> Box<Self> {
        let source = CpuSourceFactory::new(Arc::new(Bridge { controller }));
        Box::new(Self {
            api: source.api,
            _source: source,
        })
    }
}
struct Bridge<B: Backend> {
    controller: SharedController<B>,
}
fn capture_error_status(error: &crate::model::CaptureError) -> Status {
    match error.code {
        "not_found" | "access_lost" | "device_lost" => MEDIA_CLOSED,
        "unsupported_platform"
        | "unsupported_format"
        | "geometry_unavailable"
        | "source_color_unavailable"
        | "source_range_mismatch" => MEDIA_UNSUPPORTED,
        _ => MEDIA_BACKEND_ERROR,
    }
}
impl<B: Backend> Bridge<B> {
    fn controller(&self) -> Result<std::sync::MutexGuard<'_, Controller<B>>, Status> {
        match self.controller.try_lock() {
            Ok(c) => Ok(c),
            Err(std::sync::TryLockError::WouldBlock) => Err(MEDIA_BUSY),
            Err(_) => Err(MEDIA_BACKEND_ERROR),
        }
    }
    fn legacy(&self, caller: &str, stream: u64) -> Result<Option<CpuSourcePresentation>, Status> {
        let mut controller = self.controller()?;
        let capture_format = controller
            .capture_format(caller, stream)
            .map_err(|e| capture_error_status(&e))?;
        if capture_format.ten_bit {
            return Err(MEDIA_UNSUPPORTED);
        }
        let Some(frame) = controller
            .read(caller, stream, 0)
            .map_err(|e| capture_error_status(&e))?
        else {
            return Ok(None);
        };
        if frame.surface.is_some() {
            return Err(MEDIA_UNSUPPORTED);
        }
        Ok(Some(CpuSourcePresentation {
            receipt: None,
            width: frame.width,
            height: frame.height,
            stride: frame.stride,
            pixel_format: PIXEL_BGRA8,
            color: if capture_format.explicit {
                sdr_color()
            } else {
                FrameColorMetadata::default()
            },
            bytes: Arc::new(frame.bytes).into(),
            timestamp_us: frame.timestamp_us,
            capture_pacing_timestamp_us: frame.timestamp_us,
            sequence: frame.sequence,
            freshness: CAPTURE_FRAME_FRESH,
        }))
    }
}
impl<B: Backend> CpuSourceProvider for Bridge<B> {
    fn describe(&self, caller: &str, stream: u64) -> Result<CpuSourceDescription, Status> {
        let mut c = self.controller()?;
        let info = c
            .source_info(caller, stream)
            .map_err(|e| capture_error_status(&e))?
            .ok_or(MEDIA_UNSUPPORTED)?;
        info.format
            .validate_dimensions(info.width, info.height)
            .map_err(|e| capture_error_status(&e))?;
        let formats = if info.format.ten_bit {
            vec![
                CpuSourceFormat {
                    pixel_format: PIXEL_P010,
                    color: p010_color(info.format.hdr, YuvRange::Limited),
                },
                CpuSourceFormat {
                    pixel_format: PIXEL_P010,
                    color: p010_color(info.format.hdr, YuvRange::Full),
                },
            ]
        } else {
            vec![CpuSourceFormat {
                pixel_format: PIXEL_BGRA8,
                color: if info.format.explicit {
                    sdr_color()
                } else {
                    FrameColorMetadata::default()
                },
            }]
        };
        Ok(CpuSourceDescription {
            generation: 1,
            width: info.width,
            height: info.height,
            formats,
            hdr: Some(HdrMetadata {
                enabled: u32::from(info.format.hdr),
                ..HdrMetadata::default()
            }),
            authoritative_color: info.format.explicit,
        })
    }
    fn read(
        &self,
        caller: &str,
        stream: u64,
        format: &CpuSourceFormat,
    ) -> Result<Option<CpuSourcePresentation>, Status> {
        if format.pixel_format == PIXEL_BGRA8 {
            return self.legacy(caller, stream);
        }
        let (frame, requested) = {
            let mut c = self.controller()?;
            let requested = c
                .capture_format(caller, stream)
                .map_err(|e| capture_error_status(&e))?;
            let frame = c
                .read(caller, stream, 0)
                .map_err(|e| capture_error_status(&e))?;
            (frame, requested)
        };
        let Some(frame) = frame else { return Ok(None) };
        if !requested.ten_bit || format.pixel_format != PIXEL_P010 {
            return Err(MEDIA_UNSUPPORTED);
        }
        let surface = frame.surface.ok_or(MEDIA_BACKEND_ERROR)?;
        let range = match format.color.range {
            FRAME_RANGE_LIMITED => YuvRange::Limited,
            FRAME_RANGE_FULL => YuvRange::Full,
            _ => return Err(MEDIA_UNSUPPORTED),
        };
        let pixels = surface.to_p010(requested.hdr, range)?;
        Ok(Some(CpuSourcePresentation {
            receipt: None,
            width: pixels.width,
            height: pixels.height,
            stride: pixels.stride,
            pixel_format: pixels.pixel_format,
            color: pixels.color,
            bytes: pixels.bytes.into(),
            timestamp_us: frame.timestamp_us,
            capture_pacing_timestamp_us: frame.timestamp_us,
            sequence: frame.sequence,
            freshness: CAPTURE_FRAME_FRESH,
        }))
    }
    fn read_legacy(
        &self,
        caller: &str,
        stream: u64,
    ) -> Result<Option<CpuSourcePresentation>, Status> {
        self.legacy(caller, stream)
    }
    fn geometry(&self, caller: &str, stream: u64) -> Result<CaptureGeometry, Status> {
        self.controller()?
            .geometry(caller, stream)
            .map_err(|e| capture_error_status(&e))?
            .ok_or(MEDIA_UNSUPPORTED)
    }
}

fn sdr_color() -> FrameColorMetadata {
    FrameColorMetadata {
        version: 1,
        size: size_of::<FrameColorMetadata>() as u32,
        primaries: FRAME_PRIMARIES_BT709,
        transfer: FRAME_TRANSFER_SRGB,
        matrix: FRAME_MATRIX_RGB,
        range: FRAME_RANGE_FULL,
    }
}
