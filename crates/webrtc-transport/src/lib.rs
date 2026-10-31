//! A socket-independent WebRTC peer for a browser receiver.
//!
//! The embedding owner authenticates signaling, selects and binds the local
//! address, drives the clock and datagrams, and tears down its media sources.
//! This crate does not authorize peers or own sockets or capture devices.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use amaterasu_media::{MAX_ENCODED_AUDIO_BYTES, MAX_PACKET_BYTES};
use str0m::change::SdpOffer;
use str0m::format::{Codec, CodecSpec};
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};

mod sessions;
pub use sessions::{
    BrowserSessionSet, OpenedSession, OwnerContext, SessionError, SessionEvent, SessionState,
};

const MAX_SDP_BYTES: usize = 256 * 1024;
const MAX_DATAGRAM_BYTES: usize = 65_535;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    InvalidAddress,
    InvalidOffer,
    InvalidDatagram,
    UnrelatedDatagram,
    InvalidMedia,
    UnsupportedMedia,
    OutputNotDrained,
    AlreadyNegotiated,
    NotConnected,
    EngineFailure,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for TransportError {}

type Result<T> = std::result::Result<T, TransportError>;

/// One WebRTC connection. After every successful mutation, poll until a
/// `Timeout` is returned before invoking another mutating operation.
pub struct BrowserTransport {
    rtc: Rtc,
    local_address: SocketAddr,
    must_drain: bool,
    negotiated: bool,
    failed: bool,
    video: Option<Mid>,
    audio: Option<Mid>,
    last_video_time: Option<u64>,
    last_audio_time: Option<u64>,
}

impl BrowserTransport {
    /// `local_address` is the actual bound host candidate, never a wildcard.
    /// No network operation is performed here.
    pub fn new(local_address: SocketAddr, now: Instant) -> Result<Self> {
        if local_address.port() == 0
            || local_address.ip().is_unspecified()
            || local_address.ip().is_multicast()
            || matches!(local_address.ip(), std::net::IpAddr::V4(address) if address.is_broadcast())
        {
            return Err(TransportError::InvalidAddress);
        }
        let provider = Arc::new(str0m::crypto::from_feature_flags());
        let mut configuration = Rtc::builder()
            .set_crypto_provider(provider)
            .clear_codecs()
            .enable_opus(true, false);
        // Complete access units may exceed one RTP packet. Only advertise the
        // non-interleaved H.264 mode that permits fragmentation.
        for (payload, retransmit, profile) in [
            (127, 121, 0x42001f),
            (108, 109, 0x42e01f),
            (123, 119, 0x4d001f),
            (114, 115, 0x64001f),
        ] {
            configuration.codec_config().add_h264(
                payload.into(),
                Some(retransmit.into()),
                true,
                profile,
            );
        }
        let mut rtc = configuration.build(now);
        if !matches!(rtc.poll_output(), Ok(Output::Timeout(_))) {
            return Err(TransportError::EngineFailure);
        }
        let candidate =
            Candidate::host(local_address, "udp").map_err(|_| TransportError::InvalidAddress)?;
        rtc.add_local_candidate(candidate)
            .ok_or(TransportError::InvalidAddress)?;
        if !matches!(rtc.poll_output(), Ok(Output::Timeout(_))) {
            return Err(TransportError::EngineFailure);
        }
        Ok(Self {
            rtc,
            local_address,
            must_drain: false,
            negotiated: false,
            failed: false,
            video: None,
            audio: None,
            last_video_time: None,
            last_audio_time: None,
        })
    }

    /// Accept one complete browser offer and return the SDP answer. Signaling
    /// authentication, origin policy and offer ownership are the caller's job.
    pub fn accept_offer(&mut self, sdp: &str) -> Result<String> {
        self.ensure_ready()?;
        if self.negotiated {
            return Err(TransportError::AlreadyNegotiated);
        }
        if sdp.is_empty() || sdp.len() > MAX_SDP_BYTES {
            return Err(TransportError::InvalidOffer);
        }
        let offer = SdpOffer::from_sdp_string(sdp).map_err(|_| TransportError::InvalidOffer)?;
        self.must_drain = true;
        let answer = self.rtc.sdp_api().accept_offer(offer).map_err(|_| {
            self.failed = true;
            TransportError::InvalidOffer
        })?;
        self.negotiated = true;
        Ok(answer.to_sdp_string())
    }

    /// Drain after every mutation; only a timeout permits the next mutation.
    /// Network outputs and events must be handled by the embedding owner.
    pub fn poll_output(&mut self) -> Result<Output> {
        if self.failed {
            return Err(TransportError::EngineFailure);
        }
        let output = self.rtc.poll_output().map_err(|_| {
            self.failed = true;
            TransportError::EngineFailure
        })?;
        match &output {
            Output::Timeout(_) => self.must_drain = false,
            Output::Event(Event::MediaAdded(media))
                if matches!(media.direction, Direction::SendOnly | Direction::SendRecv) =>
            {
                let slot = match media.kind {
                    MediaKind::Video => &mut self.video,
                    MediaKind::Audio => &mut self.audio,
                };
                if slot.is_some() && *slot != Some(media.mid) {
                    self.failed = true;
                    return Err(TransportError::UnsupportedMedia);
                }
                *slot = Some(media.mid);
            }
            _ => {}
        }
        Ok(output)
    }

    pub fn receive_datagram(
        &mut self,
        now: Instant,
        source: SocketAddr,
        data: &[u8],
    ) -> Result<()> {
        self.ensure_ready()?;
        if data.is_empty() || data.len() > MAX_DATAGRAM_BYTES {
            return Err(TransportError::InvalidDatagram);
        }
        let contents = data
            .try_into()
            .map_err(|_| TransportError::InvalidDatagram)?;
        let input = Input::Receive(
            now,
            Receive {
                proto: Protocol::Udp,
                source,
                destination: self.local_address,
                contents,
            },
        );
        if !self.rtc.accepts(&input) {
            return Err(TransportError::UnrelatedDatagram);
        }
        self.must_drain = true;
        self.rtc.handle_input(input).map_err(|_| {
            self.failed = true;
            TransportError::EngineFailure
        })
    }

    pub fn handle_timeout(&mut self, now: Instant) -> Result<()> {
        self.ensure_ready()?;
        self.must_drain = true;
        self.rtc.handle_input(Input::Timeout(now)).map_err(|_| {
            self.failed = true;
            TransportError::EngineFailure
        })
    }

    pub fn is_connected(&self) -> bool {
        self.rtc.is_connected()
    }

    /// The payload selected by `send_h264`. The capture owner must verify its
    /// encoder configuration against these negotiated parameters before use.
    pub fn selected_h264_format(&mut self) -> Result<CodecSpec> {
        self.ensure_ready()?;
        let mid = self.video.ok_or(TransportError::UnsupportedMedia)?;
        let writer = self
            .rtc
            .writer(mid)
            .ok_or(TransportError::UnsupportedMedia)?;
        writer
            .payload_params()
            .find(|params| {
                params.spec().codec == Codec::H264
                    && params.spec().format.packetization_mode == Some(1)
            })
            .map(|params| params.spec())
            .ok_or(TransportError::UnsupportedMedia)
    }

    /// Send one complete H.264 Annex B access unit, after the owner has checked
    /// that its encoder profile is compatible with the negotiated payload.
    pub fn send_h264(
        &mut self,
        produced_at: Instant,
        timestamp_us: u64,
        bytes: &[u8],
    ) -> Result<()> {
        self.ensure_ready()?;
        if bytes.is_empty()
            || bytes.len() > MAX_PACKET_BYTES
            || !(bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1]))
        {
            return Err(TransportError::InvalidMedia);
        }
        let timestamp = media_ticks(timestamp_us, 90_000)?;
        if self
            .last_video_time
            .is_some_and(|previous| timestamp <= previous)
        {
            return Err(TransportError::InvalidMedia);
        }
        let mid = self.video.ok_or(TransportError::UnsupportedMedia)?;
        if !self.rtc.is_connected() {
            return Err(TransportError::NotConnected);
        }
        let writer = self
            .rtc
            .writer(mid)
            .ok_or(TransportError::UnsupportedMedia)?;
        let pt = writer
            .payload_params()
            .find(|params| {
                params.spec().codec == Codec::H264
                    && params.spec().format.packetization_mode == Some(1)
            })
            .ok_or(TransportError::UnsupportedMedia)?
            .pt();
        self.must_drain = true;
        writer
            .write(
                pt,
                produced_at,
                MediaTime::from_90khz(timestamp),
                bytes.to_vec(),
            )
            .map_err(|_| {
                self.failed = true;
                TransportError::EngineFailure
            })?;
        self.last_video_time = Some(timestamp);
        Ok(())
    }

    /// Send one complete Opus packet using the source's 48 kHz timestamp.
    pub fn send_opus(
        &mut self,
        produced_at: Instant,
        timestamp_us: u64,
        bytes: &[u8],
    ) -> Result<()> {
        self.ensure_ready()?;
        if bytes.is_empty() || bytes.len() > MAX_ENCODED_AUDIO_BYTES {
            return Err(TransportError::InvalidMedia);
        }
        let timestamp = media_ticks(timestamp_us, 48_000)?;
        if self
            .last_audio_time
            .is_some_and(|previous| timestamp <= previous)
        {
            return Err(TransportError::InvalidMedia);
        }
        let mid = self.audio.ok_or(TransportError::UnsupportedMedia)?;
        if !self.rtc.is_connected() {
            return Err(TransportError::NotConnected);
        }
        let writer = self
            .rtc
            .writer(mid)
            .ok_or(TransportError::UnsupportedMedia)?;
        let pt = writer
            .payload_params()
            .find(|params| params.spec().codec == Codec::Opus)
            .ok_or(TransportError::UnsupportedMedia)?
            .pt();
        self.must_drain = true;
        writer
            .write(
                pt,
                produced_at,
                MediaTime::new(timestamp, Frequency::FORTY_EIGHT_KHZ),
                bytes.to_vec(),
            )
            .map_err(|_| {
                self.failed = true;
                TransportError::EngineFailure
            })?;
        self.last_audio_time = Some(timestamp);
        Ok(())
    }

    fn ensure_ready(&self) -> Result<()> {
        if self.failed {
            Err(TransportError::EngineFailure)
        } else if self.must_drain {
            Err(TransportError::OutputNotDrained)
        } else {
            Ok(())
        }
    }
}

fn media_ticks(timestamp_us: u64, frequency: u64) -> Result<u64> {
    u64::try_from(u128::from(timestamp_us) * u128::from(frequency) / 1_000_000)
        .map_err(|_| TransportError::InvalidMedia)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unroutable_candidates_and_invalid_offers() {
        let now = Instant::now();
        for address in [
            "0.0.0.0:1234",
            "127.0.0.1:0",
            "224.0.0.1:10",
            "255.255.255.255:10",
        ] {
            assert!(matches!(
                BrowserTransport::new(address.parse().unwrap(), now),
                Err(TransportError::InvalidAddress)
            ));
        }
        let mut peer = BrowserTransport::new("127.0.0.1:1234".parse().unwrap(), now).unwrap();
        assert_eq!(peer.accept_offer(""), Err(TransportError::InvalidOffer));
        assert_eq!(
            peer.accept_offer(&"x".repeat(MAX_SDP_BYTES + 1)),
            Err(TransportError::InvalidOffer)
        );
        assert_eq!(
            peer.accept_offer("not an SDP offer"),
            Err(TransportError::InvalidOffer)
        );
        assert_eq!(
            peer.receive_datagram(now, "127.0.0.1:1235".parse().unwrap(), &[]),
            Err(TransportError::InvalidDatagram)
        );
    }

    #[test]
    fn media_timestamps_are_checked_without_overflow() {
        assert_eq!(media_ticks(1_000_000, 90_000), Ok(90_000));
        assert_eq!(media_ticks(1_000_000, 48_000), Ok(48_000));
        assert_eq!(
            media_ticks(u64::MAX, u64::MAX),
            Err(TransportError::InvalidMedia)
        );
    }

    #[test]
    fn negotiation_requires_draining_before_the_next_mutation() {
        let now = Instant::now();
        let mut offerer = Rtc::builder()
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
            .build(now);
        let mut changes = offerer.sdp_api();
        changes.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
        let (offer, _) = changes.apply().unwrap();
        let mut peer = BrowserTransport::new("127.0.0.1:1234".parse().unwrap(), now).unwrap();
        assert!(
            !peer
                .accept_offer(&offer.to_sdp_string())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            peer.handle_timeout(now),
            Err(TransportError::OutputNotDrained)
        );
        while !matches!(peer.poll_output().unwrap(), Output::Timeout(_)) {}
        assert_eq!(
            peer.accept_offer(&offer.to_sdp_string()),
            Err(TransportError::AlreadyNegotiated)
        );
        assert_eq!(
            peer.send_h264(now, 0, &[1, 2, 3]),
            Err(TransportError::InvalidMedia)
        );
        assert_eq!(
            peer.send_opus(now, 0, &[]),
            Err(TransportError::InvalidMedia)
        );
    }

    #[test]
    fn single_nal_only_offers_do_not_enable_full_frame_sending() {
        let now = Instant::now();
        let mut config = Rtc::builder()
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
            .clear_codecs();
        config
            .codec_config()
            .add_h264(125.into(), None, false, 0x42001f);
        let mut offerer = config.build(now);
        let mut changes = offerer.sdp_api();
        changes.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
        let (offer, _) = changes.apply().unwrap();
        let mut peer = BrowserTransport::new("127.0.0.1:1234".parse().unwrap(), now).unwrap();
        match peer.accept_offer(&offer.to_sdp_string()) {
            Ok(_) => {
                while !matches!(peer.poll_output().unwrap(), Output::Timeout(_)) {}
                assert_eq!(
                    peer.selected_h264_format(),
                    Err(TransportError::UnsupportedMedia)
                );
            }
            Err(TransportError::InvalidOffer) => {}
            Err(error) => panic!("unexpected negotiation failure: {error}"),
        }
    }
}
