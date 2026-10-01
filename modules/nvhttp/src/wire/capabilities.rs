//! Host capabilities advertised in `/serverinfo`.
//!
//! The codec mask is derived from a cached snapshot of `amaterasu.encode/1`
//! `enumerate`. A refresh runs from `Router::poll`, never from a request, so
//! `/serverinfo` always answers from memory. A missing grant, a failed or
//! timed-out refresh, or a stale snapshot falls back to the H.264 baseline.
use super::{Command, ModuleResult, Response, Router, SERVICE_TIMEOUT, cleanup};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub(super) const ENCODE_SERVICE: &str = "amaterasu.encode/1";

/// `SCM_H264` in moonlight-common-c `Limelight.h`. moonlight-qt forwards the
/// advertised mask unchanged and `LiStartConnection` aborts when it is zero.
const SCM_H264: u32 = 0x0000_0001;
/// `SCM_HEVC`: 8-bit HEVC Main. The 10-bit, 4:4:4 and AV1 bits are never set.
const SCM_HEVC: u32 = 0x0000_0100;
/// The luma budget GeForce Experience and Sunshine advertise alongside HEVC.
const MAX_LUMA_PIXELS_HEVC: u32 = 1_869_449_984;
const NO_HARDWARE_ADDRESS: &str = "00:00:00:00:00:00";

const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const RETRY_INTERVAL: Duration = Duration::from_secs(10);
/// A snapshot older than this is not trusted, even if refreshes stop arriving.
const SNAPSHOT_LIFETIME: Duration = Duration::from_secs(90);

/// The fields of `/serverinfo` that describe the host rather than the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Advertised {
    pub codec_mode_support: u32,
    pub max_luma_pixels_hevc: u32,
    pub mac: String,
}

impl Advertised {
    /// H.264 only, no hardware address. Always valid for a real client.
    pub fn baseline() -> Self {
        Self {
            codec_mode_support: SCM_H264,
            max_luma_pixels_hevc: 0,
            mac: NO_HARDWARE_ADDRESS.into(),
        }
    }
}

struct Snapshot {
    observed: Instant,
    hevc_8bit: bool,
}

enum Refresh {
    Idle { due: Instant },
    Waiting { request: u64, deadline: Instant },
}

pub(super) struct HostCapabilities {
    snapshot: Option<Snapshot>,
    refresh: Refresh,
    mac: Option<String>,
}

impl HostCapabilities {
    pub fn new(now: Instant) -> Self {
        Self {
            snapshot: None,
            refresh: Refresh::Idle { due: now },
            mac: None,
        }
    }

    pub fn advertised(&self, now: Instant) -> Advertised {
        let mut advertised = Advertised::baseline();
        if let Some(mac) = &self.mac {
            advertised.mac = mac.clone();
        }
        let fresh_hevc = self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.hevc_8bit
                && now.saturating_duration_since(snapshot.observed) < SNAPSHOT_LIFETIME
        });
        if fresh_hevc {
            advertised.codec_mode_support |= SCM_HEVC;
            advertised.max_luma_pixels_hevc = MAX_LUMA_PIXELS_HEVC;
        }
        advertised
    }

    fn reset(&mut self, now: Instant) {
        self.snapshot = None;
        self.refresh = Refresh::Idle { due: now };
    }

    fn fail(&mut self, now: Instant) {
        self.snapshot = None;
        self.refresh = Refresh::Idle {
            due: now + RETRY_INTERVAL,
        };
    }
}

impl Router {
    /// Advances the background capability refresh by at most one broker call.
    pub(super) fn poll_capabilities(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) {
        if !self.launches.supported() {
            // Nothing is advertised beyond the baseline without streaming, so
            // no call (and no encoder grant) is needed.
            self.capabilities.reset(now);
            return;
        }
        match self.capabilities.refresh {
            Refresh::Idle { due } if now >= due => self.start_capability_refresh(now, send),
            Refresh::Idle { .. } => {}
            Refresh::Waiting { request, deadline } => {
                self.take_capability_result(request, deadline, now, send)
            }
        }
    }

    fn start_capability_refresh(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) {
        if self.capabilities.mac.is_none() {
            self.capabilities.mac = hardware_address();
        }
        let Some(sequence) = self.next_call.checked_add(1) else {
            self.capabilities.fail(now);
            return;
        };
        self.next_call = sequence;
        let response = send(Command::Request {
            service: ENCODE_SERVICE.into(),
            payload: json!({"action":"enumerate"}),
            timeout_ms: SERVICE_TIMEOUT.as_millis() as u64,
            operation_id: format!("nvhttp-capabilities-{sequence}"),
            parent: None,
        });
        match response {
            Ok(Response::Accepted { id }) => {
                self.capabilities.refresh = Refresh::Waiting {
                    request: id,
                    deadline: now + SERVICE_TIMEOUT,
                };
            }
            _ => self.capabilities.fail(now),
        }
    }

    fn take_capability_result(
        &mut self,
        request: u64,
        deadline: Instant,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) {
        if now >= deadline {
            self.retire_request(request, now, send);
            self.capabilities.fail(now);
            return;
        }
        let response = send(Command::TakeResult { request });
        match response {
            Ok(Response::Pending) => {}
            Ok(Response::Result { payload }) => match parse_enumeration(&payload) {
                Some(hevc_8bit) => {
                    self.capabilities.snapshot = Some(Snapshot {
                        observed: now,
                        hevc_8bit,
                    });
                    self.capabilities.refresh = Refresh::Idle {
                        due: now + REFRESH_INTERVAL,
                    };
                }
                None => self.capabilities.fail(now),
            },
            response => {
                if !cleanup::released(&response) {
                    self.retire_request(request, now, send);
                }
                self.capabilities.fail(now);
            }
        }
    }
}

/// Returns whether the enumeration proves an 8-bit HEVC encoder, or `None`
/// when the reply is not a valid enumeration. No encoder backend lists `hevc`
/// today, so this stays false until one can deliver 8-bit HEVC end to end; an
/// entry in the enumeration is the only evidence this adapter accepts.
fn parse_enumeration(payload: &Value) -> Option<bool> {
    if payload.get("ok") != Some(&Value::Bool(true)) {
        return None;
    }
    let enabled = payload.get("enabled") == Some(&Value::Bool(true));
    let encoders = payload.get("encoders")?.as_array()?;
    let proven = encoders.iter().any(|encoder| {
        encoder.get("codec").and_then(Value::as_str) == Some("hevc")
            && encoder.get("hardware") == Some(&Value::Bool(true))
    });
    Some(enabled && proven)
}

/// Best-effort MAC of the first physical network interface, for Wake-on-LAN.
/// Virtual, loopback and unreadable interfaces yield `None` (all zeros).
#[cfg(target_os = "linux")]
fn hardware_address() -> Option<String> {
    let mut names: Vec<_> = std::fs::read_dir("/sys/class/net")
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    names.into_iter().find_map(|name| {
        let base = std::path::Path::new("/sys/class/net").join(&name);
        // Physical interfaces expose a backing `device`; bridges, veths and
        // tunnels do not.
        base.join("device").exists().then_some(())?;
        let address = std::fs::read_to_string(base.join("address")).ok()?;
        valid_hardware_address(address.trim())
    })
}

#[cfg(not(target_os = "linux"))]
fn hardware_address() -> Option<String> {
    None
}

#[cfg(any(target_os = "linux", test))]
fn valid_hardware_address(text: &str) -> Option<String> {
    let octets: Vec<_> = text.split(':').collect();
    let well_formed = octets.len() == 6
        && octets
            .iter()
            .all(|octet| octet.len() == 2 && octet.bytes().all(|byte| byte.is_ascii_hexdigit()));
    (well_formed && text != NO_HARDWARE_ADDRESS).then(|| text.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_addresses_must_be_six_hex_octets_and_nonzero() {
        assert_eq!(
            valid_hardware_address("AA:bb:0c:0D:ee:ff").as_deref(),
            Some("aa:bb:0c:0d:ee:ff")
        );
        for rejected in [
            "",
            NO_HARDWARE_ADDRESS,
            "aa:bb:cc:dd:ee",
            "aa:bb:cc:dd:ee:ff:00",
            "aa:bb:cc:dd:ee:g0",
            "a:bb:cc:dd:ee:ff",
        ] {
            assert_eq!(valid_hardware_address(rejected), None, "{rejected}");
        }
    }

    #[test]
    fn enumeration_proves_hevc_only_for_an_enabled_hardware_hevc_entry() {
        let h264 = json!({"codec":"h264","hardware":true});
        let hevc = json!({"codec":"hevc","hardware":true});
        let software_hevc = json!({"codec":"hevc","hardware":false});
        let reply = |enabled: bool, encoders: Vec<Value>| json!({"ok":true,"enabled":enabled,"encoders":encoders});
        assert_eq!(
            parse_enumeration(&reply(true, vec![h264.clone()])),
            Some(false)
        );
        assert_eq!(parse_enumeration(&reply(true, vec![])), Some(false));
        assert_eq!(
            parse_enumeration(&reply(true, vec![h264.clone(), software_hevc])),
            Some(false)
        );
        assert_eq!(
            parse_enumeration(&reply(false, vec![hevc.clone()])),
            Some(false)
        );
        assert_eq!(
            parse_enumeration(&reply(true, vec![h264, hevc])),
            Some(true)
        );
        assert_eq!(parse_enumeration(&json!({"ok":false})), None);
        assert_eq!(parse_enumeration(&json!({"ok":true})), None);
        assert_eq!(
            parse_enumeration(&json!({"ok":true,"encoders":"hevc"})),
            None
        );
    }

    #[test]
    fn snapshot_expires_back_to_the_baseline() {
        let start = Instant::now();
        let mut capabilities = HostCapabilities::new(start);
        capabilities.snapshot = Some(Snapshot {
            observed: start,
            hevc_8bit: true,
        });
        let fresh = capabilities.advertised(start + SNAPSHOT_LIFETIME - Duration::from_secs(1));
        assert_eq!(fresh.codec_mode_support, SCM_H264 | SCM_HEVC);
        assert_eq!(fresh.max_luma_pixels_hevc, MAX_LUMA_PIXELS_HEVC);
        let stale = capabilities.advertised(start + SNAPSHOT_LIFETIME);
        assert_eq!(stale, Advertised::baseline());
    }

    #[test]
    fn advertised_mask_never_sets_ten_bit_444_or_av1_bits() {
        let start = Instant::now();
        let mut capabilities = HostCapabilities::new(start);
        capabilities.snapshot = Some(Snapshot {
            observed: start,
            hevc_8bit: true,
        });
        let mask = capabilities.advertised(start).codec_mode_support;
        assert_eq!(mask & !(SCM_H264 | SCM_HEVC), 0);
    }
}
