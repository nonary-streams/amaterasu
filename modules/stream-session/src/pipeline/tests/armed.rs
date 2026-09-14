use super::*;

fn arm(id: u64) -> Value {
    let mut value = start(id);
    value["action"] = json!("start_when_authenticated");
    value
}

#[test]
fn discovered_media_waits_for_both_lanes_without_opening_capture() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare_discovery());
    let id = prepared["session"].as_u64().unwrap();
    let deadline = host.pipeline.records[&id].prepared_until;
    assert_eq!(prepared["media_ready"], false);
    assert_eq!(prepared["peer_discovery"], true);
    assert!(prepared["video_peer_port"].is_null());
    assert!(prepared["audio_peer_port"].is_null());
    assert!(!prepared.to_string().contains("0123456789abcdef"));
    assert!(!prepared.to_string().contains("fedcba9876543210"));
    let media_open = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Media.service())
        .expect("media open");
    assert!(media_open.payload.get("video_port").is_none());
    assert!(media_open.payload.get("audio_port").is_none());
    assert_eq!(
        media_open.payload["discovery"]["video_ping_payload"],
        "0123456789abcdef"
    );

    assert_eq!(host.response(2, arm(id))["state"], "armed");
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Armed);
    assert_eq!(host.pipeline.records[&id].prepared_until, deadline);
    assert_eq!(host.plane.borrow().ticks, 0);
    assert!(!host.broker.calls.iter().any(|call| {
        call.service == Resource::Video.service()
            || call.service == Resource::AudioCapture.service()
    }));

    host.broker.discovered_audio_port = Some(51_002);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Armed);
    assert_eq!(host.plane.borrow().ticks, 0);

    host.broker.discovered_video_port = Some(51_001);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert_eq!(host.pipeline.records[&id].video_peer_port, Some(51_001));
    assert_eq!(host.pipeline.records[&id].audio_peer_port, Some(51_002));
}

#[test]
fn ready_but_failed_media_rolls_back_before_capture() {
    let mut host = Harness::new();
    host.broker.discovered_video_port = Some(51_001);
    host.broker.discovered_audio_port = Some(51_002);
    host.broker.media_failure = true;
    let id = host.response(1, prepare_discovery())["session"]
        .as_u64()
        .unwrap();
    assert_eq!(host.response(2, arm(id))["state"], "armed");
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Failed);
    assert_eq!(host.pipeline.records[&id].error, Some("media_unavailable"));
    assert_eq!(host.plane.borrow().ticks, 0);
    assert!(!host.broker.calls.iter().any(|call| {
        call.service == Resource::Video.service()
            || call.service == Resource::AudioCapture.service()
    }));
}

#[test]
fn discovery_timeout_and_stop_with_one_lane_release_without_capture() {
    let mut timed_out = Harness::new();
    let id = timed_out.response(1, prepare_discovery())["session"]
        .as_u64()
        .unwrap();
    assert_eq!(timed_out.response(2, arm(id))["state"], "armed");
    timed_out.now = timed_out.pipeline.records[&id].prepared_until;
    timed_out.idle();
    assert_eq!(timed_out.pipeline.records[&id].state, State::Failed);
    assert_eq!(
        timed_out.pipeline.records[&id].error,
        Some("prepare_expired")
    );
    assert_eq!(timed_out.broker.live_components(), 0);
    assert_eq!(timed_out.plane.borrow().ticks, 0);

    let mut stopped = Harness::new();
    let id = stopped.response(1, prepare_discovery())["session"]
        .as_u64()
        .unwrap();
    assert_eq!(stopped.response(2, arm(id))["state"], "armed");
    stopped.broker.discovered_audio_port = Some(51_002);
    stopped.idle();
    assert_eq!(stopped.pipeline.records[&id].state, State::Armed);
    assert_eq!(
        stopped.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert_eq!(stopped.broker.live_components(), 0);
    assert_eq!(stopped.plane.borrow().ticks, 0);
}

#[test]
fn armed_wait_has_no_hardware_and_authentication_runs_existing_start_once() {
    let mut host = Harness::new();
    host.broker.authenticated = false;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let deadline = host.pipeline.records[&id].prepared_until;
    assert_eq!(host.response(2, arm(id))["state"], "armed");
    host.broker.cancel_parent(2);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Armed);
    assert_eq!(host.plane.borrow().ticks, 0);
    assert_eq!(host.broker.live_components(), 3);
    assert_eq!(host.pipeline.records[&id].prepared_until, deadline);
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == Resource::Video.service())
    );
    host.broker.authenticated = true;
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert_eq!(host.broker.leases(), 2);
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(|call| call.service == Resource::Video.service()
                && call.payload["action"] == "open")
            .count(),
        1
    );
    assert!(
        host.broker
            .calls
            .iter()
            .filter(|call| matches!(
                call.payload["action"].as_str(),
                Some("acquire" | "activate")
            ) || call.service == Resource::Video.service()
                || call.service == Resource::AudioCapture.service())
            .all(|call| call.parent.is_none())
    );
    assert_eq!(host.response(3, arm(id))["state"], "active");
}

#[test]
fn armed_timeout_does_not_extend_prepare_and_rolls_back_without_capture() {
    let mut host = Harness::new();
    host.broker.authenticated = false;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let deadline = host.pipeline.records[&id].prepared_until;
    host.now = deadline - 10;
    assert_eq!(host.response(2, arm(id))["state"], "armed");
    assert_eq!(host.response(3, arm(id))["state"], "armed");
    assert_eq!(host.pipeline.records[&id].prepared_until, deadline);
    host.now = deadline;
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Failed);
    assert_eq!(host.pipeline.records[&id].error, Some("prepare_expired"));
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.broker.leases(), 0);
    assert_eq!(host.plane.borrow().ticks, 0);
}

#[test]
fn stop_before_authentication_cancels_armed_lifecycle() {
    let mut host = Harness::new();
    host.broker.authenticated = false;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.response(2, arm(id));
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    host.broker.authenticated = true;
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Stopped);
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.plane.borrow().ticks, 0);
    assert_eq!(host.response(4, arm(id))["state"], "stopped");
}

#[test]
fn arm_owner_validation_conflicts_and_ordinary_start_semantics_are_preserved() {
    let mut host = Harness::new();
    host.broker.authenticated = false;
    host.pipeline
        .settings
        .allowed_principals
        .push("foreign".into());
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(
        host.response(2, start(id))["code"],
        "control_not_authenticated"
    );
    host.broker.enqueue(3, "foreign", arm(id));
    host.idle();
    assert_eq!(host.broker.replies[&3]["code"], "session_not_found");
    let mut invalid = arm(id);
    invalid["configuration"]["configuration"]["width"] = json!(1);
    assert_eq!(
        host.response(4, invalid)["code"],
        "invalid_start_configuration"
    );
    host.response(5, arm(id));
    let mut changed = arm(id);
    changed["configuration"]["encoder"] = json!(1);
    assert_eq!(host.response(6, changed)["code"], "start_conflict");
    assert_eq!(host.response(7, start(id))["code"], "invalid_state");
    let mut unknown = arm(id);
    unknown["assume_authenticated"] = json!(true);
    assert_eq!(host.response(8, unknown)["code"], "invalid_request");
}

#[test]
fn cancelled_or_lost_initial_arm_completion_rolls_back_but_retry_does_not() {
    for cancelled in [true, false] {
        let mut host = Harness::new();
        host.broker.authenticated = false;
        let id = host.response(1, prepare())["session"].as_u64().unwrap();
        host.broker.enqueue(2, "owner", arm(id));
        if cancelled {
            host.broker.cancel_parent(2);
        } else {
            host.broker.fail_completion = true;
        }
        host.idle();
        assert_eq!(host.pipeline.records[&id].state, State::Cancelled);
        assert_eq!(host.broker.live_components(), 0);
        assert!(!host.pipeline.records[&id].armed_accepted);
    }
    let mut host = Harness::new();
    host.broker.authenticated = false;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.response(2, arm(id));
    host.broker.fail_completion = true;
    host.broker.enqueue(3, "owner", arm(id));
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Armed);
    host.broker.authenticated = true;
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
}

#[test]
fn autonomous_capture_failure_uses_existing_confirmed_cleanup() {
    let mut host = Harness::new();
    host.broker.authenticated = false;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.response(2, arm(id));
    host.broker.format_rate = 44100;
    host.broker.authenticated = true;
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Failed);
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.broker.leases(), 0);
    assert_eq!(
        host.pipeline.records[&id].error,
        Some("unsupported_audio_format")
    );
}
