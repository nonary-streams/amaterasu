//! Provider-neutral identities for an explicitly negotiated video profile.
use crate::encoder::DynamicRange;
use serde::{Deserialize, Serialize};

pub const MAX_VIDEO_PROFILES: usize = 16;

/// The same namespace/value/schema tuple used by the encoder provider ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormatId {
    pub namespace_id: u64,
    pub value: u64,
    pub schema: u64,
}

/// Client-visible format facts that are independent of the codec and framing
/// provider identities. Older profiles default to the standard 8-bit 4:2:0
/// SDR mode; providers may opt in to another explicitly negotiated mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoMediaFormat {
    pub bit_depth: u8,
    pub dynamic_range: DynamicRange,
    pub chroma_sampling: ChromaSampling,
}

/// Additional provider-neutral SDP attributes for an extension profile. A
/// DESCRIBE attribute must have a fixed value. ANNOUNCE may require either an
/// exact value or presence alone (`value: None`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoProfileAttribute {
    pub phase: VideoProfileAttributePhase,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoProfileAttributePhase {
    Describe,
    Announce,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromaSampling {
    #[default]
    Yuv420,
    Yuv444,
}

impl Default for VideoMediaFormat {
    fn default() -> Self {
        Self {
            bit_depth: 8,
            dynamic_range: DynamicRange::Sdr,
            chroma_sampling: ChromaSampling::Yuv420,
        }
    }
}

impl VideoMediaFormat {
    pub fn is_valid(self) -> bool {
        matches!(
            (self.bit_depth, self.dynamic_range),
            (8, DynamicRange::Sdr) | (10, DynamicRange::Hdr)
        )
    }
}

impl FormatId {
    pub fn is_valid(self) -> bool {
        self.namespace_id != 0 && self.value != 0 && self.schema != 0
    }
}

/// An extension selector and its negotiated codec/framing identity. Media
/// format and bounded extension attributes carry profile distinctions that do
/// not fit the closed legacy codec enum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoProfile {
    pub codec: FormatId,
    pub framing: FormatId,
    pub wire_selector: u32,
    pub payload_type: u8,
    pub encoding_name: String,
    /// Absent profiles keep the historical 8-bit 4:2:0 SDR contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_format: Option<VideoMediaFormat>,
    /// Bounded SDP extension attributes which are part of the selected wire
    /// profile, rather than arbitrary lines supplied by the peer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wire_attributes: Vec<VideoProfileAttribute>,
}

impl VideoProfile {
    pub fn is_valid(&self) -> bool {
        self.codec.is_valid()
            && self.framing.is_valid()
            && self.wire_selector >= 3
            && (99..=127).contains(&self.payload_type)
            && !self.encoding_name.is_empty()
            && self.encoding_name.len() <= 32
            // Moonlight searches for the AV1 marker as a substring rather than
            // parsing an rtpmap token. Reserve suffixes as well as exact codec
            // names so an extension cannot advertise a legacy codec by accident.
            && !["AV1", "H264", "H265", "HEVC"]
                .iter()
                .any(|reserved| self.encoding_name.ends_with(reserved))
            && self
                .encoding_name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
            && self.media_format.is_none_or(VideoMediaFormat::is_valid)
            && self.wire_attributes.len() <= 16
            && self.wire_attributes.iter().enumerate().all(|(index, attribute)| {
                attribute.name.starts_with("x-")
                    && attribute.name.len() <= 128
                    && attribute.name.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || b"-_.[]".contains(&byte)
                    })
                    && !matches!(
                        attribute.name.as_str(),
                        "x-nv-vqos[0].bitStreamFormat"
                            | "x-nv-video[0].dynamicRangeMode"
                            | "x-ss-video[0].chromaSamplingType"
                    )
                    && attribute.value.as_ref().is_none_or(|value| {
                        !value.is_empty()
                            && value.len() <= 128
                            && !value.starts_with(' ')
                            && !value.ends_with(' ')
                            && value.bytes().all(|byte| (b' '..=b'~').contains(&byte))
                    })
                    && (attribute.phase != VideoProfileAttributePhase::Describe
                        || attribute.value.is_some())
                    && !self.wire_attributes[..index].iter().any(|previous| {
                        previous.phase == attribute.phase && previous.name == attribute.name
                    })
            })
    }
}

/// The composition root supplies downstream support independently of provider
/// claims. An entry is advertised only if it exactly matches this allowlist.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoProfileSnapshot {
    pub profiles: Vec<VideoProfile>,
}

impl VideoProfileSnapshot {
    pub fn validate(&self) -> bool {
        self.profiles.len() <= MAX_VIDEO_PROFILES
            && self.profiles.iter().enumerate().all(|(i, profile)| {
                profile.is_valid()
                    && !self.profiles[..i].iter().any(|other| {
                        (other.wire_selector == profile.wire_selector
                            && other.media_format.unwrap_or_default()
                                == profile.media_format.unwrap_or_default())
                            || (other.payload_type == profile.payload_type
                                && other.encoding_name != profile.encoding_name)
                    })
            })
    }

    pub fn supported_by(&self, downstream: &Self) -> bool {
        self.validate()
            && downstream.validate()
            && self
                .profiles
                .iter()
                .all(|p| downstream.profiles.contains(p))
    }
}
