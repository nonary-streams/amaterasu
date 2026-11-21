//! GameStream negotiation of a PyroWave stream's wire bandwidth.
use super::{Announce, NegotiatedVideoFormat};
use amaterasu_contracts::media_profile::{VideoProfile, VideoProfileAttributePhase};
use amaterasu_media::VideoFramingLimits;

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

/// Convert a total wire rate into the encoder's fixed per-frame payload budget.
/// The length-prefixed compatibility route protects all shards with FEC.
pub fn encoder_bitrate_bps(
    wire_kbps: u32,
    fps: u32,
    audio_bps: u32,
    limits: &VideoFramingLimits,
) -> Option<u32> {
    // Reserve half again for the audio parity and 500 kbps for control and audio headers.
    let audio_kbps = (u64::from(audio_bps) * 3).div_ceil(2_000);
    let video_kbps = u64::from(wire_kbps).checked_sub(audio_kbps + 500)?;
    // Ethernet framing plus IPv6, UDP, RTP and the optional video encryption header.
    let datagram_bytes = u64::from(limits.packet_size) + 134;
    let frame_wire_bytes = (video_kbps * 125).checked_div(u64::from(fps))?;
    let available_shards = (frame_wire_bytes / datagram_bytes)
        .min(limits.max_wire_bytes / datagram_bytes)
        .min(u64::from(limits.max_datagrams)) as u32;
    let parity_shards = |data: u32| {
        (data * limits.min_fec_percentage)
            .div_ceil(100)
            .max(limits.min_parity_shards)
    };
    let data_per_block = if limits.min_fec_percentage == 0 && limits.min_parity_shards == 0 {
        1023
    } else {
        (1..255)
            .rev()
            .find(|data| data + parity_shards(*data) <= 255)?
    };
    let mut available = available_shards;
    let mut data_shards = 0;
    for _ in 0..limits.max_fec_blocks {
        let data = (1..=data_per_block.min(available)).rev().find(|data| {
            let parity = parity_shards(*data);
            data + parity <= available && parity <= (data * limits.max_fec_percentage).div_ceil(100)
        });
        let Some(data) = data else { break };
        available -= data + parity_shards(data);
        data_shards += data;
    }
    let framed_bytes = u64::from(data_shards) * u64::from(limits.packet_size.checked_sub(16)?);
    // The eight-byte transport header, codec sequence header and length words
    // also occupy payload. Adjacent codec packets together exceed 1024 bytes.
    let encoder_bytes = framed_bytes.checked_sub(24 + framed_bytes.div_ceil(128))? & !3;
    if encoder_bytes < 1024 {
        return None;
    }
    (encoder_bytes * 8 * u64::from(fps)).try_into().ok()
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

    #[test]
    fn configured_rate_fits_the_real_packet_and_fec_ceiling() {
        let mut limits = VideoFramingLimits {
            packet_size: 1360,
            min_parity_shards: 2,
            min_fec_percentage: 20,
            max_fec_percentage: 255,
            max_fec_blocks: 4,
            max_datagrams: 2048,
            max_wire_bytes: 8 * 1024 * 1024,
            ..Default::default()
        };
        for fps in [60, 120] {
            let bitrate = encoder_bitrate_bps(695_000, fps, 128_000, &limits).unwrap();
            assert!((500_000_000..=525_000_000).contains(&bitrate), "{bitrate}");
            let codec_bytes = u64::from(bitrate) / (8 * u64::from(fps));
            let framed_bytes = codec_bytes + codec_bytes.div_ceil(128) + 24;
            let shards = framed_bytes.div_ceil(1344);
            let parity = (shards / 212) * 43 + ((shards % 212) * 20).div_ceil(100).max(2);
            assert!((shards + parity) * 1494 <= 694_308 * 125 / u64::from(fps));
        }
        limits.max_fec_blocks = 1;
        let one_block = encoder_bitrate_bps(695_000, 120, 128_000, &limits).unwrap();
        assert!(one_block < 272_000_000);
        assert_eq!(encoder_bitrate_bps(500, 120, 128_000, &limits), None);
    }
}
