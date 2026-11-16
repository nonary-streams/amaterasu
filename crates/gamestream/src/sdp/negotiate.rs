use super::attributes::{Attributes, bounded};
use super::syntax;
use super::{Announce, Audio, ColorMode, ControlProtocol, Encryption, Error, Fec, Video};
use amaterasu_contracts::encoder::{DynamicRange, VideoCodec, VideoStreamFormat};
use amaterasu_contracts::media_profile::{
    ChromaSampling, VideoMediaFormat, VideoProfile, VideoProfileAttributePhase,
    VideoProfileSnapshot,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NegotiatedVideoFormat {
    Standard(VideoStreamFormat),
    Extension(VideoProfile),
}

pub fn parse_announce(bytes: &[u8]) -> Result<Announce, Error> {
    parse_with(bytes, video_format)
}

/// Parse the same bounded SDP document, selecting only from the trusted offer.
pub fn parse_profile_announce(
    bytes: &[u8],
    offer: &VideoProfileSnapshot,
) -> Result<Announce<NegotiatedVideoFormat>, Error> {
    if !offer.validate() {
        return Err(Error::Invalid("video profile offer"));
    }
    parse_with(bytes, |attributes| {
        let selector = attributes
            .0
            .get("x-nv-vqos[0].bitStreamFormat")
            .and_then(|value| syntax::decimal(value))
            .ok_or(Error::Invalid("video codec"))?;
        if selector < 3 {
            return video_format(attributes).map(NegotiatedVideoFormat::Standard);
        }
        attributes.required("x-nv-vqos[0].bitStreamFormat")?;
        let dynamic_range_mode = bounded(
            attributes.required("x-nv-video[0].dynamicRangeMode")?,
            0,
            1,
            "video dynamic range",
        )?;
        let chroma = chroma_sampling(attributes)?;
        let dynamic_range = if dynamic_range_mode == 1 {
            DynamicRange::Hdr
        } else {
            DynamicRange::Sdr
        };
        let media_format = VideoMediaFormat {
            bit_depth: if dynamic_range == DynamicRange::Hdr {
                10
            } else {
                8
            },
            dynamic_range,
            chroma_sampling: chroma,
        };
        let profile = offer
            .profiles
            .iter()
            .find(|profile| {
                profile.wire_selector == selector
                    && profile.media_format.unwrap_or_default() == media_format
                    && profile.wire_attributes.iter().all(|attribute| {
                        if attribute.phase == VideoProfileAttributePhase::Describe {
                            return true;
                        }
                        match attributes.0.get(attribute.name.as_str()) {
                            Some(actual) => attribute
                                .value
                                .as_deref()
                                .is_none_or(|expected| expected == *actual),
                            None => attribute.phase == VideoProfileAttributePhase::AnnounceOptional,
                        }
                    })
            })
            .ok_or(Error::Unsupported("video profile format"))?
            .clone();
        for attribute in profile
            .wire_attributes
            .iter()
            .filter(|attribute| attribute.phase != VideoProfileAttributePhase::Describe)
        {
            let Some(actual) = attributes.0.remove(attribute.name.as_str()) else {
                if attribute.phase == VideoProfileAttributePhase::AnnounceOptional {
                    continue;
                }
                return Err(Error::Missing("video profile attribute"));
            };
            if attribute
                .value
                .as_deref()
                .is_some_and(|expected| expected != actual)
            {
                return Err(Error::Unsupported("video profile attribute"));
            }
        }
        Ok(NegotiatedVideoFormat::Extension(profile))
    })
}

fn chroma_sampling(attributes: &mut Attributes<'_>) -> Result<ChromaSampling, Error> {
    match bounded(
        attributes.default("x-ss-video[0].chromaSamplingType", 0)?,
        0,
        1,
        "chroma sampling",
    )? {
        0 => Ok(ChromaSampling::Yuv420),
        1 => Ok(ChromaSampling::Yuv444),
        _ => unreachable!("bounded chroma sampling has two values"),
    }
}

fn parse_with<T>(
    bytes: &[u8],
    parse_format: impl FnOnce(&mut Attributes<'_>) -> Result<T, Error>,
) -> Result<Announce<T>, Error> {
    let document = syntax::parse(bytes)?;
    let original_attributes = document.attributes.clone();
    let mut attributes = Attributes(document.attributes);
    let format = parse_format(&mut attributes)?;
    let video = video(&mut attributes, format)?;
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
    let control = match attributes.default("x-nv-general.useReliableUdp", 1)? {
        1 => ControlProtocol::Enet,
        13 => ControlProtocol::EncryptedEnet,
        _ => return Err(Error::Unsupported("control protocol")),
    };
    let encryption = encryption(&mut attributes, control)?;
    let fec = fec(&mut attributes)?;
    let client_feature_flags = attributes.hint("x-ml-general.featureFlags").unwrap_or(0);
    // These bits advertise optional client capabilities, not required wire
    // behavior. Retain unknown bits for diagnostics; consumers select only
    // capabilities they understand. Encryption and transport checks stay strict.
    attributes.default_value("x-nv-general.enableRecoveryMode", 0)?;
    attributes.default_value("x-nv-ri.useControlChannel", 1)?;
    let ignored_attributes = attributes.finish()?;
    let mut announce = Announce {
        video,
        audio,
        packet_size,
        advertised_video_port: document.port,
        control,
        encryption,
        fec,
        client_feature_flags,
        ignored_attributes,
        adjustments: Vec::new(),
    };
    announce.adjustments = super::compatibility::adjustments(&original_attributes, &announce);
    Ok(announce)
}

fn video<T>(attributes: &mut Attributes<'_>, format: T) -> Result<Video<T>, Error> {
    // Codec support is decided by the requested format and selected provider.
    let _ = attributes.hint("x-nv-clientSupportHevc");
    let _ = attributes.hint("x-ss-video[0].intraRefresh");
    const SLICES_PER_FRAME: &str = "x-nv-video[0].videoEncoderSlicesPerFrame";
    let slices_per_frame = bounded(
        attributes.required(SLICES_PER_FRAME)?,
        1,
        i32::MAX as u32,
        SLICES_PER_FRAME,
    )? as i32;
    let _ = attributes.hint("x-nv-video[0].rateControlMode");
    let width = bounded(
        attributes.required("x-nv-video[0].clientViewportWd")?,
        16,
        8192,
        "width",
    )?;
    let height = bounded(
        attributes.required("x-nv-video[0].clientViewportHt")?,
        16,
        8192,
        "height",
    )?;
    if width % 2 != 0 || height % 2 != 0 {
        return Err(Error::Unsupported("YUV420 dimensions"));
    }
    let requested_fps = attributes.required("x-nv-video[0].maxFPS")?;
    let fps = if requested_fps > 4000 {
        requested_fps.saturating_add(500) / 1000
    } else {
        requested_fps
    };
    let fps = bounded(fps, 1, 1000, "fps")?;
    let mut refresh_rate_x100 = attributes
        .hint("x-nv-video[0].clientRefreshRateX100")
        .unwrap_or(0);
    if refresh_rate_x100 != 0
        && (refresh_rate_x100 > 100_000 || refresh_rate_x100.abs_diff(fps * 100) > 50)
    {
        refresh_rate_x100 = 0;
    }
    let (bitrate_kbps, configured_bitrate_kbps) = bitrate(attributes)?;
    let max_reference_frames = bounded(
        attributes.required("x-nv-video[0].maxNumReferenceFrames")?,
        0,
        16,
        "reference frames",
    )?;
    let color = match attributes.default("x-nv-video[0].encoderCscMode", 0)? {
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
    let codec = match attributes.default("x-nv-vqos[0].bitStreamFormat", 0)? {
        0 => VideoCodec::H264,
        1 => VideoCodec::Hevc,
        2 => VideoCodec::Av1,
        _ => return Err(Error::Unsupported("video codec")),
    };
    let dynamic_range = match attributes.default("x-nv-video[0].dynamicRangeMode", 0)? {
        0 => DynamicRange::Sdr,
        1 => DynamicRange::Hdr,
        _ => return Err(Error::Unsupported("video dynamic range")),
    };
    let chroma_sampling = match attributes.default("x-ss-video[0].chromaSamplingType", 0)? {
        0 => amaterasu_contracts::encoder::ChromaSampling::Yuv420,
        1 => amaterasu_contracts::encoder::ChromaSampling::Yuv444,
        _ => return Err(Error::Unsupported("chroma sampling")),
    };
    let format = VideoStreamFormat {
        codec,
        bit_depth: if dynamic_range == DynamicRange::Hdr {
            10
        } else {
            8
        },
        dynamic_range,
        chroma_sampling,
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
        1_000_000,
        "bitrate",
    )?;
    for name in [
        "x-nv-vqos[0].bw.minimumBitrateKbps",
        "x-nv-video[0].initialBitrateKbps",
        "x-nv-video[0].initialPeakBitrateKbps",
    ] {
        let _ = attributes.hint(name);
    }
    // Zero means unspecified. The RTSP owner accounts for transport and audio
    // overhead before selecting an encoder rate within its policy.
    let configured = attributes.hint("x-ml-video.configuredBitrateKbps");
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
    let _ = attributes.hint("x-nv-audio.surround.AudioQuality");
    let packet_duration_ms = attributes.default("x-nv-aqos.packetDuration", 5)?;
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
    let nv_flags = attributes.default("x-nv-general.featureFlags", 135)?;
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
