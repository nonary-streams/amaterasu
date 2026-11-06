use crate::model::{CaptureResult, Frame, Output};
use amaterasu_media::CaptureGeometry;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct TouchSurface {
    pub width: u32,
    pub height: u32,
    pub logical_width: u32,
    pub logical_height: u32,
}

impl TouchSurface {
    pub fn valid(self) -> bool {
        self.width != 0 && self.height != 0 && self.logical_width != 0 && self.logical_height != 0
    }
}

pub trait CaptureSession: Send {
    fn read(&mut self, wait_ms: u32) -> CaptureResult<Option<Frame>>;
    fn source_info(&mut self) -> CaptureResult<Option<crate::format::SourceInfo>> {
        Ok(None)
    }
    fn validate_source(&mut self) -> CaptureResult<()> {
        Ok(())
    }
    fn geometry(&mut self) -> CaptureResult<Option<CaptureGeometry>> {
        Ok(None)
    }
    fn touch_surface(&mut self) -> CaptureResult<Option<TouchSurface>> {
        Ok(None)
    }
    fn device_adapter(&self) -> Value {
        json!({})
    }
}

pub trait Backend: Send {
    fn supported(&self) -> bool;
    fn enumerate(&self) -> CaptureResult<Vec<Output>>;
    fn open(&self, output: &str) -> CaptureResult<Box<dyn CaptureSession>>;
    /// Read-only source format proof; no frame pool or capture worker is opened.
    #[cfg(any(windows, test))]
    fn verify_source_format(
        &self,
        _output: &str,
        format: crate::format::CaptureFormat,
    ) -> CaptureResult<()> {
        if format == crate::format::CaptureFormat::default() {
            Ok(())
        } else {
            Err(crate::model::CaptureError::new(
                "unsupported_format",
                "high-depth source proof unavailable",
            ))
        }
    }
    fn open_with_format(
        &self,
        output: &str,
        format: crate::format::CaptureFormat,
    ) -> CaptureResult<Box<dyn CaptureSession>> {
        if format != crate::format::CaptureFormat::default() {
            return Err(crate::model::CaptureError::new(
                "unsupported_format",
                "this capture backend has no high-depth source implementation",
            ));
        }
        self.open(output)
    }
}

#[cfg(windows)]
#[path = "windows.rs"]
mod windows;
#[cfg(windows)]
pub use windows::WindowsBackend as NativeBackend;

#[cfg(not(windows))]
pub struct NativeBackend;

#[cfg(not(windows))]
impl NativeBackend {
    pub fn new(_: Option<bool>) -> Self {
        Self
    }
}
#[cfg(not(windows))]
impl Backend for NativeBackend {
    fn supported(&self) -> bool {
        false
    }

    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        Err(unsupported())
    }

    fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        Err(unsupported())
    }
}

#[cfg(not(windows))]
fn unsupported() -> crate::model::CaptureError {
    crate::model::CaptureError::new("unsupported_platform", "WGC capture requires Windows")
}
