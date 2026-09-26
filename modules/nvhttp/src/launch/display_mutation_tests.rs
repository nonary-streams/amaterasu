fn mutating_display_fixture(option: &str, two_displays: bool) -> (Controller, Broker) {
    let mut streaming = config();
    streaming.profile.max_fps = 120;
    streaming.capture.capture_provider = CaptureProvider::Wgc;
    streaming.capture.output = "\\\\.\\DISPLAY2".into();
    let mut controller = Controller::new(Some(streaming)).unwrap();
    controller.set_global_display_policy(
        9,
        json!({
            "dd_configuration_option":option,
            "dd_refresh_rate_option":"auto",
            "dd_hdr_option":"disabled",
        }),
    );
    let mut displays = Vec::new();
    let mut entries = Vec::new();
    if two_displays {
        displays.push(json!({
            "id":"monitor.one","width":1600,"height":900,
            "refresh":{"numerator":60,"denominator":1},
            "x":0,"y":0,"rotation":"identity","hdr_enabled":false,
        }));
        entries.push(json!({"id":"monitor.one","source":"\\\\.\\DISPLAY1","active":true}));
    }
    displays.push(json!({
        "id":"Monitor.Two","width":1920,"height":1080,
        "refresh":{"numerator":60,"denominator":1},
        "x":if two_displays { 1600 } else { 0 },"y":0,
        "rotation":"identity","hdr_enabled":false,
    }));
    entries.push(json!({"id":"monitor.two","source":"\\\\.\\DISPLAY2","active":true}));
    let broker = Broker {
        display_snapshot_ids: displays
            .iter()
            .map(|display| display["id"].as_str().unwrap().to_owned())
            .collect(),
        display_snapshot_configuration: Some(json!({"displays":displays})),
        display_entries: entries,
        ..Broker::default()
    };
    (controller, broker)
}

fn inactive_display_fixture(option: &str) -> (Controller, Broker) {
    let (controller, mut broker) = mutating_display_fixture(option, true);
    broker.display_snapshot_ids.pop();
    broker.display_snapshot_configuration.as_mut().unwrap()["displays"]
        .as_array_mut()
        .unwrap()
        .pop();
    broker.display_entries[1]["id"] = json!("Monitor.Two");
    broker.display_entries[1]["active"] = json!(false);
    (controller, broker)
}

#[test]
fn inactive_ensure_active_attests_appended_target_without_changing_active_display() {
    let now = Instant::now();
    let (mut controller, mut broker) = inactive_display_fixture("ensure_active");
    let paired = peer_with_display_mode(52, "1920x1080x59.94").unwrap();
    let mut stream = request(52);
    stream.fps = 120;
    assert!(controller.launch(10, stream.clone(), paired.clone(), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 200);
    let open = opened_rtsp_payload(&broker);
    let request = &open["display_request"];
    assert_eq!(open["profile"]["fps"], 120);
    assert_eq!(request["configuration"]["displays"].as_array().unwrap().len(), 2);
    assert_eq!(request["configuration"]["displays"][0]["id"], "monitor.one");
    assert_eq!(request["configuration"]["displays"][0]["x"], 0);
    assert_eq!(request["configuration"]["displays"][0]["width"], 1600);
    assert_eq!(request["configuration"]["displays"][1]["id"], "Monitor.Two");
    assert_eq!(request["configuration"]["displays"][1]["x"], 1600);
    assert_eq!(request["configuration"]["displays"][1]["refresh"], json!({"numerator":59940,"denominator":1000}));
    assert_eq!(request["configuration"]["displays"][1]["hdr_enabled"], Value::Null);
    assert_eq!(request["mode_policy"]["display_id"], "Monitor.Two");
    assert_eq!(request["mode_policy"]["configuration_option"], "ensure_active");
    assert_eq!(open["display_admission_token"], "ab".repeat(32));
    controller.reply_delivered(10, now);
    assert_eq!(controller.launch(11, stream, paired, now).unwrap().body, reply.body);
    assert_eq!(broker.actions().iter().filter(|(service, action)|
        *service == "amaterasu.rtsp/1" && *action == "open").count(), 1);
}

#[test]
fn inactive_stream_mode_fallback_requires_static_refresh_policy() {
    let now = Instant::now();
    let (mut controller, mut broker) = inactive_display_fixture("ensure_active");
    controller.set_global_display_policy(9, json!({
        "dd_configuration_option":"ensure_active",
        "dd_refresh_rate_option":"disabled",
        "dd_hdr_option":"disabled",
    }));
    assert!(controller.launch(10, request(54), peer(54), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 200);
    let display_request = &opened_rtsp_payload(&broker)["display_request"];
    assert_eq!(display_request["configuration"]["displays"][1]["width"], 1280);
    assert_eq!(display_request["configuration"]["displays"][1]["height"], 720);
    assert_eq!(display_request["configuration"]["displays"][1]["refresh"], json!({"numerator":60000,"denominator":1000}));
    assert_eq!(display_request["mode_policy"]["context"]["client_display_mode_override"], false);
}

#[test]
fn inactive_route_fails_closed_on_drift_ambiguity_and_other_options() {
    for case in ["ensure_primary", "ensure_only_display", "verify_only", "revision", "bytes", "ambiguous", "duplicate_id", "missing", "auto_without_mode", "position_out_of_range"] {
        let now = Instant::now();
        let option = if matches!(case, "revision" | "bytes" | "ambiguous" | "duplicate_id" | "missing" | "auto_without_mode" | "position_out_of_range") {
            "ensure_active"
        } else {
            case
        };
        let (mut controller, mut broker) = inactive_display_fixture(option);
        match case {
            "revision" => broker.display_revision_drift = true,
            "bytes" => broker.display_configuration_drift = true,
            "ambiguous" => broker.display_entries.push(json!({"id":"monitor.other","source":"\\\\.\\DISPLAY2","active":false})),
            "duplicate_id" => broker.display_entries.push(json!({"id":"monitor.two","source":"\\\\.\\DISPLAY3","active":false})),
            "missing" => { broker.display_entries.pop(); },
            "position_out_of_range" => broker.display_snapshot_configuration.as_mut().unwrap()["displays"][0]["x"] = json!(65_536),
            _ => {},
        }
        assert!(controller.launch(10, request(53), peer(53), now).is_none());
        let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
        assert_ne!(reply.status, 200, "{case}");
        assert!(!broker.actions().iter().any(|(service, action)|
            *service == "amaterasu.launch/1" && *action == "launch_export"), "{case}");
        assert!(!broker.actions().iter().any(|(service, action)|
            *service == "amaterasu.rtsp/1" && *action == "open"), "{case}");
    }
}

#[test]
fn selected_two_monitor_mutation_attests_full_topology_and_physical_5994_mode() {
    let now = Instant::now();
    let (mut controller, mut broker) = mutating_display_fixture("ensure_only_display", true);
    broker.application_input_overrides =
        Some(json!({"dd_refresh_rate_option":"manual","dd_manual_refresh_rate":"60"}));
    let certificate = vec![41; 32];
    let paired = PeerIdentity::new("127.0.0.1".parse().unwrap(), certificate.clone())
        .unwrap()
        .with_paired_identity(
            "client-41".into(),
            fingerprint(&certificate),
            ClientConfigOverrides {
                display_mode: Some("1920x1080x59.94".into()),
                dd_refresh_rate_option: Some("disabled".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let mut stream = request(41);
    stream.fps = 120;
    assert!(
        controller
            .launch(10, stream.clone(), paired.clone(), now)
            .is_none()
    );
    controller.set_global_display_policy(10, json!({"dd_configuration_option":"disabled"}));
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 200);
    let open = opened_rtsp_payload(&broker);
    let display_request = &open["display_request"];
    assert_eq!(open["profile"]["fps"], 120);
    assert_eq!(
        display_request["expected_topology_revision"],
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert_eq!(
        display_request["configuration"]["displays"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        display_request["configuration"]["displays"][0]["width"],
        1600
    );
    assert_eq!(display_request["configuration"]["displays"][0]["x"], 0);
    assert_eq!(
        display_request["configuration"]["displays"][1]["id"],
        "Monitor.Two"
    );
    assert_eq!(
        display_request["configuration"]["displays"][1]["refresh"],
        json!({"numerator":59940,"denominator":1000})
    );
    assert_eq!(display_request["mode_policy"]["display_id"], "Monitor.Two");
    assert_eq!(
        display_request["mode_policy"]["configuration_option"],
        "ensure_only_display"
    );
    assert_eq!(
        display_request["mode_policy"]["context"]["client_display_mode_override"],
        true
    );
    assert!(display_request.get("mode_policies").is_none());
    assert_eq!(open["display_policy_snapshot"]["global_revision"], 9);
    assert_eq!(
        open["display_policy_snapshot"]["effective_overrides"]["dd_refresh_rate_option"],
        "disabled"
    );
    assert_eq!(open["display_admission_token"], "ab".repeat(32));
    let admission = broker
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.launch/1"
                && payload["action"] == "prepare_display_admission" =>
            {
                Some(payload)
            }
            _ => None,
        })
        .unwrap();
    let hash = Sha256::digest(serde_json::to_vec(display_request).unwrap());
    let hash: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(admission["request_sha256"], hash);
    assert_eq!(admission["global_revision"], 9);
    assert_eq!(
        admission["application_display_overrides"]["dd_manual_refresh_rate"],
        "60"
    );
    assert_eq!(
        admission["client_display_overrides"]["dd_refresh_rate_option"],
        "disabled"
    );
    assert_eq!(broker.rtsp_display_request.as_ref(), Some(display_request));
    let actions = broker.actions();
    let position = |service: &str, action: &str| {
        actions
            .iter()
            .position(|(seen_service, seen_action)| *seen_service == service && *seen_action == action)
            .expect("required launch phase")
    };
    assert!(position("amaterasu.launch/1", "preflight_export")
        < position("amaterasu.launch/1", "prepare_display_admission"));
    assert!(position("amaterasu.launch/1", "prepare_display_admission")
        < position("amaterasu.rtsp/1", "open"));
    assert!(position("amaterasu.rtsp/1", "open")
        < position("amaterasu.launch/1", "launch_export"));
    let export = broker.commands.iter().find_map(|command| match command {
        Command::Request { service, payload, .. }
            if service == "amaterasu.launch/1" && payload["action"] == "launch_export" =>
        {
            Some(payload)
        }
        _ => None,
    }).unwrap();
    assert_eq!(export["display_admission_token"], open["display_admission_token"]);
    controller.reply_delivered(10, now);
    let retry = controller
        .launch(11, stream, paired, now)
        .expect("same owner retry");
    assert_eq!(retry.body, reply.body);
    assert_eq!(
        broker
            .actions()
            .into_iter()
            .filter(|(service, action)| *service == "amaterasu.rtsp/1" && *action == "open")
            .count(),
        1
    );
}

#[test]
fn active_target_allows_active_and_primary_options_in_full_topology() {
    for (option, two_displays) in [
        ("ensure_active", false),
        ("ensure_primary", false),
        ("ensure_active", true),
        ("ensure_primary", true),
    ] {
        let now = Instant::now();
        let (mut controller, mut broker) = mutating_display_fixture(option, two_displays);
        assert!(controller.launch(10, request(42), peer(42), now).is_none());
        let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
        assert_eq!(reply.status, 200, "{option}");
        let request = &opened_rtsp_payload(&broker)["display_request"];
        assert_eq!(
            request["configuration"]["displays"]
                .as_array()
                .unwrap()
                .len(),
            if two_displays { 2 } else { 1 }
        );
        assert_eq!(request["mode_policy"]["configuration_option"], option);
        assert_eq!(
            request["mode_policy"]["context"]["client_display_mode_override"],
            false
        );
    }
}

#[test]
fn unsupported_or_changed_display_facts_fail_before_application_success() {
    for case in [
        "inactive_target",
        "rotation_null",
        "topology_bytes",
        "hdr_auto",
        "optimize_unknown",
        "catalog_revision",
        "admission_reject",
        "admission_hash",
    ] {
        let now = Instant::now();
        let (mut controller, mut broker) =
            mutating_display_fixture("ensure_only_display", true);
        match case {
            "inactive_target" => broker.display_entries[1]["active"] = json!(false),
            "rotation_null" => broker.display_snapshot_configuration.as_mut().unwrap()["displays"][1]["rotation"] = Value::Null,
            "topology_bytes" => broker.display_configuration_drift = true,
            "hdr_auto" => controller.set_global_display_policy(9, json!({"dd_configuration_option":"ensure_only_display","dd_hdr_option":"auto"})),
            "optimize_unknown" => controller.set_global_display_policy(9, json!({"dd_configuration_option":"ensure_only_display","dd_resolution_option":"auto"})),
            "catalog_revision" => broker.application_catalog_revision = 2,
            "admission_reject" => broker.reject_display_admission = true,
            "admission_hash" => broker.forge_display_admission_hash = true,
            _ => {}
        }
        assert!(controller.launch(10, request(43), peer(43), now).is_none());
        let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
        assert_ne!(reply.status, 200, "{case}");
        if case != "catalog_revision" {
            assert!(
                !broker
                    .actions()
                    .iter()
                    .any(|(service, action)| *service == "amaterasu.rtsp/1" && *action == "open"),
                "{case}"
            );
        } else {
            assert!(broker.rtsp_closed, "staged display must be restored after launch drift");
        }
        if matches!(
            case,
            "inactive_target" | "rotation_null" | "topology_bytes" | "hdr_auto" | "optimize_unknown"
        ) {
            assert!(
                !broker
                    .actions()
                    .iter()
                    .any(|(service, action)| *service == "amaterasu.launch/1"
                        && *action == "launch_export"),
                "{case}"
            );
        }
    }
}

#[test]
fn failed_display_stage_never_starts_application_and_failed_export_restores_rtsp() {
    for case in ["apply_failed", "readback_missing", "export_rejected"] {
        let now = Instant::now();
        let (mut controller, mut broker) =
            mutating_display_fixture("ensure_only_display", true);
        broker.reject_rtsp_display_apply = case == "apply_failed";
        broker.missing_rtsp_display_readback = case == "readback_missing";
        broker.reject_launch_export = case == "export_rejected";
        assert!(controller.launch(10, request(44), peer(44), now).is_none());
        let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
        assert_ne!(reply.status, 200, "{case}");
        let actions = broker.actions();
        if case != "export_rejected" {
            assert!(
                !actions.iter().any(|(service, action)|
                    *service == "amaterasu.launch/1" && *action == "launch_export"),
                "{case}"
            );
        }
        if case != "apply_failed" {
            assert!(broker.rtsp_closed, "{case}: staged RTSP must be closed");
        }
        assert!(!actions.iter().any(|(service, action)|
            *service == "amaterasu.display/1" && *action == "apply"));
    }
}
