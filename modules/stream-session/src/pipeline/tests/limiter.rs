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
fn start_rate_must_match_the_frozen_frame_limiter_snapshot() {
    let mut host = Harness::new();
    let (request, snapshot) = limiter_prepare();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    let mut mismatched = start(id);
    mismatched["configuration"]["configuration"]["fps"] = json!(120);
    assert_eq!(
        host.response(2, mismatched)["code"],
        "frame_limiter_configuration_mismatch"
    );
    assert!(!host.broker.calls.iter().any(|call| {
        call.service == "amaterasu.frame-limiter/1" && call.payload["action"] == "apply"
    }));
    let started = host.response(3, start(id));
    assert_eq!(started["state"], "active");
    assert_eq!(started["frame_limiter_applied"], true);
    assert_eq!(host.response(4, start(id))["state"], "active");
    let apply = host
        .broker
        .calls
        .iter()
        .find(|call| {
            call.service == "amaterasu.frame-limiter/1" && call.payload["action"] == "apply"
        })
        .unwrap();
    assert_eq!(
        apply.payload["requested_stream_fps_millihz"],
        snapshot["requested_stream_fps_millihz"]
    );
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(|call| {
                call.service == "amaterasu.frame-limiter/1" && call.payload["action"] == "apply"
            })
            .count(),
        1
    );

    let mut fractional_host = Harness::new();
    let (mut fractional_request, _) = limiter_prepare();
    fractional_request["frame_limiter_settings"]["requested_stream_fps_millihz"] = json!(59_940);
    let fractional_id = fractional_host.response(5, fractional_request)["session"]
        .as_u64()
        .unwrap();
    assert_eq!(
        fractional_host.response(6, start(fractional_id))["code"],
        "frame_limiter_configuration_mismatch"
    );
    assert!(!fractional_host.broker.calls.iter().any(|call| {
        call.service == "amaterasu.frame-limiter/1" && call.payload["action"] == "apply"
    }));
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

#[test]
fn malformed_status_does_not_clear_uncertain_apply_ownership() {
    let mut host = Harness::new();
    let (request, _) = limiter_prepare();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    host.broker.fault = Some(("amaterasu.frame-limiter/1".into(), Fault::AfterMutation));
    host.broker.limiter_malformed_status = true;
    let started = host.response(2, start(id));
    assert_eq!(started["state"], "active");
    assert_eq!(started["frame_limiter_applied"], false);
    let stopped = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "cleanup_failed");
    assert!(host.broker.limiter_lease.is_some());
}

#[test]
fn uncertain_apply_does_not_restore_a_lease_for_another_executable() {
    let mut host = Harness::new();
    let (request, _) = limiter_prepare();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    host.broker.fault = Some(("amaterasu.frame-limiter/1".into(), Fault::AfterMutation));
    host.broker.limiter_status_executable_override = Some(r"C:\Games\Other.exe".into());

    let started = host.response(2, start(id));
    assert_eq!(started["state"], "active");
    assert_eq!(started["frame_limiter_applied"], false);
    assert_eq!(started["frame_limiter_state"], "status_unverified");
    assert_eq!(host.pipeline.records[&id].limiter_lease, None);

    let stopped = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "cleanup_failed");
    assert_eq!(host.pipeline.records[&id].limiter_lease, None);
    assert!(host.broker.limiter_lease.is_some());
    assert!(!host.broker.calls.iter().any(|call| {
        call.service == "amaterasu.frame-limiter/1" && call.payload["action"] == "restore"
    }));

    host.broker.limiter_status_executable_override = None;
    let retried = host.response(4, json!({"action":"stop","session":id}));
    assert_eq!(retried["state"], "stopped");
    assert!(host.broker.limiter_lease.is_none());
}

#[test]
fn shutdown_does_not_dispatch_an_unsent_apply() {
    let mut host = Harness::new();
    let (request, _) = limiter_prepare();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    host.broker.enqueue(2, "owner", start(id));
    for _ in 0..100 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::ApplyLimiter
                && job
                    .pending
                    .as_ref()
                    .is_some_and(|pending| !pending.was_sent())
        }) {
            break;
        }
    }
    assert!(
        host.pipeline
            .job
            .as_ref()
            .is_some_and(|job| job.step == Step::ApplyLimiter)
    );
    assert!(
        host.pipeline
            .shutdown_with(&mut |command| host.broker.send(command))
    );
    assert!(!host.broker.calls.iter().any(
        |call| call.service == "amaterasu.frame-limiter/1" && call.payload["action"] == "apply"
    ));
    assert!(host.broker.limiter_lease.is_none());
}

#[test]
fn limiter_replies_must_match_the_frozen_session_and_action() {
    let mut host = Harness::new();
    let (request, _) = limiter_prepare();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    let record = host.pipeline.records.get_mut(&id).unwrap();
    let session = record.limiter_session();

    super::super::limiter::accept_apply(
        record,
        &json!({"ok":true,"version":1,"action":"apply","session_id":"foreign",
            "state":"applied","applied":true,"lease":"foreign-lease"}),
    );
    assert_eq!(record.limiter_lease, None);
    assert_eq!(record.limiter_state, "apply_unverified");

    super::super::limiter::accept_apply(
        record,
        &json!({"ok":true,"version":2,"action":"apply","session_id":session,
            "state":"applied","applied":true,"lease":"wrong-version"}),
    );
    assert_eq!(record.limiter_lease, None);

    super::super::limiter::accept_status(
        record,
        &json!({"ok":true,"version":1,"action":"status","session_id":"foreign",
            "state":"applied","leases":[{"session_id":session,
                "lease":"foreign-lease","applied":true}]}),
    );
    assert_eq!(record.limiter_lease, None);
    assert_eq!(record.limiter_state, "status_unverified");

    super::super::limiter::accept_status(
        record,
        &json!({"ok":true,"version":1,"action":"apply","session_id":session,
            "state":"applied","leases":[{"session_id":session,
                "lease":"wrong-action","applied":true}]}),
    );
    assert_eq!(record.limiter_lease, None);

    super::super::limiter::accept_apply(
        record,
        &json!({"ok":true,"version":1,"action":"apply","session_id":session,
            "state":"applied","applied":true,"lease":"owned-lease"}),
    );
    assert_eq!(record.limiter_lease.as_deref(), Some("owned-lease"));
    assert!(!super::super::limiter::accept_restore(
        record,
        &json!({"ok":true,"version":1,"action":"status","state":"restored",
            "applied":false}),
    ));
    assert_eq!(record.limiter_lease.as_deref(), Some("owned-lease"));
    assert!(super::super::limiter::accept_restore(
        record,
        &json!({"ok":true,"version":1,"action":"restore","state":"restored",
            "applied":false}),
    ));
    assert_eq!(record.limiter_lease, None);
}
