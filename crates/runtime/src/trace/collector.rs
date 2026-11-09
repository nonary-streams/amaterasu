//! Turns observations into per-step statistics and slow-flow reports.
//!
//! This runs on the collector thread only. Producers pay for one queue push;
//! every map, sort and string here is off their path.
use super::ring::Observation;
use amaterasu_abi::{TRACE_END, TRACE_MARK, TRACE_SPAN};
use amaterasu_timing_metrics::LatencyMetric;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};

/// A flow that ends without its own budget is slow after half a 60 Hz frame.
pub(crate) const DEFAULT_FLOW_BUDGET_US: u64 = 8_000;
/// Spans at least this long are reported individually.
pub(crate) const SLOW_SPAN_US: u64 = 8_000;
/// Shorter spans cannot explain a slow step and are kept only as statistics.
const OVERLAP_SPAN_MIN_US: u64 = 200;
const MAX_OPEN_FLOWS: usize = 2_048;
const MAX_MARKS: usize = 12;
const FLOW_TIMEOUT_US: u64 = 5_000_000;
const EXPIRY_INTERVAL_US: u64 = 1_000_000;
const RECENT_SPANS: usize = 512;
const WINDOW_US: u64 = 10_000_000;
/// A span that covers a flow's last step is recorded when that span ends,
/// which is after the flow's final mark. Wait for it before explaining.
const REPORT_DELAY_US: u64 = 30_000;
const MAX_TRACKS: usize = 32;
const MAX_STEPS_PER_TRACK: usize = 48;
const MAX_SPAN_KINDS: usize = 256;
const MAX_PENDING_SLOW_FLOWS: usize = 64;
const SLOW_FLOW_REPORTS_PER_WINDOW: u64 = 20;
const SLOW_SPAN_REPORTS_PER_WINDOW: u64 = 20;
const REPORTED_SPANS_PER_WINDOW: usize = 16;
const REPORTED_OVERLAPS: usize = 4;

pub(crate) trait Resolver {
    fn name(&self, identifier: u16) -> String;
    fn source(&self, identifier: u16) -> String;
}

#[derive(Debug, PartialEq)]
pub(crate) struct Report {
    pub target: &'static str,
    pub body: Value,
}

#[derive(Clone, Copy, Default)]
struct Mark {
    stage: u16,
    source: u16,
    at_us: u64,
    value: u64,
}

struct Flow {
    marks: [Mark; MAX_MARKS],
    count: usize,
    truncated: bool,
    started_us: u64,
}

impl Flow {
    fn new(started_us: u64) -> Self {
        Self {
            marks: [Mark::default(); MAX_MARKS],
            count: 0,
            truncated: false,
            started_us,
        }
    }

    /// A flow with too many stages keeps its first marks and its latest one,
    /// so the total and the final stage stay correct.
    fn push(&mut self, mark: Mark) {
        if self.count < MAX_MARKS {
            self.marks[self.count] = mark;
            self.count += 1;
        } else {
            self.marks[MAX_MARKS - 1] = mark;
            self.truncated = true;
        }
    }

    fn ordered(&self) -> Vec<Mark> {
        let mut marks = self.marks[..self.count].to_vec();
        marks.sort_by_key(|mark| mark.at_us);
        marks
    }
}

#[derive(Default)]
struct TrackStats {
    completed: u64,
    slow: u64,
    abandoned: u64,
    truncated: u64,
    total: LatencyMetric,
    steps: BTreeMap<(u16, u16), LatencyMetric>,
    unrecorded_steps: u64,
    slow_reported: u64,
    slow_suppressed: u64,
    worst_suppressed_us: u64,
}

#[derive(Clone, Copy)]
struct SpanSample {
    name: u16,
    label: u16,
    source: u16,
    end_us: u64,
    duration_us: u64,
}

impl SpanSample {
    fn overlap_us(&self, from_us: u64, to_us: u64) -> u64 {
        let start_us = self.end_us.saturating_sub(self.duration_us);
        self.end_us.min(to_us).saturating_sub(start_us.max(from_us))
    }
}

struct SlowFlow {
    track: u16,
    flow: u64,
    marks: Vec<Mark>,
    truncated: bool,
    budget_us: u64,
    ready_at_us: u64,
}

#[derive(Default)]
pub(crate) struct Collector {
    flows: HashMap<(u16, u64), Flow>,
    tracks: BTreeMap<u16, TrackStats>,
    spans: BTreeMap<(u16, u16), LatencyMetric>,
    recent_spans: VecDeque<SpanSample>,
    pending_slow_flows: VecDeque<SlowFlow>,
    pending_reports: Vec<Report>,
    window_started_us: Option<u64>,
    last_expiry_us: u64,
    records: u64,
    dropped_in_window: u64,
    ignored_records: u64,
    refused_flows: u64,
    untracked_spans: u64,
    slow_spans_reported: u64,
    slow_spans_suppressed: u64,
}

impl Collector {
    pub fn observe(&mut self, observation: Observation, resolver: &dyn Resolver) {
        self.records = self.records.saturating_add(1);
        self.window_started_us
            .get_or_insert(observation.timestamp_us);
        if observation.track == 0 {
            self.ignored_records = self.ignored_records.saturating_add(1);
            return;
        }
        match observation.kind {
            TRACE_MARK | TRACE_END if observation.stage != 0 => self.observe_mark(observation),
            TRACE_SPAN => self.observe_span(observation, resolver),
            _ => self.ignored_records = self.ignored_records.saturating_add(1),
        }
    }

    fn observe_mark(&mut self, observation: Observation) {
        let key = (observation.track, observation.flow);
        if !self.flows.contains_key(&key) {
            if self.flows.len() >= MAX_OPEN_FLOWS {
                self.expire(observation.timestamp_us);
            }
            if self.flows.len() >= MAX_OPEN_FLOWS
                || (!self.tracks.contains_key(&observation.track)
                    && self.tracks.len() >= MAX_TRACKS)
            {
                self.refused_flows = self.refused_flows.saturating_add(1);
                return;
            }
            self.tracks.entry(observation.track).or_default();
            self.flows.insert(key, Flow::new(observation.timestamp_us));
        }
        let flow = self.flows.get_mut(&key).expect("flow was just ensured");
        flow.push(Mark {
            stage: observation.stage,
            source: observation.source,
            at_us: observation.timestamp_us,
            value: observation.value,
        });
        if observation.kind == TRACE_END {
            let flow = self.flows.remove(&key).expect("flow is present");
            let budget_us = if observation.value == 0 {
                DEFAULT_FLOW_BUDGET_US
            } else {
                observation.value
            };
            self.complete(key.0, key.1, &flow, budget_us);
        }
    }

    fn complete(&mut self, track: u16, flow_id: u64, flow: &Flow, budget_us: u64) {
        let marks = flow.ordered();
        let (Some(first), Some(last)) = (marks.first(), marks.last()) else {
            return;
        };
        let total_us = last.at_us.saturating_sub(first.at_us);
        let stats = self.tracks.entry(track).or_default();
        stats.completed = stats.completed.saturating_add(1);
        stats.truncated = stats.truncated.saturating_add(u64::from(flow.truncated));
        stats.total.record(total_us);
        for pair in marks.windows(2) {
            let step = (pair[0].stage, pair[1].stage);
            if stats.steps.len() >= MAX_STEPS_PER_TRACK && !stats.steps.contains_key(&step) {
                stats.unrecorded_steps = stats.unrecorded_steps.saturating_add(1);
                continue;
            }
            stats
                .steps
                .entry(step)
                .or_default()
                .record(pair[1].at_us.saturating_sub(pair[0].at_us));
        }
        if total_us < budget_us {
            return;
        }
        stats.slow = stats.slow.saturating_add(1);
        if stats.slow_reported >= SLOW_FLOW_REPORTS_PER_WINDOW
            || self.pending_slow_flows.len() >= MAX_PENDING_SLOW_FLOWS
        {
            stats.slow_suppressed = stats.slow_suppressed.saturating_add(1);
            stats.worst_suppressed_us = stats.worst_suppressed_us.max(total_us);
            return;
        }
        stats.slow_reported += 1;
        self.pending_slow_flows.push_back(SlowFlow {
            track,
            flow: flow_id,
            ready_at_us: last.at_us.saturating_add(REPORT_DELAY_US),
            marks,
            truncated: flow.truncated,
            budget_us,
        });
    }

    fn observe_span(&mut self, observation: Observation, resolver: &dyn Resolver) {
        let key = (observation.track, observation.stage);
        if self.spans.len() >= MAX_SPAN_KINDS && !self.spans.contains_key(&key) {
            self.untracked_spans = self.untracked_spans.saturating_add(1);
        } else {
            self.spans.entry(key).or_default().record(observation.value);
        }
        if observation.value < OVERLAP_SPAN_MIN_US {
            return;
        }
        let sample = SpanSample {
            name: observation.track,
            label: observation.stage,
            source: observation.source,
            end_us: observation.timestamp_us,
            duration_us: observation.value,
        };
        if self.recent_spans.len() >= RECENT_SPANS {
            self.recent_spans.pop_front();
        }
        self.recent_spans.push_back(sample);
        if observation.value < SLOW_SPAN_US {
            return;
        }
        if self.slow_spans_reported >= SLOW_SPAN_REPORTS_PER_WINDOW {
            self.slow_spans_suppressed = self.slow_spans_suppressed.saturating_add(1);
            return;
        }
        self.slow_spans_reported += 1;
        self.pending_reports.push(Report {
            target: "trace.slow_span",
            body: json!({
                "event": "slow_span",
                "span": resolver.name(sample.name),
                "label": (sample.label != 0).then(|| resolver.name(sample.label)),
                "us": sample.duration_us,
                "ended_us": sample.end_us,
                "by": resolver.source(sample.source),
                "correlation": (observation.flow != 0).then(|| format!("{:#x}", observation.flow)),
                "scope": "one_thread_was_occupied_for_this_long",
            }),
        });
    }

    /// Flows that never reached their final stage would otherwise pin the
    /// table. They are counted per track, not reported one by one.
    fn expire(&mut self, now_us: u64) {
        self.last_expiry_us = now_us;
        let tracks = &mut self.tracks;
        self.flows.retain(|(track, _), flow| {
            let expired = now_us.saturating_sub(flow.started_us) >= FLOW_TIMEOUT_US;
            if expired {
                let stats = tracks.entry(*track).or_default();
                stats.abandoned = stats.abandoned.saturating_add(1);
            }
            !expired
        });
    }

    /// Reports that are due at `now_us`. `dropped_records` is the queue's loss
    /// since the previous call.
    pub fn poll(
        &mut self,
        now_us: u64,
        dropped_records: u64,
        resolver: &dyn Resolver,
    ) -> Vec<Report> {
        let mut reports = std::mem::take(&mut self.pending_reports);
        while self
            .pending_slow_flows
            .front()
            .is_some_and(|flow| flow.ready_at_us <= now_us)
        {
            let flow = self.pending_slow_flows.pop_front().expect("front exists");
            reports.push(self.slow_flow_report(&flow, resolver));
        }
        if now_us.saturating_sub(self.last_expiry_us) >= EXPIRY_INTERVAL_US {
            self.expire(now_us);
        }
        self.dropped_in_window = self.dropped_in_window.saturating_add(dropped_records);
        let window_started_us = *self.window_started_us.get_or_insert(now_us);
        if now_us.saturating_sub(window_started_us) >= WINDOW_US {
            if self.records != 0 || self.dropped_in_window != 0 {
                reports.push(self.window_report(now_us - window_started_us, resolver));
            }
            self.window_started_us = Some(now_us);
        }
        reports
    }

    fn slow_flow_report(&self, flow: &SlowFlow, resolver: &dyn Resolver) -> Report {
        let first = flow.marks.first().expect("completed flows have a mark");
        let last = flow.marks.last().expect("completed flows have a mark");
        let total_us = last.at_us.saturating_sub(first.at_us);
        let mut steps = Vec::with_capacity(flow.marks.len());
        let mut slowest: Option<(&Mark, &Mark)> = None;
        for (index, pair) in flow.marks.windows(2).enumerate() {
            let step_us = pair[1].at_us.saturating_sub(pair[0].at_us);
            // The final mark's value is the budget, not a stage detail.
            let is_final = index + 2 == flow.marks.len();
            steps.push(json!({
                "from": resolver.name(pair[0].stage),
                "to": resolver.name(pair[1].stage),
                "us": step_us,
                "by": resolver.source(pair[1].source),
                "detail": (!is_final && pair[1].value != 0).then_some(pair[1].value),
            }));
            if slowest.is_none_or(|(from, to)| step_us > to.at_us.saturating_sub(from.at_us)) {
                slowest = Some((&pair[0], &pair[1]));
            }
        }
        let slowest = slowest.map(|(from, to)| {
            let step_us = to.at_us.saturating_sub(from.at_us);
            json!({
                "from": resolver.name(from.stage),
                "to": resolver.name(to.stage),
                "us": step_us,
                "percent": (step_us.saturating_mul(100)).checked_div(total_us).unwrap_or(0),
                "by": resolver.source(to.source),
                "during": self.overlapping_spans(from.at_us, to.at_us, resolver),
            })
        });
        Report {
            target: "trace.slow_flow",
            body: json!({
                "event": "slow_flow",
                "track": resolver.name(flow.track),
                "flow": format!("{:#x}", flow.flow),
                "total_us": total_us,
                "budget_us": flow.budget_us,
                "started_us": first.at_us,
                "begun_by": resolver.source(first.source),
                "first_stage": resolver.name(first.stage),
                "steps": steps,
                "slowest_step": slowest,
                "stages_truncated": flow.truncated,
                "scope": "host_stages_on_one_clock_not_client_network_or_display",
            }),
        }
    }

    /// The spans that occupied a thread during `[from_us, to_us]`, longest
    /// overlap first. This is what names the module that held the host thread.
    fn overlapping_spans(&self, from_us: u64, to_us: u64, resolver: &dyn Resolver) -> Vec<Value> {
        let mut overlaps: Vec<(u64, &SpanSample)> = self
            .recent_spans
            .iter()
            .filter_map(|span| {
                let overlap_us = span.overlap_us(from_us, to_us);
                (overlap_us >= OVERLAP_SPAN_MIN_US).then_some((overlap_us, span))
            })
            .collect();
        overlaps.sort_by_key(|(overlap_us, _)| std::cmp::Reverse(*overlap_us));
        overlaps
            .into_iter()
            .take(REPORTED_OVERLAPS)
            .map(|(overlap_us, span)| {
                json!({
                    "span": resolver.name(span.name),
                    "label": (span.label != 0).then(|| resolver.name(span.label)),
                    "us": span.duration_us,
                    "overlap_us": overlap_us,
                })
            })
            .collect()
    }

    fn window_report(&mut self, window_us: u64, resolver: &dyn Resolver) -> Report {
        let tracks: Vec<Value> = std::mem::take(&mut self.tracks)
            .into_iter()
            .filter(|(_, stats)| stats.completed != 0 || stats.abandoned != 0)
            .map(|(track, stats)| {
                let mut steps: Vec<_> = stats.steps.iter().collect();
                steps.sort_by_key(|(_, metric)| std::cmp::Reverse(metric.total_us));
                json!({
                    "track": resolver.name(track),
                    "completed": stats.completed,
                    "slow": stats.slow,
                    "slow_reports_suppressed": stats.slow_suppressed,
                    "worst_suppressed_us": (stats.slow_suppressed != 0).then_some(stats.worst_suppressed_us),
                    "abandoned": stats.abandoned,
                    "stages_truncated": stats.truncated,
                    "unrecorded_steps": stats.unrecorded_steps,
                    "total": summary("total", &stats.total),
                    "steps": steps
                        .into_iter()
                        .map(|((from, to), metric)| {
                            summary(&format!("{}>{}", resolver.name(*from), resolver.name(*to)), metric)
                        })
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        // Open flows keep their track entry so a slow one is still counted.
        for (track, _) in self.flows.keys() {
            self.tracks.entry(*track).or_default();
        }
        let mut spans: Vec<_> = std::mem::take(&mut self.spans).into_iter().collect();
        spans.sort_by_key(|(_, metric)| std::cmp::Reverse(metric.total_us));
        let unlisted_spans = spans.len().saturating_sub(REPORTED_SPANS_PER_WINDOW);
        let spans: Vec<Value> = spans
            .into_iter()
            .take(REPORTED_SPANS_PER_WINDOW)
            .map(|((name, label), metric)| {
                let stage = if label == 0 {
                    resolver.name(name)
                } else {
                    format!("{}:{}", resolver.name(name), resolver.name(label))
                };
                summary(&stage, &metric)
            })
            .collect();
        let report = json!({
            "event": "trace_window",
            "window_us": window_us,
            "records": std::mem::take(&mut self.records),
            "dropped_records": std::mem::take(&mut self.dropped_in_window),
            "ignored_records": std::mem::take(&mut self.ignored_records),
            "refused_flows": std::mem::take(&mut self.refused_flows),
            "open_flows": self.flows.len(),
            "tracks": tracks,
            "spans": spans,
            "unlisted_spans": unlisted_spans,
            "untracked_spans": std::mem::take(&mut self.untracked_spans),
            "slow_span_reports_suppressed": std::mem::take(&mut self.slow_spans_suppressed),
            "scope": "per_step_time_between_consecutive_stages_and_thread_spans_percentiles_are_histogram_upper_bounds",
        });
        self.slow_spans_reported = 0;
        Report {
            target: "trace.window",
            body: report,
        }
    }
}

/// Compact form of a metric; `stage` keeps the name attached when a large
/// window is split across log records.
fn summary(stage: &str, metric: &LatencyMetric) -> Value {
    json!({
        "stage": stage,
        "calls": metric.calls,
        "total_us": metric.total_us,
        "max_us": metric.max_us,
        "mean_us": (metric.calls != 0).then(|| metric.total_us / metric.calls),
        "p50_upper_us": metric.percentile_upper_us(50),
        "p95_upper_us": metric.percentile_upper_us(95),
        "p99_upper_us": metric.percentile_upper_us(99),
    })
}
