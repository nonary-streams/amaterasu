use super::*;

const HISTORY_SERVICE: &str = "amaterasu.session-history/1";

fn prepare_and_start(host: &mut Harness) -> u64 {
    let prepared = host.response(1, prepare());
    let session = prepared["session"].as_u64().expect("prepared session id");
    assert_eq!(host.response(2, start(session))["state"], "active");
    session
}

fn history_call_index(host: &Harness, action: &str) -> usize {
    host.broker
        .calls
        .iter()
        .position(|call| call.service == HISTORY_SERVICE && call.payload["action"] == action)
        .expect("history request was admitted")
}

#[test]
fn active_session_is_recorded_once_with_bounded_metadata_and_ended_after_teardown() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.broker.history_recording_disabled = true;
    let session = prepare_and_start(&mut host);

    host.idle();
    host.idle();

    assert_eq!(host.broker.history_begins.len(), 1);
    let begin = host.broker.history_begins[0].clone();
    assert_eq!(begin["action"], "session_begin");
    assert!(begin["metadata"]["session_id"].as_str().unwrap().len() <= 128);
    assert_eq!(begin["metadata"]["protocol"], "sunshine-modern");
    assert_eq!(begin["metadata"]["client_name"], "owner");
    assert_eq!(begin["metadata"]["device_name"], "monitor-one");
    assert_eq!(begin["metadata"]["width"], 1280);
    assert_eq!(begin["metadata"]["height"], 720);
    assert_eq!(begin["metadata"]["target_fps"], 60);
    assert_eq!(begin["metadata"]["target_bitrate_kbps"], 10_000);
    assert_eq!(begin["metadata"]["session_id"].as_str().unwrap().len(), 31);
    assert!(begin["started_at_unix"].as_i64().is_some());

    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    host.idle();
    host.idle();

    assert_eq!(host.broker.history_ends.len(), 1);
    assert_eq!(
        host.broker.history_ends[0]["session_id"],
        begin["metadata"]["session_id"]
    );
    assert!(
        host.broker.history_ends[0]["ended_at_unix"]
            .as_i64()
            .is_some()
    );
    let begin_call = history_call_index(&host, "session_begin");
    let finish_call = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == Resource::Session.service() && call.payload["action"] == "finish_stop"
        })
        .expect("confirmed FinishSession request");
    let end_call = history_call_index(&host, "session_end");
    assert!(begin_call < finish_call && finish_call < end_call);
    assert_eq!(host.broker.history_released, 2);
    assert!(host.broker.history_requests.is_empty());
}

#[test]
fn missing_history_service_does_not_delay_activation_or_teardown() {
    let mut host = Harness::new();
    let session = prepare_and_start(&mut host);
    let start = host.now;

    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );

    assert!(host.now.saturating_sub(start) < 100);
    assert_eq!(host.broker.live_components(), 0);
    assert!(host.broker.history_begins.is_empty());
    assert!(host.broker.history_ends.is_empty());
    assert!(host.broker.history_requests.is_empty());
}

#[test]
fn slow_begin_result_does_not_hold_finish_session_or_send_end_out_of_order() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.broker.history_pending_polls = usize::MAX;
    let session = prepare_and_start(&mut host);

    host.tick(); // Admit Begin.
    host.tick(); // Begin result remains pending.
    assert_eq!(host.broker.history_requests.len(), 1);
    assert!(host.broker.history_begins.is_empty());

    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    assert_eq!(host.broker.live_components(), 0);
    assert!(host.broker.history_ends.is_empty());
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(|call| call.service == HISTORY_SERVICE)
            .count(),
        1
    );

    host.broker.history_pending_polls = 0;
    host.idle();
    host.idle();
    assert_eq!(host.broker.history_begins.len(), 1);
    assert_eq!(host.broker.history_ends.len(), 1);
    assert!(history_call_index(&host, "session_begin") < history_call_index(&host, "session_end"));
    assert_eq!(host.broker.history_released, 2);
    assert!(host.broker.history_requests.is_empty());
}

#[test]
fn unresolved_begin_is_abandoned_after_its_deadline_without_delaying_cleanup() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.broker.history_pending_polls = usize::MAX;
    let session = prepare_and_start(&mut host);

    host.tick();
    host.tick();
    assert_eq!(host.broker.history_requests.len(), 1);
    let stop_started_at = host.now;
    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    assert!(host.now.saturating_sub(stop_started_at) < 100);

    host.now = host.now.saturating_add(6_000);
    host.idle();

    assert!(host.broker.history_ends.is_empty());
    assert!(host.broker.history_requests.is_empty());
    assert_eq!(host.broker.live_components(), 0);
}

#[test]
fn storage_error_result_is_released_and_does_not_stop_stream_or_send_end() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.broker.history_storage_degraded = true;
    let session = prepare_and_start(&mut host);

    host.idle();
    assert_eq!(host.pipeline.records[&session].state, State::Active);
    assert_eq!(host.broker.history_begins.len(), 1);
    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    host.idle();

    assert!(host.broker.history_ends.is_empty());
    assert_eq!(host.broker.history_released, 1);
    assert!(host.broker.history_requests.is_empty());
    assert_eq!(host.broker.live_components(), 0);
}

#[test]
fn lost_begin_result_is_not_retried_or_ended_as_known_success() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.broker.fault = Some((HISTORY_SERVICE.into(), Fault::AfterMutation));
    let session = prepare_and_start(&mut host);

    host.idle();
    host.idle();

    assert_eq!(host.broker.history_begins.len(), 1);
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(
                |call| call.service == HISTORY_SERVICE && call.payload["action"] == "session_begin"
            )
            .count(),
        1
    );
    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    host.idle();
    assert!(host.broker.history_ends.is_empty());
    assert_eq!(host.broker.history_released, 1);
    assert!(host.broker.history_requests.is_empty());
}
