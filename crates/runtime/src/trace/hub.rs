//! Owns the queue, the name table and the collector thread.
use super::collector::{Collector, Report, Resolver};
use super::names::Names;
use super::report::{self, TrendPoint};
use super::ring::{Observation, Ring};
use crate::Broker;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const RING_CAPACITY: usize = 16_384;
/// The collector sleeps this long after finding work, and longer when idle.
const BUSY_PAUSE: Duration = Duration::from_millis(5);
const IDLE_PAUSE: Duration = Duration::from_millis(50);
/// Full detail for the last minute, a trend for the last half hour, and the
/// most recent slow moments.
const RETAINED_WINDOWS: usize = 6;
const RETAINED_TREND: usize = 180;
const RETAINED_SLOW: usize = 30;

/// What a management surface may show. The report module reduces it to one
/// bounded response.
#[derive(Clone, Default)]
struct History {
    windows: VecDeque<Value>,
    trend: VecDeque<TrendPoint>,
    slow: VecDeque<Value>,
}

fn retain<T>(history: &mut VecDeque<T>, limit: usize, item: T) {
    if history.len() >= limit {
        history.pop_front();
    }
    history.push_back(item);
}

impl History {
    fn record(&mut self, collected: &Report, wall_ms: u64) {
        if collected.target == "trace.window" {
            retain(
                &mut self.trend,
                RETAINED_TREND,
                report::trend_point(&collected.body, wall_ms),
            );
            retain(&mut self.windows, RETAINED_WINDOWS, collected.body.clone());
        } else {
            report::retain_slow(
                &mut self.slow,
                RETAINED_SLOW,
                collected.target,
                &collected.body,
                wall_ms,
            );
        }
    }
}

pub(crate) struct TraceHub {
    ring: Ring,
    names: Mutex<Names>,
    sources: Vec<String>,
    epoch: Instant,
    history: Mutex<History>,
    stop: AtomicBool,
    collector: OnceLock<Mutex<Option<JoinHandle<()>>>>,
}

struct HubResolver<'a> {
    hub: &'a TraceHub,
}

impl Resolver for HubResolver<'_> {
    fn name(&self, identifier: u16) -> String {
        self.hub.names.lock().map_or_else(
            |_| "unknown".into(),
            |names| names.name(identifier).to_owned(),
        )
    }

    fn source(&self, identifier: u16) -> String {
        self.hub.source_name(identifier).to_owned()
    }
}

impl TraceHub {
    pub fn new(sources: Vec<String>) -> Self {
        Self {
            ring: Ring::new(RING_CAPACITY),
            names: Mutex::new(Names::default()),
            sources,
            epoch: Instant::now(),
            history: Mutex::new(History::default()),
            stop: AtomicBool::new(false),
            collector: OnceLock::new(),
        }
    }

    /// Source identifiers start at one, in the runtime's principal order.
    pub fn source_id(&self, principal: &str) -> u16 {
        self.sources
            .iter()
            .position(|source| source == principal)
            .map_or(0, |index| index as u16 + 1)
    }

    fn source_name(&self, identifier: u16) -> &str {
        identifier
            .checked_sub(1)
            .and_then(|index| self.sources.get(usize::from(index)))
            .map_or("unknown", String::as_str)
    }

    /// One clock for every record. The host monotonic clock lets these
    /// timestamps line up with other diagnostics; elsewhere a runtime-local
    /// epoch still orders records from all modules consistently.
    pub fn now_us(&self) -> u64 {
        amaterasu_timing_metrics::monotonic_time_us()
            .unwrap_or_else(|| u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX))
    }

    pub fn intern(&self, name: &str) -> Option<u16> {
        self.names.lock().ok()?.intern(name)
    }

    pub fn push(&self, observation: Observation) {
        self.ring.push(observation);
    }

    #[cfg(test)]
    pub fn pop_for_test(&self) -> Option<Observation> {
        self.ring.pop()
    }

    #[cfg(test)]
    pub fn record_for_test(&self, report: &Report, wall_ms: u64) {
        self.history.lock().unwrap().record(report, wall_ms);
    }

    /// Starts the collector the first time a producer binds a sink, so a
    /// runtime nobody traces owns no thread.
    pub fn ensure_collector(self: &Arc<Self>, broker: &Arc<Mutex<Broker>>) {
        self.collector.get_or_init(|| {
            let hub = self.clone();
            let broker = broker.clone();
            let handle = std::thread::Builder::new()
                .name("amaterasu-trace".into())
                .spawn(move || hub.collect(&broker))
                .ok();
            Mutex::new(handle)
        });
    }

    fn collect(&self, broker: &Mutex<Broker>) {
        let mut collector = Collector::default();
        let resolver = HubResolver { hub: self };
        loop {
            let stopping = self.stop.load(Ordering::Acquire);
            let mut drained = 0_u32;
            while let Some(observation) = self.ring.pop() {
                collector.observe(observation, &resolver);
                drained += 1;
            }
            let reports = collector.poll(self.now_us(), self.ring.take_dropped(), &resolver);
            self.publish(reports, broker);
            if stopping {
                return;
            }
            std::thread::park_timeout(if drained == 0 { IDLE_PAUSE } else { BUSY_PAUSE });
        }
    }

    fn publish(&self, reports: Vec<Report>, broker: &Mutex<Broker>) {
        if reports.is_empty() {
            return;
        }
        let wall_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            });
        if let Ok(mut history) = self.history.lock() {
            for report in &reports {
                history.record(report, wall_ms);
            }
        }
        // The broker lock is taken once per batch of reports, never per record.
        if let Ok(mut broker) = broker.lock() {
            for report in reports {
                let notable = report::is_notable(report.target, &report.body);
                broker.record_diagnostic(notable, report.target, report.body.to_string());
            }
        }
    }

    /// The performance report for a management surface: the last minute as
    /// one summary, a half-hour trend and the recent slow moments, within
    /// `report::REPORT_BYTE_BUDGET`.
    pub fn snapshot(&self) -> Value {
        let collecting = self.collector.get().is_some();
        // Build the report outside the lock the collector publishes under.
        let history = self
            .history
            .lock()
            .map(|history| history.clone())
            .unwrap_or_default();
        report::bounded(collecting, &history.windows, history.trend, history.slow)
    }

    /// Stops and joins the collector. Sinks must no longer be in use.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        let Some(collector) = self.collector.get() else {
            return;
        };
        let handle = collector.lock().ok().and_then(|mut handle| handle.take());
        if let Some(handle) = handle {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}
