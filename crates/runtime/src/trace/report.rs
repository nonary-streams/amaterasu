//! The compact performance report a management surface reads.
//!
//! The collector's own reports are written for the log: one per window, with
//! every step and span. A management response is far smaller than a minute of
//! those, so this module reduces the retained history to what a person reads:
//! the last minute merged into one summary, a half-hour trend as plain number
//! columns, and the most recent slow moments.
use super::collector::SLOW_SPAN_US;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, VecDeque};

/// A report is one management response. It stays below the broker payload
/// limit and well below the management HTTP response limit.
pub(super) const REPORT_BYTE_BUDGET: usize = 14 * 1024;
const LISTED_TRACKS: usize = 6;
const LISTED_STEPS: usize = 8;
const LISTED_SPANS: usize = 16;
const TREND_TRACKS: usize = 4;
/// Slow flows that share a cause within this time are one moment.
const SAME_MOMENT_MS: u64 = 1_000;
/// The host's own bookkeeping spans. They are summarized, never listed as the
/// reason something was slow: a pass is always running.
const HOST_PASS: &str = "host.pass";
const HOST_WAIT: &str = "host.wait";

fn count(value: &Value) -> u64 {
    value.as_u64().unwrap_or(0)
}

/// Whether a collected report belongs in the default log. A window is written
/// every ten seconds whatever happened, and a flow or span only just over its
/// limit is routine on a busy host; both are debug records. A flow or span at
/// least twice over its limit is a pause a person would notice.
pub(super) fn is_notable(target: &str, body: &Value) -> bool {
    match target {
        "trace.slow_flow" => {
            count(&body["total_us"]) >= count(&body["budget_us"]).saturating_mul(2)
        }
        "trace.slow_span" => count(&body["us"]) >= SLOW_SPAN_US * 2,
        _ => false,
    }
}

/// Calls, total and worst of one stage, merged across windows. The 95th
/// percentile of a merged stage is the highest any window reported, so it
/// remains an upper bound.
#[derive(Clone, Default)]
struct Merged {
    calls: u64,
    total_us: u64,
    max_us: u64,
    p95_upper_us: u64,
}

impl Merged {
    fn add(&mut self, metric: &Value) {
        self.calls = self.calls.saturating_add(count(&metric["calls"]));
        self.total_us = self.total_us.saturating_add(count(&metric["total_us"]));
        self.max_us = self.max_us.max(count(&metric["max_us"]));
        self.p95_upper_us = self.p95_upper_us.max(count(&metric["p95_upper_us"]));
    }

    fn to_json(&self, stage: &str) -> Value {
        json!({
            "stage": stage,
            "calls": self.calls,
            "mean_us": self.total_us.checked_div(self.calls),
            "p95_upper_us": self.p95_upper_us,
            "max_us": self.max_us,
            "total_us": self.total_us,
        })
    }
}

#[derive(Default)]
struct MergedTrack {
    completed: u64,
    slow: u64,
    abandoned: u64,
    budget_us: u64,
    total: Merged,
    steps: BTreeMap<String, Merged>,
}

fn items(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

fn merged_tracks(windows: &VecDeque<Value>) -> Vec<Value> {
    let mut tracks: BTreeMap<String, MergedTrack> = BTreeMap::new();
    for track in windows.iter().flat_map(|window| items(&window["tracks"])) {
        let Some(name) = track["track"].as_str() else {
            continue;
        };
        let merged = tracks.entry(name.to_owned()).or_default();
        merged.completed += count(&track["completed"]);
        merged.slow += count(&track["slow"]);
        merged.abandoned += count(&track["abandoned"]);
        merged.budget_us = merged.budget_us.max(count(&track["budget_us"]));
        merged.total.add(&track["total"]);
        for step in items(&track["steps"]) {
            if let Some(stage) = step["stage"].as_str() {
                merged.steps.entry(stage.to_owned()).or_default().add(step);
            }
        }
    }
    let mut tracks: Vec<_> = tracks.into_iter().collect();
    tracks.sort_by_key(|(_, track)| std::cmp::Reverse(track.completed));
    tracks
        .into_iter()
        .take(LISTED_TRACKS)
        .map(|(name, track)| {
            let mut steps: Vec<_> = track.steps.iter().collect();
            steps.sort_by_key(|(_, metric)| std::cmp::Reverse(metric.total_us));
            json!({
                "track": name,
                "completed": track.completed,
                "slow": track.slow,
                "abandoned": track.abandoned,
                "budget_us": track.budget_us,
                "total": track.total.to_json("total"),
                "steps": steps
                    .into_iter()
                    .take(LISTED_STEPS)
                    .map(|(stage, metric)| metric.to_json(stage))
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// The host loop's own spans, and the other spans that held a thread longest.
fn merged_spans(windows: &VecDeque<Value>) -> (Value, Vec<Value>, usize) {
    let mut spans: BTreeMap<String, Merged> = BTreeMap::new();
    for span in windows.iter().flat_map(|window| items(&window["spans"])) {
        if let Some(stage) = span["stage"].as_str() {
            spans.entry(stage.to_owned()).or_default().add(span);
        }
    }
    let host = json!({
        "pass": spans.remove(HOST_PASS).map(|metric| metric.to_json(HOST_PASS)),
        "wait": spans.remove(HOST_WAIT).map(|metric| metric.to_json(HOST_WAIT)),
    });
    let mut listed: Vec<_> = spans.iter().collect();
    listed.sort_by_key(|(_, metric)| std::cmp::Reverse((metric.max_us, metric.total_us)));
    let omitted = listed.len().saturating_sub(LISTED_SPANS);
    let listed = listed
        .into_iter()
        .take(LISTED_SPANS)
        .map(|(stage, metric)| metric.to_json(stage))
        .collect();
    (host, listed, omitted)
}

/// The retained windows as one summary, or null before the first window.
pub(super) fn recent(windows: &VecDeque<Value>) -> Value {
    if windows.is_empty() {
        return Value::Null;
    }
    let sum = |field: &str| -> u64 { windows.iter().map(|window| count(&window[field])).sum() };
    let (host, spans, spans_omitted) = merged_spans(windows);
    json!({
        "window_us": sum("window_us"),
        "dropped_records": sum("dropped_records"),
        "tracks": merged_tracks(windows),
        "host": host,
        "spans": spans,
        "spans_omitted": spans_omitted,
    })
}

/// One track's flows in one window.
#[derive(Clone)]
struct TrackPoint {
    track: String,
    p95_upper_us: u64,
    max_us: u64,
    budget_us: u64,
}

/// What one window contributes to the trend: how slow each track's flows got
/// and how long the longest host pass was.
#[derive(Clone)]
pub(super) struct TrendPoint {
    observed_unix_ms: u64,
    tracks: Vec<TrackPoint>,
    host_pass_max_us: Option<u64>,
}

pub(super) fn trend_point(window: &Value, observed_unix_ms: u64) -> TrendPoint {
    TrendPoint {
        observed_unix_ms,
        tracks: items(&window["tracks"])
            .filter(|track| count(&track["completed"]) != 0)
            .filter_map(|track| {
                Some(TrackPoint {
                    track: track["track"].as_str()?.to_owned(),
                    p95_upper_us: count(&track["total"]["p95_upper_us"]),
                    max_us: count(&track["total"]["max_us"]),
                    budget_us: count(&track["budget_us"]),
                })
            })
            .collect(),
        host_pass_max_us: items(&window["spans"])
            .find(|span| span["stage"] == HOST_PASS)
            .map(|span| count(&span["max_us"])),
    }
}

/// The trend as columns of plain numbers, oldest first. A window in which a
/// track completed nothing holds null for it.
pub(super) fn trend(points: &VecDeque<TrendPoint>) -> Value {
    let Some(newest) = points.back() else {
        return Value::Null;
    };
    let mut names: Vec<&str> = points
        .iter()
        .flat_map(|point| point.tracks.iter().map(|track| track.track.as_str()))
        .collect();
    names.sort_unstable();
    names.dedup();
    let of = |point: &'_ TrendPoint, name: &str| -> Option<TrackPoint> {
        point
            .tracks
            .iter()
            .find(|track| track.track == name)
            .cloned()
    };
    // The tracks seen in the most windows; a host has two or three.
    names.sort_by_key(|name| {
        std::cmp::Reverse(
            points
                .iter()
                .filter(|point| of(point, name).is_some())
                .count(),
        )
    });
    names.truncate(TREND_TRACKS);
    let tracks: Map<String, Value> = names
        .into_iter()
        .map(|name| {
            let column = |pick: fn(&TrackPoint) -> u64| -> Vec<Option<u64>> {
                points
                    .iter()
                    .map(|point| of(point, name).as_ref().map(pick))
                    .collect()
            };
            let columns = json!({
                "budget_us": column(|track| track.budget_us).into_iter().flatten().max(),
                "p95_upper_us": column(|track| track.p95_upper_us),
                "max_us": column(|track| track.max_us),
            });
            (name.to_owned(), columns)
        })
        .collect();
    json!({
        "newest_unix_ms": newest.observed_unix_ms,
        "age_s": points
            .iter()
            .map(|point| newest.observed_unix_ms.saturating_sub(point.observed_unix_ms) / 1_000)
            .collect::<Vec<_>>(),
        "tracks": tracks,
        "host_pass_max_us": points.iter().map(|point| point.host_pass_max_us).collect::<Vec<_>>(),
    })
}

/// The span that most plausibly made a step slow: the one that overlapped it
/// longest, leaving out the host's own pass and wait.
fn held_by(slowest_step: &Value) -> Value {
    items(&slowest_step["during"])
        .filter(|span| span["span"] != HOST_PASS && span["span"] != HOST_WAIT)
        .max_by_key(|span| count(&span["overlap_us"]))
        .map_or(
            Value::Null,
            |span| json!({"span": span["span"], "label": span["label"], "us": span["us"]}),
        )
}

fn slow_flow(body: &Value, observed_unix_ms: u64) -> Value {
    let step = &body["slowest_step"];
    json!({
        "kind": "flow",
        "observed_unix_ms": observed_unix_ms,
        "count": 1,
        "track": body["track"],
        "us": body["total_us"],
        "budget_us": body["budget_us"],
        "step": format!(
            "{}>{}",
            step["from"].as_str().unwrap_or(""),
            step["to"].as_str().unwrap_or("")
        ),
        "step_us": step["us"],
        "held_by": held_by(step),
    })
}

fn slow_span(body: &Value, observed_unix_ms: u64) -> Value {
    json!({
        "kind": "span",
        "observed_unix_ms": observed_unix_ms,
        "count": 1,
        "span": body["span"],
        "label": body["label"],
        "by": body["by"],
        "us": body["us"],
    })
}

fn same_cause(left: &Value, right: &Value) -> bool {
    ["kind", "track", "step", "span", "label"]
        .into_iter()
        .all(|field| left[field] == right[field])
        && left["held_by"]["label"] == right["held_by"]["label"]
}

/// Adds one slow report to the retained moments. A burst behind one cause,
/// such as every input event that waited for the same pause, is one moment
/// that counts its members and keeps the worst of them.
pub(super) fn retain_slow(
    moments: &mut VecDeque<Value>,
    limit: usize,
    target: &str,
    body: &Value,
    observed_unix_ms: u64,
) {
    let moment = if target == "trace.slow_flow" {
        slow_flow(body, observed_unix_ms)
    } else if body["span"] == HOST_PASS || body["span"] == HOST_WAIT {
        // A long pass is already the pass maximum of its window, and the
        // plugin call that caused it is reported as its own span.
        return;
    } else {
        slow_span(body, observed_unix_ms)
    };
    if let Some(latest) = moments.back_mut()
        && same_cause(latest, &moment)
        && observed_unix_ms.saturating_sub(count(&latest["observed_unix_ms"])) <= SAME_MOMENT_MS
    {
        latest["count"] = (count(&latest["count"]) + 1).into();
        if count(&moment["us"]) > count(&latest["us"]) {
            for field in ["us", "step_us", "held_by"] {
                latest[field] = moment[field].clone();
            }
        }
        return;
    }
    if moments.len() >= limit {
        moments.pop_front();
    }
    moments.push_back(moment);
}

fn compose(collecting: bool, recent: &Value, trend: &Value, slow: &VecDeque<Value>) -> Value {
    json!({
        "schema": 2,
        "collecting": collecting,
        "recent": recent,
        "trend": trend,
        // Newest first: a truncated list keeps what just happened.
        "slow": slow.iter().rev().collect::<Vec<_>>(),
    })
}

fn listed<'a>(recent: &'a mut Value, list: &str) -> Option<&'a mut Vec<Value>> {
    recent.get_mut(list).and_then(Value::as_array_mut)
}

/// Removes the last entry of a list in the summary while it holds more than `keep`.
fn shorten(recent: &mut Value, list: &str, keep: usize) -> bool {
    listed(recent, list)
        .filter(|entries| entries.len() > keep)
        .is_some_and(|entries| entries.pop().is_some())
}

/// Removes the least costly step of the track that lists the most steps.
fn shorten_steps(recent: &mut Value, keep: usize) -> bool {
    listed(recent, "tracks")
        .into_iter()
        .flatten()
        .filter_map(|track| track.get_mut("steps").and_then(Value::as_array_mut))
        .filter(|steps| steps.len() > keep)
        .max_by_key(|steps| steps.len())
        .is_some_and(|steps| steps.pop().is_some())
}

/// Gives up one piece of detail, in the order a reader misses it least: older
/// trend points, older slow moments, the shortest thread spans, the cheapest
/// steps, the quietest tracks. The newest slow moment goes last.
fn reduce(
    recent: &mut Value,
    points: &mut VecDeque<TrendPoint>,
    slow: &mut VecDeque<Value>,
) -> bool {
    if points.len() > 30 {
        return points.pop_front().is_some();
    }
    if slow.len() > 4 {
        return slow.pop_front().is_some();
    }
    shorten(recent, "spans", 4)
        || shorten_steps(recent, 3)
        || shorten(recent, "tracks", 2)
        || points.pop_front().is_some()
        || (slow.len() > 1 && slow.pop_front().is_some())
        || shorten(recent, "spans", 0)
        || shorten_steps(recent, 0)
        || shorten(recent, "tracks", 0)
        || slow.pop_front().is_some()
}

/// The report, reduced until it fits its budget.
pub(super) fn bounded(
    collecting: bool,
    windows: &VecDeque<Value>,
    mut points: VecDeque<TrendPoint>,
    mut slow: VecDeque<Value>,
) -> Value {
    let mut recent = recent(windows);
    loop {
        let report = compose(collecting, &recent, &trend(&points), &slow);
        if report.to_string().len() <= REPORT_BYTE_BUDGET
            || !reduce(&mut recent, &mut points, &mut slow)
        {
            return report;
        }
    }
}
