use crate::launch::StreamingConfig;
use serde::Deserialize;
use std::net::SocketAddr;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub http_bind: SocketAddr,
    pub https_bind: SocketAddr,
    pub allow_remote: bool,
    pub hostname: String,
    pub streaming: Option<StreamingConfig>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            http_bind: SocketAddr::from(([127, 0, 0, 1], 47989)),
            https_bind: SocketAddr::from(([127, 0, 0, 1], 47984)),
            allow_remote: false,
            hostname: "Amaterasu".into(),
            streaming: None,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.allow_remote
            && (!self.http_bind.ip().is_loopback() || !self.https_bind.ip().is_loopback())
        {
            return Err("remote_binding_requires_allow_remote");
        }
        if self.hostname.is_empty()
            || self.hostname.len() > 128
            || self.hostname.chars().any(char::is_control)
        {
            return Err("invalid_hostname");
        }
        if self.http_bind == self.https_bind && self.http_bind.port() != 0 {
            return Err("listener_addresses_conflict");
        }
        if let Some(streaming) = &self.streaming {
            streaming.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_bind_nothing_and_remote_opt_in_is_explicit() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.enabled);
        settings.validate().unwrap();
        let mut settings: Settings =
            serde_json::from_str(r#"{"http_bind":"0.0.0.0:47989"}"#).unwrap();
        assert!(settings.validate().is_err());
        settings.allow_remote = true;
        settings.validate().unwrap();
    }

    #[test]
    fn streaming_requires_the_complete_trusted_capture_and_profile_contract() {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "streaming": {
                "capture": {
                    "capture_provider": "dxgi",
                    "output": "display-one",
                    "encoder": 0,
                    "audio_endpoint": "default"
                },
                "profile": {
                    "max_width": 1920,
                    "max_height": 1080,
                    "max_fps": 60,
                    "bitrate_kbps": 10000,
                    "max_reference_frames": 1,
                    "packet_size": 1024,
                    "fec_percentage": 20,
                    "min_parity_shards": 2,
                    "max_fec_blocks": 1,
                    "packet_ms": 10,
                    "audio_bitrate": 128000,
                    "encrypt_video": true,
                    "encrypt_audio": true,
                    "permit_legacy_audio_iv_wrap": false
                }
            }
        }))
        .unwrap();
        settings.validate().unwrap();
        assert!(settings.streaming.is_some());

        let unsupported = serde_json::json!({
            "streaming": {
                "enabled": true,
                "capture": {},
                "profile": {}
            }
        });
        assert!(serde_json::from_value::<Settings>(unsupported).is_err());
    }
}
