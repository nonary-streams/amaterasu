use crate::model::{HostSnapshot, Sample, Settings};
use crate::provider::{AVAILABLE, PLATFORM, Provider};
use std::collections::{BTreeSet, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_STREAM_ACTIVITIES: usize = 128;
const MAX_RESPONSE_SAMPLES: usize = 32;
const MIN_DASHBOARD_LEASE: Duration = Duration::from_secs(5);

pub struct Collector {
    provider: Provider,
    settings: Settings,
    dashboard_expires: Option<Instant>,
    stream_activities: BTreeSet<String>,
    next_sample_at: Option<Instant>,
    samples: VecDeque<Sample>,
}

impl Collector {
    pub fn new(settings: Settings) -> Self {
        Self {
            provider: Provider::new(),
            settings,
            dashboard_expires: None,
            stream_activities: BTreeSet::new(),
            next_sample_at: None,
            samples: VecDeque::new(),
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn apply_settings(&mut self, settings: Settings, now: Instant) {
        self.settings = settings;
        if !self.settings.enabled {
            self.samples.clear();
            self.dashboard_expires = None;
        }
        self.prune();
        self.samples
            .truncate(self.settings.max_history_points as usize);
        self.next_sample_at = Some(now);
    }

    pub fn read(&mut self, now: Instant) -> (bool, Option<i64>, HostSnapshot, Vec<Sample>) {
        if self.settings.enabled {
            let lease = Duration::from_millis(u64::from(self.settings.poll_interval_ms) * 3)
                .max(MIN_DASHBOARD_LEASE);
            self.dashboard_expires = Some(now + lease);
        }
        self.prune();
        if !self.settings.enabled {
            return (AVAILABLE, None, HostSnapshot::unavailable(), Vec::new());
        }
        let latest = self.samples.back();
        (
            AVAILABLE,
            latest.map(|sample| sample.recorded_at_unix_ms),
            latest
                .map(|sample| sample.host.clone())
                .unwrap_or_else(HostSnapshot::unavailable),
            self.response_samples(),
        )
    }

    pub fn stream_activity_begin(&mut self, session_id: &str) -> Result<(), &'static str> {
        validate_activity_id(session_id)?;
        if self.stream_activities.contains(session_id) {
            return Ok(());
        }
        if self.stream_activities.len() >= MAX_STREAM_ACTIVITIES {
            return Err("too many active stream metric consumers");
        }
        self.stream_activities.insert(session_id.to_owned());
        Ok(())
    }

    pub fn stream_activity_end(&mut self, session_id: &str) -> Result<(), &'static str> {
        validate_activity_id(session_id)?;
        self.stream_activities.remove(session_id);
        Ok(())
    }

    pub fn tick(&mut self, now: Instant) {
        self.prune();
        if !self.settings.enabled || !self.sampling_requested(now) {
            self.next_sample_at = None;
            self.provider.reset_rate_baselines();
            return;
        }
        if self.next_sample_at.is_some_and(|deadline| now < deadline) {
            return;
        }
        self.samples.push_back(Sample {
            recorded_at_unix_ms: unix_time_milliseconds(),
            host: self.provider.sample(now),
        });
        while self.samples.len() > self.settings.max_history_points as usize {
            self.samples.pop_front();
        }
        self.next_sample_at =
            Some(now + Duration::from_millis(u64::from(self.settings.poll_interval_ms)));
        self.prune();
    }

    pub fn platform(&self) -> &'static str {
        PLATFORM
    }

    fn sampling_requested(&self, now: Instant) -> bool {
        if !self.settings.pause_when_hidden {
            return true;
        }
        self.dashboard_expires.is_some_and(|expires| now < expires)
            || !self.stream_activities.is_empty()
    }

    fn response_samples(&self) -> Vec<Sample> {
        let count = self.samples.len();
        if count <= MAX_RESPONSE_SAMPLES {
            return self.samples.iter().cloned().collect();
        }

        let last_index = count - 1;
        (0..MAX_RESPONSE_SAMPLES)
            .map(|point| {
                let index = point * last_index / (MAX_RESPONSE_SAMPLES - 1);
                self.samples[index].clone()
            })
            .collect()
    }

    fn prune(&mut self) {
        let retention_ms = i64::from(self.settings.history_retention_seconds).saturating_mul(1_000);
        let cutoff = unix_time_milliseconds().saturating_sub(retention_ms);
        while self
            .samples
            .front()
            .is_some_and(|sample| sample.recorded_at_unix_ms < cutoff)
        {
            self.samples.pop_front();
        }
        while self.samples.len() > self.settings.max_history_points as usize {
            self.samples.pop_front();
        }
    }
}

fn validate_activity_id(session_id: &str) -> Result<(), &'static str> {
    if session_id.is_empty() || session_id.len() > 128 || session_id.chars().any(char::is_control) {
        return Err("session_id has invalid length or control characters");
    }
    Ok(())
}

fn unix_time_milliseconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Settings;

    #[test]
    fn dashboard_and_stream_consumers_gate_sampling_without_synthetic_samples() {
        let start = Instant::now();
        let mut collector = Collector::new(Settings::default());
        collector.tick(start);
        assert!(collector.samples.is_empty());

        collector.stream_activity_begin("stream-1").unwrap();
        collector.tick(start);
        assert_eq!(collector.samples.len(), 1);
        collector.stream_activity_end("stream-1").unwrap();
        collector.tick(start + Duration::from_secs(10));
        assert_eq!(collector.samples.len(), 1);

        let (available, sampled_at, host, returned) =
            collector.read(start + Duration::from_secs(11));
        assert_eq!(available, AVAILABLE);
        assert!(sampled_at.is_some());
        assert_eq!(returned.len(), 1);
        if !AVAILABLE {
            assert_eq!(host, HostSnapshot::unavailable());
        }
    }

    #[test]
    fn retention_and_point_limits_trim_only_measured_samples() {
        let now = Instant::now();
        let settings = Settings {
            pause_when_hidden: false,
            poll_interval_ms: 250,
            max_history_points: 40,
            ..Settings::default()
        };
        let mut collector = Collector::new(settings);
        for index in 0..50 {
            collector.tick(now + Duration::from_millis(250 * index));
        }
        assert_eq!(collector.samples.len(), 40);
        assert!(
            collector
                .samples
                .iter()
                .all(|sample| sample.recorded_at_unix_ms > 0)
        );
        let (_, _, _, response_samples) = collector.read(now + Duration::from_secs(9));
        assert_eq!(response_samples.len(), MAX_RESPONSE_SAMPLES);
        assert_eq!(response_samples.first(), collector.samples.front());
        assert_eq!(response_samples.last(), collector.samples.back());
        assert!(
            response_samples.windows(2).all(|samples| {
                samples[0].recorded_at_unix_ms <= samples[1].recorded_at_unix_ms
            })
        );
    }

    #[test]
    fn duplicate_activity_is_idempotent_and_invalid_ids_are_rejected() {
        let mut collector = Collector::new(Settings::default());
        collector.stream_activity_begin("session").unwrap();
        collector.stream_activity_begin("session").unwrap();
        assert_eq!(collector.stream_activities.len(), 1);
        assert!(collector.stream_activity_begin("bad\nvalue").is_err());
        collector.stream_activity_end("session").unwrap();
        assert!(collector.stream_activities.is_empty());
    }
}
