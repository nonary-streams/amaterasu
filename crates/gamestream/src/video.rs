//! Bounded GameStream video packetization, with bounded Reed–Solomon parity.
//!
//! H.264 and HEVC travel as Annex B, AV1 as low-overhead OBU temporal units.
//! This is the GameStream video framing protocol, not RFC 6184 fragmentation.
//! See `video/README.md` for receiver references and compatibility limits.

mod av1;
mod fec;
pub(crate) use fec::linear_combination;

const RTP_BYTES: usize = 16;
const NV_BYTES: usize = 16;
const FRAME_BYTES: usize = 8;
const MAX_UDP_BYTES: usize = 65_507;
const MAX_PACKETS: usize = 1023;
const MAX_ACCESS_UNIT_BYTES: usize = 8 * 1024 * 1024;
const STREAM_INDEX_MASK: u32 = 0x00ff_ffff;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VideoCodec {
    #[default]
    H264,
    Hevc,
    Av1,
}

/// Negotiated video settings. Unsupported security/recovery modes fail explicitly.
#[derive(Debug, Clone, Copy)]
pub struct VideoConfig {
    /// NV header plus payload bytes. The UDP datagram adds 16 RTP header bytes.
    pub packet_size: usize,
    pub fec_percentage: u8,
    /// Minimum parity shards per block when FEC is enabled (Moonlight uses 2).
    pub min_parity_shards: u8,
    /// Negotiated version limit: 1 for legacy clients, up to 4 for >=7.1.431.
    pub max_fec_blocks: u8,
    pub encrypted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoError {
    InvalidPacketSize,
    InvalidFec,
    InvalidBlockCount,
    FecWorkLimit,
    UnsupportedEncryption,
    InvalidAnnexB,
    InvalidAv1,
    AccessUnitTooLarge,
    TooManyPackets,
}

impl std::fmt::Display for VideoError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for VideoError {}

/// Exact UDP queue reservation for a successful packetization of this input size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub datagrams: usize,
    pub bytes: usize,
}

/// One stream's sequence state. Create a new instance for a new stream.
///
/// Failed validation consumes no sequence numbers. A successful packetization
/// consumes all numbers, even if the transport subsequently drops some packets.
/// The caller supplies frame indices (normally starting at 1) and a 90 kHz RTP
/// timestamp from the stream's epoch; both may wrap as unsigned 32-bit values.
pub struct VideoPacketizer {
    config: VideoConfig,
    codec: VideoCodec,
    payload_bytes: usize,
    stream_index: u32,
}

impl VideoPacketizer {
    pub fn new(config: VideoConfig) -> Result<Self, VideoError> {
        Self::new_with_codec(config, VideoCodec::H264)
    }

    pub fn new_with_codec(config: VideoConfig, codec: VideoCodec) -> Result<Self, VideoError> {
        if config.encrypted {
            return Err(VideoError::UnsupportedEncryption);
        }
        fec::validate_config(config)?;
        // The first shard must hold the complete short frame header and the
        // beginning of a NAL unit or OBU. Bound the actual IPv4 UDP payload.
        if !(NV_BYTES + FRAME_BYTES + 5..=MAX_UDP_BYTES - RTP_BYTES).contains(&config.packet_size) {
            return Err(VideoError::InvalidPacketSize);
        }
        Ok(Self {
            config,
            codec,
            payload_bytes: config.packet_size - NV_BYTES,
            stream_index: 0,
        })
    }

    /// Bound queue space without parsing, parity generation, or sequence changes.
    /// Bitstream validity still must be checked by packetize before committing.
    pub fn reservation(&self, access_unit_len: usize) -> Result<Reservation, VideoError> {
        if access_unit_len > MAX_ACCESS_UNIT_BYTES {
            return Err(VideoError::AccessUnitTooLarge);
        }
        if access_unit_len == 0 {
            return Err(self.invalid_bitstream());
        }
        if self.config.fec_percentage != 0 {
            return fec::reservation(access_unit_len, self.config);
        }
        let total_bytes = access_unit_len + FRAME_BYTES;
        let datagrams = total_bytes.div_ceil(self.payload_bytes);
        if datagrams > MAX_PACKETS {
            return Err(VideoError::TooManyPackets);
        }
        Ok(Reservation {
            datagrams,
            bytes: total_bytes + datagrams * (RTP_BYTES + NV_BYTES),
        })
    }

    /// Return complete UDP payloads, in transmission order, for one access unit.
    /// Without FEC the final datagram is short. FEC shards are zero-padded to
    /// the negotiated size, as required for recovery by the receiver.
    ///
    /// `encoder_keyframe` is the encoder's keyframe flag. AV1 announces it in
    /// the frame header because Moonlight does not parse AV1. H.264 and HEVC
    /// derive the IDR type from the NAL units, which the receiver also parses.
    pub fn packetize(
        &mut self,
        access_unit: &[u8],
        encoder_keyframe: bool,
        frame_index: u32,
        timestamp_90khz: u32,
    ) -> Result<Vec<Vec<u8>>, VideoError> {
        self.packetize_with_latency(
            access_unit,
            encoder_keyframe,
            frame_index,
            timestamp_90khz,
            0,
        )
    }

    /// [`Self::packetize`], also reporting how long the host held this frame,
    /// in 0.1 ms units, in the frame header's host processing latency field
    /// that Moonlight shows in its statistics. Zero means unknown.
    pub fn packetize_with_latency(
        &mut self,
        access_unit: &[u8],
        encoder_keyframe: bool,
        frame_index: u32,
        timestamp_90khz: u32,
        host_processing_latency: u16,
    ) -> Result<Vec<Vec<u8>>, VideoError> {
        if access_unit.len() > MAX_ACCESS_UNIT_BYTES {
            return Err(VideoError::AccessUnitTooLarge);
        }
        if self.config.fec_percentage != 0 {
            return self.packetize_with_fec(
                access_unit,
                encoder_keyframe,
                frame_index,
                timestamp_90khz,
                host_processing_latency,
            );
        }
        let total_bytes = access_unit.len() + FRAME_BYTES;
        let packet_count = total_bytes.div_ceil(self.payload_bytes);
        if packet_count > MAX_PACKETS {
            return Err(VideoError::TooManyPackets);
        }
        let is_idr = self.keyframe(access_unit, encoder_keyframe)?;
        let frame_header = short_frame_header(
            is_idr,
            ((total_bytes - 1) % self.payload_bytes + 1) as u16,
            host_processing_latency,
        );
        let mut datagrams = Vec::with_capacity(packet_count);
        for packet_index in 0..packet_count {
            let start = packet_index * self.payload_bytes;
            let end = (start + self.payload_bytes).min(total_bytes);
            let stream_index =
                self.stream_index.wrapping_add(packet_index as u32) & STREAM_INDEX_MASK;
            let mut packet = packet_header(
                stream_index,
                frame_index,
                timestamp_90khz,
                packet_index,
                packet_count,
                end - start,
            );
            if start < FRAME_BYTES {
                packet.extend_from_slice(&frame_header[start..end.min(FRAME_BYTES)]);
            }
            packet.extend_from_slice(
                &access_unit[start.saturating_sub(FRAME_BYTES)..end - FRAME_BYTES],
            );
            datagrams.push(packet);
        }
        self.stream_index = self.stream_index.wrapping_add(packet_count as u32) & STREAM_INDEX_MASK;
        Ok(datagrams)
    }

    /// Validate the access unit for this codec and return whether the frame
    /// header announces an IDR (keyframe) picture.
    fn keyframe(&self, access_unit: &[u8], encoder_keyframe: bool) -> Result<bool, VideoError> {
        match self.codec {
            VideoCodec::H264 => validate_annex_b(access_unit),
            VideoCodec::Hevc => validate_hevc_annex_b(access_unit),
            VideoCodec::Av1 => av1::validate_temporal_unit(access_unit, encoder_keyframe),
        }
    }

    fn invalid_bitstream(&self) -> VideoError {
        match self.codec {
            VideoCodec::H264 | VideoCodec::Hevc => VideoError::InvalidAnnexB,
            VideoCodec::Av1 => VideoError::InvalidAv1,
        }
    }
}

/// The eight-byte short frame header that opens the first packet's payload.
///
/// Byte 0 is the short header type (1). Bytes 1..3 hold the little-endian host
/// processing latency in 0.1 ms units, zero when unknown. Byte 3 is the frame type: 2 for IDR,
/// 1 for an ordinary picture. Bytes 4..6 hold the little-endian length of the
/// final data packet's payload, counting this header when that packet is also
/// the first. Receivers truncate the final packet to that length for codecs
/// that cannot tolerate trailing zero padding, such as AV1. Bytes 6..8 are zero.
fn short_frame_header(
    is_idr: bool,
    final_payload_bytes: u16,
    host_processing_latency: u16,
) -> [u8; FRAME_BYTES] {
    let mut header = [0; FRAME_BYTES];
    header[0] = 1;
    header[1..3].copy_from_slice(&host_processing_latency.to_le_bytes());
    header[3] = if is_idr { 2 } else { 1 };
    header[4..6].copy_from_slice(&final_payload_bytes.to_le_bytes());
    header
}

fn packet_header(
    stream_index: u32,
    frame_index: u32,
    timestamp: u32,
    packet_index: usize,
    packet_count: usize,
    payload_bytes: usize,
) -> Vec<u8> {
    let mut packet = Vec::with_capacity(RTP_BYTES + NV_BYTES + payload_bytes);
    packet.resize(RTP_BYTES + NV_BYTES, 0);
    // RTP v2 plus the GameStream four-byte extension. Payload type, SSRC and
    // extension bytes are zero; EOF is an NV flag, not an RTP marker bit.
    packet[0] = 0x90;
    packet[2..4].copy_from_slice(&(stream_index as u16).to_be_bytes());
    packet[4..8].copy_from_slice(&timestamp.to_be_bytes());
    packet[16..20].copy_from_slice(&(stream_index << 8).to_le_bytes());
    packet[20..24].copy_from_slice(&frame_index.to_le_bytes());
    packet[24] = 1
        | if packet_index == 0 { 4 } else { 0 }
        | if packet_index + 1 == packet_count {
            2
        } else {
            0
        };
    packet[26] = 0x10;
    // One block: current and last block indices are both zero. The ten-bit
    // data count and ten-bit shard index are present even with zero parity.
    let fec_info = ((packet_count as u32) << 22) | ((packet_index as u32) << 12);
    packet[28..32].copy_from_slice(&fec_info.to_le_bytes());
    packet
}

/// Check H.264 Annex B framing and supported NAL headers, not the encoded
/// slice syntax.
fn validate_annex_b(bytes: &[u8]) -> Result<bool, VideoError> {
    let mut nal_start = None;
    let mut zero_run = 0;
    let mut has_picture = false;
    let mut has_idr = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte == 1 && zero_run >= 2 {
            if let Some(start) = nal_start {
                validate_nal(
                    bytes,
                    start,
                    index - zero_run,
                    &mut has_picture,
                    &mut has_idr,
                )?;
            } else if index != zero_run {
                return Err(VideoError::InvalidAnnexB);
            }
            nal_start = Some(index + 1);
        }
        zero_run = if byte == 0 { zero_run + 1 } else { 0 };
    }
    let start = nal_start.ok_or(VideoError::InvalidAnnexB)?;
    validate_nal(
        bytes,
        start,
        bytes.len() - zero_run,
        &mut has_picture,
        &mut has_idr,
    )?;
    if !has_picture {
        return Err(VideoError::InvalidAnnexB);
    }
    Ok(has_idr)
}

/// Validate the two-byte HEVC NAL header and the recovery-prefix ordering
/// expected by Moonlight. Initial support accepts closed-GOP IDR only; CRA and
/// leading pictures require additional recovery handling.
fn validate_hevc_annex_b(bytes: &[u8]) -> Result<bool, VideoError> {
    let mut nal_start = None;
    let mut zero_run = 0usize;
    let mut prefix = 0u8;
    let mut picture = None;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte == 1 && zero_run >= 2 {
            if let Some(start) = nal_start {
                validate_hevc_nal(bytes, start, index - zero_run, &mut prefix, &mut picture)?;
            } else if index != zero_run {
                return Err(VideoError::InvalidAnnexB);
            }
            nal_start = Some(index + 1);
        }
        zero_run = if byte == 0 { zero_run + 1 } else { 0 };
    }
    let start = nal_start.ok_or(VideoError::InvalidAnnexB)?;
    validate_hevc_nal(
        bytes,
        start,
        bytes.len() - zero_run,
        &mut prefix,
        &mut picture,
    )?;
    let picture_type = picture.ok_or(VideoError::InvalidAnnexB)?;
    let is_idr = matches!(picture_type, 19 | 20);
    if is_idr && prefix != 3 {
        return Err(VideoError::InvalidAnnexB);
    }
    Ok(is_idr)
}

fn validate_hevc_nal(
    bytes: &[u8],
    start: usize,
    end: usize,
    prefix: &mut u8,
    picture: &mut Option<u8>,
) -> Result<(), VideoError> {
    if end.saturating_sub(start) < 2 {
        return Err(VideoError::InvalidAnnexB);
    }
    let first = bytes[start];
    let second = bytes[start + 1];
    let kind = (first >> 1) & 0x3f;
    if first & 0x80 != 0 || (first & 1) != 0 || second >> 3 != 0 || second & 0x07 == 0 {
        return Err(VideoError::InvalidAnnexB);
    }
    match kind {
        32 if *prefix == 0 && picture.is_none() => *prefix = 1,
        33 if *prefix == 1 && picture.is_none() => *prefix = 2,
        34 if *prefix == 2 && picture.is_none() => *prefix = 3,
        0..=7 | 19 | 20 => {
            let idr = matches!(kind, 19 | 20);
            // Distinct VCL NAL types can carry different picture/recovery
            // semantics. A single access unit must not silently collapse them
            // to the same short-header IDR bit.
            if picture.is_some_and(|previous| previous != kind)
                || (idr && *prefix != 3)
                || (*prefix != 0 && *prefix != 3)
            {
                return Err(VideoError::InvalidAnnexB);
            }
            *picture = Some(kind);
        }
        // Prefix/suffix SEI and access-unit delimiters carry no picture.
        35 | 39 | 40 => {}
        _ => return Err(VideoError::InvalidAnnexB),
    }
    Ok(())
}

fn validate_nal(
    bytes: &[u8],
    start: usize,
    end: usize,
    has_picture: &mut bool,
    has_idr: &mut bool,
) -> Result<(), VideoError> {
    if start >= end {
        return Err(VideoError::InvalidAnnexB);
    }
    let header = bytes[start];
    let kind = header & 0x1f;
    if header & 0x80 != 0 || !(1..=23).contains(&kind) {
        return Err(VideoError::InvalidAnnexB);
    }
    *has_picture |= kind == 1 || kind == 5;
    *has_idr |= kind == 5;
    Ok(())
}

#[cfg(test)]
#[path = "video/tests.rs"]
mod tests;
