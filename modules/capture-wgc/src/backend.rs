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
    /// Stop requesting frames and poll resource retirement without waiting for
    /// a worker. True permits the controller to destroy the session; worker
    /// implementations must join their thread before reporting completion.
    fn quiesce(&mut self) -> CaptureResult<bool> {
        Ok(true)
    }
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

pub trait Backend: Send + Sync + 'static {
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

#[cfg(all(not(windows), test))]
type TestOpener = dyn Fn(&str) -> CaptureResult<Box<dyn CaptureSession>> + Send + Sync;

#[cfg(not(windows))]
pub struct NativeBackend {
    #[cfg(test)]
    opener: Option<std::sync::Arc<TestOpener>>,
}

#[cfg(not(windows))]
impl NativeBackend {
    pub fn new(_: Option<bool>) -> Self {
        Self {
            #[cfg(test)]
            opener: None,
        }
    }

    #[cfg(test)]
    pub fn with_opener(
        opener: impl Fn(&str) -> CaptureResult<Box<dyn CaptureSession>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            opener: Some(std::sync::Arc::new(opener)),
        }
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

    fn open(&self, output: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        #[cfg(test)]
        if let Some(opener) = &self.opener {
            return opener(output);
        }
        let _ = output;
        Err(unsupported())
    }
}

#[cfg(not(windows))]
fn unsupported() -> crate::model::CaptureError {
    crate::model::CaptureError::new("unsupported_platform", "WGC capture requires Windows")
}
