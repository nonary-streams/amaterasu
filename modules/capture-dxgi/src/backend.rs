use crate::model::{CaptureResult, Frame, Output};
use amaterasu_media::CaptureGeometry;
use serde_json::{Value, json};

pub trait CaptureSession: Send {
    fn read(&mut self, wait_ms: u32) -> CaptureResult<Option<Frame>>;
    fn geometry(&mut self) -> CaptureResult<Option<CaptureGeometry>> {
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
}

#[cfg(windows)]
#[path = "windows.rs"]
mod windows;
#[cfg(windows)]
pub use windows::WindowsBackend as NativeBackend;

#[cfg(not(windows))]
pub struct NativeBackend;
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
    crate::model::CaptureError::new("unsupported_platform", "DXGI capture requires Windows")
}
