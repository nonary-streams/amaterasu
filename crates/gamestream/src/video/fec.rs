//! Systematic Cauchy Reed–Solomon over GF(256), matching the receiver's matrix.
//! This is error correction, not cryptography. No external code or tables are used.
use super::*;
use std::sync::OnceLock;

const MAX_SHARDS: usize = 255;
// Bound worst-case scalar parity work separately from input and UDP size limits.
const MAX_FIELD_OPERATIONS: usize = 256 * 1024 * 1024;

struct Block {
    data_shards: usize,
    parity_shards: usize,
    percentage: u8,
}

pub(super) fn validate_config(config: VideoConfig) -> Result<(), VideoError> {
    if !(1..=4).contains(&config.max_fec_blocks) {
        return Err(VideoError::InvalidBlockCount);
    }
    if config.fec_percentage == 0 && config.min_parity_shards != 0 {
        return Err(VideoError::InvalidFec);
    }
    if config.fec_percentage != 0 && largest_block(config).is_none() {
        return Err(VideoError::InvalidFec);
    }
    Ok(())
}

fn parity_for(data_shards: usize, config: VideoConfig) -> Option<Block> {
    // Pick the lowest representable percentage satisfying both requested limits.
    // The receiver computes ceil(data * percentage / 100), so derive our count
    // from that same wire value instead of rounding a desired count incorrectly.
    for percentage in config.fec_percentage..=u8::MAX {
        let parity_shards = (data_shards * usize::from(percentage)).div_ceil(100);
        if parity_shards >= usize::from(config.min_parity_shards) {
            return (data_shards + parity_shards <= MAX_SHARDS).then_some(Block {
                data_shards,
                parity_shards,
                percentage,
            });
        }
    }
    None
}

fn largest_block(config: VideoConfig) -> Option<usize> {
    (1..MAX_SHARDS)
        .rev()
        .find(|count| parity_for(*count, config).is_some())
}

fn plan_blocks(data_shards: usize, config: VideoConfig) -> Result<Vec<Block>, VideoError> {
    let maximum = largest_block(config).ok_or(VideoError::InvalidFec)?;
    let block_count = data_shards.div_ceil(maximum);
    if block_count > usize::from(config.max_fec_blocks) {
        return Err(VideoError::TooManyPackets);
    }
    // Balance data across the blocks; only the final global shard is padded.
    let base = data_shards / block_count;
    let remainder = data_shards % block_count;
    let mut blocks = Vec::with_capacity(block_count);
    let mut operations = 0usize;
    for index in 0..block_count {
        let count = base + usize::from(index < remainder);
        let block = parity_for(count, config).ok_or(VideoError::InvalidFec)?;
        operations += block.data_shards * block.parity_shards * (config.packet_size + RTP_BYTES);
        if operations > MAX_FIELD_OPERATIONS {
            return Err(VideoError::FecWorkLimit);
        }
        blocks.push(block);
    }
    Ok(blocks)
}

pub(super) fn reservation(
    access_unit_len: usize,
    config: VideoConfig,
) -> Result<Reservation, VideoError> {
    let data = (access_unit_len + FRAME_BYTES).div_ceil(config.packet_size - NV_BYTES);
    let blocks = plan_blocks(data, config)?;
    let datagrams = blocks
        .iter()
        .map(|block| block.data_shards + block.parity_shards)
        .sum();
    Ok(Reservation {
        datagrams,
        bytes: datagrams * (config.packet_size + RTP_BYTES),
    })
}

impl VideoPacketizer {
    pub(super) fn packetize_with_fec(
        &mut self,
        access_unit: &[u8],
        encoder_keyframe: bool,
        frame_index: u32,
        timestamp: u32,
        host_processing_latency: u16,
    ) -> Result<Vec<Vec<u8>>, VideoError> {
        let total_bytes = access_unit.len() + FRAME_BYTES;
        let blocks = plan_blocks(total_bytes.div_ceil(self.payload_bytes), self.config)?;
        let is_idr = self.keyframe(access_unit, encoder_keyframe)?;
        let mut frame = Vec::with_capacity(total_bytes);
        frame.extend_from_slice(&short_frame_header(
            is_idr,
            ((total_bytes - 1) % self.payload_bytes + 1) as u16,
            host_processing_latency,
        ));
        frame.extend_from_slice(access_unit);
        let total_packets = blocks
            .iter()
            .map(|block| block.data_shards + block.parity_shards)
            .sum();
        let mut packets = Vec::with_capacity(total_packets);
        let mut frame_offset = 0;
        for (block_index, block) in blocks.iter().enumerate() {
            let first_stream_index =
                self.stream_index.wrapping_add(packets.len() as u32) & STREAM_INDEX_MASK;
            let block_bits = ((blocks.len() - 1) << 6 | block_index << 4) as u8;
            let mut shards = self.data_shards(
                &frame,
                &mut frame_offset,
                block,
                first_stream_index,
                frame_index,
                block_bits,
            );
            append_parity(&mut shards, block.parity_shards);
            for (shard_index, shard) in shards.iter_mut().enumerate() {
                stamp_transport_header(
                    shard,
                    first_stream_index.wrapping_add(shard_index as u32),
                    frame_index,
                    timestamp,
                    block_bits,
                    block,
                    shard_index,
                );
            }
            packets.extend(shards);
        }
        self.stream_index =
            self.stream_index.wrapping_add(packets.len() as u32) & STREAM_INDEX_MASK;
        Ok(packets)
    }

    fn data_shards(
        &self,
        frame: &[u8],
        offset: &mut usize,
        block: &Block,
        first_stream_index: u32,
        frame_index: u32,
        block_bits: u8,
    ) -> Vec<Vec<u8>> {
        let mut shards = Vec::with_capacity(block.data_shards + block.parity_shards);
        for index in 0..block.data_shards {
            let mut shard = packet_header(
                first_stream_index.wrapping_add(index as u32) & STREAM_INDEX_MASK,
                frame_index,
                0,
                index,
                block.data_shards,
                self.payload_bytes,
            );
            // The receiver reconstructs transport-derived fields separately.
            // Encode NV picture/index fields, but zero RTP/FEC transport fields.
            shard[..RTP_BYTES].fill(0);
            shard[28..32].fill(0);
            shard[27] = block_bits;
            let end = (*offset + self.payload_bytes).min(frame.len());
            shard.extend_from_slice(&frame[*offset..end]);
            shard.resize(self.config.packet_size + RTP_BYTES, 0);
            *offset = end;
            shards.push(shard);
        }
        shards
    }
}

fn stamp_transport_header(
    shard: &mut [u8],
    stream_index: u32,
    frame_index: u32,
    timestamp: u32,
    block_bits: u8,
    block: &Block,
    shard_index: usize,
) {
    shard[0] = 0x90;
    shard[2..4].copy_from_slice(&(stream_index as u16).to_be_bytes());
    shard[4..8].copy_from_slice(&timestamp.to_be_bytes());
    shard[20..24].copy_from_slice(&frame_index.to_le_bytes());
    shard[27] = block_bits;
    let info = (block.data_shards as u32) << 22
        | (shard_index as u32) << 12
        | u32::from(block.percentage) << 4;
    shard[28..32].copy_from_slice(&info.to_le_bytes());
}

fn append_parity(shards: &mut Vec<Vec<u8>>, parity_count: usize) {
    let data_count = shards.len();
    for row in 0..parity_count {
        // Distinct Cauchy coordinates: x=row, y=parity_count+column.
        // Addition in the field is XOR; each coefficient is 1/(x+y).
        let coefficients: Vec<_> = (0..data_count)
            .map(|column| inverse(((parity_count + column) ^ row) as u8))
            .collect();
        let parity = linear_combination(&coefficients, &shards[..data_count]);
        shards.push(parity);
    }
}

/// Internal shared field primitive. Callers validate equal shard lengths and
/// supply one coefficient per shard; the audio protocol uses a different matrix.
pub(crate) fn linear_combination(coefficients: &[u8], shards: &[Vec<u8>]) -> Vec<u8> {
    assert_eq!(coefficients.len(), shards.len());
    let mut output = vec![0; shards[0].len()];
    let field = multiplication_table();
    for (coefficient, data) in coefficients.iter().zip(shards) {
        assert_eq!(data.len(), output.len());
        let products = &field[usize::from(*coefficient)];
        for (destination, input) in output.iter_mut().zip(data) {
            *destination ^= products[usize::from(*input)];
        }
    }
    output
}

fn multiply(mut left: u8, mut right: u8) -> u8 {
    let mut product = 0;
    for _ in 0..8 {
        if right & 1 != 0 {
            product ^= left;
        }
        let carry = left & 0x80 != 0;
        left <<= 1;
        if carry {
            left ^= 0x1d; // x^8 + x^4 + x^3 + x^2 + 1 (0x11d).
        }
        right >>= 1;
    }
    product
}

fn inverse(value: u8) -> u8 {
    // Nonzero GF(256) elements satisfy a^255=1, hence a^-1=a^254.
    let mut power = value;
    let mut exponent = 254;
    let mut result = 1;
    while exponent != 0 {
        if exponent & 1 != 0 {
            result = multiply(result, power);
        }
        power = multiply(power, power);
        exponent >>= 1;
    }
    result
}

fn multiplication_table() -> &'static [[u8; 256]; 256] {
    static TABLE: OnceLock<Box<[[u8; 256]; 256]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Box::new([[0; 256]; 256]);
        for (left, row) in table.iter_mut().enumerate() {
            for (right, product) in row.iter_mut().enumerate() {
                *product = multiply(left as u8, right as u8);
            }
        }
        table
    })
}

#[cfg(test)]
mod tests;
