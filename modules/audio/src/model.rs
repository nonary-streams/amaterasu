use amaterasu_media::{PCM_F32, PCM_I16};
use serde::{Deserialize, Deserializer, Serialize};

pub type AudioResult<T> = Result<T, &'static str>;

#[derive(Clone, Debug, Serialize)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter_name: Option<String>,
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
                6 => 0x003f,
                8 => 0x063f,
                _ => 0,
            },
            name,
        })
    }

    pub fn code(self) -> u32 {
        if self.name == "f32" { PCM_F32 } else { PCM_I16 }
    }

    pub fn block_align(self) -> usize {
        self.channels as usize * if self.code() == PCM_F32 { 4 } else { 2 }
    }

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
    pub recovering: bool,
    #[serde(skip)]
    pub active_endpoint: Option<String>,
    pub discontinuities: u64,
    pub dropped_packets: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuousAudio {
    pub packet_ms: u32,
}

impl ContinuousAudio {
    pub fn validate(self) -> AudioResult<()> {
        if matches!(self.packet_ms, 5 | 10) {
            Ok(())
        } else {
            Err("invalid_continuous_audio_packet_size")
        }
    }
}

/// Optional PCM output requested from ordinary endpoint loopback. When absent,
/// capture retains the endpoint's supported channel layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PcmCaptureFormat {
    pub sample_rate: u32,
    pub channels: u32,
    pub sample_format: PcmSampleFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PcmSampleFormat {
    F32,
    I16,
}

impl PcmCaptureFormat {
    pub fn validate(self) -> AudioResult<()> {
        if self.sample_rate != 48_000
            || !matches!(self.channels, 2 | 6 | 8)
            || self.sample_format != PcmSampleFormat::F32
        {
            return Err("unsupported_capture_format");
        }
        Ok(())
    }
}

pub enum Operation {
    Enumerate,
    Open {
        endpoint: String,
        virtual_sink: Option<String>,
        channels: Option<u32>,
        capture_format: Option<PcmCaptureFormat>,
        host_audio_enabled: bool,
        operation_id: Option<String>,
        capture_sink_without_routing: bool,
    },
    OpenContinuous {
        endpoint: String,
        virtual_sink: Option<String>,
        channels: Option<u32>,
        capture_format: Option<PcmCaptureFormat>,
        host_audio_enabled: bool,
        operation_id: Option<String>,
        capture_sink_without_routing: bool,
        continuous_audio: ContinuousAudio,
    },
    Lookup {
        operation_id: String,
    },
    Close {
        stream: u64,
    },
    Status {
        stream: Option<u64>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum OperationInput {
    Enumerate,
    Open {
        endpoint: String,
        #[serde(default)]
        virtual_sink: Option<String>,
        #[serde(default)]
        channels: Option<u32>,
        #[serde(default)]
        capture_format: Option<PcmCaptureFormat>,
        #[serde(default)]
        host_audio_enabled: bool,
        #[serde(default)]
        operation_id: Option<String>,
        #[serde(default)]
        capture_sink_without_routing: bool,
        #[serde(default)]
        continuous_audio: Option<ContinuousAudio>,
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

impl<'de> Deserialize<'de> for Operation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match OperationInput::deserialize(deserializer)? {
            OperationInput::Enumerate => Self::Enumerate,
            OperationInput::Open {
                endpoint,
                virtual_sink,
                channels,
                capture_format,
                host_audio_enabled,
                operation_id,
                capture_sink_without_routing,
                continuous_audio,
            } => match continuous_audio {
                Some(continuous_audio) => Self::OpenContinuous {
                    endpoint,
                    virtual_sink,
                    channels,
                    capture_format,
                    host_audio_enabled,
                    operation_id,
                    capture_sink_without_routing,
                    continuous_audio,
                },
                None => Self::Open {
                    endpoint,
                    virtual_sink,
                    channels,
                    capture_format,
                    host_audio_enabled,
                    operation_id,
                    capture_sink_without_routing,
                },
            },
            OperationInput::Lookup { operation_id } => Self::Lookup { operation_id },
            OperationInput::Close { stream } => Self::Close { stream },
            OperationInput::Status { stream } => Self::Status { stream },
        })
    }
}

pub fn validate_endpoint(endpoint: &str) -> AudioResult<()> {
    if endpoint.len() > 4096 || endpoint.chars().any(char::is_control) {
        return Err("invalid_endpoint");
    }
    Ok(())
}

pub fn validate_virtual_sink(virtual_sink: &str) -> AudioResult<()> {
    if virtual_sink.len() > 4096 || virtual_sink.chars().any(char::is_control) {
        return Err("invalid_virtual_sink");
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
