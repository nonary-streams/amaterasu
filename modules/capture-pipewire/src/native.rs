//! PipeWire's immutable mailbox through the common bounded resource SDK.
use crate::{
    backend::Backend,
    controller::{Controller, Reader},
};
use amaterasu_media::*;
use amaterasu_module_sdk::{
    abi::{NativeServiceFactory, Status},
    capture::{
        CpuPresentationReceipt, CpuSourceDescription, CpuSourceFactory, CpuSourceFormat,
        CpuSourcePresentation, CpuSourceProvider,
    },
    media::CpuPixels,
};
use std::sync::{Arc, Mutex};
pub type Shared<B> = Arc<Mutex<Controller<B>>>;
pub struct Factory<B: Backend> {
    pub api: NativeServiceFactory,
    source: Box<CpuSourceFactory<Bridge<B>>>,
}
impl<B: Backend + 'static> Factory<B> {
    pub fn new(controller: Shared<B>) -> Box<Self> {
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
    controller: Shared<B>,
}
impl<B: Backend> Bridge<B> {
    fn reader(&self, caller: &str, stream: u64) -> Result<Reader, Status> {
        let c = match self.controller.try_lock() {
            Ok(c) => c,
            Err(std::sync::TryLockError::WouldBlock) => return Err(MEDIA_BUSY),
            Err(_) => return Err(MEDIA_BACKEND_ERROR),
        };
        c.reader(caller, stream).map_err(|e| {
            if e == "not_found" {
                MEDIA_CLOSED
            } else {
                MEDIA_BACKEND_ERROR
            }
        })
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
        let reader = self.reader(caller, stream)?;
        let (width, height) = reader.mailbox.dimensions()?.ok_or(MEDIA_PENDING)?;
        Ok(CpuSourceDescription {
            generation: 1,
            width,
            height,
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
        let reader = self.reader(caller, stream)?;
        let delivery = match reader.mailbox.claim()? {
            Some(d) => d,
            None => return Ok(None),
        };
        let permit = reader.admission.enter().ok_or(MEDIA_CLOSED)?;
        self.reader(caller, stream)?;
        let pixels = delivery.frame.clone();
        let frame = &pixels;
        let timestamp_us = frame.timestamp_us;
        let sequence = frame.sequence;
        let presentation = CpuSourcePresentation {
            receipt: Some(CpuPresentationReceipt::new(move |status| {
                let _permit = permit;
                delivery.finish(status);
            })),
            width: frame.width,
            height: frame.height,
            stride: frame.stride,
            pixel_format: PIXEL_BGRA8,
            color: sdr(),
            bytes: CpuPixels::from_owner(pixels.clone()),
            timestamp_us,
            capture_pacing_timestamp_us: timestamp_us,
            sequence,
            freshness: CAPTURE_FRAME_FRESH,
        };
        // Read immutable identity from the shared owner before transferring it.
        Ok(Some(presentation))
    }
    fn geometry(&self, caller: &str, stream: u64) -> Result<CaptureGeometry, Status> {
        self.reader(caller, stream)?
            .geometry
            .ok_or(MEDIA_UNSUPPORTED)
    }
}
#[cfg(test)]
mod tests;
