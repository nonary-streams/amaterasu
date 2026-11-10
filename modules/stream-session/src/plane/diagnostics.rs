use amaterasu_module_sdk::diagnostics::{Cadence, LatencyMetric};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[derive(Default, Serialize)]
pub(super) struct ForwardingWindow {
    pub control_read_us: LatencyMetric,
    pub video_forward_us: LatencyMetric,
    pub audio_forward_us: LatencyMetric,
    pub audio_capture_us: LatencyMetric,
    pub audio_encoder_submit_us: LatencyMetric,
    pub audio_encoded_read_us: LatencyMetric,
    pub audio_processor_submit_us: LatencyMetric,
    pub audio_processor_read_us: LatencyMetric,
    pub audio_transport_admission_us: LatencyMetric,
    pub control_budget_exhausted: u64,
}

pub(super) struct ForwardingTiming {
    pub window: ForwardingWindow,
    pub cadence: Cadence,
    started: Instant,
}

impl Default for ForwardingTiming {
    fn default() -> Self {
        Self {
            window: ForwardingWindow::default(),
            cadence: Cadence::default(),
            started: Instant::now(),
        }
    }
}

impl ForwardingTiming {
    pub fn take_if_due(&mut self, now: Instant) -> Option<Value> {
        if now.saturating_duration_since(self.started) < Duration::from_secs(10) {
            return None;
        }
        let window = std::mem::take(&mut self.window);
        let interval = self.cadence.take();
        let started = std::mem::replace(&mut self.started, now);
        Some(json!({"event":"stream_forwarding_window",
            "window_us":now.saturating_duration_since(started).as_micros().min(u128::from(u64::MAX)) as u64,
            "timing_us":window,"tick_interval_us":interval,
            "scope":"local_bounded_control_video_audio_pump_calls_including_pending_polls_not_delivery"}))
    }
}
