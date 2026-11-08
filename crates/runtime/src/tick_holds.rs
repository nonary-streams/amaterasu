//! Attributes time on the host's module thread to the module that held it.
//!
//! The host ticks every module in turn on one thread, so a module that blocks
//! delays video delivery for all of them. Each module's broker calls mark its
//! turn, which identifies the holder without changing the host or any module.
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt::Write,
    time::{Duration, Instant},
};

const REPORT_INTERVAL: Duration = Duration::from_secs(10);
/// Long enough to be visible in a 120 Hz frame budget.
const STALL_THRESHOLD: Duration = Duration::from_millis(2);
const REPORTED_HOLDERS: usize = 8;
/// Host work between waking and the first module's first broker call.
const PASS_START_HOLDER: usize = 0;

#[derive(Clone, Copy, Default)]
struct Durations {
    count: u64,
    total: Duration,
    max: Duration,
}

impl Durations {
    fn record(&mut self, duration: Duration) {
        self.count = self.count.saturating_add(1);
        self.total = self.total.saturating_add(duration);
        self.max = self.max.max(duration);
    }

    fn average_us(&self) -> u128 {
        self.total.as_micros() / u128::from(self.count.max(1))
    }
}

struct Holder {
    principal: String,
    held: Durations,
    stalls: u64,
}

struct Tracker {
    window_started: Instant,
    holders: BTreeMap<usize, Holder>,
    passes: Durations,
    broker_calls: Durations,
    pass_started: Option<Instant>,
    current_holder: Option<usize>,
    released_at: Option<Instant>,
    call_started: Option<Instant>,
}

impl Tracker {
    fn new(now: Instant) -> Self {
        Self {
            window_started: now,
            holders: BTreeMap::new(),
            passes: Durations::default(),
            broker_calls: Durations::default(),
            pass_started: None,
            current_holder: None,
            released_at: None,
            call_started: None,
        }
    }

    fn charge(&mut self, holder: usize, principal: &str, duration: Duration) {
        let entry = self.holders.entry(holder).or_insert_with(|| Holder {
            principal: principal.to_owned(),
            held: Durations::default(),
            stalls: 0,
        });
        entry.held.record(duration);
        entry.stalls = entry
            .stalls
            .saturating_add(u64::from(duration >= STALL_THRESHOLD));
    }

    fn charge_gap_ending_at(&mut self, now: Instant) {
        if let (Some(holder), Some(released)) = (self.current_holder, self.released_at) {
            let gap = now.saturating_duration_since(released);
            self.charge(holder, "", gap);
        } else if let Some(started) = self.pass_started {
            let gap = now.saturating_duration_since(started);
            self.charge(PASS_START_HOLDER, "host.pass_start", gap);
        }
    }

    fn enter_call(&mut self, key: usize, principal: &str, now: Instant) {
        self.charge_gap_ending_at(now);
        self.holders.entry(key).or_insert_with(|| Holder {
            principal: principal.to_owned(),
            held: Durations::default(),
            stalls: 0,
        });
        self.current_holder = Some(key);
        self.released_at = None;
        self.call_started = Some(now);
    }

    fn exit_call(&mut self, now: Instant) {
        if let Some(started) = self.call_started.take() {
            self.broker_calls
                .record(now.saturating_duration_since(started));
        }
        self.released_at = Some(now);
    }

    fn begin_wait(&mut self, now: Instant) -> Option<String> {
        self.charge_gap_ending_at(now);
        if let Some(started) = self.pass_started.take() {
            self.passes.record(now.saturating_duration_since(started));
        }
        self.current_holder = None;
        self.released_at = None;
        let elapsed = now.saturating_duration_since(self.window_started);
        if elapsed < REPORT_INTERVAL {
            return None;
        }
        let report = self.report(elapsed);
        self.window_started = now;
        self.holders.clear();
        self.passes = Durations::default();
        self.broker_calls = Durations::default();
        Some(report)
    }

    fn end_wait(&mut self, now: Instant) {
        self.pass_started = Some(now);
    }

    /// The holders that could explain a stall, slowest single hold first.
    fn report(&self, elapsed: Duration) -> String {
        let mut holders: Vec<&Holder> = self
            .holders
            .values()
            .filter(|holder| holder.held.count > 0)
            .collect();
        holders.sort_by_key(|holder| std::cmp::Reverse(holder.held.max));
        let mut report = format!(
            "window_ms={} passes={} pass_avg_us={} pass_max_us={} broker_calls={} broker_avg_us={} broker_max_us={} slowest_holders=",
            elapsed.as_millis(),
            self.passes.count,
            self.passes.average_us(),
            self.passes.max.as_micros(),
            self.broker_calls.count,
            self.broker_calls.average_us(),
            self.broker_calls.max.as_micros(),
        );
        for holder in holders.into_iter().take(REPORTED_HOLDERS) {
            let _ = write!(
                report,
                "{}[max_us={},total_us={},stalls={}] ",
                holder.principal,
                holder.held.max.as_micros(),
                holder.held.total.as_micros(),
                holder.stalls,
            );
        }
        report.truncate(report.trim_end().len());
        report
    }
}

thread_local! {
    /// Present only on the thread that waits for module work: the module thread.
    static TRACKER: RefCell<Option<Tracker>> = const { RefCell::new(None) };
}

/// Marks the end of a broker call on the module thread when dropped.
pub(crate) struct CallBoundary {
    tracked: bool,
}

impl Drop for CallBoundary {
    fn drop(&mut self) {
        if !self.tracked {
            return;
        }
        let now = Instant::now();
        TRACKER.with_borrow_mut(|tracker| {
            if let Some(tracker) = tracker {
                tracker.exit_call(now);
            }
        });
    }
}

/// Records the start of a broker call. Other threads pay only the lookup.
pub(crate) fn enter_call(key: usize, principal: &str) -> CallBoundary {
    let tracked = TRACKER.with_borrow_mut(|tracker| match tracker {
        Some(tracker) => {
            tracker.enter_call(key, principal, Instant::now());
            true
        }
        None => false,
    });
    CallBoundary { tracked }
}

/// The module thread finished a pass. Returns a report when one is due.
pub(crate) fn begin_wait() -> Option<String> {
    let now = Instant::now();
    TRACKER.with_borrow_mut(|tracker| {
        tracker
            .get_or_insert_with(|| Tracker::new(now))
            .begin_wait(now)
    })
}

/// The module thread woke and is starting another pass.
pub(crate) fn end_wait() {
    let now = Instant::now();
    TRACKER.with_borrow_mut(|tracker| {
        if let Some(tracker) = tracker {
            tracker.end_wait(now);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: usize = 1;
    const SECOND: usize = 2;

    fn at(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn a_module_blocking_between_broker_calls_is_named_as_the_holder() {
        let start = Instant::now();
        let mut tracker = Tracker::new(start);
        tracker.end_wait(at(start, 0));
        tracker.enter_call(FIRST, "first", at(start, 0));
        tracker.exit_call(at(start, 0));
        // `first` keeps the thread for 40 ms after its call returns.
        tracker.enter_call(SECOND, "second", at(start, 40));
        tracker.exit_call(at(start, 40));
        tracker.begin_wait(at(start, 41));

        let report = tracker.report(Duration::from_secs(10));
        let first = report.find("first[max_us=40000").expect(&report);
        let second = report.find("second[max_us=1000").expect(&report);
        assert!(
            first < second,
            "the slowest holder is listed first: {report}"
        );
        assert!(report.contains("stalls=1"), "{report}");
    }

    #[test]
    fn idle_time_between_passes_is_not_charged_to_any_module() {
        let start = Instant::now();
        let mut tracker = Tracker::new(start);
        tracker.end_wait(at(start, 0));
        tracker.enter_call(FIRST, "first", at(start, 0));
        tracker.exit_call(at(start, 0));
        tracker.begin_wait(at(start, 1));
        // The thread sleeps for 500 ms waiting for work.
        tracker.end_wait(at(start, 501));
        tracker.enter_call(FIRST, "first", at(start, 501));
        tracker.exit_call(at(start, 501));
        tracker.begin_wait(at(start, 502));

        let holder = &tracker.holders[&FIRST];
        assert_eq!(holder.held.max, Duration::from_millis(1));
        assert_eq!(holder.stalls, 0);
    }

    #[test]
    fn host_work_before_the_first_module_is_reported_separately() {
        let start = Instant::now();
        let mut tracker = Tracker::new(start);
        tracker.end_wait(at(start, 0));
        tracker.enter_call(FIRST, "first", at(start, 7));
        tracker.exit_call(at(start, 7));

        assert_eq!(
            tracker.holders[&PASS_START_HOLDER].held.max,
            Duration::from_millis(7)
        );
    }

    #[test]
    fn the_report_appears_once_per_interval_and_restarts_the_window() {
        let start = Instant::now();
        let mut tracker = Tracker::new(start);
        tracker.end_wait(at(start, 0));
        tracker.enter_call(FIRST, "first", at(start, 0));
        tracker.exit_call(at(start, 1));
        assert!(tracker.begin_wait(at(start, 5)).is_none());

        tracker.end_wait(at(start, 10_000));
        tracker.enter_call(FIRST, "first", at(start, 10_000));
        tracker.exit_call(at(start, 10_001));
        let report = tracker
            .begin_wait(at(start, 10_005))
            .expect("report is due");

        assert!(report.starts_with("window_ms=10005 passes=2"), "{report}");
        assert!(tracker.holders.is_empty());
        assert_eq!(tracker.passes.count, 0);
    }

    #[test]
    fn threads_that_never_wait_are_not_tracked() {
        assert!(!enter_call(FIRST, "worker").tracked);
    }
}
