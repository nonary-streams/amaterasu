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
            .and_then(|value| value.trim().parse::<u32>().ok())
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
