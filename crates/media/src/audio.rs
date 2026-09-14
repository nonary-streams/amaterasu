//! Interleaved PCM borrowed for a single synchronous capture callback.
use amaterasu_abi::{NativeInterface, STATUS_INVALID_ARGUMENT, Status};
use std::ffi::c_void;

pub const PCM_F32: u32 = 1;
pub const PCM_I16: u32 = 2;
pub const AUDIO_DISCONTINUITY: u32 = 1;
pub const AUDIO_SILENT: u32 = 2;
pub const AUDIO_TIMESTAMP_ESTIMATED: u32 = 4;
pub const MAX_AUDIO_BYTES: usize = 1024 * 1024;

/// Samples use little-endian scalar bytes, interleaved in the platform's reported
/// channel order. Silent
/// packets still contain initialized zero samples. A discontinuity means consumers
/// must reset continuity assumptions; timestamps are relative stream microseconds.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioFrame {
    pub sample_rate: u32,
    pub channels: u32,
    pub format: u32,
    pub frames: u32,
    pub flags: u32,
    pub data: *const u8,
    pub len: usize,
    pub timestamp_us: u64,
    pub sequence: u64,
}

impl AudioFrame {
    /// Validates shape, duration and flags, without proving pointer validity.
    pub fn validate(&self) -> Result<(), Status> {
        let known_flags = AUDIO_DISCONTINUITY | AUDIO_SILENT | AUDIO_TIMESTAMP_ESTIMATED;
        if self.flags & !known_flags != 0 || self.data.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let expected = audio_size(self.sample_rate, self.channels, self.format, self.frames)?;
        if self.len != expected {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }

    /// # Safety
    /// The allocation contains `len` initialized bytes, remains alive for this
    /// borrow, and is not mutated until the borrow ends.
    pub unsafe fn bytes(&self) -> Result<&[u8], Status> {
        self.validate()?;
        // SAFETY: allocation validity and borrowing are the caller's obligations.
        Ok(unsafe { std::slice::from_raw_parts(self.data, self.len) })
    }
}

/// Bounds one delivery to 100 ms of PCM at up to 192 kHz and eight channels.
pub fn audio_size(rate: u32, channels: u32, format: u32, frames: u32) -> Result<usize, Status> {
    if !(8000..=192000).contains(&rate)
        || !(1..=8).contains(&channels)
        || frames == 0
        || frames > rate / 10
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let sample_bytes = match format {
        PCM_F32 => 4,
        PCM_I16 => 2,
        _ => return Err(STATUS_INVALID_ARGUMENT),
    };
    (frames as usize)
        .checked_mul(channels as usize)
        .and_then(|samples| samples.checked_mul(sample_bytes))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MEDIA_BUSY;

    #[test]
    fn pcm_layout_bounds_rate_channels_format_and_duration() {
        assert_eq!(audio_size(48000, 2, PCM_F32, 480), Ok(3840));
        assert_eq!(audio_size(44100, 2, PCM_I16, 441), Ok(1764));
        for (rate, channels, format, frames) in [
            (7999, 2, PCM_F32, 80),
            (192001, 2, PCM_F32, 80),
            (48000, 0, PCM_F32, 80),
            (48000, 9, PCM_F32, 80),
            (48000, 2, 99, 80),
            (48000, 2, PCM_F32, 0),
            (48000, 2, PCM_F32, 4801),
            (48000, u32::MAX, PCM_F32, u32::MAX),
        ] {
            assert_eq!(
                audio_size(rate, channels, format, frames),
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
            format: PCM_I16,
            frames: 80,
            flags: AUDIO_SILENT | AUDIO_DISCONTINUITY | AUDIO_TIMESTAMP_ESTIMATED,
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
        assert!(AudioFrame { flags: 8, ..frame }.validate().is_err());
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
        assert_eq!(size_of::<AudioFrame>(), 56);
        assert_eq!(std::mem::offset_of!(AudioFrame, data), 24);
        assert_eq!(std::mem::offset_of!(AudioFrame, timestamp_us), 40);
        assert_eq!(size_of::<AudioCaptureApi>(), 40);
        assert_eq!(std::mem::offset_of!(AudioCaptureApi, base), 0);
    }
}
