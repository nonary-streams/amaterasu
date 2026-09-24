use crate::launch::StreamingConfig;
use serde::Deserialize;
use std::net::SocketAddr;

const FALLBACK_HOSTNAME: &str = "Amaterasu";
const HOSTNAME_BUFFER_LENGTH: usize = 256;
pub const DEFAULT_PORT: u16 = 47_989;
pub const MIN_PORT: u16 = 1_029;
pub const MAX_PORT: u16 = 65_514;
pub const HTTPS_PORT_OFFSET: u16 = 5;

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
            hostname: default_hostname(),
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
        validate_configured_hostname(&self.hostname)?;
        validate_hostname(&effective_hostname(&self.hostname))?;
        if self.http_bind == self.https_bind && self.http_bind.port() != 0 {
            return Err("listener_addresses_conflict");
        }
        if let Some(streaming) = &self.streaming {
            streaming.validate()?;
        }
        Ok(())
    }
}

/// Returns the configured hostname, or the machine hostname when the setting is blank.
pub fn effective_hostname(configured: &str) -> String {
    if configured.is_empty() {
        default_hostname()
    } else {
        configured.to_owned()
    }
}

fn default_hostname() -> String {
    hostname_or_fallback(system_hostname())
}

fn hostname_or_fallback(hostname: Option<String>) -> String {
    hostname
        .filter(|hostname| validate_hostname(hostname).is_ok())
        .unwrap_or_else(|| FALLBACK_HOSTNAME.into())
}

#[cfg(unix)]
fn system_hostname() -> Option<String> {
    use std::ffi::{CStr, c_char};

    unsafe extern "C" {
        fn gethostname(name: *mut c_char, length: usize) -> i32;
    }

    // Reserve the final byte for a terminator if the platform fills the buffer.
    let mut buffer = [0 as c_char; HOSTNAME_BUFFER_LENGTH];
    // SAFETY: `buffer` is process-owned and writable for `HOSTNAME_BUFFER_LENGTH`
    // bytes. The synchronous POSIX call may write at most the provided length and
    // does not retain the pointer after returning.
    let result = unsafe { gethostname(buffer.as_mut_ptr(), HOSTNAME_BUFFER_LENGTH - 1) };
    if result != 0 {
        return None;
    }

    // SAFETY: the buffer remains owned and alive here. It was zero-initialized
    // and its final byte was excluded from the OS write, so it contains a NUL
    // terminator and the returned CStr borrow cannot outlive this buffer.
    let hostname = unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    (!hostname.is_empty()).then_some(hostname)
}

#[cfg(windows)]
fn system_hostname() -> Option<String> {
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        #[link_name = "GetComputerNameW"]
        fn get_computer_name(buffer: *mut u16, length: *mut u32) -> i32;
    }

    let mut buffer = [0_u16; HOSTNAME_BUFFER_LENGTH];
    let mut length = HOSTNAME_BUFFER_LENGTH as u32;
    // SAFETY: `buffer` is process-owned and remains alive for this synchronous
    // call. The Win32 function receives its writable code-unit capacity and does
    // not retain the pointer after returning.
    let result = unsafe { get_computer_name(buffer.as_mut_ptr(), &mut length) };
    if result == 0 || length == 0 || length as usize > buffer.len() {
        return None;
    }
    String::from_utf16(&buffer[..length as usize]).ok()
}

#[cfg(not(any(unix, windows)))]
fn system_hostname() -> Option<String> {
    None
}

pub(crate) fn validate_configured_hostname(hostname: &str) -> Result<(), &'static str> {
    if hostname.len() > 128 || hostname.chars().any(char::is_control) {
        Err("invalid_hostname")
    } else {
        Ok(())
    }
}

pub fn validate_hostname(hostname: &str) -> Result<(), &'static str> {
    if hostname.is_empty() {
        Err("invalid_hostname")
    } else {
        validate_configured_hostname(hostname)
    }
}

pub fn validate_port(port: u16) -> Result<(), &'static str> {
    if (MIN_PORT..=MAX_PORT).contains(&port) {
        Ok(())
    } else {
        Err("invalid_port")
    }
}

pub fn https_port(port: u16) -> u16 {
    port - HTTPS_PORT_OFFSET
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_bind_nothing_and_remote_opt_in_is_explicit() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.enabled);
        settings.validate().unwrap();
        assert_eq!(settings.hostname, default_hostname());
        let mut settings: Settings =
            serde_json::from_str(r#"{"http_bind":"0.0.0.0:47989"}"#).unwrap();
        assert!(settings.validate().is_err());
        settings.allow_remote = true;
        settings.validate().unwrap();
    }

    #[test]
    fn empty_hostname_uses_the_machine_default_and_nonempty_values_are_preserved() {
        let machine_hostname = default_hostname();
        assert!(!machine_hostname.is_empty());

        let empty = Settings {
            hostname: String::new(),
            ..Settings::default()
        };
        empty.validate().unwrap();
        assert_eq!(effective_hostname(&empty.hostname), machine_hostname);

        let configured = "  Living Room & Games  ";
        assert_eq!(effective_hostname(configured), configured);
        validate_configured_hostname("").unwrap();
        validate_hostname(machine_hostname.as_str()).unwrap();
    }

    #[test]
    fn configured_hostname_bounds_remain_enforced() {
        assert!(validate_configured_hostname(&"x".repeat(129)).is_err());
        assert!(validate_configured_hostname("host\nname").is_err());
        assert!(validate_hostname("").is_err());
    }

    #[test]
    fn unavailable_or_invalid_machine_hostname_uses_the_documented_fallback() {
        assert_eq!(hostname_or_fallback(None), FALLBACK_HOSTNAME);
        assert_eq!(
            hostname_or_fallback(Some("host\nname".into())),
            FALLBACK_HOSTNAME
        );
    }

    #[test]
    fn port_range_preserves_https_and_rtsp_offset_bounds() {
        assert!(validate_port(MIN_PORT).is_ok());
        assert_eq!(https_port(MIN_PORT), 1_024);
        assert!(validate_port(MAX_PORT).is_ok());
        assert_eq!(u32::from(MAX_PORT) + 21, 65_535);
        assert!(validate_port(MIN_PORT - 1).is_err());
        assert!(validate_port(MAX_PORT + 1).is_err());
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
