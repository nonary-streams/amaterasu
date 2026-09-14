use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_STREAMS: usize = 4;
pub const MAX_WAIT_MS: u32 = 16;

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
    pub device_name: String,
    pub attached: bool,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub rotation: u32,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Status,
    Enumerate,
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

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub timestamp_us: u64,
    pub sequence: u64,
    pub bytes: Vec<u8>,
}

#[cfg(any(windows, test))]
pub fn frame_layout(width: u32, height: u32) -> CaptureResult<(u32, usize)> {
    let stride = width.checked_mul(4).ok_or_else(invalid_frame)?;
    let length = (stride as usize)
        .checked_mul(height as usize)
        .ok_or_else(invalid_frame)?;
    if width == 0
        || height == 0
        || length > amaterasu_media::MAX_FRAME_BYTES
        || width > 8192
        || height > 8192
    {
        return Err(invalid_frame());
    }
    Ok((stride, length))
}

#[cfg(any(windows, test))]
fn invalid_frame() -> CaptureError {
    CaptureError::new(
        "invalid_frame",
        "BGRA frame dimensions exceed the 128 MiB limit",
    )
}

/// # Safety
/// Each of `height` rows at `source + row * row_pitch` must contain at
/// least `width * 4` initialized readable bytes through this call. Padding
/// need not be initialized; it is never included in a Rust slice.
#[cfg(any(windows, test))]
pub unsafe fn copy_bgra_rows(
    width: u32,
    height: u32,
    row_pitch: u32,
    source: *const u8,
) -> CaptureResult<Vec<u8>> {
    let (stride, length) = frame_layout(width, height)?;
    mapped_length(height, row_pitch, stride)?;
    if source.is_null() {
        return Err(CaptureError::new(
            "invalid_frame",
            "mapped texture pointer is null",
        ));
    }
    let mut bytes = vec![0; length];
    for row in 0..height as usize {
        // SAFETY: caller guarantees initialized pixel bytes at each row;
        // validated arithmetic bounds offsets, and no padding is borrowed.
        let pixels = unsafe {
            std::slice::from_raw_parts(source.add(row * row_pitch as usize), stride as usize)
        };
        bytes[row * stride as usize..(row + 1) * stride as usize].copy_from_slice(pixels);
    }
    Ok(bytes)
}

#[cfg(any(windows, test))]
pub fn mapped_length(height: u32, row_pitch: u32, stride: u32) -> CaptureResult<usize> {
    if height == 0 || row_pitch < stride {
        return Err(CaptureError::new(
            "invalid_frame",
            "D3D11 mapped row pitch is invalid",
        ));
    }
    (row_pitch as usize)
        .checked_mul(height as usize - 1)
        .and_then(|length| length.checked_add(stride as usize))
        .filter(|length| *length <= 2 * amaterasu_media::MAX_FRAME_BYTES)
        .ok_or_else(invalid_frame)
}
