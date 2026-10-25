use super::*;
use amaterasu_contracts::encoder::{DynamicRange, VideoCodec, VideoStreamFormat};
const FIXTURE: &str = include_str!("fixtures/moonlight-h264.sdp");

fn changed(name: &str, value: &str) -> String {
    let prefix = format!("a={name}:");
    FIXTURE
        .split_inclusive("\r\n")
        .map(|line| {
            if line.starts_with(&prefix) {
                format!("{prefix}{value} \r\n")
            } else {
                line.into()
            }
        })
        .collect()
}

fn all_support() -> TransportSupport {
    TransportSupport {
        hevc: false,
        av1: false,
        stereo_audio: true,
        enet_control: true,
        encrypted_control: true,
        control_v2: true,
        encrypted_remote_input: true,
        encrypted_video: true,
        encrypted_audio: true,
        fec: true,
    }
}

#[test]
fn hevc_sdr_announce_is_parsed_but_requires_end_to_end_capability() {
    let hevc = changed("x-nv-vqos[0].bitStreamFormat", "1")
        .replace("a=x-nv-clientSupportHevc:0 ", "a=x-nv-clientSupportHevc:1 ");
    let announce = parse_announce(hevc.as_bytes()).unwrap();
    assert_eq!(announce.video.format.codec, VideoCodec::Hevc);
    assert_eq!(
        announce.check_support(&all_support()),
        Err(Error::Unsupported("HEVC video"))
    );
    let mut enabled = all_support();
    enabled.hevc = true;
    announce.check_support(&enabled).unwrap();

    let h264_capable = changed("x-nv-clientSupportHevc", "1");
    assert_eq!(
        parse_announce(h264_capable.as_bytes())
            .unwrap()
            .video
            .format
            .codec,
        VideoCodec::H264
    );
    assert!(parse_announce(changed("x-nv-vqos[0].bitStreamFormat", "1").as_bytes()).is_err());
}

#[test]
fn bit_stream_format_selects_h264_hevc_and_av1_and_rejects_other_codecs() {
    let av1 =
        changed("x-nv-vqos[0].bitStreamFormat", "2").replace("a=x-nv-clientSupportHevc:0 \r\n", "");
    assert!(!av1.contains("clientSupportHevc"));
    let announce = parse_announce(av1.as_bytes()).unwrap();
    assert_eq!(announce.video.format.codec, VideoCodec::Av1);
    assert_eq!(
        announce.check_support(&all_support()),
        Err(Error::Unsupported("AV1 video"))
    );
    let mut enabled = all_support();
    enabled.av1 = true;
    announce.check_support(&enabled).unwrap();
    // HEVC support alone does not admit AV1, nor AV1 support HEVC.
    enabled.av1 = false;
    enabled.hevc = true;
    assert!(announce.check_support(&enabled).is_err());

    assert_eq!(
        parse_announce(FIXTURE.as_bytes())
            .unwrap()
            .video
            .format
            .codec,
        VideoCodec::H264
    );
    for unsupported in ["3", "4", "-1"] {
        assert!(
            parse_announce(changed("x-nv-vqos[0].bitStreamFormat", unsupported).as_bytes())
                .is_err()
        );
    }
}

#[test]
fn primary_client_shape_negotiates_exact_h264_sdr_requirements() {
    let result = parse_announce(FIXTURE.as_bytes()).unwrap();
    assert_eq!(
        (result.video.width, result.video.height, result.video.fps),
        (1920, 1080, 60)
    );
    assert_eq!(result.video.format, VideoStreamFormat::H264_SDR);
    assert_eq!(result.video.bitrate_kbps, 16000);
    assert_eq!(result.video.configured_bitrate_kbps, Some(20000));
    assert_eq!(result.video.slices_per_frame, 1);
    assert_eq!(result.video.color, ColorMode::Bt709Limited);
    assert_eq!(result.video.format.codec, VideoCodec::H264);
    assert_eq!(result.packet_size, 1392);
    assert_eq!(result.audio.packet_duration_ms, 5);
    assert_eq!(result.fec.minimum_packets, 2);
    assert_eq!(result.control, ControlProtocol::EncryptedEnet);
    assert_eq!(result.client_feature_flags, 3);
    assert!(!result.encryption.video);
    assert!(result.encryption.remote_input);
    assert!(result.check_support(&TransportSupport::default()).is_err());
    result.check_support(&all_support()).unwrap();
    let mut support = all_support();
    support.fec = false;
    assert_eq!(
        result.check_support(&support),
        Err(Error::Unsupported("FEC parity"))
    );
}

#[test]
fn pcm_haptics_feature_flag_is_retained_while_unknown_bits_stay_rejected() {
    let enabled = parse_announce(changed("x-ml-general.featureFlags", "7").as_bytes()).unwrap();
    assert_eq!(enabled.client_feature_flags, 7);
    let legacy = parse_announce(changed("x-ml-general.featureFlags", "3").as_bytes()).unwrap();
    assert_eq!(legacy.client_feature_flags, 3);
    assert!(parse_announce(changed("x-ml-general.featureFlags", "8").as_bytes()).is_err());
}

#[test]
fn selected_codec_and_dynamic_range_produce_the_shared_video_format() {
    let cases = [
        (0, 0, VideoCodec::H264, 8, DynamicRange::Sdr),
        (1, 0, VideoCodec::Hevc, 8, DynamicRange::Sdr),
        (1, 1, VideoCodec::Hevc, 10, DynamicRange::Hdr),
        (2, 0, VideoCodec::Av1, 8, DynamicRange::Sdr),
        (2, 1, VideoCodec::Av1, 10, DynamicRange::Hdr),
    ];

    for (bitstream_format, range_mode, codec, bit_depth, dynamic_range) in cases {
        let announce = changed(
            "x-nv-vqos[0].bitStreamFormat",
            &bitstream_format.to_string(),
        );
        let announce = announce.replace(
            "a=x-nv-video[0].dynamicRangeMode:0 ",
            &format!("a=x-nv-video[0].dynamicRangeMode:{range_mode} "),
        );
        let announce = if codec == VideoCodec::Hevc {
            announce.replace("a=x-nv-clientSupportHevc:0 ", "a=x-nv-clientSupportHevc:1 ")
        } else {
            announce
        };
        let parsed = parse_announce(announce.as_bytes()).unwrap();
        assert_eq!(
            parsed.video.format,
            VideoStreamFormat {
                codec,
                bit_depth,
                dynamic_range,
            },
            "bitStreamFormat={bitstream_format}, dynamicRangeMode={range_mode}"
        );
    }
}

#[test]
fn unsupported_codec_and_dynamic_range_combinations_are_rejected() {
    for (bitstream_format, range_mode) in [(3, 0), (4, 0), (0, 1), (1, 2), (2, 2)] {
        let announce = changed(
            "x-nv-vqos[0].bitStreamFormat",
            &bitstream_format.to_string(),
        );
        let announce = announce.replace(
            "a=x-nv-video[0].dynamicRangeMode:0 ",
            &format!("a=x-nv-video[0].dynamicRangeMode:{range_mode} "),
        );
        assert!(
            parse_announce(announce.as_bytes()).is_err(),
            "accepted bitStreamFormat={bitstream_format}, dynamicRangeMode={range_mode}"
        );
    }
}

#[test]
fn client_csc_modes_have_distinct_matrix_and_range() {
    for (wire, expected) in [
        ("0", ColorMode::Bt601Limited),
        ("1", ColorMode::Bt601Full),
        ("2", ColorMode::Bt709Limited),
        ("3", ColorMode::Bt709Full),
    ] {
        let parsed = parse_announce(changed("x-nv-video[0].encoderCscMode", wire).as_bytes())
            .expect("supported color mode");
        assert_eq!(parsed.video.color, expected);
    }
    assert!(parse_announce(changed("x-nv-video[0].encoderCscMode", "4").as_bytes()).is_err());
}

#[test]
fn surround_audio_requires_exact_channel_mask_and_enable_pair() {
    for (channels, mask) in [(6, 63), (8, 1599)] {
        let announce = changed("x-nv-audio.surround.numChannels", &channels.to_string());
        let announce = announce.replace(
            "a=x-nv-audio.surround.channelMask:3 ",
            &format!("a=x-nv-audio.surround.channelMask:{mask} "),
        );
        let announce = announce.replace(
            "a=x-nv-audio.surround.enable:0 ",
            "a=x-nv-audio.surround.enable:1 ",
        );
        let parsed = parse_announce(announce.as_bytes()).unwrap();
        assert_eq!(
            (parsed.audio.channels, parsed.audio.channel_mask),
            (channels, mask)
        );
        for (name, value) in [
            ("x-nv-audio.surround.channelMask", "3"),
            ("x-nv-audio.surround.enable", "0"),
            ("x-nv-audio.surround.AudioQuality", "1"),
            ("x-nv-aqos.packetDuration", "20"),
        ] {
            let changed = announce.replace(
                &format!(
                    "a={name}:{} ",
                    if name == "x-nv-audio.surround.channelMask" {
                        mask.to_string()
                    } else if name == "x-nv-audio.surround.enable" {
                        "1".into()
                    } else if name == "x-nv-audio.surround.AudioQuality" {
                        "0".into()
                    } else {
                        "5".into()
                    }
                ),
                &format!("a={name}:{value} "),
            );
            assert!(
                parse_announce(changed.as_bytes()).is_err(),
                "accepted {name}={value}"
            );
        }
    }
}

#[test]
fn encryption_requirements_are_preserved_and_cannot_silently_downgrade() {
    let result = parse_announce(changed("x-ss-general.encryptionEnabled", "7").as_bytes()).unwrap();
    assert!(result.encryption.control_v2 && result.encryption.video && result.encryption.audio);
    for missing in ["video", "audio", "control"] {
        let mut support = all_support();
        match missing {
            "video" => support.encrypted_video = false,
            "audio" => support.encrypted_audio = false,
            _ => support.control_v2 = false,
        }
        assert!(result.check_support(&support).is_err());
    }
    let legacy = parse_announce(changed("x-nv-general.featureFlags", "167").as_bytes()).unwrap();
    assert!(legacy.encryption.audio);
    assert!(parse_announce(changed("x-ss-general.encryptionEnabled", "8").as_bytes()).is_err());
}

#[test]
fn unsupported_media_modes_and_invalid_numeric_boundaries_fail() {
    for (name, value) in [
        ("x-nv-vqos[0].bitStreamFormat", "3"),
        ("x-nv-vqos[0].bitStreamFormat", "4"),
        ("x-nv-video[0].dynamicRangeMode", "2"),
        ("x-ss-video[0].chromaSamplingType", "1"),
        ("x-nv-video[0].clientViewportWd", "1919"),
        ("x-nv-video[0].clientViewportHt", "2162"),
        ("x-nv-video[0].maxFPS", "0"),
        ("x-nv-video[0].maxFPS", "121"),
        ("x-nv-video[0].maxFPS", "60.0"),
        ("x-nv-video[0].maxFPS", "4294967296"),
        ("x-nv-video[0].maxFPS", "+60"),
        ("x-nv-video[0].clientRefreshRateX100", "12000"),
        ("x-nv-video[0].packetSize", "1391"),
        ("x-nv-video[0].packetSize", "65535"),
        ("x-nv-video[0].videoEncoderSlicesPerFrame", "0"),
        ("x-nv-video[0].videoEncoderSlicesPerFrame", "2147483648"),
        ("x-nv-video[0].maxNumReferenceFrames", "2"),
        ("x-nv-video[0].encoderCscMode", "4"),
        ("x-nv-audio.surround.numChannels", "4"),
        ("x-nv-audio.surround.channelMask", "63"),
        ("x-nv-audio.surround.AudioQuality", "1"),
        ("x-nv-aqos.packetDuration", "20"),
        ("x-nv-vqos[0].fec.enable", "0"),
        ("x-nv-vqos[0].fec.minRequiredFecPackets", "3"),
        ("x-nv-vqos[0].bllFec.enable", "1"),
        ("x-nv-vqos[0].drc.enable", "1"),
        ("x-nv-vqos[0].bw.maximumBitrateKbps", "15000"),
    ] {
        assert!(
            parse_announce(changed(name, value).as_bytes()).is_err(),
            "accepted {name}={value}"
        );
    }
}

#[test]
fn client_slice_count_is_parsed_and_retained_as_signed_native_int() {
    let announce =
        parse_announce(changed("x-nv-video[0].videoEncoderSlicesPerFrame", "3").as_bytes())
            .unwrap();
    assert_eq!(announce.video.slices_per_frame, 3);
    let maximum = parse_announce(
        changed("x-nv-video[0].videoEncoderSlicesPerFrame", "2147483647").as_bytes(),
    )
    .unwrap();
    assert_eq!(maximum.video.slices_per_frame, i32::MAX);
}

#[test]
fn duplicate_missing_and_critical_extensions_fail_but_bounded_extensions_survive() {
    let duplicate = format!("{FIXTURE}a=x-nv-video[0].maxFPS:60 \r\n");
    assert_eq!(
        parse_announce(duplicate.as_bytes()),
        Err(Error::DuplicateField)
    );
    let missing = FIXTURE.replace("a=x-nv-video[0].maxFPS:60 \r\n", "");
    assert_eq!(
        parse_announce(missing.as_bytes()),
        Err(Error::Missing("x-nv-video[0].maxFPS"))
    );
    for suffix in [
        "a=x-ss-video[0].intraRefresh:1\r\n",
        "a=x-nv-video[1].clientViewportWd:1920\r\n",
        "a=x-nv-video[0].newCodec:1\r\n",
    ] {
        assert!(parse_announce(format!("{FIXTURE}{suffix}").as_bytes()).is_err());
    }
    let extended = format!("{FIXTURE}a=x-example.clientHint:future value \r\n");
    assert_eq!(
        parse_announce(extended.as_bytes())
            .unwrap()
            .ignored_attributes,
        1
    );
}

#[test]
fn syntax_and_size_limits_reject_partial_binary_or_ambiguous_sdp() {
    for text in [
        FIXTURE.replace("\r\n", "\n"),
        FIXTURE.replace("t=0 0", "t=1 1"),
        FIXTURE.replace("m=video 47998  ", "m=audio 47998  "),
        FIXTURE.replace("v=0", "v=1"),
        FIXTURE.replace("m=video 47998  ", "m=video 0  "),
        format!("{FIXTURE}s=duplicate\r\n"),
        format!("{FIXTURE}a=x-test:bad\0value\r\n"),
        format!("{FIXTURE}a=x-test:{}\r\n", "a".repeat(MAX_LINE_BYTES)),
        format!(
            "{FIXTURE}{}",
            (0..MAX_ATTRIBUTES)
                .map(|index| format!("a=x-extra-{index}:0\r\n"))
                .collect::<String>()
        ),
    ] {
        assert!(parse_announce(text.as_bytes()).is_err());
    }
    assert!(parse_announce(&vec![b'x'; MAX_BYTES + 1]).is_err());
    assert!(parse_announce(b"").is_err());
    assert!(parse_announce(&FIXTURE.as_bytes()[..FIXTURE.len() - 1]).is_err());
}

#[test]
fn omitted_parity_policy_is_distinct_from_explicit_zero_and_input_encryption_is_not_doubled() {
    let result = parse_announce(FIXTURE.as_bytes()).unwrap();
    assert_eq!(result.fec.repair_percent, None);
    let mut support = all_support();
    support.encrypted_remote_input = false;
    result.check_support(&support).unwrap();
    let legacy = parse_announce(changed("x-nv-general.useReliableUdp", "1").as_bytes()).unwrap();
    assert_eq!(
        legacy.check_support(&support),
        Err(Error::Unsupported("remote input encryption"))
    );
    let zero = format!(
        "{}a=x-nv-vqos[0].fec.repairPercent:0\r\n",
        changed("x-nv-vqos[0].fec.minRequiredFecPackets", "0")
    );
    let zero = parse_announce(zero.as_bytes()).unwrap();
    assert_eq!(zero.fec.repair_percent, Some(0));
    support.fec = false;
    zero.check_support(&support).unwrap();
}
