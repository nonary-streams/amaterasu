//! Safe, observable compatibility choices. Wire codec and encryption are never substituted.
use super::Announce;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamAdjustment {
    pub code: String,
    pub requested: u32,
    pub effective: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl StreamAdjustment {
    pub fn new(code: &str, requested: u32, effective: u32) -> Self {
        Self {
            code: code.into(),
            requested,
            effective,
            reason: None,
        }
    }

    pub fn valid(&self) -> bool {
        self.reason.as_ref().is_none_or(|reason| {
            reason.len() <= 128
                && reason
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        }) && matches!(
            self.code.as_str(),
            "refresh_hint_ignored"
                | "intra_refresh_unavailable"
                | "audio_quality_default"
                | "hevc_capability_hint_ignored"
                | "invalid_optional_hints"
                | "optional_client_features"
                | "bandwidth_adjusted"
                | "compatibility_defaults"
                | "capture_fallback"
                | "encoder_fallback"
        )
    }
}

pub(super) fn adjustments(
    attributes: &BTreeMap<&str, &str>,
    announce: &Announce,
) -> Vec<StreamAdjustment> {
    let value = |key: &str| {
        attributes
            .get(key)
            .and_then(|value| super::syntax::decimal(value))
            .unwrap_or(0)
    };
    let mut adjustments = Vec::new();
    let refresh = value("x-nv-video[0].clientRefreshRateX100");
    if refresh != announce.video.refresh_rate_x100 {
        adjustments.push(StreamAdjustment::new(
            "refresh_hint_ignored",
            refresh,
            announce.video.refresh_rate_x100,
        ));
    }
    for (attribute, code) in [
        ("x-ss-video[0].intraRefresh", "intra_refresh_unavailable"),
        ("x-nv-audio.surround.AudioQuality", "audio_quality_default"),
    ] {
        let requested = value(attribute);
        if requested != 0 {
            adjustments.push(StreamAdjustment::new(code, requested, 0));
        }
    }
    let hevc_hint = value("x-nv-clientSupportHevc");
    if hevc_hint > 1 {
        adjustments.push(StreamAdjustment::new(
            "hevc_capability_hint_ignored",
            hevc_hint,
            0,
        ));
    }
    // One bounded bit per known non-negotiating numeric hint. Do not copy
    // arbitrary client text into diagnostics, and do not create an unbounded
    // number of adjustment records when several hints are malformed.
    let invalid_hints = OPTIONAL_NUMERIC_HINTS
        .iter()
        .enumerate()
        .filter(|(_, name)| {
            attributes
                .get(**name)
                .is_some_and(|value| super::syntax::decimal(value).is_none())
        })
        .fold(0_u32, |mask, (index, _)| mask | (1 << index));
    if invalid_hints != 0 {
        adjustments.push(StreamAdjustment::new(
            "invalid_optional_hints",
            invalid_hints,
            0,
        ));
    }
    let ignored = announce.client_feature_flags & !super::KNOWN_CLIENT_FEATURE_FLAGS;
    if ignored != 0 {
        adjustments.push(StreamAdjustment::new(
            "optional_client_features",
            ignored,
            0,
        ));
    }
    let defaults = [
        "x-nv-video[0].encoderCscMode",
        "x-nv-vqos[0].bitStreamFormat",
        "x-nv-video[0].dynamicRangeMode",
        "x-nv-aqos.packetDuration",
        "x-nv-general.useReliableUdp",
        "x-nv-general.featureFlags",
    ]
    .iter()
    .filter(|key| !attributes.contains_key(**key))
    .count();
    if defaults != 0 {
        adjustments.push(StreamAdjustment::new(
            "compatibility_defaults",
            defaults as u32,
            0,
        ));
    }
    adjustments
}

/// Bit positions in the `invalid_optional_hints` adjustment.
pub(super) const OPTIONAL_NUMERIC_HINTS: &[&str] = &[
    "x-ml-general.featureFlags",
    "x-nv-clientSupportHevc",
    "x-ss-video[0].intraRefresh",
    "x-nv-audio.surround.AudioQuality",
    "x-nv-video[0].clientRefreshRateX100",
    "x-nv-video[0].rateControlMode",
    "x-nv-vqos[0].bw.minimumBitrateKbps",
    "x-nv-video[0].initialBitrateKbps",
    "x-nv-video[0].initialPeakBitrateKbps",
    "x-ml-video.configuredBitrateKbps",
    "x-nv-video[0].timeoutLengthMs",
    "x-nv-video[0].framesWithInvalidRefThreshold",
    "x-nv-vqos[0].videoQualityScoreUpdateTime",
    "x-nv-vqos[0].qosTrafficType",
    "x-nv-aqos.qosTrafficType",
];
