use super::*;

fn display_request() -> Value {
    json!({"configuration":{"displays":[
        {"id":"first","width":1920,"height":1080,"refresh":{"numerator":60,"denominator":1},"x":0,"y":0},
        {"id":"second","width":1280,"height":720,"refresh":{"numerator":60,"denominator":1},"x":1920,"y":0}
    ]},"mode_policies":[
        {"display_id":"first","remapping":{},"context":{"resolution_is_automatic":true,"refresh_rate_is_automatic":true,"client_display_mode_override":false,"optimize_game_settings":false}},
        {"display_id":"second","remapping":{},"context":{"resolution_is_automatic":true,"refresh_rate_is_automatic":true,"client_display_mode_override":false,"optimize_game_settings":false}}
    ]})
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
fn lost_apply_result_uses_status_and_does_not_apply_twice() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["display_request"] = display_request();
    let id = host.response(1, request)["session"].as_u64().unwrap();
    host.broker.fault = Some(("amaterasu.display/1:apply".into(), Fault::AfterMutation));
    let response = host.response(2, start(id));
    assert_eq!(response["state"], "active");
    assert!(
        host.pipeline.records[&id].display_lease.is_some(),
        "response={response}; calls={:?}",
        host.broker
            .calls
            .iter()
            .filter(|call| call.service == "amaterasu.display/1")
            .map(|call| call.payload.clone())
            .collect::<Vec<_>>()
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
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "stopped"
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
