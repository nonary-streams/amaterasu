//! Allocation-free latency observations for provider and consumer boundaries.
//!
//! Measurements use a local monotonic clock. They do not establish network,
//! game, or display latency. Percentiles are conservative histogram upper bounds,
//! not exact samples; the overflow bucket uses the observed maximum.
use serde::{Serialize, Serializer, ser::SerializeStruct};
use std::time::Instant;

pub const LATENCY_BUCKET_UPPER_US: [u64; 15] = [
    50, 100, 250, 500, 1_000, 2_000, 4_000, 8_000, 12_000, 16_000, 24_000, 32_000, 64_000, 128_000,
    256_000,
];

/// Fixed storage, saturating counters, and no logging or allocation on record.
/// Empty metrics serialize unknown values as null, rather than measured zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LatencyMetric {
    pub calls: u64,
    pub total_us: u64,
    pub max_us: u64,
    min_us: Option<u64>,
    max_observed_monotonic_us: Option<u64>,
    slow_samples: [Option<SlowSample>; 8],
    slow_samples_total: u64,
    buckets: [u64; LATENCY_BUCKET_UPPER_US.len() + 1],
}

impl LatencyMetric {
    pub fn record(&mut self, elapsed_us: u64) {
        // Keep the histogram and denominator consistent even on saturation.
        if self.calls == u64::MAX {
            return;
        }
        self.calls += 1;
        self.total_us = self.total_us.saturating_add(elapsed_us);
        let observed = (self.calls == 1 || elapsed_us > self.max_us || elapsed_us >= 8000)
            .then(monotonic_time_us)
            .flatten();
        if self.calls == 1 || elapsed_us > self.max_us {
            self.max_us = elapsed_us;
            self.max_observed_monotonic_us = observed;
        }
        if elapsed_us >= 8000 {
            self.slow_samples[(self.slow_samples_total % 8) as usize] = Some(SlowSample {
                elapsed_us,
                observed_monotonic_us: observed,
            });
            self.slow_samples_total = self.slow_samples_total.saturating_add(1);
        }
        self.min_us = Some(self.min_us.map_or(elapsed_us, |old| old.min(elapsed_us)));
        let bucket = LATENCY_BUCKET_UPPER_US.partition_point(|bound| *bound < elapsed_us);
        self.buckets[bucket] += 1;
    }

    pub fn record_since(&mut self, started: Instant) {
        self.record(elapsed_us(started));
    }

    /// Observe one synchronous call, including pending/error returns, without
    /// changing its result or measuring an asynchronous worker's completion.
    pub fn measure<T>(&mut self, operation: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let result = operation();
        self.record_since(started);
        result
    }

    pub fn percentile_upper_us(&self, percentile: u8) -> Option<u64> {
        if self.calls == 0 || !(1..=100).contains(&percentile) {
            return None;
        }
        let rank = (u128::from(self.calls) * u128::from(percentile)).div_ceil(100);
        let mut cumulative = 0u128;
        for (index, count) in self.buckets.iter().enumerate() {
            cumulative += u128::from(*count);
            if cumulative >= rank {
                return Some(
                    LATENCY_BUCKET_UPPER_US
                        .get(index)
                        .copied()
                        .unwrap_or(self.max_us)
                        .min(self.max_us),
                );
            }
        }
        None
    }
}

impl Serialize for LatencyMetric {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("LatencyMetric", 11)?;
        state.serialize_field("calls", &self.calls)?;
        state.serialize_field("total_us", &self.total_us)?;
        state.serialize_field("max_us", &self.max_us)?;
        state.serialize_field("min_us", &self.min_us)?;
        state.serialize_field("max_observed_monotonic_us", &self.max_observed_monotonic_us)?;
        state.serialize_field("samples_at_least_8ms", &self.slow_samples_total)?;
        let start = self.slow_samples_total.saturating_sub(8);
        let samples: Vec<_> = (start..self.slow_samples_total)
            .filter_map(|index| self.slow_samples[(index % 8) as usize])
            .collect();
        state.serialize_field("recent_samples_at_least_8ms", &samples)?;
        state.serialize_field(
            "mean_us",
            &((self.calls != 0).then(|| self.total_us as f64 / self.calls as f64)),
        )?;
        state.serialize_field("p50_upper_us", &self.percentile_upper_us(50))?;
        state.serialize_field("p95_upper_us", &self.percentile_upper_us(95))?;
        state.serialize_field("p99_upper_us", &self.percentile_upper_us(99))?;
        state.end()
    }
}

pub fn elapsed_us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Keep the previous observation across report resets so stalls crossing a
/// window boundary remain visible. Each interval is measured exactly once.
#[derive(Default)]
pub struct Cadence {
    previous: Option<Instant>,
    intervals: LatencyMetric,
}

impl Cadence {
    pub fn observe(&mut self, now: Instant) {
        if let Some(previous) = self.previous.replace(now) {
            self.intervals.record(
                u64::try_from(now.saturating_duration_since(previous).as_micros())
                    .unwrap_or(u64::MAX),
            );
        }
    }

    pub fn take(&mut self) -> LatencyMetric {
        std::mem::take(&mut self.intervals)
    }
}

/// Calling thread CPU time. Missing platform support is explicit, never zero.
pub fn thread_cpu_time_us() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let mut stamp = std::mem::MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: clock_gettime initializes the correctly aligned output on success.
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, stamp.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: the successful call initialized both timespec fields.
        let stamp = unsafe { stamp.assume_init() };
        if stamp.tv_sec < 0 || stamp.tv_nsec < 0 {
            return None;
        }
        Some(
            (stamp.tv_sec as u64)
                .saturating_mul(1_000_000)
                .saturating_add(stamp.tv_nsec as u64 / 1000),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
struct SlowSample {
    elapsed_us: u64,
    observed_monotonic_us: Option<u64>,
}

/// CLOCK_MONOTONIC is shared across processes and modules on this Linux host.
/// A platform without a defined common clock reports unknown correlation.
pub fn monotonic_time_us() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let mut stamp = std::mem::MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: correctly aligned timespec output is initialized on success.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, stamp.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: successful clock_gettime initialized both fields.
        let stamp = unsafe { stamp.assume_init() };
        if stamp.tv_sec < 0 || stamp.tv_nsec < 0 {
            return None;
        }
        Some(
            (stamp.tv_sec as u64)
                .saturating_mul(1_000_000)
                .saturating_add(stamp.tv_nsec as u64 / 1000),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

mod stages;
pub use stages::{StageObservation, measure_stage, take_stages};

pub fn current_thread_id() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: gettid takes no arguments and has no caller-owned memory.
        let id = unsafe { libc::syscall(libc::SYS_gettid) };
        (id > 0).then_some(id as u64)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

mod windows;
pub use windows::window_parts;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_the_last_eight_long_observations_are_retained_with_a_shared_clock() {
        let mut metric = LatencyMetric::default();
        for value in 0..20 {
            metric.record(8000 + value);
        }
        let encoded = serde_json::to_value(metric).unwrap();
        assert_eq!(encoded["samples_at_least_8ms"], 20);
        let samples = encoded["recent_samples_at_least_8ms"].as_array().unwrap();
        assert_eq!(samples.len(), 8);
        assert_eq!(samples[0]["elapsed_us"], 8012);
        assert_eq!(samples[7]["elapsed_us"], 8019);
        if cfg!(target_os = "linux") {
            assert!(encoded["max_observed_monotonic_us"].is_u64());
        }
    }
}
