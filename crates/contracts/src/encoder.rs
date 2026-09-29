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
}
