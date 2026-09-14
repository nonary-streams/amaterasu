use super::{
    CryptoError, Direction,
    envelope::{Opener, Sealer},
};

pub const RTSP_ENVELOPE_BYTES: usize = 24;
pub const MAX_RTSP_PLAINTEXT_BYTES: usize =
    crate::rtsp::MAX_HEADER_BYTES + crate::rtsp::MAX_BODY_BYTES;
const ENCRYPTED: u32 = 0x8000_0000;

pub struct RtspMessage {
    pub sequence: u32,
    pub plaintext: Vec<u8>,
}

/// Retain across TCP connections within a session. The first invocation is one.
pub struct RtspSealer {
    inner: Sealer,
}

impl RtspSealer {
    pub fn new(key: &[u8; 16], direction: Direction) -> Result<Self, CryptoError> {
        Ok(Self {
            inner: Sealer::new(key, direction, b'R', 1)?,
        })
    }
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if plaintext.is_empty() || plaintext.len() > MAX_RTSP_PLAINTEXT_BYTES {
            return Err(CryptoError::InvalidPacketSize);
        }
        let (sequence, tag, ciphertext) = self.inner.seal(plaintext)?;
        let mut packet = Vec::with_capacity(RTSP_ENVELOPE_BYTES + ciphertext.len());
        packet.extend_from_slice(&(ENCRYPTED | ciphertext.len() as u32).to_be_bytes());
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(&tag);
        packet.extend_from_slice(&ciphertext);
        Ok(packet)
    }
}

/// Session-wide replay state, not connection-local state. The 128-message window
/// permits independent TCP transactions to arrive out of order once each.
pub struct RtspOpener {
    inner: Opener,
}

impl RtspOpener {
    pub fn new(key: &[u8; 16], direction: Direction) -> Result<Self, CryptoError> {
        Ok(Self {
            inner: Opener::new(key, direction, b'R', 128)?,
        })
    }
    pub fn open(&mut self, packet: &[u8]) -> Result<RtspMessage, CryptoError> {
        if packet.len() < RTSP_ENVELOPE_BYTES {
            return Err(CryptoError::InvalidEnvelope);
        }
        let expected = frame_length(packet)?;
        if packet.len() != expected {
            return Err(CryptoError::InvalidEnvelope);
        }
        let sequence = u32::from_be_bytes(packet[4..8].try_into().expect("checked prefix"));
        if sequence == 0 {
            return Err(CryptoError::InvalidEnvelope);
        }
        let plaintext = self
            .inner
            .decrypt(sequence, &packet[8..24], &packet[24..])?;
        self.inner.accept(sequence);
        Ok(RtspMessage {
            sequence,
            plaintext,
        })
    }
}

/// Return the complete encrypted envelope length once four prefix bytes exist.
/// This checks only bounded framing; it authenticates nothing and never accepts
/// plaintext RTSP as a fallback. A caller still needs all bytes and an opener.
pub fn encrypted_rtsp_frame_length(prefix: &[u8]) -> Result<Option<usize>, CryptoError> {
    if prefix.len() < 4 {
        return Ok(None);
    }
    frame_length(prefix).map(Some)
}

pub(super) fn frame_length(prefix: &[u8]) -> Result<usize, CryptoError> {
    let field = u32::from_be_bytes(prefix[..4].try_into().expect("four-byte prefix"));
    if field & ENCRYPTED == 0 {
        return Err(CryptoError::UnsupportedProfile);
    }
    let length = (field & !ENCRYPTED) as usize;
    if !(1..=MAX_RTSP_PLAINTEXT_BYTES).contains(&length) {
        return Err(CryptoError::InvalidPacketSize);
    }
    Ok(RTSP_ENVELOPE_BYTES + length)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn last_invocation_is_one_shot_and_invalid_input_preserves_it() {
        let mut sealer = RtspSealer::new(&[0; 16], Direction::ClientToHost).unwrap();
        sealer.inner.next = Some(u32::MAX);
        assert_eq!(sealer.seal(&[]), Err(CryptoError::InvalidPacketSize));
        assert_eq!(&sealer.seal(b"x").unwrap()[4..8], &u32::MAX.to_be_bytes());
        assert_eq!(sealer.seal(b"x"), Err(CryptoError::NonceExhausted));
    }
}
