use super::CryptoError;
use ring::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};

pub const VIDEO_PREFIX_BYTES: usize = 32;
pub const MAX_VIDEO_PLAINTEXT_BYTES: usize = 65_507 - VIDEO_PREFIX_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoProfile {
    /// Sunshine SS_ENC_VIDEO, host to client, 12-byte nonce and 16-byte tag.
    SunshineHostToClient,
}

/// One session's video invocation counter and key. Deliberately not Clone/Debug.
/// The owner must prevent reuse of this key by another video sealer, including
/// after a process restart. Distinct control directions use separate nonce domains.
pub struct VideoSealer {
    key: LessSafeKey,
    next_counter: Option<u64>,
}

impl VideoSealer {
    pub fn new(key: &[u8; 16], profile: VideoProfile) -> Result<Self, CryptoError> {
        match profile {
            VideoProfile::SunshineHostToClient => {}
        }
        let key = UnboundKey::new(&AES_128_GCM, key).map_err(|_| CryptoError::EncryptionFailed)?;
        Ok(Self {
            key: LessSafeKey::new(key),
            next_counter: Some(0),
        })
    }

    /// Seal a complete plaintext RTP/NV packet after packetization and FEC.
    /// The outer frame number is an unauthenticated routing hint, as specified by
    /// the client. No AAD is used. Authenticated frame metadata stays inside RTP/NV.
    pub fn seal(&mut self, frame_index: u32, packet: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if !(16..=MAX_VIDEO_PLAINTEXT_BYTES).contains(&packet.len()) {
            return Err(CryptoError::InvalidPacketSize);
        }
        let counter = self.next_counter.ok_or(CryptoError::NonceExhausted)?;
        // Burn this invocation before calling crypto. Neither errors nor lost
        // datagrams allow reusing its nonce with another plaintext.
        self.next_counter = counter.checked_add(1);
        let nonce = nonce(counter);
        let mut output = vec![0; VIDEO_PREFIX_BYTES + packet.len()];
        output[..12].copy_from_slice(&nonce);
        output[12..16].copy_from_slice(&frame_index.to_le_bytes());
        output[VIDEO_PREFIX_BYTES..].copy_from_slice(packet);
        let tag = self
            .key
            .seal_in_place_separate_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::empty(),
                &mut output[VIDEO_PREFIX_BYTES..],
            )
            .map_err(|_| CryptoError::EncryptionFailed)?;
        output[16..VIDEO_PREFIX_BYTES].copy_from_slice(tag.as_ref());
        Ok(output)
    }
}

fn nonce(counter: u64) -> [u8; 12] {
    let mut nonce = [0; 12];
    nonce[..8].copy_from_slice(&counter.to_le_bytes());
    nonce[11] = b'V';
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_counter_is_used_once_then_permanently_exhausted() {
        let mut sealer = VideoSealer::new(&[0; 16], VideoProfile::SunshineHostToClient).unwrap();
        sealer.next_counter = Some(u64::MAX);
        assert_eq!(
            sealer.seal(1, &[0; 15]),
            Err(CryptoError::InvalidPacketSize)
        );
        let last = sealer.seal(1, &[0; 16]).unwrap();
        assert_eq!(&last[..8], &[255; 8]);
        assert_eq!(&last[8..12], &[0, 0, 0, b'V']);
        assert_eq!(sealer.seal(2, &[0; 16]), Err(CryptoError::NonceExhausted));
        assert_eq!(sealer.seal(2, &[0; 16]), Err(CryptoError::NonceExhausted));
    }
}
