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

/// A frame is slow when the host spent more than a 120 Hz frame and a half on
/// it between capture and transport admission.
const VIDEO_FLOW_BUDGET: std::time::Duration = std::time::Duration::from_millis(12);

/// Continues the video pipeline's frame flow through this session: the packet
/// was read from the video provider, then admitted by the media transport.
#[derive(Clone, Copy)]
pub(super) struct FrameTrace {
    tracer: amaterasu_module_sdk::trace::Tracer,
    track: amaterasu_module_sdk::trace::Track,
    packet_read: amaterasu_module_sdk::trace::Stage,
    transport_admitted: amaterasu_module_sdk::trace::Stage,
    video_stream: u64,
    last_read: Option<u64>,
}

impl FrameTrace {
    pub fn new(tracer: amaterasu_module_sdk::trace::Tracer, video_stream: u64) -> Self {
        use amaterasu_module_sdk::trace::names;
        Self {
            track: tracer.track(names::VIDEO_TRACK),
            packet_read: tracer.stage(names::SESSION_PACKET_READ),
            transport_admitted: tracer.stage(names::TRANSPORT_ADMITTED),
            tracer,
            video_stream,
            last_read: None,
        }
    }

    /// Marks a frame once, on its first fragment.
    pub fn packet_read(&mut self, sequence: u64) {
        if self.last_read.replace(sequence) != Some(sequence) {
            self.tracer.mark(
                self.track,
                amaterasu_module_sdk::trace::flow_id(self.video_stream, sequence),
                self.packet_read,
            );
        }
    }

    pub fn transport_admitted(&self, sequence: u64) {
        self.tracer.finish(
            self.track,
            amaterasu_module_sdk::trace::flow_id(self.video_stream, sequence),
            self.transport_admitted,
            VIDEO_FLOW_BUDGET,
        );
    }
}
