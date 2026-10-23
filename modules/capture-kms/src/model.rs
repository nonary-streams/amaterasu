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
    pub framebuffer_export_supported: bool,
    pub readback_reason: Option<&'static str>,
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
    pub framebuffer_export_supported: bool,
    pub presentation_events: bool,
    pub managed_virtual_output: bool,
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
        "DRM BGRA frame dimensions are outside limits",
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

pub fn validate_output_id(id: &str) -> CaptureResult<()> {
    let rest = id.strip_prefix("kms:").ok_or_else(invalid_output_id)?;
    let (card, identity) = rest.split_once('/').ok_or_else(invalid_output_id)?;
    let card_number = card.strip_prefix("card").ok_or_else(invalid_output_id)?;
    if card_number.is_empty()
        || !card_number.bytes().all(|byte| byte.is_ascii_digit())
        || (card_number.len() > 1 && card_number.starts_with('0'))
        || card.len() > 63
        || identity.chars().any(char::is_control)
    {
        return Err(invalid_output_id());
    }
    let mut connector = None;
    let mut crtc = None;
    let mut plane = None;
    for field in identity.split('/') {
        if let Some(value) = field.strip_prefix("conn=") {
            connector = parse_nonzero_id(value);
        } else if let Some(value) = field.strip_prefix("crtc=") {
            crtc = parse_nonzero_id(value);
        } else if let Some(value) = field.strip_prefix("plane=") {
            plane = parse_nonzero_id(value);
        } else {
            return Err(invalid_output_id());
        }
    }
    if connector.is_some()
        && crtc.is_some()
        && plane.is_some()
        && identity.matches('/').count() == 2
    {
        Ok(())
    } else {
        Err(invalid_output_id())
    }
}

fn parse_nonzero_id(value: &str) -> Option<u32> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse::<u32>().ok().filter(|value| *value != 0)
}

fn invalid_output_id() -> CaptureError {
    CaptureError::new("invalid_output", "selected DRM output ID is invalid")
}
