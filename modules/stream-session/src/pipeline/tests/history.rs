use super::*;

const HISTORY_SERVICE: &str = "amaterasu.session-history/1";

#[test]
fn reserved_activity_binds_signed_prepare_and_history_session_id() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    let reserve = json!({"action":"reserve_history_activity", "activity_id":"launch-activity",
        "rtsp_operation_id":"launch-activity"});
    assert_eq!(
        host.response_from(1, "other", reserve.clone())["code"],
        "permission_denied"
    );
    assert_eq!(
        host.response_from(2, "amaterasu.nvhttp", reserve.clone())["reserved"],
        true
    );
    assert_eq!(
        host.response_from(3, "amaterasu.nvhttp", reserve)["reserved"],
        true
    );
    assert_eq!(host.broker.history_activities.len(), 1);

    let mut tagged = prepare();
    tagged["history_activity_id"] = json!("launch-activity");
    tagged["rtsp_operation_id"] = json!("launch-activity");
    assert_eq!(
        host.response_from(4, "owner", tagged.clone())["code"],
        "permission_denied"
    );
    let mut wrong = tagged.clone();
    wrong["rtsp_operation_id"] = json!("wrong-operation");
    assert_eq!(
        host.response_from(5, "amaterasu.rtsp", wrong)["code"],
        "invalid_history_activity"
    );
    let prepared = host.response_from(6, "amaterasu.rtsp", tagged.clone());
    assert_eq!(prepared["history_activity_id"], "launch-activity");
    assert_eq!(prepared["rtsp_operation_id"], "launch-activity");
    let session = prepared["session"].as_u64().unwrap();
    assert_eq!(
        host.response_from(7, "amaterasu.rtsp", tagged)["session"],
        session
    );
    assert_eq!(
        host.response_from(
            8,
            "amaterasu.nvhttp",
            json!({"action":"release_history_activity",
        "activity_id":"launch-activity","rtsp_operation_id":"launch-activity"})
        )["code"],
        "history_activity_in_use"
    );
    assert_eq!(
        host.response_from(9, "amaterasu.rtsp", start(session))["state"],
        "active"
    );
    host.idle();
    assert_eq!(
        host.broker.history_begins[0]["metadata"]["session_id"],
        "launch-activity"
    );
    assert_eq!(host.broker.host_metrics_activities.len(), 1);
    assert!(
        host.broker
            .host_metrics_activities
            .contains(&format!("stream-session-{session:016x}"))
    );
    assert_eq!(host.broker.updater_session_starts.len(), 1);
    assert_eq!(
        host.broker.updater_session_starts[0]["session_id"],
        format!("stream-session-{session:016x}")
    );
    assert_eq!(host.broker.updater_active_sessions.len(), 1);
    assert_eq!(host.broker.tray_activity_updates[0]["state"], "playing");
    assert_eq!(
        host.response_from(
            10,
            "amaterasu.rtsp",
            json!({"action":"stop","session":session})
        )["state"],
        "stopped"
    );
    host.idle();
    assert_eq!(host.broker.history_ends[0]["session_id"], "launch-activity");
    assert!(host.broker.host_metrics_activities.is_empty());
    assert_eq!(host.broker.host_metrics_activity_begins.len(), 1);
    assert_eq!(host.broker.host_metrics_activity_ends.len(), 1);
    assert_eq!(host.broker.updater_session_stops.len(), 1);
    assert!(host.broker.updater_active_sessions.is_empty());
    assert_eq!(host.broker.tray_activity_updates[1]["state"], "stopped");
    let release = json!({"action":"release_history_activity", "activity_id":"launch-activity",
        "rtsp_operation_id":"launch-activity"});
    assert_eq!(
        host.response_from(11, "amaterasu.nvhttp", release.clone())["released"],
        true
    );
    assert_eq!(
        host.response_from(12, "amaterasu.nvhttp", release)["released"],
        false
    );
    assert!(host.broker.history_activities.is_empty());
}

#[test]
fn host_metrics_consumer_tracks_only_activated_sessions_and_is_best_effort() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare());
    let session = prepared["session"].as_u64().unwrap();
    assert!(host.broker.host_metrics_activity_begins.is_empty());

    host.broker.host_metrics_available = false;
    assert_eq!(host.response(2, start(session))["state"], "active");
    host.idle();
    assert!(host.broker.host_metrics_activity_begins.is_empty());
    assert!(host.broker.host_metrics_activities.is_empty());
    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    host.idle();
    assert!(host.broker.host_metrics_activity_ends.is_empty());
}

#[test]
fn host_metrics_lease_is_released_during_stream_session_shutdown() {
    let mut host = Harness::new();
    let session = prepare_and_start(&mut host);
    host.idle();
    assert_eq!(host.broker.host_metrics_activities.len(), 1);

    let settled = {
        let (pipeline, broker) = (&mut host.pipeline, &mut host.broker);
        pipeline.shutdown_with(&mut |command| broker.send(command))
    };

    assert!(settled);
    assert!(host.broker.host_metrics_activities.is_empty());
    assert_eq!(host.broker.host_metrics_activity_ends.len(), 1);
    assert_eq!(
        host.broker.host_metrics_activity_ends[0]["session_id"],
        format!("stream-session-{session:016x}")
    );
}

#[test]
fn unavailable_reservation_is_nonfatal_and_cannot_prepare_tagged_stream() {
    let mut host = Harness::new();
    let reserve = json!({"action":"reserve_history_activity", "activity_id":"activity",
        "rtsp_operation_id":"activity"});
    assert_eq!(
        host.response_from(1, "amaterasu.nvhttp", reserve)["reserved"],
        false
    );
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    let mut tagged = prepare();
    tagged["history_activity_id"] = json!("activity");
    tagged["rtsp_operation_id"] = json!("activity");
    assert_eq!(
        host.response_from(2, "amaterasu.rtsp", tagged)["code"],
        "history_activity_unavailable"
    );
    assert_eq!(
        host.response_from(3, "owner", prepare())["state"],
        "prepared"
    );
}

#[test]
fn active_activity_outlives_pending_ttl_and_release_waits_for_history_end() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    let activity = "rtsp-long-running";
    assert_eq!(
        host.response_from(
            1,
            "amaterasu.nvhttp",
            json!({"action":"reserve_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity})
        )["reserved"],
        true
    );
    let mut tagged = prepare();
    tagged["history_activity_id"] = json!(activity);
    tagged["rtsp_operation_id"] = json!(activity);
    let prepared = host.response_from(2, "amaterasu.rtsp", tagged);
    let session = prepared["session"].as_u64().unwrap();
    assert_eq!(
        host.response_from(3, "amaterasu.rtsp", start(session))["state"],
        "active"
    );
    host.now += 600_100;
    host.tick();
    assert!(host.broker.history_activities.contains(activity));
    assert_eq!(
        host.response_from(
            4,
            "amaterasu.nvhttp",
            json!({"action":"release_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity})
        )["code"],
        "history_activity_in_use"
    );
    host.broker.history_pending_polls = usize::MAX;
    assert_eq!(
        host.response_from(
            5,
            "amaterasu.rtsp",
            json!({"action":"stop","session":session})
        )["state"],
        "stopped"
    );
    assert_eq!(
        host.response_from(
            6,
            "amaterasu.nvhttp",
            json!({"action":"release_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity})
        )["code"],
        "history_activity_in_use"
    );
    host.broker.history_pending_polls = 0;
    host.idle();
    assert_eq!(
        host.response_from(
            7,
            "amaterasu.nvhttp",
            json!({"action":"release_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity})
        )["released"],
        true
    );
    assert!(
        history_call_index(&host, "session_end") < history_call_index(&host, "stream_activity_end")
    );
}

#[test]
fn abandoned_uncertain_begin_is_ended_after_late_completion() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    let activity = "rtsp-lost-begin";
    host.broker.fault = Some((
        format!("{HISTORY_SERVICE}:stream_activity_begin"),
        Fault::AfterMutation,
    ));
    host.broker.enqueue(
        1,
        "amaterasu.nvhttp",
        json!({"action":"reserve_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity}),
    );
    for _ in 0..5 {
        host.tick();
    }
    assert!(host.broker.history_activities.contains(activity));
    host.now += 10_001;
    host.tick();
    assert_eq!(host.broker.replies.remove(&1).unwrap()["reserved"], false);
    host.broker.enqueue(
        2,
        "amaterasu.nvhttp",
        json!({"action":"release_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity}),
    );
    for _ in 0..8 {
        host.now += 1_001;
        host.tick();
        if host.broker.replies.contains_key(&2) {
            break;
        }
    }
    assert_eq!(host.broker.replies.remove(&2).unwrap()["released"], true);
    assert!(host.broker.history_activities.is_empty());
    assert!(
        history_call_index(&host, "stream_activity_begin")
            < history_call_index(&host, "stream_activity_end")
    );
}

#[test]
fn pending_end_timeout_is_not_reported_as_confirmed_release() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    let activity = "rtsp-slow-end";
    assert_eq!(
        host.response_from(
            1,
            "amaterasu.nvhttp",
            json!({"action":"reserve_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity})
        )["reserved"],
        true
    );
    host.broker.history_pending_polls = usize::MAX;
    host.broker.enqueue(
        2,
        "amaterasu.nvhttp",
        json!({"action":"release_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity}),
    );
    host.tick();
    host.tick();
    host.now += 10_001;
    host.tick();
    let timeout = host
        .broker
        .replies
        .remove(&2)
        .expect("bounded release response");
    assert_eq!(timeout["ok"], false);
    assert_eq!(timeout["code"], "history_activity_end_unconfirmed");
    assert!(host.broker.history_activities.contains(activity));
    host.broker.history_pending_polls = 0;
    host.idle();
    assert!(host.broker.history_activities.is_empty());
    assert_eq!(
        host.response_from(
            3,
            "amaterasu.nvhttp",
            json!({"action":"release_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity})
        )["released"],
        false
    );
}

#[test]
fn unresolved_begin_is_settled_before_expiry_end() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.broker.history_pending_polls = usize::MAX;
    host.broker.fail_cancel = true;
    let activity = "rtsp-delayed-begin";
    host.broker.enqueue(
        1,
        "amaterasu.nvhttp",
        json!({"action":"reserve_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity}),
    );
    host.tick();
    host.tick();
    host.now += 300_001;
    host.tick();
    assert_eq!(host.broker.replies.remove(&1).unwrap()["reserved"], false);
    assert!(
        host.broker
            .calls
            .iter()
            .any(|call| call.payload["action"] == "stream_activity_begin")
    );
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.payload["action"] == "stream_activity_end")
    );
    host.broker.fail_cancel = false;
    host.broker.history_pending_polls = 0;
    host.idle();
    assert!(
        host.broker
            .calls
            .iter()
            .any(|call| call.payload["action"] == "stream_activity_end")
    );
    assert!(host.broker.history_activities.is_empty());
}

#[test]
fn completed_stream_without_explicit_release_frees_bounded_reservation_slot() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    let activity = "rtsp-completed";
    assert_eq!(
        host.response_from(
            1,
            "amaterasu.nvhttp",
            json!({"action":"reserve_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity})
        )["reserved"],
        true
    );
    let mut tagged = prepare();
    tagged["history_activity_id"] = json!(activity);
    tagged["rtsp_operation_id"] = json!(activity);
    let session = host.response_from(2, "amaterasu.rtsp", tagged)["session"]
        .as_u64()
        .unwrap();
    assert_eq!(
        host.response_from(3, "amaterasu.rtsp", start(session))["state"],
        "active"
    );
    host.idle();
    assert_eq!(
        host.response_from(
            4,
            "amaterasu.rtsp",
            json!({"action":"stop","session":session})
        )["state"],
        "stopped"
    );
    host.idle();
    host.now += 300_001;
    host.tick();
    host.idle();
    assert!(
        host.broker
            .history_activity_ends
            .iter()
            .any(|call| call["session_id"] == activity)
    );
    host.now += 300_001;
    host.tick();
    assert!(
        !host
            .pipeline
            .history_activities
            .matches(activity, activity, host.now)
    );
}

#[test]
fn expired_end_does_not_strand_waiting_release_caller() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    let activity = "rtsp-expired-end";
    assert_eq!(
        host.response_from(
            1,
            "amaterasu.nvhttp",
            json!({"action":"reserve_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity})
        )["reserved"],
        true
    );
    host.broker.history_pending_polls = usize::MAX;
    host.broker.enqueue(
        2,
        "amaterasu.nvhttp",
        json!({"action":"release_history_activity",
        "activity_id":activity,"rtsp_operation_id":activity}),
    );
    host.tick();
    host.tick();
    assert_eq!(host.broker.history_requests.len(), 1);
    host.broker.fail_cancel = true;
    host.now += 600_001;
    host.tick();
    assert_eq!(
        host.broker.replies.remove(&2).unwrap()["code"],
        "history_activity_end_unconfirmed"
    );
    assert_eq!(host.broker.history_requests.len(), 1);
    host.broker.fail_cancel = false;
    host.tick();
    assert!(host.broker.history_requests.is_empty());
}

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

fn history_event_call_index(host: &Harness, event_type: &str) -> usize {
    host.broker
        .calls
        .iter()
        .position(|call| {
            call.service == HISTORY_SERVICE
                && call.payload["action"] == "record_event"
                && call.payload["event"]["event_type"] == event_type
        })
        .expect("history lifecycle event was admitted")
}

#[test]
fn active_session_is_recorded_once_with_bounded_metadata_and_ended_after_teardown() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.broker.history_recording_disabled = true;
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    let mut start_request = start(session);
    start_request["configuration"]["device_name"] = json!("Studio PC");
    assert_eq!(host.response(2, start_request)["state"], "active");

    host.idle();
    host.idle();

    assert_eq!(host.broker.history_begins.len(), 1);
    let begin = host.broker.history_begins[0].clone();
    assert_eq!(begin["action"], "session_begin");
    assert!(begin["metadata"]["session_id"].as_str().unwrap().len() <= 128);
    assert_eq!(begin["metadata"]["protocol"], "sunshine-modern");
    assert_eq!(begin["metadata"]["client_name"], "owner");
    assert_eq!(begin["metadata"]["device_name"], "Studio PC");
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
    let limits_call = history_event_call_index(&host, "telemetry_limits");
    let stopping_call = history_event_call_index(&host, "stream_stopping");
    let stopped_call = history_event_call_index(&host, "stream_stopped");
    assert!(begin_call < limits_call);
    assert!(stopping_call < finish_call && finish_call < stopped_call && stopped_call < end_call);
    assert_eq!(host.broker.history_released, host.broker.history_calls());
    assert!(host.broker.history_requests.is_empty());
    assert!(
        host.broker
            .calls
            .iter()
            .filter(|call| call.service == HISTORY_SERVICE)
            .all(|call| call.parent.is_none())
    );
}

#[test]
fn history_uses_capture_output_for_legacy_starts_without_a_device_name() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    let _session = prepare_and_start(&mut host);

    host.idle();

    assert_eq!(
        host.broker.history_begins[0]["metadata"]["device_name"],
        "monitor-one"
    );
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
    assert_eq!(host.broker.history_begins.len(), 0);

    host.broker.history_pending_polls = 0;
    host.idle();
    host.idle();
    assert_eq!(host.broker.history_begins.len(), 1);
    assert_eq!(host.broker.history_ends.len(), 1);
    assert!(history_call_index(&host, "session_begin") < history_call_index(&host, "session_end"));
    assert_eq!(host.broker.history_released, host.broker.history_calls());
    assert!(host.broker.history_requests.is_empty());
}

#[test]
fn timed_out_begin_is_settled_then_reconciled_with_an_ordered_end_without_delaying_stop() {
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
    host.tick(); // Observe the timeout and cancel the same Begin operation.
    host.tick(); // Settle and release the outstanding Begin request.
    assert!(host.broker.history_requests.is_empty());

    host.broker.history_pending_polls = 0;
    host.idle();

    assert_eq!(host.broker.history_begins.len(), 0);
    assert_eq!(host.broker.history_ends.len(), 1);
    assert!(
        history_call_index(&host, "session_begin")
            < history_event_call_index(&host, "stream_stopping")
    );
    assert!(
        history_event_call_index(&host, "stream_stopped")
            < history_call_index(&host, "session_end")
    );
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
    assert_eq!(host.broker.history_released, host.broker.history_calls());
    assert!(host.broker.history_requests.is_empty());
    assert_eq!(host.broker.live_components(), 0);
}

#[test]
fn failed_sample_and_event_results_are_released_without_stopping_streaming() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.broker
        .history_failed_actions
        .extend(["record_event".into(), "record_sample".into()]);
    let session = prepare_and_start(&mut host);
    host.idle();
    assert_eq!(host.pipeline.records[&session].state, State::Active);
    {
        let mut plane = host.plane.borrow_mut();
        plane.accepted_video_access_units = 2;
        plane.accepted_media_payload_bytes = 640;
        plane.observed_video_sequence_gaps = 1;
    }
    host.now = host.now.saturating_add(1_000);
    host.tick();
    host.idle();

    assert_eq!(host.pipeline.records[&session].state, State::Active);
    assert_eq!(
        host.broker.history_events[0]["event_type"],
        "telemetry_limits"
    );
    assert_eq!(host.broker.history_samples.len(), 1);
    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    host.idle();
    assert_eq!(host.broker.history_ends.len(), 1);
    assert_eq!(host.broker.history_released, host.broker.history_calls());
    assert!(host.broker.history_requests.is_empty());
}

#[test]
fn lost_begin_result_is_not_replayed_and_uncertain_commit_is_ended_after_stop() {
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
    assert_eq!(host.broker.history_ends.len(), 1);
    assert_eq!(host.broker.history_released, host.broker.history_calls());
    assert!(host.broker.history_requests.is_empty());
}

#[test]
fn sample_payload_uses_exact_contract_fields_and_local_admission_counter_rates() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    let session = prepare_and_start(&mut host);
    let activation_at = host.now;
    host.tick(); // Admit session_begin.
    host.tick(); // Complete session_begin.
    host.tick(); // Admit the telemetry limits event.
    {
        let mut plane = host.plane.borrow_mut();
        plane.accepted_video_access_units = 4;
        plane.accepted_media_payload_bytes = 1_200;
        plane.observed_video_sequence_gaps = 1;
    }
    host.now = activation_at.saturating_add(999);
    host.tick(); // Consume counters and complete the telemetry limits event.
    host.tick();
    host.idle();

    assert_eq!(host.broker.history_samples.len(), 1);
    let sample = &host.broker.history_samples[0];
    let expected_fields = [
        "session_id",
        "recorded_at_unix",
        "frames_sent",
        "bytes_sent",
        "dropped_frames",
        "actual_fps",
        "actual_bitrate_kbps",
        "encode_latency_ms",
    ];
    let mut actual_fields = sample
        .as_object()
        .expect("sample object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    actual_fields.sort_unstable();
    let mut expected_fields = expected_fields.to_vec();
    expected_fields.sort_unstable();
    assert_eq!(actual_fields, expected_fields);
    assert_eq!(
        sample["session_id"],
        json!(format!("stream-session-{session:016x}"))
    );
    assert!(sample["recorded_at_unix"].as_i64().is_some());
    assert_eq!(sample["frames_sent"], 4);
    assert_eq!(sample["bytes_sent"], 1_200);
    assert_eq!(sample["dropped_frames"], 1);
    assert!(sample["actual_fps"].as_f64().unwrap().is_finite());
    assert!((sample["actual_fps"].as_f64().unwrap() - 4.0).abs() < 0.01);
    assert!((sample["actual_bitrate_kbps"].as_f64().unwrap() - 9.6).abs() < 0.02);
    assert!(sample["encode_latency_ms"].is_null());
    assert_eq!(
        host.broker.history_events[0]["event_type"],
        "telemetry_limits"
    );
    assert_eq!(
        host.broker.history_events[0]["payload"]["transport_acceptance_is_client_delivery"],
        false
    );
    assert!(history_call_index(&host, "session_begin") < history_call_index(&host, "record_event"));
    assert!(history_call_index(&host, "record_event") < history_call_index(&host, "record_sample"));
}

#[test]
fn delayed_sample_completion_preserves_a_newer_coalesced_snapshot() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    let _session = prepare_and_start(&mut host);
    host.idle();
    host.now = 0;

    host.plane.borrow_mut().accepted_video_access_units = 4;
    host.tick(); // Consume the first counter delta.
    host.broker.history_pending_polls = usize::MAX;
    host.now = 2_000;
    host.tick(); // Admit sample one and leave its result pending.
    assert_eq!(host.broker.history_samples.len(), 0);
    assert_eq!(host.broker.history_requests.len(), 1);

    host.plane.borrow_mut().accepted_video_access_units = 3;
    host.tick(); // The second delta arrives while sample one is in flight.
    host.now = 3_100;
    host.tick(); // Queue the newer cumulative sample behind sample one.
    assert_eq!(host.broker.history_samples.len(), 0);

    host.broker.history_pending_polls = 0;
    host.idle();

    assert_eq!(host.broker.history_samples.len(), 2);
    assert_eq!(host.broker.history_samples[0]["frames_sent"], 4);
    assert_eq!(host.broker.history_samples[1]["frames_sent"], 7);
    assert_eq!(host.broker.history_released, host.broker.history_calls());
    assert!(host.broker.history_requests.is_empty());
}

#[test]
fn confirmed_short_stream_records_final_counters_with_finite_rates() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    let session = prepare_and_start(&mut host);
    host.now = 0;
    {
        let mut plane = host.plane.borrow_mut();
        plane.accepted_video_access_units = 1;
        plane.accepted_media_payload_bytes = 321;
        plane.observed_video_sequence_gaps = 1;
    }
    host.tick();

    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    host.idle();

    assert_eq!(host.broker.history_samples.len(), 1);
    let sample = &host.broker.history_samples[0];
    assert_eq!(sample["frames_sent"], 1);
    assert_eq!(sample["bytes_sent"], 321);
    assert_eq!(sample["dropped_frames"], 1);
    assert!(sample["actual_fps"].as_f64().unwrap().is_finite());
    assert!(sample["actual_bitrate_kbps"].as_f64().unwrap().is_finite());
    assert_eq!(host.broker.history_ends.len(), 1);
}

#[test]
fn shutdown_after_confirmed_stop_flushes_queued_events_and_end() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    let session = prepare_and_start(&mut host);
    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    // Journal confirmation can let the history pump send End before the stop
    // reply; shutdown must still leave exactly one End.

    let settled = {
        let (pipeline, broker) = (&mut host.pipeline, &mut host.broker);
        pipeline.shutdown_with(&mut |command| broker.send(command))
    };

    assert!(settled);
    assert_eq!(host.broker.history_ends.len(), 1);
    assert!(
        history_call_index(&host, "session_begin")
            < history_event_call_index(&host, "stream_stopping")
    );
    assert!(
        history_event_call_index(&host, "stream_stopping")
            < history_event_call_index(&host, "stream_stopped")
    );
    assert!(
        history_event_call_index(&host, "stream_stopped")
            < history_call_index(&host, "session_end")
    );
    assert_eq!(host.broker.history_released, host.broker.history_calls());
    assert!(host.broker.history_requests.is_empty());
}

#[test]
fn media_admitted_before_a_plane_error_is_kept_in_history_counters() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    let session = prepare_and_start(&mut host);
    host.idle();
    {
        let mut plane = host.plane.borrow_mut();
        plane.accepted_video_access_units = 1;
        plane.accepted_media_payload_bytes = 321;
        plane.observed_video_sequence_gaps = 2;
        plane.failure = Some("video_failed_after_admission");
    }

    host.tick();

    let history = &host.pipeline.records[&session].history;
    assert_eq!(history.observed_counters(), (1, 321, 2));
    assert_eq!(host.pipeline.records[&session].state, State::Stopping);
}

#[test]
fn repeated_start_and_stop_do_not_duplicate_history_lifecycle_requests() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    let session = prepare_and_start(&mut host);
    host.idle();
    assert_eq!(host.response(3, start(session))["state"], "active");
    assert_eq!(host.broker.history_begins.len(), 1);
    host.idle();
    assert_eq!(
        host.response(4, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    assert_eq!(
        host.response(5, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    host.idle();
    assert_eq!(host.broker.history_begins.len(), 1);
    assert_eq!(host.broker.history_ends.len(), 1);
    assert_eq!(
        host.broker
            .history_events
            .iter()
            .filter(|event| event["event_type"] == "stream_stopping")
            .count(),
        1
    );
    assert_eq!(
        host.broker
            .history_events
            .iter()
            .filter(|event| event["event_type"] == "stream_stopped")
            .count(),
        1
    );
}

#[test]
fn failed_result_release_retries_the_same_history_operation_without_replaying_mutation() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.broker.history_release_failures = 1;
    let _session = prepare_and_start(&mut host);
    host.idle();

    assert_eq!(host.broker.history_begins.len(), 1);
    assert!(host.broker.history_release_attempts > host.broker.history_released);
    assert_eq!(host.broker.history_released, host.broker.history_calls());
    assert!(host.broker.history_requests.is_empty());
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(|call| {
                call.service == HISTORY_SERVICE && call.payload["action"] == "session_begin"
            })
            .count(),
        1
    );
}

#[test]
fn shutdown_recovers_a_lost_history_admission_then_cancels_and_releases_it() {
    let mut host = Harness::new();
    host.broker.history_available = true;
    host.broker.fault = Some((HISTORY_SERVICE.into(), Fault::Admission));
    let _session = prepare_and_start(&mut host);
    host.tick();
    assert_eq!(host.broker.history_requests.len(), 1);

    let settled = {
        let (pipeline, broker) = (&mut host.pipeline, &mut host.broker);
        pipeline.shutdown_with(&mut |command| broker.send(command))
    };

    assert!(settled);
    assert!(host.broker.history_requests.is_empty());
    assert_eq!(host.broker.history_calls(), 1);
}
