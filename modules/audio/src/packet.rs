//! Owned packet creation is independent of COM and testable without audio devices.
use crate::model::{AudioResult, Format, Packet};
use amaterasu_media::{AUDIO_DISCONTINUITY, AUDIO_SILENT, AUDIO_TIMESTAMP_ESTIMATED};
use std::time::Instant;

pub struct Timeline {
    epoch: Instant,
    qpc_origin: Option<u64>,
    last_timestamp: u64,
    next_sequence: u64,
}

impl Timeline {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            qpc_origin: None,
            last_timestamp: 0,
            next_sequence: 0,
        }
    }

    pub fn packet(
        &mut self,
        frames: u32,
        mut flags: u32,
        bytes: Vec<u8>,
        qpc_100ns: u64,
    ) -> AudioResult<Packet> {
        let elapsed_us = self.epoch.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        let timestamp_us = if flags & AUDIO_TIMESTAMP_ESTIMATED != 0 {
            elapsed_us
        } else {
            let origin = *self.qpc_origin.get_or_insert(qpc_100ns);
            match qpc_100ns.checked_sub(origin) {
                Some(delta) => delta / 10,
                None => {
                    flags |= AUDIO_TIMESTAMP_ESTIMATED | AUDIO_DISCONTINUITY;
                    elapsed_us
                }
            }
        };
        let timestamp_us = if timestamp_us < self.last_timestamp {
            flags |= AUDIO_TIMESTAMP_ESTIMATED;
            self.last_timestamp
        } else {
            timestamp_us
        };
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or("sequence_exhausted")?;
        self.last_timestamp = timestamp_us;
        Ok(Packet {
            frames,
            flags,
            bytes,
            timestamp_us,
            sequence,
        })
    }
}

/// # Safety
/// Unless silent, `pointer` must point to the initialized PCM bytes for `frames`
/// complete interleaved sample frames and remain readable until this call returns.
pub unsafe fn copy_samples(
    format: Format,
    frames: u32,
    flags: u32,
    pointer: *const u8,
) -> AudioResult<Vec<u8>> {
    let length = format.packet_bytes(frames)?;
    if flags & AUDIO_SILENT != 0 {
        return Ok(vec![0; length]);
    }
    if pointer.is_null() {
        return Err("invalid_audio_buffer");
    }
    // SAFETY: validated length and caller-provided capture allocation lifetime.
    Ok(unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_media::{PCM_F32, PCM_I16};
    #[test]
    fn copies_interleaved_pcm_and_materializes_silence_without_reading_pointer() {
        let format = Format::new(48_000, 2, PCM_I16).unwrap();
        let bytes = [1, 0, 255, 127, 0, 128, 0, 0];
        // SAFETY: initialized byte array covers two stereo i16 sample frames.
        assert_eq!(
            unsafe { copy_samples(format, 2, 0, bytes.as_ptr()) }.unwrap(),
            bytes
        );
        assert_eq!(
            unsafe { copy_samples(format, 2, AUDIO_SILENT, std::ptr::null()) }.unwrap(),
            [0; 8]
        );
        assert!(unsafe { copy_samples(format, 2, 0, std::ptr::null()) }.is_err());
        assert!(Format::new(48_000, 9, PCM_F32).is_err());
        assert!(format.packet_bytes(4_801).is_err());
    }

    #[test]
    fn timestamps_and_discontinuities_are_explicit_and_monotonic() {
        let mut timeline = Timeline::new();
        let first = timeline.packet(1, 0, vec![0; 4], 100_000).unwrap();
        let next = timeline
            .packet(1, AUDIO_DISCONTINUITY, vec![0; 4], 200_000)
            .unwrap();
        let regressed = timeline.packet(1, 0, vec![0; 4], 1).unwrap();
        assert_eq!((first.timestamp_us, first.sequence), (0, 0));
        assert_eq!((next.timestamp_us, next.sequence), (10_000, 1));
        assert_ne!(next.flags & AUDIO_DISCONTINUITY, 0);
        assert!(regressed.timestamp_us >= next.timestamp_us);
        assert_ne!(regressed.flags & AUDIO_TIMESTAMP_ESTIMATED, 0);
        assert_eq!(regressed.sequence, 2);
    }
}
