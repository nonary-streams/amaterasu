//! Interrupted end recovery uses the real broker with simulated native providers.
use super::*;

fn enable_recovery(harness: &mut Harness) {
    harness
        .settings
        .operation_grants
        .get_mut("transport")
        .unwrap()
        .insert("recover_session".into());
    harness
        .coordinator
        .as_mut()
        .unwrap()
        .configure_for_test(harness.settings.clone());
}

fn recover_request(harness: &mut Harness, session: &Value, operation: &str) -> Value {
    let snapshot = harness.call("transport", json!({"operation":"snapshot"}));
    json!({"operation":"recover_session","operation_id":operation,
        "target":session,"expected_host_epoch":snapshot["host_epoch"],
        "original_operation_id":"interrupted-end"})
}

#[test]
fn interrupted_end_rechecks_application_and_releases_exact_old_session() {
    for external in [true, false] {
        let mut harness = Harness::with_processing_mode(true, None, false, external);
        enable_recovery(&mut harness);
        for participant in &mut harness.settings.participants {
            participant.settings = json!({"teardown_tag":"original"});
        }
        let (mut harness, session, source) = prepare_shared_source(harness);
        let retired = retire_shared_client(&mut harness, &source["target"], "before-restart");
        assert!(
            amaterasu_module_sdk::extension_api::stream_retirement_proof(
                &retired,
                &serde_json::from_value(source["target"].clone()).unwrap(),
                None
            )
            .is_ok_and(|proof| proof.consumer_retired && proof.resources_released)
        );
        let pending = harness.call(
            "transport",
            json!({"operation":"end_session",
        "operation_id":"interrupted-end","target":session}),
        );
        assert_eq!(pending["status"], "pending");
        harness.coordinator.take();
        for participant in &mut harness.settings.participants {
            participant.settings = json!({"teardown_tag":"current"});
        }
        harness.start();
        let recovered = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"interrupted-end"}),
        );
        assert_eq!(recovered["status"], "recovery_required");
        let request = recover_request(&mut harness, &session, "recover-end");
        let mut stale = request.clone();
        stale["expected_host_epoch"] = json!("stale");
        assert_eq!(
            harness.call("transport", stale)["reason"],
            "stale_recovery_target"
        );
        let mut changed = request.clone();
        changed["target"]["generation"] = json!(2);
        assert_eq!(
            harness.call("transport", changed)["reason"],
            "session_not_recoverable"
        );
        assert_eq!(
            harness
                .coordinator
                .as_mut()
                .unwrap()
                .handle("transport", "foreign", request.clone())["reason"],
            "permission_denied"
        );
        assert_eq!(
            harness
                .coordinator
                .as_mut()
                .unwrap()
                .handle("capture", "capture", request.clone())["reason"],
            "permission_denied"
        );
        assert!(
            !harness
                .events
                .iter()
                .any(|(provider, _)| provider == "launch")
        );
        let started = harness.call("transport", request.clone());
        assert_eq!(started["status"], "pending", "{started}");
        assert_eq!(started["reconciliation_started"], true);
        let verified = wait_shared_operation(&mut harness, "recover-end");
        assert_eq!(verified["status"], "succeeded", "{verified}");
        let ended = wait_shared_operation(&mut harness, "interrupted-end");
        assert_eq!(ended["status"], "succeeded", "{ended}");
        assert_eq!(ended["target"], session);
        assert!(!harness.native_live);
        let replay = harness.call("transport", request);
        assert!(
            replay == verified || replay["reason"] == "receipt_expired",
            "{replay}"
        );
        assert!(
            harness
                .provider_calls
                .iter()
                .filter(|call| call["arguments"]["phase"] == "session.release")
                .all(|call| call["arguments"]["settings"]["teardown_tag"] == "original")
        );
        assert_eq!(
            harness
                .provider_calls
                .iter()
                .filter(|call| call["method"] == "lifecycle_callback"
                    && call["arguments"]["phase"] == "session.prepare")
                .count(),
            1,
            "recovery must never prepare a new resource incarnation"
        );
        harness.fresh_domain = true;
        let fresh = harness.call(
            "transport",
            json!({"operation":"prepare","operation_id":"after-recovery-session",
            "application_ref":{"kind":"application","app_id":"next-app"}}),
        );
        assert_eq!(fresh["status"], "pending", "{fresh}");
        let fresh = wait_shared_operation(&mut harness, "after-recovery-session");
        assert_eq!(fresh["status"], "succeeded", "{fresh}");
        assert_ne!(fresh["target"], session);
        assert_eq!(harness.call("transport", json!({"operation":"bind_application", "operation_id":"after-recovery-bind",
            "target":fresh["target"], "launch_id":43, "application_operation_id":"after-recovery-launch"}))["status"], "succeeded");
        if !external {
            continue;
        }
        let stream = harness.call("transport", json!({"operation":"prepare_stream", "operation_id":"after-recovery-stream",
            "target":fresh["target"], "intent":"initial", "requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},
            "consumer_requirements":["capture"]}));
        assert_eq!(stream["status"], "pending", "{stream}");
        let stream = wait_shared_operation(&mut harness, "after-recovery-stream");
        assert_eq!(stream["status"], "succeeded", "{stream}");
    }
}

#[test]
fn interrupted_end_requires_exact_child_retirement_before_application_query() {
    let mut harness = Harness::with_processing_mode(true, None, false, true);
    enable_recovery(&mut harness);
    let (mut harness, session, source) = prepare_shared_source(harness);
    assert_eq!(
        harness.call(
            "transport",
            json!({"operation":"end_session",
        "operation_id":"interrupted-end","target":session})
        )["status"],
        "pending"
    );
    harness.coordinator.take();
    harness.start();
    let request = recover_request(&mut harness, &session, "early-recovery");
    assert_eq!(
        harness.call("transport", request)["reason"],
        "session_consumers_not_retired"
    );
    assert!(
        !harness
            .events
            .iter()
            .any(|(provider, _)| provider == "launch")
    );
    let retired = harness.call("capture", json!({"operation":"retire_stream", "operation_id":"recover-child",
        "restart_recovery":true,"target":source["target"],
        "retirement_receipt":{"service":"video/1","operation_id":"exact-original-video","response":{"code":"not_found"}}}));
    assert_eq!(retired["status"], "pending", "{retired}");
    let mut child = Value::Null;
    for _ in 0..100 {
        child = harness.call(
            "capture",
            json!({"operation":"lookup","operation_id":"recover-child"}),
        );
        if child["status"] != "pending" {
            break;
        }
    }
    assert_eq!(child["status"], "succeeded", "{child}");
    let request = recover_request(&mut harness, &session, "recover-end");
    assert_eq!(harness.call("transport", request)["status"], "pending");
    assert_eq!(
        wait_shared_operation(&mut harness, "interrupted-end")["status"],
        "succeeded"
    );
}

#[test]
fn application_recovery_rejects_foreign_proof_and_can_retry_or_restart() {
    for restart_verification in [false, true] {
        let mut harness = Harness::with_processing_mode(true, None, false, true);
        enable_recovery(&mut harness);
        let (mut harness, session, source) = prepare_shared_source(harness);
        retire_shared_client(&mut harness, &source["target"], "before-restart");
        harness.call(
            "transport",
            json!({"operation":"end_session","operation_id":"interrupted-end","target":session}),
        );
        harness.coordinator.take();
        harness.start();
        harness.retirement_result = Some(
            json!({"ok":true,"retired":true,"owner":"foreign","launch_id":42,"operation_id":"launch-42"}),
        );
        let request = recover_request(&mut harness, &session, "first-recovery");
        assert_eq!(harness.call("transport", request)["status"], "pending");
        if restart_verification {
            harness.coordinator.take();
            harness.start();
        } else {
            let rejected = wait_shared_operation(&mut harness, "first-recovery");
            assert_eq!(rejected["status"], "rejected", "{rejected}");
        }
        assert_eq!(
            harness.call(
                "transport",
                json!({"operation":"lookup","operation_id":"interrupted-end"})
            )["status"],
            "recovery_required"
        );
        assert!(
            !harness
                .provider_calls
                .iter()
                .any(|call| call["arguments"]["phase"] == "session.release")
        );
        harness.retirement_result = None;
        let request = recover_request(&mut harness, &session, "second-recovery");
        assert_eq!(harness.call("transport", request)["status"], "pending");
        assert_eq!(
            wait_shared_operation(&mut harness, "second-recovery")["status"],
            "succeeded"
        );
        assert_eq!(
            wait_shared_operation(&mut harness, "interrupted-end")["status"],
            "succeeded"
        );
        let old = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"first-recovery"}),
        );
        assert_ne!(old["status"], "pending");
    }
}

#[test]
fn recovery_preserves_desktop_and_never_admitted_application_proofs() {
    for desktop in [true, false] {
        let mut harness = Harness::with_processing_mode(true, None, false, true);
        enable_recovery(&mut harness);
        let operation = "ale1:11111111111111111111111111111111:never-launched";
        let mut request = json!({"operation":"prepare","operation_id":"no-launch-session",
            "application_ref":if desktop {json!({"kind":"desktop"})} else {json!({"kind":"application","app_id":"app"})}});
        if !desktop {
            request["application_operation_id"] = json!(operation);
        }
        assert_eq!(harness.call("transport", request)["status"], "pending");
        let session = wait_shared_operation(&mut harness, "no-launch-session")["target"].clone();
        assert_eq!(harness.call("transport", json!({"operation":"end_session", "operation_id":"interrupted-end","target":session}))["status"], "pending");
        harness.coordinator.take();
        harness.start();
        if !desktop {
            harness.retirement_result =
                Some(json!({"ok":true,"retired":true,"never_admitted":true,
                "owner":"transport","launch_id":null,"operation_id":operation}));
        }
        let request = recover_request(&mut harness, &session, "recover-no-launch");
        let response = harness.call("transport", request);
        assert_eq!(
            response["status"],
            if desktop { "succeeded" } else { "pending" },
            "{response}"
        );
        assert_eq!(
            wait_shared_operation(&mut harness, "interrupted-end")["status"],
            "succeeded"
        );
        assert_eq!(
            harness
                .events
                .iter()
                .filter(|(provider, _)| provider == "launch")
                .count(),
            usize::from(!desktop)
        );
    }
}

#[test]
fn interrupted_release_rejects_changed_retained_method_contract() {
    let mut harness = Harness::with_provider(true);
    enable_recovery(&mut harness);
    let (mut harness, session, source) = prepare_shared_source(harness);
    retire_shared_client(&mut harness, &source["target"], "before-restart");
    harness.call(
        "transport",
        json!({"operation":"end_session", "operation_id":"interrupted-end","target":session}),
    );
    harness.coordinator.take();
    harness.settings.participants[0].callback_method.method = "replacement_callback".into();
    harness.start();
    let request = recover_request(&mut harness, &session, "changed-contract");
    let rejected = harness.call("transport", request);
    assert_eq!(
        rejected["reason"], "recovery_provider_contract_changed",
        "{rejected}"
    );
    assert!(
        !harness
            .events
            .iter()
            .any(|(provider, _)| provider == "launch")
    );
}
