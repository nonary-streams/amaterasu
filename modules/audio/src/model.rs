use amaterasu_media::{PCM_F32, PCM_I16};
use serde::{Deserialize, Serialize};

pub type AudioResult<T> = Result<T, &'static str>;

#[derive(Clone, Debug, Serialize)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub default_multimedia: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Format {
    pub sample_rate: u32,
    pub channels: u32,
    pub channel_mask: u32,
    #[serde(rename = "format")]
    pub name: &'static str,
}

impl Format {
    #[cfg(any(windows, test))]
    pub fn new(sample_rate: u32, channels: u32, format: u32) -> AudioResult<Self> {
        if !(8_000..=192_000).contains(&sample_rate) || !(1..=8).contains(&channels) {
            return Err("unsupported_format");
        }
        let name = match format {
            PCM_F32 => "f32",
            PCM_I16 => "i16",
            _ => return Err("unsupported_format"),
        };
        Ok(Self {
            sample_rate,
            channels,
            channel_mask: match channels {
                1 => 4,
                2 => 3,
                _ => 0,
            },
            name,
        })
    }

    pub fn code(self) -> u32 {
        if self.name == "f32" { PCM_F32 } else { PCM_I16 }
    }

    #[cfg(any(windows, test))]
    pub fn block_align(self) -> usize {
        self.channels as usize * if self.code() == PCM_F32 { 4 } else { 2 }
    }

    #[cfg(any(windows, test))]
    pub fn packet_bytes(self, frames: u32) -> AudioResult<usize> {
        if frames == 0 || frames > self.sample_rate / 10 {
            return Err("unsupported_packet_size");
        }
        let bytes = (frames as usize)
            .checked_mul(self.block_align())
            .ok_or("unsupported_packet_size")?;
        if bytes > 1024 * 1024 {
            return Err("unsupported_packet_size");
        }
        Ok(bytes)
    }
}

pub struct Packet {
    pub frames: u32,
    pub flags: u32,
    pub bytes: Vec<u8>,
    pub timestamp_us: u64,
    pub sequence: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Health {
    pub failure: Option<&'static str>,
    pub discontinuities: u64,
    pub dropped_packets: u64,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Enumerate,
    Open {
        endpoint: String,
        #[serde(default)]
        operation_id: Option<String>,
        #[serde(default)]
        capture_sink_without_routing: bool,
    },
    Lookup {
        operation_id: String,
    },
    Close {
        stream: u64,
    },
    Status {
        #[serde(default)]
        stream: Option<u64>,
    },
}

pub fn validate_endpoint(endpoint: &str) -> AudioResult<()> {
    if endpoint.is_empty() || endpoint.len() > 4096 || endpoint.chars().any(char::is_control) {
        return Err("invalid_endpoint");
    }
    Ok(())
}

pub fn validate_operation(operation: &str) -> AudioResult<()> {
    if operation.is_empty()
        || operation.len() > 128
        || !operation.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err("invalid_operation_id");
    }
    Ok(())
}
