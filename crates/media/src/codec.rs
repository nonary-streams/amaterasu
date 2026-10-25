use serde::{Deserialize, Serialize};

/// The coded video format carried by the unchanged `EncodedPacket.codec` ABI.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    #[default]
    H264,
    Hevc,
    Av1,
}

impl VideoCodec {
    pub const fn is_default(&self) -> bool {
        matches!(self, Self::H264)
    }

    pub const fn abi_id(self) -> u32 {
        match self {
            Self::H264 => crate::CODEC_H264,
            Self::Hevc => crate::CODEC_HEVC,
            Self::Av1 => crate::CODEC_AV1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_defaults_and_abi_ids_are_stable() {
        assert!(VideoCodec::default().is_default());
        assert_eq!(VideoCodec::H264.abi_id(), crate::CODEC_H264);
        assert_eq!(VideoCodec::Hevc.abi_id(), crate::CODEC_HEVC);
        assert_eq!(
            serde_json::to_string(&VideoCodec::Hevc).unwrap(),
            "\"hevc\""
        );
        assert_eq!(VideoCodec::Av1.abi_id(), crate::CODEC_AV1);
        assert_eq!(
            serde_json::from_str::<VideoCodec>("\"av1\"").unwrap(),
            VideoCodec::Av1
        );
        assert!(serde_json::from_str::<VideoCodec>("\"vp9\"").is_err());
    }
}
