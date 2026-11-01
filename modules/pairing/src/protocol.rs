use amaterasu_module_sdk::effect_authority::FeedbackCapability;
pub use amaterasu_module_sdk::effect_authority::{FeedbackPermission, FeedbackPolicy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_DOCUMENT_BYTES: usize = 12_000;
pub const MAX_CLIENTS: usize = 8;
const MAX_FRAME_LIMITER_FPS: f64 = 1_000.0;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub administrator_principals: BTreeSet<String>,
    pub transport_principals: BTreeSet<String>,
    pub effect_authority: Option<EffectAuthoritySettings>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectAuthoritySettings {
    pub protocol_owner: String,
    pub effect_owner: String,
    /// Trusted startup composition selects the configuration owner explicitly.
    #[serde(default)]
    pub configuration_owner: Option<String>,
    #[serde(default)]
    pub feedback_policies: Vec<FeedbackPolicy>,
}
impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        for principals in [&self.administrator_principals, &self.transport_principals] {
            if principals.len() > 16 || principals.iter().any(|value| !identifier(value, 128)) {
                return Err("invalid_settings");
            }
        }
        if self.effect_authority.as_ref().is_some_and(|settings| {
            !identifier(&settings.protocol_owner, 128)
                || !identifier(&settings.effect_owner, 128)
                || !self.transport_principals.contains(&settings.protocol_owner)
                || settings.protocol_owner == settings.effect_owner
                || settings.configuration_owner.as_ref().is_some_and(|owner| {
                    !identifier(owner, 128)
                        || owner == &settings.protocol_owner
                        || owner == &settings.effect_owner
                })
                || settings.feedback_policies.len() > 32
                || settings
                    .feedback_policies
                    .iter()
                    .any(|policy| !policy.capability.valid())
                || settings
                    .feedback_policies
                    .iter()
                    .map(|policy| &policy.capability)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != settings.feedback_policies.len()
        }) {
            return Err("invalid_settings");
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Initialize {},
    ImportIdentity {
        certificate_pem: String,
        private_key_pem: String,
        host_id: Option<String>,
    },
    ImportClient {
        client_id: String,
        name: String,
        certificate_pem: String,
    },
    Identity {},
    Begin {
        client_id: String,
        name: String,
        client_certificate_pem: String,
        salt: String,
    },
    ApprovePin {
        pairing_id: String,
        pin: String,
    },
    GetServerCert {
        pairing_id: String,
    },
    ClientChallenge {
        pairing_id: String,
        clientchallenge: String,
    },
    ServerChallengeResponse {
        pairing_id: String,
        serverchallengeresp: String,
    },
    ClientPairingSecret {
        pairing_id: String,
        clientpairingsecret: String,
    },
    Pending {},
    Cancel {
        pairing_id: String,
    },
    ListClients {},
    Unpair {
        certificate_id: String,
        expected_revision: Option<u64>,
    },
    UpdateClientConfigOverrides {
        certificate_id: String,
        config_overrides: Box<ClientConfigOverrides>,
        expected_revision: u64,
    },
    UpdateClientPermissions {
        certificate_id: String,
        permissions: ClientPermissions,
        expected_revision: u64,
    },
    UpdateClientFeedbackGrants {
        certificate_id: String,
        grants: BTreeSet<FeedbackCapability>,
        expected_revision: u64,
    },
    SetPermissionMode {
        enabled: bool,
        expected_revision: u64,
    },
    CheckClientCertificate {
        certificate_pem: String,
    },
    /// Removes exactly the certificate a trusted transport authenticated on a
    /// live TLS connection. Never selected by client ID or fingerprint.
    UnpairPresentedClient {
        certificate_pem: String,
    },
}
impl Operation {
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid = match self {
            Self::ImportIdentity {
                certificate_pem,
                private_key_pem,
                host_id,
            } => {
                certificate_pem.len() <= 4096
                    && private_key_pem.len() <= 4096
                    && host_id.as_deref().is_none_or(valid_host_id)
            }
            Self::ImportClient {
                client_id,
                name,
                certificate_pem,
            } => {
                identifier(client_id, 64)
                    && !name.is_empty()
                    && name.len() <= 128
                    && !name.chars().any(char::is_control)
                    && certificate_pem.len() <= 4096
            }
            Self::Begin {
                client_id,
                name,
                client_certificate_pem,
                salt,
            } => {
                identifier(client_id, 64)
                    && !name.is_empty()
                    && name.len() <= 128
                    && !name.chars().any(char::is_control)
                    && client_certificate_pem.len() <= 4096
                    && hex_length(salt, 16)
            }
            Self::ApprovePin { pairing_id, pin } => {
                identifier(pairing_id, 32)
                    && pin.len() == 4
                    && pin.bytes().all(|byte| byte.is_ascii_digit())
            }
            Self::GetServerCert { pairing_id } | Self::Cancel { pairing_id } => {
                identifier(pairing_id, 32)
            }
            Self::ClientChallenge {
                pairing_id,
                clientchallenge,
            } => identifier(pairing_id, 32) && hex_length(clientchallenge, 16),
            Self::ServerChallengeResponse {
                pairing_id,
                serverchallengeresp,
            } => identifier(pairing_id, 32) && hex_length(serverchallengeresp, 32),
            Self::ClientPairingSecret {
                pairing_id,
                clientpairingsecret,
            } => {
                identifier(pairing_id, 32)
                    && clientpairingsecret.len() <= 1056
                    && clientpairingsecret.len() >= 544
                    && hex_length(clientpairingsecret, clientpairingsecret.len() / 2)
            }
            Self::Unpair { certificate_id, .. } => hex_length(certificate_id, 32),
            Self::UpdateClientConfigOverrides {
                certificate_id,
                config_overrides,
                expected_revision: _,
            } => hex_length(certificate_id, 32) && config_overrides.validate().is_ok(),
            Self::UpdateClientPermissions { certificate_id, .. } => hex_length(certificate_id, 32),
            Self::UpdateClientFeedbackGrants {
                certificate_id,
                grants,
                ..
            } => {
                hex_length(certificate_id, 32)
                    && grants.len() <= 32
                    && grants.iter().all(FeedbackCapability::valid)
            }
            Self::CheckClientCertificate { certificate_pem }
            | Self::UnpairPresentedClient { certificate_pem } => certificate_pem.len() <= 4096,
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err("invalid_request")
        }
    }
    pub fn may_write(&self) -> bool {
        matches!(
            self,
            Self::Initialize {}
                | Self::ImportIdentity { .. }
                | Self::ImportClient { .. }
                | Self::ClientPairingSecret { .. }
                | Self::Unpair { .. }
                | Self::UnpairPresentedClient { .. }
                | Self::UpdateClientConfigOverrides { .. }
                | Self::UpdateClientPermissions { .. }
                | Self::UpdateClientFeedbackGrants { .. }
                | Self::SetPermissionMode { .. }
        )
    }

    /// Operations only a configured transport principal may call, because the
    /// supplied certificate is trusted to come from an authenticated handshake.
    pub fn transport_only(&self) -> bool {
        matches!(
            self,
            Self::CheckClientCertificate { .. } | Self::UnpairPresentedClient { .. }
        )
    }

    pub fn admin_only(&self) -> bool {
        matches!(
            self,
            Self::Initialize {}
                | Self::ImportIdentity { .. }
                | Self::ImportClient { .. }
                | Self::ApprovePin { .. }
                | Self::Pending {}
                | Self::ListClients {}
                | Self::Unpair { .. }
                | Self::UpdateClientConfigOverrides { .. }
                | Self::UpdateClientPermissions { .. }
                | Self::UpdateClientFeedbackGrants { .. }
                | Self::SetPermissionMode { .. }
        )
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub identity: Option<Identity>,
    pub clients: Vec<Client>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub permission_mode_enabled: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub host_id: String,
    pub certificate_pem: String,
    pub private_key_pem: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    pub certificate_id: String,
    pub client_id: String,
    pub name: String,
    pub certificate_pem: String,
    pub paired_at: u64,
    #[serde(default, skip_serializing_if = "ClientPermissions::is_default")]
    pub permissions: ClientPermissions,
    #[serde(default, skip_serializing_if = "ClientConfigOverrides::is_empty")]
    pub config_overrides: ClientConfigOverrides,
    /// Explicit per-client opt-in; old clients have no extension grants.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub feedback_grants: BTreeSet<FeedbackCapability>,
}

/// Certificate-bound privileges. Missing legacy metadata retains full access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientPermissions {
    pub list_apps: bool,
    pub view_streams: bool,
    pub launch_apps: bool,
    pub terminate_streams: bool,
}
impl Default for ClientPermissions {
    fn default() -> Self {
        Self {
            list_apps: true,
            view_streams: true,
            launch_apps: true,
            terminate_streams: true,
        }
    }
}
impl ClientPermissions {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn subsequent_client() -> Self {
        Self {
            list_apps: true,
            view_streams: true,
            launch_apps: false,
            terminate_streams: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfigOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sw_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sw_tune: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder: Option<amaterasu_module_sdk::EncoderBackend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_threads: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qp: Option<i32>,
    /// Physical presentation mode; the streamed viewport and cadence are separate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fec_percentage: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bitrate: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_fps_target: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_max_batch_size_kb: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_mode_remapping: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_resolution_option: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_manual_resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_refresh_rate_option: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_manual_refresh_rate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_hdr_option: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_configuration_option: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_hdr_request_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_fps_limit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_enable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_disable_vsync: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_auto_virtual_framegen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtss_frame_limit_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_audio: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virtual_sink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sink_capture_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvenc_spatial_aq: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvenc_preset: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvenc_h264_cavlc: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvenc_twopass: Option<amaterasu_module_sdk::NvencTwoPass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvenc_vbv_increase: Option<u32>,
    #[serde(
        default,
        alias = "nvenc_force_split_encode",
        skip_serializing_if = "Option::is_none"
    )]
    pub nvenc_split_encode: Option<amaterasu_module_sdk::NvencSplitEncode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvenc_realtime_hags: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvenc_opengl_vulkan_on_dxgi: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvenc_latency_over_power: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vaapi_strict_rc_buffer: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back_button_timeout: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamepad: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ds4_back_as_touchpad_click: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_pen_touch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_as_ds4: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touchpad_as_ds4: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ds5_inputtino_randomize_mac: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proton_dualsense_compatibility: Option<bool>,
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
}

impl ClientConfigOverrides {
    pub fn validate(&self) -> Result<(), &'static str> {
        amaterasu_module_sdk::validate_software_options(
            self.sw_preset.as_deref(),
            self.sw_tune.as_deref(),
        )
        .map_err(|_| "invalid_config_overrides")?;
        amaterasu_module_sdk::validate_min_threads(self.min_threads)
            .map_err(|_| "invalid_config_overrides")?;
        if (amaterasu_module_sdk::NvencSettingsOverrides {
            nvenc_preset: self.nvenc_preset,
            nvenc_h264_cavlc: self.nvenc_h264_cavlc,
            nvenc_twopass: self.nvenc_twopass,
            nvenc_vbv_increase: self.nvenc_vbv_increase,
            nvenc_split_encode: self.nvenc_split_encode,
            nvenc_realtime_hags: self.nvenc_realtime_hags,
            nvenc_opengl_vulkan_on_dxgi: self.nvenc_opengl_vulkan_on_dxgi,
            nvenc_latency_over_power: self.nvenc_latency_over_power,
            vaapi_strict_rc_buffer: self.vaapi_strict_rc_buffer,
        })
        .validate()
        .is_err()
        {
            return Err("invalid_config_overrides");
        }
        if self
            .display_mode
            .as_deref()
            .is_some_and(|mode| parse_client_display_mode(mode).is_none())
        {
            return Err("invalid_config_overrides");
        }
        if self
            .fec_percentage
            .is_some_and(|percentage| percentage == 0)
        {
            return Err("invalid_config_overrides");
        }
        if self
            .minimum_fps_target
            .is_some_and(|target| !target.is_finite() || !(0.0..=1000.0).contains(&target))
            || self.video_max_batch_size_kb.is_some_and(|size| size > 64)
            || self
                .frame_limiter_fps_limit
                .is_some_and(|fps| !valid_frame_limiter_fps_limit(fps))
            || self
                .frame_limiter_provider
                .as_deref()
                .is_some_and(|provider| !valid_frame_limiter_provider(provider))
            || self
                .frame_limiter_auto_virtual_framegen
                .as_deref()
                .is_some_and(|mode| !matches!(mode, "enabled" | "vrr" | "legacy" | "disabled"))
            || self.rtss_frame_limit_type.as_deref().is_some_and(|mode| {
                !matches!(
                    mode,
                    "async" | "front edge sync" | "back edge sync" | "nvidia reflex"
                )
            })
            || self.has_unsupported_frame_limiter_fields()
        {
            return Err("invalid_config_overrides");
        }
        if self
            .dd_mode_remapping
            .as_ref()
            .is_some_and(|rules| !valid_display_mode_remapping(rules))
            || self
                .dd_resolution_option
                .as_deref()
                .is_some_and(|option| !matches!(option, "auto" | "disabled" | "manual"))
            || self
                .dd_manual_resolution
                .as_deref()
                .is_some_and(|resolution| !valid_manual_resolution(resolution))
            || self
                .dd_refresh_rate_option
                .as_deref()
                .is_some_and(|option| {
                    !matches!(option, "auto" | "disabled" | "manual" | "prefer_highest")
                })
            || self
                .dd_manual_refresh_rate
                .as_deref()
                .is_some_and(|refresh| !valid_manual_refresh_rate(refresh))
            || self
                .dd_hdr_option
                .as_deref()
                .is_some_and(|option| !matches!(option, "auto" | "disabled"))
            || self
                .dd_configuration_option
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
            || self
                .dd_hdr_request_override
                .as_deref()
                .is_some_and(|option| !matches!(option, "auto" | "force_on" | "force_off"))
        {
            return Err("invalid_config_overrides");
        }
        if self
            .audio_sink
            .as_deref()
            .is_some_and(|endpoint| endpoint.len() > 4096 || endpoint.chars().any(char::is_control))
        {
            return Err("invalid_config_overrides");
        }
        if self
            .virtual_sink
            .as_deref()
            .is_some_and(|selector| selector.len() > 4096 || selector.chars().any(char::is_control))
        {
            return Err("invalid_config_overrides");
        }
        if self
            .gamepad
            .as_deref()
            .is_some_and(|profile| profile.len() > 32 || !valid_gamepad_profile(profile))
        {
            return Err("invalid_config_overrides");
        }
        if self
            .key_repeat_delay
            .is_some_and(|delay| delay > i32::MAX as u32)
            || self
                .key_repeat_frequency
                .is_some_and(|frequency| !frequency.is_finite() || frequency < 0.0)
        {
            return Err("invalid_config_overrides");
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.sw_preset.is_none()
            && self.sw_tune.is_none()
            && self.encoder.is_none()
            && self.min_threads.is_none()
            && self.qp.is_none()
            && self.display_mode.is_none()
            && self.fec_percentage.is_none()
            && self.max_bitrate.is_none()
            && self.minimum_fps_target.is_none()
            && self.video_max_batch_size_kb.is_none()
            && self.dd_mode_remapping.is_none()
            && self.dd_resolution_option.is_none()
            && self.dd_manual_resolution.is_none()
            && self.dd_refresh_rate_option.is_none()
            && self.dd_manual_refresh_rate.is_none()
            && self.dd_hdr_option.is_none()
            && self.dd_configuration_option.is_none()
            && self.dd_hdr_request_override.is_none()
            && self.frame_limiter_fps_limit.is_none()
            && self.frame_limiter_enable.is_none()
            && self.frame_limiter_provider.is_none()
            && self.frame_limiter_disable_vsync.is_none()
            && self.frame_limiter_auto_virtual_framegen.is_none()
            && self.rtss_frame_limit_type.is_none()
            && self.stream_audio.is_none()
            && self.audio_sink.is_none()
            && self.virtual_sink.is_none()
            && self.audio_sink_capture_only.is_none()
            && self.nvenc_spatial_aq.is_none()
            && self.nvenc_preset.is_none()
            && self.nvenc_h264_cavlc.is_none()
            && self.nvenc_twopass.is_none()
            && self.nvenc_vbv_increase.is_none()
            && self.nvenc_split_encode.is_none()
            && self.nvenc_realtime_hags.is_none()
            && self.nvenc_opengl_vulkan_on_dxgi.is_none()
            && self.nvenc_latency_over_power.is_none()
            && self.vaapi_strict_rc_buffer.is_none()
            && self.back_button_timeout.is_none()
            && self.controller.is_none()
            && self.gamepad.is_none()
            && self.ds4_back_as_touchpad_click.is_none()
            && self.keyboard.is_none()
            && self.mouse.is_none()
            && self.native_pen_touch.is_none()
            && self.motion_as_ds4.is_none()
            && self.touchpad_as_ds4.is_none()
            && self.ds5_inputtino_randomize_mac.is_none()
            && self.proton_dualsense_compatibility.is_none()
            && self.high_resolution_scrolling.is_none()
            && self.always_send_scancodes.is_none()
            && self.key_rightalt_to_key_win.is_none()
            && self.keybindings.is_none()
            && self.key_repeat_delay.is_none()
            && self.key_repeat_frequency.is_none()
    }

    fn has_unsupported_frame_limiter_fields(&self) -> bool {
        #[cfg(windows)]
        {
            false
        }
        #[cfg(target_os = "linux")]
        {
            self.frame_limiter_disable_vsync.is_some()
                || self.frame_limiter_auto_virtual_framegen.is_some()
                || self.rtss_frame_limit_type.is_some()
        }
        #[cfg(target_os = "macos")]
        {
            self.frame_limiter_disable_vsync.is_some()
                || self.frame_limiter_auto_virtual_framegen.is_some()
                || self.rtss_frame_limit_type.is_some()
        }
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        {
            self.frame_limiter_disable_vsync.is_some()
                || self.frame_limiter_auto_virtual_framegen.is_some()
                || self.rtss_frame_limit_type.is_some()
        }
    }
}

/// The final component is Hz with up to three decimals. Historical integral
/// values above 1000 are already millihertz, as in paired client exports.
fn parse_client_display_mode(mode: &str) -> Option<(u32, u32, u32)> {
    let mut components = mode.split('x');
    let parse_unsigned = |text: &str| {
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse::<u32>().ok())
            .flatten()
    };
    let width = parse_unsigned(components.next()?)?;
    let height = parse_unsigned(components.next()?)?;
    let refresh = components.next()?;
    if components.next().is_some()
        || width == 0
        || height == 0
        || width > i32::MAX as u32
        || height > i32::MAX as u32
    {
        return None;
    }
    let millihertz = if let Some((whole, fraction)) = refresh.split_once('.') {
        if fraction.is_empty()
            || fraction.len() > 3
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let whole = parse_unsigned(whole)?;
        let scale = 10_u32.pow(3 - fraction.len() as u32);
        let fractional_millihertz = parse_unsigned(fraction)?.checked_mul(scale)?;
        whole
            .checked_mul(1000)?
            .checked_add(fractional_millihertz)?
    } else {
        let raw = parse_unsigned(refresh)?;
        if raw > 1000 {
            raw
        } else {
            raw.checked_mul(1000)?
        }
    };
    (10_000..=1_000_000)
        .contains(&millihertz)
        .then_some((width, height, millihertz))
}

fn valid_display_mode_remapping(value: &serde_json::Value) -> bool {
    let Some(lists) = value.as_object() else {
        return false;
    };
    lists.iter().all(|(kind, entries)| {
        matches!(
            kind.as_str(),
            "mixed" | "resolution_only" | "refresh_rate_only"
        ) && entries.as_array().is_some_and(|entries| {
            entries.len() <= 128
                && entries.iter().all(|entry| {
                    entry.as_object().is_some_and(|fields| {
                        fields.iter().all(|(name, value)| {
                            matches!(
                                name.as_str(),
                                "requested_resolution"
                                    | "requested_fps"
                                    | "final_resolution"
                                    | "final_refresh_rate"
                            ) && value.as_str().is_some_and(|text| {
                                text.len() <= 64 && !text.chars().any(char::is_control)
                            })
                        }) && valid_display_rule(kind, fields)
                    })
                })
        })
    })
}

fn valid_display_rule(kind: &str, fields: &serde_json::Map<String, serde_json::Value>) -> bool {
    let field = |name: &str| {
        fields
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let resolution = matches!(kind, "mixed" | "resolution_only");
    let refresh = matches!(kind, "mixed" | "refresh_rate_only");
    let final_resolution = field("final_resolution");
    let final_refresh = field("final_refresh_rate");
    (resolution && !final_resolution.trim().is_empty()
        || refresh && !final_refresh.trim().is_empty())
        && (!resolution
            || valid_rule_resolution(field("requested_resolution"))
                && valid_rule_resolution(final_resolution))
        && (!refresh
            || valid_rule_rate(field("requested_fps"), false)
                && valid_rule_rate(final_refresh, true))
        && (resolution || field("requested_resolution").is_empty() && final_resolution.is_empty())
        && (refresh || field("requested_fps").is_empty() && final_refresh.is_empty())
}

fn valid_rule_resolution(value: &str) -> bool {
    let normalized = value.trim().replace('×', "x");
    if normalized.is_empty() {
        return true;
    }
    let Some((width, height)) = normalized.split_once('x') else {
        return false;
    };
    !width.is_empty()
        && !height.is_empty()
        && width.bytes().all(|byte| byte.is_ascii_digit())
        && height.bytes().all(|byte| byte.is_ascii_digit())
        && width.parse::<u32>().is_ok_and(|value| value > 0)
        && height.parse::<u32>().is_ok_and(|value| value > 0)
}

fn valid_rule_rate(value: &str, allow_fraction: bool) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    let mut parts = value.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || parts.next().is_some()
        || !allow_fraction && fraction.is_some()
        || fraction.is_some_and(|digits| {
            digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return false;
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = fraction.unwrap_or_default().trim_end_matches('0');
    let numerator = format!("{whole}{fraction}");
    numerator.parse::<u32>().is_ok_and(|value| value > 0)
        && (fraction.is_empty() || 10_u32.checked_pow(fraction.len() as u32).is_some())
}

fn valid_manual_resolution(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    if value.len() > 32 {
        return false;
    }
    let mut parts = value.split(['x', '×']);
    let valid_dimension = |part: Option<&str>| {
        part.is_some_and(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u32>().is_ok()
        })
    };
    valid_dimension(parts.next()) && valid_dimension(parts.next()) && parts.next().is_none()
}

fn valid_manual_refresh_rate(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    if value.len() > 64 {
        return false;
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || value.contains('.') && fraction.is_empty()
    {
        return false;
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let Ok(integer) = whole.parse::<u32>() else {
        return false;
    };
    let significant = fraction.trim_end_matches('0');
    if significant.is_empty() {
        return true;
    }
    if significant.len() > 31 {
        return false;
    }
    let (Ok(fraction), Some(denominator)) = (
        significant.parse::<u128>(),
        10_u128.checked_pow(significant.len() as u32),
    ) else {
        return false;
    };
    let mut left = fraction;
    let mut right = denominator;
    while right != 0 {
        (left, right) = (right, left % right);
    }
    let reduced_denominator = denominator / left;
    reduced_denominator <= u32::MAX as u128
        && (u128::from(integer) * reduced_denominator + fraction / left) <= u32::MAX as u128
}

fn valid_frame_limiter_fps_limit(fps: f64) -> bool {
    if !fps.is_finite() || !(0.0..=MAX_FRAME_LIMITER_FPS).contains(&fps) {
        return false;
    }

    let millihz = fps * 1_000.0;
    (millihz - millihz.round()).abs() <= 0.000_001
}

fn valid_frame_limiter_provider(provider: &str) -> bool {
    #[cfg(windows)]
    {
        matches!(provider, "auto" | "rtss" | "nvidia-control-panel" | "none")
    }
    #[cfg(target_os = "linux")]
    {
        matches!(
            provider,
            "auto" | "mangohud" | "proton" | "mangohud-proton" | "none"
        )
    }
    #[cfg(target_os = "macos")]
    {
        provider == "auto"
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        provider == "auto"
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
impl Document {
    pub fn validate(&self, now: u64) -> Result<(), &'static str> {
        if self.clients.len() > MAX_CLIENTS
            || serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > MAX_DOCUMENT_BYTES)
        {
            return Err("invalid_state");
        }
        if let Some(identity) = &self.identity {
            if !valid_host_id(&identity.host_id) {
                return Err("invalid_state");
            }
            crate::crypto::validate_identity(identity, now).map_err(|_| "invalid_state")?;
        } else if !self.clients.is_empty() {
            return Err("invalid_state");
        }
        let mut ids = BTreeSet::new();
        for client in &self.clients {
            // Expired paired certificates remain removable/listable. Authentication
            // validates time separately; loading old records never renews trust.
            let certificate = crate::crypto::parse_certificate(&client.certificate_pem, None)
                .map_err(|_| "invalid_state")?;
            if !ids.insert(&client.certificate_id)
                || certificate.fingerprint != client.certificate_id
                || !identifier(&client.client_id, 64)
                || client.name.is_empty()
                || client.name.len() > 128
                || client.config_overrides.validate().is_err()
                || client.feedback_grants.len() > 32
                || client.feedback_grants.iter().any(|grant| !grant.valid())
            {
                return Err("invalid_state");
            }
        }
        Ok(())
    }
}
fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-:/".contains(&byte))
}
fn hex_length(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn error(code: &str) -> Value {
    json!({"ok":false,"paired":0,"code":code})
}

pub fn valid_host_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn client_audio_override_is_sparse_and_preserves_legacy_fec_records() {
        let legacy: ClientConfigOverrides =
            serde_json::from_value(json!({"fec_percentage":73})).unwrap();
        assert_eq!(legacy.fec_percentage, Some(73));
        assert_eq!(legacy.max_bitrate, None);
        assert_eq!(legacy.minimum_fps_target, None);
        assert_eq!(legacy.video_max_batch_size_kb, None);
        assert_eq!(legacy.stream_audio, None);
        legacy.validate().unwrap();
        assert_eq!(
            serde_json::to_value(legacy).unwrap(),
            json!({"fec_percentage":73})
        );

        let inherited = ClientConfigOverrides::default();
        assert!(inherited.is_empty());
        assert_eq!(serde_json::to_value(inherited).unwrap(), json!({}));

        let disabled: ClientConfigOverrides = serde_json::from_value(json!({
            "fec_percentage":73,
            "stream_audio":false
        }))
        .unwrap();
        assert_eq!(disabled.fec_percentage, Some(73));
        assert_eq!(disabled.stream_audio, Some(false));
        assert!(!disabled.is_empty());
        disabled.validate().unwrap();
        assert_eq!(
            serde_json::to_value(disabled).unwrap(),
            json!({"fec_percentage":73,"stream_audio":false})
        );
    }

    #[test]
    fn client_native_pen_touch_override_is_sparse_and_strictly_boolean() {
        let missing = ClientConfigOverrides::default();
        assert_eq!(missing.native_pen_touch, None);
        assert!(missing.is_empty());
        assert_eq!(serde_json::to_value(missing).unwrap(), json!({}));

        let null: ClientConfigOverrides =
            serde_json::from_value(json!({"native_pen_touch":null})).unwrap();
        assert_eq!(null.native_pen_touch, None);
        assert!(null.is_empty());
        assert_eq!(serde_json::to_value(null).unwrap(), json!({}));

        for value in [false, true] {
            let overrides: ClientConfigOverrides =
                serde_json::from_value(json!({"native_pen_touch":value})).unwrap();
            assert_eq!(overrides.native_pen_touch, Some(value));
            assert!(!overrides.is_empty());
            overrides.validate().unwrap();
            assert_eq!(
                serde_json::to_value(overrides).unwrap(),
                json!({"native_pen_touch":value})
            );
        }

        for invalid in [
            json!({"native_pen_touch":0}),
            json!({"native_pen_touch":1}),
            json!({"native_pen_touch":"false"}),
            json!({"native_pen_touch":[]}),
            json!({"native_pen_touch":{}}),
        ] {
            assert!(serde_json::from_value::<ClientConfigOverrides>(invalid).is_err());
        }
    }

    #[test]
    fn client_ds4_and_proton_overrides_are_sparse_and_strictly_boolean() {
        for name in [
            "motion_as_ds4",
            "touchpad_as_ds4",
            "ds5_inputtino_randomize_mac",
            "proton_dualsense_compatibility",
        ] {
            let missing: ClientConfigOverrides = serde_json::from_value(json!({})).unwrap();
            assert!(missing.is_empty());

            for value in [true, false] {
                let overrides: ClientConfigOverrides =
                    serde_json::from_value(json!({(name):value})).unwrap();
                let serialized = serde_json::to_value(&overrides).unwrap();
                assert_eq!(serialized[name], value);
            }

            let explicit_null: ClientConfigOverrides =
                serde_json::from_value(json!({(name):null})).unwrap();
            assert!(explicit_null.is_empty());

            for invalid in [json!(0), json!(1), json!("false"), json!([]), json!({})] {
                assert!(
                    serde_json::from_value::<ClientConfigOverrides>(json!({(name):invalid}))
                        .is_err(),
                    "{name} accepted {invalid}"
                );
            }
        }
    }

    #[test]
    fn client_ds4_back_touchpad_click_override_is_sparse_and_preserves_legacy_records() {
        let legacy: ClientConfigOverrides = serde_json::from_value(json!({
            "controller": true,
            "gamepad": "xbox"
        }))
        .unwrap();
        assert_eq!(legacy.ds4_back_as_touchpad_click, None);
        assert_eq!(
            serde_json::to_value(legacy).unwrap(),
            json!({"controller":true,"gamepad":"xbox"})
        );

        let inherited = ClientConfigOverrides::default();
        assert_eq!(inherited.ds4_back_as_touchpad_click, None);
        assert!(inherited.is_empty());
        assert_eq!(serde_json::to_value(inherited).unwrap(), json!({}));

        for value in [false, true] {
            let overrides: ClientConfigOverrides = serde_json::from_value(json!({
                "ds4_back_as_touchpad_click": value
            }))
            .unwrap();
            assert_eq!(overrides.ds4_back_as_touchpad_click, Some(value));
            assert!(!overrides.is_empty());
            overrides.validate().unwrap();
            assert_eq!(
                serde_json::to_value(overrides).unwrap(),
                json!({"ds4_back_as_touchpad_click":value})
            );
        }
    }

    #[test]
    fn client_video_overrides_preserve_absence_null_and_explicit_zero() {
        let absent = ClientConfigOverrides::default();
        assert!(absent.is_empty());
        assert_eq!(serde_json::to_value(&absent).unwrap(), json!({}));

        let null_values: ClientConfigOverrides = serde_json::from_value(json!({
            "max_bitrate":null,
            "minimum_fps_target":null,
            "video_max_batch_size_kb":null
        }))
        .unwrap();
        assert_eq!(null_values.max_bitrate, None);
        assert_eq!(null_values.minimum_fps_target, None);
        assert_eq!(null_values.video_max_batch_size_kb, None);
        assert!(null_values.is_empty());
        assert_eq!(serde_json::to_value(null_values).unwrap(), json!({}));

        let zero_values: ClientConfigOverrides = serde_json::from_value(json!({
            "max_bitrate":0,
            "minimum_fps_target":0.0,
            "video_max_batch_size_kb":0
        }))
        .unwrap();
        assert_eq!(zero_values.max_bitrate, Some(0));
        assert_eq!(zero_values.minimum_fps_target, Some(0.0));
        assert_eq!(zero_values.video_max_batch_size_kb, Some(0));
        assert!(!zero_values.is_empty());
        zero_values.validate().unwrap();
        assert_eq!(
            serde_json::to_value(zero_values).unwrap(),
            json!({
                "max_bitrate":0,
                "minimum_fps_target":0.0,
                "video_max_batch_size_kb":0
            })
        );
    }

    #[test]
    fn client_video_overrides_accept_boundaries_and_signed_bitrates() {
        for overrides in [
            ClientConfigOverrides {
                max_bitrate: Some(i32::MIN),
                minimum_fps_target: Some(0.0),
                video_max_batch_size_kb: Some(0),
                ..ClientConfigOverrides::default()
            },
            ClientConfigOverrides {
                max_bitrate: Some(i32::MAX),
                minimum_fps_target: Some(1000.0),
                video_max_batch_size_kb: Some(64),
                ..ClientConfigOverrides::default()
            },
        ] {
            overrides.validate().unwrap();
        }

        for overrides in [
            ClientConfigOverrides {
                minimum_fps_target: Some(-0.1),
                ..ClientConfigOverrides::default()
            },
            ClientConfigOverrides {
                minimum_fps_target: Some(1000.1),
                ..ClientConfigOverrides::default()
            },
            ClientConfigOverrides {
                minimum_fps_target: Some(f64::NAN),
                ..ClientConfigOverrides::default()
            },
            ClientConfigOverrides {
                minimum_fps_target: Some(f64::INFINITY),
                ..ClientConfigOverrides::default()
            },
        ] {
            assert_eq!(overrides.validate(), Err("invalid_config_overrides"));
        }

        let oversized_batch_size: ClientConfigOverrides =
            serde_json::from_value(json!({"video_max_batch_size_kb":65})).unwrap();
        assert_eq!(
            oversized_batch_size.validate(),
            Err("invalid_config_overrides")
        );

        for invalid in [
            json!({"max_bitrate":2147483648_u64}),
            json!({"minimum_fps_target":"60"}),
        ] {
            assert!(serde_json::from_value::<ClientConfigOverrides>(invalid).is_err());
        }
    }

    #[test]
    fn client_nvenc_spatial_aq_override_preserves_sparse_boolean_values() {
        let absent = ClientConfigOverrides::default();
        assert_eq!(absent.nvenc_spatial_aq, None);
        assert!(absent.is_empty());
        assert_eq!(serde_json::to_value(absent).unwrap(), json!({}));

        let inherited: ClientConfigOverrides = serde_json::from_value(json!({})).unwrap();
        assert_eq!(inherited.nvenc_spatial_aq, None);
        assert!(inherited.is_empty());

        let null: ClientConfigOverrides =
            serde_json::from_value(json!({"nvenc_spatial_aq":null})).unwrap();
        assert_eq!(null.nvenc_spatial_aq, None);
        assert!(null.is_empty());
        assert_eq!(serde_json::to_value(null).unwrap(), json!({}));

        for value in [false, true] {
            let overrides: ClientConfigOverrides =
                serde_json::from_value(json!({"nvenc_spatial_aq":value})).unwrap();
            assert_eq!(overrides.nvenc_spatial_aq, Some(value));
            assert!(!overrides.is_empty());
            overrides.validate().unwrap();
            assert_eq!(
                serde_json::to_value(&overrides).unwrap(),
                json!({"nvenc_spatial_aq":value})
            );
        }

        for invalid in [
            json!({"nvenc_spatial_aq":0}),
            json!({"nvenc_spatial_aq":"false"}),
            json!({"nvenc_spatial_aq":[]}),
        ] {
            assert!(serde_json::from_value::<ClientConfigOverrides>(invalid).is_err());
        }

        let client: Client = serde_json::from_value(json!({
            "certificate_id":"ab".repeat(32),
            "client_id":"paired-client",
            "name":"Paired client",
            "certificate_pem":"certificate",
            "paired_at":12,
            "config_overrides":{"nvenc_spatial_aq":false}
        }))
        .unwrap();
        assert_eq!(client.config_overrides.nvenc_spatial_aq, Some(false));
        assert_eq!(
            serde_json::to_value(client).unwrap()["config_overrides"]["nvenc_spatial_aq"],
            false
        );
    }

    #[test]
    fn client_nvenc_settings_overrides_are_sparse_bounded_and_roundtrip() {
        for value in [
            json!({"nvenc_preset":1,"nvenc_vbv_increase":0}),
            json!({"nvenc_preset":7,"nvenc_h264_cavlc":true,"nvenc_twopass":"full_res","nvenc_vbv_increase":400,"nvenc_split_encode":"disabled","nvenc_realtime_hags":false,"nvenc_opengl_vulkan_on_dxgi":false,"nvenc_latency_over_power":false,"vaapi_strict_rc_buffer":true}),
            json!({"nvenc_force_split_encode":"driver_decides"}),
        ] {
            let overrides: ClientConfigOverrides = serde_json::from_value(value.clone()).unwrap();
            assert!(overrides.validate().is_ok());
            assert!(!overrides.is_empty());
            let serialized = serde_json::to_value(overrides).unwrap();
            if value.get("nvenc_preset").is_none() {
                assert_eq!(serialized["nvenc_split_encode"], "auto");
                assert!(serialized.get("nvenc_force_split_encode").is_none());
            }
        }
        for invalid in [
            json!({"nvenc_preset":0}),
            json!({"nvenc_preset":8}),
            json!({"nvenc_vbv_increase":401}),
            json!({"nvenc_twopass":"fast"}),
            json!({"nvenc_split_encode":"forced"}),
        ] {
            if let Ok(overrides) = serde_json::from_value::<ClientConfigOverrides>(invalid) {
                assert!(overrides.validate().is_err());
            }
        }
    }

    #[test]
    fn client_frame_limiter_overrides_preserve_sparse_values_and_update_validation() {
        let inherited = ClientConfigOverrides::default();
        assert!(inherited.is_empty());
        assert_eq!(inherited.frame_limiter_fps_limit, None);
        assert_eq!(inherited.frame_limiter_enable, None);
        assert_eq!(inherited.frame_limiter_provider, None);
        assert_eq!(serde_json::to_value(&inherited).unwrap(), json!({}));

        let null_values: ClientConfigOverrides = serde_json::from_value(json!({
            "frame_limiter_fps_limit":null,
            "frame_limiter_enable":null,
            "frame_limiter_provider":null,
            "frame_limiter_disable_vsync":null,
            "frame_limiter_auto_virtual_framegen":null,
            "rtss_frame_limit_type":null,
        }))
        .unwrap();
        assert!(null_values.is_empty());
        assert_eq!(serde_json::to_value(null_values).unwrap(), json!({}));

        for one_override in [
            json!({"frame_limiter_fps_limit":0.0}),
            json!({"frame_limiter_enable":false}),
            json!({"frame_limiter_provider":"auto"}),
            json!({"frame_limiter_disable_vsync":false}),
            json!({"frame_limiter_auto_virtual_framegen":"enabled"}),
            json!({"rtss_frame_limit_type":"async"}),
        ] {
            let override_value: ClientConfigOverrides =
                serde_json::from_value(one_override.clone()).unwrap();
            assert!(!override_value.is_empty());
            assert_eq!(serde_json::to_value(override_value).unwrap(), one_override);
        }

        let override_json = json!({
            "frame_limiter_fps_limit":0.0,
            "frame_limiter_enable":false,
            "frame_limiter_provider":"auto"
        });
        #[cfg(windows)]
        let override_json = {
            let mut override_json = override_json;
            override_json["frame_limiter_disable_vsync"] = json!(false);
            override_json["frame_limiter_auto_virtual_framegen"] = json!("enabled");
            override_json["rtss_frame_limit_type"] = json!("async");
            override_json
        };

        let selected: ClientConfigOverrides =
            serde_json::from_value(override_json.clone()).unwrap();
        selected.validate().unwrap();
        assert!(!selected.is_empty());
        assert_eq!(selected.frame_limiter_fps_limit, Some(0.0));
        assert_eq!(selected.frame_limiter_enable, Some(false));
        assert_eq!(selected.frame_limiter_provider.as_deref(), Some("auto"));
        assert_eq!(serde_json::to_value(&selected).unwrap(), override_json);

        let operation: Operation = serde_json::from_value(json!({
            "action":"update_client_config_overrides",
            "certificate_id":"ab".repeat(32),
            "config_overrides":serde_json::to_value(&selected).unwrap(),
            "expected_revision":7
        }))
        .unwrap();
        operation.validate().unwrap();
    }

    #[test]
    fn legacy_client_records_load_without_frame_limiter_fields() {
        let legacy_record = json!({
            "certificate_id":"ab".repeat(32),
            "client_id":"legacy-client",
            "name":"Legacy client",
            "certificate_pem":"legacy certificate",
            "paired_at":12,
            "config_overrides":{"fec_percentage":73,"controller":false}
        });
        let legacy: Client = serde_json::from_value(legacy_record.clone()).unwrap();
        assert_eq!(legacy.config_overrides.fec_percentage, Some(73));
        assert_eq!(legacy.config_overrides.frame_limiter_fps_limit, None);
        assert_eq!(legacy.config_overrides.frame_limiter_provider, None);
        assert_eq!(serde_json::to_value(&legacy).unwrap(), legacy_record);

        let mut no_overrides = legacy_record;
        assert!(
            no_overrides
                .as_object_mut()
                .unwrap()
                .remove("config_overrides")
                .is_some()
        );
        let legacy: Client = serde_json::from_value(no_overrides.clone()).unwrap();
        assert!(legacy.config_overrides.is_empty());
        assert_eq!(serde_json::to_value(legacy).unwrap(), no_overrides);
    }

    #[test]
    fn client_frame_limiter_fps_is_decimal_hz_with_exact_millihertz_precision() {
        for fps in [0.0, 59.94, 143.997, MAX_FRAME_LIMITER_FPS] {
            let overrides: ClientConfigOverrides = serde_json::from_value(json!({
                "frame_limiter_fps_limit":fps
            }))
            .unwrap();
            overrides.validate().unwrap();
            assert_eq!(
                serde_json::to_value(&overrides).unwrap()["frame_limiter_fps_limit"],
                fps
            );
        }

        let boundary = ClientConfigOverrides {
            frame_limiter_fps_limit: Some(MAX_FRAME_LIMITER_FPS),
            ..ClientConfigOverrides::default()
        };
        boundary.validate().unwrap();

        let above_boundary = ClientConfigOverrides {
            frame_limiter_fps_limit: Some(MAX_FRAME_LIMITER_FPS + 0.001),
            ..ClientConfigOverrides::default()
        };
        assert_eq!(above_boundary.validate(), Err("invalid_config_overrides"));
        for fps in [-0.001, 1000.001, 59.9999] {
            let overrides: ClientConfigOverrides = serde_json::from_value(json!({
                "frame_limiter_fps_limit":fps
            }))
            .unwrap();
            assert_eq!(overrides.validate(), Err("invalid_config_overrides"));
        }

        assert!(
            serde_json::from_value::<ClientConfigOverrides>(json!({
                "frame_limiter_fps_limit":"60"
            }))
            .is_err()
        );
    }

    #[test]
    fn client_frame_limiter_provider_enums_and_platform_fields_are_validated() {
        #[cfg(windows)]
        let (providers, invalid_provider): (&[&str], &str) = (
            &["auto", "rtss", "nvidia-control-panel", "none"],
            "mangohud",
        );
        #[cfg(target_os = "linux")]
        let (providers, invalid_provider): (&[&str], &str) = (
            &["auto", "mangohud", "proton", "mangohud-proton", "none"],
            "rtss",
        );
        #[cfg(target_os = "macos")]
        let (providers, invalid_provider): (&[&str], &str) = (&["auto"], "rtss");
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        let (providers, invalid_provider): (&[&str], &str) = (&["auto"], "rtss");

        for provider in providers {
            let overrides = ClientConfigOverrides {
                frame_limiter_provider: Some((*provider).into()),
                ..ClientConfigOverrides::default()
            };
            overrides.validate().unwrap();
        }
        for provider in [invalid_provider, "invalid-provider"] {
            let overrides = ClientConfigOverrides {
                frame_limiter_provider: Some(provider.into()),
                ..ClientConfigOverrides::default()
            };
            assert_eq!(overrides.validate(), Err("invalid_config_overrides"));
        }

        #[cfg(windows)]
        {
            for mode in ["enabled", "vrr", "legacy", "disabled"] {
                ClientConfigOverrides {
                    frame_limiter_auto_virtual_framegen: Some(mode.into()),
                    ..ClientConfigOverrides::default()
                }
                .validate()
                .unwrap();
            }
            for mode in [
                "async",
                "front edge sync",
                "back edge sync",
                "nvidia reflex",
            ] {
                ClientConfigOverrides {
                    rtss_frame_limit_type: Some(mode.into()),
                    ..ClientConfigOverrides::default()
                }
                .validate()
                .unwrap();
            }
            assert!(
                ClientConfigOverrides {
                    frame_limiter_auto_virtual_framegen: Some("invalid".into()),
                    ..ClientConfigOverrides::default()
                }
                .validate()
                .is_err()
            );
        }

        #[cfg(target_os = "linux")]
        {
            assert!(
                ClientConfigOverrides {
                    frame_limiter_disable_vsync: Some(false),
                    ..ClientConfigOverrides::default()
                }
                .validate()
                .is_err()
            );
        }

        for invalid in [
            json!({"frame_limiter_enable":"false"}),
            json!({"frame_limiter_provider":false}),
            json!({"rtss_frame_limit_type":true}),
            json!({"frame_limiter_enable":0}),
        ] {
            assert!(serde_json::from_value::<ClientConfigOverrides>(invalid).is_err());
        }
    }

    #[test]
    fn client_audio_sink_preserves_explicit_automatic_and_rejects_controls() {
        let automatic: ClientConfigOverrides =
            serde_json::from_value(json!({"audio_sink":""})).unwrap();
        automatic.validate().unwrap();
        assert!(!automatic.is_empty());
        assert_eq!(
            serde_json::to_value(automatic).unwrap(),
            json!({"audio_sink":""})
        );
        for endpoint in ["bad\nendpoint".to_owned(), "x".repeat(4097)] {
            let override_value: ClientConfigOverrides =
                serde_json::from_value(json!({"audio_sink":endpoint})).unwrap();
            assert!(override_value.validate().is_err());
        }
    }

    #[test]
    fn client_virtual_sink_preserves_inherit_empty_and_exact_selector() {
        let inherited: ClientConfigOverrides = serde_json::from_value(json!({})).unwrap();
        assert_eq!(inherited.virtual_sink, None);
        assert!(inherited.is_empty());

        for selector in [
            "",
            "Steam Streaming Speakers",
            "{0.0.1.00000000}.{endpoint-guid}",
        ] {
            let overrides: ClientConfigOverrides =
                serde_json::from_value(json!({"virtual_sink":selector})).unwrap();
            assert_eq!(overrides.virtual_sink.as_deref(), Some(selector));
            assert!(overrides.validate().is_ok());
            assert!(!overrides.is_empty());
            assert_eq!(
                serde_json::to_value(overrides).unwrap()["virtual_sink"],
                selector
            );
        }

        for selector in ["bad\nselector", &"x".repeat(4097)] {
            let overrides: ClientConfigOverrides =
                serde_json::from_value(json!({"virtual_sink":selector})).unwrap();
            assert_eq!(overrides.validate(), Err("invalid_config_overrides"));
        }
    }

    #[test]
    fn client_audio_sink_capture_only_preserves_absence_and_explicit_false() {
        let inherited = ClientConfigOverrides::default();
        assert!(inherited.is_empty());
        assert_eq!(inherited.audio_sink_capture_only, None);

        let disabled: ClientConfigOverrides =
            serde_json::from_value(json!({"audio_sink_capture_only":false})).unwrap();
        disabled.validate().unwrap();
        assert_eq!(disabled.audio_sink_capture_only, Some(false));
        assert!(!disabled.is_empty());
        assert_eq!(
            serde_json::to_value(disabled).unwrap(),
            json!({"audio_sink_capture_only":false})
        );
        assert!(
            serde_json::from_value::<ClientConfigOverrides>(json!({
                "audio_sink_capture_only":0
            }))
            .is_err()
        );
    }

    #[test]
    fn client_back_button_timeout_preserves_absence_and_explicit_disable() {
        let inherited = ClientConfigOverrides::default();
        assert!(inherited.is_empty());
        assert_eq!(inherited.back_button_timeout, None);
        assert_eq!(serde_json::to_value(inherited).unwrap(), json!({}));

        for timeout in [-1, 0, 1250] {
            let override_value: ClientConfigOverrides = serde_json::from_value(json!({
                "back_button_timeout":timeout
            }))
            .unwrap();
            override_value.validate().unwrap();
            assert_eq!(override_value.back_button_timeout, Some(timeout));
            assert_eq!(
                serde_json::to_value(override_value).unwrap(),
                json!({"back_button_timeout":timeout})
            );
        }
        assert!(
            serde_json::from_value::<ClientConfigOverrides>(json!({
                "back_button_timeout":"1250"
            }))
            .is_err()
        );
    }

    #[test]
    fn client_input_overrides_keep_explicit_false_and_reject_wrong_platform_profiles() {
        let overrides: ClientConfigOverrides = serde_json::from_value(json!({
            "controller":false,
            "gamepad":"auto",
            "keyboard":true,
            "mouse":false
        }))
        .unwrap();
        overrides.validate().unwrap();
        assert_eq!(overrides.controller, Some(false));
        assert_eq!(overrides.gamepad.as_deref(), Some("auto"));
        assert_eq!(overrides.keyboard, Some(true));
        assert_eq!(overrides.mouse, Some(false));
        assert_eq!(
            serde_json::to_value(overrides).unwrap(),
            json!({
                "controller":false,
                "gamepad":"auto",
                "keyboard":true,
                "mouse":false
            })
        );

        #[cfg(windows)]
        let invalid_profile = "xone";
        #[cfg(target_os = "linux")]
        let invalid_profile = "vhf";
        #[cfg(not(any(windows, target_os = "linux")))]
        let invalid_profile = "ds4";
        for value in [
            json!({"controller":"false"}),
            json!({"gamepad":invalid_profile}),
            json!({"keyboard":0}),
            json!({"mouse":[]} ),
        ] {
            if let Ok(invalid) = serde_json::from_value::<ClientConfigOverrides>(value) {
                assert!(invalid.validate().is_err());
            }
        }
    }

    #[test]
    fn client_keyboard_policy_overrides_are_sparse_and_strictly_bounded() {
        let inherited = ClientConfigOverrides::default();
        assert!(inherited.is_empty());
        assert_eq!(inherited.key_repeat_delay, None);
        assert_eq!(inherited.key_repeat_frequency, None);
        assert_eq!(serde_json::to_value(inherited).unwrap(), json!({}));

        let selected: ClientConfigOverrides = serde_json::from_value(json!({
            "high_resolution_scrolling":false,
            "always_send_scancodes":false,
            "key_rightalt_to_key_win":true,
            "key_repeat_delay":0,
            "key_repeat_frequency":24.9
        }))
        .unwrap();
        selected.validate().unwrap();
        assert_eq!(selected.high_resolution_scrolling, Some(false));
        assert_eq!(selected.always_send_scancodes, Some(false));
        assert_eq!(selected.key_rightalt_to_key_win, Some(true));
        assert_eq!(selected.key_repeat_delay, Some(0));
        assert_eq!(selected.key_repeat_frequency, Some(24.9));
        assert_eq!(
            serde_json::to_value(selected).unwrap(),
            json!({
                "high_resolution_scrolling":false,
                "always_send_scancodes":false,
                "key_rightalt_to_key_win":true,
                "key_repeat_delay":0,
                "key_repeat_frequency":24.9
            })
        );

        for invalid in [
            json!({"key_repeat_delay": i32::MAX as u64 + 1}),
            json!({"key_repeat_frequency": -0.1}),
        ] {
            let overrides: ClientConfigOverrides = serde_json::from_value(invalid).unwrap();
            assert_eq!(overrides.validate(), Err("invalid_config_overrides"));
        }
        for invalid in [
            json!({"high_resolution_scrolling": 1}),
            json!({"key_repeat_delay": 1.5}),
            json!({"key_repeat_frequency": "24.9"}),
        ] {
            assert!(serde_json::from_value::<ClientConfigOverrides>(invalid).is_err());
        }
    }

    #[test]
    fn client_display_policy_roundtrips_sparse_values_and_rejects_malformed_rules() {
        let absent: ClientConfigOverrides = serde_json::from_value(json!({})).unwrap();
        let inherited: ClientConfigOverrides = serde_json::from_value(json!({
            "dd_mode_remapping":null,
            "dd_refresh_rate_option":null,
            "dd_configuration_option":null
        }))
        .unwrap();
        assert!(absent.is_empty());
        assert!(inherited.is_empty());
        assert_eq!(serde_json::to_value(inherited).unwrap(), json!({}));

        let selected = json!({
            "dd_mode_remapping":{"mixed":[{
                "requested_resolution":"1920x1080",
                "requested_fps":"60",
                "final_resolution":"1280x720",
                "final_refresh_rate":"59.94"
            }]},
            "dd_resolution_option":"manual",
            "dd_manual_resolution":"1920x1080",
            "dd_refresh_rate_option":"manual",
            "dd_manual_refresh_rate":"59.94",
            "dd_hdr_option":"disabled",
            "dd_configuration_option":"ensure_active",
            "dd_hdr_request_override":"force_off"
        });
        let overrides: ClientConfigOverrides = serde_json::from_value(selected.clone()).unwrap();
        overrides.validate().unwrap();
        assert!(!overrides.is_empty());
        assert_eq!(serde_json::to_value(overrides).unwrap(), selected);

        for invalid in [
            json!({"dd_mode_remapping":[]}),
            json!({"dd_mode_remapping":{"unknown":[]}}),
            json!({"dd_mode_remapping":{"mixed":[{"requested_fps":60}]}}),
            json!({"dd_mode_remapping":{"mixed":[{"final_resolution":"bad\nvalue"}]}}),
            json!({"dd_mode_remapping":{"mixed":[{}]}}),
            json!({"dd_mode_remapping":{"mixed":[{"requested_fps":"59.94","final_resolution":"1280x720"}]}}),
            json!({"dd_mode_remapping":{"resolution_only":[{"final_refresh_rate":"120"}]}}),
            json!({"dd_mode_remapping":{"refresh_rate_only":[{"final_refresh_rate":"120","requested_resolution":"1920x1080"}]}}),
            json!({"dd_mode_remapping":{"mixed":[{"final_resolution":"0x720"}]}}),
            json!({"dd_resolution_option":"highest"}),
            json!({"dd_manual_resolution":"1920x"}),
            json!({"dd_refresh_rate_option":"unknown"}),
            json!({"dd_manual_refresh_rate":"NaN"}),
            json!({"dd_manual_refresh_rate":"0.0000000001"}),
            json!({"dd_hdr_option":"force_on"}),
            json!({"dd_configuration_option":"unknown"}),
            json!({"dd_hdr_request_override":"disabled"}),
        ] {
            let decoded: ClientConfigOverrides = serde_json::from_value(invalid).unwrap();
            assert_eq!(decoded.validate(), Err("invalid_config_overrides"));
        }
        for invalid in [
            json!({"dd_refresh_rate_option":false}),
            json!({"dd_configuration_option":{}}),
        ] {
            assert!(serde_json::from_value::<ClientConfigOverrides>(invalid).is_err());
        }
        let zero_resolution: ClientConfigOverrides =
            serde_json::from_value(json!({"dd_manual_resolution":"1920x0"})).unwrap();
        zero_resolution.validate().unwrap();
        let spaced_resolution: ClientConfigOverrides =
            serde_json::from_value(json!({"dd_manual_resolution":" 1920×1080 "})).unwrap();
        spaced_resolution.validate().unwrap();
    }

    #[test]
    fn paired_client_display_mode_keeps_exact_physical_refresh() {
        let inherited: ClientConfigOverrides =
            serde_json::from_value(json!({"display_mode":null})).unwrap();
        inherited.validate().unwrap();
        assert!(inherited.is_empty());
        assert_eq!(serde_json::to_value(inherited).unwrap(), json!({}));

        for (mode, expected_millihertz) in [
            ("1920x1080x59.94", 59_940),
            ("1920x1080x59.004", 59_004),
            ("1920x1080x60", 60_000),
            ("1920x1080x60000", 60_000),
        ] {
            let selected: ClientConfigOverrides =
                serde_json::from_value(json!({"display_mode":mode})).unwrap();
            selected.validate().unwrap();
            assert!(!selected.is_empty());
            assert_eq!(
                parse_client_display_mode(mode).unwrap().2,
                expected_millihertz
            );
            assert_eq!(
                serde_json::to_value(selected).unwrap(),
                json!({"display_mode":mode})
            );
        }
        for mode in [
            "",
            "0x1080x60",
            "1920x0x60",
            "1920x1080x0",
            "1920x1080x9.999",
            "1920x1080x1000.001",
            "1920x1080x59.1234",
            "1920x1080x59.",
            "1920x1080x.5",
            "1920x1080x+60",
            "1920x1080x60x1",
            "1920×1080×60",
        ] {
            let selected: ClientConfigOverrides =
                serde_json::from_value(json!({"display_mode":mode})).unwrap();
            assert_eq!(
                selected.validate(),
                Err("invalid_config_overrides"),
                "{mode}"
            );
        }
    }
}

#[cfg(test)]
mod software_encoder_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn software_and_qp_fields_are_sparse_exact_and_signed() {
        let absent: ClientConfigOverrides = serde_json::from_value(
            json!({"sw_preset":null,"sw_tune":null,"encoder":null,"min_threads":null,"qp":null}),
        )
        .unwrap();
        assert!(absent.is_empty());
        let selected: ClientConfigOverrides = serde_json::from_value(
            json!({"sw_preset":"","sw_tune":"film,zerolatency","encoder":"software","qp":i32::MIN}),
        )
        .unwrap();
        assert!(selected.validate().is_ok());
        assert!(!selected.is_empty());
        assert_eq!(
            serde_json::to_value(selected).unwrap(),
            json!({"sw_preset":"","sw_tune":"film,zerolatency","encoder":"software","qp":i32::MIN})
        );
        for invalid in [
            json!({"sw_preset":false}),
            json!({"encoder":"nvenc"}),
            json!({"encoder":""}),
        ] {
            assert!(serde_json::from_value::<ClientConfigOverrides>(invalid).is_err());
        }
        for invalid in [
            json!({"sw_preset":"x".repeat(129)}),
            json!({"sw_tune":"film\n"}),
        ] {
            let options: ClientConfigOverrides = serde_json::from_value(invalid).unwrap();
            assert!(options.validate().is_err());
        }
        for threads in [i32::MIN, -1, 0, 1, 2, i32::MAX] {
            let options: ClientConfigOverrides =
                serde_json::from_value(json!({"min_threads":threads})).unwrap();
            assert!(options.validate().is_ok());
            assert_eq!(
                serde_json::to_value(options).unwrap(),
                json!({"min_threads":threads})
            );
        }
        for key in ["min_threads", "qp"] {
            for value in [i64::from(i32::MIN) - 1, i64::from(i32::MAX) + 1] {
                assert!(
                    serde_json::from_value::<ClientConfigOverrides>(json!({key:value})).is_err()
                );
            }
        }
    }
}

#[cfg(test)]
mod keybindings_override_tests {
    use super::ClientConfigOverrides;
    use serde_json::{Value, json};

    #[test]
    fn ordered_keybindings_are_sparse_and_explicit_empty_replaces_inheritance() {
        for inherited in [json!({}), json!({"keybindings":null})] {
            let overrides: ClientConfigOverrides = serde_json::from_value(inherited).unwrap();
            assert!(overrides.keybindings.is_none());
            assert!(overrides.is_empty());
            assert_eq!(serde_json::to_value(overrides).unwrap(), json!({}));
        }
        for list in [
            json!([]),
            json!([0, 255, 65, 66, 65, 67]),
            json!(vec![0_u8; 512]),
        ] {
            let value = json!({"keybindings":list});
            let overrides: ClientConfigOverrides = serde_json::from_value(value.clone()).unwrap();
            assert!(overrides.validate().is_ok());
            assert!(!overrides.is_empty());
            assert_eq!(serde_json::to_value(overrides).unwrap(), value);
        }
    }

    #[test]
    fn malformed_keybindings_reject_exact_types_byte_bounds_and_pair_count() {
        for invalid in [
            json!([65]),
            json!([-1, 0]),
            json!([0, 256]),
            json!([65.0, 66]),
            json!([true, 66]),
            json!(["65", 66]),
            json!([[65, 66]]),
            json!({"65":66}),
            json!("65,66"),
            json!(vec![0_u8; 514]),
        ] {
            assert!(
                serde_json::from_value::<ClientConfigOverrides>(json!({"keybindings":invalid}))
                    .is_err(),
                "{invalid}"
            );
        }
        assert!(serde_json::from_value::<ClientConfigOverrides>(Value::Null).is_err());
    }
}
