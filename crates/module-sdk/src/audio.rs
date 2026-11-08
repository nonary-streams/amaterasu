//! Typed native audio interfaces shared by audio providers and consumers.
//!
//! These identifiers describe the interface family independently of a service
//! alias. Signed composition still controls provider selection and authorization.
use crate::{ModuleResult, NativeBinding, RuntimeClient, abi::NativeServiceFactory};
use amaterasu_media::{
    AUDIO_EXTENSIBLE_END, AudioCaptureApi, AudioEncoderApi, AudioFormatId, AudioUnit,
    AudioUnitSink, INTERFACE_MAJOR, MEDIA_BUSY, MEDIA_CLOSED, MEDIA_UNSUPPORTED,
};
use std::marker::PhantomData;
use std::{
    collections::VecDeque,
    io::{self, Write},
    net::SocketAddr,
};

pub const CAPTURE_CONTRACT: &str = "amaterasu.audio-capture.native/1";
pub const ENCODER_CONTRACT: &str = "amaterasu.audio-encoder.native/1";
pub const ENCODED_SOURCE_CONTRACT: &str = "amaterasu.encoded-audio-source.native/1";
pub const PROCESSOR_CONTRACT: &str = "amaterasu.audio-processor.native/1";
/// Timestamps are microseconds from the start of the selected stream.
pub const STREAM_CLOCK: AudioFormatId = AudioFormatId::standard(1);
/// Each output unit is one complete codec access unit.
pub const ACCESS_UNIT_FRAMING: AudioFormatId = AudioFormatId::standard(1);
/// Ordered speaker identifiers describe one channel per entry.
pub const SPEAKER_LAYOUT: AudioFormatId = AudioFormatId::standard(1);

/// Provider-defined stream configuration or metadata, copied at session open.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioSchemaData {
    pub schema: AudioFormatId,
    pub bytes: Vec<u8>,
}

/// Layout schema and optional provider-defined layout data. For the standard
/// speaker layout, `positions` is the exact order in the audio stream.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioLayout {
    pub schema: AudioFormatId,
    pub positions: Vec<AudioFormatId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<AudioSchemaData>,
}

impl AudioLayout {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.schema.is_present()
            || self.positions.len() > 256
            || (self.schema == SPEAKER_LAYOUT && self.positions.is_empty())
            || self.positions.iter().any(|position| !position.is_present())
            || self
                .data
                .as_ref()
                .is_some_and(|data| !data.schema.is_present() || data.bytes.len() > 65_536)
        {
            return Err("invalid_audio_layout");
        }
        Ok(())
    }
}

/// Codec-independent session data. The clock identity defines the epoch and
/// units of timestamps in this stream; packet timestamps and durations are in
/// microseconds on that clock. Generation changes whenever decoder setup does.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioStreamDescription {
    pub framing: AudioFormatId,
    pub clock: AudioFormatId,
    pub layout: AudioLayout,
    pub generation: u64,
    pub encoder_delay_us: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec_initialization: Option<AudioSchemaData>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata: Vec<AudioSchemaData>,
}

impl AudioStreamDescription {
    pub fn validate(&self) -> Result<(), &'static str> {
        let entries = self.codec_initialization.iter().chain(self.metadata.iter());
        if !self.framing.is_present()
            || !self.clock.is_present()
            || self.generation == 0
            || self.layout.validate().is_err()
            || self.metadata.len() > 32
            || entries
                .clone()
                .any(|entry| !entry.schema.is_present() || entry.bytes.len() > 65_536)
            || entries.map(|entry| entry.bytes.len()).sum::<usize>() > 131_072
        {
            return Err("invalid_audio_stream_description");
        }
        Ok(())
    }
}

/// The source sample representation and ordered speaker positions negotiated
/// for a stream. Speaker, sample and packing identifiers may be provider-defined.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PcmFormat {
    pub sample_rate: u32,
    pub sample_format: AudioFormatId,
    pub sample_bytes: u32,
    pub valid_bits: u32,
    pub packing: AudioFormatId,
    pub speakers: Vec<AudioFormatId>,
}

impl PcmFormat {
    /// Check representation bounds without imposing a list of known formats.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.sample_rate == 0
            || self.sample_bytes == 0
            || self.valid_bits == 0
            || u64::from(self.valid_bits) > u64::from(self.sample_bytes) * 8
            || !self.sample_format.is_present()
            || !self.packing.is_present()
            || amaterasu_media::standard_pcm_sample_bytes(self.sample_format)
                .is_some_and(|bytes| bytes != self.sample_bytes)
            || self.speakers.is_empty()
            || self.speakers.len() > 256
            || self.speakers.iter().any(|speaker| !speaker.is_present())
        {
            return Err("invalid_pcm_format");
        }
        Ok(())
    }
}

/// Source type selected by the protocol and confirmed by the source provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioSource {
    #[default]
    Pcm,
    Encoded,
}

impl AudioSource {
    pub fn is_pcm(&self) -> bool {
        matches!(self, Self::Pcm)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioProcessingFormat {
    pub output: PcmFormat,
    pub latency_us: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options_schema: Option<AudioFormatId>,
    pub options: serde_json::Value,
}

/// The complete proposed custom audio configuration, shared by session,
/// protocol, source, processor, encoder, and transport providers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioProfile {
    pub codec: AudioFormatId,
    pub sample_rate: u32,
    pub stream: AudioStreamDescription,
    #[serde(default, skip_serializing_if = "AudioSource::is_pcm")]
    pub source: AudioSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pcm: Option<PcmFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing: Option<AudioProcessingFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options_schema: Option<AudioFormatId>,
    pub options: serde_json::Value,
}

/// Complete custom session profile forwarded unchanged to every selected role.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioSessionProfile {
    pub packet_ms: u32,
    pub bitrate: u32,
    pub layout: String,
    pub channels: u32,
    pub channel_mask: u32,
    pub custom: AudioProfile,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_offers: Vec<AudioFormatOffer>,
}

/// Shared control messages for audio roles. `parameters` contains role-owned
/// open settings such as an endpoint; signed composition still selects services.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AudioControlRequest {
    Enumerate,
    Capabilities,
    Probe {
        audio_profile: AudioSessionProfile,
    },
    Open {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio_profile: Option<AudioSessionProfile>,
        operation_id: String,
        #[serde(flatten)]
        parameters: serde_json::Map<String, serde_json::Value>,
    },
    Effective {
        stream: u64,
    },
    Status {
        stream: u64,
    },
    Reconfigure {
        stream: u64,
        audio_profile: AudioSessionProfile,
    },
    Close {
        stream: u64,
    },
}

impl AudioControlRequest {
    /// Serialize a control request for `Command::Call` or provider dispatch.
    pub fn payload(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

/// Common successful open or reconfiguration response. Role-specific fields,
/// such as a transport's local ports, are carried in `details`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudioOpenResult {
    pub ok: bool,
    pub stream: u64,
    pub audio_profile: AudioSessionProfile,
    #[serde(flatten)]
    pub details: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudioCloseResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<u64>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudioStatusEntry {
    pub stream: u64,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_profile: Option<AudioSessionProfile>,
    #[serde(flatten)]
    pub details: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudioStatusResult {
    pub ok: bool,
    pub streams: Vec<AudioStatusEntry>,
    #[serde(flatten)]
    pub details: serde_json::Map<String, serde_json::Value>,
}

/// Advertised combinations and resource limits for one provider role. Empty
/// lists mean the provider does not offer that representation. Unknown latency
/// and buffering limits are `None`, not invented estimates.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioProviderCapabilities {
    pub pcm_inputs: Vec<PcmFormat>,
    pub pcm_outputs: Vec<PcmFormat>,
    pub encoded_formats: Vec<AudioFormatOffer>,
    pub encoded_source: bool,
    pub max_buffered_bytes: Option<u64>,
    pub min_latency_us: Option<u64>,
    pub max_latency_us: Option<u64>,
    pub properties: serde_json::Value,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudioCapabilitiesResult {
    pub ok: bool,
    pub capabilities: AudioProviderCapabilities,
}

/// Standard WAVE-order speaker positions for the built-in layouts. Custom
/// layouts can supply any namespaced positions without using this helper.
pub fn standard_speaker_positions(channels: u32) -> Option<Vec<AudioFormatId>> {
    match channels {
        1 => Some(vec![SPEAKER_FRONT_CENTER]),
        2 => Some(vec![SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT]),
        6 => Some(vec![
            SPEAKER_FRONT_LEFT,
            SPEAKER_FRONT_RIGHT,
            SPEAKER_FRONT_CENTER,
            SPEAKER_LOW_FREQUENCY,
            SPEAKER_BACK_LEFT,
            SPEAKER_BACK_RIGHT,
        ]),
        8 => Some(vec![
            SPEAKER_FRONT_LEFT,
            SPEAKER_FRONT_RIGHT,
            SPEAKER_FRONT_CENTER,
            SPEAKER_LOW_FREQUENCY,
            SPEAKER_BACK_LEFT,
            SPEAKER_BACK_RIGHT,
            SPEAKER_SIDE_LEFT,
            SPEAKER_SIDE_RIGHT,
        ]),
        _ => None,
    }
}

pub fn standard_pcm_format(channels: u32, sample_format: AudioFormatId) -> Option<PcmFormat> {
    let sample_bytes = amaterasu_media::standard_pcm_sample_bytes(sample_format)?;
    Some(PcmFormat {
        sample_rate: 48_000,
        sample_format,
        sample_bytes,
        valid_bits: sample_bytes * 8,
        packing: amaterasu_media::AUDIO_PACKING_INTERLEAVED,
        speakers: standard_speaker_positions(channels)?,
    })
}

/// Build a standard access-unit offer with an unknown latency range.
pub fn standard_encoded_offer(
    codec: AudioFormatId,
    channels: u32,
    max_unit_bytes: u32,
) -> Option<AudioFormatOffer> {
    Some(AudioFormatOffer {
        codec,
        sample_rate: 48_000,
        framing: ACCESS_UNIT_FRAMING,
        clock: STREAM_CLOCK,
        layout: AudioLayout {
            schema: SPEAKER_LAYOUT,
            positions: standard_speaker_positions(channels)?,
            data: None,
        },
        metadata_schemas: vec![],
        max_unit_bytes,
        max_metadata_bytes: 0,
        min_latency_us: 0,
        max_latency_us: u64::MAX,
    })
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudioProbeResult {
    pub ok: bool,
    pub supported: bool,
    pub audio_profile: AudioSessionProfile,
}

/// One concrete combination a protocol or provider is prepared to carry.
/// Providers may return several entries and probe a profile for extra options.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioFormatOffer {
    pub codec: AudioFormatId,
    pub sample_rate: u32,
    pub framing: AudioFormatId,
    pub clock: AudioFormatId,
    pub layout: AudioLayout,
    pub metadata_schemas: Vec<AudioFormatId>,
    pub max_unit_bytes: u32,
    pub max_metadata_bytes: u32,
    pub min_latency_us: u64,
    pub max_latency_us: u64,
}

impl AudioFormatOffer {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.codec.is_present()
            || self.sample_rate == 0
            || !self.framing.is_present()
            || !self.clock.is_present()
            || self.layout.validate().is_err()
            || self.metadata_schemas.len() > 64
            || self
                .metadata_schemas
                .iter()
                .any(|schema| !schema.is_present())
            || self.max_unit_bytes == 0
            || self.max_unit_bytes as usize > amaterasu_media::AUDIO_EXTENSIBLE_MAX_PAYLOAD
            || self.max_metadata_bytes as usize > amaterasu_media::AUDIO_EXTENSIBLE_MAX_METADATA
            || self.min_latency_us > self.max_latency_us
        {
            return Err("invalid_audio_offer");
        }
        Ok(())
    }

    /// Checks the declared wire combination. Provider-specific options still
    /// require the provider's probe before opening a session.
    pub fn supports(&self, profile: &AudioProfile) -> bool {
        self.validate().is_ok()
            && self.codec == profile.codec
            && self.sample_rate == profile.sample_rate
            && self.framing == profile.stream.framing
            && self.clock == profile.stream.clock
            && self.layout == profile.stream.layout
            && profile
                .stream
                .metadata
                .iter()
                .all(|item| self.metadata_schemas.contains(&item.schema))
    }
}

/// Find the first preferred wire format supported by all selected participants.
/// Payload, metadata, latency and schema limits are intersected conservatively.
pub fn negotiate_audio_offers(
    preferred: &[AudioFormatOffer],
    participants: &[&[AudioFormatOffer]],
) -> Result<Option<AudioFormatOffer>, &'static str> {
    if preferred.len() > 64
        || participants.len() > 16
        || participants.iter().any(|offers| offers.len() > 64)
    {
        return Err("audio_offer_limit");
    }
    fn intersect(
        current: AudioFormatOffer,
        participants: &[&[AudioFormatOffer]],
        budget: &mut usize,
    ) -> Result<Option<AudioFormatOffer>, &'static str> {
        let Some((offers, rest)) = participants.split_first() else {
            return Ok(Some(current));
        };
        for other in *offers {
            if *budget == 0 {
                return Err("audio_offer_search_limit");
            }
            *budget -= 1;
            if other.validate().is_err()
                || other.codec != current.codec
                || other.sample_rate != current.sample_rate
                || other.framing != current.framing
                || other.clock != current.clock
                || other.layout != current.layout
                || other.min_latency_us > current.max_latency_us
                || other.max_latency_us < current.min_latency_us
            {
                continue;
            }
            let mut combined = current.clone();
            combined.max_unit_bytes = combined.max_unit_bytes.min(other.max_unit_bytes);
            combined.max_metadata_bytes = combined.max_metadata_bytes.min(other.max_metadata_bytes);
            combined.min_latency_us = combined.min_latency_us.max(other.min_latency_us);
            combined.max_latency_us = combined.max_latency_us.min(other.max_latency_us);
            combined
                .metadata_schemas
                .retain(|schema| other.metadata_schemas.contains(schema));
            if let Some(result) = intersect(combined, rest, budget)? {
                return Ok(Some(result));
            }
        }
        Ok(None)
    }
    let mut budget = 4096;
    for first in preferred.iter().filter(|offer| offer.validate().is_ok()) {
        if let Some(result) = intersect(first.clone(), participants, &mut budget)? {
            return Ok(Some(result));
        }
    }
    Ok(None)
}

/// Standard speaker identifiers use the WAVE speaker-position bit value.
/// Their order in `PcmFormat::speakers` is the actual sample order.
pub const SPEAKER_FRONT_LEFT: AudioFormatId = AudioFormatId::standard(0x1);
pub const SPEAKER_FRONT_RIGHT: AudioFormatId = AudioFormatId::standard(0x2);
pub const SPEAKER_FRONT_CENTER: AudioFormatId = AudioFormatId::standard(0x4);
pub const SPEAKER_LOW_FREQUENCY: AudioFormatId = AudioFormatId::standard(0x8);
pub const SPEAKER_BACK_LEFT: AudioFormatId = AudioFormatId::standard(0x10);
pub const SPEAKER_BACK_RIGHT: AudioFormatId = AudioFormatId::standard(0x20);
pub const SPEAKER_SIDE_LEFT: AudioFormatId = AudioFormatId::standard(0x200);
pub const SPEAKER_SIDE_RIGHT: AudioFormatId = AudioFormatId::standard(0x400);
pub const SPEAKER_TOP_FRONT_LEFT: AudioFormatId = AudioFormatId::standard(0x1000);
pub const SPEAKER_TOP_FRONT_RIGHT: AudioFormatId = AudioFormatId::standard(0x4000);
pub const SPEAKER_TOP_BACK_LEFT: AudioFormatId = AudioFormatId::standard(0x8000);
pub const SPEAKER_TOP_BACK_RIGHT: AudioFormatId = AudioFormatId::standard(0x20000);

/// A datagram channel targeting a destination chosen by the session owner.
///
/// This helper uses the selected Network SDK provider and has no background
/// queue. A successful send means only that the provider accepted the datagram.
/// The stream host does not yet provision these channels to audio plugins.
pub struct ClientAudioChannel {
    socket: Option<crate::network::UdpSocket>,
    destination: Option<SocketAddr>,
}

impl ClientAudioChannel {
    /// Wrap a provider-owned socket and the destination selected by the session.
    /// The caller is responsible for obtaining the destination from trusted
    /// session state and for retaining the channel only for that session.
    pub fn from_session(socket: crate::network::UdpSocket, destination: SocketAddr) -> Self {
        Self {
            socket: Some(socket),
            destination: Some(destination),
        }
    }

    /// Wrap a socket already connected to the session's selected client.
    /// The caller retains responsibility for selecting and maintaining its peer.
    pub fn from_connected_session(socket: crate::network::UdpSocket) -> Self {
        Self {
            socket: Some(socket),
            destination: None,
        }
    }

    /// Send one complete datagram. WouldBlock means it can be retried later.
    pub fn send_datagram(&self, data: &[u8]) -> io::Result<()> {
        let socket = self.socket.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        let sent = match self.destination {
            Some(destination) => socket.send_to(data, destination)?,
            None => socket.send(data)?,
        };
        match sent {
            count if count == data.len() => Ok(()),
            _ => Err(io::ErrorKind::WriteZero.into()),
        }
    }

    /// Stop further sends and release this channel's socket guard.
    pub fn close(&mut self) {
        self.socket = None;
    }
}

/// A bounded datagram queue. Packet boundaries remain intact and a busy send
/// leaves the first packet queued. The caller controls pacing by choosing when
/// and how many packets to flush; there is no background worker.
pub struct QueuedAudioDatagrams {
    channel: ClientAudioChannel,
    queue: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    max_packets: usize,
    max_bytes: usize,
    closed: bool,
}

impl QueuedAudioDatagrams {
    pub fn new(
        channel: ClientAudioChannel,
        max_packets: usize,
        max_bytes: usize,
    ) -> io::Result<Self> {
        if max_packets == 0 || max_bytes == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            channel,
            queue: VecDeque::new(),
            queued_bytes: 0,
            max_packets,
            max_bytes,
            closed: false,
        })
    }

    /// Copy a complete datagram into local storage. WouldBlock admits nothing.
    pub fn enqueue(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.closed {
            return Err(io::ErrorKind::NotConnected.into());
        }
        if bytes.is_empty() || bytes.len() > self.max_bytes {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if self.queue.len() == self.max_packets || bytes.len() > self.max_bytes - self.queued_bytes
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.queue.push_back(bytes.to_vec());
        self.queued_bytes += bytes.len();
        Ok(())
    }

    /// Send up to `budget` packets; the count is local network admission only.
    pub fn flush(&mut self, budget: usize) -> io::Result<usize> {
        if self.closed {
            return Err(io::ErrorKind::NotConnected.into());
        }
        let mut sent = 0;
        while sent < budget {
            let Some(packet) = self.queue.front() else {
                break;
            };
            match self.channel.send_datagram(packet) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
            let packet = self.queue.pop_front().expect("front exists");
            self.queued_bytes -= packet.len();
            sent += 1;
        }
        Ok(sent)
    }

    pub fn queued_bytes(&self) -> usize {
        self.queued_bytes
    }

    /// Cancel pending datagrams and release the socket guard.
    pub fn close(&mut self) {
        self.closed = true;
        self.queue.clear();
        self.queued_bytes = 0;
        self.channel.close();
    }
}

/// Bounded byte-stream output for a connected Network SDK TCP stream.
/// Protocol framing is supplied by the transport provider before enqueueing.
pub struct ClientAudioStream {
    stream: Option<crate::network::TcpStream>,
    queue: VecDeque<Vec<u8>>,
    offset: usize,
    queued_bytes: usize,
    max_bytes: usize,
}

impl ClientAudioStream {
    pub fn from_session(stream: crate::network::TcpStream, max_bytes: usize) -> io::Result<Self> {
        if max_bytes == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream: Some(stream),
            queue: VecDeque::new(),
            offset: 0,
            queued_bytes: 0,
            max_bytes,
        })
    }

    /// Admit a complete provider-framed buffer or return WouldBlock without copying it.
    pub fn enqueue(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.stream.is_none() {
            return Err(io::ErrorKind::NotConnected.into());
        }
        if bytes.is_empty() || bytes.len() > self.max_bytes {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if bytes.len() > self.max_bytes - self.queued_bytes {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.queue.push_back(bytes.to_vec());
        self.queued_bytes += bytes.len();
        Ok(())
    }

    /// Send at most `budget` bytes, preserving the unsent suffix on partial writes.
    pub fn flush(&mut self, budget: usize) -> io::Result<usize> {
        let stream = self.stream.as_mut().ok_or(io::ErrorKind::NotConnected)?;
        let mut sent = 0;
        while sent < budget {
            let Some(bytes) = self.queue.front() else {
                break;
            };
            let remaining = &bytes[self.offset..];
            let amount = remaining.len().min(budget - sent);
            let written = match stream.write(&remaining[..amount]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(written) => written,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            };
            self.offset += written;
            self.queued_bytes -= written;
            sent += written;
            if self.offset == bytes.len() {
                self.queue.pop_front();
                self.offset = 0;
            }
        }
        Ok(sent)
    }

    pub fn queued_bytes(&self) -> usize {
        self.queued_bytes
    }

    /// Cancel pending output and release this stream's socket guard.
    pub fn close(&mut self) {
        self.stream = None;
        self.queue.clear();
        self.offset = 0;
        self.queued_bytes = 0;
    }
}

/// Result of local audio admission. Acceptance does not imply client delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOutputError {
    Busy,
    Closed,
    Unsupported,
    Invalid,
    Provider(crate::abi::Status),
}

impl AudioOutputError {
    /// Convert an SDK admission result back to a provider callback status.
    pub fn status(self) -> crate::abi::Status {
        match self {
            Self::Busy => MEDIA_BUSY,
            Self::Closed => MEDIA_CLOSED,
            Self::Unsupported => MEDIA_UNSUPPORTED,
            Self::Invalid => crate::abi::STATUS_INVALID_ARGUMENT,
            Self::Provider(status) => status,
        }
    }
}

/// A wrapper for an audio sink using one negotiated format.
/// The session negotiates the format and selects the client separately.
pub struct AudioOutput<'a> {
    sink: AudioUnitSink,
    format: AudioFormatId,
    generation: u64,
    finished: bool,
    _lifetime: PhantomData<&'a mut ()>,
}

impl<'a> AudioOutput<'a> {
    /// # Safety
    /// The sink callback and its context must remain valid for `'a`. Calls must
    /// obey the sink's threading rules and the callback must not retain borrows.
    pub unsafe fn from_sink(
        sink: AudioUnitSink,
        format: AudioFormatId,
        generation: u64,
    ) -> Result<Self, AudioOutputError> {
        if !format.is_present() || generation == 0 {
            return Err(AudioOutputError::Invalid);
        }
        Ok(Self {
            sink,
            format,
            generation,
            finished: false,
            _lifetime: PhantomData,
        })
    }

    /// Send a complete unit. BUSY leaves the unit unaccepted for retry.
    pub fn send_packet(
        &mut self,
        payload: &[u8],
        timestamp_us: u64,
        duration_us: u64,
        sequence: u64,
        metadata: Option<(AudioFormatId, &[u8])>,
    ) -> Result<(), AudioOutputError> {
        self.send_packet_with_flags(payload, timestamp_us, duration_us, sequence, metadata, 0)
    }

    /// Send a unit with negotiated flags. An accepted END flag finishes output,
    /// and may accompany final payload or metadata.
    pub fn send_packet_with_flags(
        &mut self,
        payload: &[u8],
        timestamp_us: u64,
        duration_us: u64,
        sequence: u64,
        metadata: Option<(AudioFormatId, &[u8])>,
        flags: u32,
    ) -> Result<(), AudioOutputError> {
        let (metadata_schema, metadata_bytes) = metadata.unwrap_or((
            AudioFormatId {
                namespace: 0,
                value: 0,
                version: 0,
            },
            &[],
        ));
        let unit = AudioUnit {
            generation: self.generation,
            format: self.format,
            metadata_schema,
            flags,
            timestamp_us,
            duration_us,
            sequence,
            payload: crate::abi::BorrowedBytes::new(payload),
            metadata: crate::abi::BorrowedBytes::new(metadata_bytes),
        };
        self.submit(&unit)?;
        if flags & AUDIO_EXTENSIBLE_END != 0 {
            self.finished = true;
        }
        Ok(())
    }

    /// Submit the terminal marker. A busy result may be retried.
    pub fn finish(&mut self, timestamp_us: u64, sequence: u64) -> Result<(), AudioOutputError> {
        let unit = AudioUnit {
            generation: self.generation,
            format: self.format,
            metadata_schema: AudioFormatId {
                namespace: 0,
                value: 0,
                version: 0,
            },
            flags: AUDIO_EXTENSIBLE_END,
            timestamp_us,
            duration_us: 0,
            sequence,
            payload: crate::abi::BorrowedBytes::new(&[]),
            metadata: crate::abi::BorrowedBytes::new(&[]),
        };
        self.submit(&unit)?;
        self.finished = true;
        Ok(())
    }

    fn submit(&mut self, unit: &AudioUnit) -> Result<(), AudioOutputError> {
        if self.finished {
            return Err(AudioOutputError::Closed);
        }
        unit.validate().map_err(|_| AudioOutputError::Invalid)?;
        // SAFETY: from_sink establishes callback lifetime; unit and byte slices
        // remain borrowed until the synchronous callback returns.
        let status = unsafe { self.sink.send(unit) };
        match status {
            crate::abi::STATUS_OK => Ok(()),
            MEDIA_BUSY => Err(AudioOutputError::Busy),
            MEDIA_CLOSED => Err(AudioOutputError::Closed),
            MEDIA_UNSUPPORTED => Err(AudioOutputError::Unsupported),
            crate::abi::STATUS_INVALID_ARGUMENT => Err(AudioOutputError::Invalid),
            other => Err(AudioOutputError::Provider(other)),
        }
    }
}

/// Register the PCM capture table for a selected service alias.
///
/// # Safety
/// The factory must implement `AudioCaptureApi` and satisfy the native SDK's
/// factory lifetime, callback, ownership, and concurrency requirements.
pub unsafe fn register_capture(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, CAPTURE_CONTRACT, factory) }
}

/// Register the encoded audio table for a selected service alias.
///
/// # Safety
/// The factory must implement `AudioEncoderApi` and satisfy the native SDK's
/// factory lifetime, callback, ownership, and concurrency requirements.
pub unsafe fn register_encoder(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, ENCODER_CONTRACT, factory) }
}

/// Register a source that delivers already encoded units.
///
/// # Safety
/// The factory must implement `EncodedAudioSourceApi` and honor native SDK
/// ownership, callback lifetime and concurrency requirements.
pub unsafe fn register_encoded_source(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, ENCODED_SOURCE_CONTRACT, factory) }
}

/// Bind an explicitly selected source of already encoded units.
///
/// # Safety
/// The runtime and provider outlive the binding and all calls; the caller
/// obeys the source's ownership and threading rules.
pub unsafe fn bind_encoded_source(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            ENCODED_SOURCE_CONTRACT,
            INTERFACE_MAJOR,
            0,
            size_of::<amaterasu_media::EncodedAudioSourceApi>() as u32,
        )
    }
}

/// Register a PCM processor implementation under a selected service.
///
/// # Safety
/// The factory must implement `AudioProcessorApi` and honor the native SDK's
/// ownership, callback lifetime and threading requirements.
pub unsafe fn register_processor(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, PROCESSOR_CONTRACT, factory) }
}

/// Bind the selected PCM processor.
///
/// # Safety
/// The runtime and provider must outlive the binding and all calls.
pub unsafe fn bind_processor(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            PROCESSOR_CONTRACT,
            INTERFACE_MAJOR,
            0,
            size_of::<amaterasu_media::AudioProcessorApi>() as u32,
        )
    }
}

/// Bind a provider promising the PCM capture contract.
///
/// # Safety
/// The runtime and provider must outlive the returned binding and all calls.
/// The caller must obey the audio interface's ownership and threading rules.
pub unsafe fn bind_capture(runtime: &RuntimeClient, service: &str) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            CAPTURE_CONTRACT,
            INTERFACE_MAJOR,
            0,
            size_of::<AudioCaptureApi>() as u32,
        )
    }
}

/// Bind a provider promising the encoded audio contract.
///
/// # Safety
/// The runtime and provider must outlive the returned binding and all calls.
/// The caller must obey the audio interface's ownership and threading rules.
pub unsafe fn bind_encoder(runtime: &RuntimeClient, service: &str) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            ENCODER_CONTRACT,
            INTERFACE_MAJOR,
            0,
            size_of::<AudioEncoderApi>() as u32,
        )
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;
    use std::ffi::c_void;

    #[test]
    fn offer_negotiation_backtracks_and_preserves_custom_identities() {
        let custom = AudioFormatId {
            namespace: 123,
            value: 456,
            version: 7,
        };
        let offer = AudioFormatOffer {
            codec: custom,
            sample_rate: 96_000,
            framing: custom,
            clock: STREAM_CLOCK,
            layout: AudioLayout {
                schema: custom,
                positions: vec![],
                data: None,
            },
            metadata_schemas: vec![custom],
            max_unit_bytes: 1000,
            max_metadata_bytes: 100,
            min_latency_us: 0,
            max_latency_us: 100,
        };
        let mut early = offer.clone();
        early.max_latency_us = 20;
        let mut later = offer.clone();
        later.min_latency_us = 60;
        later.max_unit_bytes = 500;
        let mut client = offer.clone();
        client.min_latency_us = 50;
        client.max_metadata_bytes = 50;
        let provider_offers = [early, later];
        let client_offers = [client];
        let result = negotiate_audio_offers(
            std::slice::from_ref(&offer),
            &[&provider_offers, &client_offers],
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.codec, custom);
        assert_eq!(result.min_latency_us, 60);
        assert_eq!(result.max_unit_bytes, 500);
        assert_eq!(result.max_metadata_bytes, 50);
        assert_eq!(result.metadata_schemas, [custom]);
        assert_eq!(
            negotiate_audio_offers(&vec![offer; 65], &[]),
            Err("audio_offer_limit")
        );
    }

    #[test]
    fn shared_control_messages_round_trip_arbitrary_audio_profiles() {
        let id = AudioFormatId {
            namespace: 81,
            value: 700,
            version: 1,
        };
        let profile = AudioSessionProfile {
            packet_ms: 20,
            bitrate: 123_000,
            layout: "objects".into(),
            channels: 12,
            channel_mask: 0,
            client_offers: vec![],
            custom: AudioProfile {
                codec: id,
                sample_rate: 48_000,
                source: AudioSource::Encoded,
                pcm: None,
                processing: None,
                options_schema: Some(id),
                options: serde_json::json!({"vendor":true}),
                stream: AudioStreamDescription {
                    framing: id,
                    clock: STREAM_CLOCK,
                    layout: AudioLayout {
                        schema: id,
                        positions: vec![],
                        data: None,
                    },
                    generation: 7,
                    encoder_delay_us: 2000,
                    codec_initialization: Some(AudioSchemaData {
                        schema: id,
                        bytes: vec![1, 2],
                    }),
                    metadata: vec![],
                },
            },
        };
        let request = AudioControlRequest::Reconfigure {
            stream: 9,
            audio_profile: profile.clone(),
        };
        let payload = request.payload().unwrap();
        assert_eq!(payload["action"], "reconfigure");
        let decoded: AudioControlRequest = serde_json::from_value(payload).unwrap();
        let AudioControlRequest::Reconfigure {
            stream,
            audio_profile,
        } = decoded
        else {
            panic!("wrong request")
        };
        assert_eq!(stream, 9);
        assert_eq!(audio_profile, profile);
    }

    #[test]
    fn output_retries_busy_and_finishes_only_after_acceptance() {
        unsafe extern "C" fn receive(
            context: *mut c_void,
            unit: *const AudioUnit,
        ) -> crate::abi::Status {
            // SAFETY: the test supplies a live exclusive counter and unit for the call.
            let calls = unsafe { &mut *context.cast::<u32>() };
            let unit = unsafe { &*unit };
            assert_eq!(unit.validate(), Ok(()));
            *calls += 1;
            if *calls == 1 {
                MEDIA_BUSY
            } else {
                crate::abi::STATUS_OK
            }
        }
        let mut calls = 0u32;
        let sink = AudioUnitSink {
            context: (&mut calls as *mut u32).cast(),
            write: receive,
        };
        let format = AudioFormatId {
            namespace: 7,
            value: 42,
            version: 1,
        };
        // SAFETY: the counter and callback remain live until output is dropped.
        let mut output = unsafe { AudioOutput::from_sink(sink, format, 1) }.unwrap();
        assert_eq!(
            output.send_packet(&[1], 0, 1000, 1, None),
            Err(AudioOutputError::Busy)
        );
        assert_eq!(output.send_packet(&[1], 0, 1000, 1, None), Ok(()));
        assert_eq!(output.finish(1000, 2), Ok(()));
        assert_eq!(output.finish(1000, 2), Err(AudioOutputError::Closed));
        assert_eq!(calls, 3);
    }

    #[test]
    fn output_forwards_discontinuity_and_accepts_payload_on_the_final_unit() {
        unsafe extern "C" fn receive(
            context: *mut c_void,
            unit: *const AudioUnit,
        ) -> crate::abi::Status {
            // SAFETY: the test keeps the flag storage and unit live for this call.
            let observed = unsafe { &mut *context.cast::<Vec<u32>>() };
            observed.push(unsafe { (*unit).flags });
            crate::abi::STATUS_OK
        }
        let mut observed = Vec::new();
        let sink = AudioUnitSink {
            context: (&mut observed as *mut Vec<u32>).cast(),
            write: receive,
        };
        let format = AudioFormatId {
            namespace: 7,
            value: 42,
            version: 1,
        };
        // SAFETY: callback and storage stay live until the output is no longer used.
        let mut output = unsafe { AudioOutput::from_sink(sink, format, 1) }.unwrap();
        assert_eq!(
            output.send_packet_with_flags(
                &[1],
                0,
                1000,
                1,
                None,
                amaterasu_media::AUDIO_EXTENSIBLE_DISCONTINUITY
            ),
            Ok(())
        );
        assert_eq!(
            output.send_packet_with_flags(&[1], 0, 1000, 2, None, AUDIO_EXTENSIBLE_END),
            Ok(())
        );
        assert_eq!(output.finish(1000, 2), Err(AudioOutputError::Closed));
        assert_eq!(
            observed,
            [
                amaterasu_media::AUDIO_EXTENSIBLE_DISCONTINUITY,
                AUDIO_EXTENSIBLE_END
            ]
        );
    }
}

#[cfg(all(test, feature = "network-test-support"))]
mod channel_tests {
    use super::*;
    use std::io::Read;
    use std::time::Duration;

    #[test]
    fn datagrams_use_network_provider_and_close_rejects_further_sends() {
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let network = crate::network::Network::test_provider();
        let socket = network.bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
        let mut channel = ClientAudioChannel::from_session(socket, receiver.local_addr().unwrap());
        channel.send_datagram(b"audio").unwrap();
        let mut received = [0; 16];
        let (length, _) = receiver.recv_from(&mut received).unwrap();
        assert_eq!(&received[..length], b"audio");
        channel.close();
        assert_eq!(
            channel.send_datagram(b"later").unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
    }

    #[test]
    fn queued_datagrams_are_bounded_ordered_and_cancellable() {
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let network = crate::network::Network::test_provider();
        let socket = network.bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
        let channel = ClientAudioChannel::from_session(socket, receiver.local_addr().unwrap());
        let mut queue = QueuedAudioDatagrams::new(channel, 2, 6).unwrap();
        queue.enqueue(b"one").unwrap();
        queue.enqueue(b"two").unwrap();
        assert_eq!(
            queue.enqueue(b"x").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(queue.flush(1).unwrap(), 1);
        assert_eq!(queue.queued_bytes(), 3);
        let mut bytes = [0; 8];
        let (length, _) = receiver.recv_from(&mut bytes).unwrap();
        assert_eq!(&bytes[..length], b"one");
        queue.close();
        assert_eq!(queue.queued_bytes(), 0);
        assert_eq!(
            queue.flush(1).unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
    }

    #[test]
    fn stream_flush_preserves_unsent_suffix_and_close_discards_pending_bytes() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let network = crate::network::Network::test_provider();
        let stream = network
            .connect_tcp(listener.local_addr().unwrap(), Duration::from_secs(2))
            .unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut channel = ClientAudioStream::from_session(stream, 8).unwrap();
        channel.enqueue(b"abcdef").unwrap();
        assert_eq!(
            channel.enqueue(b"xyz").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(channel.flush(2).unwrap(), 2);
        assert_eq!(channel.queued_bytes(), 4);
        channel.enqueue(b"gh").unwrap();
        assert_eq!(channel.flush(6).unwrap(), 6);
        let mut received = [0; 8];
        peer.read_exact(&mut received).unwrap();
        assert_eq!(&received, b"abcdefgh");
        channel.enqueue(b"later").unwrap();
        channel.close();
        assert_eq!(channel.queued_bytes(), 0);
        assert_eq!(
            channel.enqueue(b"x").unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
    }
}
