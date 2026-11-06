use super::*;

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
