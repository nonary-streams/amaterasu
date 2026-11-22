//! GameStream negotiation of a PyroWave stream's wire bandwidth.
use super::{Announce, NegotiatedVideoFormat};
use amaterasu_contracts::media_profile::{VideoProfile, VideoProfileAttributePhase};

pub const LINK_MBPS: &str = "x-ss-video[0].pyrowaveLinkMbps";

pub fn is_profile(profile: &VideoProfile) -> bool {
    profile.wire_selector == 3
        && profile.payload_type == 99
        && profile.encoding_name == "PYROWAVE"
        && profile.wire_attributes.iter().any(|attribute| {
            attribute.phase == VideoProfileAttributePhase::Describe
                && attribute.name == "x-ss-pyrowave.bitstream"
        })
}

pub fn wire_bitrate_kbps(announce: &Announce<NegotiatedVideoFormat>) -> Option<u32> {
    if !matches!(&announce.video.format, NegotiatedVideoFormat::Extension(profile)
        if is_profile(profile))
    {
        return None;
    }
    let configured = announce
        .video
        .configured_bitrate_kbps
        .filter(|rate| *rate != 0)?;
    Some(configured.min(1_000_000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::media_profile::VideoProfileSnapshot;

    #[test]
    fn moonlight_legacy_and_configured_bitrates_are_distinct() {
        let profile =
            serde_json::from_str(include_str!("../../../../scripts/pyrowave/profile.json"))
                .unwrap();
        let offer = VideoProfileSnapshot {
            profiles: vec![profile],
        };
        let sdp = include_str!("fixtures/moonlight-pyrowave10.sdp");
        let parse =
            |sdp: &str| super::super::parse_profile_announce(sdp.as_bytes(), &offer).unwrap();
        let announce = parse(sdp);
        assert_eq!(announce.video.bitrate_kbps, 100_000);
        assert_eq!(wire_bitrate_kbps(&announce), Some(695_000));
        let smaller_link = parse(&sdp.replace("pyrowaveLinkMbps:1000", "pyrowaveLinkMbps:500"));
        assert_eq!(wire_bitrate_kbps(&smaller_link), Some(400_000));
    }
}
