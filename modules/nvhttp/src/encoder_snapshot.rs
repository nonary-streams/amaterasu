//! The encoder capability snapshot and the codec offer it allows per launch.
//!
//! The snapshot is the `encoders` list of an `amaterasu.encode/1` enumerate
//! reply, reduced to what codec advertisement needs: each entry's codec and
//! whether it is a hardware encoder. It drives the serverinfo codec mask and
//! the codecs RTSP may offer a launched client. A codec is never offered when
//! no enumerated encoder produces it.
use amaterasu_media::VideoCodec;
use amaterasu_module_sdk::{ChromaSampling, DynamicRange, VideoStreamFormat};
use amaterasu_module_sdk::{EncoderBackend, EncoderIdentity, EncoderSelection, encode::ModeLimit};
use serde_json::Value;

/// Moonlight's base `ServerCodecModeSupport` bits (Limelight.h `SCM_*`).
const SCM_H264: u32 = 0x0000_0001;
const SCM_HEVC: u32 = 0x0000_0100;
const SCM_AV1_MAIN8: u32 = 0x0001_0000;
const SCM_HEVC_MAIN10: u32 = 0x0000_0200;
const SCM_AV1_MAIN10: u32 = 0x0002_0000;

/// Moonlight treats any nonzero `MaxLumaPixelsHEVC` as HEVC encode support and
/// zero as none. This is the luma rate Sunshine reports for HEVC hosts.
const HEVC_MAX_LUMA_PIXELS: u64 = 1_869_449_984;

/// Higher software modes merit a performance advisory before playback.
const SOFTWARE_ADVISORY_THRESHOLD: ModeLimit = ModeLimit {
    width: 1920,
    height: 1080,
    fps: 60,
};

/// Matches the video module's bound on one enumeration reply.
const MAX_ENCODERS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
struct EncoderEntry {
    /// `None` for a codec this host cannot stream; it still holds its index.
    codec: Option<VideoCodec>,
    identity: Option<EncoderIdentity>,
    hardware: bool,
    mode_limit: Option<ModeLimit>,
    hdr10: bool,
    sdr10: bool,
    yuv444: bool,
    yuv444_10bit: bool,
}

/// Enumerated encoders in registry order, so a numeric encoder selection
/// refers to the same entry the video module will open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EncoderSnapshot {
    entries: Vec<EncoderEntry>,
    transport_codec_mask: u32,
}

/// The codecs one launch may negotiate, in Moonlight's preference order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecOffer {
    pub codecs: Vec<VideoCodec>,
    pub ten_bit_formats: Vec<VideoStreamFormat>,
    /// Bound shared by every offered codec with a finite provider limit.
    pub mode_limit: Option<ModeLimit>,
    pub warn_software_performance: bool,
}

/// Values serverinfo advertises for the host's video codecs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecAdvertisement {
    pub codec_mode_support: u32,
    pub max_luma_pixels_hevc: u64,
}

impl CodecAdvertisement {
    /// What a host with streaming disabled reports.
    pub const NONE: Self = Self {
        codec_mode_support: 0,
        max_luma_pixels_hevc: 0,
    };
}

pub use amaterasu_module_sdk::encode::ModeLimit as StreamMode;

impl EncoderSnapshot {
    /// Parse an enumerate reply. A disabled encoder module yields an empty
    /// snapshot; a malformed reply is an error so the last good one is kept.
    pub fn parse(payload: &Value) -> Result<Self, &'static str> {
        if payload.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err("encoder_enumeration_failed");
        }
        if payload.get("enabled").and_then(Value::as_bool) == Some(false) {
            return Ok(Self::default());
        }
        let encoders = payload
            .get("encoders")
            .and_then(Value::as_array)
            .filter(|encoders| encoders.len() <= MAX_ENCODERS)
            .ok_or("invalid_encoder_enumeration")?;
        let entries = encoders
            .iter()
            .map(encoder_entry)
            .collect::<Result<_, _>>()?;
        let mut transport_codec_mask = 0;
        if let Some(extensions) = payload.get("transport_extensions") {
            let extensions = extensions
                .as_array()
                .filter(|list| list.len() <= 16)
                .ok_or("invalid_transport_extensions")?;
            for extension in extensions {
                let mask = extension["codec_mode_mask"]
                    .as_u64()
                    .filter(|mask| *mask <= u32::MAX as u64)
                    .ok_or("invalid_transport_extensions")? as u32;
                if mask & !0x07800000 != 0 {
                    return Err("invalid_transport_extensions");
                }
                transport_codec_mask |= mask;
            }
        }
        Ok(Self {
            entries,
            transport_codec_mask,
        })
    }

    /// The serverinfo codec mask. H.264 is always present while streaming is
    /// enabled: Moonlight refuses to start against a zero mask, and a host
    /// without an H.264 encoder refuses the launch with a clear error instead.
    pub fn advertisement(&self) -> CodecAdvertisement {
        let mut codec_mode_support = SCM_H264 | self.transport_codec_mask;
        if self.has(VideoCodec::Hevc) {
            codec_mode_support |= SCM_HEVC;
        }
        if self.has(VideoCodec::Av1) {
            codec_mode_support |= SCM_AV1_MAIN8;
        }
        if self
            .entries
            .iter()
            .any(|e| e.codec == Some(VideoCodec::Hevc) && e.hdr10)
        {
            codec_mode_support |= SCM_HEVC_MAIN10;
        }
        if self
            .entries
            .iter()
            .any(|e| e.codec == Some(VideoCodec::Av1) && e.hdr10)
        {
            codec_mode_support |= SCM_AV1_MAIN10;
        }
        for entry in &self.entries {
            if entry.yuv444 {
                codec_mode_support |= match entry.codec {
                    Some(VideoCodec::H264) => 0x0004_0000,
                    Some(VideoCodec::Hevc) => 0x0008_0000,
                    Some(VideoCodec::Av1) => 0x0020_0000,
                    None => 0,
                };
            }
            if entry.hdr10 && entry.yuv444_10bit {
                codec_mode_support |= match entry.codec {
                    Some(VideoCodec::Hevc) => 0x0010_0000,
                    Some(VideoCodec::Av1) => 0x0040_0000,
                    _ => 0,
                };
            }
        }
        CodecAdvertisement {
            codec_mode_support,
            max_luma_pixels_hevc: if self.has(VideoCodec::Hevc) {
                HEVC_MAX_LUMA_PIXELS
            } else {
                0
            },
        }
    }

    /// Codecs whose encoders can serve `mode`. A numeric selection pins one
    /// enumerated encoder and so exactly its codec; automatic selection
    /// offers every codec with a capable encoder.
    pub fn offer(
        &self,
        selection: EncoderSelection,
        mode: StreamMode,
    ) -> Result<CodecOffer, &'static str> {
        self.offer_for_range(selection, mode, DynamicRange::Sdr)
    }

    fn offer_for_range(
        &self,
        selection: EncoderSelection,
        mode: StreamMode,
        dynamic_range: DynamicRange,
    ) -> Result<CodecOffer, &'static str> {
        let candidates: Vec<EncoderEntry> = match selection {
            EncoderSelection::Index(index) => {
                let entry = self
                    .entries
                    .get(index as usize)
                    .ok_or("no video encoder is available")?;
                vec![entry.clone()]
            }
            EncoderSelection::Identity(identity) => self
                .entries
                .iter()
                .filter(|entry| entry.identity.as_ref() == Some(&identity))
                .cloned()
                .collect(),
            EncoderSelection::Backend(EncoderBackend::Auto) => self.entries.clone(),
            EncoderSelection::Backend(EncoderBackend::Software) => self
                .entries
                .iter()
                .filter(|entry| !entry.hardware)
                .cloned()
                .collect(),
        };
        let mut codecs = Vec::new();
        let mut ten_bit_formats = Vec::new();
        let mut exceeds_provider_limit = false;
        let mut warn_software_performance = false;
        let mut mode_limit: Option<ModeLimit> = None;
        for codec in [VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264] {
            let mut entries = candidates.iter().filter(|entry| {
                entry.codec == Some(codec)
                    && (dynamic_range == DynamicRange::Sdr
                        || (codec != VideoCodec::H264 && entry.hdr10))
            });
            let mode_entries = entries
                .clone()
                .filter(|entry| entry.mode_limit.is_none_or(|limit| limit.supports(mode)));
            let best = mode_entries.clone().max_by_key(encoder_priority);
            if let Some(entry) = best {
                codecs.push(codec);
                if let Some(codec) = match codec {
                    VideoCodec::H264 => None,
                    VideoCodec::Hevc => Some(amaterasu_module_sdk::VideoCodec::Hevc),
                    VideoCodec::Av1 => Some(amaterasu_module_sdk::VideoCodec::Av1),
                } {
                    for chroma_sampling in [ChromaSampling::Yuv420, ChromaSampling::Yuv444] {
                        let witness = mode_entries
                            .clone()
                            .filter(|entry| {
                                let range = match dynamic_range {
                                    DynamicRange::Sdr => entry.sdr10,
                                    DynamicRange::Hdr => entry.hdr10,
                                };
                                range
                                    && (chroma_sampling == ChromaSampling::Yuv420
                                        || entry.yuv444_10bit)
                            })
                            .max_by_key(encoder_priority);
                        if let Some(witness) = witness {
                            if let Some(limit) = witness.mode_limit {
                                mode_limit = Some(
                                    mode_limit.map_or(limit, |current| current.intersect(limit)),
                                );
                            }
                            ten_bit_formats.push(VideoStreamFormat {
                                codec,
                                bit_depth: 10,
                                chroma_sampling,
                                dynamic_range,
                            });
                        }
                    }
                }
                warn_software_performance |=
                    !entry.hardware && !SOFTWARE_ADVISORY_THRESHOLD.supports(mode);
                if let Some(limit) = entry.mode_limit {
                    mode_limit = Some(mode_limit.map_or(limit, |current| current.intersect(limit)));
                }
            } else {
                exceeds_provider_limit |= entries.next().is_some();
            }
        }
        if codecs.is_empty() {
            return Err(if exceeds_provider_limit {
                "stream mode exceeds selected encoder capabilities"
            } else {
                "no video encoder is available"
            });
        }
        Ok(CodecOffer {
            codecs,
            ten_bit_formats,
            mode_limit,
            warn_software_performance,
        })
    }

    pub fn hdr_offer(
        &self,
        selection: EncoderSelection,
        mode: StreamMode,
    ) -> Result<CodecOffer, &'static str> {
        self.offer_for_range(selection, mode, DynamicRange::Hdr)
    }

    fn has(&self, codec: VideoCodec) -> bool {
        self.entries.iter().any(|entry| entry.codec == Some(codec))
    }
}

fn encoder_priority(entry: &&EncoderEntry) -> (bool, u128) {
    let size = entry.mode_limit.map_or(u128::MAX, |limit| {
        u128::from(limit.width) * u128::from(limit.height) * u128::from(limit.fps)
    });
    (entry.hardware, size)
}

/// Baseline offer until enumeration completes. Exact media setup is still required.
pub fn unknown_encoders_offer() -> CodecOffer {
    CodecOffer {
        codecs: vec![VideoCodec::H264],
        ten_bit_formats: Vec::new(),
        mode_limit: None,
        warn_software_performance: false,
    }
}

fn encoder_entry(encoder: &Value) -> Result<EncoderEntry, &'static str> {
    let codec = encoder
        .get("codec")
        .and_then(Value::as_str)
        .ok_or("invalid_encoder_enumeration")?;
    let hardware = encoder
        .get("hardware")
        .and_then(Value::as_bool)
        .ok_or("invalid_encoder_enumeration")?;
    let mode_limit = encoder
        .get("mode_limit")
        .cloned()
        .map(serde_json::from_value::<ModeLimit>)
        .transpose()
        .map_err(|_| "invalid_encoder_enumeration")?;
    if mode_limit.is_some_and(|limit| !limit.is_valid()) {
        return Err("invalid_encoder_enumeration");
    }
    Ok(EncoderEntry {
        identity: encoder
            .get("identity")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| "invalid_encoder_enumeration")?,
        codec: match codec {
            "h264" => Some(VideoCodec::H264),
            "hevc" => Some(VideoCodec::Hevc),
            "av1" => Some(VideoCodec::Av1),
            _ => None,
        },
        hardware,
        mode_limit,
        yuv444: encoder["yuv444"].as_bool().unwrap_or(false),
        yuv444_10bit: encoder["yuv444_10bit"].as_bool().unwrap_or(false),
        hdr10: encoder["hdr10"].as_bool().unwrap_or(false),
        sdr10: encoder["sdr10"].as_bool().unwrap_or(false),
    })
}

#[cfg(test)]
mod tests;
