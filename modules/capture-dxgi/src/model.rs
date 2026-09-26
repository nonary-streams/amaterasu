#[cfg(any(windows, test))]
use amaterasu_media::CaptureGeometry;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_STREAMS: usize = 4;
pub const MAX_WAIT_MS: u32 = 16;
pub const MAX_ENUMERATE_LIMIT: usize = 256;

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
    pub adapter_vendor_id: u32,
    pub adapter_device_id: u32,
    pub adapter_description: String,
    pub device_name: String,
    pub attached: bool,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub rotation: u32,
}

#[cfg(any(windows, test))]
pub fn verify_output_mode(
    opened: &Output,
    current: &Output,
    geometry: CaptureGeometry,
) -> CaptureResult<()> {
    let rectangle = geometry.output;
    if !opened.attached
        || !current.attached
        || opened.id != current.id
        || opened.rotation != current.rotation
        || opened.left != current.left
        || opened.top != current.top
        || opened.width != current.width
        || opened.height != current.height
        || rectangle.left != current.left
        || rectangle.top != current.top
        || rectangle.width != current.width
        || rectangle.height != current.height
    {
        return Err(CaptureError::new(
            "access_lost",
            "captured output mode changed; enumerate and reopen",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Status,
    Enumerate {
        offset: Option<usize>,
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

/// Convert a desktop-duplication surface into the selected output's orientation.
/// DXGI returns the surface in the unrotated mode dimensions; `output_width`
/// and `output_height` come from the output's desktop rectangle.
///
/// # Safety
/// Each source row must have `surface_width * 4` initialized readable bytes.
/// Row padding need not be initialized and is never borrowed.
#[cfg(any(windows, test))]
pub unsafe fn orient_bgra_rows(
    surface_width: u32,
    surface_height: u32,
    row_pitch: u32,
    source: *const u8,
    output_width: u32,
    output_height: u32,
    rotation: u32,
) -> CaptureResult<Vec<u8>> {
    let (source_stride, _) = frame_layout(surface_width, surface_height)?;
    let (output_stride, output_length) = frame_layout(output_width, output_height)?;
    mapped_length(surface_height, row_pitch, source_stride)?;
    let expected = match rotation {
        1 | 3 => (output_width, output_height),
        2 | 4 => (output_height, output_width),
        _ => {
            return Err(CaptureError::new(
                "unsupported_rotation",
                "DXGI reported an unknown output rotation",
            ));
        }
    };
    if (surface_width, surface_height) != expected {
        return Err(CaptureError::new(
            "access_lost",
            "duplication surface no longer matches the selected output mode",
        ));
    }
    if source.is_null() {
        return Err(CaptureError::new(
            "invalid_frame",
            "mapped texture pointer is null",
        ));
    }
    if rotation == 1 {
        // SAFETY: the same validated source contract applies to the row copier.
        return unsafe { copy_bgra_rows(surface_width, surface_height, row_pitch, source) };
    }

    let mut bytes = vec![0; output_length];
    for y in 0..output_height as usize {
        for x in 0..output_width as usize {
            // The DXGI rotation describes how desktop pixels appear in the
            // unrotated duplication surface. Read the inverse mapping.
            let (source_x, source_y) = match rotation {
                2 => (y, surface_height as usize - 1 - x),
                3 => (
                    surface_width as usize - 1 - x,
                    surface_height as usize - 1 - y,
                ),
                4 => (surface_width as usize - 1 - y, x),
                _ => unreachable!(),
            };
            let source_offset = source_y * row_pitch as usize + source_x * 4;
            let output_offset = y * output_stride as usize + x * 4;
            // SAFETY: validated dimensions and row pitch bound this complete
            // BGRA pixel inside an initialized mapped texture row.
            let pixel = unsafe { std::slice::from_raw_parts(source.add(source_offset), 4) };
            bytes[output_offset..output_offset + 4].copy_from_slice(pixel);
        }
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
