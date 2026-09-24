use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub type TransportResult<T> = Result<T, &'static str>;
pub const MAX_DATAGRAMS: usize = 2048;
pub const MAX_QUEUE_BYTES: usize = 8 * 1024 * 1024;
pub const SENDS_PER_STREAM: usize = 16;
pub const SENDS_PER_TICK: usize = 64;
pub const DISCOVERY_TOKEN_BYTES: usize = 16;

fn yes() -> bool {
    true
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub allow_remote: bool,
    pub allowed_principals: Vec<String>,
}

impl Settings {
    pub fn validate(&self) -> TransportResult<()> {
        if self.allowed_principals.len() > 16
            || self.allowed_principals.iter().any(|principal| {
                principal.is_empty()
                    || principal.len() > 128
                    || principal.chars().any(char::is_control)
            })
        {
            return Err("invalid_settings");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoSettings {
    pub packet_size: usize,
    pub fec_percentage: u8,
    pub min_parity_shards: u8,
    pub max_fec_blocks: u8,
    #[serde(default = "default_video_max_batch_size_kb")]
    pub max_batch_size_kb: u8,
}

fn default_video_max_batch_size_kb() -> u8 {
    64
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            packet_size: 1024,
            fec_percentage: 20,
            min_parity_shards: 2,
            max_fec_blocks: 1,
            max_batch_size_kb: default_video_max_batch_size_kb(),
        }
    }
}

impl VideoSettings {
    pub fn valid_batch_size_kb(self) -> bool {
        self.max_batch_size_kb <= 64
    }

    pub fn effective(self) -> Self {
        Self {
            max_batch_size_kb: self.effective_batch_size_kb(),
            ..self
        }
    }

    pub fn effective_batch_size_kb(self) -> u8 {
        match self.max_batch_size_kb {
            16 | 32 | 64 => self.max_batch_size_kb,
            _ => 64,
        }
    }

    pub fn batch_size_fallback(self) -> bool {
        self.max_batch_size_kb != 0 && !matches!(self.max_batch_size_kb, 16 | 32 | 64)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AudioSettings {
    pub packet_ms: u32,
    pub channels: u16,
    pub wire_payload_bytes: usize,
    #[serde(default)]
    pub ssrc: u32,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Encryption {
    pub key_hex: String,
    pub key_id: u32,
    pub video: bool,
    pub audio: bool,
    #[serde(default)]
    pub permit_legacy_audio_iv_wrap: bool,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery {
    pub video_ping_payload: String,
    pub audio_ping_payload: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryTokens {
    pub video: [u8; DISCOVERY_TOKEN_BYTES],
    pub audio: [u8; DISCOVERY_TOKEN_BYTES],
}

impl Discovery {
    pub fn tokens(&self) -> TransportResult<DiscoveryTokens> {
        let video = ping_payload(&self.video_ping_payload)?;
        let audio = ping_payload(&self.audio_ping_payload)?;
        if video == audio {
            return Err("invalid_discovery");
        }
        Ok(DiscoveryTokens { video, audio })
    }
}

fn ping_payload(value: &str) -> TransportResult<[u8; DISCOVERY_TOKEN_BYTES]> {
    let bytes = value.as_bytes();
    if bytes.len() != DISCOVERY_TOKEN_BYTES
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("invalid_discovery");
    }
    bytes.try_into().map_err(|_| "invalid_discovery")
}

impl Encryption {
    pub fn key(&self) -> TransportResult<[u8; 16]> {
        if self.key_hex.len() != 32 || (!self.video && !self.audio) {
            return Err("invalid_encryption");
        }
        let mut key = [0u8; 16];
        for (index, pair) in self
            .key_hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .enumerate()
        {
            let high = (pair[0] as char).to_digit(16).ok_or("invalid_encryption")?;
            let low = (pair[1] as char).to_digit(16).ok_or("invalid_encryption")?;
            key[index] = ((high << 4) | low) as u8;
        }
        Ok(key)
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Open {
    pub peer_ip: IpAddr,
    #[serde(default)]
    pub video_port: Option<u16>,
    #[serde(default)]
    pub audio_port: Option<u16>,
    #[serde(default)]
    pub discovery: Option<Box<Discovery>>,
    #[serde(default)]
    pub video_bind: Option<SocketAddr>,
    #[serde(default)]
    pub audio_bind: Option<SocketAddr>,
    #[serde(default)]
    pub video: VideoSettings,
    pub audio: AudioSettings,
    #[serde(default = "yes")]
    pub stream_audio: bool,
    #[serde(default)]
    pub encryption: Option<Encryption>,
    #[serde(default)]
    pub operation_id: Option<String>,
}

impl Open {
    pub fn validate(&self, allow_remote: bool) -> TransportResult<()> {
        if self.peer_ip.is_unspecified()
            || self.peer_ip.is_multicast()
            || matches!(self.peer_ip, IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some())
            || self.peer_ip == IpAddr::V4(Ipv4Addr::BROADCAST)
        {
            return Err("invalid_destination");
        }
        match (self.video_port, self.audio_port, &self.discovery) {
            (Some(video), Some(audio), None) if video != 0 && audio != 0 => {}
            (None, None, Some(discovery)) => {
                discovery.tokens()?;
            }
            _ => return Err("invalid_destination"),
        }
        if !allow_remote && !self.peer_ip.is_loopback() {
            return Err("remote_disabled");
        }
        if !self.video.valid_batch_size_kb() {
            return Err("invalid_video_configuration");
        }
        for bind in [self.video_bind, self.audio_bind].into_iter().flatten() {
            if bind.is_ipv4() != self.peer_ip.is_ipv4()
                || bind.ip().is_multicast()
                || (!allow_remote && !bind.ip().is_loopback())
            {
                return Err("invalid_bind");
            }
        }
        if !matches!(self.audio.packet_ms, 5 | 10)
            || self.audio.channels != 2
            || !self.valid_audio_wire_length()
        {
            return Err("invalid_audio_configuration");
        }
        if !self.stream_audio
            && self
                .encryption
                .as_ref()
                .is_some_and(|encryption| encryption.audio)
        {
            return Err("invalid_encryption");
        }
        if let Some(operation) = &self.operation_id {
            validate_operation(operation)?;
        }
        if let Some(encryption) = &self.encryption {
            encryption.key()?;
        }
        Ok(())
    }

    fn valid_audio_wire_length(&self) -> bool {
        let length = self.audio.wire_payload_bytes;
        if self
            .encryption
            .as_ref()
            .is_some_and(|encryption| encryption.audio)
        {
            (16..=1280).contains(&length) && length.is_multiple_of(16)
        } else {
            (1..=1275).contains(&length)
        }
    }

    pub fn local(&self, video: bool) -> SocketAddr {
        let configured = if video {
            self.video_bind
        } else {
            self.audio_bind
        };
        configured.unwrap_or_else(|| {
            SocketAddr::new(
                if self.peer_ip.is_ipv4() {
                    IpAddr::V4(if self.peer_ip.is_loopback() {
                        Ipv4Addr::LOCALHOST
                    } else {
                        Ipv4Addr::UNSPECIFIED
                    })
                } else {
                    IpAddr::V6(if self.peer_ip.is_loopback() {
                        Ipv6Addr::LOCALHOST
                    } else {
                        Ipv6Addr::UNSPECIFIED
                    })
                },
                0,
            )
        })
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Open(Open),
    Lookup { operation_id: String },
    Close { stream: u64 },
    Status { stream: Option<u64> },
}

pub fn validate_operation(operation: &str) -> TransportResult<()> {
    if operation.is_empty() || operation.len() > 128 || operation.chars().any(char::is_control) {
        return Err("invalid_operation_id");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_stream_audio_field_preserves_the_enabled_legacy_default() {
        let request: Open = serde_json::from_value(json!({
            "peer_ip":"127.0.0.1",
            "video_port":50001,
            "audio_port":50002,
            "audio":{"packet_ms":10,"channels":2,"wire_payload_bytes":16}
        }))
        .unwrap();
        assert!(request.stream_audio);
        assert!(request.validate(false).is_ok());
        assert_eq!(request.video.max_batch_size_kb, 64);
    }

    #[test]
    fn video_batch_size_accepts_reference_range_and_reports_normalized_values() {
        for value in 0..=64 {
            let settings = VideoSettings {
                max_batch_size_kb: value,
                ..VideoSettings::default()
            };
            assert!(settings.valid_batch_size_kb());
            assert_eq!(
                settings.effective_batch_size_kb(),
                match value {
                    16 | 32 | 64 => value,
                    _ => 64,
                }
            );
            assert_eq!(
                settings.batch_size_fallback(),
                value != 0 && !matches!(value, 16 | 32 | 64)
            );
        }
        assert!(
            !VideoSettings {
                max_batch_size_kb: 65,
                ..VideoSettings::default()
            }
            .valid_batch_size_kb()
        );
    }

    #[test]
    fn video_batch_size_open_field_is_optional_bounded_and_normalized_at_runtime() {
        let base = json!({
            "peer_ip":"127.0.0.1",
            "video_port":50001,
            "audio_port":50002,
            "audio":{"packet_ms":10,"channels":2,"wire_payload_bytes":16}
        });
        let mut omitted = base.clone();
        let request: Open = serde_json::from_value(omitted.clone()).unwrap();
        assert_eq!(request.video.effective_batch_size_kb(), 64);

        omitted["video"] = json!({"max_batch_size_kb":17});
        let request: Open = serde_json::from_value(omitted).unwrap();
        assert!(request.validate(false).is_ok());
        assert_eq!(request.video.effective_batch_size_kb(), 64);
        assert!(request.video.batch_size_fallback());

        let mut out_of_range = base;
        out_of_range["video"] = json!({"max_batch_size_kb":65});
        let request: Open = serde_json::from_value(out_of_range).unwrap();
        assert_eq!(request.validate(false), Err("invalid_video_configuration"));
    }

    #[test]
    fn disabled_audio_rejects_audio_encryption_configuration() {
        let request: Open = serde_json::from_value(json!({
            "peer_ip":"127.0.0.1",
            "video_port":50001,
            "audio_port":50002,
            "audio":{"packet_ms":10,"channels":2,"wire_payload_bytes":16},
            "stream_audio":false,
            "encryption":{"key_hex":"000102030405060708090a0b0c0d0e0f","key_id":1,
                "video":true,"audio":true}
        }))
        .unwrap();
        assert_eq!(request.validate(false), Err("invalid_encryption"));
    }
}
