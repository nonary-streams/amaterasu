use super::CryptoError;
use ring::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    HostToClient,
    ClientToHost,
}

impl Direction {
    fn marker(self) -> u8 {
        match self {
            Self::HostToClient => b'H',
            Self::ClientToHost => b'C',
        }
    }
}

pub(super) struct Sealer {
    key: LessSafeKey,
    domain: [u8; 2],
    pub(super) next: Option<u32>,
}

impl Sealer {
    pub fn new(
        key: &[u8; 16],
        direction: Direction,
        channel: u8,
        first: u32,
    ) -> Result<Self, CryptoError> {
        Ok(Self {
            key: key_for(key)?,
            domain: [direction.marker(), channel],
            next: Some(first),
        })
    }

    pub fn seal(&mut self, plaintext: &[u8]) -> Result<(u32, [u8; 16], Vec<u8>), CryptoError> {
        let sequence = self.next.ok_or(CryptoError::NonceExhausted)?;
        self.next = sequence.checked_add(1);
        let mut ciphertext = plaintext.to_vec();
        let tag = self
            .key
            .seal_in_place_separate_tag(
                Nonce::assume_unique_for_key(nonce(sequence, self.domain)),
                Aad::empty(),
                &mut ciphertext,
            )
            .map_err(|_| CryptoError::EncryptionFailed)?;
        Ok((
            sequence,
            tag.as_ref().try_into().expect("GCM tag length"),
            ciphertext,
        ))
    }
}

pub(super) struct Opener {
    key: LessSafeKey,
    domain: [u8; 2],
    replay: Replay,
}

impl Opener {
    pub fn new(
        key: &[u8; 16],
        direction: Direction,
        channel: u8,
        window: usize,
    ) -> Result<Self, CryptoError> {
        Ok(Self {
            key: key_for(key)?,
            domain: [direction.marker(), channel],
            replay: Replay {
                highest: None,
                seen: vec![0; window.div_ceil(64)],
                window,
            },
        })
    }

    /// Authenticate without changing replay state. The wrapper must validate its
    /// authenticated inner framing before calling accept exactly once.
    pub fn decrypt(
        &self,
        sequence: u32,
        tag: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        self.replay.check(sequence)?;
        let mut buffer = ciphertext.to_vec();
        buffer.extend_from_slice(tag);
        let plaintext = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce(sequence, self.domain)),
                Aad::empty(),
                &mut buffer,
            )
            .map_err(|_| CryptoError::AuthenticationFailed)?;
        Ok(plaintext.to_vec())
    }

    pub fn accept(&mut self, sequence: u32) {
        self.replay.accept(sequence);
    }
}

fn key_for(key: &[u8; 16]) -> Result<LessSafeKey, CryptoError> {
    UnboundKey::new(&AES_128_GCM, key)
        .map(LessSafeKey::new)
        .map_err(|_| CryptoError::EncryptionFailed)
}

fn nonce(sequence: u32, domain: [u8; 2]) -> [u8; 12] {
    let mut nonce = [0; 12];
    nonce[..4].copy_from_slice(&sequence.to_le_bytes());
    nonce[10..].copy_from_slice(&domain);
    nonce
}

struct Replay {
    highest: Option<u32>,
    seen: Vec<u64>,
    window: usize,
}

impl Replay {
    fn bit(&self, sequence: u32) -> (usize, u64) {
        let index = sequence as usize % self.window;
        (index / 64, 1u64 << (index % 64))
    }

    fn check(&self, sequence: u32) -> Result<(), CryptoError> {
        let Some(highest) = self.highest else {
            return Ok(());
        };
        if sequence > highest {
            return Ok(());
        }
        let age = highest - sequence;
        let (word, bit) = self.bit(sequence);
        if age as usize >= self.window || self.seen[word] & bit != 0 {
            return Err(CryptoError::ReplayRejected);
        }
        Ok(())
    }

    fn accept(&mut self, sequence: u32) {
        if let Some(highest) = self.highest {
            if sequence > highest {
                let distance = sequence - highest;
                if distance as usize >= self.window {
                    self.seen.fill(0);
                } else {
                    for offset in 1..=distance {
                        let (word, bit) = self.bit(highest + offset);
                        self.seen[word] &= !bit;
                    }
                }
                self.highest = Some(sequence);
            }
        } else {
            self.highest = Some(sequence);
        }
        let (word, bit) = self.bit(sequence);
        self.seen[word] |= bit;
    }
}
