//! Process-less Desktop sessions: the application provider's prepare lease is
//! the only effect, and the coordination session is activated once it is ready.
use super::*;

const APP_ID: &str = "app-0000000000000001";

fn desktop_harness() -> Harness {
    let mut harness = Harness::new();
    harness.catalog_desktop = true;
    harness
}

fn marker_step(marker: &str) -> Value {
    let command = std::env::current_exe().unwrap();
    json!({
        "run":{"executable":command,"args":[format!("run-{marker}")]},
        "undo":{"executable":command,"args":[format!("undo-{marker}")]},
    })
}

fn launch_with_global_steps(operation: &str, markers: &[&str]) -> Value {
    let mut request = launch(operation);
    request["global_preparation"] =
        json!(markers.iter().map(|m| marker_step(m)).collect::<Vec<_>>());
    request
}

fn stop(launch: &Value) -> Value {
    json!({"action":"stop","launch_id":launch["launch"]["launch_id"]})
}

/// Every dependency call as `(service, action)`, in submission order.
fn calls(harness: &Harness) -> Vec<(&str, &str)> {
    harness
        .commands
        .iter()
        .map(|(service, payload, _)| (service.as_str(), payload["action"].as_str().unwrap()))
        .collect()
}

/// Calls made without a parent: the owned cleanup workflow.
fn cleanup_calls(harness: &Harness) -> Vec<(&str, &str)> {
    harness
        .commands
        .iter()
        .filter(|(_, _, parent)| parent.is_none())
        .map(|(service, payload, _)| (service.as_str(), payload["action"].as_str().unwrap()))
        .collect()
}

fn count(harness: &Harness, service: &str, action: &str) -> usize {
    calls(harness)
        .into_iter()
        .filter(|call| *call == (service, action))
        .count()
}

fn position(harness: &Harness, service: &str, action: &str) -> usize {
    calls(harness)
        .into_iter()
        .position(|call| call == (service, action))
        .unwrap_or_else(|| panic!("{service} {action} was never called"))
}

#[test]
fn desktop_launch_prepares_merged_steps_then_activates_without_a_process() {
    let mut harness = desktop_harness();
    harness.global_preparation_statuses = 2;
    harness.catalog_preparation = Some(json!([marker_step("app")]));
    let mut pipeline = pipeline();

    let started = harness.request(
        &mut pipeline,
        "alice",
        launch_with_global_steps("desktop-start", &["global"]),
    );

    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert!(started["launch"]["process_id"].is_null(), "{started}");
    assert!(started["launch"].get("executable_path").is_none());
    assert_eq!(started["launch"]["session_id"], 1);
    assert_eq!(harness.sessions[0].state, "active");
    assert!(harness.sessions[0].lease.is_some());
    assert!(harness.processes.is_empty());
    assert!(harness.provider_launches.is_empty());

    let call_order: Vec<_> = calls(&harness)
        .into_iter()
        .filter(|call| *call != (crate::workflow::APPLICATION, "preparation_status"))
        .collect();
    assert_eq!(
        call_order,
        [
            (crate::workflow::CATALOG, "resolve_launch"),
            (crate::workflow::SESSION, "create"),
            (crate::workflow::SESSION, "acquire"),
            (crate::workflow::APPLICATION, "prepare"),
            (crate::workflow::SESSION, "activate"),
        ]
    );
    assert_eq!(
        count(&harness, crate::workflow::APPLICATION, "preparation_status"),
        3,
        "activation must wait for the lease to report ready"
    );
    assert!(
        position(&harness, crate::workflow::SESSION, "activate")
            > position(&harness, crate::workflow::APPLICATION, "preparation_status")
    );
    assert_eq!(count(&harness, crate::workflow::APPLICATION, "launch"), 0);

    let prepare = &harness
        .commands
        .iter()
        .find(|(_, payload, _)| payload["action"] == "prepare")
        .unwrap()
        .1;
    let plan = prepare["preparation"].as_array().unwrap();
    assert_eq!(plan.len(), 2);
    assert_eq!(plan[0]["run"]["args"], json!(["run-global"]));
    assert_eq!(plan[1]["run"]["args"], json!(["run-app"]));
    assert_eq!(prepare["exit_timeout_seconds"], 10);
    assert_eq!(
        harness.preparations[0].operation,
        pipeline.records.values[&1].preparation_operation
    );
}

#[test]
fn desktop_without_preparation_activates_without_an_application_call() {
    let mut harness = desktop_harness();
    let mut pipeline = pipeline();

    let started = harness.request(&mut pipeline, "alice", launch("desktop-bare"));
    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert!(started["launch"]["process_id"].is_null());
    assert!(
        !calls(&harness)
            .iter()
            .any(|(service, _)| *service == crate::workflow::APPLICATION)
    );

    let stopped = harness.request(&mut pipeline, "alice", stop(&started));
    assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
    assert_eq!(
        cleanup_calls(&harness),
        [
            (crate::workflow::SESSION, "stop"),
            (crate::workflow::SESSION, "finish_stop"),
        ]
    );
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn desktop_stop_runs_preparation_undo_before_stopping_and_finishing_the_session() {
    let mut harness = desktop_harness();
    let mut pipeline = pipeline();
    let started = harness.request(
        &mut pipeline,
        "alice",
        launch_with_global_steps("desktop-stop", &["one", "two"]),
    );
    assert_eq!(started["launch"]["state"], "active", "{started}");

    let stopped = harness.request(&mut pipeline, "alice", stop(&started));

    assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
    assert!(stopped["launch"]["cleanup_error"].is_null());
    assert_eq!(
        cleanup_calls(&harness),
        [
            (crate::workflow::APPLICATION, "preparation_stop"),
            (crate::workflow::SESSION, "stop"),
            (crate::workflow::SESSION, "finish_stop"),
        ]
    );
    assert_eq!(harness.preparations[0].state, "complete");
    assert_eq!(harness.sessions[0].state, "stopped");
    assert!(harness.sessions[0].lease.is_none());
    // A repeated stop reports the retained terminal record without new effects.
    let before = harness.commands.len();
    assert_eq!(
        harness.request(&mut pipeline, "alice", stop(&started))["launch"]["state"],
        "stopped"
    );
    assert_eq!(harness.commands.len(), before);
}

#[test]
fn desktop_active_session_is_not_polled_for_a_process_that_does_not_exist() {
    let mut harness = desktop_harness();
    let mut pipeline = pipeline();
    let started = harness.request(
        &mut pipeline,
        "alice",
        launch_with_global_steps("desktop-idle", &["only"]),
    );
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let before = harness.commands.len();
    for _ in 0..40 {
        pipeline.tick(|command| harness.send(command)).unwrap();
    }
    assert_eq!(harness.commands.len(), before, "idle desktop made calls");
    assert!(pipeline.records.values[&1].state == State::Active);
}

#[test]
fn failed_desktop_preparation_rolls_back_and_releases_the_lease() {
    let mut harness = desktop_harness();
    harness.fail_global_preparation = true;
    let mut pipeline = pipeline();

    let failed = harness.request(
        &mut pipeline,
        "alice",
        launch_with_global_steps("desktop-failed-preparation", &["broken"]),
    );

    assert_eq!(failed["launch"]["state"], "failed", "{failed}");
    assert_eq!(failed["launch"]["failure"], "global_preparation_failed");
    assert!(failed["launch"]["process_id"].is_null());
    assert_eq!(count(&harness, crate::workflow::SESSION, "activate"), 0);
    assert_eq!(
        cleanup_calls(&harness),
        [
            (crate::workflow::APPLICATION, "preparation_stop"),
            (crate::workflow::SESSION, "stop"),
            (crate::workflow::SESSION, "fail"),
        ]
    );
    assert_eq!(harness.sessions[0].state, "stopped");
    assert!(harness.sessions[0].lease.is_none());

    // The same entry is immediately launchable under a new operation.
    harness.fail_global_preparation = false;
    let retried = harness.request(
        &mut pipeline,
        "alice",
        launch_with_global_steps("desktop-after-failure", &["fixed"]),
    );
    assert_eq!(retried["launch"]["state"], "active", "{retried}");
}

#[test]
fn desktop_cancellation_during_preparation_undoes_before_releasing_the_session() {
    let mut harness = desktop_harness();
    harness.global_preparation_statuses = 6;
    let mut pipeline = pipeline();
    harness.output = None;
    harness.incoming = Some(Response::Request {
        id: 7,
        caller: "alice".into(),
        initiator: "alice".into(),
        service: SERVICE.into(),
        payload: launch_with_global_steps("desktop-cancelled", &["slow"]),
        remaining_ms: 30_000,
    });
    let mut output = None;
    for _ in 0..100 {
        pipeline.tick(|command| harness.send(command)).unwrap();
        if !harness.preparations.is_empty() {
            harness.root_active = false;
        }
        if let Some(reply) = harness.output.take() {
            output = Some(reply);
            break;
        }
    }
    let cancelled = output.expect("cancelled launch completed");

    assert_eq!(cancelled["launch"]["state"], "cancelled", "{cancelled}");
    assert_eq!(cancelled["launch"]["failure"], "request_cancelled");
    assert_eq!(count(&harness, crate::workflow::SESSION, "activate"), 0);
    assert_eq!(
        cleanup_calls(&harness),
        [
            (crate::workflow::APPLICATION, "preparation_stop"),
            (crate::workflow::SESSION, "stop"),
            (crate::workflow::SESSION, "cancel"),
        ]
    );
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn lost_prepare_reply_reconciles_through_the_operation_id_without_replay() {
    let mut harness = desktop_harness();
    harness.drop_prepare_result = true;
    let mut pipeline = pipeline();

    let failed = harness.request(
        &mut pipeline,
        "alice",
        launch_with_global_steps("desktop-lost-prepare", &["maybe-ran"]),
    );

    assert_eq!(failed["launch"]["state"], "failed", "{failed}");
    assert_eq!(failed["launch"]["failure"], "dependency_unavailable");
    assert_eq!(
        count(&harness, crate::workflow::APPLICATION, "prepare"),
        1,
        "an ambiguous prepare must never be replayed"
    );
    let stop_request = &harness
        .commands
        .iter()
        .find(|(_, payload, _)| payload["action"] == "preparation_stop")
        .unwrap()
        .1;
    assert_eq!(
        stop_request["operation_id"],
        harness.preparations[0].operation
    );
    assert_eq!(harness.preparations[0].state, "complete");
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn desktop_undo_failure_retains_the_lease_until_an_explicit_stop_retries() {
    let mut harness = desktop_harness();
    harness.fail_global_preparation_cleanup_once = true;
    let mut pipeline = pipeline();
    let started = harness.request(
        &mut pipeline,
        "alice",
        launch_with_global_steps("desktop-undo-retry", &["undo-fails-once"]),
    );

    let blocked = harness.request(&mut pipeline, "alice", stop(&started));

    assert_eq!(blocked["launch"]["state"], "cleanup_failed", "{blocked}");
    assert_eq!(
        blocked["launch"]["cleanup_error"],
        "global_preparation_cleanup_failed"
    );
    assert!(harness.sessions[0].lease.is_some());
    assert_eq!(count(&harness, crate::workflow::SESSION, "finish_stop"), 0);
    assert_eq!(count(&harness, crate::workflow::SESSION, "stop"), 0);

    let recovered = harness.request(&mut pipeline, "alice", stop(&started));
    assert_eq!(recovered["launch"]["state"], "stopped", "{recovered}");
    assert!(recovered["launch"]["cleanup_error"].is_null());
    assert!(harness.sessions[0].lease.is_none());
    assert_eq!(count(&harness, crate::workflow::SESSION, "finish_stop"), 1);
}

#[test]
fn a_held_desktop_lease_blocks_a_second_launch_of_the_same_entry() {
    let mut harness = desktop_harness();
    let mut pipeline = pipeline();
    let first = harness.request(&mut pipeline, "alice", launch("desktop-first"));
    assert_eq!(first["launch"]["state"], "active", "{first}");

    let second = harness.request(&mut pipeline, "alice", launch("desktop-second"));
    assert_ne!(second["launch"]["state"], "active", "{second}");
    assert_eq!(harness.sessions[0].state, "active");
    assert!(harness.sessions[0].lease.is_some());

    harness.request(&mut pipeline, "alice", stop(&first));
    let third = harness.request(&mut pipeline, "alice", launch("desktop-third"));
    assert_eq!(third["launch"]["state"], "active", "{third}");
}

#[test]
fn exported_desktop_launch_keeps_preflight_idempotency_and_client_quit_cleanup() {
    let mut harness = desktop_harness();
    harness.catalog_preparation = Some(json!([marker_step("app")]));
    let mut pipeline = pipeline();
    let operation_id = "desktop-export";
    let preflight = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"preflight_export","operation_id":operation_id,"app_id":APP_ID,"global_preparation":[marker_step("global")]}),
    );
    assert_eq!(preflight["ok"], true, "{preflight}");
    assert!(preflight["executable_path"].is_null(), "{preflight}");
    assert!(harness.sessions.is_empty(), "preflight must not mutate");
    assert_eq!(
        preflight["cleanup_budget_ms"],
        20_000 + 2 * (10_000 + 3_000)
    );

    let mut export = launch_export(operation_id);
    export["global_preparation"] = json!([marker_step("global")]);
    export["preflight_token"] = preflight["preflight_token"].clone();
    let started = harness.request(&mut pipeline, "amaterasu.nvhttp", export.clone());
    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert!(started["launch"]["process_id"].is_null());
    assert!(started["launch"].get("executable_path").is_none());
    assert_eq!(
        started["launch"]["cleanup_budget_ms"],
        preflight["cleanup_budget_ms"]
    );

    // An identical retry returns the retained record without new effects.
    let effects = harness.commands.len();
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", export.clone()),
        started
    );
    assert_eq!(harness.commands.len(), effects);

    // A changed global plan under the same operation is a conflict.
    let mut changed = export.clone();
    changed["global_preparation"] = json!([marker_step("different")]);
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", changed)["code"],
        "operation_conflict"
    );
    let mut direct = launch(operation_id);
    direct["global_preparation"] = export["global_preparation"].clone();
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", direct)["code"],
        "operation_conflict"
    );

    // No executable exists, so the limiter verifier never accepts this launch.
    let verify = json!({
        "action":"verify_frame_limiter_snapshot",
        "application_operation_id":operation_id,
        "app_id":APP_ID,
        "catalog_revision":preflight["catalog_revision"],
        "frame_limiter_executable_path":"/usr/bin/anything",
    });
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.rtsp", verify)["code"],
        "invalid_request"
    );

    // Client quit reaches launch as an owner stop and undoes in reverse.
    let quit = harness.request(&mut pipeline, "amaterasu.nvhttp", stop(&started));
    assert_eq!(quit["launch"]["state"], "stopped", "{quit}");
    assert_eq!(
        cleanup_calls(&harness),
        [
            (crate::workflow::APPLICATION, "preparation_stop"),
            (crate::workflow::SESSION, "stop"),
            (crate::workflow::SESSION, "finish_stop"),
        ]
    );
    let lookup = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"lookup","operation_id":operation_id}),
    );
    assert_eq!(lookup["launch"]["state"], "stopped");
    assert!(lookup["launch"]["process_id"].is_null());
    assert_eq!(lookup["launch"]["cleanup_budget_ms"], 46_000);
}

#[test]
fn desktop_catalog_target_rejects_process_identity_and_elevation() {
    let accepted = LaunchTarget::from_catalog_entry(
        &json!({"name":"Desktop","launch":{"kind":"desktop"}}),
        &[],
    )
    .unwrap();
    assert!(matches!(
        accepted,
        LaunchTarget::Desktop {
            exit_timeout_seconds: 10,
            working_directory: None,
            ..
        }
    ));
    assert!(!accepted.is_monitored_after_activation());

    for launch in [
        json!({"kind":"desktop","program":"/usr/bin/true"}),
        json!({"kind":"desktop","path":"/usr/bin/true"}),
        json!({"kind":"desktop","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}),
        json!({"kind":"desktop","steam_id":"480"}),
        json!({"kind":"desktop","elevated":true}),
        json!({"kind":"desktop","exit_timeout_seconds":301}),
    ] {
        assert_eq!(
            LaunchTarget::from_catalog_entry(&json!({"name":"Desktop","launch":launch}), &[])
                .unwrap_err(),
            "invalid_catalog_response",
            "{launch}"
        );
    }
    for entry in [
        json!({"name":"Desktop","elevated":true,"launch":{"kind":"desktop"}}),
        json!({"name":"Desktop","steam_id":"480","launch":{"kind":"desktop"}}),
        json!({"name":"Desktop","steam_install_dir":"/games/steam","launch":{"kind":"desktop"}}),
    ] {
        assert_eq!(
            LaunchTarget::from_catalog_entry(&entry, &[]).unwrap_err(),
            "invalid_catalog_response",
            "{entry}"
        );
    }
}

#[test]
fn desktop_cleanup_budget_covers_every_configured_undo() {
    let step = |undo_ms: u64| -> Value {
        json!({
            "run":{"executable":std::env::current_exe().unwrap()},
            "undo":{"executable":std::env::current_exe().unwrap(),"timeout_ms":undo_ms},
        })
    };
    let target = |timeout: u64, steps: Vec<Value>| {
        LaunchTarget::from_catalog_entry(
            &json!({"name":"Desktop","launch":{
                "kind":"desktop","exit_timeout_seconds":timeout,"preparation":steps,
            }}),
            &[],
        )
        .unwrap()
    };
    let budget = |target: &LaunchTarget| cleanup_budget(Some(target)).as_millis();

    assert_eq!(budget(&target(10, vec![])), 20_000);
    assert_eq!(
        budget(&target(10, vec![step(5_000), step(30_000)])),
        20_000 + (5_000 + 3_000) + (30_000 + 3_000)
    );

    let raw = LaunchTarget::from_catalog_entry(
        &json!({
            "name":"Desktop","launch":{"kind":"desktop","exit_timeout_seconds":40},
            "prep_cmd":[{"do":"raw","undo":"raw-undo"}],
        }),
        &[],
    )
    .unwrap();
    assert_eq!(budget(&raw), 20_000 + 40_000 + 3_000);
}
