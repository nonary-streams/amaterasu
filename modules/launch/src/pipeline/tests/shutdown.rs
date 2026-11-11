use super::*;

fn drain(pipeline: &mut Pipeline, harness: &mut Harness) {
    for _ in 0..100 {
        if pipeline.quiesce() {
            return;
        }
        pipeline.tick(|command| harness.send(command)).unwrap();
    }
    panic!("launch shutdown did not retire its owned effects");
}

fn begin_launch(pipeline: &mut Pipeline, harness: &mut Harness, operation: &str) {
    harness.incoming = Some(Response::Request {
        id: 7,
        caller: "alice".into(),
        initiator: "alice".into(),
        service: SERVICE.into(),
        payload: launch(operation),
        remaining_ms: 30_000,
    });
    pipeline.tick(|command| harness.send(command)).unwrap();
}

#[test]
fn quiesce_drains_a_direct_sdk_launch_before_releasing_its_session() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.application_stopping_statuses = 2;
    assert_eq!(
        harness.request(&mut pipeline, "alice", launch("direct-sdk"))["launch"]["state"],
        "active"
    );
    let cleanup_start = harness.commands.len();
    assert!(!pipeline.quiesce());
    assert!(harness.processes[0].child.is_some());
    assert!(harness.sessions[0].lease.is_some());

    drain(&mut pipeline, &mut harness);

    assert!(harness.processes[0].child.is_none());
    assert!(harness.sessions[0].lease.is_none());
    assert!(pipeline.records.values[&1].state == State::Stopped);
    let cleanup: Vec<_> = harness.commands[cleanup_start..]
        .iter()
        .map(|(_, payload, parent)| {
            assert!(parent.is_none());
            payload["action"].as_str().unwrap()
        })
        .collect();
    assert_eq!(
        cleanup,
        [
            "stop",
            "terminate",
            "status",
            "status",
            "status",
            "finish_stop"
        ]
    );
    assert!(pipeline.records.values[&1].native_retirement_confirmed);
}

#[test]
fn quiesce_settles_a_queued_spawn_before_lookup_and_cancels_the_original_caller() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.queue_spawn = true;
    begin_launch(&mut pipeline, &mut harness, "pending-start");
    for _ in 0..30 {
        if harness.queued_spawn.is_some() {
            break;
        }
        pipeline.tick(|command| harness.send(command)).unwrap();
    }
    assert!(harness.queued_spawn.is_some());
    assert!(harness.output.is_none());
    assert!(!pipeline.quiesce());
    assert_eq!(pipeline.records.values[&1].outstanding_request, Some(50));
    assert!(pipeline.only_job().unwrap().pending.is_none());

    harness.drop_spawn_result = false;
    for _ in 0..3 {
        pipeline.tick(|command| harness.send(command)).unwrap();
    }
    assert!(harness.sessions[0].lease.is_some());
    assert!(!pipeline.records.values[&1].native_retirement_confirmed);
    harness.complete_queued_spawn();
    drain(&mut pipeline, &mut harness);

    assert!(harness.queued_spawn.is_none());
    assert!(harness.processes[0].child.is_none());
    assert!(harness.sessions[0].lease.is_none());
    assert!(pipeline.records.values[&1].outstanding_request.is_none());
    let output = harness.output.as_ref().unwrap();
    assert_eq!(output["launch"]["state"], "cancelled");
    assert_eq!(output["launch"]["failure"], "shutdown_requested");
    assert!(
        !harness
            .commands
            .iter()
            .any(|(_, payload, _)| payload["action"] == "activate")
    );
}

#[test]
fn quiesce_reconciles_a_committed_spawn_without_replaying_startup() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    begin_launch(&mut pipeline, &mut harness, "committed-start");
    for _ in 0..30 {
        if !harness.processes.is_empty() {
            break;
        }
        pipeline.tick(|command| harness.send(command)).unwrap();
    }
    assert_eq!(harness.processes.len(), 1);
    assert!(pipeline.records.values[&1].process_id.is_none());
    assert!(pipeline.only_job().unwrap().pending.is_some());

    drain(&mut pipeline, &mut harness);

    assert_eq!(harness.processes.len(), 1);
    assert!(harness.processes[0].child.is_none());
    assert_eq!(
        harness.output.as_ref().unwrap()["launch"]["state"],
        "cancelled"
    );
    assert!(harness.sessions[0].lease.is_none());
    assert!(harness.commands.iter().any(|(service, payload, _)| {
        service == crate::workflow::APPLICATION && payload["action"] == "lookup"
    }));
}

#[test]
fn repeated_quiesce_preserves_a_pending_cleanup_and_its_deadline() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.request(&mut pipeline, "alice", launch("existing-cleanup"));
    assert!(!pipeline.quiesce());
    pipeline.tick(|command| harness.send(command)).unwrap();
    let job = pipeline.only_job().unwrap();
    let deadline = job.deadline;
    let pending = job.pending;
    assert!(pending.is_some());
    for _ in 0..10 {
        assert!(!pipeline.quiesce());
        let job = pipeline.only_job().unwrap();
        assert_eq!(job.deadline, deadline);
        assert_eq!(job.pending, pending);
    }
    drain(&mut pipeline, &mut harness);
}

#[test]
fn cleanup_failure_blocks_readiness_without_replaying_undo_or_blocking_other_launches() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.fail_application_cleanup_once = true;
    harness.request(&mut pipeline, "alice", launch("failed-cleanup"));
    let failed = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":1}),
    );
    assert_eq!(failed["launch"]["state"], "cleanup_failed");
    harness.catalog_desktop = true;
    harness.request(&mut pipeline, "bob", launch("desktop"));
    let start = harness.commands.len();
    for _ in 0..40 {
        assert!(!pipeline.quiesce());
        pipeline.tick(|command| harness.send(command)).unwrap();
    }
    assert!(pipeline.records.values[&2].state == State::Stopped);
    assert!(pipeline.records.values[&1].state == State::CleanupFailed);
    assert!(harness.sessions[0].lease.is_some());
    assert!(harness.sessions[1].lease.is_none());
    assert!(
        !harness.commands[start..]
            .iter()
            .any(|(service, payload, _)| {
                service == crate::workflow::APPLICATION && payload["action"] == "terminate"
            })
    );
    let status = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"status","launch_id":1}),
    );
    assert_eq!(status["launch"]["state"], "cleanup_failed");
    let retried = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":1}),
    );
    assert_eq!(retried["launch"]["state"], "stopped");
    assert!(pipeline.quiesce());
}

#[test]
fn quiesce_fences_an_expired_delivered_mutation_until_completion_is_proved() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.queue_spawn = true;
    begin_launch(&mut pipeline, &mut harness, "unsettled");
    for _ in 0..30 {
        if harness.queued_spawn.is_some() {
            break;
        }
        pipeline.tick(|command| harness.send(command)).unwrap();
    }
    assert!(!pipeline.quiesce());
    harness.drop_spawn_result = false;
    harness.reply = Some(Response::error(
        "deadline_exceeded",
        "delivered request expired",
    ));
    pipeline.tick(|command| harness.send(command)).unwrap();
    assert!(pipeline.records.values[&1].state == State::CleanupFailed);
    assert_eq!(pipeline.records.values[&1].outstanding_request, Some(50));
    assert!(harness.sessions[0].lease.is_some());
    assert!(!pipeline.quiesce());
    // An exact provider completion allows the owner's explicit retry to proceed.
    harness.complete_queued_spawn();
    let retried = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":1}),
    );
    assert_eq!(retried["launch"]["state"], "cancelled");
    assert!(pipeline.quiesce());
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn quiesce_rejects_new_launch_and_display_admissions_without_native_effects() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    assert!(pipeline.quiesce());
    for payload in [
        launch("late"),
        launch_export("late-export"),
        json!({"action":"preflight_export","operation_id":"late-preflight","app_id":"app-0000000000000001"}),
    ] {
        assert_eq!(
            harness.request(&mut pipeline, "alice", payload)["code"],
            "shutting_down"
        );
    }
    for payload in [
        json!({
            "action":"prepare_display_admission", "application_operation_id":"late",
            "global_revision":null, "global_overrides":{}, "catalog_revision":1,
            "application_display_overrides":{}, "certificate_id":"aa".repeat(32),
            "client_display_overrides":{}, "rtsp_operation_id":"rtsp", "request_sha256":"bb".repeat(32)
        }),
        json!({
            "action":"verify_display_admission", "token":"cc".repeat(32),
            "application_operation_id":"late", "rtsp_operation_id":"rtsp", "request_sha256":"bb".repeat(32),
            "display_policy_snapshot":{}, "certificate_id":"aa".repeat(32), "catalog_revision":1
        }),
    ] {
        assert_eq!(
            harness.request(&mut pipeline, "amaterasu.nvhttp", payload)["code"],
            "shutting_down"
        );
    }
    assert!(harness.commands.is_empty());
    assert!(harness.processes.is_empty());
    assert!(harness.sessions.is_empty());
    assert!(pipeline.records.values.is_empty());
    assert!(pipeline.quiesce());
}

#[test]
fn quiesce_retires_preflight_without_starting_native_work() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    let preflight = harness.request(&mut pipeline, "alice", json!({
        "action":"preflight_export","operation_id":"preflight-only","app_id":"app-0000000000000001"
    }));
    assert_eq!(preflight["ok"], true);
    let start = harness.commands.len();
    drain(&mut pipeline, &mut harness);
    assert!(pipeline.records.values[&1].state == State::Cancelled);
    assert_eq!(harness.commands.len(), start);
    assert!(harness.sessions.is_empty());
    assert!(harness.processes.is_empty());
}

#[test]
fn expired_pending_cleanup_is_settled_and_reports_unresolved_retirement() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.request(&mut pipeline, "alice", launch("cleanup-deadline"));
    assert!(!pipeline.quiesce());
    pipeline.tick(|command| harness.send(command)).unwrap();
    let job = pipeline.only_job_mut().unwrap();
    assert_eq!(job.step, Step::StopSession);
    assert!(job.pending.is_some());
    job.deadline = Instant::now();
    pipeline.tick(|command| harness.send(command)).unwrap();
    // An exhausted cleanup budget must not turn session-stop uncertainty
    // into successful retirement or restart the cleanup deadline.
    for _ in 0..3 {
        pipeline.tick(|command| harness.send(command)).unwrap();
    }
    assert!(pipeline.records.values[&1].state == State::CleanupFailed);
    assert_eq!(
        pipeline.records.values[&1].cleanup_error,
        Some("deadline_exceeded")
    );
    assert!(pipeline.records.values[&1].outstanding_request.is_none());
    assert_eq!(harness.released, 0);
    assert!(!pipeline.quiesce());
    assert!(harness.sessions[0].lease.is_some());
}

#[test]
fn shutdown_announcement_gates_admission_without_stopping_an_active_sdk_launch() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.request(&mut pipeline, "alice", launch("announced-active"));
    let commands = harness.commands.len();

    pipeline.prepare_shutdown(1_000);

    assert!(!pipeline.quiescing);
    assert!(pipeline.only_job().is_none());
    assert!(pipeline.records.values[&1].state == State::Active);
    assert!(harness.processes[0].child.is_some());
    assert!(harness.sessions[0].lease.is_some());
    assert_eq!(harness.commands.len(), commands);
    assert_eq!(
        harness.request(
            &mut pipeline,
            "alice",
            launch("rejected-after-announcement")
        )["code"],
        "shutting_down"
    );
    assert!(pipeline.records.values[&1].state == State::Active);
    assert!(harness.processes[0].child.is_some());
    assert_eq!(harness.commands.len(), commands);

    drain(&mut pipeline, &mut harness);
    assert!(harness.processes[0].child.is_none());
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn later_quiesce_caps_long_application_cleanup_to_the_original_host_deadline() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.catalog_exit_timeout_seconds = Some(300);
    harness.request(&mut pipeline, "alice", launch("long-configured-cleanup"));
    assert!(pipeline.records.values[&1].cleanup_budget_ms > 300_000);
    pipeline.prepare_shutdown(1_000);
    let announced_deadline = pipeline.shutdown_deadline.unwrap();
    // Advancing the host announcement cannot renew its first deadline.
    pipeline.prepare_shutdown(20_000);
    assert_eq!(pipeline.shutdown_deadline, Some(announced_deadline));

    assert!(!pipeline.quiesce());
    assert_eq!(pipeline.only_job().unwrap().deadline, announced_deadline);
    pipeline.tick(|command| harness.send(command)).unwrap();
    let pending = pipeline.only_job().unwrap().pending;
    assert!(pending.is_some());
    assert_eq!(pipeline.only_job().unwrap().deadline, announced_deadline);
    assert!(!pipeline.quiesce());
    assert_eq!(pipeline.only_job().unwrap().deadline, announced_deadline);
    assert_eq!(pipeline.only_job().unwrap().pending, pending);
    drain(&mut pipeline, &mut harness);
}

#[test]
fn host_deadline_bounds_consumer_requested_cleanup_before_provider_quiesce() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.catalog_exit_timeout_seconds = Some(300);
    harness.request(
        &mut pipeline,
        "alice",
        launch("consumer-stop-before-quiesce"),
    );
    pipeline.prepare_shutdown(1_000);
    let deadline = pipeline.shutdown_deadline.unwrap();
    harness.incoming = Some(Response::Request {
        id: 7,
        caller: "alice".into(),
        initiator: "alice".into(),
        service: SERVICE.into(),
        payload: json!({"action":"stop","launch_id":1}),
        remaining_ms: 30_000,
    });
    pipeline.tick(|command| harness.send(command)).unwrap();
    assert!(!pipeline.quiescing);
    assert_eq!(pipeline.only_job().unwrap().deadline, deadline);
    // The consumer may still retire its owned launch while the host waits to
    // quiesce this provider, but the resulting cleanup shares the host budget.
    pipeline.tick(|command| harness.send(command)).unwrap();
    let job = pipeline.only_job().unwrap();
    assert!(job.cleanup);
    assert_eq!(job.deadline, deadline);
    assert!(job.pending.is_some());
    drain(&mut pipeline, &mut harness);
}

#[test]
fn expired_host_deadline_preserves_a_pending_request_and_unresolved_lease() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.request(&mut pipeline, "alice", launch("host-expired-pending"));
    assert!(!pipeline.quiesce());
    pipeline.tick(|command| harness.send(command)).unwrap();
    assert!(pipeline.only_job().unwrap().pending.is_some());
    harness.reply = None;

    pipeline.prepare_shutdown(0);
    assert!(!pipeline.quiesce());
    assert_eq!(
        pipeline.only_job().unwrap().deadline,
        pipeline.shutdown_deadline.unwrap()
    );
    pipeline.tick(|command| harness.send(command)).unwrap();

    assert!(pipeline.records.values[&1].state == State::CleanupFailed);
    assert_eq!(pipeline.records.values[&1].outstanding_request, Some(50));
    assert_eq!(
        pipeline.records.values[&1].cleanup_error,
        Some("request_cancellation_unconfirmed")
    );
    assert!(!pipeline.records.values[&1].native_retirement_confirmed);
    assert!(harness.sessions[0].lease.is_some());
    assert!(harness.processes[0].child.is_some());
    assert!(!pipeline.quiesce());
}

#[test]
fn shutdown_announcement_preserves_pending_startup_until_its_consumer_drains() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.queue_spawn = true;
    begin_launch(&mut pipeline, &mut harness, "announced-pending-start");
    for _ in 0..30 {
        if harness.queued_spawn.is_some() {
            break;
        }
        pipeline.tick(|command| harness.send(command)).unwrap();
    }
    assert!(harness.queued_spawn.is_some());
    let before = pipeline.only_job().unwrap().clone();
    let commands = harness.commands.len();
    pipeline.prepare_shutdown(1_000);
    let announced = pipeline.shutdown_deadline.unwrap();
    pipeline.prepare_shutdown(20_000);
    assert_eq!(pipeline.shutdown_deadline, Some(announced));
    let after = pipeline.only_job().unwrap();
    assert_eq!(after.step, before.step);
    assert_eq!(after.pending, before.pending);
    assert_eq!(after.deadline, before.deadline);
    assert_eq!(after.request, before.request);
    assert!(!after.cleanup);
    assert!(pipeline.records.values[&1].state == State::Starting);
    assert!(pipeline.records.values[&1].outstanding_request.is_none());
    assert!(harness.queued_spawn.is_some());
    assert!(harness.sessions[0].lease.is_some());
    assert_eq!(harness.commands.len(), commands);
    pipeline.prepare_shutdown(500);
    let tightened = pipeline.shutdown_deadline.unwrap();
    assert!(tightened < announced);
    assert!(!pipeline.quiesce());
    assert_eq!(pipeline.only_job().unwrap().deadline, tightened);
    harness.complete_queued_spawn();
    drain(&mut pipeline, &mut harness);
}

#[test]
fn oversized_host_budget_never_extends_the_announced_deadline() {
    let mut pipeline = pipeline();
    pipeline.prepare_shutdown(1_000);
    let deadline = pipeline.shutdown_deadline.unwrap();
    // Some platforms reject this Instant range; conservative rejection may
    // tighten the deadline, but a repeated announcement must never renew it.
    pipeline.prepare_shutdown(u64::MAX);
    assert!(pipeline.shutdown_deadline.unwrap() <= deadline);
    assert!(pipeline.quiesce());
}
