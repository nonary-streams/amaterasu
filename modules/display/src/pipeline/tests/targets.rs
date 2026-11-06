use super::*;

fn pipeline(mutation: bool) -> Pipeline<FakePlatform> {
    Pipeline::new(
        FakePlatform {
            current_snapshot: Some(snapshot()),
            scoped_readback: true,
            hdr_supported: true,
            ..Default::default()
        },
        mutation,
    )
}
fn selector(pipeline: &mut Pipeline<FakePlatform>) -> String {
    pipeline.platform.capture_targets().unwrap()[0]
        .descriptor
        .selector
        .clone()
}
fn borrow(selector: &str, reference: &str, operation: &str) -> Value {
    json!({"action":"borrow_target","operation_id":operation,"selector":selector,"reference_id":reference})
}
fn release(selector: &str, reference: &str) -> Value {
    json!({"action":"release_target","operation_id":"release-source","selector":selector,"reference_id":reference})
}

#[test]
fn existing_target_inventory_and_borrow_have_no_native_mutations() {
    let mut state = State::default();
    let mut pipeline = pipeline(false);
    let inventory = state.run(&mut pipeline, "remote", json!({"action":"capture_targets"}));
    let rows = amaterasu_module_sdk::display::targets::inventory_from_reply(inventory).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].selector.contains("monitor.one"));
    let resolved = state.run(
        &mut pipeline,
        "remote",
        json!({"action":"resolve_capture_target","selector":rows[0].selector}),
    );
    assert!(
        amaterasu_module_sdk::display::targets::resolved_from_reply(resolved, &rows[0].selector)
            .is_ok()
    );
    let borrowed = state.run(
        &mut pipeline,
        "remote",
        borrow(&rows[0].selector, "source", "borrow-1"),
    );
    let binding = amaterasu_module_sdk::display::targets::borrowed_from_reply(
        borrowed,
        &rows[0].selector,
        "source",
    )
    .unwrap();
    assert_eq!(binding.mode.width, 1920);
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_some());
    let released = state.run(
        &mut pipeline,
        "remote",
        release(&rows[0].selector, "source"),
    );
    assert!(amaterasu_module_sdk::display::targets::released_from_reply(
        &released,
        &rows[0].selector,
        "source"
    ));
    assert!(pipeline.platform.effects.is_empty());
    assert_eq!(
        pipeline.platform.current_snapshot.as_ref().unwrap().paths[0].id,
        "monitor.one"
    );
    assert!(state.journal.is_none());
}

#[test]
fn native_source_is_frozen_across_retries_and_provider_restart() {
    let mut state = State::default();
    let mut provider = pipeline(false);
    let selected = selector(&mut provider);
    let accepted = state.run(
        &mut provider,
        "remote",
        borrow(&selected, "source", "borrow-1"),
    );
    assert_eq!(accepted["ok"], true, "{accepted}");
    let again = state.run(
        &mut provider,
        "remote",
        borrow(&selected, "source", "borrow-1"),
    );
    assert_eq!(accepted, again);
    let mut restarted = pipeline(false);
    restarted.platform.current_snapshot.as_mut().unwrap().paths[0]
        .refresh
        .numerator = 75;
    let refused = state.run(
        &mut restarted,
        "remote",
        borrow(&selected, "source", "borrow-1"),
    );
    assert_eq!(refused["error"]["code"], "capture_target_changed");
    assert_eq!(
        state.journal.as_ref().unwrap()["scoped_targets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(restarted.platform.effects.is_empty());
    let release = state.run(&mut restarted, "remote", release(&selected, "source"));
    assert_eq!(release["released"], true);
    assert_eq!(
        restarted.platform.current_snapshot.as_ref().unwrap().paths[0]
            .refresh
            .numerator,
        75
    );
}

#[test]
fn normal_restore_keeps_the_independently_borrowed_physical_source() {
    let mut state = State::default();
    let mut pipeline = pipeline(true);
    let mut configuration = snapshot().configuration().unwrap();
    configuration.displays[0].refresh.numerator = 75;
    let normal = state.run(
        &mut pipeline,
        "game",
        json!({"action":"apply","configuration":configuration}),
    );
    assert_eq!(normal["applied"], true, "{normal}");
    let selected = selector(&mut pipeline);
    let borrowed = state.run(
        &mut pipeline,
        "remote",
        borrow(&selected, "source", "borrow-1"),
    );
    assert_eq!(borrowed["ok"], true, "{borrowed}");
    let restored = state.run(
        &mut pipeline,
        "game",
        json!({"action":"restore","lease":normal["lease"]}),
    );
    assert_eq!(restored["restored"], true, "{restored}");
    assert_eq!(
        pipeline.platform.current_snapshot.as_ref().unwrap().paths[0]
            .refresh
            .numerator,
        75
    );
    let journal: Journal = serde_json::from_value(state.journal.clone().unwrap()).unwrap();
    assert!(!journal.normal_active);
    assert!(journal.scoped_targets[0].borrowed);
    let released = state.run(&mut pipeline, "remote", release(&selected, "source"));
    assert_eq!(released["released"], true);
    assert_eq!(
        pipeline.platform.current_snapshot.as_ref().unwrap().paths[0]
            .refresh
            .numerator,
        75
    );
}

#[test]
fn foreign_release_is_inert_and_multiple_sources_share_the_same_exact_target() {
    let mut state = State::default();
    let mut pipeline = pipeline(false);
    let selected = selector(&mut pipeline);
    assert_eq!(
        state.run(
            &mut pipeline,
            "alice",
            borrow(&selected, "source", "alice-1")
        )["ok"],
        true
    );
    assert_eq!(
        state.run(&mut pipeline, "bob", release(&selected, "source"))["released"],
        true
    );
    assert_eq!(
        state.journal.as_ref().unwrap()["scoped_targets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let second = state.run(&mut pipeline, "bob", borrow(&selected, "source", "bob-1"));
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(
        state.journal.as_ref().unwrap()["scoped_targets"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        state.run(&mut pipeline, "alice", release(&selected, "source"))["released"],
        true
    );
    assert_eq!(
        state.journal.as_ref().unwrap()["scoped_targets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn a_source_changed_while_its_reference_is_committed_never_returns_a_binding() {
    let mut state = State::default();
    let mut pipeline = pipeline(false);
    let selected = selector(&mut pipeline);
    pipeline.platform.drift_capture_target_after =
        Some(pipeline.platform.capture_target_queries + 2);
    let refused = state.run(
        &mut pipeline,
        "remote",
        borrow(&selected, "source", "borrow-1"),
    );
    assert_eq!(
        refused["error"]["code"], "capture_target_changed",
        "{refused}"
    );
    assert!(state.journal.is_some());
    assert!(pipeline.platform.effects.is_empty());
    assert_eq!(
        state.run(&mut pipeline, "remote", release(&selected, "source"))["released"],
        true
    );
}

#[test]
fn lost_acceptance_and_storage_failure_do_not_abandon_or_fabricate_references() {
    let mut state = State {
        lose_set_acceptance: true,
        ..Default::default()
    };
    let mut pipeline = pipeline(false);
    let selected = selector(&mut pipeline);
    let request = borrow(&selected, "source", "borrow-1");
    assert_ne!(
        state.run(&mut pipeline, "remote", request.clone())["ok"],
        true
    );
    assert!(state.journal.is_some());
    assert_eq!(state.run(&mut pipeline, "remote", request)["ok"], true);
    assert_eq!(
        state.journal.as_ref().unwrap()["scoped_targets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        state.run(&mut pipeline, "remote", release(&selected, "source"))["released"],
        true
    );
    state.fail_save = true;
    assert_ne!(
        state.run(
            &mut pipeline,
            "remote",
            borrow(&selected, "source", "borrow-2")
        )["ok"],
        true
    );
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn an_owned_monitor_cannot_be_reconfigured_or_released_while_a_game_borrows_it() {
    let mut state = State::default();
    let mut pipeline = pipeline(true);
    let add = json!({"action":"scoped_apply","operation_id":"monitor-add",
        "identity":"00000000-0000-0000-0000-000000000001","reference_id":"monitor-retained",
        "expected_topology_revision":pipeline.platform.current_snapshot.as_ref().unwrap().revision().unwrap(),
        "target":{"id":"monitor.remote","width":1280,"height":720,"x":1920,"y":0,"refresh":{"numerator":60000,"denominator":1000},"rotation":"identity","hdr_enabled":false}});
    assert_eq!(
        state.run(&mut pipeline, "remote", add.clone())["applied"],
        true
    );
    let selected = pipeline
        .platform
        .capture_targets()
        .unwrap()
        .into_iter()
        .find(|target| target.native_id == "monitor.remote")
        .unwrap()
        .descriptor
        .selector;
    let borrowed = state.run(
        &mut pipeline,
        "game",
        borrow(&selected, "game-source", "borrow-game"),
    );
    assert_eq!(borrowed["ok"], true, "{borrowed}");
    let release_scope = json!({"action":"scoped_release","operation_id":"monitor-release",
        "identity":add["identity"],"reference_id":"monitor-retained",
        "expected_topology_revision":pipeline.platform.current_snapshot.as_ref().unwrap().revision().unwrap()});
    let refused = state.run(&mut pipeline, "remote", release_scope.clone());
    assert_eq!(refused["error"]["code"], "target_in_use", "{refused}");
    let mut change = add;
    change["operation_id"] = json!("monitor-change");
    change["expected_topology_revision"] = json!(
        pipeline
            .platform
            .current_snapshot
            .as_ref()
            .unwrap()
            .revision()
            .unwrap()
    );
    change["target"]["width"] = json!(1600);
    state.scoped_verifier_response = Some(
        json!({"ok":true,"owned":true,"target_id":"monitor.remote",
        "specification":{"identity":change["identity"],"name":"Remote Monitor","hdr_enabled":false,"width":1600,"height":720,"refresh_millihz":60000},"active":true}),
    );
    let refused = state.run(&mut pipeline, "remote", change);
    assert_eq!(refused["error"]["code"], "target_in_use", "{refused}");
    assert_eq!(
        state.run(&mut pipeline, "game", release(&selected, "game-source"))["released"],
        true
    );
    assert_eq!(
        state.run(&mut pipeline, "remote", release_scope)["released"],
        true
    );
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
}

#[test]
fn an_unplugged_or_unobservable_source_does_not_prevent_reference_retirement() {
    let mut state = State::default();
    let mut pipeline = pipeline(false);
    let selected = selector(&mut pipeline);
    assert_eq!(
        state.run(
            &mut pipeline,
            "remote",
            borrow(&selected, "source", "borrow-1")
        )["ok"],
        true
    );
    pipeline.platform.capture_targets_unavailable = true;
    let released = state.run(&mut pipeline, "remote", release(&selected, "source"));
    assert_eq!(released["released"], true, "{released}");
    assert_eq!(released["native_removed"], false);
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
    let again = state.run(&mut pipeline, "remote", release(&selected, "source"));
    assert_eq!(again["released"], true);
}
