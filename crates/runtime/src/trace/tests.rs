use super::collector::{Collector, DEFAULT_FLOW_BUDGET_US, Report, Resolver, SLOW_SPAN_US};
use super::hub::TraceHub;
use super::ring::Observation;
use super::sink::Binding;
use amaterasu_abi::{TRACE_DISCARD, TRACE_END, TRACE_MARK, TRACE_SPAN, TraceRecord, TraceSink};
use std::sync::Arc;

const INPUT: u16 = 1;
const ARRIVED: u16 = 2;
const DEQUEUED: u16 = 3;
const INJECTED: u16 = 4;
const HOST_TICK: u16 = 5;
const STATE_MODULE: u16 = 6;

struct Names;
impl Resolver for Names {
    fn name(&self, identifier: u16) -> String {
        match identifier {
            INPUT => "input",
            ARRIVED => "socket.arrived",
            DEQUEUED => "session.dequeued",
            INJECTED => "input.accepted",
            HOST_TICK => "host.tick",
            STATE_MODULE => "amaterasu.state",
            _ => "unknown",
        }
        .into()
    }
    fn source(&self, identifier: u16) -> String {
        format!("source-{identifier}")
    }
}

fn mark(flow: u64, stage: u16, at_us: u64) -> Observation {
    Observation {
        flow,
        value: 0,
        timestamp_us: at_us,
        track: INPUT,
        stage,
        kind: TRACE_MARK,
        source: 1,
    }
}

fn end(flow: u64, stage: u16, at_us: u64, budget_us: u64) -> Observation {
    Observation {
        kind: TRACE_END,
        value: budget_us,
        source: 2,
        ..mark(flow, stage, at_us)
    }
}

fn span(name: u16, label: u16, end_us: u64, duration_us: u64) -> Observation {
    Observation {
        flow: 0,
        value: duration_us,
        timestamp_us: end_us,
        track: name,
        stage: label,
        kind: TRACE_SPAN,
        source: 3,
    }
}

fn only(reports: &[Report], target: &str) -> serde_json::Value {
    let matching: Vec<_> = reports.iter().filter(|r| r.target == target).collect();
    assert_eq!(matching.len(), 1, "{reports:?}");
    matching[0].body.clone()
}

#[test]
fn a_slow_flow_names_its_slowest_step_and_the_span_that_held_the_thread() {
    let mut collector = Collector::default();
    collector.observe(mark(7, ARRIVED, 1_000), &Names);
    // The state module held the host thread from 1.2 ms to 20.8 ms.
    collector.observe(mark(7, DEQUEUED, 21_000), &Names);
    collector.observe(end(7, INJECTED, 21_300, 0), &Names);
    collector.observe(span(HOST_TICK, STATE_MODULE, 20_800, 19_600), &Names);

    // The span arrives after the flow's last mark, so the flow's report waits
    // for it. The long span itself is reported at once.
    let early = collector.poll(21_400, 0, &Names);
    only(&early, "trace.slow_span");
    assert!(
        early
            .iter()
            .all(|report| report.target != "trace.slow_flow")
    );
    let report = only(&collector.poll(60_000, 0, &Names), "trace.slow_flow");

    assert_eq!(report["track"], "input");
    assert_eq!(report["flow"], "0x7");
    assert_eq!(report["total_us"], 20_300);
    assert_eq!(report["budget_us"], DEFAULT_FLOW_BUDGET_US);
    assert_eq!(report["steps"].as_array().unwrap().len(), 2);
    let slowest = &report["slowest_step"];
    assert_eq!(slowest["from"], "socket.arrived");
    assert_eq!(slowest["to"], "session.dequeued");
    assert_eq!(slowest["us"], 20_000);
    assert_eq!(slowest["percent"], 98);
    assert_eq!(slowest["by"], "source-1");
    assert_eq!(slowest["during"][0]["span"], "host.tick");
    assert_eq!(slowest["during"][0]["label"], "amaterasu.state");
    assert_eq!(slowest["during"][0]["overlap_us"], 19_600);
}

#[test]
fn a_flow_within_its_budget_is_counted_but_not_reported() {
    let mut collector = Collector::default();
    collector.observe(mark(1, ARRIVED, 100), &Names);
    collector.observe(end(1, INJECTED, 900, 0), &Names);
    // The producer's own budget overrides the default in both directions.
    collector.observe(mark(2, ARRIVED, 100), &Names);
    collector.observe(end(2, INJECTED, 900, 500), &Names);
    collector.observe(mark(3, ARRIVED, 100), &Names);
    collector.observe(end(3, INJECTED, 9_100, 20_000), &Names);

    let reports = collector.poll(10_000_100, 0, &Names);
    let slow: Vec<_> = reports
        .iter()
        .filter(|report| report.target == "trace.slow_flow")
        .collect();
    assert_eq!(slow.len(), 1);
    assert_eq!(slow[0].body["flow"], "0x2");

    let window = only(&reports, "trace.window");
    let track = &window["tracks"][0];
    assert_eq!(track["track"], "input");
    assert_eq!(track["completed"], 3);
    assert_eq!(track["slow"], 1);
    assert_eq!(track["total"]["calls"], 3);
    assert_eq!(track["total"]["max_us"], 9_000);
    let step = &track["steps"][0];
    assert_eq!(step["stage"], "socket.arrived>input.accepted");
    assert_eq!(step["calls"], 3);
    assert_eq!(step["total_us"], 10_600);
}

#[test]
fn marks_recorded_out_of_order_are_ordered_by_their_own_timestamps() {
    let mut collector = Collector::default();
    // The kernel arrival time is learned after the first userspace stage.
    collector.observe(mark(9, DEQUEUED, 5_000), &Names);
    collector.observe(mark(9, ARRIVED, 1_000), &Names);
    collector.observe(end(9, INJECTED, 10_000, 1), &Names);

    let report = only(&collector.poll(50_000, 0, &Names), "trace.slow_flow");
    assert_eq!(report["first_stage"], "socket.arrived");
    assert_eq!(report["total_us"], 9_000);
    assert_eq!(report["steps"][0]["us"], 4_000);
    assert_eq!(report["steps"][1]["us"], 5_000);
}

#[test]
fn a_long_span_is_reported_once_and_summarized_with_its_label() {
    let mut collector = Collector::default();
    collector.observe(
        span(HOST_TICK, STATE_MODULE, 50_000, SLOW_SPAN_US + 1_000),
        &Names,
    );
    collector.observe(span(HOST_TICK, STATE_MODULE, 60_000, 150), &Names);

    let reports = collector.poll(60_100, 0, &Names);
    let slow = only(&reports, "trace.slow_span");
    assert_eq!(slow["span"], "host.tick");
    assert_eq!(slow["label"], "amaterasu.state");
    assert_eq!(slow["us"], SLOW_SPAN_US + 1_000);
    assert_eq!(slow["by"], "source-3");

    let window = only(&collector.poll(10_100_000, 0, &Names), "trace.window");
    assert_eq!(window["spans"][0]["stage"], "host.tick:amaterasu.state");
    assert_eq!(window["spans"][0]["calls"], 2);
    assert_eq!(window["spans"][0]["max_us"], SLOW_SPAN_US + 1_000);
}

#[test]
fn a_discarded_flow_leaves_no_statistics_and_raises_no_report() {
    let mut collector = Collector::default();
    // A control message waited a long time, then turned out to carry no input.
    collector.observe(mark(1, ARRIVED, 1_000), &Names);
    collector.observe(mark(1, DEQUEUED, 161_000), &Names);
    collector.observe(
        Observation {
            kind: TRACE_DISCARD,
            stage: 0,
            ..mark(1, 0, 161_010)
        },
        &Names,
    );
    // A real input event on the same track is unaffected.
    collector.observe(mark(2, ARRIVED, 200_000), &Names);
    collector.observe(end(2, INJECTED, 200_400, 0), &Names);
    // Discarding an unknown flow changes nothing.
    collector.observe(
        Observation {
            kind: TRACE_DISCARD,
            stage: 0,
            ..mark(77, 0, 200_500)
        },
        &Names,
    );

    let reports = collector.poll(10_001_000, 0, &Names);
    assert!(
        reports
            .iter()
            .all(|report| report.target != "trace.slow_flow")
    );
    let window = only(&reports, "trace.window");
    let track = &window["tracks"][0];
    assert_eq!(track["completed"], 1);
    assert_eq!(track["discarded"], 1);
    assert_eq!(track["abandoned"], 0);
    assert_eq!(track["total"]["max_us"], 400);
    assert_eq!(window["open_flows"], 0);
}

#[test]
fn unfinished_flows_expire_as_abandoned_instead_of_filling_the_table() {
    let mut collector = Collector::default();
    collector.observe(mark(1, ARRIVED, 1_000), &Names);
    collector.observe(mark(2, ARRIVED, 4_000_000), &Names);
    // Flow 1 is older than the timeout at this poll; flow 2 is not.
    assert!(collector.poll(5_500_000, 0, &Names).is_empty());
    collector.observe(end(2, INJECTED, 5_600_000, u64::MAX), &Names);

    let window = only(&collector.poll(10_001_000, 3, &Names), "trace.window");
    assert_eq!(window["tracks"][0]["abandoned"], 1);
    assert_eq!(window["tracks"][0]["completed"], 1);
    assert_eq!(window["open_flows"], 0);
    assert_eq!(window["dropped_records"], 3);
}

#[test]
fn a_burst_of_slow_flows_is_bounded_and_the_rest_are_counted() {
    let mut collector = Collector::default();
    for flow in 0..50 {
        let start = 1_000 + flow * 100;
        collector.observe(mark(flow, ARRIVED, start), &Names);
        collector.observe(end(flow, INJECTED, start + 9_000 + flow, 0), &Names);
    }
    let reports = collector.poll(10_100_000, 0, &Names);
    let reported = reports
        .iter()
        .filter(|report| report.target == "trace.slow_flow")
        .count();
    assert_eq!(reported, 20);
    let window = only(&reports, "trace.window");
    assert_eq!(window["tracks"][0]["slow"], 50);
    assert_eq!(window["tracks"][0]["slow_reports_suppressed"], 30);
    assert_eq!(window["tracks"][0]["worst_suppressed_us"], 9_049);

    // The next window may report again.
    collector.observe(mark(99, ARRIVED, 10_200_000), &Names);
    collector.observe(end(99, INJECTED, 10_220_000, 0), &Names);
    only(&collector.poll(10_300_000, 0, &Names), "trace.slow_flow");
}

#[test]
fn records_without_a_name_or_with_an_unknown_kind_are_ignored() {
    let mut collector = Collector::default();
    collector.observe(
        Observation {
            track: 0,
            ..mark(1, ARRIVED, 10)
        },
        &Names,
    );
    collector.observe(
        Observation {
            stage: 0,
            ..mark(1, ARRIVED, 10)
        },
        &Names,
    );
    collector.observe(
        Observation {
            kind: 9,
            ..mark(1, ARRIVED, 10)
        },
        &Names,
    );
    let window = only(&collector.poll(10_000_100, 0, &Names), "trace.window");
    assert_eq!(window["ignored_records"], 3);
    assert_eq!(window["open_flows"], 0);
    assert!(window["tracks"].as_array().unwrap().is_empty());
}

#[test]
fn a_bound_sink_stamps_records_and_attributes_them_to_its_endpoint() {
    let hub = Arc::new(TraceHub::new(vec!["first".into(), "second".into()]));
    let binding = Binding::new(hub.clone(), "second");
    // SAFETY: the binding is live for this test and owns the table address.
    let sink = unsafe { TraceSink::from_address(binding.address()) }.unwrap();

    let track = sink.intern("input");
    let stage = sink.intern("session.dequeued");
    assert_ne!(track, 0);
    assert_ne!(stage, 0);
    assert_eq!(sink.intern("input"), track);
    assert_eq!(sink.intern("not a valid name"), 0);

    let before = sink.now();
    sink.record(&TraceRecord {
        flow: 42,
        track,
        stage,
        kind: TRACE_MARK,
        ..Default::default()
    });
    sink.record(&TraceRecord {
        flow: 42,
        timestamp_us: 5,
        track,
        stage,
        kind: TRACE_END,
        ..Default::default()
    });
    let after = sink.now();

    let stamped = hub.pop_for_test().unwrap();
    assert_eq!(
        (stamped.flow, stamped.source, stamped.kind),
        (42, 2, TRACE_MARK)
    );
    assert!((before..=after).contains(&stamped.timestamp_us));
    // A producer-supplied timestamp is kept exactly.
    assert_eq!(hub.pop_for_test().unwrap().timestamp_us, 5);
    assert!(hub.pop_for_test().is_none());
}

#[test]
fn the_collector_thread_retains_history_for_a_management_surface() {
    let hub = Arc::new(TraceHub::new(vec!["producer".into()]));
    let broker = Arc::new(std::sync::Mutex::new(
        crate::Broker::new(Default::default()).unwrap(),
    ));
    let binding = Binding::new(hub.clone(), "producer");
    // SAFETY: the binding outlives the sink within this test.
    let sink = unsafe { TraceSink::from_address(binding.address()) }.unwrap();
    hub.ensure_collector(&broker);
    assert_eq!(hub.snapshot()["collecting"], true);

    let span = sink.intern("host.tick");
    sink.record(&TraceRecord {
        value: SLOW_SPAN_US * 2,
        track: span,
        kind: TRACE_SPAN,
        ..Default::default()
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while hub.snapshot()["slow"].as_array().unwrap().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "collector never reported"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let snapshot = hub.snapshot();
    assert_eq!(snapshot["schema"], 2);
    assert_eq!(snapshot["slow"][0]["kind"], "span");
    assert_eq!(snapshot["slow"][0]["span"], "host.tick");
    assert_eq!(snapshot["slow"][0]["by"], "producer");
    assert!(snapshot["slow"][0]["observed_unix_ms"].as_u64().unwrap() > 0);
    hub.shutdown();
    // Shutdown is idempotent and leaves history readable.
    hub.shutdown();
    assert_eq!(hub.snapshot()["slow"].as_array().unwrap().len(), 1);
}

fn window_report(completed: u64, pass_max_us: u64) -> Report {
    Report {
        target: "trace.window",
        body: serde_json::json!({
            "event": "trace_window",
            "window_us": 10_000_000,
            "dropped_records": 1,
            "tracks": [{
                "track": "input", "completed": completed, "slow": 1, "abandoned": 0, "budget_us": 8_000,
                "total": {"stage":"total","calls":completed,"total_us":900 * completed,"max_us":700 + completed,
                          "mean_us":900,"p50_upper_us":250,"p95_upper_us":500 + completed,"p99_upper_us":700},
                "steps": [
                    {"stage":"a>b","calls":completed,"total_us":600 * completed,"max_us":500,"p95_upper_us":400},
                    {"stage":"b>c","calls":completed,"total_us":300 * completed,"max_us":200,"p95_upper_us":150},
                ],
            }],
            "spans": [
                {"stage":"host.pass","calls":5_000,"total_us":3_000_000,"max_us":pass_max_us,"p95_upper_us":1_000},
                {"stage":"host.wait","calls":5_000,"total_us":6_000_000,"max_us":1_900,"p95_upper_us":1_900},
                {"stage":"host.tick:amaterasu.video","calls":3,"total_us":900,"max_us":400,"p95_upper_us":400},
                {"stage":"host.tick:amaterasu.state","calls":1,"total_us":800,"max_us":800,"p95_upper_us":800},
            ],
        }),
    }
}

fn slow_flow_report(total_us: u64, held_label: &str) -> Report {
    Report {
        target: "trace.slow_flow",
        body: serde_json::json!({
            "event": "slow_flow", "track": "input", "total_us": total_us, "budget_us": 8_000,
            "steps": [{"from":"a","to":"b","us":total_us - 10,"by":"amaterasu.x"}],
            "slowest_step": {"from":"a","to":"b","us":total_us - 10,"percent":99,"by":"amaterasu.x",
                "during": [
                    {"span":"host.pass","label":null,"us":total_us,"overlap_us":total_us},
                    {"span":"host.tick","label":held_label,"us":total_us - 20,"overlap_us":total_us - 30},
                    {"span":"host.tick","label":"amaterasu.other","us":40,"overlap_us":40},
                ]},
        }),
    }
}

#[test]
fn a_report_merges_the_last_minute_and_charts_the_last_half_hour() {
    let hub = Arc::new(TraceHub::new(vec!["producer".into()]));
    for index in 1..=200_u64 {
        hub.record_for_test(
            &window_report(index, 2_000 + index),
            1_000_000 + index * 10_000,
        );
    }

    let report = hub.snapshot();
    assert_eq!(report["schema"], 2);
    // The six newest windows are one summary: counts add up, the worst stays the worst.
    let recent = &report["recent"];
    assert_eq!(recent["window_us"], 60_000_000);
    assert_eq!(recent["dropped_records"], 6);
    let input = &recent["tracks"][0];
    assert_eq!(input["track"], "input");
    assert_eq!(input["completed"], 195 + 196 + 197 + 198 + 199 + 200);
    assert_eq!(input["slow"], 6);
    assert_eq!(input["budget_us"], 8_000);
    assert_eq!(input["total"]["mean_us"], 900);
    assert_eq!(input["total"]["max_us"], 900);
    assert_eq!(input["total"]["p95_upper_us"], 700);
    assert_eq!(input["steps"][0]["stage"], "a>b");
    assert_eq!(input["steps"][1]["mean_us"], 300);
    // The host's own spans are reported apart from the plugin calls.
    assert_eq!(recent["host"]["pass"]["max_us"], 2_200);
    assert_eq!(recent["host"]["pass"]["calls"], 30_000);
    assert_eq!(recent["spans"][0]["stage"], "host.tick:amaterasu.state");
    assert_eq!(recent["spans"].as_array().unwrap().len(), 2);
    // The trend is columns of numbers, oldest first, ending at the newest window.
    let trend = &report["trend"];
    let ages = trend["age_s"].as_array().unwrap();
    assert_eq!(ages.len(), 180);
    assert_eq!(ages[0], 1_790);
    assert_eq!(ages[179], 0);
    assert_eq!(trend["newest_unix_ms"], 3_000_000);
    assert_eq!(trend["tracks"]["input"]["max_us"][179], 900);
    assert_eq!(trend["tracks"]["input"]["budget_us"], 8_000);
    assert_eq!(trend["tracks"]["input"]["p95_upper_us"][0], 521);
    assert_eq!(trend["host_pass_max_us"][179], 2_200);
}

#[test]
fn slow_flows_behind_one_pause_are_one_moment_naming_what_held_the_thread() {
    let hub = Arc::new(TraceHub::new(vec!["producer".into()]));
    for burst in 0..18_u64 {
        hub.record_for_test(
            &slow_flow_report(100_000 + burst, "amaterasu.lifecycle"),
            5_000 + burst,
        );
    }
    hub.record_for_test(&slow_flow_report(9_000, "amaterasu.pairing"), 9_000);
    hub.record_for_test(
        &Report {
            target: "trace.slow_span",
            body: serde_json::json!({"event":"slow_span","span":"host.pass","label":null,"us":12_000,"by":"@host"}),
        },
        9_001,
    );
    hub.record_for_test(
        &Report {
            target: "trace.slow_span",
            body: serde_json::json!({"event":"slow_span","span":"host.tick","label":"amaterasu.pairing","us":9_100,"by":"@host"}),
        },
        9_002,
    );

    let slow = hub.snapshot()["slow"].clone();
    let slow = slow.as_array().unwrap();
    // Newest first; the bare host pass is not a moment of its own.
    assert_eq!(slow.len(), 3);
    assert_eq!(slow[0]["kind"], "span");
    assert_eq!(slow[0]["label"], "amaterasu.pairing");
    assert_eq!(slow[1]["held_by"]["label"], "amaterasu.pairing");
    assert_eq!(slow[1]["count"], 1);
    let burst = &slow[2];
    assert_eq!(burst["count"], 18);
    assert_eq!(burst["observed_unix_ms"], 5_000);
    assert_eq!(burst["until_unix_ms"], 5_017);
    assert_eq!(burst["us"], 100_017);
    assert_eq!(burst["step"], "a>b");
    assert_eq!(burst["budget_us"], 8_000);
    // The longest overlap that is not the host's own pass.
    assert_eq!(burst["held_by"]["span"], "host.tick");
    assert_eq!(burst["held_by"]["label"], "amaterasu.lifecycle");
}

#[test]
fn a_long_run_of_slow_frames_is_one_moment_and_a_brief_call_is_not_blamed_for_it() {
    let hub = Arc::new(TraceHub::new(vec!["producer".into()]));
    // The encoder runs slow for twelve seconds. Each frame overlaps some
    // short plugin call, a different one each time, and a pairing call is
    // reported in the middle of the run.
    let frame = |index: u64| {
        let mut report = slow_flow_report(13_000 + index, "unused");
        report.body["track"] = "video".into();
        report.body["slowest_step"]["during"] = serde_json::json!([
            {"span":"host.tick","label":format!("amaterasu.module{index}"),"us":1_700,"overlap_us":1_700},
        ]);
        report
    };
    for index in 0..12_u64 {
        hub.record_for_test(&frame(index), 100_000 + index * 1_000);
        if index == 5 {
            hub.record_for_test(
                &Report {
                    target: "trace.slow_span",
                    body: serde_json::json!({"event":"slow_span","span":"host.tick","label":"amaterasu.pairing","us":9_000,"by":"@host"}),
                },
                105_500,
            );
        }
    }
    // After a quiet gap the same cause starts a new moment.
    hub.record_for_test(&frame(50), 130_000);

    let slow = hub.snapshot()["slow"].clone();
    let slow = slow.as_array().unwrap();
    assert_eq!(slow.len(), 3);
    assert_eq!(slow[0]["count"], 1);
    assert_eq!(slow[0]["observed_unix_ms"], 130_000);
    let run = &slow[1];
    assert_eq!(run["track"], "video");
    assert_eq!(run["count"], 12);
    assert_eq!(run["observed_unix_ms"], 100_000);
    assert_eq!(run["until_unix_ms"], 111_000);
    assert_eq!(run["us"], 13_011);
    // A 1.7 ms call did not make a 13 ms step slow.
    assert_eq!(run["held_by"], serde_json::Value::Null);
    assert_eq!(slow[2]["label"], "amaterasu.pairing");
}

#[test]
fn only_a_pause_a_person_would_notice_reaches_the_default_log() {
    use super::report::is_notable;
    let flow = |total_us: u64| serde_json::json!({"total_us": total_us, "budget_us": 12_000});
    // A frame just over its limit is routine; one twice over is an event.
    assert!(!is_notable("trace.slow_flow", &flow(12_100)));
    assert!(is_notable("trace.slow_flow", &flow(24_000)));
    let span = |us: u64| serde_json::json!({"span": "host.tick", "us": us});
    assert!(!is_notable("trace.slow_span", &span(SLOW_SPAN_US + 1_000)));
    assert!(is_notable("trace.slow_span", &span(SLOW_SPAN_US * 2)));
    // A window is written every ten seconds whatever happened.
    assert!(!is_notable(
        "trace.window",
        &serde_json::json!({"records": 10_000})
    ));
}

#[test]
fn a_report_fits_one_management_response_whatever_was_recorded() {
    let hub = Arc::new(TraceHub::new(vec!["producer".into()]));
    // The longest names a producer may intern, on every track, step and span.
    let name = |prefix: &str, index: usize| format!("{prefix}{index:02}.{}", "n".repeat(90));
    let metric = |stage: String| {
        serde_json::json!({"stage":stage,"calls":u32::MAX,"total_us":u64::MAX / 8,"max_us":u32::MAX,
            "mean_us":u32::MAX,"p50_upper_us":u32::MAX,"p95_upper_us":u32::MAX,"p99_upper_us":u32::MAX})
    };
    let tracks: Vec<_> = (0..12)
        .map(|track| {
            serde_json::json!({
                "track": name("track", track), "completed": u32::MAX, "slow": u32::MAX, "abandoned": u32::MAX,
                "total": metric("total".into()),
                "steps": (0..11).map(|step| metric(format!("{}>{}", name("a", step), name("b", step)))).collect::<Vec<_>>(),
            })
        })
        .collect();
    let spans: Vec<_> = (0..64).map(|span| metric(name("span", span))).collect();
    for index in 0..200_u64 {
        hub.record_for_test(
            &Report {
                target: "trace.window",
                body: serde_json::json!({"event":"trace_window","window_us":10_000_000,"tracks":tracks,"spans":spans}),
            },
            1_000_000 + index * 10_000,
        );
        let mut flow = slow_flow_report(u64::from(u32::MAX), &name("module", index as usize % 50));
        flow.body["track"] = name("track", index as usize % 12).into();
        hub.record_for_test(&flow, 1_000_000 + index * 10_000);
    }

    let report = hub.snapshot();
    assert!(
        report.to_string().len() <= 14 * 1024,
        "{} bytes",
        report.to_string().len()
    );
    assert_eq!(report["schema"], 2);
    // What just happened survives the squeeze.
    assert!(!report["slow"].as_array().unwrap().is_empty());
}

/// Prints what one observation costs its producer. Run with
/// `cargo test --release -p amaterasu-runtime recording_cost -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, meaningful only in a release build"]
fn recording_cost_per_observation_stays_far_below_a_microsecond() {
    use std::sync::atomic::{AtomicBool, Ordering};
    const RECORDS: u64 = 4_000_000;
    let hub = Arc::new(TraceHub::new(vec!["producer".into()]));
    let binding = Binding::new(hub.clone(), "producer");
    // SAFETY: the binding outlives the sink within this test.
    let sink = unsafe { TraceSink::from_address(binding.address()) }.unwrap();
    let (track, stage) = (sink.intern("input"), sink.intern("session.dequeued"));
    // Stand in for the collector so the queue never fills.
    let stop = Arc::new(AtomicBool::new(false));
    let drainer = {
        let (hub, stop) = (hub.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut drained = 0_u64;
            while !stop.load(Ordering::Acquire) {
                while hub.pop_for_test().is_some() {
                    drained += 1;
                }
                std::thread::yield_now();
            }
            while hub.pop_for_test().is_some() {
                drained += 1;
            }
            drained
        })
    };
    let started = std::time::Instant::now();
    for flow in 0..RECORDS {
        sink.record(&TraceRecord {
            flow,
            track,
            stage,
            kind: TRACE_MARK,
            ..Default::default()
        });
    }
    let nanoseconds = started.elapsed().as_nanos() / u128::from(RECORDS);
    stop.store(true, Ordering::Release);
    let drained = drainer.join().unwrap();
    println!(
        "{nanoseconds} ns per record on the producer; {drained} of {RECORDS} reached the collector"
    );
    assert!(nanoseconds < 1_000);
}
