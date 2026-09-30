//! Additive encoder selection shapes shared by stream control consumers.
use serde::{Deserialize, Serialize};

pub const SOFTWARE_ENCODER_ID: &str = "amaterasu.software.x264";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DynamicRange {
    #[default]
    Sdr,
    Hdr,
}

/// Immutable codec facts selected from the client's negotiation and host policy.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VideoStreamFormat {
    pub codec: VideoCodec,
    pub bit_depth: u8,
    pub dynamic_range: DynamicRange,
}

impl VideoStreamFormat {
    pub const H264_SDR: Self = Self {
        codec: VideoCodec::H264,
        bit_depth: 8,
        dynamic_range: DynamicRange::Sdr,
    };

    pub fn validate(self) -> Result<(), &'static str> {
        match (self.codec, self.bit_depth, self.dynamic_range) {
            (VideoCodec::H264, 8, DynamicRange::Sdr)
            | (VideoCodec::Hevc | VideoCodec::Av1, 8, DynamicRange::Sdr)
            | (VideoCodec::Hevc | VideoCodec::Av1, 10, DynamicRange::Sdr | DynamicRange::Hdr) => {
                Ok(())
            }
            _ => Err("unsupported_video_format"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NvencTwoPass {
    Disabled,
    #[default]
    QuarterRes,
    FullRes,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NvencSplitEncode {
    #[default]
    #[serde(alias = "driver_decides")]
    Auto,
    Enabled,
    Disabled,
}

/// Effective hardware-encoder policy after global, app, and client resolution.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct NvencSettings {
    pub nvenc_preset: u8,
    pub nvenc_h264_cavlc: bool,
    pub nvenc_twopass: NvencTwoPass,
    pub nvenc_vbv_increase: u32,
    pub nvenc_split_encode: NvencSplitEncode,
    pub nvenc_realtime_hags: bool,
    pub nvenc_opengl_vulkan_on_dxgi: bool,
    pub nvenc_latency_over_power: bool,
    pub vaapi_strict_rc_buffer: bool,
}

impl Default for NvencSettings {
    fn default() -> Self {
        Self {
            nvenc_preset: 1,
            nvenc_h264_cavlc: false,
            nvenc_twopass: NvencTwoPass::QuarterRes,
            nvenc_vbv_increase: 0,
            nvenc_split_encode: NvencSplitEncode::Auto,
            nvenc_realtime_hags: true,
            nvenc_opengl_vulkan_on_dxgi: true,
            nvenc_latency_over_power: true,
            vaapi_strict_rc_buffer: false,
        }
    }
}

impl NvencSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=7).contains(&self.nvenc_preset) || self.nvenc_vbv_increase > 400 {
            return Err("invalid_nvenc_settings");
        }
        Ok(())
    }

    pub fn with_overrides(&self, overrides: &NvencSettingsOverrides) -> Self {
        Self {
            nvenc_preset: overrides.nvenc_preset.unwrap_or(self.nvenc_preset),
            nvenc_h264_cavlc: overrides.nvenc_h264_cavlc.unwrap_or(self.nvenc_h264_cavlc),
            nvenc_twopass: overrides.nvenc_twopass.unwrap_or(self.nvenc_twopass),
            nvenc_vbv_increase: overrides
                .nvenc_vbv_increase
                .unwrap_or(self.nvenc_vbv_increase),
            nvenc_split_encode: overrides
                .nvenc_split_encode
                .unwrap_or(self.nvenc_split_encode),
            nvenc_realtime_hags: overrides
                .nvenc_realtime_hags
                .unwrap_or(self.nvenc_realtime_hags),
            nvenc_opengl_vulkan_on_dxgi: overrides
                .nvenc_opengl_vulkan_on_dxgi
                .unwrap_or(self.nvenc_opengl_vulkan_on_dxgi),
            nvenc_latency_over_power: overrides
                .nvenc_latency_over_power
                .unwrap_or(self.nvenc_latency_over_power),
            vaapi_strict_rc_buffer: overrides
                .vaapi_strict_rc_buffer
                .unwrap_or(self.vaapi_strict_rc_buffer),
        }
    }
}

/// Independent sparse app/client overrides; `None` inherits from the lower scope.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct NvencSettingsOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nvenc_preset: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nvenc_h264_cavlc: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nvenc_twopass: Option<NvencTwoPass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nvenc_vbv_increase: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nvenc_split_encode: Option<NvencSplitEncode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nvenc_realtime_hags: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nvenc_opengl_vulkan_on_dxgi: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nvenc_latency_over_power: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vaapi_strict_rc_buffer: Option<bool>,
}

impl NvencSettingsOverrides {
    pub fn is_empty(&self) -> bool {
        self.nvenc_preset.is_none()
            && self.nvenc_h264_cavlc.is_none()
            && self.nvenc_twopass.is_none()
            && self.nvenc_vbv_increase.is_none()
            && self.nvenc_split_encode.is_none()
            && self.nvenc_realtime_hags.is_none()
            && self.nvenc_opengl_vulkan_on_dxgi.is_none()
            && self.nvenc_latency_over_power.is_none()
            && self.vaapi_strict_rc_buffer.is_none()
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .nvenc_preset
            .is_some_and(|preset| !(1..=7).contains(&preset))
            || self
                .nvenc_vbv_increase
                .is_some_and(|increase| increase > 400)
        {
            return Err("invalid_nvenc_settings");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct EncoderIdentity {
    pub clsid: String,
    pub hardware_url: Option<String>,
    pub friendly_name: String,
}

impl EncoderIdentity {
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid_text = |value: &str, limit: usize| {
            !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
        };
        if !valid_text(&self.clsid, 128)
            || !valid_text(&self.friendly_name, 512)
            || self
                .hardware_url
                .as_deref()
                .is_some_and(|url| !valid_text(url, 2048))
        {
            return Err("invalid_encoder_identity");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EncoderBackend {
    Auto,
    Software,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum EncoderSelection {
    Identity(EncoderIdentity),
    Index(u32),
    Backend(EncoderBackend),
}

impl EncoderSelection {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Identity(identity) => identity.validate(),
            Self::Index(_) | Self::Backend(_) => Ok(()),
        }
    }
}

/// Nullable settings remain sparse: null/absence inherit, an empty string is explicit.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SoftwareEncoderOptions {
    pub sw_preset: Option<String>,
    pub sw_tune: Option<String>,
    pub encoder: Option<EncoderBackend>,
    /// Signed thread minimum request; `None` inherits the default of two.
    /// The reference accepts any C `int`; software applies it as a lower bound
    /// against the client's positive negotiated slices per frame.
    pub min_threads: Option<i32>,
}

impl SoftwareEncoderOptions {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_software_options(self.sw_preset.as_deref(), self.sw_tune.as_deref())?;
        validate_min_threads(self.min_threads)
    }

    /// Each supplied field overrides independently; null/absence retain the lower layer.
    pub fn with_overrides(&self, overrides: &Self) -> Self {
        Self {
            sw_preset: overrides
                .sw_preset
                .clone()
                .or_else(|| self.sw_preset.clone()),
            sw_tune: overrides.sw_tune.clone().or_else(|| self.sw_tune.clone()),
            encoder: overrides.encoder.or(self.encoder),
            min_threads: overrides.min_threads.or(self.min_threads),
        }
    }

    pub fn effective_threads(&self) -> i32 {
        self.min_threads.unwrap_or(2)
    }

    pub fn effective_preset(&self) -> &str {
        self.sw_preset.as_deref().unwrap_or("superfast")
    }

    pub fn effective_tune(&self) -> &str {
        self.sw_tune.as_deref().unwrap_or("zerolatency")
    }
}

/// Syntax bounds are independent of the installed codec's preset/tune vocabulary.
/// The real software backend validates exact strings when opening an encoder.
pub fn validate_software_options(
    preset: Option<&str>,
    tune: Option<&str>,
) -> Result<(), &'static str> {
    for (value, limit) in [(preset, 128), (tune, 256)] {
        if value.is_some_and(|value| value.len() > limit || value.chars().any(char::is_control)) {
            return Err("invalid_software_encoder_options");
        }
    }
    Ok(())
}

pub fn validate_min_threads(_threads: Option<i32>) -> Result<(), &'static str> {
    // Serde's i32 representation preserves the reference C-int domain exactly.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encoder_selection_preserves_legacy_and_additive_wire_shapes() {
        for value in [
            json!(0),
            json!(4294967295_u32),
            json!("auto"),
            json!("software"),
            json!({"clsid":"test","hardware_url":null,"friendly_name":"H.264"}),
        ] {
            let selection: EncoderSelection = serde_json::from_value(value.clone()).unwrap();
            assert!(selection.validate().is_ok());
            assert_eq!(serde_json::to_value(selection).unwrap(), value);
        }
        for value in [
            json!(-1),
            json!(1.0),
            json!(true),
            json!(null),
            json!("nvenc"),
            json!("SOFTWARE"),
            json!({"clsid":"test","friendly_name":"H.264","unknown":true}),
        ] {
            assert!(serde_json::from_value::<EncoderSelection>(value).is_err());
        }
        let malformed: EncoderSelection =
            serde_json::from_value(json!({"clsid":"","friendly_name":"H.264"})).unwrap();
        assert_eq!(malformed.validate(), Err("invalid_encoder_identity"));
    }
    #[test]
    fn software_options_preserve_empty_strings_and_resolve_independent_scopes() {
        let global = SoftwareEncoderOptions::default();
        assert_eq!(
            (global.effective_preset(), global.effective_tune()),
            ("superfast", "zerolatency")
        );
        let app = SoftwareEncoderOptions {
            sw_preset: Some("slow".into()),
            sw_tune: Some("film,zerolatency".into()),
            encoder: Some(EncoderBackend::Software),
            min_threads: Some(8),
        };
        let client: SoftwareEncoderOptions = serde_json::from_value(
            json!({"sw_preset":"","sw_tune":null,"encoder":null,"min_threads":null}),
        )
        .unwrap();
        let effective = global.with_overrides(&app).with_overrides(&client);
        assert_eq!(effective.effective_preset(), "");
        assert_eq!(effective.effective_tune(), "film,zerolatency");
        assert_eq!(effective.encoder, Some(EncoderBackend::Software));
        assert_eq!(effective.min_threads, Some(8));
        assert_eq!(global.effective_threads(), 2);
        assert_eq!(effective.effective_threads(), 8);
        assert!(effective.validate().is_ok());
        for invalid in [
            json!({"sw_preset":false}),
            json!({"encoder":"nvenc"}),
            json!({"encoder":""}),
        ] {
            assert!(serde_json::from_value::<SoftwareEncoderOptions>(invalid).is_err());
        }
        for (preset, tune) in [
            (Some("a".repeat(129)), None),
            (None, Some("a".repeat(257))),
            (Some("slow\n".into()), None),
        ] {
            assert!(
                SoftwareEncoderOptions {
                    sw_preset: preset,
                    sw_tune: tune,
                    encoder: None,
                    min_threads: None,
                }
                .validate()
                .is_err()
            );
        }
        for threads in [i32::MIN, -1, 0, 1, 2, 256, i32::MAX] {
            let invalid: SoftwareEncoderOptions =
                serde_json::from_value(json!({"min_threads":threads})).unwrap();
            assert!(invalid.validate().is_ok());
            assert_eq!(invalid.effective_threads(), threads);
        }
        for threads in [i64::from(i32::MIN) - 1, i64::from(i32::MAX) + 1] {
            assert!(
                serde_json::from_value::<SoftwareEncoderOptions>(json!({"min_threads":threads}))
                    .is_err()
            );
        }
    }

    #[test]
    fn nvenc_settings_use_reference_defaults_and_resolve_sparse_scopes() {
        let global = NvencSettings::default();
        assert_eq!(
            (
                global.nvenc_preset,
                global.nvenc_twopass,
                global.nvenc_split_encode,
                global.nvenc_realtime_hags,
                global.nvenc_opengl_vulkan_on_dxgi,
                global.nvenc_latency_over_power,
                global.vaapi_strict_rc_buffer,
            ),
            (
                1,
                NvencTwoPass::QuarterRes,
                NvencSplitEncode::Auto,
                true,
                true,
                true,
                false,
            )
        );
        assert!(global.validate().is_ok());

        let app: NvencSettingsOverrides = serde_json::from_value(json!({
            "nvenc_preset": 7,
            "nvenc_twopass": "full_res",
            "vaapi_strict_rc_buffer": true
        }))
        .unwrap();
        let client: NvencSettingsOverrides = serde_json::from_value(json!({
            "nvenc_preset": null,
            "nvenc_twopass": "disabled",
            "nvenc_split_encode": "driver_decides",
            "vaapi_strict_rc_buffer": false
        }))
        .unwrap();
        assert!(app.validate().is_ok());
        assert!(client.validate().is_ok());
        let effective = global.with_overrides(&app).with_overrides(&client);
        assert_eq!(effective.nvenc_preset, 7);
        assert_eq!(effective.nvenc_twopass, NvencTwoPass::Disabled);
        assert_eq!(effective.nvenc_split_encode, NvencSplitEncode::Auto);
        assert!(!effective.vaapi_strict_rc_buffer);
        assert_eq!(
            serde_json::to_value(&effective).unwrap()["nvenc_split_encode"],
            "auto"
        );

        for invalid in [json!({"nvenc_preset": 0}), json!({"nvenc_preset": 8})] {
            let overrides: NvencSettingsOverrides = serde_json::from_value(invalid).unwrap();
            assert_eq!(overrides.validate(), Err("invalid_nvenc_settings"));
        }
        let invalid: NvencSettingsOverrides =
            serde_json::from_value(json!({"nvenc_vbv_increase": 401})).unwrap();
        assert_eq!(invalid.validate(), Err("invalid_nvenc_settings"));
        for invalid in [
            json!({"nvenc_h264_cavlc": 0}),
            json!({"nvenc_twopass": "quarter_resolution"}),
            json!({"nvenc_split_encode": "force"}),
            json!({"vaapi_strict_rc_buffer": "enabled"}),
        ] {
            assert!(serde_json::from_value::<NvencSettingsOverrides>(invalid).is_err());
        }
    }

    #[test]
    fn video_stream_format_rejects_unsupported_depth_and_hdr_combinations() {
        for format in [
            VideoStreamFormat::H264_SDR,
            VideoStreamFormat {
                codec: VideoCodec::Hevc,
                bit_depth: 8,
                dynamic_range: DynamicRange::Sdr,
            },
            VideoStreamFormat {
                codec: VideoCodec::Av1,
                bit_depth: 10,
                dynamic_range: DynamicRange::Hdr,
            },
        ] {
            assert!(format.validate().is_ok());
        }
        for format in [
            VideoStreamFormat {
                codec: VideoCodec::H264,
                bit_depth: 10,
                dynamic_range: DynamicRange::Hdr,
            },
            VideoStreamFormat {
                codec: VideoCodec::Hevc,
                bit_depth: 8,
                dynamic_range: DynamicRange::Hdr,
            },
            VideoStreamFormat {
                codec: VideoCodec::Av1,
                bit_depth: 12,
                dynamic_range: DynamicRange::Sdr,
            },
        ] {
            assert_eq!(format.validate(), Err("unsupported_video_format"));
        }
    }
}
