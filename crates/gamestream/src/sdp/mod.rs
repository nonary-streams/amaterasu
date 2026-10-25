//! A constrained GameStream ANNOUNCE profile, not a generic SDP implementation.
use amaterasu_contracts::encoder::VideoStreamFormat;

mod attributes;
mod negotiate;
mod syntax;

pub use negotiate::parse_announce;

pub const MAX_BYTES: usize = 16 * 1024;
pub const MAX_LINES: usize = 192;
pub const MAX_LINE_BYTES: usize = 1024;
pub const MAX_ATTRIBUTES: usize = 128;
/// Moonlight client feature flag for PCM controller haptics.
pub const CLIENT_FEATURE_HAPTICS_PCM: u32 = 0x04;
pub const KNOWN_CLIENT_FEATURE_FLAGS: u32 = 0x07;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    LimitExceeded,
    InvalidSyntax,
    DuplicateField,
    Missing(&'static str),
    Invalid(&'static str),
    Unsupported(&'static str),
}
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "SDP negotiation error: {self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorMode {
    Bt601Limited,
    Bt601Full,
    Bt709Limited,
    Bt709Full,
}

pub use amaterasu_contracts::encoder::VideoCodec;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlProtocol {
    Enet,
    EncryptedEnet,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Video {
    /// Codec, bit depth, and dynamic range selected by the client ANNOUNCE.
    pub format: VideoStreamFormat,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub refresh_rate_x100: u32,
    /// Encoder bitrate, not an adjusted total-network budget.
    pub bitrate_kbps: u32,
    /// Client-requested slice count, bounded by the native signed C-int representation.
    pub slices_per_frame: i32,
    /// Original user-selected network budget, when sent by Moonlight.
    pub configured_bitrate_kbps: Option<u32>,
    pub color: ColorMode,
    /// Zero permits the host to choose; one requires a single reference frame.
    pub max_reference_frames: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Audio {
    pub channels: u32,
    pub channel_mask: u32,
    pub packet_duration_ms: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Encryption {
    pub control_v2: bool,
    pub video: bool,
    pub audio: bool,
    pub remote_input: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fec {
    pub minimum_packets: u32,
    /// Absent means the host must choose its parity policy, not zero parity.
    pub repair_percent: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Announce {
    pub video: Video,
    pub audio: Audio,
    /// NV header + video payload bytes; excludes the 16-byte RTP header.
    pub packet_size: u32,
    /// SDP metadata, never authorization to send to a destination or port.
    pub advertised_video_port: u16,
    pub control: ControlProtocol,
    pub encryption: Encryption,
    pub fec: Fec,
    pub client_feature_flags: u32,
    pub ignored_attributes: usize,
}

/// A downstream implementation must explicitly acknowledge transport features.
/// Default denies audio, control, encryption and parity generation.
#[derive(Default)]
pub struct TransportSupport {
    /// HEVC Main SDR encoder, packetizer and session configuration are all ready.
    pub hevc: bool,
    /// AV1 Main 8-bit SDR encoder, packetizer and session configuration are all ready.
    pub av1: bool,
    pub stereo_audio: bool,
    pub enet_control: bool,
    pub encrypted_control: bool,
    pub control_v2: bool,
    pub encrypted_remote_input: bool,
    pub encrypted_video: bool,
    pub encrypted_audio: bool,
    pub fec: bool,
}

impl Announce {
    /// Parsing is not acceptance. Call this against actual backend capabilities
    /// before committing ANNOUNCE; resource allocation and session checks follow.
    pub fn check_support(&self, support: &TransportSupport) -> Result<(), Error> {
        let requirements = [
            (
                self.video.format.codec == VideoCodec::Hevc,
                support.hevc,
                "HEVC video",
            ),
            (
                self.video.format.codec == VideoCodec::Av1,
                support.av1,
                "AV1 video",
            ),
            (true, support.stereo_audio, "stereo audio"),
            (true, support.enet_control, "ENet control"),
            (
                self.control == ControlProtocol::EncryptedEnet,
                support.encrypted_control,
                "encrypted control",
            ),
            (
                self.encryption.control_v2,
                support.control_v2,
                "control encryption v2",
            ),
            (
                self.encryption.remote_input && self.control == ControlProtocol::Enet,
                support.encrypted_remote_input,
                "remote input encryption",
            ),
            (
                self.encryption.video,
                support.encrypted_video,
                "video encryption",
            ),
            (
                self.encryption.audio,
                support.encrypted_audio,
                "audio encryption",
            ),
            (
                self.fec.minimum_packets > 0 || self.fec.repair_percent != Some(0),
                support.fec,
                "FEC parity",
            ),
        ];
        for (required, available, name) in requirements {
            if required && !available {
                return Err(Error::Unsupported(name));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
