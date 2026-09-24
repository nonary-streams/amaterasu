use super::*;
use model::HistorySettings;
use serde_json::json;

fn operation(action: &str) -> Operation {
    let mut payload = json!({"action": action});
    match action {
        "settings_update" => {
            payload["settings"] = json!({"enabled":true,"ttl_days":0,"db_size_limit_mb":0});
            payload["expected_revision"] = json!(1);
        }
        "session_begin" => payload["metadata"] = json!({"session_id":"id","protocol":"rtsp"}),
        "session_end" | "session_detail" | "delete_session" => {
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
fn status_reports_storage_unavailability_without_failing_the_module() {
    let response = unavailable_status();
    assert_eq!(response["ok"], true);
    assert_eq!(response["status"]["available"], false);
    assert_eq!(response["status"]["degraded"], true);
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
