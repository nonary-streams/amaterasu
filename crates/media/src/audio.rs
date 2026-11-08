//! PCM borrowed for a single synchronous capture callback.
use crate::AudioFormatId;
use amaterasu_abi::{NativeInterface, STATUS_INVALID_ARGUMENT, Status};
use std::ffi::c_void;

pub const PCM_F32: u32 = 1;
pub const PCM_I16: u32 = 2;
pub const AUDIO_PCM_F32: AudioFormatId = AudioFormatId::standard(1);
pub const AUDIO_PCM_I16: AudioFormatId = AudioFormatId::standard(2);
pub const AUDIO_PCM_I24: AudioFormatId = AudioFormatId::standard(3);
pub const AUDIO_PCM_I32: AudioFormatId = AudioFormatId::standard(4);
pub const AUDIO_PCM_F64: AudioFormatId = AudioFormatId::standard(5);
pub const AUDIO_PACKING_INTERLEAVED: AudioFormatId = AudioFormatId::standard(1);
pub const AUDIO_PACKING_PLANAR: AudioFormatId = AudioFormatId::standard(2);
pub const AUDIO_DISCONTINUITY: u32 = 1;
pub const AUDIO_SILENT: u32 = 2;
pub const AUDIO_TIMESTAMP_ESTIMATED: u32 = 4;
pub const MAX_AUDIO_BYTES: usize = 1024 * 1024;

/// Bytes per scalar for a standard PCM encoding. Unknown encodings specify
/// their storage size in the negotiated descriptor.
pub fn standard_pcm_sample_bytes(format: AudioFormatId) -> Option<u32> {
    match format {
        AUDIO_PCM_F32 | AUDIO_PCM_I32 => Some(4),
        AUDIO_PCM_I16 => Some(2),
        AUDIO_PCM_I24 => Some(3),
        AUDIO_PCM_F64 => Some(8),
        _ => None,
    }
}

/// Standard samples use little-endian scalar bytes. Standard planar buffers contain
/// contiguous planes in negotiated channel order; interleaved buffers contain
/// consecutive samples in that order. Custom packing is defined by its provider.
/// Silent packets contain initialized bytes. Timestamps are stream microseconds.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioFrame {
    pub sample_rate: u32,
    pub channels: u32,
    pub format: AudioFormatId,
    pub sample_bytes: u32,
    pub frames: u32,
    pub flags: u32,
    pub packing: AudioFormatId,
    pub data: *const u8,
    pub len: usize,
    pub timestamp_us: u64,
    pub sequence: u64,
}

impl AudioFrame {
    /// Validates shape, duration and flags, without proving pointer validity.
    pub fn validate(&self) -> Result<(), Status> {
        if (self.data.is_null() && self.len != 0)
            || !self.format.is_present()
            || !self.packing.is_present()
            || self.sample_rate == 0
            || self.channels == 0
            || self.sample_bytes == 0
            || self.frames == 0
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let standard_size = standard_pcm_sample_bytes(self.format);
        if standard_size.is_some_and(|size| self.sample_bytes != size) || self.len > MAX_AUDIO_BYTES
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if matches!(
            self.packing,
            AUDIO_PACKING_INTERLEAVED | AUDIO_PACKING_PLANAR
        ) && self.len
            != audio_size(
                self.sample_rate,
                self.channels,
                self.sample_bytes,
                self.frames,
            )?
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }

    /// # Safety
    /// The allocation contains `len` initialized bytes, remains alive for this
    /// borrow, and is not mutated until the borrow ends.
    pub unsafe fn bytes(&self) -> Result<&[u8], Status> {
        self.validate()?;
        if self.len == 0 {
            return Ok(&[]);
        }
        // SAFETY: allocation validity and borrowing are the caller's obligations.
        Ok(unsafe { std::slice::from_raw_parts(self.data, self.len) })
    }
}

/// Bounds a delivery by its actual byte size, independent of layout or rate.
pub fn audio_size(
    rate: u32,
    channels: u32,
    sample_bytes: u32,
    frames: u32,
) -> Result<usize, Status> {
    if rate == 0 || channels == 0 || sample_bytes == 0 || frames == 0 {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    (frames as usize)
        .checked_mul(channels as usize)
        .and_then(|samples| samples.checked_mul(sample_bytes as usize))
        .filter(|bytes| *bytes <= MAX_AUDIO_BYTES)
        .ok_or(STATUS_INVALID_ARGUMENT)
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const AudioFrame) -> Status,
}

impl AudioSink {
    /// # Safety
    /// Callback, context and frame allocation remain live through this call.
    /// The callback cannot retain borrowed samples or unwind across the ABI.
    pub unsafe fn send(&self, frame: &AudioFrame) -> Status {
        if let Err(status) = frame.validate() {
            return status;
        }
        // SAFETY: caller guarantees the callback and borrowed packet lifetimes.
        unsafe { (self.write)(self.context, frame) }
    }
}

#[repr(C)]
pub struct AudioCaptureApi {
    pub base: NativeInterface,
    /// Delivers at most one packet. Sink rejection retains that packet for retry.
    pub read_audio: unsafe extern "C" fn(*mut c_void, u64, AudioSink) -> Status,
}

/// A stateful PCM transform. Submit accepts all input on OK and none on BUSY.
/// Receive offers at most one output, retaining it when the sink rejects it.
/// After drain, receive returns MEDIA_CLOSED when all output is delivered.
/// Input and output formats are independently negotiated.
#[repr(C)]
pub struct AudioProcessorApi {
    pub base: NativeInterface,
    pub submit: unsafe extern "C" fn(*mut c_void, u64, *const AudioFrame) -> Status,
    pub receive: unsafe extern "C" fn(*mut c_void, u64, AudioSink) -> Status,
    pub drain: unsafe extern "C" fn(*mut c_void, u64) -> Status,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MEDIA_BUSY;

    #[test]
    fn pcm_layout_bounds_rate_channels_format_and_duration() {
        assert_eq!(audio_size(48000, 2, 4, 480), Ok(3840));
        assert_eq!(audio_size(44100, 2, 2, 441), Ok(1764));
        assert_eq!(audio_size(48000, 12, 3, 480), Ok(17280));
        for (rate, channels, sample_bytes, frames) in [
            (0, 2, 4, 80),
            (48000, 0, 4, 80),
            (48000, 2, 0, 80),
            (48000, 2, 4, 0),
            (48000, u32::MAX, 4, u32::MAX),
        ] {
            assert_eq!(
                audio_size(rate, channels, sample_bytes, frames),
                Err(STATUS_INVALID_ARGUMENT)
            );
        }
    }

    #[test]
    fn silent_discontinuous_pcm_is_borrowed_and_sink_rejection_propagates() {
        let samples = vec![0u8; 320];
        let frame = AudioFrame {
            sample_rate: 8000,
            channels: 2,
            format: AUDIO_PCM_I16,
            sample_bytes: 2,
            frames: 80,
            flags: AUDIO_SILENT | AUDIO_DISCONTINUITY | AUDIO_TIMESTAMP_ESTIMATED,
            packing: AUDIO_PACKING_INTERLEAVED,
            data: samples.as_ptr(),
            len: samples.len(),
            timestamp_us: 1000,
            sequence: 7,
        };
        let mut received = Vec::<u8>::new();
        unsafe extern "C" fn copy(context: *mut c_void, frame: *const AudioFrame) -> Status {
            // SAFETY: test provides exclusive destination and live initialized samples.
            let destination = unsafe { &mut *context.cast::<Vec<u8>>() };
            let frame = unsafe { &*frame };
            destination.extend_from_slice(unsafe { frame.bytes().unwrap() });
            MEDIA_BUSY
        }
        let sink = AudioSink {
            context: (&mut received as *mut Vec<u8>).cast(),
            write: copy,
        };
        // SAFETY: callback state and sample allocation outlive synchronous delivery.
        assert_eq!(unsafe { sink.send(&frame) }, MEDIA_BUSY);
        drop(samples);
        assert_eq!(received, vec![0u8; 320]);
        assert!(AudioFrame { flags: 8, ..frame }.validate().is_ok());
        assert!(AudioFrame { len: 319, ..frame }.validate().is_err());
        assert!(
            AudioFrame {
                data: std::ptr::null(),
                ..frame
            }
            .validate()
            .is_err()
        );
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn pcm_table_matches_public_c_layout() {
        assert_eq!(size_of::<AudioFrame>(), 104);
        assert_eq!(std::mem::offset_of!(AudioFrame, data), 72);
        assert_eq!(std::mem::offset_of!(AudioFrame, timestamp_us), 88);
        assert_eq!(size_of::<AudioCaptureApi>(), 40);
        assert_eq!(std::mem::offset_of!(AudioCaptureApi, base), 0);
    }

    #[test]
    fn pcm_supports_planar_and_provider_defined_formats() {
        let bytes = [0u8; 144];
        let frame = AudioFrame {
            sample_rate: 96000,
            channels: 12,
            format: AUDIO_PCM_I24,
            sample_bytes: 3,
            frames: 4,
            flags: 0,
            packing: AUDIO_PACKING_PLANAR,
            data: bytes.as_ptr(),
            len: bytes.len(),
            timestamp_us: 0,
            sequence: 0,
        };
        assert!(frame.validate().is_ok());
        assert!(
            AudioFrame {
                sample_bytes: 4,
                ..frame
            }
            .validate()
            .is_err()
        );
        let custom = AudioFormatId {
            namespace: 123,
            value: 7,
            version: 1,
        };
        let padded = [0u8; 160];
        assert!(
            AudioFrame {
                format: custom,
                packing: custom,
                data: padded.as_ptr(),
                len: padded.len(),
                ..frame
            }
            .validate()
            .is_ok()
        );
        assert!(
            AudioFrame {
                format: custom,
                packing: custom,
                data: std::ptr::null(),
                len: 0,
                ..frame
            }
            .validate()
            .is_ok()
        );
        assert!(
            AudioFrame {
                format: custom,
                packing: custom,
                len: 5,
                ..frame
            }
            .validate()
            .is_ok()
        );
    }
}
