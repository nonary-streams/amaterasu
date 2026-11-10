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
    #[cfg(any(windows, target_os = "linux", test))]
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
    #[serde(skip)]
    pub capture_read_us: amaterasu_module_sdk::diagnostics::LatencyMetric,
    #[serde(skip)]
    pub queue_wait_us: amaterasu_module_sdk::diagnostics::LatencyMetric,
    #[serde(skip)]
    pub queue_high_water: usize,
    #[serde(skip)]
    pub queue_timing_unknown: u64,
    #[serde(skip)]
    pub queue_clock_lost: bool,
    #[serde(skip)]
    pub queued_at: std::collections::VecDeque<std::time::Instant>,
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

    pub(crate) fn from_negotiated(
        format: &amaterasu_module_sdk::audio::PcmFormat,
    ) -> AudioResult<Self> {
        use amaterasu_module_sdk::audio::*;
        format.validate()?;
        let expected: &[amaterasu_media::AudioFormatId] = match format.speakers.len() {
            2 => &[SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT],
            6 => &[
                SPEAKER_FRONT_LEFT,
                SPEAKER_FRONT_RIGHT,
                SPEAKER_FRONT_CENTER,
                SPEAKER_LOW_FREQUENCY,
                SPEAKER_BACK_LEFT,
                SPEAKER_BACK_RIGHT,
            ],
            8 => &[
                SPEAKER_FRONT_LEFT,
                SPEAKER_FRONT_RIGHT,
                SPEAKER_FRONT_CENTER,
                SPEAKER_LOW_FREQUENCY,
                SPEAKER_BACK_LEFT,
                SPEAKER_BACK_RIGHT,
                SPEAKER_SIDE_LEFT,
                SPEAKER_SIDE_RIGHT,
            ],
            _ => return Err("unsupported_capture_format"),
        };
        if format.sample_format != amaterasu_media::AUDIO_PCM_F32
            || format.sample_bytes != 4
            || format.valid_bits != 32
            || format.packing != amaterasu_media::AUDIO_PACKING_INTERLEAVED
            || format.speakers != expected
        {
            return Err("unsupported_capture_format");
        }
        let result = Self {
            sample_rate: format.sample_rate,
            channels: format.speakers.len() as u32,
            sample_format: PcmSampleFormat::F32,
        };
        result.validate()?;
        Ok(result)
    }
}

pub enum Operation {
    Enumerate,
    Capabilities,
    Probe {
        audio_profile: Box<amaterasu_module_sdk::audio::AudioSessionProfile>,
    },
    Effective {
        stream: u64,
    },
    Reconfigure {
        stream: u64,
        audio_profile: Box<amaterasu_module_sdk::audio::AudioSessionProfile>,
    },
    ConfigureSteamDriverInstall {
        enabled: bool,
        revision: u64,
    },
    Open {
        endpoint: String,
        audio_profile: Option<serde_json::Value>,
        virtual_sink: Option<String>,
        channels: Option<u32>,
        capture_format: Option<PcmCaptureFormat>,
        host_audio_enabled: bool,
        operation_id: Option<String>,
        capture_sink_without_routing: bool,
    },
    OpenContinuous {
        endpoint: String,
        audio_profile: Option<serde_json::Value>,
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
    Capabilities,
    Probe {
        audio_profile: Box<amaterasu_module_sdk::audio::AudioSessionProfile>,
    },
    Effective {
        stream: u64,
    },
    Reconfigure {
        stream: u64,
        audio_profile: Box<amaterasu_module_sdk::audio::AudioSessionProfile>,
    },
    ConfigureSteamDriverInstall {
        enabled: bool,
        revision: u64,
    },
    Open {
        endpoint: String,
        #[serde(default)]
        audio_profile: Option<serde_json::Value>,
        #[serde(default)]
        virtual_sink: Option<String>,
        #[serde(default)]
        channels: Option<u32>,
        #[serde(default)]
        capture_format: Option<PcmCaptureFormat>,
        #[serde(default)]
        pcm_format: Option<amaterasu_module_sdk::audio::PcmFormat>,
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
            OperationInput::Capabilities => Self::Capabilities,
            OperationInput::Probe { audio_profile } => Self::Probe { audio_profile },
            OperationInput::Effective { stream } => Self::Effective { stream },
            OperationInput::Reconfigure {
                stream,
                audio_profile,
            } => Self::Reconfigure {
                stream,
                audio_profile,
            },
            OperationInput::ConfigureSteamDriverInstall { enabled, revision } => {
                Self::ConfigureSteamDriverInstall { enabled, revision }
            }
            OperationInput::Open {
                endpoint,
                audio_profile,
                virtual_sink,
                channels,
                capture_format,
                pcm_format,
                host_audio_enabled,
                operation_id,
                capture_sink_without_routing,
                continuous_audio,
            } => {
                if pcm_format.is_none()
                    && audio_profile
                        .as_ref()
                        .and_then(|profile| profile.pointer("/custom/pcm"))
                        .is_some()
                {
                    return Err(serde::de::Error::custom(
                        "PCM format must accompany the audio profile",
                    ));
                }
                if let Some(format) = &pcm_format {
                    let supplied =
                        serde_json::to_value(format).map_err(serde::de::Error::custom)?;
                    if audio_profile
                        .as_ref()
                        .and_then(|profile| profile.pointer("/custom/pcm"))
                        != Some(&supplied)
                    {
                        return Err(serde::de::Error::custom("PCM profile mismatch"));
                    }
                }
                let capture_format = match (capture_format, pcm_format) {
                    (Some(_), Some(_)) => {
                        return Err(serde::de::Error::custom("conflicting PCM formats"));
                    }
                    (Some(format), None) => Some(format),
                    (None, Some(format)) => Some(
                        PcmCaptureFormat::from_negotiated(&format)
                            .map_err(serde::de::Error::custom)?,
                    ),
                    (None, None) => None,
                };
                match continuous_audio {
                    Some(continuous_audio) => Self::OpenContinuous {
                        endpoint,
                        audio_profile,
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
                        audio_profile,
                        virtual_sink,
                        channels,
                        capture_format,
                        host_audio_enabled,
                        operation_id,
                        capture_sink_without_routing,
                    },
                }
            }
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

#[cfg(test)]
mod tests {
    use super::Operation;

    #[test]
    fn steam_driver_configuration_request_is_strict_and_revisioned() {
        let operation: Operation = serde_json::from_value(serde_json::json!({
            "action":"configure_steam_driver_install",
            "enabled":true,
            "revision":9,
        }))
        .unwrap();
        assert!(matches!(
            operation,
            Operation::ConfigureSteamDriverInstall {
                enabled: true,
                revision: 9
            }
        ));
        assert!(
            serde_json::from_value::<Operation>(serde_json::json!({
                "action":"configure_steam_driver_install",
                "enabled":true,
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<Operation>(serde_json::json!({
                "action":"configure_steam_driver_install",
                "enabled":true,
                "revision":9,
                "path":"C:/untrusted.inf",
            }))
            .is_err()
        );
    }
}
