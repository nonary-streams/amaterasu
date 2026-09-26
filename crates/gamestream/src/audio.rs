//! Fixed-duration GameStream Opus RTP with four data and two audio FEC shards.
mod opus;

use crate::video::linear_combination;

const RTP_BYTES: usize = 12;
const FEC_BYTES: usize = 12;
const MAX_WIRE_PAYLOAD: usize = 1280; // Maximum AES-CBC padded Opus shard.
const MAX_OPUS_BYTES: usize = 1275;
const AUDIO_MATRIX: [[u8; 4]; 2] = [[0x77, 0x40, 0x38, 0x0e], [0xc7, 0xa7, 0x0d, 0x6c]];

#[derive(Clone, Copy, Debug)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    /// 240 (5 ms) or 480 (10 ms) decoded frames at 48 kHz.
    pub frames_per_packet: u32,
    /// Constant data-shard length after optional CBC encryption, excluding RTP.
    pub wire_payload_bytes: usize,
    pub ssrc: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioError {
    UnsupportedFormat,
    InvalidPayloadSize,
    InvalidOpus,
    FrameCountMismatch,
    PayloadLengthChanged,
    EncryptionFailed,
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for AudioError {}

/// Result of explicitly abandoning an incomplete FEC group. Its data packets
/// have already been emitted; no parity or silent replacement packets are made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Discontinuity {
    pub discarded_shards: u8,
    pub skipped_packets: u8,
    pub next_sequence: u16,
    pub timestamp_ms: u32,
}

pub struct AudioPacketizer {
    config: AudioConfig,
    sequence: u16,
    timestamp_ms: u32,
    group_timestamp_ms: u32,
    pending: Vec<Vec<u8>>,
}

impl AudioPacketizer {
    pub fn new(config: AudioConfig) -> Result<Self, AudioError> {
        if config.sample_rate != 48000
            || !matches!(config.channels, 2 | 6 | 8)
            || !matches!(config.frames_per_packet, 240 | 480)
        {
            return Err(AudioError::UnsupportedFormat);
        }
        if !(1..=MAX_WIRE_PAYLOAD).contains(&config.wire_payload_bytes) {
            return Err(AudioError::InvalidPayloadSize);
        }
        Ok(Self {
            config,
            sequence: 0,
            timestamp_ms: 0,
            group_timestamp_ms: 0,
            pending: Vec::with_capacity(4),
        })
    }

    pub fn next_sequence(&self) -> u16 {
        self.sequence
    }

    /// Non-mutating preflight for transport queue reservation: one data packet,
    /// plus two parity packets when this call completes the four-packet group.
    pub fn required_datagrams(&self) -> usize {
        if self.pending.len() == 3 { 3 } else { 1 }
    }

    pub fn maximum_datagram_bytes(&self) -> usize {
        RTP_BYTES + FEC_BYTES + self.config.wire_payload_bytes
    }

    /// Plaintext Opus must have a constant encoded length, normally using CBR.
    pub fn packetize(
        &mut self,
        opus: &[u8],
        decoded_frames: u32,
    ) -> Result<Vec<Vec<u8>>, AudioError> {
        self.validate_input(opus, decoded_frames)?;
        self.validate_wire_length(opus.len())?;
        Ok(self.commit_payload(opus.to_vec()))
    }

    /// Legacy CBC encryption happens before FEC. Length and input validation run
    /// before invoking the sealer, avoiding nonce consumption on invalid input.
    /// The callback must implement AES-128-CBC with PKCS7 and return its ciphertext.
    /// A callback failure or unexpected length is fatal to the surrounding stream:
    /// the callback may have consumed cipher state, although this packetizer has not.
    pub fn packetize_sealed(
        &mut self,
        opus: &[u8],
        decoded_frames: u32,
        seal: impl FnOnce(u16, &[u8]) -> Result<Vec<u8>, AudioError>,
    ) -> Result<Vec<Vec<u8>>, AudioError> {
        self.validate_input(opus, decoded_frames)?;
        self.validate_wire_length((opus.len() / 16 + 1) * 16)?;
        let ciphertext = seal(self.sequence, opus)?;
        self.validate_wire_length(ciphertext.len())?;
        Ok(self.commit_payload(ciphertext))
    }

    /// Discard only cached parity inputs and skip to the next four-sequence
    /// boundary. Synchronize a legacy sealer using its explicit skip_to method.
    /// Closing a stream needs no drain: dropping it simply emits no partial FEC.
    pub fn reset_discontinuity(&mut self) -> Discontinuity {
        let discarded_shards = self.pending.len() as u8;
        let skipped_packets = if discarded_shards == 0 {
            0
        } else {
            4 - discarded_shards
        };
        self.pending.clear();
        self.sequence = self.sequence.wrapping_add(u16::from(skipped_packets));
        self.timestamp_ms = self
            .timestamp_ms
            .wrapping_add(u32::from(skipped_packets) * self.duration_ms());
        self.group_timestamp_ms = self.timestamp_ms;
        Discontinuity {
            discarded_shards,
            skipped_packets,
            next_sequence: self.sequence,
            timestamp_ms: self.timestamp_ms,
        }
    }

    fn duration_ms(&self) -> u32 {
        self.config.frames_per_packet / 48
    }

    /// Validate encoded framing and negotiated duration without consuming state.
    pub fn validate_input(&self, bytes: &[u8], frames: u32) -> Result<(), AudioError> {
        if bytes.is_empty() || bytes.len() > MAX_OPUS_BYTES {
            return Err(AudioError::InvalidPayloadSize);
        }
        if frames != self.config.frames_per_packet {
            return Err(AudioError::FrameCountMismatch);
        }
        if opus::decoded_frames(bytes)? != frames {
            return Err(AudioError::FrameCountMismatch);
        }
        Ok(())
    }

    fn validate_wire_length(&self, length: usize) -> Result<(), AudioError> {
        if length != self.config.wire_payload_bytes {
            return Err(AudioError::PayloadLengthChanged);
        }
        Ok(())
    }

    fn commit_payload(&mut self, payload: Vec<u8>) -> Vec<Vec<u8>> {
        let mut packets = Vec::with_capacity(self.required_datagrams());
        if self.pending.is_empty() {
            self.group_timestamp_ms = self.timestamp_ms;
        }
        let mut data = rtp_header(97, self.sequence, self.timestamp_ms, self.config.ssrc);
        data.extend_from_slice(&payload);
        packets.push(data);
        self.pending.push(payload);
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp_ms = self.timestamp_ms.wrapping_add(self.duration_ms());
        if self.pending.len() == 4 {
            self.append_parity(&mut packets);
            self.pending.clear();
        }
        packets
    }

    fn append_parity(&self, packets: &mut Vec<Vec<u8>>) {
        let base_sequence = self.sequence.wrapping_sub(4);
        for (index, coefficients) in AUDIO_MATRIX.iter().enumerate() {
            // Parity RTP sequence values overlap the next data sequence values;
            // parity does not consume the audio data clock or sequence counter.
            let mut packet = rtp_header(
                127,
                self.sequence.wrapping_add(index as u16),
                0,
                self.config.ssrc,
            );
            packet.push(index as u8);
            packet.push(97);
            packet.extend_from_slice(&base_sequence.to_be_bytes());
            packet.extend_from_slice(&self.group_timestamp_ms.to_be_bytes());
            packet.extend_from_slice(&self.config.ssrc.to_be_bytes());
            packet.extend_from_slice(&linear_combination(coefficients, &self.pending));
            packets.push(packet);
        }
    }
}

fn rtp_header(payload_type: u8, sequence: u16, timestamp: u32, ssrc: u32) -> Vec<u8> {
    let mut header = Vec::with_capacity(RTP_BYTES);
    header.extend_from_slice(&[0x80, payload_type]);
    header.extend_from_slice(&sequence.to_be_bytes());
    header.extend_from_slice(&timestamp.to_be_bytes());
    header.extend_from_slice(&ssrc.to_be_bytes());
    header
}

#[cfg(test)]
mod tests;
