use amaterasu_module_sdk::diagnostics::LatencyMetric;
use serde::Serialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[derive(Default, Serialize)]
pub(crate) struct InjectionWindow {
    pub event_injection_us: LatencyMetric,
    pub keyboard_us: LatencyMetric,
    pub pointer_us: LatencyMetric,
    pub controller_us: LatencyMetric,
    pub pen_touch_us: LatencyMetric,
    pub failed_events: u64,
    pub suppressed_events: u64,
}

impl InjectionWindow {
    pub fn record_kind(&mut self, event: &crate::model::Event, elapsed_us: u64) {
        use crate::model::Event;
        match event {
            Event::Key { .. } => &mut self.keyboard_us,
            Event::PenTouch { .. } => &mut self.pen_touch_us,
            Event::Controller { .. }
            | Event::ControllerArrival { .. }
            | Event::ControllerTouch { .. }
            | Event::ControllerMotion { .. }
            | Event::ControllerBattery { .. } => &mut self.controller_us,
            _ => &mut self.pointer_us,
        }
        .record(elapsed_us);
    }
}

#[derive(Default)]
pub(crate) struct InjectionTiming {
    pub window: InjectionWindow,
    started: Option<Instant>,
}

impl InjectionTiming {
    pub fn begin(&mut self, now: Instant) {
        self.started.get_or_insert(now);
    }

    pub fn take_if_due(&mut self, now: Instant) -> Option<Value> {
        let started = *self.started.as_ref()?;
        if now.saturating_duration_since(started) < Duration::from_secs(10) {
            return None;
        }
        self.started = Some(now);
        let window = std::mem::take(&mut self.window);
        (window.event_injection_us.calls != 0 || window.suppressed_events != 0).then(|| json!({
            "event":"input_injection_window", "window_us":now.saturating_duration_since(started).as_micros().min(u128::from(u64::MAX)) as u64,
            "timing_us":window,
            "scope":"enabled_event_backend_and_repeat_policy_call_not_game_consumption"
        }))
    }
}
