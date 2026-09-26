use super::*;

fn display_request() -> Value {
    json!({"expected_topology_revision":"0123456789abcdef0123456789abcdef",
        "configuration":{"displays":[
        {"id":"first","width":1920,"height":1080,"refresh":{"numerator":60,"denominator":1},"x":0,"y":0},
        {"id":"second","width":1280,"height":720,"refresh":{"numerator":60,"denominator":1},"x":1920,"y":0}
    ]},"mode_policies":[
        {"display_id":"first","remapping":{},"context":{"resolution_is_automatic":true,"refresh_rate_is_automatic":true,"client_display_mode_override":false,"optimize_game_settings":false}},
        {"display_id":"second","remapping":{},"context":{"resolution_is_automatic":true,"refresh_rate_is_automatic":true,"client_display_mode_override":false,"optimize_game_settings":false}}
    ]})
}

#[test]
fn selected_display_configuration_options_reach_apply_and_cleanup() {
    let mut two_monitor = display_request();
    two_monitor["mode_policies"][0]["configuration_option"] = json!("ensure_only_display");
    two_monitor["mode_policies"][1]["configuration_option"] = json!("disabled");
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = two_monitor.clone();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["display_state"], "applied");
    let applied = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == "amaterasu.display/1" && call.payload["action"] == "apply")
        .unwrap();
    assert_eq!(
        applied.payload["mode_policies"],
        two_monitor["mode_policies"]
    );
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert!(host.broker.display_lease.is_none());

    for option in ["ensure_active", "ensure_primary"] {
        let mut sole_target = display_request();
        sole_target["configuration"]["displays"]
            .as_array_mut()
            .unwrap()
            .pop();
        sole_target.as_object_mut().unwrap().remove("mode_policies");
        sole_target["mode_policy"] = json!({"display_id":"first",
            "configuration_option":option,"remapping":{},"context":{
                "resolution_is_automatic":true,"refresh_rate_is_automatic":true,
                "client_display_mode_override":false,"optimize_game_settings":false}});
        let mut host = Harness::new();
        let mut request = prepare();
        request["display_request"] = sole_target.clone();
        let id = host.response(1, request)["session"].as_u64().unwrap();
        assert_eq!(host.response(2, start(id))["display_state"], "applied");
        let applied = host
            .broker
            .calls
            .iter()
            .find(|call| call.service == "amaterasu.display/1" && call.payload["action"] == "apply")
            .unwrap();
        assert_eq!(applied.payload["mode_policy"], sole_target["mode_policy"]);
        assert!(applied.payload.get("mode_policies").is_none());
        assert_eq!(
            host.response(3, json!({"action":"stop","session":id}))["state"],
            "stopped"
        );
    }
}

#[test]
fn unknown_display_configuration_option_fails_before_provider_call() {
    for option in ["ensure_only", "ENSURE_ACTIVE", ""] {
        let mut request = prepare();
        request["display_request"] = display_request();
        request["display_request"]["mode_policies"][0]["configuration_option"] = json!(option);
        let mut host = Harness::new();
        assert_eq!(host.response(1, request)["code"], "invalid_display_request");
        assert!(
            !host
                .broker
                .calls
                .iter()
                .any(|call| call.service == "amaterasu.display/1")
        );
    }
}

#[test]
fn full_frozen_topology_applies_once_and_restores_on_stop() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    let prepared = host.response(1, request.clone());
    let id = prepared["session"].as_u64().unwrap();
    assert_eq!(prepared["display_request"], request["display_request"]);
    assert_eq!(host.response(2, request)["session"], id);
    assert_eq!(host.response(3, start(id))["display_state"], "applied");
    let applies: Vec<_> = host
        .broker
        .calls
        .iter()
        .filter(|call| call.service == "amaterasu.display/1" && call.payload["action"] == "apply")
        .collect();
    assert_eq!(applies.len(), 1);
    assert_eq!(
        applies[0].payload["configuration"],
        display_request()["configuration"]
    );
    assert_eq!(
        applies[0].payload["mode_policies"],
        display_request()["mode_policies"]
    );
    assert_eq!(
        applies[0].payload["expected_topology_revision"],
        display_request()["expected_topology_revision"]
    );
    assert_eq!(
        applies[0].payload["operation_id"],
        host.pipeline.records[&id].display_operation()
    );
    assert!(applies[0].payload.get("mode_policy").is_none());
    assert_eq!(
        host.response(4, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert!(host.broker.display_lease.is_none());
}

#[test]
fn malformed_or_absent_display_request_never_mutates() {
    let mut absent = Harness::new();
    let id = absent.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(absent.response(2, start(id))["state"], "active");
    assert!(
        !absent
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.display/1")
    );
    for mutation in [
        ("configuration", json!({"displays":[]})),
        ("mode_policy", json!({"display_id":"missing"})),
    ] {
        let mut request = prepare();
        request["display_request"] = display_request();
        request["display_request"][mutation.0] = mutation.1;
        assert_eq!(
            Harness::new().response(1, request)["code"],
            "invalid_display_request"
        );
    }
}

#[test]
fn incomplete_duplicate_and_unknown_display_policies_are_rejected() {
    let complete = display_request();
    let mut variants = Vec::new();
    let mut no_policy = complete.clone();
    no_policy.as_object_mut().unwrap().remove("mode_policies");
    variants.push(no_policy);
    let mut missing_target = complete.clone();
    missing_target["mode_policies"]
        .as_array_mut()
        .unwrap()
        .pop();
    variants.push(missing_target);
    let mut duplicate_target = complete.clone();
    duplicate_target["mode_policies"][1]["display_id"] = json!("FIRST");
    variants.push(duplicate_target);
    let mut unknown_display_field = complete.clone();
    unknown_display_field["configuration"]["displays"][0]["unexpected"] = json!(true);
    variants.push(unknown_display_field);
    let mut unknown_policy_field = complete.clone();
    unknown_policy_field["mode_policies"][0]["unexpected"] = json!(true);
    variants.push(unknown_policy_field);
    let mut unknown_context_field = complete.clone();
    unknown_context_field["mode_policies"][0]["context"]["unexpected"] = json!(true);
    variants.push(unknown_context_field);
    let mut unknown_remapping_field = complete.clone();
    unknown_remapping_field["mode_policies"][0]["remapping"]["unexpected"] = json!([]);
    variants.push(unknown_remapping_field);
    let mut unknown_rule_field = complete.clone();
    unknown_rule_field["mode_policies"][0]["remapping"]["mixed"] = json!([{"unexpected":true}]);
    variants.push(unknown_rule_field);
    for display_request in variants {
        let mut request = prepare();
        request["display_request"] = display_request;
        let mut host = Harness::new();
        assert_eq!(host.response(1, request)["code"], "invalid_display_request");
        assert!(
            !host
                .broker
                .calls
                .iter()
                .any(|call| call.service == "amaterasu.display/1")
        );
    }
}

#[test]
fn rejected_apply_keeps_stream_alive_without_lease() {
    let mut host = Harness::new();
    host.broker.display_apply_failure = true;
    let mut request = prepare();
    request["display_request"] = display_request();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    assert!(host.broker.display_lease.is_none());
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
}

#[test]
fn lost_apply_result_adopts_only_its_operation_and_restores() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    host.broker.fault = Some(("amaterasu.display/1:apply".into(), Fault::AfterMutation));
    let response = host.response(2, start(id));
    assert_eq!(response["state"], "active");
    assert_eq!(
        host.pipeline.records[&id].display_lease.as_deref(),
        Some("display-0000000000000001")
    );
    assert_eq!(response["display_state"], "leased");
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(
                |call| call.service == "amaterasu.display/1" && call.payload["action"] == "apply"
            )
            .count(),
        1
    );
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert!(host.broker.display_lease.is_none());
}

#[test]
fn recovered_provider_journal_keeps_operation_identity_through_stop() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    host.broker.display_restart_on_lost_apply = true;
    host.broker.fault = Some(("amaterasu.display/1:apply".into(), Fault::AfterMutation));
    assert_eq!(
        host.response(2, start(id))["display_state"],
        "recovery_required"
    );
    assert_eq!(
        host.pipeline.records[&id].display_lease.as_deref(),
        Some("display-0000000000000001")
    );
    assert_eq!(
        host.broker.display_operation.as_deref(),
        Some(host.pipeline.records[&id].display_operation().as_str())
    );
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert!(
        host.broker.calls.iter().any(
            |call| call.service == "amaterasu.display/1" && call.payload["action"] == "recover"
        )
    );
}

#[test]
fn same_operation_recovery_reply_retains_lease_for_recover() {
    let mut host = Harness::new();
    host.broker.display_apply_recovery_reply = true;
    host.broker.display_recovery_required = false;
    let mut request = prepare();
    request["display_request"] = display_request();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    assert_eq!(
        host.response(2, start(id))["display_state"],
        "recovery_required"
    );
    host.broker.display_recovery_required = true;
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert!(
        host.broker.calls.iter().any(
            |call| call.service == "amaterasu.display/1" && call.payload["action"] == "recover"
        )
    );
}

#[test]
fn concurrent_owner_lease_after_preflight_is_never_restored_by_this_session() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    host.broker.display_race_on_lost_apply = true;
    host.broker.fault = Some(("amaterasu.display/1:apply".into(), Fault::BeforeMutation));
    assert_eq!(host.response(2, start(id))["state"], "active");
    assert_eq!(
        host.pipeline.records[&id].display_state,
        "status_unverified"
    );
    assert!(host.pipeline.records[&id].display_lease.is_none());
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "cleanup_failed"
    );
    assert_eq!(
        host.broker.display_lease.as_deref(),
        Some("display-0000000000000002")
    );
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.display/1"
                && matches!(call.payload["action"].as_str(), Some("restore" | "recover")))
    );
}

#[test]
fn wrong_display_operation_cannot_be_adopted_or_restored() {
    let mut host = Harness::new();
    host.broker.display_wrong_apply_operation = true;
    host.broker.display_wrong_status_operation = true;
    let mut request = prepare();
    request["display_request"] = display_request();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    assert!(host.pipeline.records[&id].display_lease.is_none());
    assert_eq!(
        host.pipeline.records[&id].display_state,
        "status_unverified"
    );
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "cleanup_failed"
    );
    assert!(host.broker.display_lease.is_some());
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.display/1"
                && matches!(call.payload["action"].as_str(), Some("restore" | "recover")))
    );
}

#[test]
fn display_revision_is_required_and_caller_cannot_supply_operation_identity() {
    let mut request = prepare();
    request["display_request"] = display_request();
    request["display_request"]["expected_topology_revision"] = json!("bad");
    assert_eq!(
        Harness::new().response(1, request)["code"],
        "invalid_display_request"
    );

    let mut request = prepare();
    request["display_request"] = display_request();
    request["display_request"]["operation_id"] = json!("forged");
    assert_eq!(
        Harness::new().response(1, request)["code"],
        "invalid_request"
    );
}

#[test]
fn failed_restore_retains_lease_for_stop_retry_and_recovers() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.broker.display_restore_failures = 1;
    let first = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(first["state"], "cleanup_failed");
    assert!(host.broker.display_lease.is_some());
    assert_eq!(
        host.response(4, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert!(host.broker.display_lease.is_none());
}

#[test]
fn another_stream_does_not_reapply_an_owned_display_lease() {
    let mut host = Harness::new();
    let mut first = prepare();
    first["display_request"] = display_request();
    let first_id = host.response(1, first)["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(first_id))["state"], "active");

    let mut second = prepare();
    second["operation_id"] = json!("second-prepare");
    second["display_request"] = display_request();
    let second_id = host.response(3, second)["session"].as_u64().unwrap();
    assert_eq!(host.response(4, start(second_id))["state"], "active");
    assert_eq!(host.pipeline.records[&second_id].display_state, "busy");
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(
                |call| call.service == "amaterasu.display/1" && call.payload["action"] == "apply"
            )
            .count(),
        1
    );
    assert_eq!(
        host.response(5, json!({"action":"stop","session":second_id}))["state"],
        "stopped"
    );
    assert!(host.broker.display_lease.is_some());
    assert_eq!(
        host.response(6, json!({"action":"stop","session":first_id}))["state"],
        "stopped"
    );
    assert!(host.broker.display_lease.is_none());
}

#[test]
fn video_open_failure_restores_display_before_session_cleanup() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    host.broker.fail_open = Some("amaterasu.video/1".into());
    assert_eq!(host.response(2, start(id))["state"], "failed");
    assert!(host.broker.display_lease.is_none());
    let restore = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.display/1" && call.payload["action"] == "restore"
        })
        .unwrap();
    let finish = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.session/1" && call.payload["action"] == "finish_stop"
        })
        .unwrap();
    assert!(restore < finish);
}
