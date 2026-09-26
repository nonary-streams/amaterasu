use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

const TICKS_PER_SECOND: i128 = 10_000_000;

/// Maps the compositor's QPC-based 100 ns frame time to this capture session.
/// Invalid or absent source times use the time at which the worker observed the
/// frame. That fallback says nothing about when the compositor produced it.
pub(super) struct SourceClock {
    origin_ticks: Option<i128>,
    frequency: Option<i128>,
    last_source_ticks: Option<i128>,
    last_timestamp_us: u64,
}

impl SourceClock {
    pub(super) fn start() -> Self {
        let mut counter = 0;
        let mut frequency = 0;
        // SAFETY: both Win32 calls write only to the provided stack locals.
        let clock = unsafe {
            QueryPerformanceFrequency(&mut frequency)
                .and_then(|_| QueryPerformanceCounter(&mut counter))
        };
        let (origin_ticks, frequency) = if clock.is_ok() && counter > 0 && frequency > 0 {
            (Some(i128::from(counter)), Some(i128::from(frequency)))
        } else {
            (None, None)
        };
        Self {
            origin_ticks,
            frequency,
            last_source_ticks: None,
            last_timestamp_us: 0,
        }
    }

    pub(super) fn map(&mut self, source_100ns: Option<i64>, observed_us: u64) -> u64 {
        let observed_ticks = self.observation_ticks();
        self.map_at(source_100ns, observed_us, observed_ticks)
    }

    fn map_at(
        &mut self,
        source_100ns: Option<i64>,
        observed_us: u64,
        observed_ticks: Option<i128>,
    ) -> u64 {
        let mapped_source = self
            .origin_ticks
            .zip(self.frequency)
            .zip(observed_ticks)
            .and_then(|((origin, frequency), observed)| {
                let source = i128::from(source_100ns?);
                let origin_100ns = origin.checked_mul(TICKS_PER_SECOND)? / frequency;
                let observed_100ns = observed.checked_mul(TICKS_PER_SECOND)? / frequency;
                if source <= 0
                    || source < origin_100ns
                    || source > observed_100ns
                    || self.last_source_ticks.is_some_and(|last| source <= last)
                {
                    return None;
                }
                let elapsed_us = u64::try_from((source - origin_100ns) / 10).ok()?;
                (elapsed_us >= self.last_timestamp_us).then_some((source, elapsed_us))
            });
        if let Some((source, _)) = mapped_source {
            self.last_source_ticks = Some(source);
        }
        let candidate = mapped_source.map_or(observed_us, |(_, elapsed_us)| elapsed_us);
        let timestamp = candidate.max(self.last_timestamp_us.saturating_add(1));
        self.last_timestamp_us = timestamp;
        timestamp
    }

    fn observation_ticks(&self) -> Option<i128> {
        self.frequency?;
        let mut counter = 0;
        // SAFETY: QPC writes one i64 to the provided stack local.
        unsafe { QueryPerformanceCounter(&mut counter) }.ok()?;
        (counter > 0).then_some(i128::from(counter))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> SourceClock {
        SourceClock {
            origin_ticks: Some(10_000_000),
            frequency: Some(10_000_000),
            last_source_ticks: None,
            last_timestamp_us: 0,
        }
    }

    #[test]
    fn queued_frame_uses_compositor_time_instead_of_observation_time() {
        let mut clock = clock();
        // A frame rendered at 10 ms and read from the queue at 40 ms.
        assert_eq!(
            clock.map_at(Some(10_100_000), 40_000, Some(10_400_000)),
            10_000
        );
        assert_eq!(
            clock.map_at(Some(10_200_000), 70_000, Some(10_700_000)),
            20_000
        );
    }

    #[test]
    fn invalid_source_times_fall_back_and_never_regress() {
        let mut clock = clock();
        assert_eq!(
            clock.map_at(Some(10_100_000), 20_000, Some(10_200_000)),
            10_000
        );
        assert_eq!(
            clock.map_at(Some(10_050_000), 30_000, Some(10_300_000)),
            30_000
        );
        assert_eq!(
            clock.map_at(Some(10_500_000), 40_000, Some(10_400_000)),
            40_000
        );
        assert_eq!(clock.map_at(Some(0), 41_000, Some(10_410_000)), 41_000);
        assert_eq!(clock.map_at(None, 42_000, Some(10_420_000)), 42_000);
        assert_eq!(
            clock.map_at(Some(10_200_000), 43_000, Some(10_430_000)),
            43_000
        );
        assert_eq!(
            clock.map_at(Some(10_200_000), 44_000, Some(10_440_000)),
            44_000
        );
    }

    #[test]
    fn old_or_unmappable_clock_samples_use_local_observation() {
        let mut clock = clock();
        assert_eq!(
            clock.map_at(Some(9_999_999), 1_000, Some(10_010_000)),
            1_000
        );
        assert_eq!(clock.map_at(Some(i64::MAX), 2_000, Some(10_020_000)), 2_000);
        assert_eq!(clock.map_at(Some(10_010_000), 3_000, None), 3_000);
        clock.frequency = None;
        assert_eq!(
            clock.map_at(Some(10_020_000), 4_000, Some(10_040_000)),
            4_000
        );
    }
}
