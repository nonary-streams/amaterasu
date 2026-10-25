use super::attributes::{Attributes, bounded};
use super::syntax;
use super::{
    Announce, Audio, ColorMode, ControlProtocol, Encryption, Error, Fec,
    KNOWN_CLIENT_FEATURE_FLAGS, Video,
};
use amaterasu_contracts::encoder::{DynamicRange, VideoCodec, VideoStreamFormat};

pub fn parse_announce(bytes: &[u8]) -> Result<Announce, Error> {
    let document = syntax::parse(bytes)?;
    let mut attributes = Attributes(document.attributes);
    let video = video(&mut attributes)?;
    let audio = audio(&mut attributes)?;
    let packet_size = bounded(
        attributes.required("x-nv-video[0].packetSize")?,
        256,
        1392,
        "packet size",
    )?;
    if packet_size % 16 != 0 {
        return Err(Error::Unsupported("packet alignment"));
    }
    let control = match attributes.required("x-nv-general.useReliableUdp")? {
        1 => ControlProtocol::Enet,
        13 => ControlProtocol::EncryptedEnet,
        _ => return Err(Error::Unsupported("control protocol")),
    };
    let encryption = encryption(&mut attributes, control)?;
    let fec = fec(&mut attributes)?;
    let client_feature_flags = attributes.default("x-ml-general.featureFlags", 0)?;
    if client_feature_flags & !KNOWN_CLIENT_FEATURE_FLAGS != 0 {
        return Err(Error::Unsupported("client feature flags"));
    }
    attributes.default_value("x-nv-general.enableRecoveryMode", 0)?;
    attributes.default_value("x-nv-ri.useControlChannel", 1)?;
    let ignored_attributes = attributes.finish()?;
    Ok(Announce {
        video,
        audio,
        packet_size,
        advertised_video_port: document.port,
        control,
        encryption,
        fec,
        client_feature_flags,
        ignored_attributes,
    })
}

fn video(attributes: &mut Attributes<'_>) -> Result<Video, Error> {
    let format = video_format(attributes)?;
    let client_hevc = bounded(
        attributes.default("x-nv-clientSupportHevc", 0)?,
        0,
        1,
        "HEVC support",
    )?;
    if format.codec == VideoCodec::Hevc && client_hevc != 1 {
        return Err(Error::Unsupported("HEVC support"));
    }
    attributes.default_value("x-ss-video[0].chromaSamplingType", 0)?;
    attributes.default_value("x-ss-video[0].intraRefresh", 0)?;
    const SLICES_PER_FRAME: &str = "x-nv-video[0].videoEncoderSlicesPerFrame";
    let slices_per_frame = bounded(
        attributes.required(SLICES_PER_FRAME)?,
        1,
        i32::MAX as u32,
        SLICES_PER_FRAME,
    )? as i32;
    attributes.require_value("x-nv-video[0].rateControlMode", 4)?;
    let width = bounded(
        attributes.required("x-nv-video[0].clientViewportWd")?,
        16,
        3840,
        "width",
    )?;
    let height = bounded(
        attributes.required("x-nv-video[0].clientViewportHt")?,
        16,
        2160,
        "height",
    )?;
    if width % 2 != 0 || height % 2 != 0 {
        return Err(Error::Unsupported("YUV420 dimensions"));
    }
    let fps = bounded(attributes.required("x-nv-video[0].maxFPS")?, 1, 120, "fps")?;
    let refresh_rate_x100 = attributes.default("x-nv-video[0].clientRefreshRateX100", 0)?;
    if refresh_rate_x100 != 0
        && (refresh_rate_x100 > 12_000 || refresh_rate_x100.abs_diff(fps * 100) > 50)
    {
        return Err(Error::Unsupported("refresh rate conflicts with fps"));
    }
    let (bitrate_kbps, configured_bitrate_kbps) = bitrate(attributes)?;
    let max_reference_frames = bounded(
        attributes.required("x-nv-video[0].maxNumReferenceFrames")?,
        0,
        1,
        "reference frames",
    )?;
    let color = match attributes.required("x-nv-video[0].encoderCscMode")? {
        0 => ColorMode::Bt601Limited,
        1 => ColorMode::Bt601Full,
        2 => ColorMode::Bt709Limited,
        3 => ColorMode::Bt709Full,
        _ => return Err(Error::Unsupported("color mode")),
    };
    Ok(Video {
        format,
        width,
        height,
        fps,
        refresh_rate_x100,
        bitrate_kbps,
        configured_bitrate_kbps,
        slices_per_frame,
        color,
        max_reference_frames,
    })
}

fn video_format(attributes: &mut Attributes<'_>) -> Result<VideoStreamFormat, Error> {
    let codec = match attributes.required("x-nv-vqos[0].bitStreamFormat")? {
        0 => VideoCodec::H264,
        1 => VideoCodec::Hevc,
        2 => VideoCodec::Av1,
        _ => return Err(Error::Unsupported("video codec")),
    };
    let dynamic_range = match attributes.required("x-nv-video[0].dynamicRangeMode")? {
        0 => DynamicRange::Sdr,
        1 => DynamicRange::Hdr,
        _ => return Err(Error::Unsupported("video dynamic range")),
    };
    let format = VideoStreamFormat {
        codec,
        bit_depth: if dynamic_range == DynamicRange::Hdr {
            10
        } else {
            8
        },
        dynamic_range,
    };
    format
        .validate()
        .map_err(|_| Error::Unsupported("video stream format"))?;
    Ok(format)
}

fn bitrate(attributes: &mut Attributes<'_>) -> Result<(u32, Option<u32>), Error> {
    let maximum = bounded(
        attributes.required("x-nv-vqos[0].bw.maximumBitrateKbps")?,
        100,
        100_000,
        "bitrate",
    )?;
    for name in [
        "x-nv-vqos[0].bw.minimumBitrateKbps",
        "x-nv-video[0].initialBitrateKbps",
        "x-nv-video[0].initialPeakBitrateKbps",
    ] {
        if attributes.required(name)? != maximum {
            return Err(Error::Unsupported("variable bitrate negotiation"));
        }
    }
    let configured = attributes.optional("x-ml-video.configuredBitrateKbps")?;
    if let Some(value) = configured {
        bounded(value, maximum, 100_000, "configured bitrate")?;
    }
    Ok((maximum, configured))
}

fn audio(attributes: &mut Attributes<'_>) -> Result<Audio, Error> {
    let channels = attributes.required("x-nv-audio.surround.numChannels")?;
    let channel_mask = attributes.required("x-nv-audio.surround.channelMask")?;
    let enabled = attributes.required("x-nv-audio.surround.enable")?;
    if !matches!(
        (channels, channel_mask, enabled),
        (2, 3, 0) | (6, 63, 1) | (8, 1599, 1)
    ) {
        return Err(Error::Unsupported("audio channel layout"));
    }
    attributes.require_value("x-nv-audio.surround.AudioQuality", 0)?;
    let packet_duration_ms = attributes.required("x-nv-aqos.packetDuration")?;
    if !matches!(packet_duration_ms, 5 | 10) {
        return Err(Error::Unsupported("audio packet duration"));
    }
    Ok(Audio {
        channels,
        channel_mask,
        packet_duration_ms,
    })
}

fn encryption(
    attributes: &mut Attributes<'_>,
    control: ControlProtocol,
) -> Result<Encryption, Error> {
    let flags = attributes.default("x-ss-general.encryptionEnabled", 0)?;
    let nv_flags = attributes.required("x-nv-general.featureFlags")?;
    if flags & !7 != 0 || nv_flags & !0xa7 != 0 || nv_flags & 7 != 7 {
        return Err(Error::Unsupported("encryption feature flags"));
    }
    if flags & 1 != 0 && control != ControlProtocol::EncryptedEnet {
        return Err(Error::Invalid("control encryption protocol mismatch"));
    }
    Ok(Encryption {
        control_v2: flags & 1 != 0,
        video: flags & 2 != 0,
        audio: flags & 4 != 0 || nv_flags & 0x20 != 0,
        remote_input: nv_flags & 0x80 != 0,
    })
}

fn fec(attributes: &mut Attributes<'_>) -> Result<Fec, Error> {
    attributes.require_value("x-nv-vqos[0].fec.enable", 1)?;
    attributes.default_value("x-nv-vqos[0].bllFec.enable", 0)?;
    attributes.default_value("x-nv-vqos[0].drc.enable", 0)?;
    let minimum_packets = bounded(
        attributes.default("x-nv-vqos[0].fec.minRequiredFecPackets", 0)?,
        0,
        2,
        "minimum FEC packets",
    )?;
    let repair_percent = attributes
        .optional("x-nv-vqos[0].fec.repairPercent")?
        .map(|value| bounded(value, 0, 100, "FEC repair percent"))
        .transpose()?;
    Ok(Fec {
        minimum_packets,
        repair_percent,
    })
}
