use super::*;
use model::HistorySettings;
use serde_json::json;
use std::fs;

fn operation(action: &str) -> Operation {
    let mut payload = json!({"action": action});
    match action {
        "settings_update" => {
            payload["settings"] = json!({"enabled":true,"ttl_days":0,"db_size_limit_mb":0});
            payload["expected_revision"] = json!(1);
        }
        "session_begin" => payload["metadata"] = json!({"session_id":"id","protocol":"rtsp"}),
        "stream_activity_begin"
        | "stream_activity_end"
        | "session_end"
        | "session_detail"
        | "delete_session" => {
            payload["session_id"] = json!("id");
        }
        "record_sample" => {
            payload["sample"] = json!({"session_id":"id","recorded_at_unix":0});
        }
        "record_event" => {
            payload["event"] = json!({"session_id":"id","recorded_at_unix":0,"event_type":"test"});
        }
        _ => {}
    }
    serde_json::from_value(payload).expect("known operation")
}

#[test]
fn rpc_access_is_split_between_management_and_stream_lifecycle() {
    for action in [
        "settings_get",
        "settings_update",
        "list_sessions",
        "session_detail",
        "active_sessions",
        "history_status",
        "delete_session",
    ] {
        let operation = operation(action);
        assert!(is_authorized(MANAGEMENT_CALLER, &operation), "{action}");
        assert!(
            !is_authorized(STREAM_SESSION_CALLER, &operation),
            "{action}"
        );
    }

    for action in [
        "stream_activity_begin",
        "stream_activity_end",
        "session_begin",
        "session_end",
        "record_sample",
        "record_event",
    ] {
        let operation = operation(action);
        assert!(!is_authorized(MANAGEMENT_CALLER, &operation), "{action}");
        assert!(is_authorized(STREAM_SESSION_CALLER, &operation), "{action}");
    }
}

#[test]
fn history_settings_rpc_rejects_negative_retention_values() {
    let invalid: Result<HistorySettings, _> = serde_json::from_value(json!({
        "enabled": true,
        "ttl_days": -1,
        "db_size_limit_mb": 0
    }));
    assert!(invalid.is_ok());
    assert!(invalid.unwrap().validate().is_err());
}

#[test]
fn sample_latency_is_nullable_and_missing_legacy_values_are_unknown() {
    let mut missing = json!({
        "session_id":"sample",
        "recorded_at_unix":1,
        "actual_fps":0.0,
        "actual_bitrate_kbps":0.0
    });
    let sample: model::SessionSample = serde_json::from_value(missing.clone()).unwrap();
    assert_eq!(sample.encode_latency_ms, None);
    assert!(sample.validate().is_ok());

    missing["encode_latency_ms"] = json!(null);
    let explicit_unknown: model::SessionSample = serde_json::from_value(missing).unwrap();
    assert_eq!(explicit_unknown.encode_latency_ms, None);
    assert!(explicit_unknown.validate().is_ok());

    let zero_is_a_present_measurement = serde_json::from_value::<model::SessionSample>(json!({
        "session_id":"sample",
        "recorded_at_unix":1,
        "encode_latency_ms":0.0
    }))
    .unwrap();
    assert_eq!(zero_is_a_present_measurement.encode_latency_ms, Some(0.0));
}

#[test]
fn status_reports_storage_unavailability_without_failing_the_module() {
    let response = unavailable_status();
    assert_eq!(response["ok"], true);
    assert_eq!(response["status"]["available"], false);
    assert!(response["status"]["applied_revision"].is_null());
    assert_eq!(response["status"]["degraded"], true);
    assert_eq!(response["status"]["pending_activities"], 0);
}

#[test]
fn status_rpc_expires_launches_before_reporting_effective_recording() {
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-history-pending-status-{}-{}",
        std::process::id(),
        amaterasu_storage::random_identifier().unwrap()
    ));
    let mut store = HistoryStore::open(&directory.join("history.db"), 100).unwrap();
    store
        .apply_configuration_with_revision(HistorySettings::default(), 1, 100)
        .unwrap();
    assert!(store.begin_stream_activity("pending", 101).unwrap());
    store
        .apply_configuration_with_revision(
            HistorySettings {
                enabled: false,
                ..HistorySettings::default()
            },
            2,
            102,
        )
        .unwrap();
    let waiting = execute(&mut store, operation("history_status"), 400).unwrap();
    assert_eq!(waiting["status"]["pending_activities"], 1);
    assert_eq!(waiting["status"]["active_streams"], 0);
    assert_eq!(waiting["status"]["effective_enabled"], true);
    assert_eq!(waiting["status"]["applied_revision"], 1);
    let status = execute(&mut store, operation("history_status"), 401).unwrap();
    assert_eq!(status["status"]["pending_activities"], 0);
    assert_eq!(status["status"]["effective_enabled"], false);
    assert_eq!(status["status"]["applied_revision"], 2);
    drop(store);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn configured_database_path_is_parsed_and_must_be_absolute() {
    let configured_path = std::env::temp_dir().join("session-history-test.db");
    let settings: ModuleSettings = serde_json::from_value(json!({
        "database_path": configured_path.to_string_lossy().into_owned()
    }))
    .unwrap();
    assert_eq!(
        database_path_from_settings(settings).unwrap(),
        configured_path
    );
    assert!(validate_database_path("relative/history.db".into()).is_err());
}

#[test]
fn saved_recording_toggle_waits_for_stream_drain_but_retention_survives_restart() {
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-history-policy-{}-{}",
        std::process::id(),
        amaterasu_storage::random_identifier().unwrap()
    ));
    let database = directory.join("history.db");
    let call = |store: &mut HistoryStore, payload: serde_json::Value, now| {
        let operation: Operation = serde_json::from_value(payload).unwrap();
        execute(store, operation, now).unwrap()
    };
    let policy = |enabled| HistorySettings {
        enabled,
        ttl_days: 1,
        db_size_limit_mb: 0,
    };

    let mut store = HistoryStore::open(&database, 100).unwrap();
    store.apply_configuration(policy(false), 100).unwrap();
    assert_eq!(
        call(
            &mut store,
            json!({"action":"session_begin","metadata":{"session_id":"unrecorded","protocol":"sunshine-modern"}}),
            100
        )["recorded"],
        false
    );
    store.apply_configuration(policy(true), 101).unwrap();
    assert_eq!(store.settings(), (&policy(true), false));
    assert_eq!(
        call(
            &mut store,
            json!({"action":"session_begin","metadata":{"session_id":"also-unrecorded","protocol":"sunshine-modern"}}),
            102
        )["recorded"],
        false
    );
    for id in ["unrecorded", "also-unrecorded"] {
        assert_eq!(
            call(
                &mut store,
                json!({"action":"session_end","session_id":id}),
                103
            )["recorded"],
            false
        );
    }
    assert_eq!(store.settings(), (&policy(true), true));
    assert_eq!(
        call(
            &mut store,
            json!({"action":"session_begin","metadata":{"session_id":"recorded","protocol":"sunshine-modern"}}),
            104
        )["recorded"],
        true
    );
    store.apply_configuration(policy(false), 105).unwrap();
    assert_eq!(store.settings(), (&policy(false), true));
    assert_eq!(
        call(
            &mut store,
            json!({"action":"session_begin","metadata":{"session_id":"also-recorded","protocol":"sunshine-modern"}}),
            106
        )["recorded"],
        true
    );
    assert_eq!(
        call(
            &mut store,
            json!({"action":"record_sample","sample":{"session_id":"recorded","recorded_at_unix":107,"frames_sent":7}}),
            107
        )["recorded"],
        true
    );
    assert_eq!(
        call(
            &mut store,
            json!({"action":"session_end","session_id":"recorded"}),
            108
        )["recorded"],
        true
    );
    assert_eq!(store.settings(), (&policy(false), true));
    assert_eq!(
        call(
            &mut store,
            json!({"action":"session_end","session_id":"also-recorded"}),
            109
        )["recorded"],
        true
    );
    assert_eq!(store.settings(), (&policy(false), false));
    assert_eq!(
        call(
            &mut store,
            json!({"action":"session_begin","metadata":{"session_id":"disabled","protocol":"sunshine-modern"}}),
            110
        )["recorded"],
        false
    );
    assert_eq!(
        call(
            &mut store,
            json!({"action":"session_end","session_id":"disabled"}),
            111
        )["recorded"],
        false
    );
    drop(store);

    let mut reopened = HistoryStore::open(&database, 200).unwrap();
    reopened.apply_configuration(policy(false), 200).unwrap();
    let list = call(
        &mut reopened,
        json!({"action":"list_sessions","limit":25}),
        200,
    );
    assert_eq!(list["sessions"].as_array().unwrap().len(), 2);
    assert_eq!(
        call(
            &mut reopened,
            json!({"action":"session_detail","session_id":"recorded"}),
            200
        )["detail"]["sample_count"],
        1
    );
    reopened.prune_if_due(86_510);
    assert_eq!(
        call(&mut reopened, json!({"action":"history_status"}), 86_510)["status"]["sessions"],
        2
    );
    drop(reopened);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn restart_during_pending_recording_toggles_adopts_saved_policy_without_phantom_sessions() {
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-history-pending-restart-{}-{}",
        std::process::id(),
        amaterasu_storage::random_identifier().unwrap()
    ));
    let database = directory.join("history.db");
    let call = |store: &mut HistoryStore, caller: &str, payload: serde_json::Value, now| {
        let operation: Operation = serde_json::from_value(payload).unwrap();
        assert!(is_authorized(caller, &operation));
        execute(store, operation, now).unwrap()
    };
    let assert_policy = |store: &mut HistoryStore, saved, effective, active, sessions| {
        // This getter supplies the settings RPC readback; status exposes both
        // the saved policy and the decision currently used at admission.
        assert_eq!(store.settings().0.enabled, saved);
        assert_eq!(store.settings().1, effective);
        let status = call(
            store,
            MANAGEMENT_CALLER,
            json!({"action":"history_status"}),
            0,
        );
        assert_eq!(status["status"]["enabled"], saved);
        assert_eq!(status["status"]["effective_enabled"], effective);
        assert_eq!(status["status"]["active_streams"], active);
        assert_eq!(status["status"]["sessions"], sessions);
    };
    let policy = |enabled| HistorySettings {
        enabled,
        ttl_days: 0,
        db_size_limit_mb: 0,
    };

    let mut store = HistoryStore::open(&database, 100).unwrap();
    store.apply_configuration(policy(true), 100).unwrap();
    assert_eq!(
        call(
            &mut store,
            STREAM_SESSION_CALLER,
            json!({
                "action":"session_begin",
                "metadata":{"session_id":"recorded","protocol":"sunshine-modern"},
            }),
            100
        )["recorded"],
        true
    );
    store.apply_configuration(policy(false), 101).unwrap();
    assert_policy(&mut store, false, true, 1, 1);
    assert_eq!(
        call(
            &mut store,
            STREAM_SESSION_CALLER,
            json!({
                "action":"record_event",
                "event":{"session_id":"recorded","recorded_at_unix":102,"event_type":"still_recording"},
            }),
            102
        )["recorded"],
        true
    );
    drop(store);

    let mut store = HistoryStore::open(&database, 110).unwrap();
    store.apply_configuration(policy(false), 111).unwrap();
    assert_policy(&mut store, false, false, 0, 1);
    assert!(
        call(
            &mut store,
            MANAGEMENT_CALLER,
            json!({"action":"active_sessions","limit":16}),
            111,
        )["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let detail = call(
        &mut store,
        MANAGEMENT_CALLER,
        json!({
            "action":"session_detail","session_id":"recorded"
        }),
        111,
    );
    assert_eq!(detail["detail"]["session"]["ended_at_unix"], 111);
    assert_eq!(
        detail["detail"]["events"][0]["event_type"],
        "stream_started"
    );
    assert_eq!(
        detail["detail"]["events"][1]["event_type"],
        "still_recording"
    );
    assert_eq!(
        detail["detail"]["events"][2]["event_type"],
        "stream_interrupted"
    );
    assert_eq!(detail["detail"]["event_count"], 3);
    assert_eq!(
        call(
            &mut store,
            STREAM_SESSION_CALLER,
            json!({
                "action":"session_end","session_id":"recorded"
            }),
            112
        )["recorded"],
        false
    );
    assert_eq!(
        call(
            &mut store,
            STREAM_SESSION_CALLER,
            json!({
                "action":"session_begin",
                "metadata":{"session_id":"unrecorded","protocol":"sunshine-modern"},
            }),
            112
        )["recorded"],
        false
    );
    store.apply_configuration(policy(true), 113).unwrap();
    assert_policy(&mut store, true, false, 1, 1);
    assert_eq!(
        call(
            &mut store,
            STREAM_SESSION_CALLER,
            json!({
                "action":"session_begin",
                "metadata":{"session_id":"also_unrecorded","protocol":"sunshine-modern"},
            }),
            114
        )["recorded"],
        false
    );
    assert_policy(&mut store, true, false, 2, 1);
    drop(store);

    let mut store = HistoryStore::open(&database, 120).unwrap();
    store.apply_configuration(policy(true), 121).unwrap();
    assert_policy(&mut store, true, true, 0, 1);
    let detail = call(
        &mut store,
        MANAGEMENT_CALLER,
        json!({
            "action":"session_detail","session_id":"recorded"
        }),
        121,
    );
    assert_eq!(detail["detail"]["event_count"], 3);
    for session_id in ["unrecorded", "also_unrecorded"] {
        assert!(matches!(
            store.session_detail(session_id),
            Err(HistoryError::NotFound)
        ));
    }
    assert_eq!(
        call(
            &mut store,
            STREAM_SESSION_CALLER,
            json!({
                "action":"session_begin",
                "metadata":{"session_id":"fresh","protocol":"sunshine-modern"},
            }),
            122
        )["recorded"],
        true
    );
    assert_eq!(
        call(
            &mut store,
            STREAM_SESSION_CALLER,
            json!({
                "action":"session_end","session_id":"fresh"
            }),
            123
        )["recorded"],
        true
    );
    assert_policy(&mut store, true, true, 0, 2);
    drop(store);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn producer_updates_remain_ordered_and_readable_after_restart() {
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-history-rpc-{}-{}",
        std::process::id(),
        amaterasu_storage::random_identifier().unwrap()
    ));
    let database = directory.join("history.db");
    let producer_call = |store: &mut HistoryStore, payload| {
        let operation: Operation = serde_json::from_value(payload).unwrap();
        assert!(is_authorized(STREAM_SESSION_CALLER, &operation));
        execute(store, operation, 200).unwrap()
    };

    {
        let mut store = HistoryStore::open(&database, 100).unwrap();
        store
            .apply_configuration(HistorySettings::default(), 100)
            .unwrap();
        assert_eq!(
            producer_call(
                &mut store,
                json!({
                    "action":"session_begin",
                    "metadata":{
                        "session_id":"stream-session-000000000000002a",
                        "protocol":"sunshine-modern",
                        "client_name":"viewer",
                        "device_name":"display",
                        "app_name":"",
                        "width":1920,
                        "height":1080,
                        "target_fps":60,
                        "target_bitrate_kbps":20000,
                        "codec":""
                    },
                    "started_at_unix":100
                }),
            )["recorded"],
            true
        );
        for (timestamp, frames) in [(110, 60), (109, 120)] {
            assert_eq!(
                producer_call(
                    &mut store,
                    json!({
                        "action":"record_sample",
                        "sample":{
                            "session_id":"stream-session-000000000000002a",
                            "recorded_at_unix":timestamp,
                            "frames_sent":frames,
                            "bytes_sent":frames * 1000,
                            "dropped_frames":0,
                            "actual_fps":60.0,
                            "actual_bitrate_kbps":480.0,
                            "encode_latency_ms":null
                        }
                    }),
                )["recorded"],
                true
            );
        }
        assert_eq!(
            producer_call(
                &mut store,
                json!({
                    "action":"record_event",
                    "event":{
                        "session_id":"stream-session-000000000000002a",
                        "recorded_at_unix":111,
                        "event_type":"stream_stopped",
                        "payload":{"state":"stopped"}
                    }
                }),
            )["recorded"],
            true
        );
        assert_eq!(
            producer_call(
                &mut store,
                json!({
                    "action":"session_end",
                    "session_id":"stream-session-000000000000002a",
                    "ended_at_unix":112
                }),
            )["recorded"],
            true
        );
        assert_eq!(
            producer_call(
                &mut store,
                json!({
                    "action":"session_end",
                    "session_id":"stream-session-000000000000002a",
                    "ended_at_unix":112
                }),
            )["recorded"],
            false
        );
    }

    let mut reopened = HistoryStore::open(&database, 120).unwrap();
    reopened
        .apply_configuration(HistorySettings::default(), 120)
        .unwrap();
    let list = execute(
        &mut reopened,
        serde_json::from_value(json!({"action":"list_sessions","limit":1,"offset":0})).unwrap(),
        120,
    )
    .unwrap();
    assert_eq!(
        list["sessions"][0]["session_id"],
        "stream-session-000000000000002a"
    );
    assert_eq!(list["next_offset"], serde_json::Value::Null);
    let detail = execute(
        &mut reopened,
        serde_json::from_value(json!({
            "action":"session_detail",
            "session_id":"stream-session-000000000000002a"
        }))
        .unwrap(),
        120,
    )
    .unwrap();
    assert_eq!(detail["detail"]["sample_count"], 2);
    assert_eq!(detail["detail"]["event_count"], 3);
    assert_eq!(detail["detail"]["samples"][0]["recorded_at_unix"], 110);
    assert_eq!(detail["detail"]["samples"][1]["recorded_at_unix"], 109);
    assert_eq!(detail["detail"]["events"][1]["payload"]["state"], "stopped");
    assert_eq!(detail["detail"]["events"][2]["event_type"], "stream_ended");
    drop(reopened);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn delayed_end_uses_processing_time_for_ttl_without_changing_saved_end_time() {
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-history-retention-{}-{}",
        std::process::id(),
        amaterasu_storage::random_identifier().unwrap()
    ));
    let database = directory.join("history.db");
    let mut store = HistoryStore::open(&database, 190_000).unwrap();
    store
        .apply_configuration(HistorySettings::default(), 190_000)
        .unwrap();
    for (id, started_at) in [("saved", 190_000), ("delayed", 195_000)] {
        let begin: Operation = serde_json::from_value(json!({
            "action":"session_begin",
            "metadata":{"session_id":id,"protocol":"sunshine-modern"},
            "started_at_unix":started_at
        }))
        .unwrap();
        assert_eq!(
            execute(&mut store, begin, started_at).unwrap()["recorded"],
            true
        );
    }
    let saved_end: Operation = serde_json::from_value(json!({
        "action":"session_end","session_id":"saved","ended_at_unix":190_010
    }))
    .unwrap();
    assert_eq!(
        execute(&mut store, saved_end, 190_010).unwrap()["recorded"],
        true
    );

    let policy = HistorySettings {
        enabled: true,
        ttl_days: 1,
        db_size_limit_mb: 0,
    };
    store.apply_configuration(policy.clone(), 200_000).unwrap();
    let delayed_end: Operation = serde_json::from_value(json!({
        "action":"session_end","session_id":"delayed","ended_at_unix":400_000
    }))
    .unwrap();
    assert_eq!(
        execute(&mut store, delayed_end, 200_000).unwrap()["recorded"],
        true
    );
    assert_eq!(
        store.session_detail("saved").unwrap().session.ended_at_unix,
        Some(190_010)
    );
    assert_eq!(
        store
            .session_detail("delayed")
            .unwrap()
            .session
            .ended_at_unix,
        Some(400_000)
    );

    drop(store);
    let mut reopened = HistoryStore::open(&database, 200_010).unwrap();
    reopened.apply_configuration(policy, 200_010).unwrap();
    let list: Operation =
        serde_json::from_value(json!({"action":"list_sessions","limit":2})).unwrap();
    let response = execute(&mut reopened, list, 200_010).unwrap();
    assert_eq!(response["sessions"].as_array().unwrap().len(), 2);
    assert_eq!(response["next_offset"], serde_json::Value::Null);

    let stale_begin: Operation = serde_json::from_value(json!({
        "action":"session_begin",
        "metadata":{"session_id":"stale","protocol":"sunshine-modern"},
        "started_at_unix":100
    }))
    .unwrap();
    assert_eq!(
        execute(&mut reopened, stale_begin, 200_010).unwrap()["recorded"],
        true
    );
    let stale_end: Operation = serde_json::from_value(json!({
        "action":"session_end","session_id":"stale","ended_at_unix":101
    }))
    .unwrap();
    assert_eq!(
        execute(&mut reopened, stale_end, 200_010).unwrap()["recorded"],
        true
    );
    assert!(matches!(
        reopened.session_detail("stale"),
        Err(store::HistoryError::NotFound)
    ));
    drop(reopened);

    let mut reopened = HistoryStore::open(&database, 200_020).unwrap();
    reopened
        .apply_configuration(
            HistorySettings {
                enabled: true,
                ttl_days: 1,
                db_size_limit_mb: 0,
            },
            200_020,
        )
        .unwrap();
    assert_eq!(reopened.status().unwrap().sessions, 2);
    assert_eq!(
        reopened
            .session_detail("saved")
            .unwrap()
            .session
            .ended_at_unix,
        Some(190_010)
    );
    assert_eq!(
        reopened
            .session_detail("delayed")
            .unwrap()
            .session
            .ended_at_unix,
        Some(400_000)
    );
    drop(reopened);
    fs::remove_dir_all(directory).unwrap();
}
