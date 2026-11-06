use amaterasu_media::{CaptureGeometry, DesktopRect, PIXEL_NV12, PIXEL_P010, frame_size};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SERVICE: &str = "amaterasu.capture-black/1";
pub const MAX_STREAMS: usize = 32;
pub const MAX_LEDGER: usize = 4_096;
pub const MAX_BUFFER_BYTES: usize = 256 * 1024 * 1024;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
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
        resource_capture_clock: Option<ResourceCaptureClock>,
        resource_callback_contract: Option<String>,
    },
    Lookup {
        operation_id: String,
    },
    Close {
        stream: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCaptureClock {
    pub domain: String,
    pub epoch_id: String,
}

impl ResourceCaptureClock {
    pub fn epoch(&self) -> Result<[u8; 16], &'static str> {
        if self.domain != "host_monotonic"
            || self.epoch_id.len() != 32
            || !self
                .epoch_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid_resource_capture_clock");
        }
        let mut epoch = [0; 16];
        for (index, destination) in epoch.iter_mut().enumerate() {
            *destination = u8::from_str_radix(&self.epoch_id[index * 2..index * 2 + 2], 16)
                .map_err(|_| "invalid_resource_capture_clock")?;
        }
        if epoch == [0; 16] {
            return Err("invalid_resource_capture_clock");
        }
        Ok(epoch)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceMode {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub format: u32,
}

impl SourceMode {
    /// Output names are an explicit request for generated content. They never
    /// select an operating-system output or fall back to desktop capture.
    pub fn parse(output: &str) -> Result<Self, &'static str> {
        let output = output.strip_prefix("black:").ok_or("invalid_output")?;
        let (mode, format) = output.rsplit_once(':').ok_or("invalid_output")?;
        let format = match format {
            "nv12" => PIXEL_NV12,
            "p010" => PIXEL_P010,
            _ => return Err("invalid_output"),
        };
        let (dimensions, fps) = mode.split_once('@').ok_or("invalid_output")?;
        let (width, height) = dimensions.split_once('x').ok_or("invalid_output")?;
        let mode = Self {
            width: canonical_integer(width)?,
            height: canonical_integer(height)?,
            fps: canonical_integer(fps)?,
            format,
        };
        if !(1..=240).contains(&mode.fps) {
            return Err("invalid_output");
        }
        mode.buffer_length()?;
        Ok(mode)
    }

    pub fn output(self) -> String {
        let format = if self.format == PIXEL_P010 {
            "p010"
        } else {
            "nv12"
        };
        format!("black:{}x{}@{}:{format}", self.width, self.height, self.fps)
    }

    pub fn buffer_length(self) -> Result<usize, &'static str> {
        frame_size(self.width, self.height, self.format, self.stride())
            .map_err(|_| "invalid_output")
    }

    pub fn stride(self) -> u32 {
        if self.format == PIXEL_P010 {
            self.width.saturating_mul(2)
        } else {
            self.width
        }
    }

    pub fn pixels(self) -> Result<Box<[u8]>, &'static str> {
        let length = self.buffer_length()?;
        let luma_length = self.width as usize * self.height as usize;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| "resource_exhausted")?;
        if self.format == PIXEL_P010 {
            for (samples, code) in [(luma_length, 64u16), (luma_length / 2, 512u16)] {
                for _ in 0..samples {
                    bytes.extend_from_slice(&(code << 6).to_le_bytes());
                }
            }
        } else {
            bytes.resize(length, 128);
            bytes[..luma_length].fill(16);
        }
        Ok(bytes.into_boxed_slice())
    }

    pub fn geometry(self) -> CaptureGeometry {
        let rectangle = DesktopRect {
            left: 0,
            top: 0,
            width: self.width,
            height: self.height,
        };
        CaptureGeometry {
            output: rectangle,
            virtual_desktop: rectangle,
        }
    }
}

fn canonical_integer(value: &str) -> Result<u32, &'static str> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("invalid_output");
    }
    value.parse().map_err(|_| "invalid_output")
}

pub fn validate_operation_id(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        Err("invalid_operation_id")
    } else {
        Ok(())
    }
}

pub fn failure(code: &str) -> Value {
    json!({"ok":false,"error":{"code":code,"message":code}})
}
