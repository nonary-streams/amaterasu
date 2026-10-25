//! Structural checks for one AV1 low-overhead temporal unit.
//!
//! GameStream carries AV1 as raw OBUs with no start codes. Moonlight does not
//! parse them; it trusts the short frame header for the frame type and the
//! final payload length. These checks reject framing errors before any
//! sequence number is consumed. They are not a decoder: frame header syntax,
//! tile data, and reference structure are not inspected.
use super::VideoError;

const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_FRAME_HEADER: u8 = 3;
const OBU_TILE_GROUP: u8 = 4;
const OBU_METADATA: u8 = 5;
const OBU_FRAME: u8 = 6;
const OBU_REDUNDANT_FRAME_HEADER: u8 = 7;
const OBU_PADDING: u8 = 15;

/// leb128 values occupy at most eight bytes and must fit in 32 bits.
const MAX_LEB128_BYTES: usize = 8;

struct ObuHeader {
    obu_type: u8,
    header_bytes: usize,
}

/// Check one temporal unit and return whether the short frame header should
/// announce a keyframe. The keyframe decision comes from the encoder: a
/// keyframe must also carry the sequence header, because Moonlight treats a
/// keyframe as a recovery point after loss.
pub(super) fn validate_temporal_unit(
    bytes: &[u8],
    encoder_keyframe: bool,
) -> Result<bool, VideoError> {
    let mut offset = 0;
    let mut index = 0;
    let mut has_sequence_header = false;
    let mut has_picture = false;
    while offset < bytes.len() {
        let header = obu_header(&bytes[offset..])?;
        let (payload_bytes, size_bytes) = leb128(&bytes[offset + header.header_bytes..])?;
        let payload_start = offset + header.header_bytes + size_bytes;
        let payload_end = payload_start
            .checked_add(payload_bytes)
            .filter(|end| *end <= bytes.len())
            .ok_or(VideoError::InvalidAv1)?;
        if (index == 0) != (header.obu_type == OBU_TEMPORAL_DELIMITER) {
            return Err(VideoError::InvalidAv1);
        }
        match header.obu_type {
            // Exactly one empty temporal delimiter, and it opens the unit.
            OBU_TEMPORAL_DELIMITER if payload_bytes == 0 => {}
            // A sequence header precedes the pictures that depend on it.
            OBU_SEQUENCE_HEADER if !has_picture && payload_bytes > 0 => {
                has_sequence_header = true;
            }
            OBU_FRAME_HEADER | OBU_FRAME if payload_bytes > 0 => has_picture = true,
            OBU_TILE_GROUP | OBU_REDUNDANT_FRAME_HEADER if has_picture => {}
            OBU_METADATA | OBU_PADDING => {}
            _ => return Err(VideoError::InvalidAv1),
        }
        offset = payload_end;
        index += 1;
    }
    if index == 0 || !has_picture || (encoder_keyframe && !has_sequence_header) {
        return Err(VideoError::InvalidAv1);
    }
    Ok(encoder_keyframe)
}

/// Parse the one-byte OBU header and its optional extension byte. The
/// low-overhead format requires every OBU to carry its own size field.
fn obu_header(bytes: &[u8]) -> Result<ObuHeader, VideoError> {
    let first = *bytes.first().ok_or(VideoError::InvalidAv1)?;
    let forbidden = first & 0x80 != 0;
    let has_extension = first & 0x04 != 0;
    let has_size_field = first & 0x02 != 0;
    let reserved = first & 0x01 != 0;
    if forbidden || reserved || !has_size_field {
        return Err(VideoError::InvalidAv1);
    }
    let header_bytes = if has_extension {
        // temporal_id(3) spatial_id(2) extension_header_reserved_3bits(3).
        let extension = *bytes.get(1).ok_or(VideoError::InvalidAv1)?;
        if extension & 0x07 != 0 {
            return Err(VideoError::InvalidAv1);
        }
        2
    } else {
        1
    };
    Ok(ObuHeader {
        obu_type: (first >> 3) & 0x0f,
        header_bytes,
    })
}

/// Decode an unsigned little-endian base-128 value, returning the value and
/// the number of bytes it used.
fn leb128(bytes: &[u8]) -> Result<(usize, usize), VideoError> {
    let mut value = 0u64;
    for (index, byte) in bytes.iter().take(MAX_LEB128_BYTES).enumerate() {
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            let value = u32::try_from(value).map_err(|_| VideoError::InvalidAv1)?;
            return Ok((value as usize, index + 1));
        }
    }
    Err(VideoError::InvalidAv1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_decodes_multibyte_sizes_and_rejects_overlong_values() {
        assert_eq!(leb128(&[0x00]), Ok((0, 1)));
        assert_eq!(leb128(&[0xe5, 0x8e, 0x26]), Ok((624_485, 3)));
        assert_eq!(
            leb128(&[0x80, 0x80, 0x80, 0x80, 0x10]),
            Err(VideoError::InvalidAv1)
        );
        assert_eq!(leb128(&[0x80; 8]), Err(VideoError::InvalidAv1));
        assert_eq!(leb128(&[0x80]), Err(VideoError::InvalidAv1));
        assert_eq!(leb128(&[]), Err(VideoError::InvalidAv1));
    }

    #[test]
    fn extension_headers_require_zero_reserved_bits() {
        assert_eq!(obu_header(&[0x36, 0x08]).map(|h| h.header_bytes), Ok(2));
        assert!(obu_header(&[0x36, 0x09]).is_err());
        assert!(obu_header(&[0x36]).is_err());
        assert!(obu_header(&[0x30]).is_err(), "missing size field");
        assert!(obu_header(&[0x33]).is_err(), "reserved bit");
        assert!(obu_header(&[0xb2]).is_err(), "forbidden bit");
    }
}
