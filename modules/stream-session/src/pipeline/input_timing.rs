use amaterasu_module_sdk::diagnostics::{LatencyMetric, elapsed_us};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct InputTiming {
    started: Option<Instant>,
    batch_started: Option<Instant>,
    batch_ack_us: LatencyMetric,
    partial_batches: u64,
    queue_high_water: usize,
    queued_at: VecDeque<Option<Instant>>,
    queue_wait_us: LatencyMetric,
    unknown_queue_samples: u64,
    cancelled_batches: u64,
    control_translation_us: LatencyMetric,
    translation_failures: u64,
    coalesced_events: u64,
}

impl InputTiming {
    pub fn translated(&mut self, started: Instant, failed: bool) {
        self.started.get_or_insert(started);
        self.control_translation_us.record_since(started);
        self.translation_failures = self.translation_failures.saturating_add(u64::from(failed));
    }

    pub fn queued(&mut self, depth: usize) {
        self.started.get_or_insert_with(Instant::now);
        self.queue_high_water = self.queue_high_water.max(depth);
        self.synchronize(depth.saturating_sub(1));
        if depth <= super::input::MAX_QUEUED_EVENTS {
            self.queued_at.push_back(Some(Instant::now()));
        }
    }

    fn synchronize(&mut self, depth: usize) {
        // Diagnostics must never affect input admission. Missing associations
        // remain unknown instead of attributing another event's timestamp.
        if self.queued_at.len() != depth {
            self.queued_at.clear();
            self.queued_at
                .resize(depth.min(super::input::MAX_QUEUED_EVENTS), None);
        }
    }

    pub fn queued_front(&mut self, depth: usize) {
        self.started.get_or_insert_with(Instant::now);
        self.queue_high_water = self.queue_high_water.max(depth);
        self.synchronize(depth.saturating_sub(1));
        if depth <= super::input::MAX_QUEUED_EVENTS {
            self.queued_at.push_front(Some(Instant::now()));
        }
    }

    pub fn coalesced(&mut self, index: usize, remaining: usize) {
        self.synchronize(remaining.saturating_add(1));
        self.queued_at.remove(index);
        self.coalesced_events = self.coalesced_events.saturating_add(1);
    }

    pub fn pop_front(&mut self) {
        self.queued_at.pop_front();
    }

    pub fn clear_queue(&mut self) {
        self.queued_at.clear();
        if self.batch_started.take().is_some() {
            self.cancelled_batches = self.cancelled_batches.saturating_add(1);
        }
    }

    pub fn begin_batch(&mut self, depth: usize, count: usize) {
        if self.batch_started.is_some() {
            return;
        }
        let now = Instant::now();
        self.started.get_or_insert(now);
        self.batch_started = Some(now);
        self.synchronize(depth);
        for timestamp in self.queued_at.iter().take(count) {
            if let Some(timestamp) = timestamp {
                self.queue_wait_us.record(
                    u64::try_from(now.saturating_duration_since(*timestamp).as_micros())
                        .unwrap_or(u64::MAX),
                );
            } else {
                self.unknown_queue_samples = self.unknown_queue_samples.saturating_add(1);
            }
        }
    }

    pub fn acknowledge(&mut self, partial: bool) {
        if let Some(started) = self.batch_started.take() {
            self.batch_ack_us.record_since(started);
        }
        self.partial_batches = self.partial_batches.saturating_add(u64::from(partial));
    }

    pub fn take_if_due(&mut self, stream: u64, depth: usize, inflight: usize) -> Option<Value> {
        let now = Instant::now();
        let started = *self.started.as_ref()?;
        if now.saturating_duration_since(started) < Duration::from_secs(10) {
            return None;
        }
        self.started = Some(now);
        let batch_ack = std::mem::take(&mut self.batch_ack_us);
        let high_water = std::mem::replace(&mut self.queue_high_water, depth);
        let partial = std::mem::take(&mut self.partial_batches);
        let queue_wait = std::mem::take(&mut self.queue_wait_us);
        let unknown = std::mem::take(&mut self.unknown_queue_samples);
        let cancelled = std::mem::take(&mut self.cancelled_batches);
        let translation = std::mem::take(&mut self.control_translation_us);
        let translation_failures = std::mem::take(&mut self.translation_failures);
        let coalesced = std::mem::take(&mut self.coalesced_events);
        Some(json!({"event":"input_batch_window", "stream":stream,
            "window_us":now.saturating_duration_since(started).as_micros().min(u128::from(u64::MAX)) as u64,
            "control_translation_us":translation,"translation_failures":translation_failures,"coalesced_events":coalesced,
            "batch_ack_us":batch_ack, "partial_batches":partial,
            "queue_wait_us":queue_wait,"unknown_queue_samples":unknown,"cancelled_batches":cancelled,
            "oldest_queued_us":self.queued_at.front().copied().flatten().map(elapsed_us),
            "queued_events":depth,"queue_high_water":high_water,"inflight_events":inflight,
            "unacknowledged_batch_age_us":self.batch_started.map(elapsed_us),
            "scope":"batch_scheduled_to_provider_ack_including_persistence_and_broker_not_game_response"}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalescing_and_partial_ack_keep_event_timestamps_aligned() {
        let now = Instant::now();
        let mut timing = InputTiming::default();
        timing.queued(1);
        timing.queued(2);
        timing.queued_at[0] = Some(now - Duration::from_millis(30));
        timing.queued_at[1] = Some(now - Duration::from_millis(10));
        timing.coalesced(1, 1);
        timing.queued(2);
        timing.begin_batch(2, 1);
        assert_eq!(timing.queue_wait_us.calls, 1);
        assert!(timing.queue_wait_us.max_us >= 30_000);
        timing.begin_batch(2, 1);
        assert_eq!(
            timing.queue_wait_us.calls, 1,
            "journal retries must not duplicate samples"
        );
        timing.pop_front();
        timing.acknowledge(true);
        assert_eq!(timing.queued_at.len(), 1);
        assert_eq!(timing.batch_ack_us.calls, 1);
        timing.started = Some(now - Duration::from_secs(11));
        let summary = timing.take_if_due(7, 1, 0).unwrap();
        assert_eq!(summary["partial_batches"], 1);
        assert_eq!(summary["queue_wait_us"]["calls"], 1);
        assert_eq!(timing.queue_wait_us.calls, 0);
        assert!(
            timing.queued_at.front().unwrap().is_some(),
            "reporting must retain pending timestamps"
        );
    }

    #[test]
    fn unknown_associations_and_cancelled_batches_are_explicit() {
        let mut timing = InputTiming::default();
        timing.begin_batch(2, 2);
        assert_eq!(timing.unknown_queue_samples, 2);
        assert_eq!(timing.queue_wait_us.calls, 0);
        timing.clear_queue();
        assert!(timing.batch_started.is_none());
        assert_eq!(timing.cancelled_batches, 1);
        assert!(timing.queued_at.is_empty());
    }
}
