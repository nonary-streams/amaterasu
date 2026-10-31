use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const MAX_ENTRIES: usize = 100_000;
const MAX_PREPARATION_STEPS: usize = 8;
const MAX_RAW_PREPARATION_COMMAND_BYTES: usize = 2_048;
const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 10_000;
const MAX_COMMAND_TIMEOUT_MS: u64 = 30_000;
const MAX_LAUNCH_WORKING_DIRECTORY_BYTES: usize = 1_024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artwork: Option<ArtworkReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steam_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steam_install_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Optional app-specific display selection. This is separate from
    /// `output`, which routes command logs.
    #[serde(
        default,
        rename = "display-output",
        skip_serializing_if = "Option::is_none",
        alias = "display_output"
    )]
    pub display_output: Option<String>,
    /// Optional app-specific request for a virtual display. Absence leaves
    /// the effective host display policy untouched.
    #[serde(
        default,
        rename = "virtual-screen",
        skip_serializing_if = "Option::is_none",
        alias = "virtual_screen"
    )]
    pub virtual_screen: Option<bool>,
    /// Optional app-specific virtual display mode. Absence inherits the host
    /// policy; `disabled` is an explicit per-application override.
    #[serde(
        default,
        rename = "virtual-display-mode",
        skip_serializing_if = "Option::is_none",
        alias = "virtual_display_mode"
    )]
    pub virtual_display_mode: Option<String>,
    /// Optional app-specific virtual display layout. Absence inherits the
    /// host policy.
    #[serde(
        default,
        rename = "virtual-display-layout",
        skip_serializing_if = "Option::is_none",
        alias = "virtual_display_layout"
    )]
    pub virtual_display_layout: Option<String>,
    #[serde(default, skip_serializing_if = "ConfigOverrides::is_empty")]
    pub config_overrides: ConfigOverrides,
    #[serde(
        default,
        skip_serializing_if = "is_false",
        alias = "exclude-global-prep-cmd"
    )]
    pub exclude_global_prep_cmd: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty", alias = "prep-cmd")]
    pub prep_cmd: Vec<RawPreparationCommand>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub elevated: bool,
    pub launch: Launch,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawPreparationCommand {
    #[serde(default, rename = "do")]
    pub do_command: String,
    #[serde(default)]
    pub undo: String,
    #[serde(default)]
    pub elevated: bool,
}

/// An opaque provider-owned image reference. The source path is never stored in
/// the catalog or exposed through the catalog API.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtworkReference {
    Steam { app_id: u32, revision: String },
    Lutris { game_id: i64, revision: String },
    Playnite { game_id: String, revision: String },
}

impl ArtworkReference {
    fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Steam { app_id, revision } => {
                let parsed_revision = revision.parse::<u64>().ok();
                if *app_id == 0
                    || revision.is_empty()
                    || revision.len() > 20
                    || !revision.bytes().all(|byte| byte.is_ascii_digit())
                    || parsed_revision.is_none_or(|number| number.to_string() != *revision)
                {
                    return Err("invalid Steam artwork reference");
                }
            }
            Self::Playnite { game_id, revision } => {
                if !valid_guid(game_id)
                    || game_id.bytes().any(|byte| byte.is_ascii_uppercase())
                    || revision.len() != 64
                    || !revision
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err("invalid Playnite artwork reference");
                }
            }
            Self::Lutris { game_id, revision } => {
                if *game_id <= 0
                    || revision.len() != 64
                    || !revision
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err("invalid Lutris artwork reference");
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigOverrides {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fec_percentage: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bitrate: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_fps_target: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_max_batch_size_kb: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<String>,
    /// Sparse stream-scoped display policy additions. The legacy fields below
    /// remain flat so existing catalog clients can continue to read them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_policy: Option<serde_json::Value>,
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
    pub stream_audio: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virtual_sink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sink_capture_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back_button_timeout: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamepad: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse: Option<bool>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amd_av1_latency_mode: Option<String>,
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
    pub prefer_10bit_sdr: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_enable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_fps_limit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_disable_vsync: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_auto_virtual_framegen"
    )]
    pub frame_limiter_auto_virtual_framegen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lossless_scaling_framegen: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lossless_scaling_target_fps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_generation_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_generation_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtss_frame_limit_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ds4_back_as_touchpad_click: Option<bool>,
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
}

fn deserialize_auto_virtual_framegen<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(serde_json::Value::Bool(enabled)) => Ok(Some(if enabled {
            "enabled".into()
        } else {
            "disabled".into()
        })),
        Some(serde_json::Value::String(value)) if value == "true" => Ok(Some("enabled".into())),
        Some(serde_json::Value::String(value)) if value == "false" => Ok(Some("disabled".into())),
        Some(serde_json::Value::String(value))
            if matches!(value.as_str(), "enabled" | "vrr" | "legacy" | "disabled") =>
        {
            Ok(Some(value))
        }
        _ => Err(serde::de::Error::custom(
            "frame_limiter_auto_virtual_framegen must be enabled, vrr, legacy, or disabled",
        )),
    }
}

impl ConfigOverrides {
    pub fn validate(&self) -> Result<(), &'static str> {
        amaterasu_module_sdk::validate_software_options(
            self.sw_preset.as_deref(),
            self.sw_tune.as_deref(),
        )
        .map_err(|_| "software encoder options are invalid")?;
        amaterasu_module_sdk::validate_min_threads(self.min_threads)
            .map_err(|_| "software encoder thread request is invalid")?;
        amaterasu_module_sdk::NvencSettingsOverrides {
            nvenc_preset: self.nvenc_preset,
            nvenc_h264_cavlc: self.nvenc_h264_cavlc,
            nvenc_twopass: self.nvenc_twopass,
            nvenc_vbv_increase: self.nvenc_vbv_increase,
            nvenc_split_encode: self.nvenc_split_encode,
            nvenc_realtime_hags: self.nvenc_realtime_hags,
            nvenc_opengl_vulkan_on_dxgi: self.nvenc_opengl_vulkan_on_dxgi,
            nvenc_latency_over_power: self.nvenc_latency_over_power,
            vaapi_strict_rc_buffer: self.vaapi_strict_rc_buffer,
        }
        .validate()
        .map_err(|_| "NVENC settings override is invalid")?;
        if self
            .key_repeat_delay
            .is_some_and(|delay| delay > i32::MAX as u32)
        {
            return Err("key_repeat_delay must be between 0 and 2147483647 milliseconds");
        }
        if self
            .key_repeat_frequency
            .is_some_and(|frequency| !frequency.is_finite() || frequency < 0.0)
        {
            return Err("key_repeat_frequency must be finite and nonnegative");
        }
        if self
            .fec_percentage
            .is_some_and(|percentage| percentage == 0)
        {
            return Err("fec_percentage must be between 1 and 255");
        }
        if self
            .minimum_fps_target
            .is_some_and(|target| !target.is_finite() || !(0.0..=1000.0).contains(&target))
        {
            return Err("minimum_fps_target must be finite and between 0 and 1000");
        }
        if self
            .dd_refresh_rate_option
            .as_deref()
            .is_some_and(|option| {
                !matches!(option, "auto" | "disabled" | "manual" | "prefer_highest")
            })
        {
            return Err("dd_refresh_rate_option must be auto, disabled, manual, or prefer_highest");
        }
        if !valid_display_overrides(
            self.dd_mode_remapping.as_ref(),
            self.dd_resolution_option.as_deref(),
            self.dd_manual_resolution.as_deref(),
            self.dd_manual_refresh_rate.as_deref(),
            self.dd_hdr_option.as_deref(),
            self.dd_configuration_option.as_deref(),
        ) {
            return Err("invalid application display override");
        }
        if self
            .dd_hdr_request_override
            .as_deref()
            .is_some_and(|mode| !matches!(mode, "auto" | "force_on" | "force_off"))
        {
            return Err("dd_hdr_request_override must be auto, force_on, or force_off");
        }
        if self
            .frame_limiter_fps_limit
            .is_some_and(|limit| !valid_millihertz_value(limit))
        {
            return Err("frame_limiter_fps_limit must be 0..=1000 with at most 3 decimals");
        }
        if self
            .lossless_scaling_target_fps
            .is_some_and(|fps| !(1..=1000).contains(&fps))
        {
            return Err("lossless_scaling_target_fps must be between 1 and 1000");
        }
        if self
            .frame_limiter_provider
            .as_deref()
            .is_some_and(|provider| !valid_frame_limiter_provider(provider))
        {
            return Err("frame_limiter_provider is not available on this platform");
        }
        if self
            .frame_limiter_auto_virtual_framegen
            .as_deref()
            .is_some_and(|mode| !matches!(mode, "enabled" | "vrr" | "legacy" | "disabled"))
        {
            return Err("frame_limiter_auto_virtual_framegen has an invalid mode");
        }
        if self
            .frame_generation_provider
            .as_deref()
            .is_some_and(|provider| !valid_frame_generation_provider(provider))
            || self
                .frame_generation_mode
                .as_deref()
                .is_some_and(|mode| !valid_frame_generation_mode(mode))
        {
            return Err("frame generation provider or mode is invalid");
        }
        if self
            .rtss_frame_limit_type
            .as_deref()
            .is_some_and(|limit_type| {
                !cfg!(windows)
                    || !matches!(
                        limit_type,
                        "async" | "front edge sync" | "back edge sync" | "nvidia reflex"
                    )
            })
        {
            return Err("rtss_frame_limit_type is a Windows RTSS setting");
        }
        if self.video_max_batch_size_kb.is_some_and(|size| size > 64) {
            return Err("video_max_batch_size_kb must be between 0 and 64");
        }
        if self.capture.as_deref().is_some_and(|capture| {
            !matches!(
                capture,
                "auto" | "ddx" | "wgc" | "wgcc" | "pipewire" | "x11" | "kms"
            )
        }) {
            return Err("capture must select a supported provider");
        }
        if self
            .display_policy
            .as_ref()
            .is_some_and(|policy| !valid_display_policy_override(policy))
        {
            return Err("display_policy contains an invalid or unsupported override");
        }
        if self
            .audio_sink
            .as_deref()
            .is_some_and(|endpoint| endpoint.len() > 4096 || endpoint.chars().any(char::is_control))
        {
            return Err("audio_sink must be a bounded endpoint ID without controls");
        }
        if self
            .virtual_sink
            .as_deref()
            .is_some_and(|selector| selector.len() > 4096 || selector.chars().any(char::is_control))
        {
            return Err("virtual_sink must be a bounded device selector without controls");
        }
        if self
            .gamepad
            .as_deref()
            .is_some_and(|profile| profile.len() > 32 || !valid_gamepad_profile(profile))
        {
            return Err("gamepad must be a recognized profile for this platform");
        }
        if self.amd_av1_latency_mode.as_deref().is_some_and(|mode| {
            !matches!(
                mode,
                "auto" | "none" | "power_saving" | "realtime" | "lowest"
            )
        }) {
            return Err("amd_av1_latency_mode has an invalid mode");
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.sw_preset.is_none()
            && self.sw_tune.is_none()
            && self.encoder.is_none()
            && self.min_threads.is_none()
            && self.qp.is_none()
            && self.fec_percentage.is_none()
            && self.max_bitrate.is_none()
            && self.minimum_fps_target.is_none()
            && self.video_max_batch_size_kb.is_none()
            && self.capture.is_none()
            && self.display_policy.is_none()
            && self.dd_mode_remapping.is_none()
            && self.dd_resolution_option.is_none()
            && self.dd_manual_resolution.is_none()
            && self.dd_refresh_rate_option.is_none()
            && self.dd_manual_refresh_rate.is_none()
            && self.dd_hdr_option.is_none()
            && self.dd_configuration_option.is_none()
            && self.dd_hdr_request_override.is_none()
            && self.stream_audio.is_none()
            && self.audio_sink.is_none()
            && self.virtual_sink.is_none()
            && self.audio_sink_capture_only.is_none()
            && self.back_button_timeout.is_none()
            && self.controller.is_none()
            && self.gamepad.is_none()
            && self.keyboard.is_none()
            && self.mouse.is_none()
            && self.high_resolution_scrolling.is_none()
            && self.always_send_scancodes.is_none()
            && self.key_rightalt_to_key_win.is_none()
            && self.keybindings.is_none()
            && self.key_repeat_delay.is_none()
            && self.key_repeat_frequency.is_none()
            && self.amd_av1_latency_mode.is_none()
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
            && self.prefer_10bit_sdr.is_none()
            && self.frame_limiter_enable.is_none()
            && self.frame_limiter_fps_limit.is_none()
            && self.frame_limiter_provider.is_none()
            && self.frame_limiter_disable_vsync.is_none()
            && self.frame_limiter_auto_virtual_framegen.is_none()
            && self.lossless_scaling_framegen.is_none()
            && self.lossless_scaling_target_fps.is_none()
            && self.frame_generation_provider.is_none()
            && self.frame_generation_mode.is_none()
            && self.rtss_frame_limit_type.is_none()
            && self.ds4_back_as_touchpad_click.is_none()
            && self.native_pen_touch.is_none()
            && self.motion_as_ds4.is_none()
            && self.touchpad_as_ds4.is_none()
            && self.ds5_inputtino_randomize_mac.is_none()
            && self.proton_dualsense_compatibility.is_none()
    }
}

fn valid_frame_generation_provider(provider: &str) -> bool {
    matches!(
        provider,
        "lossless-scaling" | "game-provided" | "nvidia-smooth-motion"
    )
}

fn valid_frame_generation_mode(mode: &str) -> bool {
    mode == "off" || valid_frame_generation_provider(mode)
}

fn valid_display_overrides(
    remapping: Option<&serde_json::Value>,
    resolution_option: Option<&str>,
    manual_resolution: Option<&str>,
    manual_refresh_rate: Option<&str>,
    hdr_option: Option<&str>,
    configuration_option: Option<&str>,
) -> bool {
    if resolution_option.is_some_and(|value| !matches!(value, "auto" | "disabled" | "manual"))
        || hdr_option.is_some_and(|value| !matches!(value, "auto" | "disabled"))
        || configuration_option.is_some_and(|value| {
            !matches!(
                value,
                "disabled"
                    | "verify_only"
                    | "ensure_active"
                    | "ensure_primary"
                    | "ensure_only_display"
            )
        })
        || manual_resolution.is_some_and(|value| {
            if value.len() > 32 {
                return true;
            }
            let value = value.trim();
            if value.is_empty() {
                return false;
            }
            value.split_once(['x', '×']).is_none_or(|(width, height)| {
                width.parse::<u32>().is_err() || height.parse::<u32>().is_err()
            })
        })
        || manual_refresh_rate.is_some_and(|value| !valid_manual_refresh_rate(value))
    {
        return false;
    }
    let Some(remapping) = remapping else {
        return true;
    };
    let Some(groups) = remapping.as_object() else {
        return false;
    };
    if groups.len() > 3
        || groups.keys().any(|key| {
            !matches!(
                key.as_str(),
                "mixed" | "resolution_only" | "refresh_rate_only"
            )
        })
    {
        return false;
    }
    groups.iter().all(|(kind, group)| {
        group.as_array().is_some_and(|entries| {
            entries.len() <= 128
                && entries.iter().all(|entry| {
                    entry.as_object().is_some_and(|fields| {
                        fields.len() <= 4
                            && fields.iter().all(|(key, value)| {
                                matches!(
                                    key.as_str(),
                                    "requested_resolution"
                                        | "requested_fps"
                                        | "final_resolution"
                                        | "final_refresh_rate"
                                ) && value.as_str().is_some_and(|text| {
                                    text.len() <= 64 && !text.chars().any(char::is_control)
                                })
                            })
                            && valid_remapping_entry(kind, fields)
                    })
                })
        })
    })
}

fn valid_display_policy_override(policy: &serde_json::Value) -> bool {
    const ALLOWED: &[&str] = &[
        "adapter_name",
        "adapter_pnp_id",
        "virtual_display_mode",
        "virtual_display_layout",
        "dd_mode_remapping",
        "dd_resolution_option",
        "dd_manual_resolution",
        "dd_refresh_rate_option",
        "dd_manual_refresh_rate",
        "dd_hdr_option",
        "dd_configuration_option",
        "dd_hdr_request_override",
        "dd_config_revert_delay",
        "dd_config_revert_on_disconnect",
        "dd_paused_virtual_display_timeout_secs",
        "dd_always_restore_from_golden",
        "dd_display_helper_engine",
        "dd_snapshot_restore_hotkey",
        "dd_snapshot_restore_hotkey_modifiers",
        "dd_use_sunshine_virtual_display_driver",
        "dd_activate_virtual_display",
        "dd_virtual_display_scale",
        "dd_virtual_display_permanent_count",
        "virtual_display_outputs",
        "dd_snapshot_exclude_devices",
        "dd_wa_dummy_plug_hdr10",
    ];
    let Some(fields) = policy.as_object() else {
        return false;
    };
    if fields.len() > ALLOWED.len() || fields.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        return false;
    }
    fields.iter().all(|(key, value)| {
        if value.is_null() {
            return true;
        }
        match key.as_str() {
            "adapter_name" => value.as_str().is_some_and(|text| bounded_text(text, 1024)),
            "adapter_pnp_id" => value.as_str().is_some_and(|text| bounded_text(text, 256)),
            "virtual_display_mode" => value
                .as_str()
                .is_some_and(|mode| matches!(mode, "disabled" | "per_client" | "shared")),
            "virtual_display_layout" => value.as_str().is_some_and(|layout| {
                matches!(
                    layout,
                    "exclusive"
                        | "extended"
                        | "extended_primary"
                        | "extended_isolated"
                        | "extended_primary_isolated"
                )
            }),
            "dd_mode_remapping" => {
                valid_display_overrides(Some(value), None, None, None, None, None)
            }
            "dd_resolution_option" => value
                .as_str()
                .is_some_and(|option| matches!(option, "auto" | "disabled" | "manual")),
            "dd_manual_resolution" => value.as_str().is_some_and(|resolution| {
                bounded_text(resolution, 32)
                    && (resolution.is_empty() || valid_remapping_resolution(resolution))
            }),
            "dd_refresh_rate_option" => value.as_str().is_some_and(|option| {
                matches!(option, "auto" | "disabled" | "manual" | "prefer_highest")
            }),
            "dd_manual_refresh_rate" => value.as_str().is_some_and(|rate| {
                bounded_text(rate, 64) && (rate.is_empty() || valid_manual_refresh_rate(rate))
            }),
            "dd_hdr_option" => value
                .as_str()
                .is_some_and(|option| matches!(option, "auto" | "disabled")),
            "dd_configuration_option" => value.as_str().is_some_and(|option| {
                matches!(
                    option,
                    "disabled"
                        | "verify_only"
                        | "ensure_active"
                        | "ensure_primary"
                        | "ensure_only_display"
                )
            }),
            "dd_hdr_request_override" => value
                .as_str()
                .is_some_and(|mode| matches!(mode, "auto" | "force_on" | "force_off")),
            "dd_config_revert_delay" | "dd_paused_virtual_display_timeout_secs" => value
                .as_u64()
                .is_some_and(|number| number <= i32::MAX as u64),
            "dd_config_revert_on_disconnect"
            | "dd_always_restore_from_golden"
            | "dd_use_sunshine_virtual_display_driver"
            | "dd_activate_virtual_display"
            | "dd_wa_dummy_plug_hdr10" => value.is_boolean(),
            "dd_display_helper_engine" => value
                .as_str()
                .is_some_and(|engine| matches!(engine, "auto" | "v2" | "legacy")),
            "dd_snapshot_restore_hotkey" => value
                .as_str()
                .is_some_and(|hotkey| hotkey.len() <= 64 && !hotkey.chars().any(char::is_control)),
            "dd_snapshot_restore_hotkey_modifiers" => value.as_str().is_some_and(|modifiers| {
                modifiers.len() <= 128 && !modifiers.chars().any(char::is_control)
            }),
            "dd_virtual_display_scale" => value.as_i64().is_some_and(|scale| {
                matches!(
                    scale,
                    -1 | 0 | 100 | 125 | 150 | 175 | 200 | 225 | 250 | 300 | 350 | 400 | 450 | 500
                )
            }),
            "dd_virtual_display_permanent_count" => value.as_u64().is_some_and(|count| count <= 4),
            "virtual_display_outputs" => valid_string_array(value, 32, 512),
            "dd_snapshot_exclude_devices" => valid_string_array(value, 256, 1024),
            _ => false,
        }
    })
}

fn valid_string_array(value: &serde_json::Value, count: usize, item_bytes: usize) -> bool {
    value.as_array().is_some_and(|items| {
        items.len() <= count
            && items.iter().all(|item| {
                item.as_str()
                    .is_some_and(|text| !text.is_empty() && bounded_text(text, item_bytes))
            })
    })
}

fn valid_remapping_entry(kind: &str, fields: &serde_json::Map<String, serde_json::Value>) -> bool {
    let field = |name| {
        fields
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let final_resolution = field("final_resolution");
    let final_refresh_rate = field("final_refresh_rate");
    valid_remapping_resolution(field("requested_resolution"))
        && valid_remapping_resolution(final_resolution)
        && valid_remapping_rate(field("requested_fps"), false)
        && valid_remapping_rate(final_refresh_rate, true)
        && match kind {
            "mixed" => !final_resolution.trim().is_empty() || !final_refresh_rate.trim().is_empty(),
            "resolution_only" => !final_resolution.trim().is_empty(),
            "refresh_rate_only" => !final_refresh_rate.trim().is_empty(),
            _ => false,
        }
}

fn valid_remapping_resolution(value: &str) -> bool {
    let value = value.trim().replace('×', "x");
    if value.is_empty() {
        return true;
    }
    value.split_once('x').is_some_and(|(width, height)| {
        !width.is_empty()
            && !height.is_empty()
            && width.bytes().all(|byte| byte.is_ascii_digit())
            && height.bytes().all(|byte| byte.is_ascii_digit())
            && width.parse::<u32>().is_ok()
            && height.parse::<u32>().is_ok()
    })
}

fn valid_remapping_rate(value: &str, allow_fraction: bool) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    let Some((whole, fraction)) = value.split_once('.') else {
        return value.bytes().all(|byte| byte.is_ascii_digit()) && value.parse::<u32>().is_ok();
    };
    if !allow_fraction
        || whole.is_empty()
        || fraction.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        return whole.parse::<u32>().is_ok();
    }
    format!("{whole}{fraction}").parse::<u32>().is_ok()
        && u32::try_from(fraction.len())
            .ok()
            .and_then(|length| 10_u32.checked_pow(length))
            .is_some()
}

fn valid_manual_refresh_rate(value: &str) -> bool {
    if value.len() > 64 {
        return false;
    }
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || (!fraction.is_empty() && !fraction.bytes().all(|byte| byte.is_ascii_digit()))
        || (value.contains('.') && fraction.is_empty())
    {
        return false;
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let Ok(integer) = whole.parse::<u32>() else {
        return false;
    };
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        return true;
    }
    if fraction.len() > 31 {
        return false;
    }
    let (Ok(numerator), Some(denominator)) = (
        fraction.parse::<u128>(),
        10_u128.checked_pow(fraction.len() as u32),
    ) else {
        return false;
    };
    let mut left = numerator;
    let mut right = denominator;
    while right != 0 {
        (left, right) = (right, left % right);
    }
    let numerator = numerator / left;
    let denominator = denominator / left;
    u32::try_from(denominator).is_ok()
        && u128::from(integer)
            .checked_mul(denominator)
            .and_then(|scaled| scaled.checked_add(numerator))
            .is_some_and(|combined| u32::try_from(combined).is_ok())
}

fn valid_millihertz_value(value: f64) -> bool {
    if !value.is_finite() || !(0.0..=1000.0).contains(&value) {
        return false;
    }
    let millihertz = value * 1000.0;
    let rounding_tolerance = f64::EPSILON * millihertz.abs().max(1.0) * 4.0;
    (millihertz - millihertz.round()).abs() <= rounding_tolerance
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Launch {
    Executable {
        program: String,
        #[serde(default)]
        arguments: Vec<String>,
        // Accept the sibling's per-application spelling but store one canonical field.
        #[serde(
            default,
            alias = "working-dir",
            deserialize_with = "empty_working_directory_as_none"
        )]
        working_directory: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            alias = "auto-detach"
        )]
        auto_detach: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none", alias = "wait-all")]
        wait_all: Option<bool>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            alias = "exit-timeout"
        )]
        exit_timeout_seconds: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        detached: Vec<DetachedCommand>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        preparation: Vec<PreparationStep>,
    },
    Application {
        path: String,
    },
    /// A launch target owned by the trusted Playnite provider. The game ID is
    /// resolved by Playnite's authenticated extension IPC, never by a shell.
    Playnite {
        game_id: String,
        #[serde(default = "default_playnite_exit_timeout_seconds")]
        exit_timeout_seconds: u64,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            alias = "working-dir",
            deserialize_with = "empty_working_directory_as_none"
        )]
        working_directory: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        preparation: Vec<PreparationStep>,
    },
    /// A manual Playnite Fullscreen launch. The authenticated Playnite provider
    /// resolves the installation and process; catalog callers cannot supply a
    /// path, URI, or game identifier.
    PlayniteFullscreen {
        #[serde(default = "default_playnite_fullscreen_exit_timeout_seconds")]
        exit_timeout_seconds: u64,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            alias = "working-dir",
            deserialize_with = "empty_working_directory_as_none"
        )]
        working_directory: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        preparation: Vec<PreparationStep>,
    },
    /// A process-less desktop session. It names no program, path, or provider
    /// identity: launch only prepares the host, holds the session, and undoes
    /// the preparation when the client leaves.
    Desktop {
        #[serde(default = "default_desktop_exit_timeout_seconds")]
        exit_timeout_seconds: u64,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            alias = "working-dir",
            deserialize_with = "empty_working_directory_as_none"
        )]
        working_directory: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        preparation: Vec<PreparationStep>,
    },
}

/// Keep legacy command strings and per-command settings in one catalog value.
#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DetachedCommand {
    Legacy(String),
    Configured(DetachedCommandSettings),
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DetachedCommandSettings {
    pub command: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "working_directory"
    )]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

impl DetachedCommand {
    fn validate(&self) -> Result<(), &'static str> {
        let settings = match self {
            Self::Legacy(command) => return validate_detached_command_text(command),
            Self::Configured(settings) => settings,
        };
        validate_detached_command_text(&settings.command)?;
        validate_command_environment(&settings.environment)?;
        for path in [
            settings.cwd.as_deref(),
            settings.stdout.as_deref(),
            settings.stderr.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_path(path)?;
            if path.len() > 1_024 {
                return Err("detached path exceeds bounds");
            }
        }
        Ok(())
    }
}

fn validate_detached_command_text(command: &str) -> Result<(), &'static str> {
    if command.is_empty()
        || command.len() > MAX_DETACHED_COMMAND_BYTES
        || command.chars().any(char::is_control)
    {
        return Err("invalid detached command");
    }
    Ok(())
}

fn default_playnite_exit_timeout_seconds() -> u64 {
    10
}

fn default_playnite_fullscreen_exit_timeout_seconds() -> u64 {
    10
}

fn default_desktop_exit_timeout_seconds() -> u64 {
    10
}

pub fn valid_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn empty_working_directory_as_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.filter(|directory| !directory.is_empty()))
}

const MAX_EXIT_TIMEOUT_SECONDS: u64 = 300;
const MAX_DETACHED_COMMANDS: usize = 16;
const MAX_DETACHED_COMMAND_BYTES: usize = 2048;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationStep {
    pub run: ConfiguredCommand,
    pub undo: Option<ConfiguredCommand>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredCommand {
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<String>,
    #[serde(default = "default_command_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_command_timeout_ms() -> u64 {
    DEFAULT_COMMAND_TIMEOUT_MS
}

impl Entry {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.elevated && !matches!(&self.launch, Launch::Executable { .. }) {
            return Err("elevated is supported only for executable launch targets");
        }
        if let Some(artwork) = &self.artwork {
            artwork.validate()?;
        }
        if self
            .steam_id
            .as_deref()
            .is_some_and(|id| !valid_steam_id(id))
        {
            return Err("steam_id must be canonical nonzero u32 decimal");
        }
        if let Some(install_directory) = &self.steam_install_dir {
            validate_steam_install_directory(install_directory)?;
        }
        if self
            .output
            .as_deref()
            .is_some_and(|output| !bounded_text(output, 1_024))
        {
            return Err("output exceeds 1024 bytes or contains control characters");
        }
        if self
            .display_output
            .as_deref()
            .is_some_and(|output| !bounded_text(output, 1_024))
        {
            return Err("display_output exceeds 1024 bytes or contains control characters");
        }
        if self
            .virtual_display_mode
            .as_deref()
            .is_some_and(|mode| !matches!(mode, "disabled" | "per_client" | "shared"))
        {
            return Err("virtual_display_mode must be disabled, per_client, or shared");
        }
        if self
            .virtual_display_layout
            .as_deref()
            .is_some_and(|layout| {
                !matches!(
                    layout,
                    "exclusive"
                        | "extended"
                        | "extended_primary"
                        | "extended_isolated"
                        | "extended_primary_isolated"
                )
            })
        {
            return Err("invalid virtual_display_layout");
        }
        self.config_overrides.validate()?;
        validate_raw_preparation(&self.prep_cmd)?;
        if matches!(
            &self.launch,
            Launch::PlayniteFullscreen { .. } | Launch::Desktop { .. }
        ) && (self.artwork.is_some()
            || self.steam_id.is_some()
            || self.steam_install_dir.is_some())
        {
            return Err("Playnite Fullscreen and Desktop entries cannot contain Steam metadata");
        }
        if !bounded_text(&self.name, 160) || self.name.trim().is_empty() {
            return Err("name must contain 1..160 bytes of printable text");
        }
        if !bounded_text(&self.description, 1024) {
            return Err("description exceeds 1024 bytes or contains control characters");
        }
        if self.tags.len() > 16
            || self
                .tags
                .iter()
                .any(|tag| tag.trim().is_empty() || !bounded_text(tag, 64))
        {
            return Err("at most 16 nonempty printable tags of 64 bytes are allowed");
        }
        match &self.launch {
            Launch::Executable {
                program,
                arguments,
                working_directory,
                auto_detach: _,
                wait_all: _,
                exit_timeout_seconds,
                detached,
                preparation,
            } => {
                validate_path(program)?;
                if arguments.len() > 32 || arguments.iter().any(|arg| !bounded_text(arg, 1024)) {
                    return Err("at most 32 printable arguments of 1024 bytes are allowed");
                }
                if let Some(directory) = working_directory {
                    validate_working_directory(directory)?;
                }
                if exit_timeout_seconds.is_some_and(|seconds| seconds > MAX_EXIT_TIMEOUT_SECONDS)
                    || detached.len() > MAX_DETACHED_COMMANDS
                    || detached.iter().any(|command| command.validate().is_err())
                {
                    return Err("invalid per-application process policy");
                }
                validate_preparation(preparation)?;
                validate_combined_preparation_count(preparation.len(), self.prep_cmd.len())?;
            }
            Launch::Application { path } => validate_path(path)?,
            Launch::Playnite {
                game_id,
                exit_timeout_seconds,
                working_directory,
                preparation,
            } => {
                if !valid_guid(game_id) {
                    return Err("Playnite launch target must contain a GUID");
                }
                if *exit_timeout_seconds == 0 || *exit_timeout_seconds > MAX_EXIT_TIMEOUT_SECONDS {
                    return Err("invalid Playnite exit timeout");
                }
                if let Some(directory) = working_directory {
                    validate_working_directory(directory)?;
                }
                validate_preparation(preparation)?;
                validate_combined_preparation_count(preparation.len(), self.prep_cmd.len())?;
            }
            Launch::PlayniteFullscreen {
                exit_timeout_seconds,
                working_directory,
                preparation,
            } => {
                if *exit_timeout_seconds > MAX_EXIT_TIMEOUT_SECONDS {
                    return Err("invalid Playnite Fullscreen exit timeout");
                }
                if let Some(directory) = working_directory {
                    validate_working_directory(directory)?;
                }
                validate_preparation(preparation)?;
                validate_combined_preparation_count(preparation.len(), self.prep_cmd.len())?;
            }
            Launch::Desktop {
                exit_timeout_seconds,
                working_directory,
                preparation,
            } => {
                if *exit_timeout_seconds > MAX_EXIT_TIMEOUT_SECONDS {
                    return Err("invalid Desktop exit timeout");
                }
                if let Some(directory) = working_directory {
                    validate_working_directory(directory)?;
                }
                validate_preparation(preparation)?;
                validate_combined_preparation_count(preparation.len(), self.prep_cmd.len())?;
            }
        }
        if serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > 11_950) {
            return Err("encoded entry exceeds 11950 bytes");
        }
        Ok(())
    }
}

pub fn valid_steam_id(value: &str) -> bool {
    let Ok(app_id) = value.parse::<u32>() else {
        return false;
    };
    app_id != 0 && app_id.to_string() == value
}

fn validate_steam_install_directory(value: &str) -> Result<(), &'static str> {
    if value.trim() != value || value.is_empty() || value.len() > 1_024 {
        return Err("steam_install_dir must be 1..1024 bytes without surrounding whitespace");
    }
    if value.chars().any(char::is_control)
        || value
            .split(['/', '\\'])
            .any(|component| matches!(component, "." | ".."))
    {
        return Err("steam_install_dir must be absolute and contain no dot segments or controls");
    }

    let components = value
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let drive_rooted = value.len() >= 3
        && value.as_bytes()[0].is_ascii_alphabetic()
        && value.as_bytes()[1] == b':'
        && matches!(value.as_bytes()[2], b'/' | b'\\');
    let unc_rooted = value.starts_with("\\\\") || value.starts_with("//");
    let posix_rooted = value.starts_with('/') && !unc_rooted;
    if (drive_rooted && components.len() <= 1)
        || (unc_rooted && components.len() < 3)
        || (posix_rooted && components.is_empty())
        || !(drive_rooted || unc_rooted || posix_rooted)
    {
        return Err("steam_install_dir must name a directory below its filesystem root");
    }
    Ok(())
}

fn validate_preparation(steps: &[PreparationStep]) -> Result<(), &'static str> {
    if steps.len() > MAX_PREPARATION_STEPS {
        return Err("at most 8 preparation steps are allowed");
    }
    for step in steps {
        validate_command(&step.run)?;
        if let Some(undo) = &step.undo {
            validate_command(undo)?;
        }
    }
    Ok(())
}

fn validate_raw_preparation(steps: &[RawPreparationCommand]) -> Result<(), &'static str> {
    if steps.len() > MAX_PREPARATION_STEPS
        || steps.iter().any(|step| {
            [step.do_command.as_str(), step.undo.as_str()]
                .iter()
                .any(|command| {
                    command.len() > MAX_RAW_PREPARATION_COMMAND_BYTES || command.contains('\0')
                })
        })
    {
        return Err("raw preparation commands exceed bounds");
    }
    Ok(())
}

fn validate_combined_preparation_count(
    structured_count: usize,
    raw_count: usize,
) -> Result<(), &'static str> {
    if structured_count + raw_count > MAX_PREPARATION_STEPS {
        return Err("at most 8 preparation steps are allowed");
    }
    Ok(())
}

fn validate_command(command: &ConfiguredCommand) -> Result<(), &'static str> {
    if !valid_command_path(&command.executable)
        || command
            .cwd
            .as_deref()
            .is_some_and(|directory| !valid_command_path(directory))
    {
        return Err("command executable and cwd must be native absolute paths");
    }
    if Path::new(&command.executable)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
        })
    {
        return Err("batch command targets require an explicit interpreter");
    }
    if command.args.len() > 64
        || command
            .args
            .iter()
            .any(|argument| argument.len() > 1_024 || argument.contains('\0'))
        || command.args.iter().map(String::len).sum::<usize>() > 8_192
    {
        return Err("command arguments exceed bounds");
    }
    if !(1..=MAX_COMMAND_TIMEOUT_MS).contains(&command.timeout_ms) {
        return Err("command timeout exceeds bounds");
    }
    validate_command_environment(&command.environment)
}

fn valid_command_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1_024
        && !value.contains('\0')
        && Path::new(value).is_absolute()
}

fn validate_command_environment(
    environment: &BTreeMap<String, String>,
) -> Result<(), &'static str> {
    if environment.len() > 32
        || environment.iter().any(|(key, value)| {
            key.is_empty()
                || key.len() > 128
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || value.len() > 1_024
                || value.contains('\0')
        })
        || environment
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 4_096
    {
        return Err("command environment exceeds bounds");
    }
    let mut names = environment
        .keys()
        .map(|key| key.to_ascii_uppercase())
        .collect::<Vec<_>>();
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("command environment names are ambiguous");
    }
    Ok(())
}

pub fn bounded_text(value: &str, limit: usize) -> bool {
    value.len() <= limit && !value.chars().any(char::is_control)
}

fn validate_path(path: &str) -> Result<(), &'static str> {
    // Accept explicit POSIX, drive-rooted Windows, and UNC metadata on any host.
    // This is structural validation, not filesystem access or execution authorization.
    let bytes = path.as_bytes();
    let drive_rooted = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    if !bounded_text(path, 2048)
        || !(path.starts_with('/') || path.starts_with("\\\\") || drive_rooted)
        || path
            .split(['/', '\\'])
            .any(|part| part == ".." || part == ".")
    {
        return Err(
            "launch paths must be absolute, printable, at most 2048 bytes, without dot segments",
        );
    }
    Ok(())
}

fn validate_working_directory(path: &str) -> Result<(), &'static str> {
    validate_path(path)?;
    if path.len() > MAX_LAUNCH_WORKING_DIRECTORY_BYTES {
        return Err("working directory exceeds the 1024-byte launch protocol limit");
    }
    Ok(())
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub revision: u64,
    pub next_id: u64,
    pub entries: BTreeMap<String, Entry>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            revision: 0,
            next_id: 1,
            entries: BTreeMap::new(),
        }
    }
}

impl Document {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.next_id == 0 || self.entries.len() > MAX_ENTRIES {
            return Err("invalid catalog size or ID counter");
        }
        for (id, entry) in &self.entries {
            let number = parse_id(id).ok_or("invalid entry ID")?;
            if number >= self.next_id {
                return Err("entry ID exceeds allocated range");
            }
            entry.validate()?;
        }
        Ok(())
    }
}

pub fn parse_id(id: &str) -> Option<u64> {
    let number = u64::from_str_radix(id.strip_prefix("app-")?, 16).ok()?;
    (number != 0 && id == format!("app-{number:016x}")).then_some(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn display_remapping_requires_resolver_usable_final_values_and_tokens() {
        for rules in [
            json!({"mixed":[{}]}),
            json!({"resolution_only":[{"final_refresh_rate":"60"}]}),
            json!({"refresh_rate_only":[{"final_resolution":"1920x1080"}]}),
            json!({"mixed":[{"requested_fps":"59.94","final_resolution":"1920x1080"}]}),
            json!({"mixed":[{"requested_resolution":"bad","final_resolution":"1920x1080"}]}),
            json!({"mixed":[{"final_refresh_rate":"4294967296"}]}),
        ] {
            let overrides: ConfigOverrides =
                serde_json::from_value(json!({"dd_mode_remapping":rules})).unwrap();
            assert!(overrides.validate().is_err());
        }
        let valid: ConfigOverrides = serde_json::from_value(json!({
            "dd_mode_remapping":{"mixed":[{"requested_fps":"60","final_resolution":"1920x1080","final_refresh_rate":"59.94"}]}
        })).unwrap();
        valid.validate().unwrap();
    }

    #[test]
    fn manual_refresh_rate_requires_an_exact_representable_rational() {
        for rate in ["0.0000000001", "4294967296", "59.1234567890", "1."] {
            let overrides: ConfigOverrides =
                serde_json::from_value(json!({"dd_manual_refresh_rate":rate})).unwrap();
            assert!(overrides.validate().is_err(), "{rate}");
        }
        for rate in ["", "59.94", "60", "00060.000"] {
            let overrides: ConfigOverrides =
                serde_json::from_value(json!({"dd_manual_refresh_rate":rate})).unwrap();
            overrides.validate().unwrap();
        }
    }

    #[test]
    fn stream_audio_override_preserves_absence_false_and_existing_fec() {
        let legacy: Entry = serde_json::from_value(json!({
            "name":"Legacy entry",
            "config_overrides":{"fec_percentage":37},
            "launch":{"kind":"executable","program":"/games/example"}
        }))
        .unwrap();
        legacy.validate().unwrap();
        assert!(!legacy.elevated);
        assert_eq!(legacy.config_overrides.fec_percentage, Some(37));
        assert_eq!(legacy.config_overrides.max_bitrate, None);
        assert_eq!(legacy.config_overrides.minimum_fps_target, None);
        assert_eq!(legacy.config_overrides.video_max_batch_size_kb, None);
        assert_eq!(legacy.config_overrides.stream_audio, None);
        let legacy_json = serde_json::to_value(legacy).unwrap();
        assert_eq!(legacy_json["config_overrides"]["fec_percentage"], 37);
        assert!(
            legacy_json["config_overrides"]
                .get("stream_audio")
                .is_none()
        );

        let inherited: Entry = serde_json::from_value(json!({
            "name":"Inherited entry",
            "launch":{"kind":"application","path":"/games/example"}
        }))
        .unwrap();
        inherited.validate().unwrap();
        assert!(inherited.config_overrides.is_empty());
        assert!(
            serde_json::to_value(inherited)
                .unwrap()
                .get("config_overrides")
                .is_none()
        );

        let disabled: ConfigOverrides =
            serde_json::from_value(json!({"fec_percentage":37,"stream_audio":false})).unwrap();
        assert_eq!(disabled.fec_percentage, Some(37));
        assert_eq!(disabled.stream_audio, Some(false));
        assert!(!disabled.is_empty());
        assert_eq!(
            serde_json::to_value(disabled).unwrap(),
            json!({"fec_percentage":37,"stream_audio":false})
        );
    }

    #[test]
    fn frame_generation_provider_and_mode_preserve_sparse_canonical_values() {
        for value in [json!({}), json!({"frame_generation_provider": null})] {
            let overrides: ConfigOverrides = serde_json::from_value(value).unwrap();
            assert_eq!(overrides.frame_generation_provider, None);
            assert!(overrides.is_empty());
            assert_eq!(serde_json::to_value(overrides).unwrap(), json!({}));
        }

        for (field, values) in [
            (
                "frame_generation_provider",
                &["lossless-scaling", "game-provided", "nvidia-smooth-motion"][..],
            ),
            (
                "frame_generation_mode",
                &[
                    "off",
                    "lossless-scaling",
                    "game-provided",
                    "nvidia-smooth-motion",
                ][..],
            ),
        ] {
            for value in values {
                let overrides: ConfigOverrides =
                    serde_json::from_value(json!({(field): value})).unwrap();
                overrides.validate().unwrap();
                assert!(!overrides.is_empty());
                assert_eq!(
                    serde_json::to_value(overrides).unwrap(),
                    json!({(field): value})
                );
            }
            for value in ["unknown", "disabled", "none"] {
                let overrides: ConfigOverrides =
                    serde_json::from_value(json!({(field): value})).unwrap();
                assert!(overrides.validate().is_err());
            }
        }
    }

    #[test]
    fn lossless_scaling_target_fps_is_sparse_positive_and_bounded() {
        for value in [json!({}), json!({"lossless_scaling_target_fps": null})] {
            let overrides: ConfigOverrides = serde_json::from_value(value).unwrap();
            assert_eq!(overrides.lossless_scaling_target_fps, None);
            assert!(overrides.is_empty());
            assert_eq!(serde_json::to_value(overrides).unwrap(), json!({}));
        }

        for fps in [1, 60, 1000] {
            let overrides: ConfigOverrides =
                serde_json::from_value(json!({"lossless_scaling_target_fps": fps})).unwrap();
            overrides.validate().unwrap();
            assert_eq!(overrides.lossless_scaling_target_fps, Some(fps));
            assert_eq!(
                serde_json::to_value(overrides).unwrap(),
                json!({"lossless_scaling_target_fps": fps})
            );
        }

        for value in [json!(0), json!(1001), json!("60"), json!(60.0)] {
            let parsed = serde_json::from_value::<ConfigOverrides>(json!({
                "lossless_scaling_target_fps": value
            }));
            assert!(parsed.is_err() || parsed.unwrap().validate().is_err());
        }
    }

    #[test]
    fn lossless_scaling_framegen_override_defaults_off_and_preserves_explicit_false() {
        for value in [json!({}), json!({"lossless_scaling_framegen": null})] {
            let overrides: ConfigOverrides = serde_json::from_value(value).unwrap();
            assert_eq!(overrides.lossless_scaling_framegen, None);
            assert!(overrides.is_empty());
            assert_eq!(serde_json::to_value(overrides).unwrap(), json!({}));
        }

        for enabled in [false, true] {
            let overrides: ConfigOverrides = serde_json::from_value(json!({
                "lossless_scaling_framegen": enabled
            }))
            .unwrap();
            overrides.validate().unwrap();
            assert_eq!(overrides.lossless_scaling_framegen, Some(enabled));
            assert!(!overrides.is_empty());
            assert_eq!(
                serde_json::to_value(overrides).unwrap(),
                json!({"lossless_scaling_framegen": enabled})
            );
        }

        for value in [json!("true"), json!(0)] {
            assert!(
                serde_json::from_value::<ConfigOverrides>(json!({
                    "lossless_scaling_framegen": value
                }))
                .is_err()
            );
        }
    }

    #[test]
    fn amd_av1_latency_mode_is_sparse_exact_and_roundtrips_through_catalog_entries() {
        let absent = ConfigOverrides::default();
        let null: ConfigOverrides =
            serde_json::from_value(json!({"amd_av1_latency_mode":null})).unwrap();
        assert_eq!(absent.amd_av1_latency_mode, None);
        assert_eq!(null.amd_av1_latency_mode, None);
        assert!(absent.is_empty());
        assert!(null.is_empty());
        assert_eq!(serde_json::to_value(absent).unwrap(), json!({}));
        assert_eq!(serde_json::to_value(null).unwrap(), json!({}));

        let modes = ["auto", "none", "power_saving", "realtime", "lowest"];
        for mode in modes {
            let entry: Entry = serde_json::from_value(json!({
                "name":"AV1 latency mode",
                "config_overrides":{"amd_av1_latency_mode":mode},
                "launch":{"kind":"application","path":"/games/example"}
            }))
            .unwrap();
            entry.validate().unwrap();
            assert_eq!(
                entry.config_overrides.amd_av1_latency_mode.as_deref(),
                Some(mode)
            );
            assert_eq!(
                serde_json::to_value(&entry).unwrap()["config_overrides"],
                json!({"amd_av1_latency_mode":mode})
            );
        }

        let explicit_auto: ConfigOverrides =
            serde_json::from_value(json!({"amd_av1_latency_mode":"auto"})).unwrap();
        assert_eq!(explicit_auto.amd_av1_latency_mode.as_deref(), Some("auto"));
        assert!(!explicit_auto.is_empty());

        for invalid in [
            "AUTO",
            "Power_saving",
            "power_Saving",
            "power_saving ",
            "unknown",
        ] {
            let overrides: ConfigOverrides = serde_json::from_value(json!({
                "amd_av1_latency_mode":invalid
            }))
            .unwrap();
            assert!(overrides.validate().is_err(), "accepted mode {invalid:?}");
        }
        for invalid in [json!(1), json!(true), json!([]), json!({})] {
            assert!(
                serde_json::from_value::<ConfigOverrides>(json!({
                    "amd_av1_latency_mode":invalid
                }))
                .is_err(),
                "accepted non-string mode {invalid}"
            );
        }
    }

    #[test]
    fn capture_override_is_sparse_and_rejects_unknown_modes() {
        let parsed: ConfigOverrides =
            serde_json::from_value(json!({"capture":"pipewire"})).unwrap();
        assert_eq!(parsed.capture.as_deref(), Some("pipewire"));
        parsed.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            json!({"capture":"pipewire"})
        );

        for invalid in ["fallback", "dxgi", "PIPEWIRE"] {
            let parsed: ConfigOverrides =
                serde_json::from_value(json!({"capture":invalid})).unwrap();
            assert!(parsed.validate().is_err());
        }
        assert!(serde_json::from_value::<ConfigOverrides>(json!({"capture":7})).is_err());
    }

    #[test]
    fn display_policy_override_is_sparse_typed_and_scope_limited() {
        let parsed: ConfigOverrides = serde_json::from_value(json!({
            "display_policy":{
                "adapter_name":"/dev/dri/renderD128",
                "virtual_display_layout":"extended_primary",
                "dd_activate_virtual_display":true,
                "dd_resolution_option":"manual",
                "dd_manual_resolution":"1920x1080",
                "dd_refresh_rate_option":"manual",
                "dd_manual_refresh_rate":"59.94",
                "dd_virtual_display_permanent_count":2,
                "virtual_display_outputs":["Vibeshine-1"]
            }
        }))
        .unwrap();
        parsed.validate().unwrap();
        assert!(!parsed.is_empty());
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            json!({"display_policy":{
                "adapter_name":"/dev/dri/renderD128",
                "virtual_display_layout":"extended_primary",
                "dd_activate_virtual_display":true,
                "dd_resolution_option":"manual",
                "dd_manual_resolution":"1920x1080",
                "dd_refresh_rate_option":"manual",
                "dd_manual_refresh_rate":"59.94",
                "dd_virtual_display_permanent_count":2,
                "virtual_display_outputs":["Vibeshine-1"]
            }})
        );

        for policy in [
            json!({"dd_virtual_display_permanent_count":5}),
            json!({"dd_virtual_display_scale":275}),
            json!({"dd_config_revert_delay":-1}),
            json!({"vulkan_hdr_layer":true}),
            json!({"adapter_name":false}),
        ] {
            let invalid: ConfigOverrides =
                serde_json::from_value(json!({"display_policy":policy})).unwrap();
            assert!(invalid.validate().is_err(), "{policy}");
        }
        assert!(
            serde_json::from_value::<ConfigOverrides>(json!({
                "display_policy":{"unknown_display_key":true}
            }))
            .unwrap()
            .validate()
            .is_err()
        );
    }

    #[test]
    fn dd_refresh_rate_option_is_sparse_exact_and_roundtrips_through_catalog_entries() {
        let absent: ConfigOverrides = serde_json::from_value(json!({})).unwrap();
        let null: ConfigOverrides =
            serde_json::from_value(json!({"dd_refresh_rate_option":null})).unwrap();
        assert_eq!(absent.dd_refresh_rate_option, None);
        assert_eq!(null.dd_refresh_rate_option, None);
        assert!(absent.is_empty());
        assert!(null.is_empty());
        assert_eq!(serde_json::to_value(absent).unwrap(), json!({}));
        assert_eq!(serde_json::to_value(null).unwrap(), json!({}));

        let absent_entry: Entry = serde_json::from_value(json!({
            "name":"Inherited refresh rate option",
            "launch":{"kind":"application","path":"/games/example"}
        }))
        .unwrap();
        absent_entry.validate().unwrap();
        assert_eq!(absent_entry.config_overrides.dd_refresh_rate_option, None);
        assert!(
            serde_json::to_value(absent_entry)
                .unwrap()
                .get("config_overrides")
                .is_none()
        );

        let inherited: Entry = serde_json::from_value(json!({
            "name":"Inherited refresh rate option",
            "config_overrides":{"dd_refresh_rate_option":null},
            "launch":{"kind":"application","path":"/games/example"}
        }))
        .unwrap();
        inherited.validate().unwrap();
        assert_eq!(inherited.config_overrides.dd_refresh_rate_option, None);
        assert!(
            serde_json::to_value(inherited)
                .unwrap()
                .get("config_overrides")
                .is_none()
        );

        for option in ["auto", "disabled", "manual", "prefer_highest"] {
            let entry: Entry = serde_json::from_value(json!({
                "name":"Selected refresh rate option",
                "config_overrides":{"dd_refresh_rate_option":option},
                "launch":{"kind":"application","path":"/games/example"}
            }))
            .unwrap();
            entry.validate().unwrap();
            assert_eq!(
                entry.config_overrides.dd_refresh_rate_option.as_deref(),
                Some(option)
            );

            let serialized = serde_json::to_value(&entry).unwrap();
            assert_eq!(
                serialized["config_overrides"]["dd_refresh_rate_option"],
                option
            );
            let restored: Entry = serde_json::from_value(serialized).unwrap();
            restored.validate().unwrap();
            assert_eq!(
                restored.config_overrides.dd_refresh_rate_option.as_deref(),
                Some(option)
            );
        }

        let explicit_auto: ConfigOverrides =
            serde_json::from_value(json!({"dd_refresh_rate_option":"auto"})).unwrap();
        assert_eq!(
            explicit_auto.dd_refresh_rate_option.as_deref(),
            Some("auto")
        );
        assert!(!explicit_auto.is_empty());

        for unsupported in ["AUTO", "prefer-highest", "future", "manual "] {
            let overrides: ConfigOverrides = serde_json::from_value(json!({
                "dd_refresh_rate_option":unsupported
            }))
            .unwrap();
            assert!(
                overrides.validate().is_err(),
                "accepted refresh rate option {unsupported:?}"
            );
        }
        for invalid in [json!(1), json!(true), json!([]), json!({})] {
            assert!(
                serde_json::from_value::<ConfigOverrides>(json!({
                    "dd_refresh_rate_option":invalid
                }))
                .is_err(),
                "accepted non-string refresh rate option {invalid}"
            );
        }
    }

    #[test]
    fn dd_hdr_request_override_is_sparse_exact_and_roundtrips_through_catalog_entries() {
        let absent: ConfigOverrides = serde_json::from_value(json!({})).unwrap();
        let null: ConfigOverrides =
            serde_json::from_value(json!({"dd_hdr_request_override":null})).unwrap();
        assert_eq!(absent.dd_hdr_request_override, None);
        assert_eq!(null.dd_hdr_request_override, None);
        assert!(absent.is_empty());
        assert!(null.is_empty());
        assert_eq!(serde_json::to_value(absent).unwrap(), json!({}));
        assert_eq!(serde_json::to_value(null).unwrap(), json!({}));

        let absent_entry: Entry = serde_json::from_value(json!({
            "name":"Inherited HDR request",
            "launch":{"kind":"application","path":"/games/example"}
        }))
        .unwrap();
        absent_entry.validate().unwrap();
        assert_eq!(absent_entry.config_overrides.dd_hdr_request_override, None);
        assert!(
            serde_json::to_value(absent_entry)
                .unwrap()
                .get("config_overrides")
                .is_none()
        );

        let inherited_entry: Entry = serde_json::from_value(json!({
            "name":"Inherited HDR request",
            "config_overrides":{"dd_hdr_request_override":null},
            "launch":{"kind":"application","path":"/games/example"}
        }))
        .unwrap();
        inherited_entry.validate().unwrap();
        assert_eq!(
            inherited_entry.config_overrides.dd_hdr_request_override,
            None
        );
        assert!(
            serde_json::to_value(inherited_entry)
                .unwrap()
                .get("config_overrides")
                .is_none()
        );

        for mode in ["auto", "force_on", "force_off"] {
            let entry: Entry = serde_json::from_value(json!({
                "name":"Selected HDR request",
                "config_overrides":{"dd_hdr_request_override":mode},
                "launch":{"kind":"application","path":"/games/example"}
            }))
            .unwrap();
            entry.validate().unwrap();
            assert_eq!(
                entry.config_overrides.dd_hdr_request_override.as_deref(),
                Some(mode)
            );

            let serialized = serde_json::to_value(&entry).unwrap();
            assert_eq!(
                serialized["config_overrides"]["dd_hdr_request_override"],
                mode
            );
            let restored: Entry = serde_json::from_value(serialized).unwrap();
            restored.validate().unwrap();
            assert_eq!(
                restored.config_overrides.dd_hdr_request_override.as_deref(),
                Some(mode)
            );
        }

        let explicit_auto: ConfigOverrides =
            serde_json::from_value(json!({"dd_hdr_request_override":"auto"})).unwrap();
        assert_eq!(
            explicit_auto.dd_hdr_request_override.as_deref(),
            Some("auto")
        );
        assert!(!explicit_auto.is_empty());

        for unsupported in ["AUTO", "force-on", "unknown", "auto "] {
            let overrides: ConfigOverrides = serde_json::from_value(json!({
                "dd_hdr_request_override":unsupported
            }))
            .unwrap();
            assert!(
                overrides.validate().is_err(),
                "accepted HDR request override {unsupported:?}"
            );
        }
        for invalid in [json!(1), json!(true), json!([]), json!({})] {
            assert!(
                serde_json::from_value::<ConfigOverrides>(json!({
                    "dd_hdr_request_override":invalid
                }))
                .is_err(),
                "accepted non-string HDR request override {invalid}"
            );
        }
    }

    #[test]
    fn video_overrides_preserve_sparse_values_and_validate_bounds() {
        let inherited = ConfigOverrides::default();
        assert!(inherited.is_empty());
        assert_eq!(
            serde_json::from_value::<ConfigOverrides>(json!({
                "max_bitrate": null,
                "minimum_fps_target": null,
                "video_max_batch_size_kb": null
            }))
            .unwrap(),
            inherited
        );

        for max_bitrate in [i32::MIN, -1, 0, 42_000] {
            let selected: ConfigOverrides = serde_json::from_value(json!({
                "max_bitrate":max_bitrate,
                "minimum_fps_target":59.94,
                "video_max_batch_size_kb":0
            }))
            .unwrap();
            selected.validate().unwrap();
            assert_eq!(selected.max_bitrate, Some(max_bitrate));
            assert_eq!(selected.minimum_fps_target, Some(59.94));
            assert_eq!(selected.video_max_batch_size_kb, Some(0));
            assert_eq!(
                serde_json::to_value(selected).unwrap(),
                json!({
                    "max_bitrate":max_bitrate,
                    "minimum_fps_target":59.94,
                    "video_max_batch_size_kb":0
                })
            );
        }

        let entry: Entry = serde_json::from_value(json!({
            "name":"Video override entry",
            "config_overrides":{
                "max_bitrate":-1,
                "minimum_fps_target":59.94,
                "video_max_batch_size_kb":64
            },
            "launch":{"kind":"executable","program":"/games/example"}
        }))
        .unwrap();
        entry.validate().unwrap();
        let persisted_entry = serde_json::to_value(&entry).unwrap();
        assert_eq!(persisted_entry["config_overrides"]["max_bitrate"], -1);
        assert_eq!(
            persisted_entry["config_overrides"]["minimum_fps_target"],
            59.94
        );
        assert_eq!(
            persisted_entry["config_overrides"]["video_max_batch_size_kb"],
            64
        );
        let restored_entry: Entry = serde_json::from_value(persisted_entry).unwrap();
        restored_entry.validate().unwrap();
        assert_eq!(
            restored_entry.config_overrides.minimum_fps_target,
            Some(59.94)
        );

        for target in [0.0, 1000.0] {
            ConfigOverrides {
                minimum_fps_target: Some(target),
                ..ConfigOverrides::default()
            }
            .validate()
            .unwrap();
        }
        for target in [-0.1, 1000.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                ConfigOverrides {
                    minimum_fps_target: Some(target),
                    ..ConfigOverrides::default()
                }
                .validate()
                .is_err(),
                "accepted minimum FPS target {target}"
            );
        }
        for size in [0, 64] {
            ConfigOverrides {
                video_max_batch_size_kb: Some(size),
                ..ConfigOverrides::default()
            }
            .validate()
            .unwrap();
        }
        assert!(
            ConfigOverrides {
                video_max_batch_size_kb: Some(65),
                ..ConfigOverrides::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn elevated_defaults_false_round_trips_for_executables_and_rejects_other_targets() {
        let omitted: Entry = serde_json::from_value(json!({
            "name":"Legacy executable",
            "launch":{"kind":"executable","program":"/games/example"}
        }))
        .unwrap();
        assert!(!omitted.elevated);
        assert!(
            serde_json::to_value(&omitted)
                .unwrap()
                .get("elevated")
                .is_none()
        );

        let elevated: Entry = serde_json::from_value(json!({
            "name":"Elevated executable",
            "elevated":true,
            "launch":{"kind":"executable","program":"/games/example"}
        }))
        .unwrap();
        elevated.validate().unwrap();
        assert_eq!(serde_json::to_value(elevated).unwrap()["elevated"], true);

        for launch in [
            json!({"kind":"application","path":"/games/example"}),
            json!({"kind":"playnite","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}),
            json!({"kind":"playnite_fullscreen"}),
            json!({"kind":"desktop"}),
        ] {
            let entry: Entry = serde_json::from_value(json!({
                "name":"Not executable",
                "elevated":true,
                "launch":launch
            }))
            .unwrap();
            assert!(entry.validate().is_err());
        }
    }

    #[test]
    fn audio_sink_override_keeps_explicit_automatic_distinct_from_inherit() {
        let inherited = ConfigOverrides::default();
        assert!(inherited.is_empty());
        let automatic: ConfigOverrides = serde_json::from_value(json!({"audio_sink":""})).unwrap();
        automatic.validate().unwrap();
        assert!(!automatic.is_empty());
        assert_eq!(automatic.audio_sink.as_deref(), Some(""));
        assert_eq!(
            serde_json::to_value(automatic).unwrap(),
            json!({"audio_sink":""})
        );
        for endpoint in ["bad\nendpoint".to_owned(), "x".repeat(4097)] {
            let override_value: ConfigOverrides =
                serde_json::from_value(json!({"audio_sink":endpoint})).unwrap();
            assert!(override_value.validate().is_err());
        }
    }

    #[test]
    fn virtual_sink_override_preserves_inherit_empty_and_exact_selector() {
        let inherited = ConfigOverrides::default();
        assert_eq!(inherited.virtual_sink, None);
        assert!(inherited.is_empty());

        for selector in [
            "",
            "Steam Streaming Speakers",
            "{0.0.1.00000000}.{endpoint-guid}",
        ] {
            let overrides: ConfigOverrides =
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
            let overrides: ConfigOverrides =
                serde_json::from_value(json!({"virtual_sink":selector})).unwrap();
            assert!(overrides.validate().is_err());
        }
    }

    #[test]
    fn audio_sink_capture_only_override_preserves_absence_and_explicit_false() {
        let inherited = ConfigOverrides::default();
        assert!(inherited.is_empty());
        assert_eq!(inherited.audio_sink_capture_only, None);
        assert_eq!(serde_json::to_value(inherited).unwrap(), json!({}));

        let disabled: ConfigOverrides =
            serde_json::from_value(json!({"audio_sink_capture_only":false})).unwrap();
        disabled.validate().unwrap();
        assert_eq!(disabled.audio_sink_capture_only, Some(false));
        assert!(!disabled.is_empty());
        assert_eq!(
            serde_json::to_value(disabled).unwrap(),
            json!({"audio_sink_capture_only":false})
        );

        let enabled: ConfigOverrides =
            serde_json::from_value(json!({"audio_sink_capture_only":true})).unwrap();
        assert_eq!(enabled.audio_sink_capture_only, Some(true));
        assert!(
            serde_json::from_value::<ConfigOverrides>(json!({
                "audio_sink_capture_only":"false"
            }))
            .is_err()
        );
    }

    #[test]
    fn advanced_input_overrides_validate_types_and_repeat_boundaries() {
        let inherited: ConfigOverrides = serde_json::from_value(json!({
            "high_resolution_scrolling":null, "always_send_scancodes":null,
            "key_rightalt_to_key_win":null, "key_repeat_delay":null, "key_repeat_frequency":null,
        }))
        .unwrap();
        assert!(inherited.is_empty());
        assert_eq!(serde_json::to_value(inherited).unwrap(), json!({}));
        for value in [
            json!({"key_repeat_delay":0}),
            json!({"key_repeat_delay":i32::MAX}),
            json!({"key_repeat_frequency":0}),
            json!({"key_repeat_frequency":24.9}),
        ] {
            let overrides: ConfigOverrides = serde_json::from_value(value).unwrap();
            assert!(overrides.validate().is_ok());
            assert!(!overrides.is_empty());
        }
        for (key, value) in [
            ("high_resolution_scrolling", json!("false")),
            ("always_send_scancodes", json!(1)),
            ("key_rightalt_to_key_win", json!([])),
            ("key_repeat_delay", json!(-1)),
            ("key_repeat_delay", json!(0.5)),
            ("key_repeat_delay", json!(i32::MAX as u64 + 1)),
            ("key_repeat_frequency", json!(-0.1)),
            ("key_repeat_frequency", json!("24.9")),
        ] {
            let value = json!({key:value});
            assert!(
                serde_json::from_value::<ConfigOverrides>(value)
                    .map(|overrides| overrides.validate().is_err())
                    .unwrap_or(true)
            );
        }
        let overrides = ConfigOverrides {
            key_repeat_frequency: Some(f64::INFINITY),
            ..Default::default()
        };
        assert!(overrides.validate().is_err());
    }

    #[test]
    fn nvenc_spatial_aq_override_is_sparse_boolean_and_roundtrips_entries() {
        let absent = ConfigOverrides::default();
        let null: ConfigOverrides =
            serde_json::from_value(json!({"nvenc_spatial_aq":null})).unwrap();
        assert_eq!(absent.nvenc_spatial_aq, None);
        assert_eq!(null.nvenc_spatial_aq, None);
        assert!(absent.is_empty());
        assert!(null.is_empty());
        assert_eq!(serde_json::to_value(absent).unwrap(), json!({}));
        assert_eq!(serde_json::to_value(null).unwrap(), json!({}));

        for enabled in [true, false] {
            let overrides: ConfigOverrides = serde_json::from_value(json!({
                "nvenc_spatial_aq":enabled
            }))
            .unwrap();
            overrides.validate().unwrap();
            assert_eq!(overrides.nvenc_spatial_aq, Some(enabled));
            assert!(!overrides.is_empty());
            assert_eq!(
                serde_json::to_value(overrides).unwrap(),
                json!({"nvenc_spatial_aq":enabled})
            );
        }

        let entry: Entry = serde_json::from_value(json!({
            "name":"NVENC spatial AQ disabled",
            "config_overrides":{"nvenc_spatial_aq":false},
            "launch":{"kind":"application","path":"/games/example"}
        }))
        .unwrap();
        entry.validate().unwrap();
        let stored = serde_json::to_value(entry).unwrap();
        assert_eq!(stored["config_overrides"]["nvenc_spatial_aq"], false);
        let restored: Entry = serde_json::from_value(stored).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored.config_overrides.nvenc_spatial_aq, Some(false));

        for invalid in [json!("false"), json!(0), json!([]), json!({})] {
            assert!(
                serde_json::from_value::<ConfigOverrides>(json!({
                    "nvenc_spatial_aq":invalid
                }))
                .is_err(),
                "accepted non-boolean spatial AQ value {invalid}"
            );
        }
    }

    #[test]
    fn nvenc_settings_overrides_are_sparse_bounded_and_roundtrip() {
        for (value, preset, increase) in [
            (json!({"nvenc_preset":1,"nvenc_vbv_increase":0}), 1, 0),
            (
                json!({"nvenc_preset":7,"nvenc_h264_cavlc":true,"nvenc_twopass":"full_res","nvenc_vbv_increase":400,"nvenc_split_encode":"disabled","nvenc_realtime_hags":false,"nvenc_opengl_vulkan_on_dxgi":false,"nvenc_latency_over_power":false,"vaapi_strict_rc_buffer":true}),
                7,
                400,
            ),
            (json!({"nvenc_force_split_encode":"driver_decides"}), 1, 0),
        ] {
            let overrides: ConfigOverrides = serde_json::from_value(value.clone()).unwrap();
            assert!(overrides.validate().is_ok());
            assert!(!overrides.is_empty());
            let roundtrip = serde_json::to_value(overrides).unwrap();
            if value.get("nvenc_preset").is_some() {
                assert_eq!(roundtrip["nvenc_preset"], preset);
                assert_eq!(roundtrip["nvenc_vbv_increase"], increase);
            } else {
                assert_eq!(roundtrip["nvenc_split_encode"], "auto");
                assert!(roundtrip.get("nvenc_force_split_encode").is_none());
            }
        }
        for invalid in [
            json!({"nvenc_preset":0}),
            json!({"nvenc_preset":8}),
            json!({"nvenc_vbv_increase":401}),
            json!({"nvenc_vbv_increase":-1}),
            json!({"nvenc_twopass":"fast"}),
            json!({"nvenc_split_encode":"forced"}),
        ] {
            if let Ok(overrides) = serde_json::from_value::<ConfigOverrides>(invalid) {
                assert!(overrides.validate().is_err());
            }
        }
    }

    #[test]
    fn back_button_timeout_override_preserves_absence_and_explicit_disable() {
        let inherited = ConfigOverrides::default();
        assert_eq!(inherited.back_button_timeout, None);
        assert_eq!(serde_json::to_value(&inherited).unwrap(), json!({}));

        for timeout in [-1, 0, 1250] {
            let override_value: ConfigOverrides =
                serde_json::from_value(json!({"back_button_timeout":timeout})).unwrap();
            override_value.validate().unwrap();
            assert_eq!(override_value.back_button_timeout, Some(timeout));
            assert_eq!(
                serde_json::to_value(override_value).unwrap(),
                json!({"back_button_timeout":timeout})
            );
        }
        assert!(
            serde_json::from_value::<ConfigOverrides>(json!({
                "back_button_timeout":2147483648i64
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ConfigOverrides>(json!({
                "back_button_timeout":1.5
            }))
            .is_err()
        );
    }

    #[test]
    fn playnite_target_requires_a_canonical_guid_and_bounded_exit_timeout() {
        let valid: Entry = serde_json::from_value(json!({
            "name":"Playnite game",
            "launch":{"kind":"playnite","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}
        }))
        .unwrap();
        valid.validate().unwrap();
        let persisted = serde_json::to_value(valid).unwrap();
        assert_eq!(persisted["launch"]["exit_timeout_seconds"], 10);
        assert!(persisted["launch"].get("working_directory").is_none());

        let working_directory: Entry = serde_json::from_value(json!({
            "name":"Playnite game with directory",
            "launch":{
                "kind":"playnite",
                "game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab",
                "working_directory":"/games/space title"
            }
        }))
        .unwrap();
        working_directory.validate().unwrap();
        assert_eq!(
            serde_json::to_value(working_directory).unwrap()["launch"]["working_directory"],
            "/games/space title"
        );
        let legacy_working_directory: Entry = serde_json::from_value(json!({
            "name":"Playnite game with legacy directory",
            "launch":{
                "kind":"playnite",
                "game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab",
                "working-dir":"/games/legacy"
            }
        }))
        .unwrap();
        legacy_working_directory.validate().unwrap();
        assert_eq!(
            serde_json::to_value(legacy_working_directory).unwrap()["launch"]["working_directory"],
            "/games/legacy"
        );

        for game_id in [
            "../other",
            "a1b2c3d4-1234-5678-9abc-0123456789a",
            "{a1b2c3d4-1234-5678-9abc-0123456789ab}",
        ] {
            let entry: Entry = serde_json::from_value(json!({
                "name":"Invalid Playnite game",
                "launch":{"kind":"playnite","game_id":game_id}
            }))
            .unwrap();
            assert!(entry.validate().is_err(), "accepted {game_id}");
        }
        let entry: Entry = serde_json::from_value(json!({
            "name":"Invalid timeout",
            "launch":{"kind":"playnite","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab","exit_timeout_seconds":301}
        }))
        .unwrap();
        assert!(entry.validate().is_err());

        for directory in ["relative/path", "/games/../other", "/games/./current"] {
            let entry: Entry = serde_json::from_value(json!({
                "name":"Invalid Playnite working directory",
                "launch":{
                    "kind":"playnite",
                    "game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab",
                    "working_directory":directory
                }
            }))
            .unwrap();
            assert!(entry.validate().is_err(), "accepted {directory}");
        }
        let oversized_directory = format!("/{}", "x".repeat(MAX_LAUNCH_WORKING_DIRECTORY_BYTES));
        let entry: Entry = serde_json::from_value(json!({
            "name":"Oversized Playnite working directory",
            "launch":{
                "kind":"playnite",
                "game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab",
                "working_directory":oversized_directory
            }
        }))
        .unwrap();
        assert!(entry.validate().is_err());
    }

    #[test]
    fn manual_fullscreen_target_is_typed_bounded_and_carries_no_provider_identity() {
        let entry: Entry = serde_json::from_value(json!({
            "name":"Fullscreen",
            "launch":{"kind":"playnite_fullscreen"}
        }))
        .unwrap();
        entry.validate().unwrap();
        assert!(matches!(
            entry.launch,
            Launch::PlayniteFullscreen {
                exit_timeout_seconds: 10,
                ..
            }
        ));
        assert_eq!(
            serde_json::to_value(entry).unwrap()["launch"],
            json!({"kind":"playnite_fullscreen","exit_timeout_seconds":10})
        );

        let with_working_directory: Entry = serde_json::from_value(json!({
            "name":"Fullscreen with directory",
            "launch":{
                "kind":"playnite_fullscreen",
                "working_directory":"/games/fullscreen"
            }
        }))
        .unwrap();
        with_working_directory.validate().unwrap();
        assert_eq!(
            serde_json::to_value(with_working_directory).unwrap()["launch"]["working_directory"],
            "/games/fullscreen"
        );

        for timeout in [0, 300] {
            let entry: Entry = serde_json::from_value(json!({
                "name":"Fullscreen",
                "launch":{"kind":"playnite_fullscreen","exit_timeout_seconds":timeout}
            }))
            .unwrap();
            entry.validate().unwrap();
        }
        let invalid: Entry = serde_json::from_value(json!({
            "name":"Fullscreen",
            "launch":{"kind":"playnite_fullscreen","exit_timeout_seconds":301}
        }))
        .unwrap();
        assert!(invalid.validate().is_err());

        for forged in [
            json!({"name":"Fullscreen","launch":{"kind":"playnite_fullscreen","path":"C:/evil.exe"}}),
            json!({"name":"Fullscreen","launch":{"kind":"playnite_fullscreen","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}}),
            json!({"name":"Fullscreen","steam_id":"42","launch":{"kind":"playnite_fullscreen"}}),
        ] {
            if let Ok(entry) = serde_json::from_value::<Entry>(forged.clone()) {
                assert!(entry.validate().is_err(), "accepted {forged}");
            }
        }
    }

    #[test]
    fn desktop_target_is_process_less_normalized_and_bounded() {
        let bare: Entry = serde_json::from_value(json!({
            "name":"Desktop",
            "launch":{"kind":"desktop"}
        }))
        .unwrap();
        bare.validate().unwrap();
        assert!(matches!(
            bare.launch,
            Launch::Desktop {
                exit_timeout_seconds: 10,
                working_directory: None,
                ..
            }
        ));
        assert_eq!(
            serde_json::to_value(&bare).unwrap()["launch"],
            json!({"kind":"desktop","exit_timeout_seconds":10})
        );

        let configured: Entry = serde_json::from_value(json!({
            "name":"Desktop",
            "prep_cmd":[{"do":"enable","undo":"disable"}],
            "launch":{
                "kind":"desktop",
                "exit_timeout_seconds":30,
                "working-dir":"/home/player",
                "preparation":[{
                    "run":{"executable":"/usr/bin/configure","args":["enable"]},
                    "undo":{"executable":"/usr/bin/configure","args":["disable"]}
                }]
            }
        }))
        .unwrap();
        configured.validate().unwrap();
        let normalized = serde_json::to_value(&configured).unwrap();
        assert_eq!(normalized["launch"]["exit_timeout_seconds"], 30);
        assert_eq!(normalized["launch"]["working_directory"], "/home/player");
        assert_eq!(
            normalized["launch"]["preparation"][0]["run"]["timeout_ms"],
            10_000
        );
        assert_eq!(
            normalized["launch"]["preparation"][0]["undo"]["cwd"],
            json!(null)
        );
        // Normalization is stable: reading the stored form changes nothing.
        let reread: Entry = serde_json::from_value(normalized.clone()).unwrap();
        assert_eq!(serde_json::to_value(reread).unwrap(), normalized);

        for timeout in [0, 300] {
            let entry: Entry = serde_json::from_value(json!({
                "name":"Desktop","launch":{"kind":"desktop","exit_timeout_seconds":timeout}
            }))
            .unwrap();
            entry.validate().unwrap();
        }
        let too_long: Entry = serde_json::from_value(json!({
            "name":"Desktop","launch":{"kind":"desktop","exit_timeout_seconds":301}
        }))
        .unwrap();
        assert!(too_long.validate().is_err());
        let relative: Entry = serde_json::from_value(json!({
            "name":"Desktop","launch":{"kind":"desktop","working_directory":"relative"}
        }))
        .unwrap();
        assert!(relative.validate().is_err());
    }

    #[test]
    fn desktop_target_rejects_programs_identities_elevation_and_unknown_fields() {
        for launch in [
            json!({"kind":"desktop","program":"/usr/bin/true"}),
            json!({"kind":"desktop","path":"/usr/bin/true"}),
            json!({"kind":"desktop","arguments":["--fullscreen"]}),
            json!({"kind":"desktop","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}),
            json!({"kind":"desktop","steam_id":"480"}),
            json!({"kind":"desktop","elevated":true}),
            json!({"kind":"desktop","detached":["true"]}),
            json!({"kind":"desktop","unknown":1}),
        ] {
            assert!(
                serde_json::from_value::<Entry>(json!({"name":"Desktop","launch":launch.clone()}))
                    .is_err(),
                "accepted {launch}"
            );
        }
        for entry in [
            json!({"name":"Desktop","elevated":true,"launch":{"kind":"desktop"}}),
            json!({"name":"Desktop","steam_id":"480","launch":{"kind":"desktop"}}),
            json!({"name":"Desktop","steam_install_dir":"/games/steam","launch":{"kind":"desktop"}}),
            json!({"name":"Desktop","artwork":{"provider":"steam","app_id":480,"revision":"1"},"launch":{"kind":"desktop"}}),
        ] {
            let parsed: Entry = serde_json::from_value(entry.clone()).unwrap();
            assert!(parsed.validate().is_err(), "accepted {entry}");
        }
        let too_many_steps: Entry = serde_json::from_value(json!({
            "name":"Desktop",
            "prep_cmd":(0..8).map(|index| json!({"do":format!("step-{index}")})).collect::<Vec<_>>(),
            "launch":{"kind":"desktop","preparation":[{"run":{"executable":"/usr/bin/configure"}}]}
        }))
        .unwrap();
        assert!(too_many_steps.validate().is_err());
    }

    #[test]
    fn steam_artwork_references_are_opaque_typed_and_bounded() {
        let entry: Entry = serde_json::from_value(json!({
            "name":"Steam game",
            "artwork":{"provider":"steam","app_id":42,"revision":"18446744073709551615"},
            "launch":{"kind":"application","path":"/games/example"}
        }))
        .unwrap();
        entry.validate().unwrap();
        assert_eq!(
            serde_json::to_value(entry).unwrap()["artwork"],
            json!({"provider":"steam","app_id":42,"revision":"18446744073709551615"})
        );

        for reference in [
            json!({"provider":"steam","app_id":0,"revision":"1"}),
            json!({"provider":"steam","app_id":42,"revision":""}),
            json!({"provider":"steam","app_id":42,"revision":"01"}),
            json!({"provider":"steam","app_id":42,"revision":"18446744073709551616"}),
            json!({"provider":"steam","app_id":42,"revision":"12x"}),
        ] {
            let entry: Entry = serde_json::from_value(json!({
                "name":"Invalid artwork",
                "artwork":reference,
                "launch":{"kind":"application","path":"/games/example"}
            }))
            .unwrap();
            assert!(entry.validate().is_err(), "accepted artwork {reference}");
        }

        assert!(
            serde_json::from_value::<Entry>(json!({
                "name":"Unknown artwork provider",
                "artwork":{"provider":"local_path","path":"C:/private/image.png"},
                "launch":{"kind":"application","path":"/games/example"}
            }))
            .is_err()
        );
    }

    #[test]
    fn lutris_artwork_references_require_positive_ids_and_digest_revisions() {
        let revision = "a".repeat(64);
        let valid: Entry = serde_json::from_value(json!({
            "name":"Lutris game",
            "artwork":{"provider":"lutris","game_id":42,"revision":revision},
            "launch":{"kind":"application","path":"/games/example"}
        }))
        .unwrap();
        valid.validate().unwrap();

        for reference in [
            json!({"provider":"lutris","game_id":0,"revision":revision}),
            json!({"provider":"lutris","game_id":-1,"revision":revision}),
            json!({"provider":"lutris","game_id":42,"revision":""}),
            json!({"provider":"lutris","game_id":42,"revision":"a"}),
            json!({"provider":"lutris","game_id":42,"revision":"A".repeat(64)}),
            json!({"provider":"lutris","game_id":42,"revision":"/".repeat(64)}),
            json!({"provider":"lutris","game_id":42,"revision":"12x"}),
        ] {
            let entry: Entry = serde_json::from_value(json!({
                "name":"Invalid artwork",
                "artwork":reference,
                "launch":{"kind":"application","path":"/games/example"}
            }))
            .unwrap();
            assert!(entry.validate().is_err(), "accepted artwork {reference}");
        }
        assert!(
            serde_json::from_value::<Entry>(json!({
                "name":"Path forgery",
                "artwork":{"provider":"lutris","game_id":42,"revision":revision,"path":"/secret"},
                "launch":{"kind":"application","path":"/games/example"}
            }))
            .is_err()
        );
    }

    #[test]
    fn steam_launch_metadata_is_sparse_canonical_and_path_bounded() {
        let legacy: Entry = serde_json::from_value(json!({
            "name":"Legacy entry",
            "launch":{"kind":"application","path":"/games/example"}
        }))
        .unwrap();
        legacy.validate().unwrap();
        let legacy = serde_json::to_value(legacy).unwrap();
        assert!(legacy.get("steam_id").is_none());
        assert!(legacy.get("steam_install_dir").is_none());

        assert!(valid_steam_id("4294967295"));
        for invalid in ["0", "01", "+42", "steam:42", "4294967296"] {
            assert!(!valid_steam_id(invalid), "accepted Steam ID {invalid}");
        }

        for directory in [
            "/games/space game",
            "C:\\SteamLibrary\\steamapps\\common\\Game",
            "//server/share/games/Game",
            "\\\\server\\share\\games\\Game",
        ] {
            validate_steam_install_directory(directory).unwrap();
        }
        for directory in [
            "",
            "relative/game",
            "/",
            "C:\\",
            "\\\\server\\share",
            "//server/share",
            "\\foo",
            "/games/./example",
            "/games/../example",
            " /games/example",
            "/games/example ",
            &format!("/{}", "x".repeat(1_024)),
        ] {
            assert!(
                validate_steam_install_directory(directory).is_err(),
                "accepted Steam install directory {directory:?}"
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
        let absent: ConfigOverrides = serde_json::from_value(
            json!({"sw_preset":null,"sw_tune":null,"encoder":null,"min_threads":null,"qp":null}),
        )
        .unwrap();
        assert!(absent.is_empty());
        let selected: ConfigOverrides = serde_json::from_value(
            json!({"sw_preset":"","sw_tune":"film,zerolatency","encoder":"software","min_threads":i32::MIN,"qp":i32::MAX}),
        )
        .unwrap();
        assert!(selected.validate().is_ok());
        assert!(!selected.is_empty());
        assert_eq!(
            serde_json::to_value(selected).unwrap(),
            json!({"sw_preset":"","sw_tune":"film,zerolatency","encoder":"software","min_threads":i32::MIN,"qp":i32::MAX})
        );
        for invalid in [
            json!({"sw_preset":false}),
            json!({"encoder":"nvenc"}),
            json!({"encoder":""}),
        ] {
            assert!(serde_json::from_value::<ConfigOverrides>(invalid).is_err());
        }
        for invalid in [
            json!({"sw_preset":"x".repeat(129)}),
            json!({"sw_tune":"film\n"}),
        ] {
            let options: ConfigOverrides = serde_json::from_value(invalid).unwrap();
            assert!(options.validate().is_err());
        }
        for threads in [i32::MIN, -1, 0, 1, 2, i32::MAX] {
            let options: ConfigOverrides =
                serde_json::from_value(json!({"min_threads":threads})).unwrap();
            assert!(options.validate().is_ok());
        }
        for key in ["min_threads", "qp"] {
            for value in [i64::from(i32::MIN) - 1, i64::from(i32::MAX) + 1] {
                assert!(serde_json::from_value::<ConfigOverrides>(json!({key:value})).is_err());
            }
        }
    }
}

#[cfg(test)]
mod keybindings_override_tests {
    use super::ConfigOverrides;
    use serde_json::{Value, json};

    #[test]
    fn ordered_keybindings_are_sparse_and_explicit_empty_replaces_inheritance() {
        for inherited in [json!({}), json!({"keybindings":null})] {
            let overrides: ConfigOverrides = serde_json::from_value(inherited).unwrap();
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
            let overrides: ConfigOverrides = serde_json::from_value(value.clone()).unwrap();
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
                serde_json::from_value::<ConfigOverrides>(json!({"keybindings":invalid})).is_err(),
                "{invalid}"
            );
        }
        assert!(serde_json::from_value::<ConfigOverrides>(Value::Null).is_err());
    }
}
