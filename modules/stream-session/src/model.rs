use amaterasu_contracts::virtual_display::VirtualDisplayIntent;
#[cfg(test)]
use amaterasu_module_sdk::EncoderBackend;
use amaterasu_module_sdk::EncoderSelection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub providers: crate::providers::ProviderServices,
    pub enabled: bool,
    pub allow_remote: bool,
    pub allowed_principals: Vec<String>,
    pub prepare_timeout_ms: u64,
    pub drain_timeout_ms: u64,
    pub enable_input: bool,
    pub input_lease_ttl_ms: u64,
    pub outbound_policy: Option<OutboundPolicy>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            providers: Default::default(),
            enabled: false,
            allow_remote: false,
            allowed_principals: Vec::new(),
            prepare_timeout_ms: 5000,
            drain_timeout_ms: 5000,
            enable_input: false,
            input_lease_ttl_ms: 30000,
            outbound_policy: None,
        }
    }
}
/// Trusted startup selection, independent of any peer-supplied agreement.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundPolicy {
    pub authority: String,
    pub input_service: String,
    pub approved: amaterasu_module_sdk::input::outbound::OutboundAgreement,
}

impl OutboundPolicy {
    fn valid(&self, allowed_principals: &[String]) -> bool {
        valid_name(&self.authority)
            && allowed_principals.contains(&self.authority)
            && self.input_service == amaterasu_module_sdk::input::INPUT_SERVICE
            && self.approved.validate().is_ok()
            && self
                .approved
                .schemas
                .iter()
                .all(|schema| !reserved_control_type(schema.message_type))
    }

    pub(crate) fn approves(
        &self,
        agreement: &amaterasu_module_sdk::input::outbound::OutboundAgreement,
    ) -> bool {
        agreement
            .schemas
            .iter()
            .all(|schema| self.approved.schemas.contains(schema))
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.providers.validate()?;
        if self
            .outbound_policy
            .as_ref()
            .is_some_and(|policy| !policy.valid(&self.allowed_principals))
            || self.allowed_principals.len() > 16
            || self
                .allowed_principals
                .iter()
                .any(|owner| !valid_name(owner))
            || !(100..=5000).contains(&self.prepare_timeout_ms)
            || !(100..=30000).contains(&self.drain_timeout_ms)
            || !(1000..=30000).contains(&self.input_lease_ttl_ms)
        {
            return Err("invalid_settings");
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoWire {
    /// The single codec of this stream when `codecs` is empty.
    #[serde(
        default,
        skip_serializing_if = "amaterasu_media::VideoCodec::is_default"
    )]
    pub codec: amaterasu_media::VideoCodec,
    /// Codecs offered before RTSP ANNOUNCE; authenticated start selects one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codecs: Vec<amaterasu_media::VideoCodec>,
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
impl Default for VideoWire {
    fn default() -> Self {
        Self {
            codec: amaterasu_media::VideoCodec::H264,
            codecs: Vec::new(),
            packet_size: 1024,
            fec_percentage: 20,
            min_parity_shards: 2,
            max_fec_blocks: 1,
            max_batch_size_kb: default_video_max_batch_size_kb(),
        }
    }
}
impl VideoWire {
    pub fn effective(&self) -> Self {
        let mut effective = self.clone();
        effective.max_batch_size_kb = match self.max_batch_size_kb {
            16 | 32 | 64 => self.max_batch_size_kb,
            _ => 64,
        };
        effective
    }

    pub fn batch_size_fallback(&self) -> bool {
        self.max_batch_size_kb != 0 && !matches!(self.max_batch_size_kb, 16 | 32 | 64)
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioWire {
    pub packet_ms: u32,
    pub bitrate: u32,
    #[serde(skip_serializing_if = "is_stereo_layout")]
    pub layout: String,
    #[serde(skip_serializing_if = "is_stereo_channels")]
    pub channels: u32,
    #[serde(skip_serializing_if = "is_stereo_mask")]
    pub channel_mask: u32,
}
fn is_stereo_channels(channels: &u32) -> bool {
    *channels == 2
}
fn is_stereo_mask(mask: &u32) -> bool {
    *mask == 3
}
fn is_stereo_layout(layout: &String) -> bool {
    layout == "stereo"
}
impl Default for AudioWire {
    fn default() -> Self {
        Self {
            packet_ms: 10,
            bitrate: 128000,
            layout: "stereo".into(),
            channels: 2,
            channel_mask: 3,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoEncryptionPolicySnapshot {
    pub lan_mode: i64,
    pub wan_mode: i64,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayRequest {
    pub configuration: Value,
    pub expected_topology_revision: String,
    #[serde(default)]
    pub mode_policy: Option<Value>,
    #[serde(default)]
    pub mode_policies: Option<Vec<Value>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayTopology {
    displays: Vec<DisplayMode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayMode {
    id: String,
    width: u32,
    height: u32,
    refresh: DisplayRefresh,
    #[serde(rename = "x")]
    _x: i32,
    #[serde(rename = "y")]
    _y: i32,
    rotation: Option<String>,
    #[serde(rename = "hdr_enabled")]
    _hdr_enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayRefresh {
    numerator: u32,
    denominator: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayModePolicy {
    display_id: String,
    configuration_option: Option<String>,
    #[serde(rename = "remapping")]
    _remapping: DisplayRemapping,
    #[serde(rename = "context")]
    _context: DisplayModeContext,
    resolution_option: Option<String>,
    #[serde(rename = "manual_resolution")]
    _manual_resolution: Option<String>,
    refresh_rate_option: Option<String>,
    #[serde(rename = "manual_refresh_rate")]
    _manual_refresh_rate: Option<String>,
    hdr_option: Option<String>,
    #[serde(rename = "effective_client_hdr_request")]
    _effective_client_hdr_request: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayRemapping {
    #[serde(default)]
    #[serde(rename = "mixed")]
    _mixed: Vec<DisplayRemappingEntry>,
    #[serde(default)]
    #[serde(rename = "resolution_only")]
    _resolution_only: Vec<DisplayRemappingEntry>,
    #[serde(default)]
    #[serde(rename = "refresh_rate_only")]
    _refresh_rate_only: Vec<DisplayRemappingEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayRemappingEntry {
    #[serde(default)]
    #[serde(rename = "requested_resolution")]
    _requested_resolution: String,
    #[serde(default)]
    #[serde(rename = "requested_fps")]
    _requested_fps: String,
    #[serde(default)]
    #[serde(rename = "final_resolution")]
    _final_resolution: String,
    #[serde(default)]
    #[serde(rename = "final_refresh_rate")]
    _final_refresh_rate: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayModeContext {
    #[serde(rename = "resolution_is_automatic")]
    _resolution_is_automatic: bool,
    #[serde(rename = "refresh_rate_is_automatic")]
    _refresh_rate_is_automatic: bool,
    #[serde(rename = "client_display_mode_override")]
    _client_display_mode_override: bool,
    #[serde(rename = "optimize_game_settings")]
    _optimize_game_settings: bool,
}

impl DisplayRequest {
    fn validate(&self) -> Result<(), &'static str> {
        if self.expected_topology_revision.len() != 32
            || !self
                .expected_topology_revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("invalid_display_request");
        }
        let topology: DisplayTopology = serde_json::from_value(self.configuration.clone())
            .map_err(|_| "invalid_display_request")?;
        if topology.displays.is_empty() || topology.displays.len() > 16 {
            return Err("invalid_display_request");
        }
        let mut identities = Vec::with_capacity(topology.displays.len());
        for display in &topology.displays {
            if display.id.is_empty()
                || display.id.len() > 4096
                || display.width == 0
                || display.height == 0
                || display.refresh.numerator == 0
                || display.refresh.denominator == 0
                || display.rotation.as_deref().is_some_and(|rotation| {
                    !matches!(
                        rotation,
                        "identity" | "rotate90" | "rotate180" | "rotate270"
                    )
                })
                || identities
                    .iter()
                    .any(|prior: &&str| prior.eq_ignore_ascii_case(&display.id))
            {
                return Err("invalid_display_request");
            }
            identities.push(display.id.as_str());
        }
        let raw_policies: Vec<&Value> = match (&self.mode_policy, &self.mode_policies) {
            (Some(policy), None)
                if identities.len() == 1
                    || matches!(
                        policy["configuration_option"].as_str(),
                        Some("ensure_active" | "ensure_primary" | "ensure_only_display")
                    ) =>
            {
                vec![policy]
            }
            (None, Some(policies)) if policies.len() == identities.len() => {
                policies.iter().collect()
            }
            _ => return Err("invalid_display_request"),
        };
        let mut targets: Vec<String> = Vec::with_capacity(raw_policies.len());
        for raw_policy in raw_policies {
            let policy: DisplayModePolicy = serde_json::from_value(raw_policy.clone())
                .map_err(|_| "invalid_display_request")?;
            if !identities
                .iter()
                .any(|id| id.eq_ignore_ascii_case(&policy.display_id))
                || targets
                    .iter()
                    .any(|id| id.eq_ignore_ascii_case(&policy.display_id))
                || self.mode_policies.is_some() && raw_policy.get("configuration_option").is_some()
                || policy
                    .configuration_option
                    .as_deref()
                    .is_some_and(|option| {
                        !matches!(
                            option,
                            "disabled"
                                | "verify_only"
                                | "ensure_active"
                                | "ensure_primary"
                                | "ensure_only_display"
                        )
                    })
                || policy
                    .resolution_option
                    .as_deref()
                    .is_some_and(|option| !matches!(option, "auto" | "disabled" | "manual"))
                || policy.refresh_rate_option.as_deref().is_some_and(|option| {
                    !matches!(option, "auto" | "disabled" | "manual" | "prefer_highest")
                })
                || policy
                    .hdr_option
                    .as_deref()
                    .is_some_and(|option| !matches!(option, "auto" | "disabled"))
            {
                return Err("invalid_display_request");
            }
            targets.push(policy.display_id);
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareConfig {
    pub peer_ip: IpAddr,
    /// Coordinator-issued logical session selected by the transport adapter.
    #[serde(default)]
    pub lifecycle_session_target: Option<amaterasu_extension_api::LifecycleTarget>,
    /// Prelaunch History reservation, bound to the signed RTSP operation.
    #[serde(default)]
    pub history_activity_id: Option<String>,
    #[serde(default)]
    pub rtsp_operation_id: Option<String>,
    /// Complete, frozen display Apply request from the authenticated launch.
    #[serde(default)]
    pub display_request: Option<Box<DisplayRequest>>,
    /// Launch-attested display identity/specification for this stream.
    #[serde(default)]
    pub virtual_display_intent: Option<VirtualDisplayIntent>,
    /// Frozen launch policy; retained with the prepared operation for retries.
    #[serde(default)]
    pub frame_limiter_settings: Option<Value>,
    /// Exact executable selected by the authenticated Catalog launch preflight.
    #[serde(default)]
    pub frame_limiter_executable_path: Option<String>,
    #[serde(default)]
    pub video_encryption_policy: Option<VideoEncryptionPolicySnapshot>,
    #[serde(default)]
    pub video_port: Option<u16>,
    #[serde(default)]
    pub audio_port: Option<u16>,
    #[serde(default)]
    pub discovery: Option<PeerDiscovery>,
    #[serde(default)]
    pub video_bind: Option<SocketAddr>,
    #[serde(default)]
    pub audio_bind: Option<SocketAddr>,
    #[serde(default)]
    pub control_bind: Option<SocketAddr>,
    pub key_hex: String,
    pub key_id: u32,
    pub connect_data: u32,
    #[serde(default = "default_ping_timeout_ms")]
    pub ping_timeout_ms: u32,
    /// Time allowed for an authenticated launch to reach Start after display Apply.
    #[serde(default)]
    pub display_prepare_timeout_ms: Option<u32>,
    #[serde(default)]
    pub video: VideoWire,
    #[serde(default)]
    pub audio: AudioWire,
    #[serde(default = "yes")]
    pub stream_audio: bool,
    #[serde(default = "yes")]
    pub encrypt_video: bool,
    #[serde(default = "yes")]
    pub encrypt_audio: bool,
    #[serde(default)]
    pub permit_legacy_audio_iv_wrap: bool,
    pub operation_id: String,
}
impl PrepareConfig {
    pub fn validate(&self, allow_remote: bool) -> Result<(), &'static str> {
        if let Some(target) = &self.lifecycle_session_target {
            match target {
                amaterasu_extension_api::LifecycleTarget::Session {
                    host_epoch,
                    session_id,
                    generation,
                } if valid_lifecycle_id(host_epoch)
                    && valid_lifecycle_id(session_id)
                    && *generation > 0 => {}
                _ => return Err("invalid_lifecycle_identity"),
            }
            if self.display_request.is_some() || self.virtual_display_intent.is_some() {
                return Err("lifecycle_display_policy_conflict");
            }
        }
        match (&self.history_activity_id, &self.rtsp_operation_id) {
            (None, None) => {}
            (Some(activity), Some(operation)) if valid_name(activity) && activity == operation => {}
            _ => return Err("invalid_history_activity"),
        }
        if let Some(request) = &self.display_request {
            request.validate()?;
        }
        if self
            .virtual_display_intent
            .as_ref()
            .is_some_and(|intent| intent.validate().is_err())
        {
            return Err("invalid_virtual_display_intent");
        }
        self.validate_frame_limiter_snapshot()?;
        if self.ping_timeout_ms > i32::MAX as u32 {
            return Err("invalid_ping_timeout");
        }
        if let Some(timeout) = self.display_prepare_timeout_ms
            && (self.display_request.is_none()
                && !self
                    .virtual_display_intent
                    .as_ref()
                    .is_some_and(VirtualDisplayIntent::active)
                || !(1..=120_000).contains(&timeout))
        {
            return Err("invalid_display_prepare_timeout");
        }
        self.validate_destination(allow_remote)?;
        self.validate_bindings(allow_remote)?;
        self.validate_identity()?;
        self.audio.validate()?;
        self.video.validate()
    }

    fn validate_frame_limiter_snapshot(&self) -> Result<(), &'static str> {
        match (
            &self.frame_limiter_settings,
            &self.frame_limiter_executable_path,
        ) {
            (None, None) => Ok(()),
            (Some(snapshot), Some(path)) => {
                if path.trim().is_empty()
                    || path.len() > 4096
                    || path.chars().any(char::is_control)
                    || snapshot.get("version").and_then(Value::as_u64) != Some(1)
                    || !valid_frame_limiter_settings(snapshot)
                {
                    return Err("invalid_frame_limiter_snapshot");
                }
                Ok(())
            }
            _ => Err("invalid_frame_limiter_snapshot"),
        }
    }

    fn validate_destination(&self, allow_remote: bool) -> Result<(), &'static str> {
        if invalid_peer(self.peer_ip) {
            return Err("invalid_peer");
        }
        if !allow_remote && !self.peer_ip.is_loopback() {
            return Err("remote_disabled");
        }
        match &self.discovery {
            None if self.video_port.is_some_and(|port| port != 0)
                && self.audio_port.is_some_and(|port| port != 0) => {}
            Some(discovery)
                if self.video_port.is_none()
                    && self.audio_port.is_none()
                    && discovery.validate() => {}
            Some(_) => return Err("invalid_peer_discovery"),
            None => return Err("invalid_peer"),
        }
        Ok(())
    }

    fn validate_bindings(&self, allow_remote: bool) -> Result<(), &'static str> {
        for bind in [self.video_bind, self.audio_bind, self.control_bind]
            .into_iter()
            .flatten()
        {
            if bind.is_ipv4() != self.peer_ip.is_ipv4()
                || bind.ip().is_multicast()
                || bind.ip() == IpAddr::V4(Ipv4Addr::BROADCAST)
                || mapped(bind.ip())
                || (!allow_remote && !bind.ip().is_loopback())
            {
                return Err("invalid_bind");
            }
        }
        Ok(())
    }

    fn validate_identity(&self) -> Result<(), &'static str> {
        if !valid_name(&self.operation_id) {
            return Err("invalid_operation_id");
        }
        if self.key_hex.len() != 32 || !self.key_hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("invalid_key");
        }
        Ok(())
    }
}

fn valid_frame_limiter_settings(snapshot: &Value) -> bool {
    let Some(fields) = snapshot.as_object() else {
        return false;
    };
    let Some(settings) = fields.get("settings").and_then(Value::as_object) else {
        return false;
    };
    const SNAPSHOT_FIELDS: [&str; 9] = [
        "version",
        "settings",
        "requested_stream_fps_millihz",
        "client_display_mode_override_millihz",
        "lossless_scaling_limit_millihz",
        "virtual_display_active",
        "physical_framegen_capture",
        "game_frame_generation_mode",
        "rtss_frame_limit_type_explicit_override",
    ];
    if fields.len() != SNAPSHOT_FIELDS.len()
        || fields
            .keys()
            .any(|key| !SNAPSHOT_FIELDS.contains(&key.as_str()))
        || settings.len() != 11
    {
        return false;
    }
    let rate = |key: &str| {
        settings
            .get(key)
            .and_then(Value::as_u64)
            .is_some_and(|rate| rate <= 1_000_000)
    };
    let text = |key: &str| settings.get(key).and_then(Value::as_str);
    let boolean = |key: &str| settings.get(key).is_some_and(Value::is_boolean);
    if !rate("frame_limiter_fps_limit_millihz")
        || !boolean("frame_limiter_enable")
        || !boolean("frame_limiter_disable_vsync")
        || !boolean("rtss_allow_virtual_display_override")
        || !boolean("mangohud_always_show_graph")
        || !text("frame_limiter_provider").is_some_and(|value| {
            matches!(
                value,
                "auto"
                    | "rtss"
                    | "nvidia-control-panel"
                    | "mangohud"
                    | "proton"
                    | "mangohud-proton"
                    | "none"
            )
        })
        || !text("frame_limiter_auto_virtual_framegen")
            .is_some_and(|value| matches!(value, "enabled" | "vrr" | "legacy" | "disabled"))
        || !text("rtss_frame_limit_type").is_some_and(|value| {
            matches!(
                value,
                "async" | "front edge sync" | "back edge sync" | "nvidia reflex"
            )
        })
        || !text("mangohud_preset")
            .is_some_and(|value| matches!(value, "custom" | "1" | "2" | "3" | "4"))
        || !text("mangohud_limiter_method").is_some_and(|value| matches!(value, "early" | "late"))
        || !text("rtss_install_path")
            .is_some_and(|value| value.len() <= 4096 && !value.chars().any(char::is_control))
    {
        return false;
    }
    if !fields
        .get("requested_stream_fps_millihz")
        .and_then(Value::as_u64)
        .is_some_and(|rate| (1..=1_000_000).contains(&rate))
    {
        return false;
    }
    for key in [
        "client_display_mode_override_millihz",
        "lossless_scaling_limit_millihz",
    ] {
        if !fields.get(key).is_some_and(|value| {
            value.is_null() || value.as_u64().is_some_and(|rate| rate <= 1_000_000)
        }) {
            return false;
        }
    }
    for key in [
        "virtual_display_active",
        "physical_framegen_capture",
        "rtss_frame_limit_type_explicit_override",
    ] {
        if !fields
            .get(key)
            .is_some_and(|value| value.is_null() || value.is_boolean())
        {
            return false;
        }
    }
    fields
        .get("game_frame_generation_mode")
        .is_some_and(|value| {
            value.is_null()
                || value.as_str().is_some_and(|mode| {
                    matches!(
                        mode,
                        "none"
                            | "lossless-scaling"
                            | "game-provided"
                            | "nvidia-smooth-motion"
                            | "other"
                    )
                })
        })
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerDiscovery {
    pub video_ping_payload: String,
    pub audio_ping_payload: String,
}

impl PeerDiscovery {
    fn validate(&self) -> bool {
        self.video_ping_payload != self.audio_ping_payload
            && valid_ping_payload(&self.video_ping_payload)
            && valid_ping_payload(&self.audio_ping_payload)
    }
}

fn valid_ping_payload(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

impl AudioWire {
    pub fn channel_mapping(&self) -> Option<(u32, u32, &'static [u8], &'static [u8])> {
        match self.channels {
            6 => Some((4, 2, &[0, 4, 1, 2, 3, 5], &[0, 1, 4, 5, 2, 3])),
            8 => Some((5, 3, &[0, 6, 1, 2, 3, 4, 5, 7], &[0, 1, 6, 7, 4, 5, 2, 3])),
            _ => None,
        }
    }
    fn validate(&self) -> Result<(), &'static str> {
        let bitrate_range = match (self.channels, self.packet_ms, self.channel_mask) {
            (2, 5 | 10, 3) if self.layout == "stereo" => 6000..=512000,
            (6, 5 | 10, 0x3f) if self.layout == "5.1" => 256000..=1536000,
            (8, 5, 0x63f) if self.layout == "7.1" => 512000..=2048000,
            (8, 10, 0x63f) if self.layout == "7.1" => 450000..=2048000,
            _ => return Err("invalid_audio_configuration"),
        };
        let packet_bytes = (u64::from(self.bitrate) * u64::from(self.packet_ms)).div_ceil(8000);
        if !bitrate_range.contains(&self.bitrate) || !(1..=1275).contains(&packet_bytes) {
            return Err("invalid_audio_configuration");
        }
        Ok(())
    }
}

impl VideoWire {
    /// The codecs authenticated start may select for this prepared transport.
    pub fn offered_codecs(&self) -> &[amaterasu_media::VideoCodec] {
        if self.codecs.is_empty() {
            std::slice::from_ref(&self.codec)
        } else {
            &self.codecs
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        let unique_codecs = self
            .codecs
            .iter()
            .enumerate()
            .all(|(index, codec)| !self.codecs[..index].contains(codec));
        if self.max_batch_size_kb > 64
            || (!self.codecs.is_empty() && (!self.codec.is_default() || !unique_codecs))
        {
            return Err("invalid_video_configuration");
        }
        // Framing and FEC limits do not depend on the codec.
        let codec = match self.offered_codecs()[0] {
            amaterasu_media::VideoCodec::H264 => amaterasu_gamestream::video::VideoCodec::H264,
            amaterasu_media::VideoCodec::Hevc => amaterasu_gamestream::video::VideoCodec::Hevc,
            amaterasu_media::VideoCodec::Av1 => amaterasu_gamestream::video::VideoCodec::Av1,
        };
        amaterasu_gamestream::video::VideoPacketizer::new_with_codec(
            amaterasu_gamestream::video::VideoConfig {
                packet_size: self.packet_size,
                fec_percentage: self.fec_percentage,
                min_parity_shards: self.min_parity_shards,
                max_fec_blocks: self.max_fec_blocks,
                encrypted: false,
            },
            codec,
        )
        .map_err(|_| "invalid_video_configuration")?;
        Ok(())
    }
}

fn invalid_peer(address: IpAddr) -> bool {
    address.is_unspecified()
        || address.is_multicast()
        || address == IpAddr::V4(Ipv4Addr::BROADCAST)
        || mapped(address)
}

#[derive(Clone, Default, PartialEq, Eq)]
pub enum CaptureProvider {
    #[default]
    Dxgi,
    Wgc,
    PipeWire,
    X11,
    Kms,
    /// A configured external capture alias, resolved by the video service.
    External(String),
}

impl Serialize for CaptureProvider {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let name = match self {
            Self::Dxgi => "dxgi",
            Self::Wgc => "wgc",
            Self::PipeWire => "pipewire",
            Self::X11 => "x11",
            Self::Kms => "kms",
            Self::External(name) => name,
        };
        serializer.serialize_str(name)
    }
}

impl<'de> Deserialize<'de> for CaptureProvider {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        match name.as_str() {
            "dxgi" => Ok(Self::Dxgi),
            "wgc" => Ok(Self::Wgc),
            "pipewire" => Ok(Self::PipeWire),
            "x11" => Ok(Self::X11),
            "kms" => Ok(Self::Kms),
            _ if !name.is_empty()
                && name.len() <= 64
                && name.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'_'
                        || byte == b'-'
                }) =>
            {
                Ok(Self::External(name))
            }
            _ => Err(serde::de::Error::custom("invalid capture provider alias")),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoConfiguration {
    /// Codec and range accepted by the authenticated RTSP ANNOUNCE.
    #[serde(
        default = "default_video_stream_format",
        skip_serializing_if = "is_default_video_stream_format"
    )]
    pub video_stream_format: amaterasu_module_sdk::VideoStreamFormat,
    /// Client-requested H.264 slices per frame from Moonlight's ANNOUNCE.
    #[serde(
        default = "default_slices_per_frame",
        skip_serializing_if = "is_default_slices_per_frame"
    )]
    pub slices_per_frame: i32,
    /// Minimum software slice count requested by the host settings.
    #[serde(
        default = "default_min_threads",
        skip_serializing_if = "is_default_min_threads"
    )]
    pub min_threads: i32,
    /// QP remains metadata until a CQP encoder consumes it.
    #[serde(default = "default_qp", skip_serializing_if = "is_default_qp")]
    pub qp: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sw_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sw_tune: Option<String>,
    /// Encoder adapter filters resolved from the host/application/client
    /// settings snapshot. An absent value leaves adapter selection automatic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_pnp_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "amaterasu_media::VideoCodec::is_default"
    )]
    pub codec: amaterasu_media::VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    /// Zero leaves H.264 reference-frame selection to the encoder; a positive
    /// value requests the exact maximum in the stream's H.264 SPS.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_reference_frames: u32,
    /// Frozen host hardware-encoder controls resolved before stream activation.
    #[serde(default, skip_serializing_if = "is_default_nvenc_settings")]
    pub nvenc_settings: amaterasu_module_sdk::NvencSettings,
    #[serde(default)]
    pub nvenc_spatial_aq: bool,
    /// Negotiated SDP color matrix and range, carried unchanged to the encoder.
    #[serde(default)]
    pub color_mode: amaterasu_media::ColorMode,
}

fn default_video_stream_format() -> amaterasu_module_sdk::VideoStreamFormat {
    amaterasu_module_sdk::VideoStreamFormat::H264_SDR
}

fn is_default_video_stream_format(format: &amaterasu_module_sdk::VideoStreamFormat) -> bool {
    *format == amaterasu_module_sdk::VideoStreamFormat::H264_SDR
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

fn is_default_nvenc_settings(settings: &amaterasu_module_sdk::NvencSettings) -> bool {
    settings == &amaterasu_module_sdk::NvencSettings::default()
}

fn default_slices_per_frame() -> i32 {
    1
}

fn is_default_slices_per_frame(value: &i32) -> bool {
    *value == default_slices_per_frame()
}

fn default_min_threads() -> i32 {
    2
}

fn is_default_min_threads(value: &i32) -> bool {
    *value == default_min_threads()
}

fn default_qp() -> i32 {
    28
}

fn is_default_qp(value: &i32) -> bool {
    *value == default_qp()
}
#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartConfigData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_target: Option<amaterasu_extension_api::LifecycleTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_candidate: Option<amaterasu_extension_api::CaptureBinding>,
    #[serde(default)]
    pub capture_provider: CaptureProvider,
    #[serde(default)]
    pub constant_rate: bool,
    #[serde(default = "default_minimum_fps_target")]
    pub minimum_fps_target: f64,
    #[serde(default = "default_wgc_pacing_smoothing")]
    pub wgc_pacing_smoothing: bool,
    #[serde(default)]
    pub capture_sink_without_routing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back_button_timeout: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamepad: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ds4_back_as_touchpad_click: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_as_ds4: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touchpad_as_ds4: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ds5_inputtino_randomize_mac: Option<bool>,
    /// Frozen agreement supplied by the trusted protocol owner; absent denies contributions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbound_agreement: Option<amaterasu_module_sdk::input::outbound::OutboundAgreement>,
    /// Immutable Moonlight feature flags retained from the accepted RTSP ANNOUNCE.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_feature_flags: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuous_audio: Option<bool>,
    /// Optional PLAY snapshot; when present it must match the immutable
    /// encryption policy accepted during prepare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypt_video: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypt_audio: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_pen_touch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high_resolution_scrolling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always_send_scancodes: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_rightalt_to_key_win: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keybindings: Option<amaterasu_input_policy::KeyBindings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_repeat_delay: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_repeat_frequency: Option<f64>,
    /// Moonlight's device name for the session; independent from capture output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virtual_sink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    #[serde(default)]
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder_options: Option<serde_json::Value>,
    pub encoder: EncoderSelection,
    pub configuration: VideoConfiguration,
    pub audio_endpoint: String,
    #[serde(default)]
    pub host_audio_enabled: bool,
}

#[derive(Clone, PartialEq)]
pub struct StartConfig(StartConfigData);

impl Serialize for StartConfig {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = serde_json::to_value(&self.0).map_err(serde::ser::Error::custom)?;
        if self.source_id.is_some() {
            let object = value
                .as_object_mut()
                .expect("start configuration is an object");
            for field in [
                "capture_provider",
                "output",
                "constant_rate",
                "minimum_fps_target",
                "wgc_pacing_smoothing",
            ] {
                object.remove(field);
            }
        }
        value.serialize(serializer)
    }
}

impl std::ops::Deref for StartConfig {
    type Target = StartConfigData;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for StartConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl<'de> Deserialize<'de> for StartConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("source_id").is_some_and(|id| !id.is_null()) {
            for field in [
                "capture_provider",
                "output",
                "constant_rate",
                "minimum_fps_target",
                "wgc_pacing_smoothing",
            ] {
                if value.get(field).is_some() {
                    return Err(serde::de::Error::custom(
                        "source mode cannot specify capture options",
                    ));
                }
            }
        }
        serde_json::from_value(value)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

// Reject types owned by the existing control protocol in either direction.
// Further extension mappings are selected by the trusted protocol owner.
fn reserved_control_type(kind: u16) -> bool {
    matches!(
        kind,
        0 | 0x0001
            | 0x0109
            | 0x010b
            | 0x010e
            | 0x0200
            | 0x0201
            | 0x0204
            | 0x0206
            | 0x0301
            | 0x0302
            | 0x0305
            | 0x0307
            | 0x0350
            | 0x5500..=0x5503 | 0x5601
    )
}

impl StartConfig {
    pub fn matches_prepared_codec(&self, prepared: &PrepareConfig) -> bool {
        prepared
            .video
            .offered_codecs()
            .contains(&self.configuration.codec)
    }

    pub fn matches_prepared_frame_limiter(&self, prepared: &PrepareConfig) -> bool {
        prepared
            .frame_limiter_settings
            .as_ref()
            .is_none_or(|snapshot| {
                self.configuration
                    .fps
                    .checked_mul(1_000)
                    .is_some_and(|fps| {
                        snapshot["requested_stream_fps_millihz"].as_u64() == Some(u64::from(fps))
                    })
            })
    }

    pub fn matches_prepared_audio(&self, prepared: &PrepareConfig) -> bool {
        !prepared.stream_audio
            || prepared.audio.channels == 2 && self.channels.is_none()
            || self.channels == Some(prepared.audio.channels)
    }
    pub fn matches_prepared_encryption(&self, prepared: &PrepareConfig) -> bool {
        self.encrypt_video
            .is_none_or(|encrypt_video| encrypt_video == prepared.encrypt_video)
            && self
                .encrypt_audio
                .is_none_or(|encrypt_audio| encrypt_audio == prepared.encrypt_audio)
    }

    pub fn validate(&self, stream_audio: bool) -> Result<(), &'static str> {
        self.encoder.validate()?;
        if self.encoder_provider.as_deref().is_some_and(|alias| {
            alias.is_empty()
                || alias.len() > 64
                || !alias.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
        }) {
            return Err("invalid_encoder_provider");
        }
        if self.encoder_options.as_ref().is_some_and(|options| {
            self.encoder_provider.is_none()
                || !options.is_object()
                || serde_json::to_vec(options).map_or(true, |bytes| bytes.len() > 64 * 1024)
        }) {
            return Err("invalid_encoder_options");
        }
        if let Some(source_id) = self.source_id {
            if source_id == 0 || self.encoder_provider.is_none() {
                return Err("invalid_encoder_source");
            }
            if !matches!(self.encoder, EncoderSelection::Identity(_)) {
                return Err("source_encoder_identity_required");
            }
        }
        if self.continuous_audio == Some(true) && !stream_audio {
            return Err("continuous_audio_requires_stream_audio");
        }
        if self.outbound_agreement.as_ref().is_some_and(|agreement| {
            agreement.validate().is_err()
                || agreement
                    .schemas
                    .iter()
                    .any(|schema| reserved_control_type(schema.message_type))
        }) {
            return Err("invalid_outbound_agreement");
        }
        if self.client_feature_flags.is_some_and(|flags| {
            flags & !amaterasu_gamestream::sdp::KNOWN_CLIENT_FEATURE_FLAGS != 0
        }) {
            return Err("invalid_start_configuration");
        }
        if !valid_minimum_fps_target(self.minimum_fps_target) {
            return Err("invalid_minimum_fps_target");
        }
        if self
            .device_name
            .as_deref()
            .is_some_and(|name| !valid_name(name))
        {
            return Err("invalid_start_configuration");
        }
        if self
            .gamepad
            .as_deref()
            .is_some_and(|profile| !valid_gamepad_profile(profile))
        {
            return Err("invalid_start_configuration");
        }
        if self
            .key_repeat_delay
            .is_some_and(|delay| delay > i32::MAX as u32)
            || self
                .key_repeat_frequency
                .is_some_and(|frequency| !frequency.is_finite() || frequency < 0.0)
        {
            return Err("invalid_start_configuration");
        }
        if self.output == "amaterasu-owned-display"
            && !matches!(
                self.capture_provider,
                CaptureProvider::PipeWire | CaptureProvider::External(_)
            )
        {
            return Err("owned_display_requires_pipewire");
        }
        if self.constant_rate && self.capture_provider != CaptureProvider::Wgc {
            return Err("constant_rate_requires_wgc");
        }
        if (self.source_id.is_none() && !valid_label(&self.output, 512))
            || (!self.audio_endpoint.is_empty() && !valid_label(&self.audio_endpoint, 4096))
            || !self.configuration.valid()
        {
            return Err("invalid_start_configuration");
        }
        match (&self.virtual_sink, self.channels) {
            (None, None) => {
                if stream_audio && !valid_label(&self.audio_endpoint, 4096) {
                    return Err("invalid_start_configuration");
                }
            }
            (None, Some(_)) => return Err("invalid_virtual_sink_channels"),
            (Some(selector), Some(channels))
                if selector.len() <= 4096
                    && !selector.chars().any(char::is_control)
                    && matches!(channels, 2 | 6 | 8)
                    && self.audio_endpoint.is_empty()
                    && !self.capture_sink_without_routing => {}
            (Some(_), _) => return Err("invalid_virtual_sink_channels"),
        }
        Ok(())
    }
}

fn valid_gamepad_profile(profile: &str) -> bool {
    #[cfg(windows)]
    {
        matches!(
            profile,
            "auto"
                | "x360"
                | "vhf"
                | "vhf_xbox"
                | "vhf_xbox_one"
                | "vhf_ds4"
                | "vhf_ds5"
                | "vhf_switch"
                | "ds4"
        )
    }
    #[cfg(target_os = "linux")]
    {
        matches!(profile, "auto" | "xone" | "ds4" | "ds5" | "switch")
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        profile == "auto"
    }
}

fn default_minimum_fps_target() -> f64 {
    20.0
}

fn default_wgc_pacing_smoothing() -> bool {
    true
}

fn valid_minimum_fps_target(value: f64) -> bool {
    value.is_finite() && (0.0..=1000.0).contains(&value)
}

impl VideoConfiguration {
    pub fn normalize_codec(&mut self) -> Result<(), &'static str> {
        let negotiated = match self.video_stream_format.codec {
            amaterasu_module_sdk::VideoCodec::H264 => amaterasu_media::VideoCodec::H264,
            amaterasu_module_sdk::VideoCodec::Hevc => amaterasu_media::VideoCodec::Hevc,
            amaterasu_module_sdk::VideoCodec::Av1 => amaterasu_media::VideoCodec::Av1,
        };
        if self.video_stream_format != default_video_stream_format() {
            if self.codec != amaterasu_media::VideoCodec::H264 && self.codec != negotiated {
                return Err("conflicting_video_codec");
            }
            self.codec = negotiated;
        } else {
            self.video_stream_format.codec = match self.codec {
                amaterasu_media::VideoCodec::H264 => amaterasu_module_sdk::VideoCodec::H264,
                amaterasu_media::VideoCodec::Hevc => amaterasu_module_sdk::VideoCodec::Hevc,
                amaterasu_media::VideoCodec::Av1 => amaterasu_module_sdk::VideoCodec::Av1,
            };
        }
        Ok(())
    }

    fn valid(&self) -> bool {
        self.video_stream_format.validate().is_ok()
            && ((self.video_stream_format.bit_depth == 8
                && self.video_stream_format.dynamic_range
                    == amaterasu_module_sdk::DynamicRange::Sdr)
                || (cfg!(target_os = "linux")
                    && self.video_stream_format.bit_depth == 10
                    && self.video_stream_format.dynamic_range
                        == amaterasu_module_sdk::DynamicRange::Hdr))
            && (16..=3840).contains(&self.width)
            && (16..=2160).contains(&self.height)
            && self.width.is_multiple_of(2)
            && self.height.is_multiple_of(2)
            && (1..=120).contains(&self.fps)
            && (100_000..=100_000_000).contains(&self.bitrate)
            && self.slices_per_frame >= 1
            && self.nvenc_settings.validate().is_ok()
            && self.max_reference_frames <= 16
            && amaterasu_module_sdk::validate_software_options(
                self.sw_preset.as_deref(),
                self.sw_tune.as_deref(),
            )
            .is_ok()
            && self
                .adapter_name
                .as_deref()
                .is_none_or(|name| name.len() <= 1024 && !name.chars().any(char::is_control))
            && self
                .adapter_pnp_id
                .as_deref()
                .is_none_or(|id| id.len() <= 256 && !id.chars().any(char::is_control))
            && self.adapter_pnp_id.as_deref().is_none_or(|id| {
                id.is_empty()
                    || self
                        .adapter_name
                        .as_deref()
                        .is_some_and(|name| !name.is_empty())
            })
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    ReserveHistoryActivity {
        activity_id: String,
        rtsp_operation_id: String,
    },
    ReleaseHistoryActivity {
        activity_id: String,
        rtsp_operation_id: String,
    },
    Prepare(Box<PrepareConfig>),
    Start {
        session: u64,
        configuration: StartConfig,
    },
    StartWhenAuthenticated {
        session: u64,
        configuration: StartConfig,
    },
    Stop {
        session: u64,
    },
    Close {
        session: u64,
    },
    Status {
        session: Option<u64>,
    },
    ManagementLiveSessions {},
    Lookup {
        operation_id: String,
    },
}

fn yes() -> bool {
    true
}
fn default_ping_timeout_ms() -> u32 {
    10_000
}
pub fn valid_name(value: &str) -> bool {
    valid_label(value, 128)
}
fn valid_lifecycle_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}
fn valid_label(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}
fn mapped(value: IpAddr) -> bool {
    matches!(value, IpAddr::V6(value) if value.to_ipv4_mapped().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn prepare_json() -> Value {
        json!({"peer_ip":"127.0.0.1","video_port":49000,"audio_port":49001,
            "key_hex":"0123456789abcdef0123456789abcdef","key_id":1,
            "connect_data":2,"operation_id":"prepare-one"})
    }

    fn prepare() -> PrepareConfig {
        serde_json::from_value(prepare_json()).unwrap()
    }

    #[test]
    fn lifecycle_session_target_is_a_bounded_session_identity() {
        let mut request = prepare_json();
        request["lifecycle_session_target"] = json!({"kind":"session", "host_epoch":"epoch", "session_id":"session:1", "generation":1});
        assert!(
            serde_json::from_value::<PrepareConfig>(request.clone())
                .unwrap()
                .validate(false)
                .is_ok()
        );
        for invalid in ["", "a b", "☃", &"x".repeat(129)] {
            request["lifecycle_session_target"]["session_id"] = json!(invalid);
            assert_eq!(
                serde_json::from_value::<PrepareConfig>(request.clone())
                    .unwrap()
                    .validate(false),
                Err("invalid_lifecycle_identity")
            );
        }
    }

    fn start_json() -> Value {
        json!({"output":"display-1","encoder":0,"audio_endpoint":"default",
            "configuration":{"width":1920,"height":1080,"fps":60,"bitrate":5000000}})
    }

    #[test]
    fn external_capture_alias_is_bounded_and_forwarded() {
        let mut value = start_json();
        value["capture_provider"] = json!("acme_capture-1");
        let start: StartConfig = serde_json::from_value(value).unwrap();
        assert!(start.validate(true).is_ok());
        assert_eq!(
            serde_json::to_value(start).unwrap()["capture_provider"],
            "acme_capture-1"
        );
        for alias in ["", "acme.capture", "Acme", "acme/service", &"a".repeat(65)] {
            assert!(serde_json::from_value::<CaptureProvider>(json!(alias)).is_err());
        }
    }

    #[test]
    fn owned_output_accepts_pipewire_and_external_aliases_only() {
        for (provider, expected) in [
            ("pipewire", Ok(())),
            ("vkms", Ok(())),
            ("dxgi", Err("owned_display_requires_pipewire")),
            ("wgc", Err("owned_display_requires_pipewire")),
            ("kms", Err("owned_display_requires_pipewire")),
            ("x11", Err("owned_display_requires_pipewire")),
        ] {
            let mut value = start_json();
            value["capture_provider"] = json!(provider);
            value["output"] = json!("amaterasu-owned-display");
            let start: StartConfig = serde_json::from_value(value).unwrap();
            assert_eq!(start.validate(true), expected, "provider {provider}");
        }
    }

    #[test]
    fn selected_pipewire_source_is_valid_without_windows_capture_flags() {
        let mut request = start_json();
        request["capture_provider"] = json!("pipewire");
        request["output"] = json!("portal-selected-session-source");
        let mut start: StartConfig = serde_json::from_value(request).unwrap();
        assert!(start.validate(false).is_ok());
        assert_eq!(
            serde_json::to_value(&start).unwrap()["capture_provider"],
            "pipewire"
        );
        start.constant_rate = true;
        assert_eq!(start.validate(false), Err("constant_rate_requires_wgc"));
    }

    #[test]
    fn selected_x11_output_is_valid_without_windows_capture_flags() {
        let mut request = start_json();
        request["capture_provider"] = json!("x11");
        request["output"] = json!("x11:DP-1/000000a4");
        let mut start: StartConfig = serde_json::from_value(request).unwrap();
        assert!(start.validate(false).is_ok());
        assert_eq!(
            serde_json::to_value(&start).unwrap()["capture_provider"],
            "x11"
        );
        start.constant_rate = true;
        assert_eq!(start.validate(false), Err("constant_rate_requires_wgc"));
    }

    #[test]
    fn selected_kms_output_is_valid_without_windows_capture_flags() {
        let mut request = start_json();
        request["capture_provider"] = json!("kms");
        request["output"] = json!("kms:card1/conn=34/crtc=56/plane=78");
        let mut start: StartConfig = serde_json::from_value(request).unwrap();
        assert!(start.validate(false).is_ok());
        assert_eq!(
            serde_json::to_value(&start).unwrap()["capture_provider"],
            "kms"
        );
        start.constant_rate = true;
        assert_eq!(start.validate(false), Err("constant_rate_requires_wgc"));
    }

    #[test]
    fn prepared_codec_offer_admits_each_offered_start_codec_only() {
        let mut config = prepare_json();
        config["video"] = json!({"codecs":["h264","hevc","av1"]});
        let prepared: PrepareConfig = serde_json::from_value(config).unwrap();
        assert!(prepared.validate(false).is_ok());
        for (codec, expected) in [("h264", true), ("hevc", true), ("av1", true)] {
            let mut start = start_json();
            start["configuration"]["codec"] = json!(codec);
            let start: StartConfig = serde_json::from_value(start).unwrap();
            assert_eq!(start.matches_prepared_codec(&prepared), expected, "{codec}");
        }

        let mut config = prepare_json();
        config["video"] = json!({"codecs":["h264","av1"]});
        let prepared: PrepareConfig = serde_json::from_value(config).unwrap();
        let mut start = start_json();
        start["configuration"]["codec"] = json!("hevc");
        let start: StartConfig = serde_json::from_value(start).unwrap();
        assert!(!start.matches_prepared_codec(&prepared));

        // The single-codec form remains exact.
        let mut config = prepare_json();
        config["video"] = json!({"codec":"av1"});
        let prepared: PrepareConfig = serde_json::from_value(config).unwrap();
        assert!(prepared.validate(false).is_ok());
        let start: StartConfig = serde_json::from_value(start_json()).unwrap();
        assert!(!start.matches_prepared_codec(&prepared));

        for invalid in [
            json!({"codecs":["av1","av1"]}),
            json!({"codec":"hevc","codecs":["hevc"]}),
        ] {
            let mut config = prepare_json();
            config["video"] = invalid;
            let prepared: PrepareConfig = serde_json::from_value(config).unwrap();
            assert_eq!(prepared.validate(false), Err("invalid_video_configuration"));
        }
    }

    #[test]
    fn defaults_are_disabled_and_default_media_configuration_is_accepted() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(
            !settings.enabled
                && !settings.allow_remote
                && !settings.enable_input
                && settings.input_lease_ttl_ms == 30000
                && settings.allowed_principals.is_empty()
        );
        assert!(settings.validate().is_ok());
        let config = prepare();
        assert_eq!(config.ping_timeout_ms, 10_000);
        assert!(config.validate(false).is_ok());
        assert!(
            config.encrypt_audio && config.encrypt_video && !config.permit_legacy_audio_iv_wrap
        );
        assert_eq!((config.audio.packet_ms, config.audio.bitrate), (10, 128000));
        let start: StartConfig = serde_json::from_value(start_json()).unwrap();
        assert!(start.validate(true).is_ok());
        assert!(!start.capture_sink_without_routing);
        assert!(!start.host_audio_enabled);
        let mut capture_only = start_json();
        capture_only["capture_sink_without_routing"] = json!(true);
        let capture_only: StartConfig = serde_json::from_value(capture_only).unwrap();
        assert!(capture_only.capture_sink_without_routing);
        let without_audio: StartConfig = serde_json::from_value(json!({
            "output":"display-1","encoder":0,"audio_endpoint":"",
            "configuration":{"width":1920,"height":1080,"fps":60,"bitrate":5000000}
        }))
        .unwrap();
        assert!(without_audio.validate(false).is_ok());
        assert!(without_audio.validate(true).is_err());
        assert!(matches!(start.capture_provider, CaptureProvider::Dxgi));
        assert_eq!(start.minimum_fps_target, 20.0);
        assert!(start.wgc_pacing_smoothing);
        assert_eq!(start.configuration.max_reference_frames, 0);
        assert!(
            serde_json::to_value(&start.configuration).unwrap()["max_reference_frames"].is_null()
        );
        let mut strict = start.configuration.clone();
        strict.max_reference_frames = 2;
        assert!(strict.valid());
        assert_eq!(
            serde_json::to_value(&strict).unwrap()["max_reference_frames"],
            2
        );
        strict.max_reference_frames = 16;
        assert!(strict.valid());
        strict.max_reference_frames = 17;
        assert!(!strict.valid());
    }

    #[test]
    fn pipewire_capture_provider_and_portal_sentinel_are_accepted_and_preserved() {
        let mut value = start_json();
        value["capture_provider"] = json!("pipewire");
        value["output"] = json!("portal-selected-session-source");
        let start: StartConfig = serde_json::from_value(value).unwrap();

        assert!(start.validate(true).is_ok());
        assert!(matches!(start.capture_provider, CaptureProvider::PipeWire));
        let serialized = serde_json::to_value(&start).unwrap();
        assert_eq!(serialized["capture_provider"], "pipewire");
        assert_eq!(serialized["output"], "portal-selected-session-source");
    }

    #[test]
    fn encoder_index_zero_and_auto_alias_round_trip_without_collapsing() {
        let index_zero: StartConfig = serde_json::from_value(start_json()).unwrap();
        assert_eq!(index_zero.encoder, EncoderSelection::Index(0));
        assert_eq!(serde_json::to_value(&index_zero).unwrap()["encoder"], 0);

        let mut automatic = start_json();
        automatic["encoder"] = json!("auto");
        let automatic: StartConfig = serde_json::from_value(automatic).unwrap();
        assert_eq!(
            automatic.encoder,
            EncoderSelection::Backend(EncoderBackend::Auto)
        );
        assert_eq!(serde_json::to_value(&automatic).unwrap()["encoder"], "auto");

        let mut explicit = start_json();
        explicit["encoder"] = json!(7);
        let explicit: StartConfig = serde_json::from_value(explicit).unwrap();
        assert_eq!(explicit.encoder, EncoderSelection::Index(7));
        assert_eq!(serde_json::to_value(&explicit).unwrap()["encoder"], 7);
    }

    #[test]
    fn host_audio_policy_defaults_off_and_accepts_an_explicit_snapshot() {
        let mut value = start_json();
        value["host_audio_enabled"] = json!(true);
        let start: StartConfig = serde_json::from_value(value).unwrap();

        assert!(start.host_audio_enabled);
        assert!(start.validate(true).is_ok());
        assert_eq!(
            serde_json::to_value(&start).unwrap()["host_audio_enabled"],
            true
        );
    }

    #[test]
    fn virtual_sink_start_requires_a_valid_selector_channel_pair() {
        let mut value = start_json();
        value["audio_endpoint"] = json!("");
        value["virtual_sink"] = json!("Steam Streaming Speakers");
        value["channels"] = json!(2);
        let stereo: StartConfig = serde_json::from_value(value.clone()).unwrap();
        assert!(stereo.validate(true).is_ok());

        value["channels"] = json!(4);
        let invalid_channels: StartConfig = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            invalid_channels.validate(true),
            Err("invalid_virtual_sink_channels")
        );

        for channels in [6, 8] {
            value["channels"] = json!(channels);
            let surround: StartConfig = serde_json::from_value(value.clone()).unwrap();
            assert!(surround.validate(true).is_ok());
        }

        value["channels"] = json!(2);
        value["capture_sink_without_routing"] = json!(true);
        let capture_only: StartConfig = serde_json::from_value(value).unwrap();
        assert_eq!(
            capture_only.validate(true),
            Err("invalid_virtual_sink_channels")
        );

        let mut regular = start_json();
        regular["channels"] = json!(2);
        let regular: StartConfig = serde_json::from_value(regular).unwrap();
        assert_eq!(regular.validate(true), Err("invalid_virtual_sink_channels"));
    }

    #[test]
    fn ping_timeout_accepts_zero_and_reference_maximum_but_rejects_overflow() {
        for selected in [0, 250, i32::MAX as u32] {
            let mut value = prepare_json();
            value["ping_timeout_ms"] = json!(selected);
            let config: PrepareConfig = serde_json::from_value(value).unwrap();
            assert_eq!(config.ping_timeout_ms, selected);
            assert!(config.validate(false).is_ok());
        }
        let mut value = prepare_json();
        value["ping_timeout_ms"] = json!(i32::MAX as u64 + 1);
        let config: PrepareConfig = serde_json::from_value(value).unwrap();
        assert_eq!(config.validate(false), Err("invalid_ping_timeout"));
    }

    #[test]
    fn minimum_fps_target_defaults_and_validates_as_a_finite_bounded_number() {
        let defaulted: StartConfig = serde_json::from_value(start_json()).unwrap();
        assert_eq!(defaulted.minimum_fps_target, 20.0);
        assert!(defaulted.validate(true).is_ok());

        let mut selected = start_json();
        selected["minimum_fps_target"] = json!(23.5);
        let selected: StartConfig = serde_json::from_value(selected).unwrap();
        assert_eq!(selected.minimum_fps_target, 23.5);
        assert!(selected.validate(true).is_ok());

        for invalid in [-0.1, 1000.1] {
            let mut request = start_json();
            request["minimum_fps_target"] = json!(invalid);
            let parsed: StartConfig = serde_json::from_value(request).unwrap();
            assert_eq!(parsed.validate(true), Err("invalid_minimum_fps_target"));
        }
    }

    #[test]
    fn outbound_agreement_is_optional_bounded_and_rejects_reserved_types() {
        let absent: StartConfig = serde_json::from_value(start_json()).unwrap();
        assert!(absent.outbound_agreement.is_none());
        let mut value = start_json();
        value["outbound_agreement"] = json!({"version":1,"schemas":[
            {"name":"org.example.alpha","version":1,"message_type":24577},
            {"name":"org.example.beta","version":2,"message_type":24578}
        ]});
        let accepted: StartConfig = serde_json::from_value(value.clone()).unwrap();
        assert!(accepted.validate(true).is_ok());
        for reserved in [
            0x0001, 0x0109, 0x010b, 0x010e, 0x0206, 0x0307, 0x5500, 0x5503, 0x5601,
        ] {
            value["outbound_agreement"]["schemas"][0]["message_type"] = json!(reserved);
            let rejected: StartConfig = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(rejected.validate(true), Err("invalid_outbound_agreement"));
        }
    }

    #[test]
    fn client_feature_flags_are_optional_frozen_known_bits() {
        let absent: StartConfig = serde_json::from_value(start_json()).unwrap();
        assert_eq!(absent.client_feature_flags, None);
        assert!(absent.validate(true).is_ok());
        for flags in [0, 3, 7] {
            let mut value = start_json();
            value["client_feature_flags"] = json!(flags);
            let start: StartConfig = serde_json::from_value(value).unwrap();
            assert_eq!(start.client_feature_flags, Some(flags));
            assert!(start.validate(true).is_ok());
        }
        let mut unknown = start_json();
        unknown["client_feature_flags"] = json!(8);
        let start: StartConfig = serde_json::from_value(unknown).unwrap();
        assert_eq!(start.validate(true), Err("invalid_start_configuration"));
    }

    #[test]
    fn negotiated_video_format_accepts_hdr10_but_rejects_invalid_depths() {
        let legacy: StartConfig = serde_json::from_value(start_json()).unwrap();
        assert_eq!(
            legacy.configuration.video_stream_format,
            amaterasu_module_sdk::VideoStreamFormat::H264_SDR
        );
        assert!(
            serde_json::to_value(&legacy).unwrap()["configuration"]
                .get("video_stream_format")
                .is_none()
        );
        assert!(legacy.validate(true).is_ok());

        for codec in ["h264", "hevc", "av1"] {
            for (bits, range, accepted) in [
                (8, "sdr", true),
                (10, "sdr", false),
                (10, "hdr", cfg!(target_os = "linux") && codec != "h264"),
                (8, "hdr", false),
            ] {
                let mut request = start_json();
                request["configuration"]["video_stream_format"] =
                    json!({"codec":codec,"bit_depth":bits,"dynamic_range":range});
                let start: StartConfig = serde_json::from_value(request).unwrap();
                assert_eq!(
                    start.validate(false).is_ok(),
                    accepted,
                    "{codec}/{bits}/{range}"
                );
            }
        }
    }

    #[test]
    fn ordered_keybindings_start_accepts_sparse_lists_and_rejects_malformed_packets() {
        for selected in [
            None,
            Some(json!(null)),
            Some(json!([])),
            Some(json!([0, 255, 65, 66, 65, 67])),
        ] {
            let mut value = start_json();
            if let Some(selected) = &selected {
                value["keybindings"] = selected.clone();
            }
            let request: StartConfig = serde_json::from_value(value).unwrap();
            assert!(request.validate(true).is_ok());
            assert_eq!(
                serde_json::to_value(&request.keybindings).unwrap(),
                selected.unwrap_or(Value::Null)
            );
        }
        for invalid in [
            json!([65]),
            json!([0, 256]),
            json!([-1, 0]),
            json!([65.0, 66]),
            json!(["65", 66]),
            json!({}),
            json!(vec![0_u8; 514]),
        ] {
            let mut value = start_json();
            value["keybindings"] = invalid.clone();
            assert!(
                serde_json::from_value::<StartConfig>(value).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn scoped_keyboard_behavior_overrides_are_sparse_and_bounded() {
        let inherited: StartConfig = serde_json::from_value(start_json()).unwrap();
        assert!(inherited.high_resolution_scrolling.is_none());
        assert!(inherited.always_send_scancodes.is_none());
        assert!(inherited.key_rightalt_to_key_win.is_none());
        assert!(inherited.key_repeat_delay.is_none());
        assert!(inherited.key_repeat_frequency.is_none());
        assert!(inherited.validate(true).is_ok());

        let mut selected = start_json();
        selected["high_resolution_scrolling"] = json!(false);
        selected["always_send_scancodes"] = json!(false);
        selected["key_rightalt_to_key_win"] = json!(true);
        selected["key_repeat_delay"] = json!(0);
        selected["key_repeat_frequency"] = json!(0.0);
        let selected: StartConfig = serde_json::from_value(selected).unwrap();
        assert_eq!(selected.high_resolution_scrolling, Some(false));
        assert_eq!(selected.always_send_scancodes, Some(false));
        assert_eq!(selected.key_rightalt_to_key_win, Some(true));
        assert_eq!(selected.key_repeat_delay, Some(0));
        assert_eq!(selected.key_repeat_frequency, Some(0.0));
        assert!(selected.validate(true).is_ok());

        for (field, value) in [
            ("key_repeat_delay", json!(i32::MAX as u64 + 1)),
            ("key_repeat_frequency", json!(-0.1)),
        ] {
            let mut invalid = start_json();
            invalid[field] = value;
            let invalid: StartConfig = serde_json::from_value(invalid).unwrap();
            assert_eq!(invalid.validate(true), Err("invalid_start_configuration"));
        }
    }

    #[test]
    fn native_pen_touch_start_override_preserves_absence_and_explicit_booleans() {
        let inherited: StartConfig = serde_json::from_value(start_json()).unwrap();
        assert_eq!(inherited.native_pen_touch, None);
        assert!(
            serde_json::to_value(&inherited)
                .unwrap()
                .get("native_pen_touch")
                .is_none()
        );

        for value in [false, true] {
            let mut selected = start_json();
            selected["native_pen_touch"] = json!(value);
            let selected: StartConfig = serde_json::from_value(selected).unwrap();
            assert_eq!(selected.native_pen_touch, Some(value));
        }

        for invalid in [json!(0), json!(1), json!("false"), json!([]), json!({})] {
            let mut selected = start_json();
            selected["native_pen_touch"] = invalid;
            assert!(serde_json::from_value::<StartConfig>(selected).is_err());
        }
    }

    #[test]
    fn wgc_pacing_smoothing_defaults_on_and_preserves_explicit_disable() {
        let defaulted: StartConfig = serde_json::from_value(start_json()).unwrap();
        assert!(defaulted.wgc_pacing_smoothing);

        let mut selected = start_json();
        selected["wgc_pacing_smoothing"] = json!(false);
        let selected: StartConfig = serde_json::from_value(selected).unwrap();
        assert!(!selected.wgc_pacing_smoothing);
        assert!(selected.validate(true).is_ok());
    }

    #[test]
    fn video_batch_size_defaults_and_rejects_values_above_the_reference_range() {
        let legacy = prepare();
        assert_eq!(legacy.video.max_batch_size_kb, 64);
        assert!(legacy.validate(false).is_ok());

        let mut selected = prepare_json();
        selected["video"] = json!({"max_batch_size_kb":32});
        let selected: PrepareConfig = serde_json::from_value(selected).unwrap();
        assert_eq!(selected.video.max_batch_size_kb, 32);
        assert!(selected.validate(false).is_ok());

        let mut invalid = prepare_json();
        invalid["video"] = json!({"max_batch_size_kb":65});
        let invalid: PrepareConfig = serde_json::from_value(invalid).unwrap();
        assert_eq!(invalid.validate(false), Err("invalid_video_configuration"));
    }

    #[test]
    fn settings_reject_capacity_and_deadline_overflow() {
        for value in [
            json!({"allowed_principals":vec!["owner";17]}),
            json!({"allowed_principals":[""]}),
            json!({"allowed_principals":["line\nfeed"]}),
            json!({"prepare_timeout_ms":99}),
            json!({"prepare_timeout_ms":5001}),
            json!({"drain_timeout_ms":99}),
            json!({"drain_timeout_ms":30001}),
            json!({"input_lease_ttl_ms":999}),
            json!({"input_lease_ttl_ms":30001}),
        ] {
            assert_eq!(
                serde_json::from_value::<Settings>(value)
                    .unwrap()
                    .validate(),
                Err("invalid_settings")
            );
        }
        let settings: Settings = serde_json::from_value(json!({"allowed_principals":vec!["owner";16],"prepare_timeout_ms":100,"drain_timeout_ms":30000})).unwrap();
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn peer_policy_rejects_mapped_multicast_unspecified_and_zero_ports() {
        for address in [
            "0.0.0.0",
            "::",
            "239.1.2.3",
            "ff02::1",
            "255.255.255.255",
            "::ffff:127.0.0.1",
            "::ffff:192.0.2.1",
        ] {
            let mut config = prepare();
            config.peer_ip = address.parse().unwrap();
            assert_eq!(config.validate(true), Err("invalid_peer"), "{address}");
        }
        let mut config = prepare();
        config.peer_ip = "192.0.2.1".parse().unwrap();
        assert_eq!(config.validate(false), Err("remote_disabled"));
        assert!(config.validate(true).is_ok());
        config.video_port = Some(0);
        assert_eq!(config.validate(true), Err("invalid_peer"));
        config.video_port = Some(49000);
        config.audio_port = Some(0);
        assert_eq!(config.validate(true), Err("invalid_peer"));
    }

    #[test]
    fn discovery_requires_omitted_ports_and_distinct_bounded_payloads() {
        let mut value = prepare_json();
        let object = value.as_object_mut().unwrap();
        object.remove("video_port");
        object.remove("audio_port");
        value["discovery"] = json!({
            "video_ping_payload":"0123456789abcdef",
            "audio_ping_payload":"fedcba9876543210"
        });
        let config: PrepareConfig = serde_json::from_value(value.clone()).unwrap();
        assert!(config.validate(false).is_ok());

        value["video_port"] = json!(49000);
        assert_eq!(
            serde_json::from_value::<PrepareConfig>(value.clone())
                .unwrap()
                .validate(false),
            Err("invalid_peer_discovery")
        );
        value.as_object_mut().unwrap().remove("video_port");
        value["discovery"]["audio_ping_payload"] = json!("0123456789abcdef");
        assert_eq!(
            serde_json::from_value::<PrepareConfig>(value.clone())
                .unwrap()
                .validate(false),
            Err("invalid_peer_discovery")
        );
        value["discovery"]["audio_ping_payload"] = json!("bad+payload!!!!!");
        assert_eq!(
            serde_json::from_value::<PrepareConfig>(value)
                .unwrap()
                .validate(false),
            Err("invalid_peer_discovery")
        );
    }

    #[test]
    fn every_bind_obeys_family_and_remote_policy() {
        for field in ["video_bind", "audio_bind", "control_bind"] {
            for address in [
                "[::1]:0",
                "239.1.2.3:0",
                "255.255.255.255:0",
                "[::ffff:127.0.0.1]:0",
            ] {
                let mut value = prepare_json();
                value[field] = json!(address);
                let config: PrepareConfig = serde_json::from_value(value).unwrap();
                assert_eq!(
                    config.validate(true),
                    Err("invalid_bind"),
                    "{field}/{address}"
                );
            }
            let mut value = prepare_json();
            value[field] = json!("0.0.0.0:0");
            let config: PrepareConfig = serde_json::from_value(value).unwrap();
            assert_eq!(config.validate(false), Err("invalid_bind"));
            assert!(config.validate(true).is_ok());
        }
        let mut config = prepare();
        config.peer_ip = "::1".parse().unwrap();
        config.control_bind = Some("[::1]:0".parse().unwrap());
        assert!(config.validate(false).is_ok());
    }

    #[test]
    fn unsupported_wire_audio_video_and_invalid_secrets_fail_before_allocation() {
        for (field, value) in [
            ("audio", json!({"packet_ms":20})),
            ("audio", json!({"packet_ms":0})),
            ("audio", json!({"bitrate":5999})),
            ("audio", json!({"bitrate":512001})),
            ("video", json!({"packet_size":0})),
            ("video", json!({"packet_size":65535})),
            ("video", json!({"max_fec_blocks":0})),
            ("video", json!({"max_fec_blocks":5})),
        ] {
            let mut config = prepare_json();
            config[field] = value;
            let config: PrepareConfig = serde_json::from_value(config).unwrap();
            assert_eq!(
                config.validate(false),
                Err(if field == "audio" {
                    "invalid_audio_configuration"
                } else {
                    "invalid_video_configuration"
                })
            );
        }
        for key in [
            "",
            "0123456789abcdef0123456789abcdeg",
            "0123456789abcdef0123456789abcdef00",
        ] {
            let mut config = prepare();
            config.key_hex = key.into();
            assert_eq!(config.validate(false), Err("invalid_key"));
        }
        let mut config = prepare();
        config.operation_id = "\n".into();
        assert_eq!(config.validate(false), Err("invalid_operation_id"));
        for packet_ms in [5, 10] {
            for bitrate in [6000, 256000, 512000] {
                let mut config = prepare();
                config.audio = AudioWire {
                    packet_ms,
                    bitrate,
                    ..AudioWire::default()
                };
                assert!(config.validate(false).is_ok());
            }
        }
    }

    #[test]
    fn start_validation_rejects_unsupported_geometry_and_labels() {
        for (field, value) in [
            ("width", 17),
            ("width", 3842),
            ("height", 15),
            ("height", 2162),
            ("fps", 0),
            ("fps", 121),
            ("bitrate", 99999),
            ("bitrate", 100000001),
            ("max_reference_frames", 17),
        ] {
            let mut config = start_json();
            config["configuration"][field] = json!(value);
            assert_eq!(
                serde_json::from_value::<StartConfig>(config)
                    .unwrap()
                    .validate(true),
                Err("invalid_start_configuration")
            );
        }
        for field in ["output", "audio_endpoint", "device_name"] {
            let mut config = start_json();
            config[field] = json!("bad\nlabel");
            assert!(
                serde_json::from_value::<StartConfig>(config)
                    .unwrap()
                    .validate(true)
                    .is_err()
            );
        }
    }

    #[test]
    fn unknown_fields_are_rejected_at_each_nested_boundary() {
        let mut config = prepare_json();
        config["owner"] = json!("forged");
        assert!(serde_json::from_value::<PrepareConfig>(config).is_err());
        for field in ["video", "audio"] {
            let mut config = prepare_json();
            config[field] = json!({"unsupported":true});
            assert!(serde_json::from_value::<PrepareConfig>(config).is_err());
        }
        let mut config = start_json();
        config["configuration"]["codec"] = json!("vp9");
        assert!(serde_json::from_value::<StartConfig>(config).is_err());
        let mut config = start_json();
        config["capture_provider"] = json!("arbitrary/service");
        assert!(serde_json::from_value::<StartConfig>(config).is_err());
        assert!(serde_json::from_value::<Settings>(json!({"administrator":true})).is_err());
        assert!(
            serde_json::from_value::<Operation>(json!({"action":"status","owner":"forged"}))
                .is_err()
        );
    }
}

#[cfg(test)]
mod encoder_source_tests {
    use super::*;
    use serde_json::json;

    fn source() -> serde_json::Value {
        json!({"source_id":42,"encoder_provider":"fixture","encoder":{"clsid":"source","friendly_name":"Source","hardware_url":null},"configuration":{"width":1280,"height":720,"fps":60,"bitrate":10000000},"audio_endpoint":"default"})
    }

    #[test]
    fn source_round_trips_and_requires_authorizable_identity() {
        let config: StartConfig = serde_json::from_value(source()).unwrap();
        assert!(config.validate(true).is_ok());
        let encoded = serde_json::to_value(&config).unwrap();
        assert!(encoded.get("capture_provider").is_none());
        assert!(
            serde_json::from_value::<StartConfig>(encoded)
                .unwrap()
                .validate(true)
                .is_ok()
        );
        for field in [
            "capture_provider",
            "output",
            "constant_rate",
            "minimum_fps_target",
            "wgc_pacing_smoothing",
        ] {
            let mut request = source();
            request[field] = json!(null);
            assert!(serde_json::from_value::<StartConfig>(request).is_err());
        }
        for options in [
            json!([]),
            json!("invalid"),
            json!({"large": "x".repeat(65536)}),
        ] {
            let mut request = source();
            request["encoder_options"] = options;
            assert_eq!(
                serde_json::from_value::<StartConfig>(request)
                    .unwrap()
                    .validate(true),
                Err("invalid_encoder_options")
            );
        }
        let mut without_provider = source();
        without_provider["encoder_options"] = json!({});
        without_provider["encoder_provider"] = json!(null);
        assert_eq!(
            serde_json::from_value::<StartConfig>(without_provider)
                .unwrap()
                .validate(true),
            Err("invalid_encoder_options")
        );
        for (field, value) in [
            ("source_id", json!(0)),
            ("encoder_provider", json!("bad/service")),
            ("encoder_provider", json!(null)),
            ("encoder", json!(0)),
        ] {
            let mut request = source();
            request[field] = value;
            assert!(
                serde_json::from_value::<StartConfig>(request)
                    .unwrap()
                    .validate(true)
                    .is_err()
            );
        }
    }
}
