use super::{
    CryptoError, Direction,
    envelope::{Opener, Sealer},
};

pub const MAX_CONTROL_PAYLOAD_BYTES: usize = 65_511;
pub const CONTROL_ENVELOPE_BYTES: usize = 24;
pub const CONTROL_REPLAY_WINDOW: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlProfile {
    SunshineV2,
    NvidiaLegacy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlOrdering {
    /// Strict increasing global sequence, suitable only for one reliable channel.
    SingleReliableChannel,
    /// One shared 65536-message replay window for independently ordered channels.
    MultipleChannels,
}

pub struct ControlMessage {
    pub sequence: u32,
    pub kind: u16,
    pub payload: Vec<u8>,
}

/// Session/direction-owned; never recreate or clone with a previously used key.
pub struct ControlSealer {
    inner: Sealer,
}

impl ControlSealer {
    pub fn new(
        key: &[u8; 16],
        profile: ControlProfile,
        direction: Direction,
    ) -> Result<Self, CryptoError> {
        modern(profile)?;
        Ok(Self {
            inner: Sealer::new(key, direction, b'C', 0)?,
        })
    }

    pub fn seal(&mut self, kind: u16, payload: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if payload.len() > MAX_CONTROL_PAYLOAD_BYTES {
            return Err(CryptoError::InvalidPacketSize);
        }
        let mut plaintext = Vec::with_capacity(4 + payload.len());
        plaintext.extend_from_slice(&kind.to_le_bytes());
        plaintext.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        plaintext.extend_from_slice(payload);
        let (sequence, tag, ciphertext) = self.inner.seal(&plaintext)?;
        let mut packet = Vec::with_capacity(CONTROL_ENVELOPE_BYTES + ciphertext.len());
        packet.extend_from_slice(&1u16.to_le_bytes());
        packet.extend_from_slice(&((20 + ciphertext.len()) as u16).to_le_bytes());
        packet.extend_from_slice(&sequence.to_le_bytes());
        packet.extend_from_slice(&tag);
        packet.extend_from_slice(&ciphertext);
        Ok(packet)
    }
}

/// Keep one opener per launch key/direction across all ENet channels. A separate
/// opener per channel would admit duplicates and lose the global replay window.
pub struct ControlOpener {
    inner: Opener,
}

impl ControlOpener {
    pub fn new(
        key: &[u8; 16],
        profile: ControlProfile,
        direction: Direction,
        ordering: ControlOrdering,
    ) -> Result<Self, CryptoError> {
        modern(profile)?;
        Ok(Self {
            inner: Opener::new(
                key,
                direction,
                b'C',
                if ordering == ControlOrdering::MultipleChannels {
                    CONTROL_REPLAY_WINDOW
                } else {
                    1
                },
            )?,
        })
    }

    pub fn open(&mut self, packet: &[u8]) -> Result<ControlMessage, CryptoError> {
        validate_frame(packet)?;
        let sequence = u32::from_le_bytes(packet[4..8].try_into().expect("checked prefix"));
        let plaintext = self
            .inner
            .decrypt(sequence, &packet[8..24], &packet[24..])?;
        let kind = u16::from_le_bytes(plaintext[..2].try_into().expect("checked minimum"));
        let length =
            u16::from_le_bytes(plaintext[2..4].try_into().expect("checked minimum")) as usize;
        if length != plaintext.len() - 4 {
            return Err(CryptoError::InvalidEnvelope);
        }
        self.inner.accept(sequence);
        Ok(ControlMessage {
            sequence,
            kind,
            payload: plaintext[4..].to_vec(),
        })
    }
}

fn modern(profile: ControlProfile) -> Result<(), CryptoError> {
    if profile != ControlProfile::SunshineV2 {
        return Err(CryptoError::UnsupportedProfile);
    }
    Ok(())
}

fn validate_frame(packet: &[u8]) -> Result<(), CryptoError> {
    if !(28..=28 + MAX_CONTROL_PAYLOAD_BYTES).contains(&packet.len()) {
        return Err(CryptoError::InvalidPacketSize);
    }
    if packet[..2] != [1, 0] {
        return Err(CryptoError::InvalidEnvelope);
    }
    let length = u16::from_le_bytes(packet[2..4].try_into().expect("checked prefix")) as usize;
    if length != packet.len() - 4 {
        return Err(CryptoError::InvalidEnvelope);
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    #[test]
    fn counter_exhaustion_never_wraps() {
        let mut sealer = ControlSealer::new(
            &[0; 16],
            ControlProfile::SunshineV2,
            Direction::HostToClient,
        )
        .unwrap();
        sealer.inner.next = Some(u32::MAX);
        assert_eq!(&sealer.seal(1, &[]).unwrap()[4..8], &u32::MAX.to_le_bytes());
        assert_eq!(sealer.seal(1, &[]), Err(CryptoError::NonceExhausted));
    }
}
