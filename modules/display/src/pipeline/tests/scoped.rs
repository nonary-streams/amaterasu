use super::*;

mod status;

fn pipeline() -> Pipeline<FakePlatform> {
    Pipeline::new(
        FakePlatform {
            current_snapshot: Some(snapshot()),
            scoped_readback: true,
            hdr_supported: true,
            ..Default::default()
        },
        true,
    )
}

fn add_request(pipeline: &Pipeline<FakePlatform>, operation_id: &str) -> Value {
    json!({
        "action":"scoped_apply","operation_id":operation_id,
        "identity":"00000000-0000-0000-0000-000000000001","reference_id":"monitor-retained",
        "expected_topology_revision":pipeline.platform.current_snapshot.as_ref().unwrap().revision().unwrap(),
        "target":{"id":"monitor.remote","width":1280,"height":720,"x":1920,"y":0,
            "refresh":{"numerator":60000,"denominator":1000},"rotation":"identity","hdr_enabled":false},
    })
}

fn release_request(pipeline: &Pipeline<FakePlatform>, operation_id: &str) -> Value {
    json!({
        "action":"scoped_release","operation_id":operation_id,
        "identity":"00000000-0000-0000-0000-000000000001","reference_id":"monitor-retained",
        "expected_topology_revision":pipeline.platform.current_snapshot.as_ref().unwrap().revision().unwrap(),
    })
}

fn normal_request(pipeline: &Pipeline<FakePlatform>, refresh: u32) -> Value {
    let before = pipeline.platform.current_snapshot.as_ref().unwrap();
    let mut configuration = before.configuration().unwrap();
    configuration.displays[0].refresh.numerator = refresh;
    json!({"action":"apply","configuration":configuration,"expected_topology_revision":before.revision().unwrap()})
}

#[test]
fn active_game_restore_keeps_later_monitor_and_its_latest_native_mode() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    let request = normal_request(&pipeline, 75);
    let normal = state.run(&mut pipeline, "game", request);
    assert_eq!(normal["applied"], true, "{normal}");
    let before_remote = pipeline.platform.current_snapshot.as_ref().unwrap().paths[0].clone();
    let request = add_request(&pipeline, "monitor-add");
    let added = state.run(&mut pipeline, "remote", request);
    assert_eq!(added["applied"], true, "{added}");
    assert_eq!(
        pipeline.platform.current_snapshot.as_ref().unwrap().paths[0],
        before_remote
    );
    // A later independently observed mode remains owned by the monitor, rather
    // than being overwritten by the game's older restoration baseline.
    let monitor = pipeline
        .platform
        .current_snapshot
        .as_mut()
        .unwrap()
        .paths
        .iter_mut()
        .find(|path| path.id == "monitor.remote")
        .unwrap();
    monitor.source.width = 1600;
    monitor.signal.active_width = 1600;
    let protected = monitor.clone();
    let restored = state.run(
        &mut pipeline,
        "game",
        json!({"action":"restore","lease":normal["lease"]}),
    );
    assert_eq!(restored["restored"], true, "{restored}");
    let current = pipeline.platform.current_snapshot.as_ref().unwrap();
    assert_eq!(current.paths[0].refresh.numerator, 60);
    assert_eq!(
        current
            .paths
            .iter()
            .find(|path| path.id == "monitor.remote")
            .unwrap(),
        &protected
    );
    let journal: Journal = serde_json::from_value(state.journal.clone().unwrap()).unwrap();
    assert!(!journal.normal_active);
    assert_eq!(journal.scoped_targets.len(), 1);
}

#[test]
fn retained_monitor_allows_new_game_only_when_its_configuration_is_preserved() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    let request = add_request(&pipeline, "monitor-add");
    assert_eq!(state.run(&mut pipeline, "remote", request)["applied"], true);
    let mut remove = normal_request(&pipeline, 75);
    remove["configuration"]["displays"]
        .as_array_mut()
        .unwrap()
        .retain(|display| display["id"] != "monitor.remote");
    let rejected = state.run(&mut pipeline, "game", remove);
    assert_eq!(rejected["error"]["code"], "protected_target_conflict");
    let request = normal_request(&pipeline, 75);
    let normal = state.run(&mut pipeline, "game", request);
    assert_eq!(normal["applied"], true, "{normal}");
    assert_eq!(
        serde_json::from_value::<Journal>(state.journal.clone().unwrap())
            .unwrap()
            .scoped_targets
            .len(),
        1
    );
    let restored = state.run(
        &mut pipeline,
        "game",
        json!({"action":"restore","lease":normal["lease"]}),
    );
    assert_eq!(restored["protected_targets_retained"], true, "{restored}");
}

#[test]
fn released_scope_never_claims_native_removal_or_resurrects_a_retired_target() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    let request = add_request(&pipeline, "monitor-add");
    assert_eq!(state.run(&mut pipeline, "remote", request)["applied"], true);
    let request = normal_request(&pipeline, 75);
    let normal = state.run(&mut pipeline, "game", request);
    assert_eq!(normal["applied"], true, "{normal}");
    let request = release_request(&pipeline, "monitor-release");
    let released = state.run(&mut pipeline, "remote", request);
    assert_eq!(released["released"], true, "{released}");
    assert_eq!(released["native_removed"], false);
    assert!(
        pipeline
            .platform
            .current_snapshot
            .as_ref()
            .unwrap()
            .paths
            .iter()
            .any(|path| path.id == "monitor.remote")
    );
    // The separately owned VDD provider confirms actual native retirement.
    pipeline
        .platform
        .current_snapshot
        .as_mut()
        .unwrap()
        .paths
        .retain(|path| path.id != "monitor.remote");
    let restored = state.run(
        &mut pipeline,
        "game",
        json!({"action":"restore","lease":normal["lease"]}),
    );
    assert_eq!(restored["restored"], true, "{restored}");
    assert_eq!(
        pipeline
            .platform
            .current_snapshot
            .as_ref()
            .unwrap()
            .paths
            .len(),
        1
    );
    assert!(state.journal.is_none());
}

#[test]
fn unchanged_consumer_accepts_two_verifiers_and_rejects_wrong_identity_or_target() {
    for target in ["monitor.remote", "MONITOR.REMOTE"] {
        let mut state = State::default();
        let mut pipeline = pipeline();
        let mut request = add_request(&pipeline, "monitor-add");
        request["target"]["id"] = json!(target);
        state.scoped_verifier_response = Some(json!({"ok":true,"owned":true,"target_id":target,
            "specification":{"identity":request["identity"],"name":"Remote Monitor","hdr_enabled":false,
                "width":1280,"height":720,"refresh_millihz":60000},"active":false}));
        let response = state.run(&mut pipeline, "remote", request);
        assert_eq!(response["applied"], true, "{response}");
    }
    for (field, value) in [
        ("owned", json!(false)),
        ("target_id", json!("foreign")),
        ("specification", json!({})),
    ] {
        let mut state = State::default();
        let mut pipeline = pipeline();
        let request = add_request(&pipeline, "monitor-add");
        let mut proof = json!({"ok":true,"owned":true,"target_id":"monitor.remote",
            "specification":{"identity":request["identity"],"name":"Remote Monitor","hdr_enabled":false,
                "width":1280,"height":720,"refresh_millihz":60000},"active":true});
        proof[field] = value;
        state.scoped_verifier_response = Some(proof);
        let rejected = state.run(&mut pipeline, "remote", request);
        assert_eq!(rejected["error"]["code"], "target_ownership_unverified");
        assert!(pipeline.platform.effects.is_empty());
        assert!(state.journal.is_none());
    }
}

#[test]
fn lost_admission_or_final_completion_reconciles_without_repeating_native_effects() {
    for lose_admission in [true, false] {
        let mut state = State::default();
        let mut pipeline = pipeline();
        state.lose_set_acceptance = lose_admission;
        state.lose_apply_completion = !lose_admission;
        let request = add_request(&pipeline, "monitor-add");
        let first = state.run(&mut pipeline, "remote", request.clone());
        assert_ne!(first["applied"], true);
        assert!(state.journal.is_some());
        let retried = state.run(&mut pipeline, "remote", request);
        assert!(
            retried["applied"] == true || retried["reconciled"] == true,
            "{retried}"
        );
        assert_eq!(pipeline.platform.effects, ["apply"]);
        assert!(
            serde_json::from_value::<Journal>(state.journal.clone().unwrap())
                .unwrap()
                .scoped_pending
                .is_none()
        );
    }
}

#[test]
fn lost_commit_acknowledgement_keeps_verified_target_and_retry_is_idempotent() {
    let mut state = State {
        lose_scoped_commit: true,
        ..Default::default()
    };
    let mut pipeline = pipeline();
    let request = add_request(&pipeline, "monitor-add");
    let first = state.run(&mut pipeline, "remote", request.clone());
    assert_ne!(first["applied"], true);
    let retried = state.run(&mut pipeline, "remote", request);
    assert_eq!(retried["reconciled"], true, "{retried}");
    assert_eq!(pipeline.platform.effects, ["apply"]);
}

#[test]
fn uncertain_scoped_effect_fences_game_restore_until_exact_owner_cleanup() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    let request = normal_request(&pipeline, 75);
    let normal = state.run(&mut pipeline, "game", request);
    pipeline.platform.apply_failure = true;
    let request = add_request(&pipeline, "monitor-add");
    let failed = state.run(&mut pipeline, "remote", request);
    assert_eq!(failed["scoped_pending"], true, "{failed}");
    let restored = state.run(
        &mut pipeline,
        "game",
        json!({"action":"restore","lease":normal["lease"]}),
    );
    assert_eq!(restored["error"]["code"], "scoped_recovery_required");
    pipeline.platform.apply_failure = false;
    let request = release_request(&pipeline, "monitor-release");
    let foreign = state.run(&mut pipeline, "foreign", request.clone());
    assert_eq!(foreign["error"]["code"], "scoped_recovery_required");
    let released = state.run(&mut pipeline, "remote", request);
    assert_eq!(released["released"], true, "{released}");
    pipeline
        .platform
        .current_snapshot
        .as_mut()
        .unwrap()
        .paths
        .retain(|path| path.id != "monitor.remote");
    let restored = state.run(
        &mut pipeline,
        "game",
        json!({"action":"restore","lease":normal["lease"]}),
    );
    assert_eq!(restored["restored"], true, "{restored}");
}

#[test]
fn stale_scope_and_operation_retargeting_fail_before_any_new_effect() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    let mut request = add_request(&pipeline, "monitor-add");
    request["expected_topology_revision"] = json!("stale");
    assert_eq!(
        state.run(&mut pipeline, "remote", request)["error"]["code"],
        "topology_changed"
    );
    assert!(pipeline.platform.effects.is_empty());
    let request = add_request(&pipeline, "monitor-add");
    assert_eq!(state.run(&mut pipeline, "remote", request)["applied"], true);
    let mut changed = add_request(&pipeline, "monitor-add");
    changed["target"]["y"] = json!(100);
    assert_eq!(
        state.run(&mut pipeline, "remote", changed)["error"]["code"],
        "operation_conflict"
    );
    assert_eq!(pipeline.platform.effects, ["apply"]);
}

#[test]
fn kscreen_scoped_target_moves_without_changing_peer_mode_scale_rotation_or_hdr() {
    let mut baseline = kscreen_snapshot();
    baseline.outputs[0].scale = 1.0;
    let mut remote = baseline.outputs[1].clone();
    remote.id = "Virtual-1".into();
    remote.position = KScreenPosition { x: 4480, y: 0 };
    remote.mode = Some(KScreenModeSnapshot {
        id: "remote-mode".into(),
        width: 1280,
        height: 720,
        refresh_millihz: 60000,
    });
    remote.rotation = 1;
    remote.priority = 3;
    baseline.outputs.push(remote);
    let expected_peers = baseline.outputs[..2].to_vec();
    let expected_revision = baseline.revision().unwrap();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            kscreen_baseline: Some(baseline),
            scoped_readback: true,
            scoped_only: true,
            ..Default::default()
        },
        true,
    );
    let mut state = State {
        scoped_verifier_response: Some(
            json!({"ok":true,"owned":true,"target_id":"Virtual-1","specification":{"identity":"00000000-0000-0000-0000-000000000001","name":"Remote Monitor","hdr_enabled":false,"width":1280,"height":720,"refresh_millihz":60000},"active":true}),
        ),
        ..Default::default()
    };
    let request = json!({"action":"scoped_apply","operation_id":"kscreen-position",
        "identity":"00000000-0000-0000-0000-000000000001","reference_id":"monitor-retained",
        "expected_topology_revision":expected_revision,
        "target":{"id":"Virtual-1","width":1280,"height":720,"x":4480,"y":200,
            "refresh":{"numerator":60000,"denominator":1000},"rotation":"identity","hdr_enabled":false}});
    let response = state.run(&mut pipeline, "remote", request);
    assert_eq!(response["applied"], true, "{response}");
    let readback = pipeline.platform.kscreen_baseline.as_ref().unwrap();
    assert_eq!(&readback.outputs[..2], expected_peers.as_slice());
    assert_eq!(readback.outputs[2].position.y, 200);
    assert_eq!(pipeline.platform.effects, ["apply_kscreen"]);
}

#[test]
fn primary_monitor_translates_one_coordinate_origin_and_survives_game_restore() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    let mut request = add_request(&pipeline, "monitor-primary");
    request["primary"] = json!(true);
    let repeated = request.clone();
    let applied = state.run(&mut pipeline, "remote", request);
    assert_eq!(applied["applied"], true, "{applied}");
    let current = pipeline.platform.current_snapshot.as_ref().unwrap();
    let physical = current
        .paths
        .iter()
        .find(|path| path.id == "monitor.one")
        .unwrap();
    let remote = current
        .paths
        .iter()
        .find(|path| path.id == "monitor.remote")
        .unwrap();
    assert_eq!((physical.source.x, physical.source.y), (-1920, 0));
    assert_eq!((remote.source.x, remote.source.y), (0, 0));
    assert_eq!(physical.refresh.numerator, 60);
    assert_eq!(
        state.run(&mut pipeline, "remote", repeated)["reconciled"],
        true
    );
    let request = normal_request(&pipeline, 75);
    let normal = state.run(&mut pipeline, "game", request);
    assert_eq!(normal["applied"], true, "{normal}");
    let restored = state.run(
        &mut pipeline,
        "game",
        json!({"action":"restore","lease":normal["lease"]}),
    );
    assert_eq!(restored["restored"], true, "{restored}");
    let current = pipeline.platform.current_snapshot.as_ref().unwrap();
    let physical = current
        .paths
        .iter()
        .find(|path| path.id == "monitor.one")
        .unwrap();
    let remote = current
        .paths
        .iter()
        .find(|path| path.id == "monitor.remote")
        .unwrap();
    assert_eq!((physical.source.x, physical.source.y), (-1920, 0));
    assert_eq!(physical.refresh.numerator, 60);
    assert_eq!((remote.source.x, remote.source.y), (0, 0));
    assert_eq!(current.paths[0].id, "monitor.remote");
}

#[test]
fn scaled_desktop_and_primary_priorities_use_observed_logical_footprints() {
    let mut baseline = kscreen_snapshot();
    baseline.outputs[0].mode = Some(KScreenModeSnapshot {
        id: "4k".into(),
        width: 3840,
        height: 2160,
        refresh_millihz: 60000,
    });
    baseline.outputs[0].scale = 2.0;
    baseline.outputs[1].id = "Virtual-1".into();
    baseline.outputs[1].position = KScreenPosition { x: 1920, y: 0 };
    baseline.outputs[1].rotation = 1;
    baseline.outputs[1].mode = Some(KScreenModeSnapshot {
        id: "remote".into(),
        width: 1280,
        height: 720,
        refresh_millihz: 60000,
    });
    let revision = baseline.revision().unwrap();
    let peer = baseline.outputs[0].clone();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            kscreen_baseline: Some(baseline),
            scoped_readback: true,
            scoped_only: true,
            ..Default::default()
        },
        true,
    );
    let mut state = State {
        scoped_verifier_response: Some(
            json!({"ok":true,"owned":true,"target_id":"Virtual-1","specification":{"identity":"00000000-0000-0000-0000-000000000001","name":"Remote Monitor","hdr_enabled":false,"width":1280,"height":720,"refresh_millihz":60000},"active":true}),
        ),
        ..Default::default()
    };
    let request = json!({"action":"scoped_apply","operation_id":"scaled-primary","identity":"00000000-0000-0000-0000-000000000001","reference_id":"retained","expected_topology_revision":revision,"primary":true,
        "target":{"id":"Virtual-1","width":1280,"height":720,"x":1920,"y":0,"refresh":{"numerator":60000,"denominator":1000},"rotation":"identity","hdr_enabled":false}});
    let applied = state.run(&mut pipeline, "remote", request);
    assert_eq!(applied["applied"], true, "{applied}");
    let current = pipeline.platform.kscreen_baseline.as_ref().unwrap();
    assert_eq!(current.outputs[0].position.x, -1920);
    assert_eq!(current.outputs[0].scale, peer.scale);
    assert_eq!(current.outputs[0].mode, peer.mode);
    assert_eq!(current.outputs[0].hdr_enabled, peer.hdr_enabled);
    assert!(!current.outputs[0].primary);
    assert!(current.outputs[1].primary);
    assert_eq!(current.outputs[1].position.x, 0);
}

fn retained_mode_proof(request: &Value, mode_operation: &str) -> Value {
    json!({"ok":true,"owned":true,"target_id":"monitor.remote","mode_operation_id":mode_operation,
        "specification":{"identity":request["identity"],"name":"Remote Monitor","width":1600,"height":900,"refresh_millihz":60000,"hdr_enabled":true},"active":true})
}

fn mode_request(pipeline: &Pipeline<FakePlatform>, operation: &str) -> Value {
    let mut request = add_request(pipeline, operation);
    request["action"] = json!("scoped_reconfigure");
    request["mode_operation_id"] = json!("retained-mode-1");
    request["target"]["width"] = json!(1600);
    request["target"]["height"] = json!(900);
    request["target"]["hdr_enabled"] = json!(true);
    request
}

fn reconcile_request(operation: &str) -> Value {
    json!({"action":"scoped_reconcile","operation_id":operation,
        "identity":"00000000-0000-0000-0000-000000000001","reference_id":"monitor-retained"})
}

#[test]
fn retained_mode_mutation_requires_exact_prepared_operation_and_color_proof() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    let request = add_request(&pipeline, "monitor-add");
    assert_eq!(state.run(&mut pipeline, "remote", request)["applied"], true);
    let request = mode_request(&pipeline, "monitor-change");
    state.scoped_verifier_response = Some(retained_mode_proof(&request, "different-mode"));
    assert_eq!(
        state.run(&mut pipeline, "remote", request.clone())["error"]["code"],
        "retained_mode_unverified"
    );
    let mut wrong_color = retained_mode_proof(&request, "retained-mode-1");
    wrong_color["specification"]["hdr_enabled"] = json!(false);
    state.scoped_verifier_response = Some(wrong_color);
    assert_eq!(
        state.run(&mut pipeline, "remote", request.clone())["error"]["code"],
        "target_ownership_unverified"
    );
    assert_eq!(pipeline.platform.effects, ["apply"]);
    state.scoped_verifier_response = Some(retained_mode_proof(&request, "retained-mode-1"));
    let applied = state.run(&mut pipeline, "remote", request);
    assert_eq!(applied["applied"], true, "{applied}");
    let monitor = pipeline
        .platform
        .current_snapshot
        .as_ref()
        .unwrap()
        .paths
        .iter()
        .find(|path| path.id == "monitor.remote")
        .unwrap();
    assert_eq!((monitor.source.width, monitor.source.height), (1600, 900));
    assert_eq!(monitor.hdr_enabled, Some(true));
}

#[test]
fn reconcile_recovers_prepared_mode_without_rebuilding_original_snapshot_request() {
    for native_applied in [false, true] {
        let mut state = State::default();
        let mut pipeline = pipeline();
        let request = add_request(&pipeline, "monitor-add");
        assert_eq!(state.run(&mut pipeline, "remote", request)["applied"], true);
        let request = mode_request(&pipeline, "monitor-change");
        state.scoped_verifier_response = Some(retained_mode_proof(&request, "retained-mode-1"));
        if native_applied {
            pipeline.platform.apply_failure = true;
        } else {
            state.lose_set_acceptance = true;
        }
        let response = state.run(&mut pipeline, "remote", request);
        assert_ne!(response["applied"], true);
        pipeline.platform.apply_failure = false;
        let prior_effects = pipeline.platform.effects.clone();
        let response = state.run(&mut pipeline, "remote", reconcile_request("monitor-change"));
        assert_eq!(
            response["state"],
            if native_applied {
                "succeeded"
            } else {
                "not_applied"
            },
            "{response}"
        );
        assert_eq!(response["mode_operation_id"], "retained-mode-1");
        assert_eq!(pipeline.platform.effects, prior_effects);
        assert!(
            serde_json::from_value::<Journal>(state.journal.clone().unwrap())
                .unwrap()
                .scoped_pending
                .is_none()
        );
        if !native_applied {
            let request = mode_request(&pipeline, "monitor-change-retry");
            assert_eq!(state.run(&mut pipeline, "remote", request)["applied"], true);
        }
    }
}

#[test]
fn scoped_reconcile_cannot_admit_unknown_or_foreign_native_mutations() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    assert_eq!(
        state.run(&mut pipeline, "remote", reconcile_request("unknown"))["error"]["code"],
        "scoped_operation_not_found"
    );
    let request = add_request(&pipeline, "monitor-add");
    assert_eq!(state.run(&mut pipeline, "remote", request)["applied"], true);
    let effects = pipeline.platform.effects.clone();
    assert_eq!(
        state.run(&mut pipeline, "foreign", reconcile_request("monitor-add"))["error"]["code"],
        "scoped_operation_not_found"
    );
    assert_eq!(
        state.run(&mut pipeline, "remote", reconcile_request("unknown"))["error"]["code"],
        "scoped_operation_not_found"
    );
    let completed = state.run(&mut pipeline, "remote", reconcile_request("monitor-add"));
    assert_eq!(completed["state"], "succeeded", "{completed}");
    assert_eq!(pipeline.platform.effects, effects);
}

#[test]
fn second_monitor_primary_waits_for_normal_game_lifetime_before_changing_input_origin() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    let first = add_request(&pipeline, "first-monitor");
    assert_eq!(
        state.run(&mut pipeline, "first-owner", first)["applied"],
        true
    );
    let normal = normal_request(&pipeline, 75);
    assert_eq!(state.run(&mut pipeline, "game", normal)["applied"], true);
    let before = pipeline.platform.current_snapshot.clone().unwrap();
    let effects = pipeline.platform.effects.clone();
    let mut second = add_request(&pipeline, "second-monitor-primary");
    second["identity"] = json!("00000000-0000-0000-0000-000000000002");
    second["reference_id"] = json!("second-retained");
    second["target"]["id"] = json!("monitor.second");
    second["target"]["x"] = json!(3200);
    second["primary"] = json!(true);
    state.scoped_verifier_response = Some(
        json!({"ok":true,"owned":true,"target_id":"monitor.second",
        "specification":{"identity":second["identity"],"name":"Second Monitor","width":1280,"height":720,"refresh_millihz":60000,"hdr_enabled":false},"active":false}),
    );
    let rejected = state.run(&mut pipeline, "second-owner", second);
    assert_eq!(rejected["error"]["code"], "target_in_use", "{rejected}");
    assert_eq!(pipeline.platform.effects, effects);
    assert!(
        pipeline
            .platform
            .current_snapshot
            .as_ref()
            .unwrap()
            .equivalent(&before)
    );
    let journal: Journal = serde_json::from_value(state.journal.unwrap()).unwrap();
    assert!(journal.normal_active);
    assert_eq!(journal.scoped_targets.len(), 1);
    assert!(journal.scoped_pending.is_none());
}
