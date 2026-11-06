use super::*;

fn status(operation: &str, epoch: &str) -> Value {
    json!({"action":"application_retirement_status","operation_id":operation,"expected_runtime_epoch":epoch})
}

fn retire(operation: &str, epoch: &str) -> Value {
    json!({"action":"retire_operation","operation_id":operation,"expected_runtime_epoch":epoch})
}

fn observed_pipeline() -> Pipeline {
    let mut pipeline = pipeline();
    pipeline
        .set_application_retirement_observers([("source-provider".into(), "alice".into())].into());
    pipeline
}

#[test]
fn application_retirement_waits_for_native_cleanup_and_survives_record_pruning() {
    let mut harness = Harness::new();
    let mut pipeline = observed_pipeline();
    let epoch = pipeline.records.operation_epoch().to_owned();
    let operation = format!("ale1:{epoch}:application-one");
    assert_eq!(
        harness.request(&mut pipeline, "alice", launch(&operation))["launch"]["state"],
        "active"
    );
    let active = harness.request(&mut pipeline, "source-provider", status(&operation, &epoch));
    assert_eq!(active["state"], "active");
    assert_eq!(
        active["retired"], false,
        "a paused transport does not retire its application"
    );
    assert_eq!(active["operation_id"], operation);
    assert_eq!(active["runtime_epoch"], epoch);
    harness.fail_terminate = true;
    assert_eq!(
        harness.request(
            &mut pipeline,
            "alice",
            json!({"action":"stop","launch_id":1})
        )["launch"]["state"],
        "cleanup_failed"
    );
    let blocked = harness.request(&mut pipeline, "source-provider", status(&operation, &epoch));
    assert_eq!(blocked["state"], "active");
    assert_eq!(blocked["retired"], false);
    harness.fail_terminate = false;
    assert_eq!(
        harness.request(
            &mut pipeline,
            "alice",
            json!({"action":"stop","launch_id":1})
        )["launch"]["state"],
        "stopped"
    );
    let retired = harness.request(&mut pipeline, "source-provider", status(&operation, &epoch));
    assert_eq!(retired["state"], "retired");
    assert_eq!(retired["retired"], true);
    // Metadata reclamation cannot erase already proved native retirement.
    pipeline.records.values.remove(&1);
    let next = format!("ale1:{epoch}:application-two");
    assert_eq!(
        harness.request(&mut pipeline, "alice", launch(&next))["launch"]["state"],
        "active"
    );
    assert!(
        pipeline
            .records
            .owned_operation("alice", &operation)
            .is_err()
    );
    assert_eq!(
        harness.request(&mut pipeline, "source-provider", status(&operation, &epoch))["retired"],
        true
    );
    harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":2}),
    );
}

#[test]
fn application_retirement_readback_does_not_fence_unknown_operations_or_grant_effects() {
    let mut harness = Harness::new();
    let mut pipeline = observed_pipeline();
    let epoch = pipeline.records.operation_epoch().to_owned();
    let operation = format!("ale1:{epoch}:not-yet-admitted");
    let unknown = harness.request(&mut pipeline, "source-provider", status(&operation, &epoch));
    assert_eq!(unknown["state"], "unknown");
    assert_eq!(unknown["retired"], false);
    for operation_request in [
        launch(&operation),
        json!({"action":"preflight_export","operation_id":operation,"app_id":"app-0000000000000001"}),
        json!({"action":"stop","launch_id":1}),
        json!({"action":"verify_retirement","owner":"alice","operation_id":operation,"launch_id":null}),
        json!({"action":"operation_epoch"}),
        retire(&operation, &epoch),
    ] {
        assert_eq!(
            harness.request(&mut pipeline, "source-provider", operation_request)["code"],
            "permission_denied"
        );
    }
    assert!(harness.processes.is_empty());
    assert_eq!(
        harness.request(&mut pipeline, "alice", launch(&operation))["launch"]["state"],
        "active"
    );
    harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":1}),
    );
}

#[test]
fn owner_retirement_fences_an_unadmitted_application_before_a_delayed_launch() {
    let mut harness = Harness::new();
    let mut pipeline = observed_pipeline();
    let epoch = pipeline.records.operation_epoch().to_owned();
    let operation = format!("ale1:{epoch}:bound-before-admission");
    assert_eq!(
        harness.request(&mut pipeline, "source-provider", status(&operation, &epoch))["retired"],
        false
    );
    let fenced = harness.request(&mut pipeline, "alice", retire(&operation, &epoch));
    assert_eq!(fenced["retired"], true);
    assert_eq!(fenced["fenced"], true);
    assert_eq!(
        harness.request(&mut pipeline, "alice", retire(&operation, &epoch)),
        fenced
    );
    assert_eq!(
        harness.request(&mut pipeline, "source-provider", status(&operation, &epoch))["retired"],
        true
    );
    assert_eq!(
        harness.request(&mut pipeline, "alice", launch(&operation))["code"],
        "operation_retired"
    );
    assert!(harness.processes.is_empty());
}

#[test]
fn owner_retirement_cleans_exact_native_application_and_preflight_reservations() {
    let mut harness = Harness::new();
    let mut pipeline = observed_pipeline();
    let epoch = pipeline.records.operation_epoch().to_owned();
    let preflight_op = format!("ale1:{epoch}:preflight-only");
    let preflight = harness.request(&mut pipeline, "alice", json!({"action":"preflight_export","operation_id":preflight_op,"app_id":"app-0000000000000001"}));
    assert_eq!(preflight["ok"], true, "{preflight}");
    assert_eq!(
        harness.request(&mut pipeline, "alice", retire(&preflight_op, &epoch))["retired"],
        true
    );
    assert_eq!(
        harness.request(&mut pipeline, "alice", launch(&preflight_op))["code"],
        "operation_retired"
    );
    assert!(harness.processes.is_empty());
    let operation = format!("ale1:{epoch}:native-application");
    harness.request(&mut pipeline, "alice", launch(&operation));
    let cleaned = harness.request(&mut pipeline, "alice", retire(&operation, &epoch));
    assert_eq!(cleaned["launch"]["state"], "stopped", "{cleaned}");
    assert_eq!(
        harness.request(&mut pipeline, "source-provider", status(&operation, &epoch))["retired"],
        true
    );
    assert!(
        harness
            .processes
            .iter()
            .all(|process| process.child.is_none())
    );
}

#[test]
fn application_retirement_keeps_the_source_held_until_preparation_undo_is_confirmed() {
    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 10);
    let mut pipeline = observed_pipeline();
    let epoch = pipeline.records.operation_epoch().to_owned();
    let operation = format!("ale1:{epoch}:application-with-preparation");
    let mut request = launch(&operation);
    request["global_preparation"] =
        json!([{"run":{"executable":std::env::current_exe().unwrap()}}]);
    harness.request(&mut pipeline, "alice", request);
    harness.fail_global_preparation_cleanup_once = true;
    let blocked = harness.request(&mut pipeline, "alice", retire(&operation, &epoch));
    assert_eq!(
        blocked["launch"]["cleanup_error"],
        "global_preparation_cleanup_failed"
    );
    assert_eq!(
        harness.request(&mut pipeline, "source-provider", status(&operation, &epoch))["retired"],
        false
    );
    let complete = harness.request(&mut pipeline, "alice", retire(&operation, &epoch));
    assert_eq!(complete["launch"]["state"], "stopped");
    assert_eq!(
        harness.request(&mut pipeline, "source-provider", status(&operation, &epoch))["retired"],
        true
    );
}

#[test]
fn application_retirement_observer_cannot_select_a_different_owner() {
    let mut harness = Harness::new();
    let mut pipeline = observed_pipeline();
    let epoch = pipeline.records.operation_epoch().to_owned();
    let operation = format!("ale1:{epoch}:bob-application");
    harness.request(&mut pipeline, "bob", launch(&operation));
    assert_eq!(
        harness.request(&mut pipeline, "source-provider", status(&operation, &epoch))["state"],
        "unknown"
    );
    assert_eq!(
        harness.request(&mut pipeline, "bob", status(&operation, &epoch))["state"],
        "active"
    );
    let mut forged = status(&operation, &epoch);
    forged["owner"] = json!("bob");
    assert_eq!(
        harness.request(&mut pipeline, "source-provider", forged)["code"],
        "invalid_request"
    );
    harness.request(&mut pipeline, "bob", json!({"action":"stop","launch_id":1}));
}

#[test]
fn application_retirement_after_restart_never_equates_epoch_fencing_with_cleanup() {
    let mut harness = Harness::new();
    let mut first = observed_pipeline();
    let epoch = first.records.operation_epoch().to_owned();
    let operation = format!("ale1:{epoch}:old-application");
    harness.request(&mut first, "alice", launch(&operation));
    let mut restarted = observed_pipeline();
    let proof = harness.request(
        &mut restarted,
        "source-provider",
        status(&operation, &epoch),
    );
    assert_eq!(proof["state"], "epoch_fenced");
    assert_eq!(proof["startup_reconciled"], false);
    assert_eq!(proof["retired"], false);
    assert_ne!(proof["runtime_epoch"], epoch);
    assert_eq!(
        harness.request(&mut restarted, "alice", launch(&operation))["code"],
        "stale_operation_epoch"
    );
    harness.request(&mut first, "alice", json!({"action":"stop","launch_id":1}));
}

#[test]
fn application_retirement_configuration_and_epoch_identity_are_exact() {
    let settings = |value| {
        serde_json::from_value::<Settings>(value)
            .unwrap()
            .validate()
    };
    assert!(
        settings(json!({"application_retirement_observers":{"source-provider":"alice"}})).is_ok()
    );
    for value in [
        json!({"application_retirement_observers":{"source-provider":"source-provider"}}),
        json!({"application_retirement_observers":{"source-provider":""}}),
        json!({"application_retirement_observers":{"source-provider":"alice"},"retirement_observers":["source-provider"]}),
    ] {
        assert!(settings(value).is_err());
    }
    let epoch = "a".repeat(32);
    let operation = format!("ale1:{epoch}:operation");
    for request in [
        status(&operation, &"b".repeat(32)),
        status("legacy-operation", &epoch),
        status(
            &format!("ale1:{}:operation", "A".repeat(32)),
            &"A".repeat(32),
        ),
        status(&format!("ale1:{epoch}:invalid:suffix"), &epoch),
    ] {
        assert!(
            serde_json::from_value::<Operation>(request)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}
