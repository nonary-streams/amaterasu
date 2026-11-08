use super::*;
use amaterasu_contracts::virtual_display::{VirtualDisplayLayout, VirtualDisplayMode};

fn intent(reference_id: &str, mode: VirtualDisplayMode, layout: VirtualDisplayLayout) -> Value {
    json!({
        "identity":"00000000-0000-0000-0000-000000000001",
        "reference_id":reference_id,
        "name":"Amaterasu",
        "width":1920,
        "height":1080,
        "refresh_millihz":60000,
        "dd_virtual_display_scale":0,
        "dd_activate_virtual_display":true,
        "dd_use_sunshine_virtual_display_driver":true,
        "virtual_display_mode":mode,
        "virtual_display_layout":layout,
    })
}

fn display_policy(configuration_option: &str) -> Value {
    json!({
        "configuration_option":configuration_option,
        "remapping":{
            "mixed":[{"requested_fps":"60","final_resolution":"1920x1080","final_refresh_rate":"59.94"}],
            "resolution_only":[],
            "refresh_rate_only":[]
        },
        "context":{
            "resolution_is_automatic":true,
            "refresh_rate_is_automatic":true,
            "client_display_mode_override":false,
            "optimize_game_settings":true
        },
        "resolution_option":"manual",
        "manual_resolution":"1920x1080",
        "refresh_rate_option":"manual",
        "manual_refresh_rate":"59.94",
        "hdr_option":"auto",
        "effective_client_hdr_request":true
    })
}

fn prepare_with_intent(operation: &str, intent: Value) -> Value {
    let mut request = prepare();
    request["operation_id"] = json!(operation);
    request["virtual_display_intent"] = intent;
    request
}

fn actions(host: &Harness, service: &str) -> Vec<String> {
    host.broker
        .calls
        .iter()
        .filter(|call| call.service == service)
        .map(|call| call.payload["action"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn stop_waits_for_final_virtual_display_removal() {
    let mut host = Harness::new();
    let prepared = host.response(
        1,
        prepare_with_intent(
            "delayed-removal",
            intent(
                "session-1",
                VirtualDisplayMode::PerClient,
                VirtualDisplayLayout::Extended,
            ),
        ),
    );
    let id = prepared["session"].as_u64().unwrap();
    host.broker.virtual_display_removal_statuses = 2;
    let stopped = host.response(2, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "stopped");
    assert!(host.broker.virtual_display_specification.is_none());
    let calls = actions(&host, "amaterasu.virtual-display/1");
    let remove = calls.iter().position(|action| action == "remove").unwrap();
    assert_eq!(&calls[remove + 1..], ["status", "status", "status"]);
}

#[test]
fn parked_display_releases_failed_stream_and_is_reused_by_next_stream() {
    let mut host = Harness::new();
    let mut first_intent = intent(
        "first-reference",
        VirtualDisplayMode::PerClient,
        VirtualDisplayLayout::Extended,
    );
    first_intent["display_policy"] = display_policy("disabled");
    let first = host.response(1, prepare_with_intent("first-stream", first_intent));
    assert_eq!(first["state"], "prepared", "{first}");
    let first_id = first["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(first_id))["state"], "active");
    host.broker.virtual_display_park_on_remove = true;
    let stopped = host.response(3, json!({"action":"stop","session":first_id}));
    assert_eq!(stopped["state"], "stopped", "{stopped}");
    assert!(host.broker.virtual_display_parked.is_some());
    assert!(host.broker.virtual_display_references.is_empty());

    let mut second_intent = intent(
        "second-reference",
        VirtualDisplayMode::PerClient,
        VirtualDisplayLayout::Extended,
    );
    second_intent["display_policy"] = display_policy("disabled");
    let second = host.response(4, prepare_with_intent("second-stream", second_intent));
    assert_eq!(second["state"], "prepared", "{second}");
    assert!(host.broker.virtual_display_reused_parked);
    assert_eq!(
        host.response(5, start(second["session"].as_u64().unwrap()))["state"],
        "active"
    );
}

#[test]
fn parked_display_requires_the_exact_provider_reservation() {
    let mut host = Harness::new();
    let mut requested = intent(
        "first-reference",
        VirtualDisplayMode::PerClient,
        VirtualDisplayLayout::Extended,
    );
    requested["display_policy"] = display_policy("disabled");
    let first = host.response(1, prepare_with_intent("first-stream", requested));
    let id = first["session"].as_u64().unwrap();
    host.broker.virtual_display_park_on_remove = true;
    host.broker.virtual_display_invalid_parked_status = true;
    let stopped = host.response(2, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "cleanup_failed", "{stopped}");
    assert!(host.broker.virtual_display_parked.is_some());
}

#[test]
fn direct_parked_removal_requires_the_matching_identity() {
    let mut host = Harness::new();
    let prepared = host.response(
        1,
        prepare_with_intent(
            "parked-removal",
            intent(
                "first-reference",
                VirtualDisplayMode::PerClient,
                VirtualDisplayLayout::Extended,
            ),
        ),
    );
    let record = host
        .pipeline
        .records
        .get_mut(&prepared["session"].as_u64().unwrap())
        .unwrap();
    let parked =
        json!({"ok":true,"parked":true,"removed":false,"reference_owned":false,"identity":"wrong"});
    assert!(!super::super::virtual_display::accept_remove(
        record, &parked
    ));
    assert!(record.virtual_display_reference_owned);
    let mut exact = parked;
    exact["identity"] = json!("00000000-0000-0000-0000-000000000001");
    assert!(super::super::virtual_display::accept_remove(record, &exact));
    assert_eq!(record.virtual_display_state, "parked");
}

#[test]
fn ready_monitor_applies_snapshot_and_restores_before_releasing_reference() {
    let mut host = Harness::new();
    let original_topology = host.broker.display_topology.clone();
    let request = prepare_with_intent(
        "vdisplay-single",
        intent(
            "session-1",
            VirtualDisplayMode::PerClient,
            VirtualDisplayLayout::Extended,
        ),
    );
    let prepared = host.response(1, request);
    let id = prepared["session"].as_u64().unwrap();
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["virtual_display_ready"], true);
    assert_eq!(prepared["display_applied"], true);
    let status = host.response(2, json!({"action":"status","session":id}));
    let status_text = serde_json::to_string(&status).unwrap();
    assert!(!status_text.contains("session-1"));
    assert!(!status_text.contains("00000000-0000-0000-0000-000000000001"));

    assert_eq!(
        actions(&host, "amaterasu.virtual-display/1"),
        ["create", "status"]
    );
    assert_eq!(
        actions(&host, "amaterasu.display/1"),
        ["snapshot", "status", "apply", "status"]
    );
    let apply = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == "amaterasu.display/1" && call.payload["action"] == "apply")
        .unwrap();
    let displays = apply.payload["configuration"]["displays"]
        .as_array()
        .unwrap();
    assert_eq!(displays.len(), 3);
    assert_eq!(displays[0]["id"], "desktop-primary");
    assert_eq!(displays[1]["id"], "desktop-secondary");
    assert_eq!(displays[2]["id"], "\\\\.\\DISPLAYVDD");
    assert_eq!(displays[2]["x"], 3200);
    assert_eq!(
        apply.payload["expected_topology_revision"],
        "0123456789abcdef0123456789abcdef"
    );

    let stopped = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(host.broker.display_topology, original_topology);
    assert!(host.broker.virtual_display_references.is_empty());
    assert!(host.broker.virtual_display_specification.is_none());
    let calls = &host.broker.calls;
    let restore = calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.display/1" && call.payload["action"] == "restore"
        })
        .unwrap();
    let remove = calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.virtual-display/1" && call.payload["action"] == "remove"
        })
        .unwrap();
    assert!(
        restore < remove,
        "topology restores before monitor reference release"
    );
}

#[test]
fn all_virtual_display_layouts_preserve_their_expected_topology_shape() {
    let layouts = [
        (
            VirtualDisplayLayout::Exclusive,
            "ensure_only_display",
            vec![("\\\\.\\DISPLAYVDD", 0, 0)],
        ),
        (
            VirtualDisplayLayout::Extended,
            "ensure_active",
            vec![
                ("desktop-primary", 0, 0),
                ("desktop-secondary", 1920, 0),
                ("\\\\.\\DISPLAYVDD", 3200, 0),
            ],
        ),
        (
            VirtualDisplayLayout::ExtendedPrimary,
            "ensure_primary",
            vec![
                ("desktop-primary", 1920, 0),
                ("desktop-secondary", 3840, 0),
                ("\\\\.\\DISPLAYVDD", 0, 0),
            ],
        ),
        (
            VirtualDisplayLayout::ExtendedIsolated,
            "ensure_active",
            vec![
                ("desktop-primary", 0, 0),
                ("desktop-secondary", 1920, 0),
                ("\\\\.\\DISPLAYVDD", 19584, 0),
            ],
        ),
        (
            VirtualDisplayLayout::ExtendedPrimaryIsolated,
            "ensure_primary",
            vec![
                ("desktop-primary", 32768, 0),
                ("desktop-secondary", 34688, 0),
                ("\\\\.\\DISPLAYVDD", 0, 0),
            ],
        ),
    ];

    for (index, (layout, option, expected)) in layouts.into_iter().enumerate() {
        let mut host = Harness::new();
        let original = host.broker.display_topology.clone();
        let response = host.response(
            1,
            prepare_with_intent(
                &format!("layout-{index}"),
                intent(
                    &format!("reference-{index}"),
                    VirtualDisplayMode::PerClient,
                    layout,
                ),
            ),
        );
        assert_eq!(response["state"], "prepared", "{response}");
        let apply = host
            .broker
            .calls
            .iter()
            .find(|call| call.service == "amaterasu.display/1" && call.payload["action"] == "apply")
            .unwrap();
        assert_eq!(apply.payload["mode_policy"]["configuration_option"], option);
        let displays = apply.payload["configuration"]["displays"]
            .as_array()
            .unwrap();
        assert_eq!(displays.len(), expected.len());
        for ((id, x, y), display) in expected.iter().zip(displays) {
            assert_eq!(display["id"], *id, "layout={layout:?}");
            assert_eq!(display["x"], *x, "layout={layout:?}");
            assert_eq!(display["y"], *y, "layout={layout:?}");
        }
        let stopped = host.response(2, json!({"action":"stop","session":response["session"]}));
        assert_eq!(stopped["state"], "stopped");
        assert_eq!(host.broker.display_topology, original);
    }
}

#[test]
fn resolved_display_mode_policy_reaches_the_existing_topology_transaction() {
    let mut host = Harness::new();
    let mut resolved_intent = intent(
        "session-policy",
        VirtualDisplayMode::PerClient,
        VirtualDisplayLayout::Extended,
    );
    resolved_intent["display_policy"] = display_policy("ensure_primary");
    let prepared = host.response(1, prepare_with_intent("vdisplay-policy", resolved_intent));
    assert_eq!(prepared["state"], "prepared");
    let apply = host
        .broker
        .calls
        .iter()
        .find(|call| call.service == "amaterasu.display/1" && call.payload["action"] == "apply")
        .unwrap();
    assert_eq!(
        apply.payload["mode_policy"]["configuration_option"],
        "ensure_primary"
    );
    assert_eq!(apply.payload["mode_policy"]["resolution_option"], "manual");
    assert_eq!(
        apply.payload["mode_policy"]["manual_resolution"],
        "1920x1080"
    );
    assert_eq!(
        apply.payload["mode_policy"]["refresh_rate_option"],
        "manual"
    );
    assert_eq!(apply.payload["mode_policy"]["manual_refresh_rate"], "59.94");
    assert_eq!(apply.payload["mode_policy"]["hdr_option"], "auto");
    assert_eq!(
        apply.payload["mode_policy"]["effective_client_hdr_request"],
        true
    );
    assert_eq!(
        apply.payload["mode_policy"]["remapping"]["mixed"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        apply.payload["mode_policy"]["context"]["optimize_game_settings"],
        true
    );
}

#[test]
fn virtual_display_source_range_is_independent_of_client_hdr() {
    for (source, client, expected) in [
        (Some(false), true, false),
        (Some(true), false, true),
        (None, true, true),
        (None, false, false),
    ] {
        let mut host = Harness::new();
        let mut display = intent(
            "source-range",
            VirtualDisplayMode::PerClient,
            VirtualDisplayLayout::Extended,
        );
        let mut policy = display_policy("disabled");
        policy["effective_client_hdr_request"] = json!(client);
        if let Some(source) = source {
            policy["source_hdr_required"] = json!(source);
        }
        display["display_policy"] = policy;
        let prepared = host.response(1, prepare_with_intent("source-range", display));
        assert_eq!(prepared["state"], "prepared", "{prepared}");
        let create = host
            .broker
            .calls
            .iter()
            .find(|call| {
                call.service == "amaterasu.virtual-display/1" && call.payload["action"] == "create"
            })
            .expect("virtual display was created");
        assert_eq!(create.payload["hdr_enabled"], expected);
    }
}

#[test]
fn disabled_and_verify_only_policies_do_not_acquire_or_restore_a_topology_lease() {
    for option in ["disabled", "verify_only"] {
        let mut host = Harness::new();
        host.broker.display_mutation_enabled = false;
        if option == "verify_only" {
            host.broker.display_topology["displays"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "id":"\\\\.\\DISPLAYVDD",
                    "width":1920,
                    "height":1080,
                    "refresh":{"numerator":60,"denominator":1},
                    "x":3200,
                    "y":0,
                    "rotation":"identity",
                    "hdr_enabled":false
                }));
        }
        let original_topology = host.broker.display_topology.clone();
        let mut resolved_intent = intent(
            &format!("session-{option}"),
            VirtualDisplayMode::PerClient,
            VirtualDisplayLayout::Extended,
        );
        resolved_intent["display_policy"] = display_policy(option);
        let prepared = host.response(
            1,
            prepare_with_intent(&format!("vdisplay-{option}"), resolved_intent),
        );
        let session = prepared["session"].as_u64().unwrap();
        assert_eq!(prepared["state"], "prepared", "{prepared}");
        assert_eq!(prepared["display_applied"], true);
        assert!(host.broker.display_lease.is_none());
        assert_eq!(
            actions(&host, "amaterasu.display/1"),
            ["snapshot", "status", "apply"]
        );

        assert_eq!(host.response(2, start(session))["state"], "active");
        let stopped = host.response(3, json!({"action":"stop","session":session}));
        assert_eq!(stopped["state"], "stopped", "{stopped}");
        assert!(host.broker.display_lease.is_none());
        assert_eq!(host.broker.display_topology, original_topology);
        assert!(host.broker.virtual_display_references.is_empty());
        assert!(!actions(&host, "amaterasu.display/1").contains(&"restore".into()));
        assert_eq!(
            actions(&host, "amaterasu.virtual-display/1")
                .last()
                .unwrap(),
            "remove"
        );
    }
}

#[test]
fn explicit_mode_activates_with_auto_activation_disabled() {
    let mut requested = intent(
        "session-explicit-mode",
        VirtualDisplayMode::Shared,
        VirtualDisplayLayout::Exclusive,
    );
    requested["dd_activate_virtual_display"] = json!(false);
    let mut host = Harness::new();
    let prepared = host.response(1, prepare_with_intent("explicit-mode", requested));
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["virtual_display_ready"], true);
    assert_eq!(prepared["display_applied"], true);
    assert_eq!(
        actions(&host, "amaterasu.virtual-display/1"),
        ["create", "status"]
    );
    assert_eq!(
        actions(&host, "amaterasu.display/1"),
        ["snapshot", "status", "apply", "status"]
    );
}

#[test]
fn disabled_mode_without_auto_activation_never_mutates_display_state() {
    let mut disabled = intent(
        "session-disabled-mode",
        VirtualDisplayMode::Disabled,
        VirtualDisplayLayout::Extended,
    );
    disabled["dd_activate_virtual_display"] = json!(false);
    let mut host = Harness::new();
    let prepared = host.response(1, prepare_with_intent("disabled-mode", disabled));
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["virtual_display_ready"], false);
    assert!(actions(&host, "amaterasu.virtual-display/1").is_empty());
    assert!(actions(&host, "amaterasu.display/1").is_empty());
}

#[test]
fn automatic_activation_creates_display_with_disabled_mode() {
    let mut automatic = intent(
        "session-auto-activate",
        VirtualDisplayMode::Disabled,
        VirtualDisplayLayout::Exclusive,
    );
    automatic["request_virtual_display"] = json!(true);
    automatic["dd_activate_virtual_display"] = json!(false);
    let mut host = Harness::new();
    let prepared = host.response(1, prepare_with_intent("auto-activation", automatic));
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["virtual_display_ready"], true);
    assert_eq!(prepared["display_applied"], true);
}

#[test]
fn pre_resolved_physical_selection_never_mutates_display_state() {
    let mut physical = intent(
        "session-physical-selection",
        VirtualDisplayMode::PerClient,
        VirtualDisplayLayout::Extended,
    );
    physical["request_virtual_display"] = json!(false);
    let mut host = Harness::new();
    let prepared = host.response(1, prepare_with_intent("physical-selection", physical));
    assert_eq!(prepared["state"], "prepared");
    assert_eq!(prepared["virtual_display_ready"], false);
    assert!(actions(&host, "amaterasu.virtual-display/1").is_empty());
    assert!(actions(&host, "amaterasu.display/1").is_empty());

    let mut unsupported = intent(
        "session-unsupported-driver",
        VirtualDisplayMode::PerClient,
        VirtualDisplayLayout::Exclusive,
    );
    unsupported["dd_use_sunshine_virtual_display_driver"] = json!(false);
    let mut host = Harness::new();
    let response = host.response(1, prepare_with_intent("unsupported-driver", unsupported));
    assert_eq!(response["error"], "unsupported_virtual_display_driver");
    assert!(actions(&host, "amaterasu.virtual-display/1").is_empty());
    assert!(actions(&host, "amaterasu.display/1").is_empty());
}

#[test]
fn failed_topology_apply_rolls_back_only_the_owned_monitor_reference() {
    let mut host = Harness::new();
    let original_topology = host.broker.display_topology.clone();
    host.broker.display_apply_failure = true;
    let request = prepare_with_intent(
        "vdisplay-apply-failure",
        intent(
            "session-apply-failure",
            VirtualDisplayMode::PerClient,
            VirtualDisplayLayout::Exclusive,
        ),
    );
    let response = host.response(1, request);
    assert_eq!(response["error"], "display_apply_rejected");
    assert!(host.broker.virtual_display_references.is_empty());
    assert!(host.broker.virtual_display_specification.is_none());
    assert_eq!(host.broker.display_topology, original_topology);
    assert!(actions(&host, "amaterasu.virtual-display/1").contains(&"remove".to_owned()));
    assert!(!actions(&host, "amaterasu.display/1").contains(&"restore".to_owned()));
}

#[test]
fn failed_restore_keeps_virtual_reference_until_explicit_recovery() {
    let mut host = Harness::new();
    let original_topology = host.broker.display_topology.clone();
    let prepared = host.response(
        1,
        prepare_with_intent(
            "vdisplay-restore-failure",
            intent(
                "session-restore-failure",
                VirtualDisplayMode::PerClient,
                VirtualDisplayLayout::Exclusive,
            ),
        ),
    );
    let id = prepared["session"].as_u64().unwrap();
    host.broker.display_restore_failures = 1;
    let failed = host.response(2, json!({"action":"stop","session":id}));
    assert_eq!(failed["state"], "cleanup_failed");
    assert!(host.broker.display_lease.is_some());
    assert_eq!(host.broker.virtual_display_references.len(), 1);
    assert!(host.broker.display_topology != original_topology);

    let stopped = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "stopped");
    assert!(host.broker.display_lease.is_none());
    assert!(host.broker.virtual_display_references.is_empty());
    assert_eq!(host.broker.display_topology, original_topology);
}

#[test]
fn failed_journal_keeps_shared_display_restore_with_original_owner() {
    let mut host = Harness::new();
    let first = host.response(
        1,
        prepare_with_intent(
            "shared-first",
            intent(
                "reference-first",
                VirtualDisplayMode::Shared,
                VirtualDisplayLayout::ExtendedPrimary,
            ),
        ),
    );
    let first_id = first["session"].as_u64().unwrap();
    let second = host.response(
        2,
        prepare_with_intent(
            "shared-second",
            intent(
                "reference-second",
                VirtualDisplayMode::Shared,
                VirtualDisplayLayout::ExtendedPrimary,
            ),
        ),
    );
    let second_id = second["session"].as_u64().unwrap();
    host.pipeline.journal.failed = true;
    let stopped = host.response(3, json!({"action":"stop", "session":first_id}));
    assert_eq!(stopped["state"], "cleanup_failed");
    assert!(host.pipeline.records[&first_id].owns_display_restore);
    assert!(!host.pipeline.records[&second_id].owns_display_restore);
    assert!(
        host.broker
            .virtual_display_references
            .contains("reference-first")
    );
}

#[test]
fn shared_monitor_applies_once_and_transfers_restore_ownership_until_last_stop() {
    let mut host = Harness::new();
    let original_topology = host.broker.display_topology.clone();
    let first = host.response(
        1,
        prepare_with_intent(
            "shared-first",
            intent(
                "reference-first",
                VirtualDisplayMode::Shared,
                VirtualDisplayLayout::ExtendedPrimary,
            ),
        ),
    );
    let first_id = first["session"].as_u64().unwrap();
    let second = host.response(
        2,
        prepare_with_intent(
            "shared-second",
            intent(
                "reference-second",
                VirtualDisplayMode::Shared,
                VirtualDisplayLayout::ExtendedPrimary,
            ),
        ),
    );
    let second_id = second["session"].as_u64().unwrap();
    assert_eq!(second["state"], "prepared");
    assert_eq!(second["display_applied"], true);
    assert_eq!(
        actions(&host, "amaterasu.display/1")
            .iter()
            .filter(|action| *action == "apply")
            .count(),
        1
    );
    assert!(host.pipeline.records[&first_id].owns_display_restore);
    assert!(!host.pipeline.records[&second_id].owns_display_restore);

    let first_stopped = host.response(3, json!({"action":"stop","session":first_id}));
    assert_eq!(first_stopped["state"], "stopped");
    assert!(host.broker.display_lease.is_some());
    assert_eq!(
        host.broker.virtual_display_references,
        ["reference-second".into()].into()
    );
    assert_eq!(
        actions(&host, "amaterasu.display/1")
            .iter()
            .filter(|action| *action == "restore")
            .count(),
        0
    );
    assert!(host.pipeline.records[&second_id].owns_display_restore);

    let second_stopped = host.response(4, json!({"action":"stop","session":second_id}));
    assert_eq!(second_stopped["state"], "stopped");
    assert!(host.broker.display_lease.is_none());
    assert_eq!(host.broker.display_topology, original_topology);
    assert!(host.broker.virtual_display_references.is_empty());
    assert_eq!(
        actions(&host, "amaterasu.display/1")
            .iter()
            .filter(|action| *action == "restore")
            .count(),
        1
    );
}

#[test]
fn lost_create_and_remove_results_reconcile_by_the_same_reference() {
    let mut host = Harness::new();
    host.broker.fault = Some((
        "amaterasu.virtual-display/1:create".into(),
        Fault::AfterMutation,
    ));
    let prepared = host.response(
        1,
        prepare_with_intent(
            "lost-vdisplay-create",
            intent(
                "stable-session-reference",
                VirtualDisplayMode::PerClient,
                VirtualDisplayLayout::Exclusive,
            ),
        ),
    );
    let id = prepared["session"].as_u64().unwrap();
    assert_eq!(prepared["virtual_display_ready"], true);
    assert_eq!(
        actions(&host, "amaterasu.virtual-display/1")
            .iter()
            .filter(|action| *action == "create")
            .count(),
        1
    );
    host.broker.fault = Some((
        "amaterasu.virtual-display/1:remove".into(),
        Fault::AfterMutation,
    ));
    let stopped = host.response(2, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "stopped");
    assert!(host.broker.virtual_display_references.is_empty());
    assert_eq!(
        actions(&host, "amaterasu.virtual-display/1")
            .iter()
            .filter(|action| *action == "remove")
            .count(),
        1
    );
}

#[test]
fn cancelled_prepare_after_provider_commit_restores_no_unowned_topology() {
    let mut host = Harness::new();
    host.broker.fault = Some((
        "amaterasu.virtual-display/1:create".into(),
        Fault::AfterMutation,
    ));
    host.broker.enqueue(
        91,
        "owner",
        prepare_with_intent(
            "cancelled-vdisplay-create",
            intent(
                "cancelled-session-reference",
                VirtualDisplayMode::PerClient,
                VirtualDisplayLayout::Exclusive,
            ),
        ),
    );
    for _ in 0..100 {
        host.tick();
        if host.broker.virtual_display_specification.is_some() {
            host.broker.cancel_parent(91);
            break;
        }
    }
    for _ in 0..200 {
        host.tick();
        if host.pipeline.job.is_none()
            && host
                .pipeline
                .records
                .values()
                .any(|record| record.state == State::Cancelled)
        {
            break;
        }
    }
    assert!(host.broker.virtual_display_references.is_empty());
    assert!(host.broker.virtual_display_specification.is_none());
    assert!(actions(&host, "amaterasu.display/1").is_empty());
    assert_eq!(
        host.pipeline
            .records
            .values()
            .next()
            .map(|record| record.state),
        Some(State::Cancelled)
    );
}

#[test]
fn display_is_owned_before_capture_and_removed_only_after_video_closes() {
    let mut host = Harness::new();
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(
        host.response(2, own_display_start(session))["state"],
        "active"
    );
    let identity = crate::pipeline::virtual_display::identity(&host.pipeline.records[&session]);
    assert!(host.broker.virtual_displays.contains_key(&identity));
    let create = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.virtual-display/1" && call.payload["action"] == "create"
        })
        .unwrap();
    let capture = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == Resource::Video.service() && call.payload["action"] == "open"
        })
        .unwrap();
    assert!(create < capture);
    assert_eq!(
        host.broker.calls[capture].payload["output"],
        "kwin:Virtual-2"
    );
    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    assert!(host.broker.virtual_displays.is_empty());
    let close = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == Resource::Video.service() && call.payload["action"] == "close"
        })
        .unwrap();
    let remove = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.virtual-display/1" && call.payload["action"] == "remove"
        })
        .unwrap();
    assert!(close < remove);
}

#[test]
fn external_capture_uses_the_owned_output_and_closes_before_removal() {
    let mut host = Harness::new();
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    let mut request = own_display_start(session);
    request["configuration"]["capture_provider"] = json!("virtual_screen");
    assert_eq!(host.response(2, request)["state"], "active");
    let create = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.virtual-display/1" && call.payload["action"] == "create"
        })
        .unwrap();
    let opens: Vec<_> = host
        .broker
        .calls
        .iter()
        .enumerate()
        .filter(|(_, call)| {
            call.service == Resource::Video.service() && call.payload["action"] == "open"
        })
        .collect();
    assert_eq!(opens.len(), 1);
    assert!(create < opens[0].0);
    assert_eq!(opens[0].1.payload["capture_provider"], "virtual_screen");
    assert_eq!(opens[0].1.payload["output"], "kwin:Virtual-2");
    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    assert!(host.broker.virtual_displays.is_empty());
    let close = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == Resource::Video.service() && call.payload["action"] == "close"
        })
        .unwrap();
    let remove = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.virtual-display/1" && call.payload["action"] == "remove"
        })
        .unwrap();
    assert!(close < remove);
}

#[test]
fn external_capture_failure_removes_owned_output_without_fallback() {
    let mut host = Harness::new();
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    host.broker.fail_open = Some(Resource::Video.service());
    let mut request = own_display_start(session);
    request["configuration"]["capture_provider"] = json!("unknown_alias");
    assert_eq!(host.response(2, request)["state"], "failed");
    assert!(host.broker.virtual_displays.is_empty());
    let opens: Vec<_> = host
        .broker
        .calls
        .iter()
        .filter(|call| {
            call.service == Resource::Video.service() && call.payload["action"] == "open"
        })
        .collect();
    assert_eq!(opens.len(), 1);
    assert_eq!(opens[0].payload["capture_provider"], "unknown_alias");
    assert_eq!(opens[0].payload["output"], "kwin:Virtual-2");
    assert!(host.broker.calls.iter().any(|call| {
        call.service == "amaterasu.virtual-display/1" && call.payload["action"] == "remove"
    }));
}

#[test]
fn failed_connector_cleanup_retains_the_write_ahead_record() {
    let mut host = Harness::new();
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(
        host.response(2, own_display_start(session))["state"],
        "active"
    );
    host.broker.virtual_display_fail_remove = true;
    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "cleanup_failed"
    );
    assert!(host.pipeline.records[&session].virtual_display_attempted);
    assert!(
        host.broker.state_value.as_ref().unwrap()["entries"][session.to_string()]["virtual_display_attempted"]
            == true
    );
}

#[test]
fn restart_recovers_the_exact_monitor_identity_without_new_allocation() {
    let mut host = Harness::new();
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(
        host.response(2, own_display_start(session))["state"],
        "active"
    );
    let identity = crate::pipeline::virtual_display::identity(&host.pipeline.records[&session]);
    let document = host.broker.state_value.clone().unwrap();
    let mut restarted = Harness::with_restart_state(document, host.broker.state_revision);
    restarted.broker.virtual_displays = host.broker.virtual_displays.clone();
    restarted
        .broker
        .seed_session(&format!("ss-{session:x}-session"));
    // Other resources were not recreated; lookup confirms their absence.
    restarted.idle();
    assert!(!restarted.broker.virtual_displays.contains_key(&identity));
    assert!(
        !restarted
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.virtual-display/1"
                && call.payload["action"] == "create")
    );
}

#[test]
fn owned_output_centers_the_pointer_and_refreshes_absolute_mapping_when_a_peer_arrives() {
    use amaterasu_media::{CaptureGeometry, DesktopRect};
    let mut host = Harness::new();
    host.pipeline.settings.enable_input = true;
    let first = CaptureGeometry::new(
        DesktopRect::new(1536, 0, 1920, 1080).unwrap(),
        DesktopRect::new(0, 0, 3456, 1080).unwrap(),
    )
    .unwrap();
    host.broker.desktop_geometry = Some(json!(first));
    host.broker.input_absolute_pointer = Some(json!(true));
    host.plane.borrow_mut().desktop_geometry = Some(first);
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(
        host.response(2, own_display_start(session))["state"],
        "active"
    );
    host.idle();
    let bounds = first.normalized_output().unwrap();
    assert!(
        host.broker
            .input_events
            .iter()
            .any(|event| event["kind"] == "absolute"
                && event["x"] == (u32::from(bounds.left) + u32::from(bounds.right)) / 2)
    );
    let expanded =
        CaptureGeometry::new(first.output, DesktopRect::new(0, 0, 5376, 1080).unwrap()).unwrap();
    host.plane.borrow_mut().desktop_geometry = Some(expanded);
    host.idle();
    assert_eq!(
        host.pipeline.records[&session].capture_geometry,
        Some(expanded)
    );
}

fn own_display_start(session: u64) -> Value {
    let mut request = start(session);
    request["configuration"]["capture_provider"] = json!("pipewire");
    request["configuration"]["output"] = json!(crate::pipeline::virtual_display::REQUESTED_OUTPUT);
    request
}
