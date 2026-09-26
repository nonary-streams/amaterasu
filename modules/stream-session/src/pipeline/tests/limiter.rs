use super::*;

fn limiter_prepare() -> (Value, Value) {
    let snapshot = json!({
        "version":1,
        "settings":{
            "frame_limiter_enable":true,
            "frame_limiter_fps_limit_millihz":0,
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
        "client_display_mode_override_millihz":0,
        "lossless_scaling_limit_millihz":null,
        "virtual_display_active":null,
        "physical_framegen_capture":true,
        "game_frame_generation_mode":null,
        "rtss_frame_limit_type_explicit_override":false
    });
    let mut request = prepare();
    request["frame_limiter_settings"] = snapshot.clone();
    request["frame_limiter_executable_path"] = json!(r"C:\Games\Example.exe");
    (request, snapshot)
}

#[test]
fn frozen_apply_precedes_video_and_restores_after_media_close() {
    let mut host = Harness::new();
    let (request, snapshot) = limiter_prepare();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    let started = host.response(2, start(id));
    assert_eq!(started["state"], "active");
    assert_eq!(started["frame_limiter_applied"], true);
    let apply = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.frame-limiter/1" && call.payload["action"] == "apply"
        })
        .unwrap();
    let video = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == Resource::Video.service() && call.payload["action"] == "open"
        })
        .unwrap();
    assert!(apply < video);
    let payload = &host.broker.calls[apply].payload;
    assert_eq!(payload["settings"], snapshot["settings"]);
    for field in [
        "requested_stream_fps_millihz",
        "client_display_mode_override_millihz",
        "lossless_scaling_limit_millihz",
        "virtual_display_active",
        "physical_framegen_capture",
        "game_frame_generation_mode",
        "rtss_frame_limit_type_explicit_override",
    ] {
        assert_eq!(payload[field], snapshot[field], "{field}");
    }
    assert_eq!(payload["executable_path"], r"C:\Games\Example.exe");
    assert_eq!(payload["session_id"], format!("stream-{id:016x}"));
    let stopped = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(host.broker.limiter_lease, None);
    let restore = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.frame-limiter/1" && call.payload["action"] == "restore"
        })
        .unwrap();
    let close_media = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == Resource::Media.service() && call.payload["action"] == "close"
        })
        .unwrap();
    assert!(close_media < restore);
}

#[test]
fn failed_start_and_lost_apply_result_recover_and_restore_lease() {
    for lost_result in [false, true] {
        let mut host = Harness::new();
        let (request, _) = limiter_prepare();
        let id = host.response(1, request)["session"].as_u64().unwrap();
        host.broker.fail_open = Some(Resource::Video.service());
        if lost_result {
            host.broker.fault = Some(("amaterasu.frame-limiter/1".into(), Fault::AfterMutation));
        }
        let result = host.response(2, start(id));
        assert_eq!(result["state"], "failed");
        assert_eq!(host.broker.limiter_lease, None);
        if lost_result {
            assert!(
                host.broker
                    .calls
                    .iter()
                    .any(|call| call.service == "amaterasu.frame-limiter/1"
                        && call.payload["action"] == "status")
            );
        }
    }
}

#[test]
fn unavailable_admission_is_nonfatal_and_needs_no_cleanup_lease() {
    let mut host = Harness::new();
    let (request, _) = limiter_prepare();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    host.broker.limiter_admission_unavailable = true;
    let started = host.response(2, start(id));
    assert_eq!(started["state"], "active");
    assert_eq!(started["frame_limiter_applied"], false);
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
}

#[test]
fn shutdown_restores_active_lease_with_bounded_broker_work() {
    let mut host = Harness::new();
    let (request, _) = limiter_prepare();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["frame_limiter_applied"], true);
    let settled = host
        .pipeline
        .shutdown_with(&mut |command| host.broker.send(command));
    assert!(settled);
    assert_eq!(host.broker.limiter_lease, None);
}

#[test]
fn failed_restore_retains_lease_for_explicit_stop_retry() {
    let mut host = Harness::new();
    let (request, _) = limiter_prepare();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["frame_limiter_applied"], true);
    host.broker.limiter_restore_failures = 1;
    let first = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(first["state"], "cleanup_failed");
    assert_eq!(first["frame_limiter_state"], "restore_failed");
    assert!(host.broker.limiter_lease.is_some());
    let retried = host.response(4, json!({"action":"stop","session":id}));
    assert_eq!(retried["state"], "stopped");
    assert!(host.broker.limiter_lease.is_none());
}

#[test]
fn lost_apply_admission_does_not_claim_unverified_effect() {
    let mut host = Harness::new();
    let (request, _) = limiter_prepare();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    host.broker.fault = Some(("amaterasu.frame-limiter/1".into(), Fault::Admission));
    let started = host.response(2, start(id));
    assert_eq!(started["state"], "active");
    assert_eq!(started["frame_limiter_applied"], false);
    let applies: Vec<_> = host
        .broker
        .calls
        .iter()
        .filter(|call| {
            call.service == "amaterasu.frame-limiter/1" && call.payload["action"] == "apply"
        })
        .collect();
    assert_eq!(applies.len(), 1);
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
}

#[test]
fn provider_unavailable_is_nonfatal_and_does_not_claim_a_lease() {
    let mut host = Harness::new();
    let (request, _) = limiter_prepare();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    host.broker.limiter_unavailable = true;
    let started = host.response(2, start(id));
    assert_eq!(started["state"], "active");
    assert_eq!(started["frame_limiter_state"], "runtime_unavailable");
    assert_eq!(started["frame_limiter_applied"], false);
    assert!(host.broker.limiter_lease.is_none());
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
}
