//! Capture-source representation, independent of encoder codec/output policy.
use crate::{DynamicRange, ModuleResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceFormat {
    pub bit_depth: u8,
    pub dynamic_range: DynamicRange,
}
impl SourceFormat {
    pub fn validate(&self) -> ModuleResult<()> {
        if !matches!(self.bit_depth, 8 | 10)
            || (self.dynamic_range == DynamicRange::Hdr && self.bit_depth != 10)
        {
            return Err("unsupported capture source format".into());
        }
        Ok(())
    }
}

/// Frozen capture-route context, separate from encoder configuration and codec.
/// The trusted consumer resolves `route_key` against signed provider/class
/// composition; this request never chooses a native service or proves ownership.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceCaptureContext {
    pub route_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_mode: Option<super::SourceMode>,
}
impl SourceCaptureContext {
    pub fn validate(&self) -> ModuleResult<()> {
        if self.route_key.is_empty()
            || self.route_key.len() > 128
            || self.route_key.chars().any(char::is_control)
        {
            return Err("invalid source capture route".into());
        }
        if let Some(mode) = self.source_mode {
            super::SourcePreflightRequest {
                source_class: super::SourceClass::DesktopOutput,
                route: None,
                mode,
            }
            .validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_format_has_no_encoder_codec_and_validates_hdr_depth_independently() {
        for (bit_depth, dynamic_range, valid) in [
            (8, DynamicRange::Sdr, true),
            (10, DynamicRange::Sdr, true),
            (10, DynamicRange::Hdr, true),
            (8, DynamicRange::Hdr, false),
            (12, DynamicRange::Hdr, false),
        ] {
            assert_eq!(
                SourceFormat {
                    bit_depth,
                    dynamic_range
                }
                .validate()
                .is_ok(),
                valid
            );
        }
        assert!(
            serde_json::from_value::<SourceFormat>(
                serde_json::json!({"bit_depth":10,"dynamic_range":"sdr","codec":"hevc"})
            )
            .is_err()
        );
    }
    #[test]
    fn source_context_preserves_an_opaque_route_and_validates_optional_frozen_mode() {
        let mut context = SourceCaptureContext {
            route_key: "third-party.capture-route/2".into(),
            source_mode: None,
        };
        context.validate().unwrap();
        context.source_mode = Some(super::super::SourceMode {
            width: 1920,
            height: 1080,
            refresh_millihz: 60_000,
            hdr: false,
            bit_depth: Some(10),
        });
        context.validate().unwrap();
        context.source_mode.as_mut().unwrap().hdr = true;
        context.source_mode.as_mut().unwrap().bit_depth = Some(8);
        assert!(context.validate().is_err());
        context.source_mode = None;
        context.route_key = "x".repeat(129);
        assert!(context.validate().is_err());
    }
}
