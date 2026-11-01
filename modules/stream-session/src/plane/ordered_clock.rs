//! Sample-clock continuity for ordered capture with estimated dequeue times.
use amaterasu_media::{AUDIO_DISCONTINUITY, AUDIO_TIMESTAMP_ESTIMATED, OrderedAudioFrameV2};

// Estimated dequeue clocks can vary with scheduler latency. Beyond this bounded
// window the samples are a new segment, not silently treated as continuous.
const MAX_ESTIMATED_JITTER_US: u64 = 2_000;

#[derive(Clone, Copy)]
pub(super) struct OrderedClock {
    origin_us: u64,
    frames: u64,
    last_timestamp_us: u64,
    last_sequence: u64,
}

impl OrderedClock {
    /// Prepare a candidate without advancing state. The caller commits it only
    /// after the encoder accepts the PCM; BUSY must retry the identical header.
    pub(super) fn prepare(
        previous: Option<Self>,
        mut frame: OrderedAudioFrameV2,
    ) -> Result<(OrderedAudioFrameV2, Self), &'static str> {
        let sample_rate = u64::from(frame.frame.sample_rate);
        if sample_rate != 48_000 {
            return Err("unsupported_pcm_format");
        }
        let mut origin = frame.frame.timestamp_us;
        let mut elapsed_frames = 0u64;
        if let Some(last) = previous {
            let next_sequence = last
                .last_sequence
                .checked_add(1)
                .ok_or("ordered_pcm_sequence_exhausted")?;
            if frame.frame.sequence < next_sequence {
                return Err("ordered_pcm_sequence_regressed");
            }
            let elapsed_us = last
                .frames
                .checked_mul(1_000_000)
                .ok_or("ordered_pcm_clock_exhausted")?
                / sample_rate;
            let expected = last
                .origin_us
                .checked_add(elapsed_us)
                .ok_or("ordered_pcm_clock_exhausted")?;
            let explicit_gap = frame.frame.flags & AUDIO_DISCONTINUITY != 0;
            let sequence_gap = frame.frame.sequence != next_sequence;
            let estimated = frame.frame.flags & AUDIO_TIMESTAMP_ESTIMATED != 0;
            let jitter =
                estimated && frame.frame.timestamp_us.abs_diff(expected) <= MAX_ESTIMATED_JITTER_US;
            if !explicit_gap && !sequence_gap && (frame.frame.timestamp_us == expected || jitter) {
                frame.frame.timestamp_us = expected;
                origin = last.origin_us;
                elapsed_frames = last.frames;
            } else {
                // Keep the source's boundary time. A missed packet, explicit
                // discontinuity, or large clock error starts a new FEC segment.
                frame.frame.flags |= AUDIO_DISCONTINUITY;
            }
            if frame.frame.timestamp_us <= last.last_timestamp_us {
                return Err("ordered_pcm_clock_regressed");
            }
        }
        let frames = elapsed_frames
            .checked_add(u64::from(frame.frame.frames))
            .ok_or("ordered_pcm_clock_exhausted")?;
        Ok((
            frame,
            Self {
                origin_us: origin,
                frames,
                last_timestamp_us: frame.frame.timestamp_us,
                last_sequence: frame.frame.sequence,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_media::{AUDIO_ORDERED_VERSION, AudioChannelLayoutV2, AudioFrame, PCM_F32};
    use std::mem::size_of;

    fn frame(sequence: u64, timestamp_us: u64, flags: u32, frames: u32) -> OrderedAudioFrameV2 {
        OrderedAudioFrameV2 {
            size: size_of::<OrderedAudioFrameV2>() as u32,
            version: AUDIO_ORDERED_VERSION,
            stream: 20,
            generation: 1,
            frame: AudioFrame {
                sample_rate: 48_000,
                channels: 2,
                format: PCM_F32,
                frames,
                flags,
                data: std::ptr::null(),
                len: 0,
                timestamp_us,
                sequence,
            },
            layout: AudioChannelLayoutV2 {
                size: size_of::<AudioChannelLayoutV2>() as u32,
                version: AUDIO_ORDERED_VERSION,
                channels: 2,
                reserved: 0,
                mask: 3,
                lanes: [1, 2, 0, 0, 0, 0, 0, 0],
            },
        }
    }

    #[test]
    fn estimated_jitter_is_sample_clocked_and_busy_retry_is_identical() {
        let (_, state) =
            OrderedClock::prepare(None, frame(0, 1_000_000, AUDIO_TIMESTAMP_ESTIMATED, 480))
                .unwrap();
        let input = frame(1, 1_010_001, AUDIO_TIMESTAMP_ESTIMATED, 480);
        let (first, next) = OrderedClock::prepare(Some(state), input).unwrap();
        let (retry, _) = OrderedClock::prepare(Some(state), input).unwrap();
        assert_eq!(
            (first.frame.timestamp_us, first.frame.flags),
            (1_010_000, AUDIO_TIMESTAMP_ESTIMATED)
        );
        assert_eq!(retry.frame.timestamp_us, first.frame.timestamp_us);
        let (third, _) = OrderedClock::prepare(
            Some(next),
            frame(2, 1_019_999, AUDIO_TIMESTAMP_ESTIMATED, 480),
        )
        .unwrap();
        assert_eq!(third.frame.timestamp_us, 1_020_000);
    }

    #[test]
    fn explicit_or_observed_gaps_start_a_new_segment_without_erasing_time() {
        let (_, first) =
            OrderedClock::prepare(None, frame(0, 1_000_000, AUDIO_TIMESTAMP_ESTIMATED, 480))
                .unwrap();
        for input in [
            frame(
                1,
                1_010_001,
                AUDIO_TIMESTAMP_ESTIMATED | AUDIO_DISCONTINUITY,
                480,
            ),
            frame(2, 1_010_000, AUDIO_TIMESTAMP_ESTIMATED, 480),
            frame(1, 1_016_000, AUDIO_TIMESTAMP_ESTIMATED, 480),
            frame(1, 1_010_001, 0, 480),
        ] {
            let (normalized, _) = OrderedClock::prepare(Some(first), input).unwrap();
            assert_ne!(normalized.frame.flags & AUDIO_DISCONTINUITY, 0);
            assert_eq!(normalized.frame.timestamp_us, input.frame.timestamp_us);
        }
        assert!(
            OrderedClock::prepare(
                Some(first),
                frame(1, 999_999, AUDIO_TIMESTAMP_ESTIMATED, 480)
            )
            .is_err()
        );
        assert!(
            OrderedClock::prepare(
                Some(first),
                frame(0, 1_010_000, AUDIO_TIMESTAMP_ESTIMATED, 480)
            )
            .is_err()
        );
    }

    #[test]
    fn fractional_sample_durations_do_not_accumulate_rounding_error() {
        let mut state = None;
        for sequence in 0..480 {
            let expected = 10_000 + sequence * 1_000_000 / 48_000;
            let (output, next) = OrderedClock::prepare(
                state,
                frame(sequence, expected + 1, AUDIO_TIMESTAMP_ESTIMATED, 1),
            )
            .unwrap();
            assert_eq!(output.frame.timestamp_us, expected + 1);
            state = Some(next);
        }
    }
}
