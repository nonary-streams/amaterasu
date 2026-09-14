use super::{CryptoError, MAX_RTSP_PLAINTEXT_BYTES, RTSP_ENVELOPE_BYTES, rtsp::frame_length};
const MAX_BUFFER: usize = 2 * (RTSP_ENVELOPE_BYTES + MAX_RTSP_PLAINTEXT_BYTES);

/// Connection-local encrypted envelope framing. Opener replay state is separate
/// and must survive connections. Close the connection on any decoder error.
#[derive(Default)]
pub struct RtspEnvelopeDecoder {
    buffer: Vec<u8>,
    poisoned: bool,
}

impl RtspEnvelopeDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), CryptoError> {
        if self.poisoned {
            return Err(CryptoError::DecoderPoisoned);
        }
        if bytes.len() > MAX_BUFFER - self.buffer.len() {
            return self.fail(CryptoError::InvalidPacketSize);
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, CryptoError> {
        if self.poisoned {
            return Err(CryptoError::DecoderPoisoned);
        }
        if self.buffer.len() < 4 {
            return Ok(None);
        }
        let length = match frame_length(&self.buffer) {
            Ok(length) => length,
            Err(error) => return self.fail(error),
        };
        if self.buffer.len() < length {
            return Ok(None);
        }
        Ok(Some(self.buffer.drain(..length).collect()))
    }
    pub fn finish(&mut self) -> Result<(), CryptoError> {
        if self.poisoned {
            return Err(CryptoError::DecoderPoisoned);
        }
        if !self.buffer.is_empty() {
            return self.fail(CryptoError::IncompleteEnvelope);
        }
        Ok(())
    }
    fn fail<T>(&mut self, error: CryptoError) -> Result<T, CryptoError> {
        self.buffer.clear();
        self.poisoned = true;
        Err(error)
    }
}
