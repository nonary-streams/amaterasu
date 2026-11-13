use super::{MAX_BYTES, MAX_LINES};

// Only numeric protocol settings are eligible for logging. In particular, do
// not log the SDP origin, unknown extensions, or arbitrary malformed values.
const SETTINGS: &[&str] = &[
    "x-ml-general.featureFlags",
    "x-ml-video.configuredBitrateKbps",
    "x-nv-clientSupportHevc",
    "x-nv-general.useReliableUdp",
    "x-nv-general.featureFlags",
    "x-nv-general.enableRecoveryMode",
    "x-nv-ri.useControlChannel",
    "x-ss-general.encryptionEnabled",
    "x-ss-video[0].chromaSamplingType",
    "x-ss-video[0].intraRefresh",
    "x-nv-video[0].packetSize",
    "x-nv-video[0].videoEncoderSlicesPerFrame",
    "x-nv-video[0].rateControlMode",
    "x-nv-video[0].clientViewportWd",
    "x-nv-video[0].clientViewportHt",
    "x-nv-video[0].maxFPS",
    "x-nv-video[0].clientRefreshRateX100",
    "x-nv-video[0].maxNumReferenceFrames",
    "x-nv-video[0].encoderCscMode",
    "x-nv-video[0].dynamicRangeMode",
    "x-nv-video[0].initialBitrateKbps",
    "x-nv-video[0].initialPeakBitrateKbps",
    "x-nv-vqos[0].bitStreamFormat",
    "x-nv-vqos[0].bw.maximumBitrateKbps",
    "x-nv-vqos[0].bw.minimumBitrateKbps",
    "x-nv-vqos[0].fec.enable",
    "x-nv-vqos[0].bllFec.enable",
    "x-nv-vqos[0].drc.enable",
    "x-nv-vqos[0].fec.minRequiredFecPackets",
    "x-nv-vqos[0].fec.repairPercent",
    "x-nv-audio.surround.numChannels",
    "x-nv-audio.surround.channelMask",
    "x-nv-audio.surround.enable",
    "x-nv-audio.surround.AudioQuality",
    "x-nv-aqos.packetDuration",
];

/// Extract bounded, non-secret numeric settings for a rejected announcement.
pub fn rejection_context(bytes: &[u8]) -> Vec<(&'static str, Option<u32>)> {
    if bytes.len() > MAX_BYTES {
        return Vec::new();
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Vec::new();
    };
    text.split("\r\n")
        .take(MAX_LINES)
        .filter_map(|line| {
            let (name, value) = line.strip_prefix("a=")?.split_once(':')?;
            let name = SETTINGS.iter().copied().find(|setting| *setting == name)?;
            Some((name, super::syntax::decimal(value.trim_matches(' '))))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_includes_flag_values_but_not_untrusted_text_or_extensions() {
        let body = b"a=x-ml-general.featureFlags:15\r\na=x-nv-video[0].maxFPS:120\r\na=x-nv-general.featureFlags:secret\r\na=x-secret:123\r\n";
        assert_eq!(
            rejection_context(body),
            vec![
                ("x-ml-general.featureFlags", Some(15)),
                ("x-nv-video[0].maxFPS", Some(120)),
                ("x-nv-general.featureFlags", None),
            ]
        );
        assert!(rejection_context(&vec![b'x'; MAX_BYTES + 1]).is_empty());
    }
}
