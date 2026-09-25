use super::*;

fn record() -> Value {
    json!({"action":"record","level":"info","target":"capture","message":"started"})
}

#[test]
fn records_and_counters_are_caller_scoped() {
    let mut journal = Journal::default();
    assert_eq!(journal.handle("alice", record())["sequence"], 1);
    journal.handle(
        "alice",
        json!({"action":"increment","name":"frames","amount":3}),
    );
    let foreign = journal.handle("bob", json!({"action":"query"}));
    assert_eq!(foreign["records"], json!([]));
    assert_eq!(foreign["latest_sequence"], 0);
    assert_eq!(
        journal.handle("bob", json!({"action":"counters"}))["counters"],
        json!({})
    );
    let own = journal.handle("alice", json!({"action":"query"}));
    assert_eq!(own["records"][0]["message"], "started");
    assert_eq!(
        journal.handle("alice", json!({"action":"query","caller":"bob"}))["code"],
        "invalid_request"
    );
}

#[test]
fn retention_reports_loss_and_pagination_resynchronizes() {
    let mut journal = Journal::default();
    for _ in 0..130 {
        journal.handle("alice", record());
    }
    let first = journal.handle("alice", json!({"action":"query","after":0,"limit":2}));
    assert_eq!(first["dropped_total"], 2);
    assert_eq!(first["oldest_sequence"], 3);
    assert_eq!(first["resync_required"], true);
    assert_eq!(first["records"][0]["sequence"], 3);
    assert_eq!(first["next_cursor"], 4);
    assert_eq!(first["has_more"], true);
    let next = journal.handle("alice", json!({"action":"query","after":4}));
    assert_eq!(next["resync_required"], false);
    assert_eq!(next["records"][0]["sequence"], 5);
    let tail = journal.handle("alice", json!({"action":"query","after":130}));
    assert_eq!(tail["next_cursor"], 130);
    assert_eq!(tail["has_more"], false);
    assert_eq!(
        journal.handle("alice", json!({"action":"query","after":131}))["code"],
        "invalid_cursor"
    );
}

#[test]
fn limits_reject_without_appending_or_allocating_callers() {
    let mut journal = Journal::default();
    for invalid in [
        json!({"action":"record","level":"fatal","target":"x","message":""}),
        json!({"action":"record","level":"info","target":"x","message":"x".repeat(513)}),
        json!({"action":"record","level":"info","target":"x","message":"\u{0}".repeat(512)}),
        json!({"action":"record","level":"info","target":"x","message":"","fields":{"a":1}}),
        json!({"action":"increment","name":"x\n","amount":1}),
    ] {
        assert_eq!(journal.handle("alice", invalid)["ok"], false);
    }
    assert!(journal.callers.is_empty());
    for limit in [0, 5, usize::MAX] {
        assert_eq!(
            journal.handle("alice", json!({"action":"query","limit":limit}))["code"],
            "invalid_limit"
        );
    }
    for caller in 0..MAX_CALLERS {
        assert_eq!(
            journal.handle(&format!("caller{caller}"), record())["ok"],
            true
        );
    }
    assert_eq!(journal.handle("extra", record())["code"], "caller_limit");
    assert_eq!(
        journal.handle("extra", json!({"action":"query"}))["ok"],
        true
    );
    assert_eq!(journal.handle("caller0", record())["sequence"], 2);
}

#[test]
fn counter_capacity_and_overflow_preserve_values() {
    let mut journal = Journal::default();
    for index in 0..MAX_COUNTERS {
        assert_eq!(
            journal.handle(
                "a",
                json!({"action":"increment","name":format!("counter{index}"),"amount":u64::MAX})
            )["ok"],
            true
        );
    }
    assert_eq!(
        journal.handle("a", json!({"action":"increment","name":"extra","amount":1}))["code"],
        "counter_limit"
    );
    assert_eq!(
        journal.handle(
            "a",
            json!({"action":"increment","name":"counter0","amount":1})
        )["code"],
        "counter_overflow"
    );
    assert_eq!(
        journal.handle("a", json!({"action":"counters"}))["counters"]["counter0"],
        u64::MAX
    );
}

#[test]
fn response_serialization_fits_runtime_payload_budget() {
    let mut journal = Journal::default();
    for _ in 0..4 {
        assert_eq!(journal.handle("a", json!({"action":"record","level":"error","target":"t".repeat(64),"message":"\u{0}".repeat(250),"fields":{"details":"é".repeat(100)}}))["ok"], true);
    }
    let response = journal.handle("a", json!({"action":"query"}));
    let encoded = serde_json::to_vec(&response).unwrap();
    assert!(encoded.len() < 16 * 1024);
    assert_eq!(serde_json::from_slice::<Value>(&encoded).unwrap(), response);
    for index in 0..MAX_COUNTERS {
        journal.handle(
            "a",
            json!({"action":"increment","name":format!("{index:064}"),"amount":u64::MAX}),
        );
    }
    assert!(
        serde_json::to_vec(&journal.handle("a", json!({"action":"counters"})))
            .unwrap()
            .len()
            < 16 * 1024
    );
}

#[test]
fn sequence_exhaustion_does_not_evict_a_record() {
    let mut journal = Journal::default();
    journal.handle("a", record());
    journal.callers.get_mut("a").unwrap().sequence = u64::MAX;
    assert_eq!(journal.handle("a", record())["code"], "sequence_exhausted");
    assert_eq!(journal.callers["a"].records.len(), 1);
    assert_eq!(journal.callers["a"].dropped_total, 0);
}

#[test]
fn configured_minimum_level_filters_file_output_but_keeps_caller_records() {
    let state_root = std::env::temp_dir().join(format!(
        "amaterasu-diagnostics-journal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&state_root);
    std::fs::create_dir_all(&state_root).unwrap();
    let settings = super::super::logging::LoggingSettings::from_value(json!({
        "min_log_level": 1,
        "log_path": "diagnostics.log",
        "state_root": state_root.to_string_lossy(),
    }))
    .unwrap();
    let (mut journal, warning) = Journal::with_logging(settings);
    assert_eq!(warning, None);
    assert!(
        journal.handle(
            "alice",
            json!({
                "action":"record","level":"trace","target":"capture","message":"below threshold"
            })
        )["ok"]
            .as_bool()
            .unwrap()
    );
    assert!(
        journal.handle(
            "alice",
            json!({
                "action":"record","level":"debug","target":"capture","message":"at threshold"
            })
        )["ok"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        journal.handle(
            "bob",
            json!({
                "action":"record","level":"debug","target":"capture",
                "message":"same target, different caller"
            })
        )["sequence"],
        1
    );
    assert_eq!(
        journal.handle("alice", json!({"action":"query"}))["records"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    drop(journal);
    let log_directory = state_root.join("logs");
    let logged = std::fs::read_dir(&log_directory)
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    let records: Vec<Value> = logged
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["caller"], "alice");
    assert_eq!(records[0]["target"], "capture");
    assert_eq!(records[1]["caller"], "bob");
    assert_eq!(records[1]["target"], "capture");
    assert!(logged.contains("at threshold"));
    assert!(!logged.contains("below threshold"));
    std::fs::remove_dir_all(state_root).unwrap();
}
