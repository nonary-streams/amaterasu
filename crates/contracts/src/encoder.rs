//! Additive encoder selection shapes shared by stream control consumers.
use serde::{Deserialize, Serialize};

pub const SOFTWARE_ENCODER_ID: &str = "amaterasu.software.x264";

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
}

impl SoftwareEncoderOptions {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_software_options(self.sw_preset.as_deref(), self.sw_tune.as_deref())
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
        }
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
        };
        let client: SoftwareEncoderOptions =
            serde_json::from_value(json!({"sw_preset":"","sw_tune":null,"encoder":null})).unwrap();
        let effective = global.with_overrides(&app).with_overrides(&client);
        assert_eq!(effective.effective_preset(), "");
        assert_eq!(effective.effective_tune(), "film,zerolatency");
        assert_eq!(effective.encoder, Some(EncoderBackend::Software));
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
                    encoder: None
                }
                .validate()
                .is_err()
            );
        }
    }
}
