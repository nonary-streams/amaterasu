//! Per-endpoint call timing is independent of the shared broker mutex.
use amaterasu_timing_metrics::LatencyMetric;
use std::time::{Duration, Instant};

pub(crate) struct InvocationTiming {
    started: Instant,
    decode: LatencyMetric,
    broker_lock: LatencyMetric,
    broker_dispatch: LatencyMetric,
    native_dispatch: LatencyMetric,
    serialize: LatencyMetric,
    reply: LatencyMetric,
    total: LatencyMetric,
}
impl Default for InvocationTiming {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            decode: LatencyMetric::default(),
            broker_lock: LatencyMetric::default(),
            broker_dispatch: LatencyMetric::default(),
            native_dispatch: LatencyMetric::default(),
            serialize: LatencyMetric::default(),
            reply: LatencyMetric::default(),
            total: LatencyMetric::default(),
        }
    }
}
pub(crate) struct InvocationSample {
    pub decode_us: u64,
    pub lock_us: Option<u64>,
    pub dispatch_us: Option<u64>,
    pub native_us: Option<u64>,
    pub serialize_us: u64,
    pub reply_us: u64,
    pub total_us: u64,
}
pub(crate) fn micros(at: Instant) -> u64 {
    at.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}
impl InvocationTiming {
    pub fn record(
        &mut self,
        principal: &str,
        sample: InvocationSample,
        dropped: u64,
    ) -> Option<String> {
        self.decode.record(sample.decode_us);
        if let Some(value) = sample.lock_us {
            self.broker_lock.record(value);
        }
        if let Some(value) = sample.dispatch_us {
            self.broker_dispatch.record(value);
        }
        if let Some(value) = sample.native_us {
            self.native_dispatch.record(value);
        }
        self.serialize.record(sample.serialize_us);
        self.reply.record(sample.reply_us);
        self.total.record(sample.total_us);
        if self.started.elapsed() < Duration::from_secs(10) {
            return None;
        }
        let window_us = micros(self.started);
        let report = serde_json::json!({"event":"runtime_invocation_window","principal":principal,
            "window_us":window_us,"monotonic_end_us":amaterasu_timing_metrics::monotonic_time_us(),
            "timing_us":{"decode_us":self.decode,"broker_lock_us":self.broker_lock,
                "broker_dispatch_us":self.broker_dispatch,"native_dispatch_us":self.native_dispatch,
                "serialize_us":self.serialize,"reply_callback_us":self.reply,"total_us":self.total},
            "diagnostic_observations_dropped":dropped,
            "scope":"runtime_boundary_json_broker_mutex_native_dispatch_and_reply_calls_including_errors"}).to_string();
        *self = Self::default();
        Some(report)
    }
}
