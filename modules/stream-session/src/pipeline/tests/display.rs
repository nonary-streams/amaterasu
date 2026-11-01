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

fn inactive_ensure_active_request() -> Value {
    json!({
        "expected_topology_revision":"0123456789abcdef0123456789abcdef",
        "configuration":{"displays":[
            {"id":"active-one","width":1920,"height":1080,
                "refresh":{"numerator":60,"denominator":1},"x":0,"y":0,
                "rotation":"identity","hdr_enabled":false},
            {"id":"active-two","width":1280,"height":720,
                "refresh":{"numerator":60,"denominator":1},"x":1920,"y":0,
                "rotation":"identity","hdr_enabled":false},
            {"id":"inactive-target","width":2560,"height":1440,
                "refresh":{"numerator":60000,"denominator":1000},"x":3200,"y":0,
                "rotation":"identity","hdr_enabled":false}
        ]},
        "mode_policy":{"display_id":"inactive-target",
            "configuration_option":"ensure_active","remapping":{},
            "context":{"resolution_is_automatic":false,"refresh_rate_is_automatic":false,
                "client_display_mode_override":true,"optimize_game_settings":false}}
    })
}

fn inactive_broker(host: &mut Harness) {
    host.broker.display_inactive_target = Some("inactive-target".into());
    host.broker.display_active_ids = vec!["active-one".into(), "active-two".into()];
}

#[test]
fn inactive_ensure_active_preserves_full_request_and_owned_lease_lifecycle() {
    let mut host = Harness::new();
    inactive_broker(&mut host);
    let mut request = prepare();
    request["display_request"] = inactive_ensure_active_request();
    let prepared = host.response(1, request.clone());
    let id = prepared["session"].as_u64().unwrap();
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["display_state"], "applied");
    assert_eq!(prepared["display_applied"], true);
    assert!(host.pipeline.records[&id].display_lease.is_some());
    assert_eq!(prepared["display_request"], request["display_request"]);
    assert_eq!(host.response(2, start(id))["display_state"], "applied");
    let operation = host.pipeline.records[&id].display_operation();
    let apply = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == "amaterasu.display/1" && call.payload["action"] == "apply")
        .unwrap();
    assert_eq!(
        apply.payload["configuration"],
        request["display_request"]["configuration"]
    );
    assert_eq!(
        apply.payload["mode_policy"],
        request["display_request"]["mode_policy"]
    );
    assert_eq!(
        apply.payload["expected_topology_revision"],
        request["display_request"]["expected_topology_revision"]
    );
    assert_eq!(apply.payload["operation_id"], operation);
    assert!(apply.payload.get("mode_policies").is_none());
    let status = host.response(3, json!({"action":"status","session":id}));
    assert_eq!(status["display_state"], "applied");
    assert_eq!(status["display_applied"], true);
    assert_eq!(status["display_request"], request["display_request"]);
    assert_eq!(
        host.broker.display_operation.as_deref(),
        Some(operation.as_str())
    );
    assert_eq!(
        host.response(4, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert!(host.broker.display_lease.is_none());
    assert!(
        host.broker.calls.iter().any(
            |call| call.service == "amaterasu.display/1" && call.payload["action"] == "status"
        )
    );
    assert!(
        host.broker.calls.iter().any(
            |call| call.service == "amaterasu.display/1" && call.payload["action"] == "restore"
        )
    );
}

#[test]
fn inactive_ensure_active_lost_result_reconciles_same_operation_and_restores() {
    let mut host = Harness::new();
    inactive_broker(&mut host);
    let mut request = prepare();
    request["display_request"] = inactive_ensure_active_request();
    host.broker.fault = Some(("amaterasu.display/1:apply".into(), Fault::AfterMutation));
    let prepared = host.response(1, request);
    let id = prepared["session"].as_u64().unwrap();
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["display_state"], "applied");
    assert_eq!(
        host.broker.display_operation.as_deref(),
        Some(host.pipeline.records[&id].display_operation().as_str())
    );
    assert_eq!(
        host.response(2, json!({"action":"status","session":id}))["display_state"],
        "applied"
    );
    assert_eq!(host.response(3, start(id))["state"], "active");
    assert_eq!(
        host.response(4, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert!(host.broker.display_lease.is_none());
    assert!(
        host.broker.calls.iter().any(
            |call| call.service == "amaterasu.display/1" && call.payload["action"] == "restore"
        )
    );
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
}

#[test]
fn inactive_target_other_options_and_wrong_operation_do_not_restore() {
    for option in ["ensure_primary", "ensure_only_display"] {
        let mut host = Harness::new();
        inactive_broker(&mut host);
        let mut request = prepare();
        request["display_request"] = inactive_ensure_active_request();
        request["display_request"]["mode_policy"]["configuration_option"] = json!(option);
        let rejected = host.response(1, request);
        assert_eq!(rejected["state"], "failed");
        assert_eq!(rejected["error"], "display_apply_rejected");
        assert!(host.broker.display_lease.is_none());
        assert!(
            !host
                .broker
                .calls
                .iter()
                .any(|call| call.service == "amaterasu.display/1"
                    && matches!(call.payload["action"].as_str(), Some("restore" | "recover")))
        );
    }

    let mut host = Harness::new();
    inactive_broker(&mut host);
    let mut request = prepare();
    request["display_request"] = inactive_ensure_active_request();
    host.broker.display_race_on_lost_apply = true;
    host.broker.fault = Some(("amaterasu.display/1:apply".into(), Fault::BeforeMutation));
    let rejected = host.response(1, request);
    assert_eq!(rejected["state"], "cleanup_failed");
    assert_eq!(
        host.broker.display_operation.as_deref(),
        Some("another-session-display")
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
fn selected_display_configuration_options_reach_apply_and_cleanup() {
    let mut two_monitor = display_request();
    two_monitor["mode_policy"] = two_monitor["mode_policies"][0].clone();
    two_monitor.as_object_mut().unwrap().remove("mode_policies");
    two_monitor["mode_policy"]["configuration_option"] = json!("ensure_only_display");
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
    assert_eq!(applied.payload["mode_policy"], two_monitor["mode_policy"]);
    assert!(applied.payload.get("mode_policies").is_none());
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert!(host.broker.display_lease.is_none());

    for option in ["ensure_active", "ensure_primary"] {
        let mut sole_target = display_request();
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
fn singular_topology_policy_rejects_unknown_target_and_plural_topology_changes() {
    let mut host = Harness::new();
    let mut request = prepare();
    let mut display = display_request();
    display["mode_policy"] = display["mode_policies"][0].clone();
    display.as_object_mut().unwrap().remove("mode_policies");
    display["mode_policy"]["configuration_option"] = json!("ensure_only_display");
    display["mode_policy"]["display_id"] = json!("third");
    request["display_request"] = display;
    assert_eq!(host.response(1, request)["code"], "invalid_display_request");
    let mut request = prepare();
    request["display_request"] = display_request();
    request["display_request"]["mode_policies"][0]["configuration_option"] =
        json!("ensure_only_display");
    assert_eq!(host.response(2, request)["code"], "invalid_display_request");
    let mut request = prepare();
    request["display_request"] = display_request();
    request["display_request"]["mode_policies"][0]["configuration_option"] = Value::Null;
    assert_eq!(host.response(3, request)["code"], "invalid_display_request");
    let mut request = prepare();
    let mut display = display_request();
    display["mode_policy"] = display["mode_policies"][0].clone();
    display.as_object_mut().unwrap().remove("mode_policies");
    request["display_request"] = display;
    assert_eq!(host.response(4, request)["code"], "invalid_display_request");
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.display/1")
    );
}

#[test]
fn full_frozen_topology_applies_once_and_restores_on_stop() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    let prepared = host.response(1, request.clone());
    let id = prepared["session"].as_u64().unwrap();
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["display_state"], "applied");
    assert_eq!(prepared["display_applied"], true);
    assert_eq!(
        host.pipeline.records[&id].display_lease.as_deref(),
        Some("display-0000000000000001")
    );
    assert_eq!(prepared["display_request"], request["display_request"]);
    assert_eq!(host.response(2, request)["session"], id);
    let status = host.response(3, json!({"action":"status","session":id}));
    assert_eq!(status["state"], "prepared");
    assert_eq!(status["display_state"], "applied");
    assert_eq!(status["display_applied"], true);
    assert_eq!(status["display_request"], display_request());
    assert_eq!(host.response(4, start(id))["display_state"], "applied");
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
        host.response(5, json!({"action":"stop","session":id}))["state"],
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
fn rejected_apply_fails_prepare_without_lease() {
    let mut host = Harness::new();
    host.broker.display_apply_failure = true;
    let mut request = prepare();
    request["display_request"] = display_request();
    let rejected = host.response(1, request);
    assert_eq!(rejected["state"], "failed");
    assert_eq!(rejected["error"], "display_apply_rejected");
    assert!(host.broker.display_lease.is_none());
}

#[test]
fn unavailable_display_fails_prepare_before_apply() {
    let mut host = Harness::new();
    host.broker.display_unavailable = true;
    let mut request = prepare();
    request["display_request"] = display_request();
    let rejected = host.response(1, request);
    assert_eq!(rejected["state"], "failed");
    assert_eq!(rejected["error"], "display_unavailable");
    assert!(host.broker.display_lease.is_none());
    assert!(!host.broker.calls.iter().any(|call| {
        call.service == "amaterasu.display/1" && call.payload["action"] == "apply"
    }));
}

#[test]
fn prepared_display_lease_restores_on_expiry_without_start() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    let prepared = host.response(1, request);
    let id = prepared["session"].as_u64().unwrap();
    assert_eq!(prepared["display_state"], "applied");
    assert!(host.broker.display_lease.is_some());
    host.now = host.pipeline.records[&id].prepared_until;
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Failed);
    assert_eq!(host.pipeline.records[&id].error, Some("prepare_expired"));
    assert!(host.broker.display_lease.is_none());
    assert!(host.broker.calls.iter().any(|call| {
        call.service == "amaterasu.display/1" && call.payload["action"] == "restore"
    }));
}

#[test]
fn staged_display_prepare_window_resets_to_ping_timeout_when_armed() {
    let mut host = Harness::new();
    host.broker.authenticated = false;
    let mut request = prepare();
    request["display_request"] = display_request();
    request["display_prepare_timeout_ms"] = json!(60_000);
    let prepared = host.response(1, request);
    let id = prepared["session"].as_u64().unwrap();
    let staged_until = host.pipeline.records[&id].prepared_until;
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["display_applied"], true);
    assert_eq!(prepared["display_prepare_timeout_ms"], 60_000);
    assert!(staged_until.saturating_sub(host.now) > 10_000);

    host.now = staged_until - 15_000;
    let mut armed = start(id);
    armed["action"] = json!("start_when_authenticated");
    assert_eq!(host.response(2, armed)["state"], "armed");
    let auth_until = host.pipeline.records[&id].prepared_until;
    assert!(auth_until < staged_until);
    assert_eq!(auth_until - host.now, 10_000);
    assert!(host.broker.display_lease.is_some());

    host.now = auth_until;
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Failed);
    assert_eq!(host.pipeline.records[&id].error, Some("prepare_expired"));
    assert!(host.broker.display_lease.is_none());
}

#[test]
fn staged_display_allows_direct_start_after_normal_ping_window() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    request["display_prepare_timeout_ms"] = json!(60_000);
    let prepared = host.response(1, request);
    let id = prepared["session"].as_u64().unwrap();
    host.now += 15_000;
    assert_eq!(host.response(2, start(id))["state"], "active");
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert!(host.broker.display_lease.is_none());
}

#[test]
fn display_prepare_timeout_requires_display_and_bounded_duration() {
    let mut missing_display = prepare();
    missing_display["display_prepare_timeout_ms"] = json!(60_000);
    assert_eq!(
        Harness::new().response(1, missing_display)["code"],
        "invalid_display_prepare_timeout"
    );
    for invalid in [json!(0), json!(120_001), json!(-1), json!("60000")] {
        let mut request = prepare();
        request["display_request"] = display_request();
        request["display_prepare_timeout_ms"] = invalid;
        let mut host = Harness::new();
        let code = host.response(1, request)["code"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            code == "invalid_display_prepare_timeout" || code == "invalid_request",
            "unexpected rejection: {code}"
        );
        assert!(!host.broker.calls.iter().any(|call| {
            call.service == "amaterasu.display/1" && call.payload["action"] == "apply"
        }));
    }
}

#[test]
fn lost_apply_result_adopts_only_its_operation_and_restores() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    host.broker.fault = Some(("amaterasu.display/1:apply".into(), Fault::AfterMutation));
    let response = host.response(1, request);
    let id = response["session"].as_u64().unwrap();
    assert_eq!(response["state"], "prepared");
    assert_eq!(
        host.pipeline.records[&id].display_lease.as_deref(),
        Some("display-0000000000000001")
    );
    assert_eq!(response["display_state"], "applied");
    assert_eq!(response["display_applied"], true);
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
fn recovered_provider_journal_blocks_prepare_and_recovers_owned_lease() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    host.broker.display_restart_on_lost_apply = true;
    host.broker.fault = Some(("amaterasu.display/1:apply".into(), Fault::AfterMutation));
    let rejected = host.response(1, request);
    assert_eq!(rejected["state"], "failed");
    assert_eq!(rejected["error"], "display_status_unverified");
    assert!(host.broker.display_lease.is_none());
    assert!(
        host.broker.calls.iter().any(
            |call| call.service == "amaterasu.display/1" && call.payload["action"] == "recover"
        )
    );
}

#[test]
fn same_operation_recovery_reply_needs_leased_status_before_prepare() {
    let mut host = Harness::new();
    host.broker.display_apply_recovery_reply = true;
    host.broker.display_recovery_required = false;
    let mut request = prepare();
    request["display_request"] = display_request();
    let prepared = host.response(1, request);
    let id = prepared["session"].as_u64().unwrap();
    assert_eq!(prepared["display_state"], "applied");
    assert_eq!(host.response(2, start(id))["state"], "active");
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
    );
    assert!(
        host.broker.calls.iter().any(
            |call| call.service == "amaterasu.display/1" && call.payload["action"] == "restore"
        )
    );
}

#[test]
fn concurrent_owner_lease_after_preflight_is_never_restored_by_this_session() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    host.broker.display_race_on_lost_apply = true;
    host.broker.fault = Some(("amaterasu.display/1:apply".into(), Fault::BeforeMutation));
    let rejected = host.response(1, request);
    assert_eq!(rejected["state"], "cleanup_failed");
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
    let rejected = host.response(1, request);
    assert_eq!(rejected["state"], "cleanup_failed");
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
    let second_result = host.response(3, second);
    assert_eq!(second_result["state"], "failed");
    assert_eq!(second_result["error"], "display_unavailable");
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

fn pending_display_apply(host: &mut Harness, id: u64) -> rpc::Pending {
    let operation = host.pipeline.records[&id].display_operation();
    let payload = json!({"action":"apply", "configuration":host.broker.display_topology,
        "expected_topology_revision":"0123456789abcdef0123456789abcdef",
        "operation_id":operation});
    let mut pending = rpc::Pending::new(
        "amaterasu.display/1",
        payload,
        format!("retirement-display-{id}"),
        None,
    );
    assert!(matches!(
        pending.poll(&mut |command| host.broker.send(command), host.now),
        rpc::Progress::Waiting
    ));
    host.pipeline
        .records
        .get_mut(&id)
        .unwrap()
        .display_attempted = true;
    pending
}

#[test]
fn retirement_releases_read_only_display_uncertainty() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let mut pending = rpc::Pending::new(
        "amaterasu.display/1",
        json!({"action":"status"}),
        "retirement-status".into(),
        None,
    );
    assert!(matches!(
        pending.poll(&mut |command| host.broker.send(command), host.now),
        rpc::Progress::Waiting
    ));
    let record = host.pipeline.records.get_mut(&id).unwrap();
    record.uncertain = Some(pending);
    record.uncertain_step = Some(Step::CheckDisplay);
    assert!(
        host.pipeline
            .shutdown_with(&mut |command| host.broker.send(command))
    );
    assert!(host.pipeline.records[&id].uncertain.is_none());
    assert!(host.pipeline.records[&id].uncertain_step.is_none());
}

#[test]
fn retirement_reconciles_completed_display_apply_before_clearing_uncertainty() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let pending = pending_display_apply(&mut host, id);
    let record = host.pipeline.records.get_mut(&id).unwrap();
    record.uncertain = Some(pending);
    record.uncertain_step = Some(Step::ApplyDisplay);
    // A completed apply still owns a display lease until restore confirms it.
    host.broker.display_restore_failures = 1;
    assert!(
        !host
            .pipeline
            .shutdown_with(&mut |command| host.broker.send(command))
    );
    assert!(host.pipeline.records[&id].uncertain.is_some());
    assert!(host.broker.display_lease.is_some());
    assert!(
        host.pipeline
            .shutdown_with(&mut |command| host.broker.send(command))
    );
    assert!(host.pipeline.records[&id].uncertain.is_none());
    assert!(host.broker.display_lease.is_none());
}

#[test]
fn retirement_observes_a_completed_apply_even_after_settlement_started() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let mut pending = pending_display_apply(&mut host, id);
    pending.cancel();
    let record = host.pipeline.records.get_mut(&id).unwrap();
    record.uncertain = Some(pending);
    record.uncertain_step = Some(Step::ApplyDisplay);
    assert!(
        host.pipeline
            .shutdown_with(&mut |command| host.broker.send(command))
    );
    assert!(host.pipeline.records[&id].uncertain.is_none());
    assert!(host.broker.display_lease.is_none());
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
}

#[test]
fn retirement_keeps_unknown_display_apply_fenced_after_rpc_cancellation() {
    for fault in [Fault::BeforeMutation, Fault::AfterMutation] {
        let mut host = Harness::new();
        let id = host.response(1, prepare())["session"].as_u64().unwrap();
        let pending = pending_display_apply(&mut host, id);
        let record = host.pipeline.records.get_mut(&id).unwrap();
        record.uncertain = Some(pending);
        record.uncertain_step = Some(Step::ApplyDisplay);
        host.broker.fault = Some(("amaterasu.display/1:apply".into(), fault));
        assert!(
            !host
                .pipeline
                .shutdown_with(&mut |command| host.broker.send(command))
        );
        assert!(
            !host
                .pipeline
                .shutdown_with(&mut |command| host.broker.send(command))
        );
        assert!(host.pipeline.records[&id].uncertain.is_some());
        assert_eq!(
            host.broker.display_lease.is_some(),
            matches!(fault, Fault::AfterMutation)
        );
        assert_eq!(
            host.broker
                .calls
                .iter()
                .filter(|call| call.service == "amaterasu.display/1"
                    && call.payload["action"] == "apply")
                .count(),
            1
        );
        assert!(!host.broker.calls.iter().any(
            |call| call.service == "amaterasu.display/1" && call.payload["action"] == "status"
        ));
    }
}
