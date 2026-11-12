//! Provider-neutral identities for an explicitly negotiated video profile.
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

impl FormatId {
    pub fn is_valid(self) -> bool {
        self.namespace_id != 0 && self.value != 0 && self.schema != 0
    }
}

/// An extension selector and its negotiated codec/framing identity. The wire
/// marker is constrained rather than accepting arbitrary SDP lines.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoProfile {
    pub codec: FormatId,
    pub framing: FormatId,
    pub wire_selector: u32,
    pub payload_type: u8,
    pub encoding_name: String,
}

impl VideoProfile {
    pub fn is_valid(&self) -> bool {
        self.codec.is_valid()
            && self.framing.is_valid()
            && self.wire_selector >= 3
            && (99..=127).contains(&self.payload_type)
            && !self.encoding_name.is_empty()
            && self.encoding_name.len() <= 32
            && self
                .encoding_name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
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
                        other.wire_selector == profile.wire_selector
                            || other.payload_type == profile.payload_type
                            || (other.codec == profile.codec && other.framing == profile.framing)
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
