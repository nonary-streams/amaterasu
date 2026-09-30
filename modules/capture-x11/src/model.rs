use amaterasu_media::CaptureGeometry;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_STREAMS: usize = 4;
pub const MAX_OUTPUTS: usize = 64;

#[derive(Debug)]
pub struct CaptureError {
    pub code: &'static str,
    pub message: String,
}

impl CaptureError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn payload(self) -> Value {
        json!({"ok":false,"error":{"code":self.code,"message":self.message}})
    }
}

pub type CaptureResult<T> = Result<T, CaptureError>;

#[derive(Clone, Debug, Serialize)]
pub struct Output {
    pub id: String,
    pub adapter: String,
    pub adapter_vendor_id: Option<u32>,
    pub adapter_device_id: Option<u32>,
    pub adapter_description: Option<String>,
    pub device_name: String,
    pub attached: bool,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub rotation: u32,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub timestamp_us: u64,
    pub sequence: u64,
    pub bytes: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Status,
    Enumerate {
        #[serde(default)]
        offset: Option<usize>,
        #[serde(default)]
        limit: Option<usize>,
    },
    Open {
        output: String,
        operation_id: Option<String>,
    },
    Lookup {
        operation_id: String,
    },
    Close {
        stream: u64,
    },
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct NativeAvailability {
    pub native_supported: bool,
    pub available: bool,
    pub reason: Option<&'static str>,
}

#[derive(Clone, Copy, Debug)]
pub struct DesktopGeometry {
    pub capture: CaptureGeometry,
}

pub fn frame_layout(width: u32, height: u32) -> CaptureResult<(u32, usize)> {
    let stride = width.checked_mul(4).ok_or_else(invalid_frame)?;
    let length = (stride as usize)
        .checked_mul(height as usize)
        .filter(|length| *length <= amaterasu_media::MAX_FRAME_BYTES)
        .ok_or_else(invalid_frame)?;
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err(invalid_frame());
    }
    Ok((stride, length))
}

fn invalid_frame() -> CaptureError {
    CaptureError::new(
        "invalid_frame",
        "X11 BGRA frame dimensions are outside limits",
    )
}

pub fn validate_operation_id(id: &str) -> CaptureResult<()> {
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        Err(CaptureError::new(
            "invalid_operation_id",
            "invalid capture operation ID",
        ))
    } else {
        Ok(())
    }
}
