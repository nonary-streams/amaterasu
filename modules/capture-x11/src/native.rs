//! Provider policy behind the shared bounded frame-resource SDK.
use crate::{backend::Backend, controller::Controller};
use amaterasu_media::*;
use amaterasu_module_sdk::{
    abi::{NativeServiceFactory, Status},
    capture::{
        CpuSourceDescription, CpuSourceFactory, CpuSourceFormat, CpuSourcePresentation,
        CpuSourceProvider,
    },
};
use std::sync::{Arc, Mutex};
type SharedController<B> = Arc<Mutex<Controller<B>>>;
pub struct Factory<B: Backend> {
    pub api: NativeServiceFactory,
    source: Box<CpuSourceFactory<Bridge<B>>>,
}
impl<B: Backend + 'static> Factory<B> {
    pub fn new(controller: SharedController<B>) -> Box<Self> {
        let source = CpuSourceFactory::new(Arc::new(Bridge { controller }));
        Box::new(Self {
            api: source.api,
            source,
        })
    }
    pub fn retirement_ready(&self) -> Result<bool, Status> {
        self.source.retirement_ready()
    }
}
struct Bridge<B: Backend> {
    controller: SharedController<B>,
}
impl<B: Backend> Bridge<B> {
    fn controller(&self) -> Result<std::sync::MutexGuard<'_, Controller<B>>, Status> {
        match self.controller.try_lock() {
            Ok(c) => Ok(c),
            Err(std::sync::TryLockError::WouldBlock) => Err(MEDIA_BUSY),
            Err(_) => Err(MEDIA_BACKEND_ERROR),
        }
    }
}
fn capture_status(error: &crate::model::CaptureError) -> Status {
    match error.code {
        "not_found" | "access_lost" | "device_lost" | "output_unavailable" => MEDIA_CLOSED,
        "unsupported_platform" | "unsupported_pixel_format" | "invalid_geometry" => {
            MEDIA_UNSUPPORTED
        }
        _ => MEDIA_BACKEND_ERROR,
    }
}
fn sdr() -> FrameColorMetadata {
    FrameColorMetadata {
        version: 1,
        size: size_of::<FrameColorMetadata>() as u32,
        primaries: FRAME_PRIMARIES_BT709,
        transfer: FRAME_TRANSFER_SRGB,
        matrix: FRAME_MATRIX_RGB,
        range: FRAME_RANGE_FULL,
    }
}
impl<B: Backend> CpuSourceProvider for Bridge<B> {
    fn describe(&self, caller: &str, stream: u64) -> Result<CpuSourceDescription, Status> {
        let g = self
            .controller()?
            .geometry(caller, stream)
            .map_err(|e| capture_status(&e))?;
        Ok(CpuSourceDescription {
            generation: 1,
            width: g.output.width,
            height: g.output.height,
            formats: vec![CpuSourceFormat {
                pixel_format: PIXEL_BGRA8,
                color: sdr(),
            }],
            hdr: None,
            authoritative_color: false,
        })
    }
    fn read(
        &self,
        caller: &str,
        stream: u64,
        format: &CpuSourceFormat,
    ) -> Result<Option<CpuSourcePresentation>, Status> {
        if format.pixel_format != PIXEL_BGRA8 {
            return Err(MEDIA_UNSUPPORTED);
        }
        let f = self
            .controller()?
            .read(caller, stream)
            .map_err(|e| capture_status(&e))?;
        Ok(Some(CpuSourcePresentation {
            receipt: None,
            width: f.width,
            height: f.height,
            stride: f.stride,
            pixel_format: PIXEL_BGRA8,
            color: sdr(),
            bytes: Arc::new(f.bytes).into(),
            timestamp_us: f.timestamp_us,
            capture_pacing_timestamp_us: f.timestamp_us,
            sequence: f.sequence,
            freshness: CAPTURE_FRAME_FRESH,
        }))
    }
    fn geometry(&self, caller: &str, stream: u64) -> Result<CaptureGeometry, Status> {
        self.controller()?
            .geometry(caller, stream)
            .map_err(|e| capture_status(&e))
    }
}
