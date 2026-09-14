//! Media and direction-specific control/RTSP encryption for negotiated GameStream profiles.
//! Keys must be fresh for each session; never recreate a sealer with a used key.
mod audio;
mod control;
mod envelope;
mod rtsp;
mod rtsp_decoder;
mod video;

pub use audio::{AudioIvPolicy, AudioSealer, MAX_AUDIO_PLAINTEXT_BYTES};
pub use control::{
    CONTROL_ENVELOPE_BYTES, CONTROL_REPLAY_WINDOW, ControlMessage, ControlOpener, ControlOrdering,
    ControlProfile, ControlSealer, MAX_CONTROL_PAYLOAD_BYTES,
};
pub use envelope::Direction;
pub use rtsp::{
    MAX_RTSP_PLAINTEXT_BYTES, RTSP_ENVELOPE_BYTES, RtspMessage, RtspOpener, RtspSealer,
    encrypted_rtsp_frame_length,
};
pub use rtsp_decoder::RtspEnvelopeDecoder;
pub use video::{MAX_VIDEO_PLAINTEXT_BYTES, VIDEO_PREFIX_BYTES, VideoProfile, VideoSealer};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CryptoError {
    InvalidPacketSize,
    NonceExhausted,
    AudioIvExhausted,
    AudioSequenceMismatch,
    EncryptionFailed,
    AuthenticationFailed,
    InvalidEnvelope,
    UnsupportedProfile,
    ReplayRejected,
    IncompleteEnvelope,
    DecoderPoisoned,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "media encryption error: {self:?}")
    }
}
impl std::error::Error for CryptoError {}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod envelope_tests;
