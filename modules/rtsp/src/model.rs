use amaterasu_gamestream::{
    rtsp::negotiation::UdpDiscovery,
    sdp::{Announce, ColorMode, ControlProtocol},
};
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

const DEFAULT_BASE_PORT: u16 = 47_989;
const MIN_BASE_PORT: u16 = 1_029;
const MAX_BASE_PORT: u16 = 65_514;
const RTSP_SETUP_OFFSET: u16 = 21;
const VIDEO_OFFSET: u16 = 9;
const CONTROL_OFFSET: u16 = 10;
const AUDIO_OFFSET: u16 = 11;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AddressFamily {
    #[default]
    Ipv4,
    Both,
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub allow_remote: bool,
    pub bind: SocketAddr,
    pub address_family: Option<AddressFamily>,
    pub bind_address: Option<String>,
    pub allowed_principals: Vec<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_remote: false,
            bind: SocketAddr::from(([127, 0, 0, 1], DEFAULT_BASE_PORT + RTSP_SETUP_OFFSET)),
            address_family: None,
            bind_address: None,
            allowed_principals: Vec::new(),
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.allowed_principals.len() > 16
            || self.allowed_principals.iter().any(|name| !valid_name(name))
        {
            return Err("invalid_settings");
        }
        self.resolved_bind()?;
        Ok(())
    }

    pub fn resolved_bind(&self) -> Result<SocketAddr, &'static str> {
        self.resolved_binds()?
            .into_iter()
            .next()
            .ok_or("invalid_bind")
    }

    pub fn resolved_binds(&self) -> Result<Vec<SocketAddr>, &'static str> {
        Ok(self
            .resolved_bind_ips()?
            .into_iter()
            .map(|ip| SocketAddr::new(ip, self.bind.port()))
            .collect())
    }

    fn resolved_bind_ips(&self) -> Result<Vec<IpAddr>, &'static str> {
        let family = self.resolved_address_family();
        let addresses = match self.bind_address.as_deref() {
            None => vec![self.bind.ip()],
            Some(configured) if configured.trim().is_empty() => match (family, self.allow_remote) {
                (AddressFamily::Ipv4, true) => vec![IpAddr::V4(Ipv4Addr::UNSPECIFIED)],
                (AddressFamily::Ipv4, false) => vec![self.bind.ip()],
                (AddressFamily::Both, true) => vec![
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                    IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                ],
                (AddressFamily::Both, false) => vec![
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    IpAddr::V6(Ipv6Addr::LOCALHOST),
                ],
            },
            Some(configured) => vec![
                configured
                    .trim()
                    .parse::<IpAddr>()
                    .map_err(|_| "invalid_bind")?,
            ],
        };
        addresses
            .into_iter()
            .map(|ip| {
                if !family.supports(ip) {
                    return Err("address_family_mismatch");
                }
                if invalid_configured_bind(ip)
                    || (self.bind_address.is_none() && invalid_bind_address(ip))
                    || (!self.allow_remote && !ip.is_loopback())
                {
                    return Err("invalid_bind");
                }
                Ok(ip)
            })
            .collect()
    }

    fn resolved_address_family(&self) -> AddressFamily {
        self.address_family.unwrap_or_else(|| {
            let ip = self
                .bind_address
                .as_deref()
                .and_then(|address| address.trim().parse::<IpAddr>().ok())
                .unwrap_or_else(|| self.bind.ip());
            if ip.is_ipv4() {
                AddressFamily::Ipv4
            } else {
                AddressFamily::Both
            }
        })
    }

    pub fn default_base_port(&self) -> Option<u16> {
        let port = self.bind.port();
        if port == 0 {
            None
        } else {
            port.checked_sub(RTSP_SETUP_OFFSET)
                .filter(|base| (MIN_BASE_PORT..=MAX_BASE_PORT).contains(base))
        }
    }

    pub fn set_base_port(&mut self, base_port: u16) -> Result<(), &'static str> {
        validate_base_port(base_port)?;
        self.bind.set_port(base_port + RTSP_SETUP_OFFSET);
        Ok(())
    }

    #[cfg(test)]
    pub fn rtsp_bind_for_base(&self, base_port: u16) -> Result<SocketAddr, &'static str> {
        self.rtsp_binds_for_base(base_port)?
            .into_iter()
            .next()
            .ok_or("invalid_bind")
    }

    pub fn rtsp_binds_for_base(&self, base_port: u16) -> Result<Vec<SocketAddr>, &'static str> {
        validate_base_port(base_port)?;
        Ok(self
            .resolved_bind_ips()?
            .into_iter()
            .map(|address| SocketAddr::new(address, base_port + RTSP_SETUP_OFFSET))
            .collect())
    }

    #[cfg(test)]
    pub fn media_binds(&self, base_port: u16) -> Result<MediaBinds, &'static str> {
        let address = self
            .resolved_bind_ips()?
            .into_iter()
            .next()
            .ok_or("invalid_bind")?;
        self.media_binds_on(base_port, address)
    }

    pub fn media_binds_on(
        &self,
        base_port: u16,
        local_address: IpAddr,
    ) -> Result<MediaBinds, &'static str> {
        validate_base_port(base_port)?;
        let configured_addresses = self.resolved_bind_ips()?;
        let configured_address = configured_addresses
            .into_iter()
            .find(|address| address.is_ipv4() == local_address.is_ipv4())
            .ok_or("address_family_mismatch")?;
        let address = if configured_address.is_unspecified() {
            local_address
        } else {
            configured_address
        };
        if address.is_ipv4() != configured_address.is_ipv4() {
            return Err("address_family_mismatch");
        }
        Ok(MediaBinds {
            video: SocketAddr::new(address, base_port + VIDEO_OFFSET),
            control: SocketAddr::new(address, base_port + CONTROL_OFFSET),
            audio: SocketAddr::new(address, base_port + AUDIO_OFFSET),
        })
    }

    pub fn probe_media_binds(&self, base_port: u16) -> Result<(), &'static str> {
        let binds = self.media_binds_by_family(base_port)?;
        // Hold every UDP socket until the complete set has been checked. The
        // stream-session service later binds its per-session sockets from the
        // same addresses; this preflight cannot reserve sockets across services.
        let mut sockets = Vec::with_capacity(binds.len() * 3);
        for bind in binds
            .iter()
            .flat_map(|family| [family.video, family.control, family.audio])
        {
            let socket = Socket::new(Domain::for_address(bind), Type::DGRAM, Some(Protocol::UDP))
                .map_err(|_| "udp_bind_failed")?;
            if bind.is_ipv6() {
                socket.set_only_v6(true).map_err(|_| "udp_bind_failed")?;
            }
            socket
                .bind(&SockAddr::from(bind))
                .map_err(|_| "udp_bind_failed")?;
            sockets.push(UdpSocket::from(socket));
        }
        Ok(())
    }

    pub fn media_binds_by_family(&self, base_port: u16) -> Result<Vec<MediaBinds>, &'static str> {
        self.resolved_bind_ips()?
            .into_iter()
            .map(|address| self.media_binds_on(base_port, address))
            .collect()
    }
}

impl AddressFamily {
    fn supports(self, address: IpAddr) -> bool {
        matches!(self, Self::Both) || address.is_ipv4()
    }
}

fn invalid_configured_bind(address: IpAddr) -> bool {
    address.is_multicast()
        || address == IpAddr::V4(Ipv4Addr::BROADCAST)
        || matches!(address, IpAddr::V6(address) if address.to_ipv4_mapped().is_some())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaBinds {
    pub video: SocketAddr,
    pub control: SocketAddr,
    pub audio: SocketAddr,
}

pub fn validate_base_port(port: u16) -> Result<(), &'static str> {
    if (MIN_BASE_PORT..=MAX_BASE_PORT).contains(&port) {
        Ok(())
    } else {
        Err("invalid_port")
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub capture_provider: CaptureProvider,
    pub output: String,
    pub encoder: u32,
    pub audio_endpoint: String,
}
#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureProvider {
    Dxgi,
    Wgc,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub max_reference_frames: u32,
    pub packet_size: usize,
    pub fec_percentage: u8,
    pub min_parity_shards: u8,
    pub max_fec_blocks: u8,
    pub packet_ms: u32,
    pub audio_bitrate: u32,
    pub encrypt_video: bool,
    pub encrypt_audio: bool,
    pub permit_legacy_audio_iv_wrap: bool,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 10000,
            max_reference_frames: 1,
            packet_size: 1024,
            fec_percentage: 20,
            min_parity_shards: 2,
            max_fec_blocks: 1,
            packet_ms: 10,
            audio_bitrate: 128000,
            encrypt_video: true,
            encrypt_audio: true,
            permit_legacy_audio_iv_wrap: false,
        }
    }
}
impl Profile {
    pub fn matches(&self, announce: &Announce) -> bool {
        self.matches_video(announce)
            && self.matches_audio(announce)
            && self.matches_transport(announce)
    }

    fn matches_video(&self, announce: &Announce) -> bool {
        announce.video.width == self.width
            && announce.video.height == self.height
            && announce.video.fps == self.fps
            && announce.video.bitrate_kbps == self.bitrate_kbps
            && announce.video.max_reference_frames == self.max_reference_frames
            && announce.video.color == ColorMode::Bt709Limited
            && announce.encryption.video == self.encrypt_video
    }

    fn matches_audio(&self, announce: &Announce) -> bool {
        announce.audio.channels == 2
            && announce.audio.channel_mask == 3
            && announce.audio.packet_duration_ms == self.packet_ms
            && announce.encryption.audio == self.encrypt_audio
    }

    fn matches_transport(&self, announce: &Announce) -> bool {
        announce.packet_size as usize == self.packet_size
            && announce.control == ControlProtocol::EncryptedEnet
            && announce.encryption.control_v2
            && announce.fec.minimum_packets == u32::from(self.min_parity_shards)
            && announce
                .fec
                .repair_percent
                .is_none_or(|percent| percent == u32::from(self.fec_percentage))
    }

    fn validate(&self) -> Result<(), &'static str> {
        if !(16..=3840).contains(&self.width)
            || !(16..=2160).contains(&self.height)
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
            || !(1..=120).contains(&self.fps)
            || !(100..=100000).contains(&self.bitrate_kbps)
            || self.max_reference_frames > 1
            || !matches!(self.packet_ms, 5 | 10)
            || !(6000..=256000).contains(&self.audio_bitrate)
        {
            return Err("invalid_profile");
        }

        amaterasu_gamestream::video::VideoPacketizer::new(
            amaterasu_gamestream::video::VideoConfig {
                packet_size: self.packet_size,
                fec_percentage: self.fec_percentage,
                min_parity_shards: self.min_parity_shards,
                max_fec_blocks: self.max_fec_blocks,
                // Encryption is applied by media transport after packetization.
                encrypted: false,
            },
        )
        .map_err(|_| "invalid_profile")?;
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Open {
    pub operation_id: String,
    pub peer_ip: IpAddr,
    #[serde(default)]
    pub video_port: u16,
    #[serde(default)]
    pub audio_port: u16,
    #[serde(default)]
    pub discover_peers: bool,
    pub key_hex: String,
    pub key_id: u32,
    pub connect_data: u32,
    pub capture: Capture,
    #[serde(default)]
    pub profile: Profile,
}
impl Open {
    pub fn validate(&self, allow_remote: bool) -> Result<[u8; 16], &'static str> {
        self.validate_operation_id()?;
        self.validate_destination(allow_remote)?;
        self.validate_capture()?;
        self.profile.validate()?;
        decode_key(&self.key_hex)
    }

    fn validate_operation_id(&self) -> Result<(), &'static str> {
        if !valid_name(&self.operation_id) {
            return Err("invalid_operation_id");
        }
        Ok(())
    }

    fn validate_destination(&self, allow_remote: bool) -> Result<(), &'static str> {
        let invalid_ports = if self.discover_peers {
            self.video_port != 0 || self.audio_port != 0
        } else {
            self.video_port == 0 || self.audio_port == 0
        };
        if invalid_peer(self.peer_ip) || invalid_ports {
            return Err("invalid_peer");
        }
        if !allow_remote && !self.peer_ip.is_loopback() {
            return Err("remote_disabled");
        }
        Ok(())
    }

    fn validate_capture(&self) -> Result<(), &'static str> {
        if !valid_label(&self.capture.output, 512)
            || !valid_label(&self.capture.audio_endpoint, 4096)
        {
            return Err("invalid_profile");
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn prepare(&self, operation: &str, discovery: Option<&UdpDiscovery>) -> serde_json::Value {
        self.prepare_with_media_binds(operation, discovery, None)
    }

    pub fn prepare_with_media_binds(
        &self,
        operation: &str,
        discovery: Option<&UdpDiscovery>,
        binds: Option<MediaBinds>,
    ) -> serde_json::Value {
        let mut payload = serde_json::json!({
            "action": "prepare",
            "operation_id": operation,
            "peer_ip": self.peer_ip,
            "key_hex": self.key_hex,
            "key_id": self.key_id,
            "connect_data": self.connect_data,
            "video": {
                "packet_size": self.profile.packet_size,
                "fec_percentage": self.profile.fec_percentage,
                "min_parity_shards": self.profile.min_parity_shards,
                "max_fec_blocks": self.profile.max_fec_blocks,
            },
            "audio": {
                "packet_ms": self.profile.packet_ms,
                "bitrate": self.profile.audio_bitrate,
            },
            "encrypt_video": self.profile.encrypt_video,
            "encrypt_audio": self.profile.encrypt_audio,
            "permit_legacy_audio_iv_wrap": self.profile.permit_legacy_audio_iv_wrap,
        });
        if let Some(discovery) = discovery {
            payload["discovery"] = serde_json::json!({
                "audio_ping_payload": discovery.audio_ping_payload,
                "video_ping_payload": discovery.video_ping_payload,
            });
        } else {
            payload["video_port"] = self.video_port.into();
            payload["audio_port"] = self.audio_port.into();
        }
        if let Some(binds) = binds {
            payload["video_bind"] = serde_json::json!(binds.video);
            payload["control_bind"] = serde_json::json!(binds.control);
            payload["audio_bind"] = serde_json::json!(binds.audio);
        }
        payload
    }

    pub fn arm(&self, session: u64) -> serde_json::Value {
        serde_json::json!({
            "action": "start_when_authenticated",
            "session": session,
            "configuration": {
                "capture_provider": self.capture.capture_provider,
                "output": self.capture.output,
                "encoder": self.capture.encoder,
                "audio_endpoint": self.capture.audio_endpoint,
                "configuration": {
                    "width": self.profile.width,
                    "height": self.profile.height,
                    "fps": self.profile.fps,
                    "bitrate": self.profile.bitrate_kbps * 1000,
                    "max_reference_frames": self.profile.max_reference_frames,
                },
            },
        })
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Open(Open),
    NetworkConfigure {
        port: u16,
        #[serde(default)]
        address_family: Option<AddressFamily>,
        #[serde(default)]
        bind_address: Option<String>,
    },
    Close {
        ticket: u64,
    },
    Status {
        ticket: Option<u64>,
    },
    Lookup {
        operation_id: String,
    },
}

pub fn valid_name(value: &str) -> bool {
    valid_label(value, 128)
}
fn valid_label(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn invalid_peer(address: IpAddr) -> bool {
    address.is_unspecified()
        || address.is_multicast()
        || address == IpAddr::V4(Ipv4Addr::BROADCAST)
        || is_ipv4_mapped(address)
}

fn invalid_bind_address(address: IpAddr) -> bool {
    address.is_unspecified()
        || address.is_multicast()
        || address == IpAddr::V4(Ipv4Addr::BROADCAST)
        || is_ipv4_mapped(address)
}

fn is_ipv4_mapped(address: IpAddr) -> bool {
    matches!(address, IpAddr::V6(address) if address.to_ipv4_mapped().is_some())
}

fn decode_key(value: &str) -> Result<[u8; 16], &'static str> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid_key");
    }
    let mut key = [0; 16];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).map_err(|_| "invalid_key")?;
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_gamestream::sdp::parse_announce;
    use serde_json::{Value, json};

    const PRIMARY_ANNOUNCE: &str =
        include_str!("../../../crates/gamestream/src/sdp/fixtures/moonlight-h264.sdp");

    fn open_json() -> Value {
        json!({
            "operation_id": "launch-one",
            "peer_ip": "127.0.0.1",
            "video_port": 47998,
            "audio_port": 48000,
            "key_hex": "00112233445566778899aabbccddeeff",
            "key_id": 7,
            "connect_data": 11,
            "capture": {
                "capture_provider": "dxgi",
                "output": "display-one",
                "encoder": 0,
                "audio_endpoint": "default"
            }
        })
    }

    fn open() -> Open {
        serde_json::from_value(open_json()).unwrap()
    }

    fn open_operation_json() -> Value {
        let mut value = open_json();
        value["action"] = json!("open");
        value
    }

    fn changed_announce(name: &str, value: &str) -> String {
        let prefix = format!("a={name}:");
        PRIMARY_ANNOUNCE
            .split_inclusive("\r\n")
            .map(|line| {
                if line.starts_with(&prefix) {
                    format!("{prefix}{value} \r\n")
                } else {
                    line.into()
                }
            })
            .collect()
    }

    fn matching_profile() -> Profile {
        Profile {
            bitrate_kbps: 16000,
            packet_size: 1392,
            packet_ms: 5,
            ..Profile::default()
        }
    }

    #[test]
    fn settings_default_to_a_disabled_loopback_listener() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.enabled);
        assert!(!settings.allow_remote);
        assert_eq!(settings.bind, "127.0.0.1:48010".parse().unwrap());
        assert!(settings.address_family.is_none());
        assert!(settings.bind_address.is_none());
        assert!(settings.allowed_principals.is_empty());
        assert_eq!(settings.validate(), Ok(()));

        for value in [
            json!({"allowed_principals": vec!["owner"; 17]}),
            json!({"allowed_principals": [""]}),
            json!({"allowed_principals": ["line\nfeed"]}),
        ] {
            assert_eq!(
                serde_json::from_value::<Settings>(value)
                    .unwrap()
                    .validate(),
                Err("invalid_settings")
            );
        }
    }

    #[test]
    fn global_port_uses_vibeshine_rtsp_and_media_offsets_on_the_configured_ip() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        let binds = settings.media_binds(DEFAULT_BASE_PORT).unwrap();
        assert_eq!(binds.video, "127.0.0.1:47998".parse().unwrap());
        assert_eq!(binds.control, "127.0.0.1:47999".parse().unwrap());
        assert_eq!(binds.audio, "127.0.0.1:48000".parse().unwrap());
        assert_eq!(
            settings.rtsp_bind_for_base(DEFAULT_BASE_PORT).unwrap(),
            "127.0.0.1:48010".parse().unwrap()
        );

        let ipv6: Settings = serde_json::from_value(serde_json::json!({
            "bind": "[::1]:48010"
        }))
        .unwrap();
        let ipv6_binds = ipv6.media_binds(48_000).unwrap();
        assert_eq!(ipv6_binds.video, "[::1]:48009".parse().unwrap());
        assert_eq!(ipv6_binds.control, "[::1]:48010".parse().unwrap());
        assert_eq!(ipv6_binds.audio, "[::1]:48011".parse().unwrap());

        for invalid in [0, 1_028, 65_515, u16::MAX] {
            assert_eq!(settings.media_binds(invalid), Err("invalid_port"));
            assert_eq!(settings.rtsp_bind_for_base(invalid), Err("invalid_port"));
        }
    }

    #[test]
    fn configured_family_and_address_drive_rtsp_and_all_udp_listener_addresses() {
        let settings: Settings = serde_json::from_value(json!({
            "address_family": "both",
            "bind_address": " ::1 ",
            "bind": "127.0.0.1:48010"
        }))
        .unwrap();
        assert_eq!(settings.validate(), Ok(()));
        assert_eq!(
            settings.resolved_bind().unwrap(),
            "[::1]:48010".parse().unwrap()
        );
        assert_eq!(
            settings.rtsp_bind_for_base(DEFAULT_BASE_PORT).unwrap(),
            "[::1]:48010".parse().unwrap()
        );
        assert_eq!(
            settings.media_binds(DEFAULT_BASE_PORT).unwrap(),
            MediaBinds {
                video: "[::1]:47998".parse().unwrap(),
                control: "[::1]:47999".parse().unwrap(),
                audio: "[::1]:48000".parse().unwrap(),
            }
        );

        let mismatch: Settings = serde_json::from_value(json!({
            "address_family": "ipv4",
            "bind_address": "::1"
        }))
        .unwrap();
        assert_eq!(mismatch.validate(), Err("address_family_mismatch"));

        let malformed: Settings = serde_json::from_value(json!({
            "address_family": "both",
            "bind_address": "rtsp.example"
        }))
        .unwrap();
        assert_eq!(malformed.validate(), Err("invalid_bind"));
    }

    #[test]
    fn explicit_empty_bind_address_uses_family_wildcard_only_when_remote_is_enabled() {
        let settings: Settings = serde_json::from_value(json!({
            "enabled": true,
            "allow_remote": true,
            "address_family": "both",
            "bind_address": "",
            "bind": "127.0.0.1:48010"
        }))
        .unwrap();
        assert_eq!(
            settings.resolved_binds().unwrap(),
            vec![
                "0.0.0.0:48010".parse().unwrap(),
                "[::]:48010".parse().unwrap()
            ]
        );
        assert_eq!(
            settings.media_binds_by_family(DEFAULT_BASE_PORT).unwrap(),
            vec![
                MediaBinds {
                    video: "0.0.0.0:47998".parse().unwrap(),
                    control: "0.0.0.0:47999".parse().unwrap(),
                    audio: "0.0.0.0:48000".parse().unwrap(),
                },
                MediaBinds {
                    video: "[::]:47998".parse().unwrap(),
                    control: "[::]:47999".parse().unwrap(),
                    audio: "[::]:48000".parse().unwrap(),
                }
            ]
        );
    }

    #[test]
    fn dual_family_udp_preflight_checks_the_ipv6_addresses_too() {
        let settings: Settings = serde_json::from_value(json!({
            "enabled": true,
            "allow_remote": true,
            "address_family": "both",
            "bind_address": "",
            "bind": "127.0.0.1:48010"
        }))
        .unwrap();
        let base_port = (20_000..=30_000)
            .find(|port| settings.probe_media_binds(*port).is_ok())
            .expect("find an available local UDP bind set");
        settings
            .probe_media_binds(base_port)
            .expect("all six family-specific UDP sockets bind together");

        let occupied_address =
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), base_port + VIDEO_OFFSET);
        let blocker = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        blocker.set_only_v6(true).unwrap();
        blocker.bind(&SockAddr::from(occupied_address)).unwrap();
        let _blocker = UdpSocket::from(blocker);

        assert_eq!(
            settings.probe_media_binds(base_port),
            Err("udp_bind_failed"),
            "a conflict on only the IPv6 video bind must defer network activation"
        );
    }

    #[test]
    fn rtsp_prepare_includes_configured_server_udp_source_ports() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        let binds = settings.media_binds(DEFAULT_BASE_PORT).unwrap();
        let payload = open().prepare_with_media_binds("operation", None, Some(binds));
        assert_eq!(payload["video_bind"], "127.0.0.1:47998");
        assert_eq!(payload["control_bind"], "127.0.0.1:47999");
        assert_eq!(payload["audio_bind"], "127.0.0.1:48000");

        let discovery = UdpDiscovery {
            video_ping_payload: "video-token-0001".into(),
            audio_ping_payload: "audio-token-0001".into(),
        };
        let payload = open().prepare_with_media_binds("discovery", Some(&discovery), Some(binds));
        assert!(payload.get("video_port").is_none());
        assert!(payload.get("audio_port").is_none());
        assert_eq!(payload["video_bind"], "127.0.0.1:47998");
        assert_eq!(payload["audio_bind"], "127.0.0.1:48000");
    }

    #[test]
    fn settings_reject_unusable_listener_addresses_and_enforce_remote_policy() {
        for address in [
            "0.0.0.0:48010",
            "[::]:48010",
            "239.1.2.3:48010",
            "[ff02::1]:48010",
            "255.255.255.255:48010",
            "[::ffff:127.0.0.1]:48010",
            "[::ffff:192.0.2.1]:48010",
        ] {
            let settings: Settings = serde_json::from_value(json!({
                "allow_remote": true,
                "bind": address
            }))
            .unwrap();
            assert_eq!(settings.validate(), Err("invalid_bind"), "{address}");
        }

        for address in ["192.0.2.1:48010", "[2001:db8::1]:48010"] {
            let local_only: Settings = serde_json::from_value(json!({"bind": address})).unwrap();
            assert_eq!(local_only.validate(), Err("invalid_bind"), "{address}");
            let remote: Settings =
                serde_json::from_value(json!({"allow_remote": true, "bind": address})).unwrap();
            assert_eq!(remote.validate(), Ok(()), "{address}");
        }

        let ipv6_loopback: Settings =
            serde_json::from_value(json!({"bind": "[::1]:48010"})).unwrap();
        assert_eq!(ipv6_loopback.validate(), Ok(()));
    }

    #[test]
    fn keys_require_exact_ascii_hex_without_panicking_on_utf8() {
        assert_eq!(
            decode_key("00112233445566778899aAbBcCdDeEfF"),
            Ok([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ])
        );
        for value in [
            "",
            "00112233445566778899aabbccddeef",
            "00112233445566778899aabbccddeeff00",
            "00112233445566778899aabbccddeezz",
            &"é".repeat(16),
            &format!("é{}", "0".repeat(30)),
        ] {
            assert!(
                matches!(
                    std::panic::catch_unwind(|| decode_key(value)),
                    Ok(Err("invalid_key"))
                ),
                "{value:?}"
            );
        }
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_configuration_boundary() {
        assert!(serde_json::from_value::<Settings>(json!({"listen_backlog": 8})).is_err());

        let mut operation = open_operation_json();
        operation["administrator"] = json!(true);
        assert!(serde_json::from_value::<Operation>(operation).is_err());

        let mut operation = open_operation_json();
        operation["capture"]["codec"] = json!("h264");
        assert!(serde_json::from_value::<Operation>(operation).is_err());

        let mut operation = open_operation_json();
        operation["profile"] = json!({"unsupported_fec_mode": 2});
        assert!(serde_json::from_value::<Operation>(operation).is_err());

        assert!(
            serde_json::from_value::<Operation>(
                json!({"action": "status", "ticket": null, "owner": "forged"})
            )
            .is_err()
        );
    }

    #[test]
    fn peer_validation_rejects_invalid_addresses_and_zero_ports_for_both_families() {
        for address in [
            "0.0.0.0",
            "::",
            "239.1.2.3",
            "ff02::1",
            "255.255.255.255",
            "::ffff:127.0.0.1",
            "::ffff:192.0.2.1",
        ] {
            let mut request = open();
            request.peer_ip = address.parse().unwrap();
            assert_eq!(request.validate(true), Err("invalid_peer"), "{address}");
        }

        let mut request = open();
        request.peer_ip = "192.0.2.1".parse().unwrap();
        assert_eq!(request.validate(false), Err("remote_disabled"));
        assert!(request.validate(true).is_ok());

        request.peer_ip = "::1".parse().unwrap();
        assert!(request.validate(false).is_ok());
        request.video_port = 0;
        assert_eq!(request.validate(false), Err("invalid_peer"));
        request.video_port = 47998;
        request.audio_port = 0;
        assert_eq!(request.validate(false), Err("invalid_peer"));
    }

    #[test]
    fn discovery_and_fixed_port_modes_are_unambiguous_on_both_json_boundaries() {
        let mut discovery_json = open_json();
        discovery_json.as_object_mut().unwrap().remove("video_port");
        discovery_json.as_object_mut().unwrap().remove("audio_port");
        discovery_json["discover_peers"] = json!(true);
        let discovery: Open = serde_json::from_value(discovery_json.clone()).unwrap();
        assert!(discovery.validate(false).is_ok());

        for (field, port) in [("video_port", 47998), ("audio_port", 48000)] {
            let mut mixed = discovery_json.clone();
            mixed[field] = json!(port);
            assert_eq!(
                serde_json::from_value::<Open>(mixed)
                    .unwrap()
                    .validate(false),
                Err("invalid_peer"),
                "{field}"
            );
        }

        let mut missing_fixed_port = open_json();
        missing_fixed_port
            .as_object_mut()
            .unwrap()
            .remove("video_port");
        assert_eq!(
            serde_json::from_value::<Open>(missing_fixed_port)
                .unwrap()
                .validate(false),
            Err("invalid_peer")
        );

        let tokens = UdpDiscovery {
            audio_ping_payload: "AudioToken_12345".into(),
            video_ping_payload: "VideoToken_12345".into(),
        };
        let prepare = discovery.prepare("rtsp-ticket", Some(&tokens));
        assert!(prepare.get("video_port").is_none());
        assert!(prepare.get("audio_port").is_none());
        assert_eq!(
            prepare["discovery"],
            json!({
                "audio_ping_payload": "AudioToken_12345",
                "video_ping_payload": "VideoToken_12345"
            })
        );

        let fixed = open().prepare("rtsp-ticket", None);
        assert_eq!(fixed["video_port"], 47998);
        assert_eq!(fixed["audio_port"], 48000);
        assert!(fixed.get("discovery").is_none());
    }

    #[test]
    fn profile_validation_covers_geometry_media_bounds_and_packetizer_configuration() {
        let request = open();
        assert!(request.validate(false).is_ok());

        for (field, profile_value) in [
            ("width", json!(15)),
            ("width", json!(17)),
            ("width", json!(3842)),
            ("height", json!(15)),
            ("height", json!(17)),
            ("height", json!(2162)),
            ("fps", json!(0)),
            ("fps", json!(121)),
            ("bitrate_kbps", json!(99)),
            ("bitrate_kbps", json!(100001)),
            ("max_reference_frames", json!(2)),
            ("packet_ms", json!(20)),
            ("audio_bitrate", json!(5999)),
            ("audio_bitrate", json!(256001)),
            ("packet_size", json!(0)),
            ("fec_percentage", json!(0)),
            ("max_fec_blocks", json!(0)),
            ("max_fec_blocks", json!(5)),
        ] {
            let mut value = open_json();
            value["profile"] = json!({(field): profile_value});
            let request: Open = serde_json::from_value(value).unwrap();
            assert_eq!(request.validate(false), Err("invalid_profile"), "{field}");
        }

        for maximum in [0, 1] {
            let mut request = open();
            request.profile.max_reference_frames = maximum;
            assert!(request.validate(false).is_ok(), "{maximum}");
        }

        for field in ["output", "audio_endpoint"] {
            let mut value = open_json();
            value["capture"][field] = json!("bad\nlabel");
            let request: Open = serde_json::from_value(value).unwrap();
            assert_eq!(request.validate(false), Err("invalid_profile"), "{field}");
        }
    }

    #[test]
    fn prepare_and_arm_forward_the_validated_profile_without_unit_ambiguity() {
        let mut request = open();
        request.profile = matching_profile();
        request.profile.max_reference_frames = 0;
        assert!(!request.profile.permit_legacy_audio_iv_wrap);
        request.profile.permit_legacy_audio_iv_wrap = true;

        let prepare = request.prepare("rtsp-ticket", None);
        assert_eq!(prepare["operation_id"], "rtsp-ticket");
        assert_eq!(prepare["video"]["packet_size"], 1392);
        assert_eq!(prepare["video"]["fec_percentage"], 20);
        assert_eq!(prepare["video"]["min_parity_shards"], 2);
        assert_eq!(prepare["video"]["max_fec_blocks"], 1);
        assert_eq!(prepare["audio"]["packet_ms"], 5);
        assert_eq!(prepare["audio"]["bitrate"], 128000);
        assert_eq!(prepare["encrypt_video"], true);
        assert_eq!(prepare["encrypt_audio"], true);
        assert_eq!(prepare["permit_legacy_audio_iv_wrap"], true);

        let arm = request.arm(41);
        assert_eq!(arm["session"], 41);
        let video = &arm["configuration"]["configuration"];
        assert_eq!(video["width"], 1920);
        assert_eq!(video["height"], 1080);
        assert_eq!(video["fps"], 60);
        assert_eq!(video["bitrate"], 16_000_000);
        assert_eq!(video["max_reference_frames"], 0);
    }

    #[test]
    fn announce_matching_requires_the_exact_supported_profile() {
        let profile = matching_profile();
        let encrypted = changed_announce("x-ss-general.encryptionEnabled", "7");
        let primary = parse_announce(encrypted.as_bytes()).unwrap();
        assert_eq!(
            (
                primary.video.width,
                primary.video.height,
                primary.video.fps,
                primary.video.bitrate_kbps,
                primary.packet_size,
                primary.audio.packet_duration_ms,
            ),
            (1920, 1080, 60, 16000, 1392, 5)
        );
        assert!(profile.matches(&primary));

        let original = parse_announce(PRIMARY_ANNOUNCE.as_bytes()).unwrap();
        assert!(!profile.matches(&original), "unencrypted ANNOUNCE matched");

        for (name, value) in [
            ("x-ss-general.encryptionEnabled", "5"),
            ("x-nv-vqos[0].fec.minRequiredFecPackets", "1"),
            ("x-nv-video[0].encoderCscMode", "3"),
            ("x-nv-video[0].maxNumReferenceFrames", "0"),
            ("x-nv-aqos.packetDuration", "10"),
        ] {
            let announce_text = changed_announce(name, value);
            let announce = parse_announce(announce_text.as_bytes()).unwrap();
            assert!(!profile.matches(&announce), "matched {name}={value}");
        }

        let fec_percent = format!("{encrypted}a=x-nv-vqos[0].fec.repairPercent:19\r\n");
        let announce = parse_announce(fec_percent.as_bytes()).unwrap();
        assert!(
            !profile.matches(&announce),
            "matched a different FEC percent"
        );

        let mut zero_reference_profile = profile.clone();
        zero_reference_profile.max_reference_frames = 0;
        let zero_reference = changed_announce("x-nv-video[0].maxNumReferenceFrames", "0");
        let zero_reference = zero_reference.replace(
            "a=x-ss-general.encryptionEnabled:0 ",
            "a=x-ss-general.encryptionEnabled:7 ",
        );
        let announce = parse_announce(zero_reference.as_bytes()).unwrap();
        assert!(zero_reference_profile.matches(&announce));
    }
}
