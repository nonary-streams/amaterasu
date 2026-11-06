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
}
