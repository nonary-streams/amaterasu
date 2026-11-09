//! Owns the queue, the name table and the collector thread.
use super::collector::{Collector, Report, Resolver};
use super::names::Names;
use super::ring::{Observation, Ring};
use crate::Broker;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const RING_CAPACITY: usize = 16_384;
/// The collector sleeps this long after finding work, and longer when idle.
const BUSY_PAUSE: Duration = Duration::from_millis(5);
const IDLE_PAUSE: Duration = Duration::from_millis(50);
const RETAINED_WINDOWS: usize = 30;
const RETAINED_SLOW_FLOWS: usize = 60;
const RETAINED_SLOW_SPANS: usize = 60;

/// What a management surface may show: recent windows and recent slow items.
#[derive(Default)]
struct History {
    windows: VecDeque<Value>,
    slow_flows: VecDeque<Value>,
    slow_spans: VecDeque<Value>,
}

fn retain(history: &mut VecDeque<Value>, limit: usize, mut item: Value, wall_ms: u64) {
    item["observed_unix_ms"] = wall_ms.into();
    if history.len() >= limit {
        history.pop_front();
    }
    history.push_back(item);
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
                match report.target {
                    "trace.window" => retain(
                        &mut history.windows,
                        RETAINED_WINDOWS,
                        report.body.clone(),
                        wall_ms,
                    ),
                    "trace.slow_flow" => retain(
                        &mut history.slow_flows,
                        RETAINED_SLOW_FLOWS,
                        report.body.clone(),
                        wall_ms,
                    ),
                    _ => retain(
                        &mut history.slow_spans,
                        RETAINED_SLOW_SPANS,
                        report.body.clone(),
                        wall_ms,
                    ),
                }
            }
        }
        // The broker lock is taken once per batch of reports, never per record.
        if let Ok(mut broker) = broker.lock() {
            for report in reports {
                broker.record_diagnostic(report.target, report.body.to_string());
            }
        }
    }

    /// Bounded recent history for a management surface. Oldest first.
    pub fn snapshot(&self) -> Value {
        let Ok(history) = self.history.lock() else {
            return json!({"schema": 1, "windows": [], "slow_flows": [], "slow_spans": []});
        };
        json!({
            "schema": 1,
            "collecting": self.collector.get().is_some(),
            "windows": history.windows,
            "slow_flows": history.slow_flows,
            "slow_spans": history.slow_spans,
        })
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
