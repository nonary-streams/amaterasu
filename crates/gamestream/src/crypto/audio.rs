use super::CryptoError;
use aes::cipher::{BlockEncryptMut, InnerIvInit, KeyInit, block_padding::Pkcs7};

pub const MAX_AUDIO_PLAINTEXT_BYTES: usize = 1275;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioIvPolicy {
    /// End/rekey the session before any RTP sequence-derived IV can repeat.
    RejectReuse,
    /// Explicit legacy compatibility: the IV repeats after 65536 sequence slots.
    /// CBC provides no authentication and this policy provides no replay protection.
    PermitLegacyWrap,
}

/// Legacy host-to-client audio CBC. Store one instance per session, and encrypt
/// Opus bytes before audio FEC. RTP headers and parity packets are not encrypted.
/// The AES expanded key is zeroized by the enabled RustCrypto feature on drop.
/// This type deliberately does not implement Clone or Debug.
pub struct AudioSealer {
    cipher: aes::Aes128,
    key_id: u32,
    policy: AudioIvPolicy,
    next_sequence: Option<u16>,
    consumed_slots: u64,
}

impl AudioSealer {
    pub fn new(key: &[u8; 16], key_id: u32, policy: AudioIvPolicy) -> Self {
        Self {
            cipher: aes::Aes128::new(key.into()),
            key_id,
            policy,
            next_sequence: None,
            consumed_slots: 0,
        }
    }

    pub fn encrypt(&mut self, sequence: u16, opus: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if opus.is_empty() || opus.len() > MAX_AUDIO_PLAINTEXT_BYTES {
            return Err(CryptoError::InvalidPacketSize);
        }
        if self
            .next_sequence
            .is_some_and(|expected| sequence != expected)
        {
            return Err(CryptoError::AudioSequenceMismatch);
        }
        self.consume(1)?;
        self.next_sequence = Some(sequence.wrapping_add(1));
        let iv = audio_iv(self.key_id, sequence);
        let cipher = cbc::Encryptor::<aes::Aes128>::inner_iv_init(self.cipher.clone(), &iv.into());
        let padded_length = (opus.len() / 16 + 1) * 16;
        let mut ciphertext = vec![0; padded_length];
        cipher
            .encrypt_padded_b2b_mut::<Pkcs7>(opus, &mut ciphertext)
            .map_err(|_| CryptoError::EncryptionFailed)?;
        Ok(ciphertext)
    }

    /// Reserve skipped RTP sequence positions during an explicit discontinuity.
    /// The caller must use the same next sequence in its packetizer. Ambiguous
    /// backward jumps are rejected; skipped slots count toward the IV budget.
    pub fn skip_to(&mut self, sequence: u16) -> Result<(), CryptoError> {
        if let Some(expected) = self.next_sequence {
            let gap = sequence.wrapping_sub(expected);
            if gap > i16::MAX as u16 {
                return Err(CryptoError::AudioSequenceMismatch);
            }
            self.consume(u64::from(gap))?;
        }
        self.next_sequence = Some(sequence);
        Ok(())
    }

    /// Reports the legacy policy's first repeated sequence-derived IV, including
    /// positions deliberately skipped. This does not imply CBC authenticity.
    pub fn iv_reuse_started(&self) -> bool {
        self.consumed_slots > 65_536
    }

    fn consume(&mut self, slots: u64) -> Result<(), CryptoError> {
        let consumed = self
            .consumed_slots
            .checked_add(slots)
            .ok_or(CryptoError::NonceExhausted)?;
        if self.policy == AudioIvPolicy::RejectReuse && consumed > 65_536 {
            return Err(CryptoError::AudioIvExhausted);
        }
        self.consumed_slots = consumed;
        Ok(())
    }
}

fn audio_iv(key_id: u32, sequence: u16) -> [u8; 16] {
    let mut iv = [0; 16];
    iv[..4].copy_from_slice(&key_id.wrapping_add(u32::from(sequence)).to_be_bytes());
    iv
}
