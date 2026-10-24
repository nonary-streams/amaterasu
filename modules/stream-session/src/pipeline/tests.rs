mod armed;
mod display;
mod harness;
mod history;
mod input;
mod limiter;
use super::*;
use harness::*;

#[test]
fn session_status_list_is_scoped_to_the_exact_authenticated_caller() {
    let mut host = Harness::new();
    host.pipeline.settings.allowed_principals =
        vec!["amaterasu.rtsp".into(), "another.module".into()];
    let rtsp_session = host.response_from(1, "amaterasu.rtsp", prepare())["session"]
        .as_u64()
        .unwrap();
    let foreign_session = host.response_from(2, "another.module", {
        let mut request = prepare();
        request["operation_id"] = json!("foreign-launch");
        request
    })["session"]
        .as_u64()
        .unwrap();

    let rtsp_status = host.response_from(3, "amaterasu.rtsp", json!({"action":"status"}));
    let foreign_status = host.response_from(4, "another.module", json!({"action":"status"}));

    assert_eq!(rtsp_status["total"], 1);
    assert_eq!(rtsp_status["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(rtsp_status["sessions"][0]["session"], rtsp_session);
    assert_eq!(foreign_status["total"], 1);
    assert_eq!(foreign_status["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(foreign_status["sessions"][0]["session"], foreign_session);
}

#[test]
fn negotiated_color_reaches_video_open_unchanged() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare());
    let session = prepared["session"].as_u64().expect("prepared session");
    let mut request = start(session);
    request["configuration"]["configuration"]["color_mode"] = json!("bt601_full");
    let started = host.response(2, request);
    assert_eq!(started["state"], "active", "{started}");
    let video = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Video.service() && call.payload["action"] == "open")
        .expect("video opened");
    assert_eq!(video.payload["configuration"]["color_mode"], "bt601_full");
}

#[test]
fn negotiated_video_geometry_and_rate_are_allocated_only_at_authenticated_start() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare());
    let session = prepared["session"].as_u64().expect("prepared session");
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == Resource::Video.service()
                && call.payload["action"] == "open")
    );

    let mut request = start(session);
    request["configuration"]["configuration"] = json!({
        "width": 1280,
        "height": 720,
        "fps": 30,
        "bitrate": 8_000_000,
        "max_reference_frames": 0,
    });
    let started = host.response(2, request);
    assert_eq!(started["state"], "active", "{started}");

    let video = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Video.service() && call.payload["action"] == "open")
        .expect("video opens after authenticated start");
    assert_eq!(video.payload["configuration"]["width"], 1280);
    assert_eq!(video.payload["configuration"]["height"], 720);
    assert_eq!(video.payload["configuration"]["fps"], 30);
    assert_eq!(video.payload["configuration"]["bitrate"], 8_000_000);
    assert!(video.payload["configuration"]["max_reference_frames"].is_null());
    let forwarded_configuration: crate::model::VideoConfiguration =
        serde_json::from_value(video.payload["configuration"].clone())
            .expect("omitted zero uses the encoder's default reference selection");
    assert_eq!(forwarded_configuration.max_reference_frames, 0);
}

#[test]
fn stereo_512k_prepare_reaches_encoder_with_bounded_packet_size() {
    for (packet_ms, packet_bytes) in [(5, 320), (10, 640)] {
        let mut host = Harness::new();
        let mut request = prepare();
        request["audio"] = json!({"packet_ms":packet_ms,"bitrate":512000});
        let prepared = host.response(1, request);
        assert_eq!(prepared["state"], "prepared", "{prepared}");
        assert_eq!(
            prepared["audio"],
            json!({"packet_ms":packet_ms,"bitrate":512000})
        );
        assert_eq!(prepared["packet_bytes"], packet_bytes);
        assert!(packet_bytes <= 1275);
        assert_eq!(
            prepared["wire_audio_bytes"],
            if packet_ms == 5 { 336 } else { 656 }
        );
        let encoder = host
            .broker
            .calls
            .iter()
            .find(|call| {
                call.service == Resource::AudioEncoder.service() && call.payload["action"] == "open"
            })
            .expect("audio encoder opened");
        assert_eq!(encoder.payload["channels"], 2);
        assert_eq!(encoder.payload["packet_ms"], packet_ms);
        assert_eq!(encoder.payload["bitrate"], 512000);
        let media = host
            .broker
            .calls
            .iter()
            .find(|call| {
                call.service == Resource::Media.service() && call.payload["action"] == "open"
            })
            .expect("media transport opened");
        assert_eq!(
            media.payload["audio"]["wire_payload_bytes"],
            prepared["wire_audio_bytes"]
        );
    }

    let mut host = Harness::new();
    let mut request = prepare();
    request["audio"] = json!({"packet_ms":10,"bitrate":512001});
    assert_eq!(
        host.response(1, request)["code"],
        "invalid_audio_configuration"
    );
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == Resource::AudioEncoder.service())
    );
}

#[test]
fn prepared_stereo_session_requests_48khz_float_stereo_loopback() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare());
    let session = prepared["session"].as_u64().expect("prepared session");

    let started = host.response(2, start(session));
    assert_eq!(started["state"], "active", "{started}");

    let audio_open = host
        .broker
        .calls
        .iter()
        .find(|call| {
            call.service == Resource::AudioCapture.service() && call.payload["action"] == "open"
        })
        .expect("audio capture opened");
    assert_eq!(
        audio_open.payload["capture_format"],
        json!({"sample_rate":48000,"channels":2,"sample_format":"f32"})
    );
    // The fake provider returns the exact requested format. Reaching active
    // means lifecycle validation accepted its 48 kHz float stereo metadata.
    assert_eq!(host.broker.format_rate, 48_000);
}

#[test]
fn surround_prepare_start_and_stop_preserve_layout_and_bounded_wire() {
    for (channels, mask, packet_ms, bitrate, bytes, wire) in [
        (6, 0x3f, 5, 1_536_000, 960, 976),
        (6, 0x3f, 10, 800_000, 1000, 1008),
        (8, 0x63f, 5, 512_000, 320, 336),
        (8, 0x63f, 10, 450_000, 562, 576),
    ] {
        let mut host = Harness::new();
        let mut request = prepare();
        request["audio"] = json!({"channels":channels,"channel_mask":mask,
            "layout":if channels == 6 { "5.1" } else { "7.1" },
            "packet_ms":packet_ms,"bitrate":bitrate});
        let prepared = host.response(1, request.clone());
        let id = prepared["session"]
            .as_u64()
            .expect("prepared surround stream");
        assert_eq!(prepared["audio"], request["audio"]);
        assert_eq!(prepared["packet_bytes"], bytes);
        assert_eq!(prepared["wire_audio_bytes"], wire);
        assert_eq!(prepared["audio_encoder_layout"]["mapping_family"], 1);
        let encoder = host
            .broker
            .calls
            .iter()
            .find(|call| {
                call.service == Resource::AudioEncoder.service() && call.payload["action"] == "open"
            })
            .unwrap();
        assert_eq!(encoder.payload["channels"], channels);
        assert_eq!(encoder.payload["bitrate"], bitrate);
        let media = host
            .broker
            .calls
            .iter()
            .find(|call| {
                call.service == Resource::Media.service() && call.payload["action"] == "open"
            })
            .unwrap();
        assert_eq!(
            media.payload["audio"],
            json!({"channels":channels,
            "packet_ms":packet_ms,"wire_payload_bytes":wire})
        );
        let mut start_request = start(id);
        start_request["configuration"]["audio_endpoint"] = json!("");
        start_request["configuration"]["virtual_sink"] = json!("Steam Streaming Speakers");
        start_request["configuration"]["channels"] = json!(channels);
        assert_eq!(host.response(2, start_request)["state"], "active");
        let capture = host
            .broker
            .calls
            .iter()
            .find(|call| {
                call.service == Resource::AudioCapture.service() && call.payload["action"] == "open"
            })
            .unwrap();
        assert_eq!(capture.payload["channels"], channels);
        assert_eq!(
            capture.payload["capture_format"],
            json!({"sample_rate":48000,"channels":channels,"sample_format":"f32"})
        );
        assert_eq!(
            host.response(3, json!({"action":"stop","session":id}))["state"],
            "stopped"
        );
        assert_eq!(host.broker.leases(), 0);
    }
}

#[test]
fn surround_invalid_layout_and_packet_size_fail_before_resources() {
    for (channels, mask, packet_ms, bitrate) in [
        (6, 3, 5, 256_000),
        (8, 0x63f, 5, 450_000),
        (8, 0x63f, 5, 2_048_000),
        (6, 0x3f, 10, 1_536_000),
    ] {
        let mut host = Harness::new();
        let mut request = prepare();
        request["audio"] = json!({"channels":channels,"channel_mask":mask,
            "layout":if channels == 6 { "5.1" } else { "7.1" },
            "packet_ms":packet_ms,"bitrate":bitrate});
        assert_eq!(
            host.response(1, request)["code"],
            "invalid_audio_configuration"
        );
        assert!(host.broker.calls.is_empty());
    }
}

#[test]
fn surround_start_rejects_capture_layout_mismatch_before_capture() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["audio"] = json!({"channels":6,"channel_mask":0x3f,"layout":"5.1",
        "packet_ms":5,"bitrate":256000});
    let id = host.response(1, request)["session"].as_u64().unwrap();
    let mut start_request = start(id);
    start_request["configuration"]["audio_endpoint"] = json!("");
    start_request["configuration"]["virtual_sink"] = json!("Steam Streaming Speakers");
    start_request["configuration"]["channels"] = json!(8);
    assert_eq!(
        host.response(2, start_request)["code"],
        "audio_configuration_mismatch"
    );
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == Resource::AudioCapture.service())
    );
}

#[test]
fn active_audio_failure_recovers_same_encoder_stream_before_more_media() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    let encoder = host.pipeline.records[&id].resources[&Resource::AudioEncoder];
    host.plane.borrow_mut().failure = Some("audio_encoder_failed");
    host.tick();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert!(host.pipeline.records[&id].audio_recovery_needed);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert!(!host.pipeline.records[&id].audio_recovery_needed);
    assert_eq!(host.plane.borrow().failure, None);
    let recover: Vec<_> = host
        .broker
        .calls
        .iter()
        .filter(|call| call.payload["action"] == "recover")
        .collect();
    assert_eq!(recover.len(), 1);
    assert_eq!(recover[0].service, Resource::AudioEncoder.service());
    assert_eq!(recover[0].payload["stream"], encoder);
}

#[test]
fn rejected_audio_recovery_enters_owned_cleanup_without_replacing_stream() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.broker.fail_audio_recover = true;
    host.plane.borrow_mut().failure = Some("audio_encoder_failed");
    host.idle();
    assert!(matches!(
        host.pipeline.records[&id].state,
        State::Failed | State::CleanupFailed
    ));
    assert_eq!(
        host.pipeline.records[&id].error,
        Some("dependency_rejected")
    );
    assert_eq!(host.plane.borrow().dropped, 1);
}

#[test]
fn mismatched_audio_recovery_stream_enters_confirmed_cleanup() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.broker.malformed_audio_recover = true;
    host.plane.borrow_mut().failure = Some("audio_encoder_failed");
    host.idle();
    assert!(matches!(
        host.pipeline.records[&id].state,
        State::Failed | State::CleanupFailed
    ));
    assert_eq!(
        host.pipeline.records[&id].error,
        Some("invalid_audio_recovery_response")
    );
    assert!(!host.pipeline.records[&id].audio_recovery_needed);
    assert_eq!(host.broker.live_components(), 0);
}

#[test]
fn lost_recovery_admission_retries_one_operation_and_stop_still_owns_cleanup() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.broker.fault = Some((Resource::AudioEncoder.service(), Fault::Admission));
    host.plane.borrow_mut().failure = Some("audio_encoder_failed");
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(|call| call.payload["action"] == "recover")
            .count(),
        1
    );
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert_eq!(host.broker.live_components(), 0);
}

#[test]
fn encoder_failure_while_draining_recovers_before_transport_cleanup() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    {
        let record = host.pipeline.records.get_mut(&id).unwrap();
        record.state = State::Stopping;
        record.outcome = State::Stopped;
        record.plane.as_mut().unwrap().stop();
    }
    host.pipeline.job = Some(Job::new(id, None, Step::WaitDrain, host.now + 5000));
    host.plane.borrow_mut().failure = Some("audio_encoder_failed");
    host.tick();
    assert!(host.pipeline.job.as_ref().unwrap().step == Step::RecoverAudio);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Stopped);
    assert_eq!(host.broker.live_components(), 0);
}

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
fn prepared_frame_limiter_snapshot_is_frozen_across_retry_and_readback() {
    let mut host = Harness::new();
    let mut request = prepare();
    let snapshot = json!({
        "version":1,
        "settings":{
            "frame_limiter_enable":true,
            "frame_limiter_fps_limit_millihz":60000,
            "frame_limiter_provider":"auto",
            "frame_limiter_disable_vsync":false,
            "frame_limiter_auto_virtual_framegen":"enabled",
            "rtss_frame_limit_type":"async",
            "rtss_allow_virtual_display_override":false,
            "rtss_install_path":"",
            "mangohud_limiter_method":"late",
            "mangohud_preset":"custom",
            "mangohud_always_show_graph":false
        },
        "requested_stream_fps_millihz":60000,
        "client_display_mode_override_millihz":null,
        "lossless_scaling_limit_millihz":null,
        "virtual_display_active":null,
        "physical_framegen_capture":null,
        "game_frame_generation_mode":null,
        "rtss_frame_limit_type_explicit_override":null
    });
    request["frame_limiter_settings"] = snapshot.clone();
    request["frame_limiter_executable_path"] = json!(r"C:\Games\Example.exe");

    let prepared = host.response(1, request.clone());
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["frame_limiter_settings"], snapshot);
    let session = prepared["session"].as_u64().unwrap();
    assert_eq!(host.response(2, request.clone())["session"], session);

    request["frame_limiter_settings"]["settings"]["frame_limiter_enable"] = json!(false);
    assert_eq!(host.response(3, request)["code"], "operation_conflict");
    let status = host.response(4, json!({"action":"status","session":session}));
    assert_eq!(status["frame_limiter_settings"], snapshot);

    let mut malformed = prepare();
    malformed["frame_limiter_settings"] = snapshot.clone();
    malformed["frame_limiter_settings"]["settings"]["frame_limiter_provider"] = json!("unknown");
    malformed["frame_limiter_executable_path"] = json!(r"C:\Games\Example.exe");
    assert_eq!(
        Harness::new().response(1, malformed)["code"],
        "invalid_frame_limiter_snapshot"
    );

    let mut missing_rate = prepare();
    missing_rate["frame_limiter_settings"] = snapshot;
    missing_rate["frame_limiter_settings"]["requested_stream_fps_millihz"] = json!(0);
    missing_rate["frame_limiter_executable_path"] = json!(r"C:\Games\Example.exe");
    assert_eq!(
        Harness::new().response(1, missing_rate)["code"],
        "invalid_frame_limiter_snapshot"
    );
}

#[test]
fn configured_ping_timeout_reaches_media_discovery_and_control() {
    for selected in [0, 250, i32::MAX as u32] {
        let mut host = Harness::new();
        let mut request = prepare_discovery();
        request["ping_timeout_ms"] = json!(selected);
        assert_eq!(host.response(1, request)["state"], "prepared");
        for resource in [Resource::Media, Resource::Control] {
            let open = host
                .broker
                .calls
                .iter()
                .find(|call| call.service == resource.service() && call.payload["action"] == "open")
                .expect("component open");
            assert_eq!(open.payload["ping_timeout_ms"], selected);
        }
    }
}

#[test]
fn video_encryption_policy_snapshot_reaches_media_open() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["video_encryption_policy"] = json!({"lan_mode":2,"wan_mode":0});
    let prepared = host.response(1, request);
    assert_eq!(prepared["state"], "prepared");

    let media_open = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Media.service() && call.payload["action"] == "open")
        .expect("media open request");
    assert_eq!(media_open.payload["peer_ip"], "127.0.0.1");
    assert_eq!(
        media_open.payload["video_encryption_policy"],
        json!({"lan_mode":2,"wan_mode":0})
    );
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
    let audio_open = host
        .broker
        .calls
        .iter()
        .find(|call| {
            call.service == Resource::AudioCapture.service() && call.payload["action"] == "open"
        })
        .expect("audio capture open");
    assert_eq!(audio_open.payload["host_audio_enabled"], false);
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
fn host_audio_policy_snapshot_reaches_audio_capture_open() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare());
    let id = prepared["session"].as_u64().unwrap();
    let mut request = start(id);
    request["configuration"]["host_audio_enabled"] = json!(true);

    assert_eq!(host.response(2, request)["state"], "active");
    let audio_open = host
        .broker
        .calls
        .iter()
        .find(|call| {
            call.service == Resource::AudioCapture.service() && call.payload["action"] == "open"
        })
        .expect("audio capture open");
    assert_eq!(audio_open.payload["host_audio_enabled"], true);
}

#[test]
fn continuous_audio_open_uses_prepared_packet_duration_and_stays_opt_in() {
    for (setting, packet_ms) in [
        (None, 10),
        (Some(false), 5),
        (Some(true), 5),
        (Some(true), 10),
    ] {
        let mut host = Harness::new();
        let mut prepare_request = prepare();
        prepare_request["audio"] = json!({"packet_ms":packet_ms,"bitrate":128000});
        let id = host.response(1, prepare_request)["session"]
            .as_u64()
            .unwrap();
        let mut start_request = start(id);
        if let Some(enabled) = setting {
            start_request["configuration"]["continuous_audio"] = json!(enabled);
        }

        assert_eq!(host.response(2, start_request)["state"], "active");
        let audio_open = host
            .broker
            .calls
            .iter()
            .find(|call| {
                call.service == Resource::AudioCapture.service() && call.payload["action"] == "open"
            })
            .expect("audio capture open");
        if setting == Some(true) {
            assert_eq!(
                audio_open.payload["continuous_audio"],
                json!({"packet_ms":packet_ms})
            );
        } else {
            assert!(audio_open.payload.get("continuous_audio").is_none());
        }
    }
}

#[test]
fn continuous_audio_is_rejected_when_the_prepared_stream_disables_audio() {
    let mut host = Harness::new();
    let mut prepare_request = prepare();
    prepare_request["stream_audio"] = json!(false);
    let id = host.response(1, prepare_request)["session"]
        .as_u64()
        .unwrap();
    let mut start_request = start(id);
    start_request["configuration"]["continuous_audio"] = json!(true);

    let reply = host.response(2, start_request);
    assert_eq!(reply["code"], "continuous_audio_requires_stream_audio");
    assert!(
        host.broker
            .calls
            .iter()
            .all(|call| call.service != Resource::AudioCapture.service())
    );
}

#[test]
fn capture_only_snapshot_reaches_audio_capture_open() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare());
    let id = prepared["session"].as_u64().unwrap();
    let mut request = start(id);
    request["configuration"]["capture_sink_without_routing"] = json!(true);

    assert_eq!(host.response(2, request)["state"], "active");
    let audio_open = host
        .broker
        .calls
        .iter()
        .find(|call| {
            call.service == Resource::AudioCapture.service() && call.payload["action"] == "open"
        })
        .expect("audio capture open");
    assert_eq!(audio_open.payload["capture_sink_without_routing"], true);
}

#[test]
fn virtual_sink_selector_and_stereo_layout_reach_audio_capture_open() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare());
    let id = prepared["session"].as_u64().unwrap();
    let mut request = start(id);
    request["configuration"]["audio_endpoint"] = json!("");
    request["configuration"]["virtual_sink"] = json!("Steam Streaming Speakers");
    request["configuration"]["channels"] = json!(2);
    request["configuration"]["capture_sink_without_routing"] = json!(true);

    assert_ne!(host.response(2, request)["state"], "active");
    assert!(
        host.broker
            .calls
            .iter()
            .all(|call| call.service != Resource::AudioCapture.service())
    );

    let mut request = start(id);
    request["configuration"]["audio_endpoint"] = json!("");
    request["configuration"]["virtual_sink"] = json!("Steam Streaming Speakers");
    request["configuration"]["channels"] = json!(2);
    request["configuration"]["capture_sink_without_routing"] = json!(false);
    assert_eq!(host.response(3, request)["state"], "active");
    let audio_open = host
        .broker
        .calls
        .iter()
        .find(|call| {
            call.service == Resource::AudioCapture.service() && call.payload["action"] == "open"
        })
        .expect("virtual audio capture open");
    assert_eq!(audio_open.payload["endpoint"], "");
    assert_eq!(
        audio_open.payload["virtual_sink"],
        "Steam Streaming Speakers"
    );
    assert_eq!(audio_open.payload["channels"], 2);
    assert_eq!(audio_open.payload["capture_sink_without_routing"], false);
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
fn pipewire_portal_source_reaches_video_open_with_its_capture_lease() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare());
    let id = prepared["session"].as_u64().unwrap();
    let mut configuration = start(id);
    configuration["configuration"]["capture_provider"] = json!("pipewire");
    configuration["configuration"]["output"] = json!("portal-selected-session-source");

    assert_eq!(host.response(2, configuration)["state"], "active");
    let record = &host.pipeline.records[&id];
    assert!(matches!(
        record.start.as_ref().unwrap().capture_provider,
        crate::model::CaptureProvider::PipeWire
    ));
    let video_open = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Video.service() && call.payload["action"] == "open")
        .expect("PipeWire video open");
    assert_eq!(video_open.payload["capture_provider"], "pipewire");
    assert_eq!(
        video_open.payload["output"],
        "portal-selected-session-source"
    );
    assert_eq!(video_open.payload["encoder"], 0);
}

#[test]
fn automatic_encoder_alias_reaches_video_open_without_becoming_index_zero() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare());
    let id = prepared["session"].as_u64().unwrap();
    let mut configuration = start(id);
    configuration["configuration"]["encoder"] = json!("auto");

    assert_eq!(host.response(2, configuration)["state"], "active");
    let video_open = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Video.service() && call.payload["action"] == "open")
        .expect("video open");
    assert_eq!(video_open.payload["encoder"], "auto");
}

#[test]
fn explicit_encoder_index_reaches_video_open_unchanged() {
    let mut host = Harness::new();
    let prepared = host.response(1, prepare());
    let id = prepared["session"].as_u64().unwrap();
    let mut configuration = start(id);
    configuration["configuration"]["encoder"] = json!(7);

    assert_eq!(host.response(2, configuration)["state"], "active");
    let video_open = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == Resource::Video.service() && call.payload["action"] == "open")
        .expect("video open");
    assert_eq!(video_open.payload["encoder"], 7);
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
    let deadline = host.pipeline.records[&id].prepared_until;
    assert_eq!(deadline - host.now, 10_000);
    host.now = deadline;
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
