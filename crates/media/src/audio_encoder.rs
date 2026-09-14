//! Encoded audio delivery. Each nonterminal packet contains one complete Opus packet.
use crate::AudioFrame;
use amaterasu_abi::{NativeInterface, STATUS_INVALID_ARGUMENT, Status};
use std::ffi::c_void;

pub const CODEC_OPUS: u32 = 2;
pub const AUDIO_PACKET_DISCONTINUITY: u32 = 1;
pub const AUDIO_PACKET_END_OF_STREAM: u32 = 2;
pub const MAX_ENCODED_AUDIO_BYTES: usize = 64 * 1024;

/// Opus output uses a 48 kHz sample clock. `frames` counts decoded samples per
/// channel, and `timestamp_us` identifies the first sample relative to the stream
/// epoch. Channel ordering and multistream mapping belong to the control contract.
/// Discontinuity marks a codec/continuity reset; EOS contains no samples or bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncodedAudioPacket {
    pub codec: u32,
    pub flags: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub frames: u32,
    pub data: *const u8,
    pub len: usize,
    pub timestamp_us: u64,
    pub sequence: u64,
}

impl EncodedAudioPacket {
    /// Validates bounded metadata, not packet syntax or pointer validity.
    pub fn validate(&self) -> Result<(), Status> {
        if self.codec != CODEC_OPUS
            || self.sample_rate != 48_000
            || !(1..=8).contains(&self.channels)
            || self.flags & !(AUDIO_PACKET_DISCONTINUITY | AUDIO_PACKET_END_OF_STREAM) != 0
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if self.flags & AUDIO_PACKET_END_OF_STREAM != 0 {
            if self.flags != AUDIO_PACKET_END_OF_STREAM || self.frames != 0 || self.len != 0 {
                return Err(STATUS_INVALID_ARGUMENT);
            }
        } else if !(120..=5760).contains(&self.frames)
            || !self.frames.is_multiple_of(120)
            || self.len == 0
            || self.len > MAX_ENCODED_AUDIO_BYTES
            || self.data.is_null()
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }

    /// # Safety
    /// Nonempty data must contain `len` initialized bytes and remain alive and
    /// immutable for the returned borrow. Validation does not establish this.
    pub unsafe fn bytes(&self) -> Result<&[u8], Status> {
        self.validate()?;
        if self.len == 0 {
            return Ok(&[]);
        }
        // SAFETY: allocation validity and borrow lifetime are caller obligations.
        Ok(unsafe { std::slice::from_raw_parts(self.data, self.len) })
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioPacketSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const EncodedAudioPacket) -> Status,
}

impl AudioPacketSink {
    /// # Safety
    /// The callback/context and packet allocation remain valid through this call.
    /// The callback must not retain the borrow or unwind across the ABI.
    pub unsafe fn send(&self, packet: &EncodedAudioPacket) -> Status {
        if let Err(status) = packet.validate() {
            return status;
        }
        // SAFETY: the caller guarantees both the callback and packet lifetimes.
        unsafe { (self.write)(self.context, packet) }
    }
}

#[repr(C)]
pub struct AudioEncoderApi {
    pub base: NativeInterface,
    /// STATUS_OK accepts all samples; MEDIA_BUSY accepts none. Input is borrowed
    /// for this call only. Format and continuity policy belong to the control API.
    pub submit: unsafe extern "C" fn(*mut c_void, u64, *const AudioFrame) -> Status,
    /// At most one packet callback. Rejected packets remain available for retry.
    pub receive: unsafe extern "C" fn(*mut c_void, u64, AudioPacketSink) -> Status,
    /// Stop accepting input and deliver remaining output, followed by one EOS.
    pub drain: unsafe extern "C" fn(*mut c_void, u64) -> Status,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MEDIA_BUSY;

    fn packet(data: &[u8]) -> EncodedAudioPacket {
        EncodedAudioPacket {
            codec: CODEC_OPUS,
            flags: 0,
            sample_rate: 48_000,
            channels: 2,
            frames: 480,
            data: data.as_ptr(),
            len: data.len(),
            timestamp_us: 10_000,
            sequence: 1,
        }
    }

    #[test]
    fn encoded_audio_rejects_invalid_shapes_and_allows_empty_eos() {
        let data = [1, 2, 3];
        let valid = packet(&data);
        assert_eq!(valid.validate(), Ok(()));
        for invalid in [
            EncodedAudioPacket { codec: 1, ..valid },
            EncodedAudioPacket { flags: 4, ..valid },
            EncodedAudioPacket {
                sample_rate: 44_100,
                ..valid
            },
            EncodedAudioPacket {
                channels: 0,
                ..valid
            },
            EncodedAudioPacket {
                channels: 9,
                ..valid
            },
            EncodedAudioPacket { frames: 0, ..valid },
            EncodedAudioPacket {
                frames: 119,
                ..valid
            },
            EncodedAudioPacket {
                frames: 5761,
                ..valid
            },
            EncodedAudioPacket {
                len: MAX_ENCODED_AUDIO_BYTES + 1,
                ..valid
            },
            EncodedAudioPacket {
                data: std::ptr::null(),
                ..valid
            },
            EncodedAudioPacket {
                flags: AUDIO_PACKET_END_OF_STREAM,
                ..valid
            },
        ] {
            assert_eq!(invalid.validate(), Err(STATUS_INVALID_ARGUMENT));
        }
        let eos = EncodedAudioPacket {
            flags: AUDIO_PACKET_END_OF_STREAM,
            frames: 0,
            len: 0,
            data: std::ptr::null(),
            ..valid
        };
        // SAFETY: EOS never dereferences its null data pointer.
        assert_eq!(unsafe { eos.bytes() }, Ok([].as_slice()));
        assert!(
            EncodedAudioPacket {
                flags: AUDIO_PACKET_END_OF_STREAM | AUDIO_PACKET_DISCONTINUITY,
                ..eos
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn audio_packet_callback_copies_data_and_preserves_rejection() {
        unsafe extern "C" fn copy(
            context: *mut c_void,
            input: *const EncodedAudioPacket,
        ) -> Status {
            // SAFETY: the test supplies a live packet and exclusive output vector.
            let output = unsafe { &mut *context.cast::<Vec<u8>>() };
            let packet = unsafe { &*input };
            output.extend_from_slice(unsafe { packet.bytes().unwrap() });
            MEDIA_BUSY
        }
        let mut output = Vec::<u8>::new();
        let data = vec![1, 2, 3];
        let sink = AudioPacketSink {
            context: (&mut output as *mut Vec<u8>).cast(),
            write: copy,
        };
        // SAFETY: all callback state and input bytes outlive synchronous delivery.
        assert_eq!(unsafe { sink.send(&packet(&data)) }, MEDIA_BUSY);
        drop(data);
        assert_eq!(output, [1, 2, 3]);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn audio_encoder_matches_public_c_layout() {
        assert_eq!(size_of::<EncodedAudioPacket>(), 56);
        assert_eq!(std::mem::offset_of!(EncodedAudioPacket, data), 24);
        assert_eq!(std::mem::offset_of!(EncodedAudioPacket, timestamp_us), 40);
        assert_eq!(size_of::<AudioEncoderApi>(), 56);
        assert_eq!(std::mem::offset_of!(AudioEncoderApi, submit), 32);
        assert_eq!(std::mem::offset_of!(AudioEncoderApi, receive), 40);
        assert_eq!(std::mem::offset_of!(AudioEncoderApi, drain), 48);
    }
}
