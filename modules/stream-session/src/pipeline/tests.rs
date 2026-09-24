mod armed;
mod harness;
mod input;
use super::*;
use harness::*;

#[test]
fn fixed_media_response_without_readiness_fields_remains_compatible() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare());
    let id = prepared["session"].as_u64().unwrap();
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["media_ready"], true);
    assert_eq!(prepared["peer_discovery"], false);
    assert_eq!(prepared["video_peer_port"], 51_001);
    assert_eq!(prepared["audio_peer_port"], 51_002);
    assert!(host.pipeline.records[&id].media_ready);
}

#[test]
fn prepare_start_stop_uses_actual_packet_size_and_canonical_shared_leases() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare());
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["packet_bytes"], 160);
    assert_eq!(prepared["wire_audio_bytes"], 176);
    let id = prepared["session"].as_u64().unwrap();
    assert_eq!(host.plane.borrow().ticks, 0);
    assert!(!prepared.to_string().contains("00112233"));
    let media = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Media.service())
        .unwrap();
    assert_eq!(media.payload["audio"]["wire_payload_bytes"], 176);
    let active = host.response(2, start(id));
    assert_eq!(active["state"], "active");
    assert_eq!(host.broker.leases(), 2);
    let record = &host.pipeline.records[&id];
    assert_eq!(
        record.audio_endpoint.as_deref(),
        Some("resolved-canonical-endpoint")
    );
    assert!(
        host.broker
            .calls
            .iter()
            .filter(|call| call.payload["action"] == "acquire")
            .all(|call| call.payload["mode"] == "shared")
    );
    host.broker.video_close_polls = 3;
    let stopped = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.broker.leases(), 0);
    assert_eq!(host.plane.borrow().dropped, 1);
    assert!(
        host.broker
            .calls
            .iter()
            .filter(|call| matches!(
                call.payload["action"].as_str(),
                Some("close" | "drain" | "stop" | "finish_stop")
            ))
            .all(|call| call.parent.is_none())
    );
    let final_lookup = host
        .broker
        .calls
        .iter()
        .rposition(|call| {
            call.service == Resource::Video.service() && call.payload["action"] == "lookup"
        })
        .unwrap();
    let release = host
        .broker
        .calls
        .iter()
        .position(|call| call.payload["action"] == "finish_stop")
        .unwrap();
    assert!(release > final_lookup);
}

#[test]
fn selected_video_batch_limit_reaches_media_transport_open() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["video"] = json!({"max_batch_size_kb":32});

    let prepared = host.response(1, request);

    assert_eq!(prepared["state"], "prepared");
    let media_open = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Media.service())
        .expect("media transport open request");
    assert_eq!(media_open.payload["video"]["max_batch_size_kb"], 32);
}

#[test]
fn media_readback_accepts_normalized_batch_values_and_checks_fallback_metadata() {
    for (configured, effective, fallback) in [(0, 64, false), (17, 64, true)] {
        let mut host = Harness::new();
        let mut request = prepare();
        request["video"] = json!({"max_batch_size_kb":configured});

        let prepared = host.response(1, request);

        assert_eq!(prepared["state"], "prepared");
        assert_eq!(prepared["media_ready"], true);
        let media_open = host
            .broker
            .calls
            .iter()
            .find(|call| call.service == Resource::Media.service())
            .expect("media transport open request");
        assert_eq!(media_open.payload["video"]["max_batch_size_kb"], configured);
        let record = host
            .pipeline
            .records
            .get(&prepared["session"].as_u64().unwrap())
            .expect("prepared session record");
        assert_eq!(record.config.video.effective().max_batch_size_kb, effective);
        assert_eq!(record.config.video.batch_size_fallback(), fallback);
    }
}

#[test]
fn constant_rate_mode_reaches_video_open_and_uses_a_distinct_capture_lease() {
    let mut constant_host = Harness::new();
    let prepared = constant_host.response(1, prepare());
    let id = prepared["session"].as_u64().unwrap();
    let mut configuration = start(id);
    configuration["configuration"]["capture_provider"] = json!("wgc");
    configuration["configuration"]["constant_rate"] = json!(true);
    assert_eq!(constant_host.response(2, configuration)["state"], "active");
    let constant_record = &constant_host.pipeline.records[&id];
    let constant_lease = constant_record.lease(false);
    let constant_open = constant_host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Video.service() && call.payload["action"] == "open")
        .expect("constant-rate video open");
    assert_eq!(constant_open.payload["capture_provider"], "wgc");
    assert_eq!(constant_open.payload["constant_rate"], true);

    let mut variable_host = Harness::new();
    let prepared = variable_host.response(1, prepare());
    let id = prepared["session"].as_u64().unwrap();
    let mut configuration = start(id);
    configuration["configuration"]["capture_provider"] = json!("wgc");
    assert_eq!(variable_host.response(2, configuration)["state"], "active");
    let variable_record = &variable_host.pipeline.records[&id];
    let variable_lease = variable_record.lease(false);
    let variable_open = variable_host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Video.service() && call.payload["action"] == "open")
        .expect("variable-rate video open");
    assert_eq!(variable_open.payload["constant_rate"], false);
    assert_ne!(constant_lease, variable_lease);
}

#[test]
fn wgc_pacing_smoothing_snapshot_reaches_video_open() {
    let mut enabled_host = Harness::new();
    let id = enabled_host.response(1, prepare())["session"]
        .as_u64()
        .unwrap();
    let mut enabled = start(id);
    enabled["configuration"]["capture_provider"] = json!("wgc");
    assert_eq!(enabled_host.response(2, enabled)["state"], "active");
    let enabled_open = enabled_host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Video.service() && call.payload["action"] == "open")
        .expect("enabled WGC video open");
    assert_eq!(enabled_open.payload["wgc_pacing_smoothing"], true);

    let mut disabled_host = Harness::new();
    let id = disabled_host.response(1, prepare())["session"]
        .as_u64()
        .unwrap();
    let mut disabled = start(id);
    disabled["configuration"]["capture_provider"] = json!("wgc");
    disabled["configuration"]["wgc_pacing_smoothing"] = json!(false);
    assert_eq!(disabled_host.response(2, disabled)["state"], "active");
    let disabled_open = disabled_host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Video.service() && call.payload["action"] == "open")
        .expect("disabled WGC video open");
    assert_eq!(disabled_open.payload["wgc_pacing_smoothing"], false);
}

#[test]
fn disabled_audio_skips_encoder_capture_and_audio_lease_but_keeps_video_plane() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["stream_audio"] = json!(false);
    let prepared = host.response(1, request);
    let id = prepared["session"].as_u64().unwrap();
    assert_eq!(prepared["stream_audio"], false);
    assert_eq!(prepared["packet_bytes"], 0);
    assert_eq!(prepared["wire_audio_bytes"], 16);
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == Resource::AudioEncoder.service())
    );
    let media = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Media.service())
        .unwrap();
    assert_eq!(media.payload["stream_audio"], false);
    assert_eq!(media.payload["audio"]["wire_payload_bytes"], 16);
    assert_eq!(media.payload["encryption"]["audio"], false);

    let mut configuration = start(id);
    configuration["configuration"]["audio_endpoint"] = json!("");
    let active = host.response(2, configuration);
    assert_eq!(active["state"], "active");
    assert_eq!(host.broker.leases(), 1);
    assert!(
        host.broker
            .calls
            .iter()
            .any(|call| call.service == Resource::Video.service())
    );
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == Resource::AudioCapture.service())
    );
    let leases: Vec<_> = host
        .broker
        .calls
        .iter()
        .filter(|call| call.payload["action"] == "acquire")
        .map(|call| call.payload["resource"].as_str().unwrap())
        .collect();
    assert_eq!(leases.len(), 1);
}

#[test]
fn start_forwards_required_reference_frame_limit_to_video() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let mut configuration = start(id);
    configuration["configuration"]["configuration"]["max_reference_frames"] = json!(1);
    assert_eq!(host.response(2, configuration)["state"], "active");
    let video = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Video.service() && call.payload["action"] == "open")
        .unwrap();
    assert_eq!(video.payload["configuration"]["max_reference_frames"], 1);
}

#[test]
fn caller_isolation_retry_conflicts_and_terminal_tombstones() {
    let mut host = Harness::new();
    let first = host.response(1, prepare());
    let id = first["session"].as_u64().unwrap();
    let count = host.broker.live_components();
    assert_eq!(host.response(2, prepare())["session"], id);
    assert_eq!(host.broker.live_components(), count);
    let mut changed = prepare();
    changed["audio_port"] = json!(51003);
    assert_eq!(host.response(3, changed)["code"], "operation_conflict");
    host.broker
        .enqueue(4, "foreign", json!({"action":"status","session":id}));
    host.idle();
    assert_eq!(host.broker.replies[&4]["code"], "session_not_found");
    assert_eq!(
        host.response(5, json!({"action":"close","session":id}))["state"],
        "stopped"
    );
    assert_eq!(host.response(6, prepare())["state"], "stopped");
    assert_eq!(host.broker.live_components(), 0);
}

#[test]
fn unauthenticated_start_is_retryable_without_hardware_or_new_allocations() {
    let mut host = Harness::new();
    host.broker.authenticated = false;
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(
        host.response(2, start(id))["code"],
        "control_not_authenticated"
    );
    assert_eq!(host.pipeline.records[&id].state, State::Prepared);
    assert_eq!(host.broker.live_components(), 3);
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == Resource::Video.service())
    );
    host.broker.authenticated = true;
    assert_eq!(host.response(3, start(id))["state"], "active");
}

#[test]
fn prepared_deadline_runs_cleanup_under_continuous_status_traffic() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.now += 6000;
    for request in 2..50 {
        host.broker
            .enqueue(request, "owner", json!({"action":"status","session":id}));
    }
    host.idle();
    assert!(host.pipeline.records[&id].state.terminal());
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.pipeline.records[&id].error, Some("prepare_expired"));
}

#[test]
fn lost_admission_and_take_result_before_or_after_commit_are_reconciled() {
    for fault in [
        Fault::Admission,
        Fault::BeforeMutation,
        Fault::AfterMutation,
    ] {
        let mut host = Harness::new();
        host.broker.fault = Some((Resource::Media.service(), fault));
        let result = host.response(1, prepare());
        assert_eq!(result["state"], "failed");
        assert_eq!(host.broker.live_components(), 0);
        assert_eq!(host.broker.leases(), 0);
        let cancel = host
            .broker
            .commands
            .iter()
            .position(|command| matches!(command, Command::Cancel { .. }))
            .unwrap();
        let lookup = host.broker.commands.iter().position(|command| matches!(command, Command::Request { service, payload, .. } if service == &Resource::Media.service() && payload["action"]=="lookup")).unwrap();
        assert!(cancel < lookup);
    }
}

#[test]
fn unconfirmed_cancellation_retains_resources_for_explicit_stop_retry() {
    let mut host = Harness::new();
    host.broker.fault = Some((Resource::Media.service(), Fault::AfterMutation));
    host.broker.fail_cancel = true;
    let failed = host.response(1, prepare());
    let id = failed["session"].as_u64().unwrap();
    assert_eq!(failed["state"], "cleanup_failed");
    assert!(host.pipeline.records[&id].uncertain.is_some());
    assert!(host.broker.live_components() > 0);
    host.broker.fail_cancel = false;
    assert_eq!(
        host.response(2, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert_eq!(host.broker.live_components(), 0);
}

#[test]
fn failed_close_keeps_lease_until_stop_retry_confirms_release() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.broker.fail_close = Some(Resource::AudioCapture.service());
    let failed = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(failed["state"], "cleanup_failed");
    assert_eq!(host.broker.leases(), 2);
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.payload["action"] == "finish_stop")
    );
    host.broker.fail_close = None;
    assert_eq!(
        host.response(4, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert_eq!(host.broker.leases(), 0);
    assert_eq!(host.broker.live_components(), 0);
}

#[test]
fn incompatible_capture_format_rolls_back_all_components_and_leases() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.broker.format_rate = 44100;
    let reply = host.response(2, start(id));
    assert_eq!(reply["state"], "failed");
    assert_eq!(reply["error"], "unsupported_audio_format");
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.broker.leases(), 0);
    assert_eq!(host.plane.borrow().ticks, 0);
}

#[test]
fn cancelled_parent_and_lost_success_completion_release_allocations() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    host.tick();
    host.tick();
    host.broker.cancel_parent(1);
    host.idle();
    assert_eq!(host.broker.live_components(), 0);
    assert!(
        host.pipeline
            .records
            .values()
            .all(|record| record.state.terminal())
    );
    let mut host = Harness::new();
    host.broker.fail_completion = true;
    host.broker.enqueue(1, "owner", prepare());
    host.idle();
    assert_eq!(host.broker.live_components(), 0);
    assert!(
        host.pipeline
            .records
            .values()
            .all(|record| record.state.terminal())
    );
}

#[test]
fn host_direction_termination_is_rejected_and_idr_is_coalesced() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.response(2, start(id));
    host.plane
        .borrow_mut()
        .controls
        .push(crate::plane::ControlRecord {
            message_type: 0x0109,
            payload: vec![0; 4],
            sequence: 3,
        });
    host.tick();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert_eq!(host.pipeline.records[&id].rejected_controls, 1);
    for sequence in 4..8 {
        host.plane
            .borrow_mut()
            .controls
            .push(crate::plane::ControlRecord {
                message_type: 0x0302,
                payload: vec![0, 0],
                sequence,
            });
    }
    host.idle();
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(|call| call.payload["action"] == "force_idr")
            .count(),
        1
    );
}

#[test]
fn continuous_idr_control_cannot_starve_explicit_stop() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.response(2, start(id));
    host.broker
        .enqueue(3, "owner", json!({"action":"stop","session":id}));
    for sequence in 0..200 {
        host.plane
            .borrow_mut()
            .controls
            .push(crate::plane::ControlRecord {
                message_type: 0x0302,
                payload: vec![0, 0],
                sequence,
            });
        host.tick();
        if host.broker.replies.contains_key(&3) {
            break;
        }
    }
    assert_eq!(host.broker.replies[&3]["state"], "stopped");
    assert_eq!(host.broker.live_components(), 0);
}

#[test]
fn unload_releases_bindings_without_claiming_asynchronous_provider_cleanup() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.response(2, start(id));
    assert!(
        host.pipeline
            .shutdown_with(&mut |command| host.broker.send(command))
    );
    assert_eq!(host.plane.borrow().dropped, 1);
    assert_eq!(host.broker.leases(), 2);
    assert_eq!(host.broker.live_components(), 5);
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    host.tick();
    host.tick();
    assert!(
        host.pipeline
            .shutdown_with(&mut |command| host.broker.send(command))
    );
    assert!(
        host.broker
            .commands
            .iter()
            .all(|command| !matches!(command, Command::Request { .. } | Command::Cancel { .. }))
    );
    assert_eq!(host.broker.live_components(), 0);
}
