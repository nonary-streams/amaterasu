//! SDK timing diagnostics shared with native providers and the runtime.
pub use amaterasu_timing_metrics::{
    Cadence, LATENCY_BUCKET_UPPER_US, LatencyMetric, current_thread_id, elapsed_us, measure_stage,
    monotonic_time_us,
};
use std::time::Instant;

/// Lifecycle adapter observations identify SDK modules that hold a cooperative
/// host thread or are themselves delayed. Healthy idle modules stay quiet.
pub(crate) struct ModuleTiming {
    started: Instant,
    cadence: Cadence,
    ticks: LatencyMetric,
    health: LatencyMetric,
    failed_ticks: u64,
    failed_health: u64,
    cpu_started_us: Option<u64>,
    cpu_completed_us: Option<u64>,
    tick_thread_cpu_us: LatencyMetric,
    tick_non_cpu_us: LatencyMetric,
    cpu_timing_unknown: u64,
    direct: LatencyMetric,
}

impl ModuleTiming {
    pub fn new(now: Instant) -> Self {
        Self {
            started: now,
            cadence: Cadence::default(),
            ticks: LatencyMetric::default(),
            health: LatencyMetric::default(),
            failed_ticks: 0,
            failed_health: 0,
            cpu_started_us: None,
            cpu_completed_us: None,
            tick_thread_cpu_us: LatencyMetric::default(),
            tick_non_cpu_us: LatencyMetric::default(),
            cpu_timing_unknown: 0,
            direct: LatencyMetric::default(),
        }
    }

    /// Direct calls run inside another module's tick; they are this module's
    /// time on the host thread all the same.
    pub fn observe_direct(&mut self, started: Instant) {
        self.direct.record_since(started);
    }

    pub fn begin(&mut self, now: Instant) {
        self.cadence.observe(now);
        self.cpu_started_us = amaterasu_timing_metrics::thread_cpu_time_us();
    }

    pub fn end_tick_cpu(&mut self) {
        self.cpu_completed_us = amaterasu_timing_metrics::thread_cpu_time_us();
    }

    pub fn observe_health(&mut self, started: Instant, now: Instant, failed: bool) {
        self.health.record(
            u64::try_from(now.saturating_duration_since(started).as_micros()).unwrap_or(u64::MAX),
        );
        self.failed_health = self.failed_health.saturating_add(u64::from(failed));
    }

    pub fn finish(
        &mut self,
        started: Instant,
        now: Instant,
        failed: bool,
    ) -> Option<serde_json::Value> {
        self.ticks.record(
            u64::try_from(now.saturating_duration_since(started).as_micros()).unwrap_or(u64::MAX),
        );
        let wall_us = now
            .saturating_duration_since(started)
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        if let Some(cpu_us) = self
            .cpu_started_us
            .take()
            .zip(
                self.cpu_completed_us
                    .take()
                    .or_else(amaterasu_timing_metrics::thread_cpu_time_us),
            )
            .and_then(|(before, after)| after.checked_sub(before))
        {
            self.tick_thread_cpu_us.record(cpu_us.min(wall_us));
            self.tick_non_cpu_us.record(wall_us.saturating_sub(cpu_us));
        } else {
            self.cpu_timing_unknown = self.cpu_timing_unknown.saturating_add(1);
        }
        self.failed_ticks = self.failed_ticks.saturating_add(u64::from(failed));
        if now.saturating_duration_since(self.started) < std::time::Duration::from_secs(10) {
            return None;
        }
        let ticks = std::mem::take(&mut self.ticks);
        let health = std::mem::take(&mut self.health);
        let interval = self.cadence.take();
        let failed_ticks = std::mem::take(&mut self.failed_ticks);
        let failed_health = std::mem::take(&mut self.failed_health);
        let tick_thread_cpu_us = std::mem::take(&mut self.tick_thread_cpu_us);
        let tick_non_cpu_us = std::mem::take(&mut self.tick_non_cpu_us);
        let cpu_timing_unknown = std::mem::take(&mut self.cpu_timing_unknown);
        let direct = std::mem::take(&mut self.direct);
        let window_start = std::mem::replace(&mut self.started, now);
        let stages = amaterasu_timing_metrics::take_stages();
        let report_required = ticks.calls != 0
            || health.calls != 0
            || ticks.max_us >= 1_000
            || health.max_us >= 1_000
            || interval.max_us >= 20_000
            || failed_ticks != 0
            || failed_health != 0;
        report_required.then(|| serde_json::json!({
            "event":"module_tick_window", "window_us":now.saturating_duration_since(window_start).as_micros().min(u128::from(u64::MAX)) as u64,
            "monotonic_end_us":amaterasu_timing_metrics::monotonic_time_us(),"os_thread_id":amaterasu_timing_metrics::current_thread_id(),"stages":stages,
            "tick_thread_cpu_us":tick_thread_cpu_us,"tick_non_cpu_us":tick_non_cpu_us,"cpu_timing_unknown":cpu_timing_unknown,
            "direct_call_us":direct,
            "tick_us":ticks,"health_us":health,"tick_interval_us":interval,"failed_ticks":failed_ticks,"failed_health":failed_health,
            "scope":"sdk_module_lifecycle_calls_and_tick_start_interval_not_worker_or_gpu_duration"
        }))
    }
}

/// Emit complete structured observations within the host logger's record budget.
pub fn log_window(runtime: &crate::RuntimeClient, target: &str, payload: &serde_json::Value) {
    for part in window_parts(payload) {
        let _ = runtime.log(crate::LogLevel::Info, target, &part.to_string());
    }
}
pub use amaterasu_timing_metrics::window_parts;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn unknown_zero_and_overflow_are_distinct() {
        let mut metric = LatencyMetric::default();
        let empty = serde_json::to_value(metric).unwrap();
        assert!(empty["mean_us"].is_null());
        assert!(empty["p99_upper_us"].is_null());
        metric.record(0);
        assert_eq!(metric.percentile_upper_us(99), Some(0));
        metric.record(300_000);
        assert_eq!(metric.percentile_upper_us(99), Some(300_000));
        assert_eq!(metric.total_us, 300_000);
    }

    #[test]
    fn tails_use_nearest_rank_and_bucket_upper_bounds() {
        let mut metric = LatencyMetric::default();
        for _ in 0..94 {
            metric.record(1_001);
        }
        for _ in 0..5 {
            metric.record(8_001);
        }
        metric.record(90_000);
        assert_eq!(metric.percentile_upper_us(50), Some(2_000));
        assert_eq!(metric.percentile_upper_us(95), Some(12_000));
        assert_eq!(metric.percentile_upper_us(99), Some(12_000));
        assert_eq!(metric.percentile_upper_us(100), Some(90_000));
        assert_eq!(metric.percentile_upper_us(0), None);
    }

    #[test]
    fn report_reset_keeps_stalls_across_windows() {
        let start = Instant::now();
        let mut cadence = Cadence::default();
        cadence.observe(start);
        cadence.observe(start + Duration::from_millis(1));
        assert_eq!(cadence.take().max_us, 1_000);
        cadence.observe(start + Duration::from_millis(101));
        let next = cadence.take();
        assert_eq!(next.calls, 1);
        assert_eq!(next.max_us, 100_000);
    }

    #[test]
    fn module_windows_distinguish_a_slow_call_from_delayed_scheduling() {
        let start = Instant::now();
        let mut timing = ModuleTiming::new(start);
        timing.begin(start);
        assert!(
            timing
                .finish(start, start + Duration::from_micros(50), false)
                .is_none()
        );
        let later = start + Duration::from_secs(11);
        timing.begin(later);
        let summary = timing
            .finish(later, later + Duration::from_micros(100), false)
            .unwrap();
        assert_eq!(summary["tick_us"]["max_us"], 100);
        assert_eq!(summary["tick_interval_us"]["max_us"], 11_000_000);
        assert_eq!(summary["failed_ticks"], 0);
    }
}
