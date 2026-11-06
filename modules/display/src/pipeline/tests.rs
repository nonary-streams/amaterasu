use super::*;
use crate::model::{
    Address, DisplayPath, KScreenModeSnapshot, KScreenOutputSnapshot, KScreenPosition,
    KScreenSnapshot, ModePolicy, Rational, RecoveryBaseline, Signal, Snapshot, SourceMode,
};

fn snapshot() -> Snapshot {
    Snapshot {
        format_version: 1,
        paths: vec![DisplayPath {
            id: "monitor.one".into(),
            source_name: "DISPLAY1".into(),
            source_address: Address {
                adapter_low: 1,
                adapter_high: 0,
                id: 0,
            },
            target_address: Address {
                adapter_low: 1,
                adapter_high: 0,
                id: 1,
            },
            source: SourceMode {
                width: 1920,
                height: 1080,
                pixel_format: 4,
                x: 0,
                y: 0,
            },
            signal: Signal {
                pixel_rate: 148_500_000,
                horizontal_sync: Rational {
                    numerator: 67_500,
                    denominator: 1,
                },
                vertical_sync: Rational {
                    numerator: 60,
                    denominator: 1,
                },
                active_width: 1920,
                active_height: 1080,
                total_width: 2200,
                total_height: 1125,
                video_standard: 0,
                scan_line_ordering: 1,
            },
            rotation: 1,
            scaling: 1,
            refresh: Rational {
                numerator: 60,
                denominator: 1,
            },
            scan_line_ordering: 1,
            output_technology: 5,
            source_flags: 0,
            target_flags: 0,
            path_flags: 1,
            hdr_enabled: Some(false),
        }],
    }
}
fn kscreen_snapshot() -> KScreenSnapshot {
    KScreenSnapshot {
        version: 1,
        outputs: vec![
            KScreenOutputSnapshot {
                id: "DP-1".into(),
                connected: true,
                enabled: true,
                mode: Some(KScreenModeSnapshot {
                    id: "25".into(),
                    width: 2560,
                    height: 1440,
                    refresh_millihz: 60_000,
                }),
                position: KScreenPosition { x: 0, y: 0 },
                scale: 1.5,
                rotation: 1,
                priority: 1,
                primary: true,
                hdr_enabled: Some(true),
            },
            KScreenOutputSnapshot {
                id: "DP-2".into(),
                connected: true,
                enabled: true,
                mode: Some(KScreenModeSnapshot {
                    id: "31".into(),
                    width: 1920,
                    height: 1080,
                    refresh_millihz: 60_000,
                }),
                position: KScreenPosition { x: 2560, y: 0 },
                scale: 1.0,
                rotation: 2,
                priority: 2,
                primary: false,
                hdr_enabled: Some(false),
            },
        ],
    }
}
fn kscreen_configuration() -> Value {
    json!({
        "displays": [
            {
                "id": "DP-1", "width": 1280, "height": 720,
                "refresh": {"numerator": 60, "denominator": 1},
                "x": 0, "y": 0
            },
            {
                "id": "DP-2", "width": 1920, "height": 1080,
                "refresh": {"numerator": 60, "denominator": 1},
                "x": 1280, "y": 0, "rotation": "rotate90", "hdr_enabled": false
            }
        ]
    })
}
fn two_display_snapshot() -> Snapshot {
    let mut before = snapshot();
    let mut second = before.paths[0].clone();
    second.id = "monitor.two".into();
    second.source_name = "DISPLAY2".into();
    second.source_address.id = 2;
    second.target_address.id = 3;
    second.source.x = 1920;
    before.paths.push(second);
    before
}
fn cloned_snapshot() -> Snapshot {
    let mut before = two_display_snapshot();
    before.paths[1].source_name = before.paths[0].source_name.clone();
    before.paths[1].source_address = before.paths[0].source_address.clone();
    before.paths[1].source = before.paths[0].source.clone();
    before
}

#[test]
fn revision_tracks_clone_path_priority_without_tracking_independent_path_order() {
    let baseline = cloned_snapshot();
    let initial = baseline.revision().unwrap();
    let mut swapped_clone = baseline.clone();
    swapped_clone.paths.swap(0, 1);
    assert_ne!(initial, swapped_clone.revision().unwrap());

    let mut with_independent = baseline;
    let mut third = two_display_snapshot().paths.remove(1);
    third.id = "monitor.three".into();
    third.source_address.id = 4;
    third.target_address.id = 5;
    with_independent.paths.push(third);
    let before = with_independent.revision().unwrap();
    let mut independent_swapped = with_independent.clone();
    independent_swapped.paths.swap(0, 2);
    assert_ne!(before, independent_swapped.revision().unwrap());
    let mut independent_only = two_display_snapshot();
    let independent_revision = independent_only.revision().unwrap();
    independent_only.paths.swap(0, 1);
    assert_eq!(independent_revision, independent_only.revision().unwrap());

    let mut two_independent_before_clone = with_independent.clone();
    let mut fourth = two_display_snapshot().paths.remove(1);
    fourth.id = "monitor.four".into();
    fourth.source_address.id = 6;
    fourth.target_address.id = 7;
    fourth.source.x = 3840;
    two_independent_before_clone.paths.insert(0, fourth);
    two_independent_before_clone.paths.rotate_right(1);
    let stable = two_independent_before_clone.revision().unwrap();
    two_independent_before_clone.paths.swap(0, 1);
    assert_eq!(stable, two_independent_before_clone.revision().unwrap());
}

#[test]
fn stale_clone_priority_revision_rejects_before_journal_and_native_effect() {
    let baseline = cloned_snapshot();
    let old_revision = json!(baseline.revision().unwrap());
    let mut changed = baseline.clone();
    changed.paths.swap(0, 1);
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(changed.clone()),
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let mut selected = configuration();
    selected["displays"][0]["id"] = json!("monitor.two");
    let rejected = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request("ensure_primary", "monitor.two", selected, &old_revision),
    );
    assert_eq!(rejected["error"]["code"], "topology_changed");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
    assert!(pipeline.platform.prepared_configuration.is_none());

    let rejected_plain = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"apply", "configuration":changed.configuration().unwrap(),
            "expected_topology_revision":old_revision}),
    );
    assert_eq!(rejected_plain["error"]["code"], "topology_changed");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());

    let independent = two_display_snapshot();
    let revision = json!(independent.revision().unwrap());
    let mut reordered = independent.clone();
    reordered.paths.swap(0, 1);
    let mut independent_pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(reordered.clone()),
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let accepted = state.run(
        &mut independent_pipeline,
        "owner",
        json!({"action":"apply", "configuration":reordered.configuration().unwrap(),
            "expected_topology_revision":revision}),
    );
    assert_eq!(accepted["applied"], true, "{accepted}");
    let restored = state.run(
        &mut independent_pipeline,
        "owner",
        json!({"action":"restore","lease":accepted["lease"]}),
    );
    assert_eq!(restored["restored"], true);
}

#[test]
fn clone_configuration_preserves_target_state_and_rejects_false_clone() {
    let mut baseline = cloned_snapshot();
    baseline.paths[1].refresh = Rational {
        numerator: 75,
        denominator: 1,
    };
    baseline.paths[1].hdr_enabled = Some(true);
    let configuration = baseline.configuration().unwrap();
    assert_eq!(configuration.displays[0].x, configuration.displays[1].x);
    assert_eq!(configuration.displays[1].refresh.numerator, 75);
    assert_eq!(configuration.displays[1].hdr_enabled, Some(true));
    let mut rebound = baseline.clone();
    for path in &mut rebound.paths {
        path.source_address.adapter_low = 99;
        path.target_address.adapter_low = 99;
    }
    assert!(baseline.equivalent(&rebound));
    rebound.paths[1].source_address.id += 1;
    assert!(!baseline.equivalent(&rebound));
    assert_eq!(rebound.validate().unwrap_err().code, "unsupported_topology");
}

#[test]
fn selected_clone_mode_policy_updates_shared_source_and_restores_for_either_target() {
    for selected in ["monitor.one", "monitor.two"] {
        let baseline = cloned_snapshot();
        let mut state = State::default();
        let mut pipeline = Pipeline::new(
            FakePlatform {
                current_snapshot: Some(baseline.clone()),
                hdr_supported: true,
                ..Default::default()
            },
            true,
        );
        let mut requested = configuration();
        requested["displays"][0]["id"] = selected.into();
        let mut request = mutating_option_request(
            "ensure_active",
            selected,
            requested,
            &json!(baseline.revision().unwrap()),
        );
        request["mode_policy"]["remapping"] =
            policy(selected, "1600x900", "75")["remapping"].clone();
        request["mode_policy"]["context"]["optimize_game_settings"] = true.into();
        let result = state.run(&mut pipeline, "owner", request);
        assert_eq!(result["applied"], true, "{selected}: {result}");
        assert_eq!(pipeline.platform.prepared_modes.len(), 2);
        assert!(
            pipeline
                .platform
                .prepared_modes
                .iter()
                .all(|(_, width, height, _)| *width == 1600 && *height == 900)
        );
        let restored = state.run(
            &mut pipeline,
            "owner",
            json!({"action":"restore","lease":result["lease"]}),
        );
        assert_eq!(restored["restored"], true);
        assert!(state.journal.is_none());
    }
}

#[test]
fn ensure_primary_on_active_clone_translates_shared_source_and_recovers_exact_baseline() {
    let mut baseline = cloned_snapshot();
    baseline.paths[0].source.x = 1920;
    baseline.paths[1].source.x = 1920;
    baseline.paths[1].refresh.numerator = 75;
    let mut third = two_display_snapshot().paths.remove(1);
    third.id = "monitor.three".into();
    third.source.x = 0;
    third.source_address.id = 4;
    third.target_address.id = 5;
    baseline.paths.push(third);
    let revision = json!(baseline.revision().unwrap());
    let mut selected = configuration();
    selected["displays"][0]["id"] = json!("monitor.two");
    let mut request = mutating_option_request("ensure_primary", "monitor.two", selected, &revision);
    request["mode_policy"]["remapping"] =
        policy("monitor.two", "1600x900", "75")["remapping"].clone();
    request["mode_policy"]["context"]["optimize_game_settings"] = true.into();
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let applied = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(applied["applied"], true, "{applied}");
    let prepared = pipeline.platform.prepared_configuration.as_ref().unwrap();
    assert_eq!(prepared.displays.len(), 3);
    assert_eq!(prepared.displays[0].x, 0);
    assert_eq!(prepared.displays[1].x, 0);
    assert_eq!(prepared.displays[2].x, -1920);
    assert_eq!(
        (prepared.displays[0].width, prepared.displays[0].height),
        (1600, 900)
    );
    assert_eq!(
        (prepared.displays[1].width, prepared.displays[1].height),
        (1600, 900)
    );
    assert_eq!(prepared.displays[1].refresh.numerator, 75);
    assert_eq!(
        pipeline.platform.preserving_selected,
        [
            "prepare:monitor.two:PrimaryActive",
            "apply:monitor.two:PrimaryActive"
        ]
    );
    let denied = state.run(
        &mut pipeline,
        "foreign",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(denied["error"]["code"], "permission_denied");
    assert_eq!(pipeline.platform.effects, ["apply"]);
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert_eq!(pipeline.platform.effects, ["apply", "restore"]);
    assert!(state.journal.is_none());

    let mut stale = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            ..Default::default()
        },
        true,
    );
    let mut old_request = request;
    old_request["expected_topology_revision"] = json!("00000000000000000000000000000000");
    let rejected = state.run(&mut stale, "owner", old_request);
    assert_eq!(rejected["error"]["code"], "topology_changed");
    assert!(stale.platform.effects.is_empty());
}

#[test]
fn active_clone_primary_failed_restore_retains_owner_recovery_lease() {
    let baseline = cloned_snapshot();
    let mut selected = configuration();
    selected["displays"][0]["id"] = json!("monitor.two");
    let request = mutating_option_request(
        "ensure_primary",
        "monitor.two",
        selected,
        &json!(baseline.revision().unwrap()),
    );
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline),
            hdr_supported: true,
            restore_failure: true,
            ..Default::default()
        },
        true,
    );
    let applied = state.run(&mut pipeline, "owner", request);
    assert_eq!(applied["applied"], true, "{applied}");
    let failed = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(failed["recovery_required"], true);
    assert!(state.journal.is_some());
    let foreign = state.run(
        &mut pipeline,
        "foreign",
        json!({"action":"recover","lease":applied["lease"]}),
    );
    assert_eq!(foreign["error"]["code"], "permission_denied");
    pipeline.platform.restore_failure = false;
    let recovered = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"recover","lease":applied["lease"]}),
    );
    assert_eq!(recovered["restored"], true);
    assert_eq!(pipeline.platform.effects, ["apply", "restore", "restore"]);
    assert!(state.journal.is_none());
}

#[test]
fn conflicting_clone_mode_policies_fail_before_journal_or_native_effect() {
    let baseline = cloned_snapshot();
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            ..Default::default()
        },
        true,
    );
    let policies: Vec<_> = [("monitor.one", "1280x720"), ("monitor.two", "1600x900")]
        .into_iter()
        .map(|(display_id, resolution)| {
            json!({
                "display_id":display_id,
                "remapping":{},
                "context":{"resolution_is_automatic":false,"refresh_rate_is_automatic":true,
                    "client_display_mode_override":false,"optimize_game_settings":true},
                "resolution_option":"manual","manual_resolution":resolution
            })
        })
        .collect();
    let reply = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action":"apply","configuration":baseline.configuration().unwrap(),
            "mode_policies":policies,
            "expected_topology_revision":baseline.revision().unwrap()
        }),
    );
    assert_eq!(reply["error"]["code"], "unsupported_topology");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
    assert!(pipeline.platform.prepared_configuration.is_none());
}

#[test]
fn inactive_clone_cas_binds_route_and_retry_restores_from_persisted_baseline() {
    let baseline = snapshot();
    let mut configuration = baseline.configuration().unwrap();
    let mut clone = configuration.displays[0].clone();
    clone.id = "monitor.two".into();
    clone.hdr_enabled = None;
    configuration.displays.push(clone);
    let revision = baseline.revision().unwrap();
    let request = json!({"action":"apply","configuration":configuration,
        "expected_topology_revision":revision,"operation_id":"clone-session"});
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let mut stale_revision = request.clone();
    stale_revision["expected_topology_revision"] = "00000000000000000000000000000000".into();
    let stale_result = state.run(&mut pipeline, "owner", stale_revision);
    assert_eq!(stale_result["error"]["code"], "topology_changed");
    let mut unrevisioned = request.clone();
    unrevisioned
        .as_object_mut()
        .unwrap()
        .remove("expected_topology_revision");
    let unrevisioned_result = state.run(&mut pipeline, "owner", unrevisioned);
    assert_eq!(unrevisioned_result["error"]["code"], "invalid_request");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
    let applied = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(applied["applied"], true, "{applied}");
    assert_eq!(
        pipeline.platform.preserving_selected,
        [
            "prepare:monitor.two:CloneActive",
            "apply:monitor.two:CloneActive"
        ]
    );
    assert_eq!(
        state.journal.as_ref().unwrap()["pending_route"]["id"],
        "monitor.two"
    );
    let retry = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(retry["applied"], false);
    assert_eq!(retry["lease"], applied["lease"]);
    let denied = state.run(
        &mut pipeline,
        "foreign",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(denied["error"]["code"], "permission_denied");
    let stale = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":"stale"}),
    );
    assert_eq!(stale["error"]["code"], "permission_denied");
    let mut restarted = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(cloned_snapshot()),
            ..Default::default()
        },
        true,
    );
    let recovery = state.run(&mut restarted, "owner", request);
    assert_eq!(recovery["state"], "recovery_required");
    assert_eq!(restarted.platform.effects.len(), 0);
    let restored = state.run(
        &mut restarted,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert!(state.journal.is_none());
    assert_eq!(restarted.platform.effects, ["restore"]);
}
fn configuration() -> Value {
    json!({"displays":[{"id":"monitor.one","width":1280,"height":720,"refresh":{"numerator":60,"denominator":1},"x":0,"y":0}]})
}
fn two_display_configuration() -> Value {
    json!({"displays":[
        {"id":"monitor.one","width":1280,"height":720,"refresh":{"numerator":60,"denominator":1},"x":0,"y":0},
        {"id":"monitor.two","width":1280,"height":720,"refresh":{"numerator":60,"denominator":1},"x":1920,"y":0}
    ]})
}
fn policy(display_id: &str, resolution: &str, refresh: &str) -> Value {
    json!({
        "display_id": display_id,
        "context": {
            "resolution_is_automatic": true,
            "refresh_rate_is_automatic": true,
            "client_display_mode_override": false,
            "optimize_game_settings": true
        },
        "remapping": {"mixed": [{
            "requested_resolution": "1280x720",
            "requested_fps": "60",
            "final_resolution": resolution,
            "final_refresh_rate": refresh
        }]}
    })
}
fn configuration_option_request(option: &str, display_id: &str, configuration: Value) -> Value {
    json!({"action":"apply","configuration":configuration,"mode_policy":{
        "display_id":display_id,
        "configuration_option":option,
        "remapping":{},
        "context":{"resolution_is_automatic":true,"refresh_rate_is_automatic":true,"client_display_mode_override":false,"optimize_game_settings":true},
        "resolution_option":"manual",
        "manual_resolution":"invalid ignored value"
    }})
}
fn mutating_option_request(
    option: &str,
    display_id: &str,
    configuration: Value,
    revision: &Value,
) -> Value {
    json!({"action":"apply","configuration":configuration,"expected_topology_revision":revision,"mode_policy":{
        "display_id":display_id,
        "configuration_option":option,
        "remapping":{},
        "context":{"resolution_is_automatic":true,"refresh_rate_is_automatic":true,"client_display_mode_override":false,"optimize_game_settings":false}
    }})
}

#[test]
fn public_single_policy_resolution_validates_resulting_configuration() {
    let mut concrete: Configuration = serde_json::from_value(configuration()).unwrap();
    let undersized: ModePolicy =
        serde_json::from_value(policy("monitor.one", "200x100", "120")).unwrap();
    let error = concrete.resolve_mode_policy(&undersized, None).unwrap_err();
    assert_eq!(error.code, "invalid_request");

    let mut invalid_input: Configuration = serde_json::from_value(configuration()).unwrap();
    invalid_input.displays[0].refresh.denominator = 0;
    let error = invalid_input
        .resolve_mode_policy(&undersized, None)
        .unwrap_err();
    assert_eq!(error.code, "invalid_request");
}
fn hdr_apply(option: &str, requested: Option<bool>) -> Value {
    json!({
        "action":"apply",
        "configuration":configuration(),
        "mode_policy":{
            "display_id":"monitor.one",
            "context":{
                "resolution_is_automatic":true,
                "refresh_rate_is_automatic":true,
                "client_display_mode_override":false,
                "optimize_game_settings":false
            },
            "remapping":{},
            "hdr_option":option,
            "effective_client_hdr_request":requested
        }
    })
}
#[derive(Default)]
struct FakePlatform {
    capture_target_queries: usize,
    capture_targets_unavailable: bool,
    drift_capture_target_after: Option<usize>,
    scoped_readback: bool,
    scoped_only: bool,
    current_snapshot: Option<Snapshot>,
    prepared_configuration: Option<Configuration>,
    preserving_selected: Vec<String>,
    inactive_route_ambiguous: bool,
    inactive_route_changed: bool,
    restoration_unavailable: bool,
    virtual_layout_unsupported: bool,
    effects: Vec<&'static str>,
    prepared_mode: Option<(u32, u32, Rational)>,
    applied_mode: Option<(u32, u32, Rational)>,
    prepared_modes: Vec<(String, u32, u32, Rational)>,
    applied_modes: Vec<(String, u32, u32, Rational)>,
    apply_failure: bool,
    late_mode_drift: bool,
    restore_failure: bool,
    topology_changed: bool,
    drift_on_prepare: bool,
    drift_on_apply: bool,
    available_modes: Vec<crate::model::AvailableMode>,
    second_available_modes: Vec<crate::model::AvailableMode>,
    mode_queries: usize,
    drift_after_mode_query: bool,
    lose_mode_after_first_query: bool,
    queried_displays: Vec<String>,
    inactive: bool,
    inactive_activation_hdr: bool,
    hdr_supported: bool,
    hdr_state: bool,
    hdr_mutations: usize,
    prepared_hdr: Option<Option<bool>>,
    reorder_readback: bool,
    kscreen_baseline: Option<KScreenSnapshot>,
    kscreen_capture_count: usize,
    kscreen_drift_on_second_capture: bool,
    kscreen_apply_readback_failure: bool,
    kscreen_restore_blocked: bool,
    kscreen_restore_failure: bool,
    kscreen_restore_readback_failure: bool,
    kscreen_restore_calls: usize,
}
impl Platform for FakePlatform {
    fn capture_targets(&mut self) -> DisplayResult<Vec<crate::targets::NativeTarget>> {
        if self.capture_targets_unavailable {
            return Err(DisplayError::new(
                "capture_target_unavailable",
                "source observation unavailable",
            ));
        }
        self.capture_target_queries += 1;
        let mut snapshot = self.current_snapshot.clone().unwrap_or_else(snapshot);
        if self
            .drift_capture_target_after
            .is_some_and(|after| self.capture_target_queries >= after)
        {
            snapshot.paths[0].refresh.numerator = 75;
        }
        snapshot
            .configuration()?
            .displays
            .into_iter()
            .map(|setting| {
                let output = format!("exact-test-source:{}", setting.id);
                crate::targets::NativeTarget::observed(self.name(), setting, "Living room", output)
            })
            .collect()
    }
    fn name(&self) -> &'static str {
        "fake_platform_test_only"
    }
    fn supported(&self) -> bool {
        !self.scoped_only
    }
    fn supports_scoped_composition(&self) -> bool {
        true
    }
    fn read_configuration_snapshot(&mut self) -> DisplayResult<Value> {
        if self.scoped_readback
            && let Some(baseline) = &self.kscreen_baseline
        {
            let displays=baseline.outputs.iter().filter(|output|output.connected && output.enabled).map(|output| {
                let mode=output.mode.as_ref().unwrap();
                json!({"id":output.id,"width":mode.width,"height":mode.height,"x":output.position.x,"y":output.position.y,
                    "refresh":{"numerator":mode.refresh_millihz,"denominator":1000},
                    "rotation":match output.rotation {1=>"identity",2=>"rotate90",4=>"rotate180",8=>"rotate270",_=>panic!("invalid rotation")},
                    "hdr_enabled":output.hdr_enabled})
            }).collect::<Vec<_>>();
            return Ok(
                json!({"configuration":{"displays":displays},"topology_revision":baseline.revision()?}),
            );
        }
        let snapshot = self.snapshot()?;
        Ok(
            json!({"configuration":snapshot.configuration()?,"topology_revision":snapshot.revision()?}),
        )
    }
    fn capture_kscreen_snapshot(&mut self) -> DisplayResult<Option<KScreenSnapshot>> {
        self.kscreen_capture_count += 1;
        let Some(mut baseline) = self.kscreen_baseline.clone() else {
            return Ok(None);
        };
        if self.kscreen_drift_on_second_capture && self.kscreen_capture_count == 2 {
            baseline.outputs[0].scale = 1.75;
        }
        Ok(Some(baseline))
    }
    fn prepare_kscreen_snapshot(
        &mut self,
        configuration: &Configuration,
        baseline: &KScreenSnapshot,
    ) -> DisplayResult<()> {
        baseline.validate_requested_topology(configuration)
    }
    fn apply_kscreen_snapshot(
        &mut self,
        configuration: &Configuration,
        baseline: &KScreenSnapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        baseline.validate()?;
        if self.scoped_readback {
            configuration.validate_kscreen(baseline)?;
        } else {
            configuration.validate()?;
        }
        guard()?;
        let mut readback = baseline.clone();
        for display in &configuration.displays {
            let output = readback
                .outputs
                .iter_mut()
                .find(|output| output.id.eq_ignore_ascii_case(&display.id))
                .ok_or_else(|| DisplayError::new("unsupported_display", "missing output"))?;
            output.position.x = display.x;
            output.position.y = display.y;
            output.rotation = match display.rotation {
                crate::model::Rotation::Identity => 1,
                crate::model::Rotation::Rotate90 => 2,
                crate::model::Rotation::Rotate180 => 4,
                crate::model::Rotation::Rotate270 => 8,
            };
            if let Some(hdr) = display.hdr_enabled {
                output.hdr_enabled = Some(hdr);
            }
            if !self.scoped_readback
                || output.mode.as_ref().is_none_or(|mode| {
                    mode.width != display.width
                        || mode.height != display.height
                        || u64::from(mode.refresh_millihz) * u64::from(display.refresh.denominator)
                            != 1000 * u64::from(display.refresh.numerator)
                })
            {
                output.mode = Some(KScreenModeSnapshot {
                    id: format!("{}x{}@60", display.width, display.height),
                    width: display.width,
                    height: display.height,
                    refresh_millihz: 60_000,
                });
            }
        }
        self.kscreen_baseline = Some(readback);
        if self.kscreen_apply_readback_failure {
            self.kscreen_baseline.as_mut().unwrap().outputs[0]
                .position
                .x = 1;
        }
        self.effects.push("apply_kscreen");
        if self.apply_failure {
            return Err(DisplayError::new(
                "platform_error",
                "partial KScreen apply in fake platform",
            )
            .after_mutation());
        }
        Ok(())
    }
    fn restore_kscreen_snapshot(
        &mut self,
        baseline: &KScreenSnapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        baseline.validate()?;
        self.kscreen_restore_calls += 1;
        if self.kscreen_restore_blocked {
            return Err(DisplayError::new(
                "topology_changed",
                "fake hotplug reconciliation held restoration",
            ));
        }
        guard()?;
        self.kscreen_baseline = Some(baseline.clone());
        if self.kscreen_restore_readback_failure {
            self.kscreen_baseline.as_mut().unwrap().outputs[0].scale = 1.75;
        }
        self.effects.push("restore_kscreen");
        if self.kscreen_restore_failure {
            return Err(DisplayError::new(
                "verification_failed",
                "fake KScreen restoration did not verify",
            )
            .after_mutation());
        }
        Ok(())
    }
    fn bind_inactive(&mut self, display: &str, before: &Snapshot) -> DisplayResult<RouteBinding> {
        if self.inactive_route_ambiguous {
            return Err(DisplayError::new(
                "unsupported_topology",
                "ambiguous inactive route",
            ));
        }
        if before
            .paths
            .iter()
            .any(|path| path.id.eq_ignore_ascii_case(display))
        {
            return Err(DisplayError::new("unsupported_display", "target is active"));
        }
        Ok(RouteBinding {
            id: display.into(),
            source_name: "DISPLAY-INACTIVE".into(),
            source_address: Address {
                adapter_low: 1,
                adapter_high: 0,
                id: 42,
            },
            target_address: Address {
                adapter_low: 1,
                adapter_high: 0,
                id: 43,
            },
            output_technology: 5,
        })
    }
    fn bind_clone(
        &mut self,
        display: &str,
        source: &Address,
        before: &Snapshot,
    ) -> DisplayResult<RouteBinding> {
        if self.inactive_route_ambiguous {
            return Err(DisplayError::new(
                "unsupported_topology",
                "ambiguous clone route",
            ));
        }
        let source_name = before
            .paths
            .iter()
            .find(|path| path.source_address == *source)
            .ok_or_else(|| DisplayError::new("unsupported_topology", "missing clone source"))?
            .source_name
            .clone();
        Ok(RouteBinding {
            id: display.into(),
            source_name,
            source_address: source.clone(),
            target_address: Address {
                adapter_low: source.adapter_low,
                adapter_high: source.adapter_high,
                id: 43,
            },
            output_technology: 5,
        })
    }
    fn prepare_scoped_kscreen_snapshot(
        &mut self,
        configuration: &Configuration,
        baseline: &KScreenSnapshot,
        primary: Option<&str>,
    ) -> DisplayResult<()> {
        let expected = if let Some(target) = primary {
            baseline.scoped_primary_baseline(target)?
        } else {
            baseline.clone()
        };
        expected.validate_requested_topology(configuration)
    }
    fn apply_scoped_kscreen_snapshot(
        &mut self,
        configuration: &Configuration,
        baseline: &KScreenSnapshot,
        primary: Option<&str>,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        let expected = if let Some(target) = primary {
            baseline.scoped_primary_baseline(target)?
        } else {
            baseline.clone()
        };
        self.apply_kscreen_snapshot(configuration, baseline, guard)?;
        if primary.is_some() {
            for output in &mut self.kscreen_baseline.as_mut().unwrap().outputs {
                let metadata = expected
                    .outputs
                    .iter()
                    .find(|candidate| candidate.id == output.id)
                    .unwrap();
                output.priority = metadata.priority;
                output.primary = metadata.primary;
            }
        }
        if !self
            .kscreen_baseline
            .as_ref()
            .unwrap()
            .matches_configuration(&expected, configuration)
        {
            return Err(
                DisplayError::new("verification_failed", "scoped fake readback mismatch")
                    .after_mutation(),
            );
        }
        Ok(())
    }
    fn snapshot(&mut self) -> DisplayResult<Snapshot> {
        if self.capture_targets_unavailable {
            return Err(DisplayError::new(
                "capture_target_unavailable",
                "source observation unavailable",
            ));
        }
        if let Some(current) = &self.current_snapshot {
            return Ok(current.clone());
        }
        let mut current = if self.reorder_readback {
            two_display_snapshot()
        } else {
            snapshot()
        };
        if self.inactive {
            current.paths[0].id = "monitor.other".into();
        }
        if self.topology_changed {
            current.paths[0].source.width += 1;
        }
        Ok(current)
    }
    fn enumerate(&mut self, _: Option<&str>, _: usize, _: usize) -> DisplayResult<Value> {
        Ok(json!({"ok":true,"entries":[]}))
    }
    fn available_modes(
        &mut self,
        display: &str,
    ) -> DisplayResult<Vec<crate::model::AvailableMode>> {
        self.mode_queries += 1;
        self.queried_displays.push(display.to_lowercase());
        if self.drift_after_mode_query && self.mode_queries == 1 {
            let mut changed = self.current_snapshot.clone().unwrap_or_else(snapshot);
            changed.paths[0].target_address.id += 1;
            self.current_snapshot = Some(changed);
        }
        if self.inactive {
            return Err(DisplayError::new(
                "unsupported_display",
                "display is inactive",
            ));
        }
        if display.eq_ignore_ascii_case("monitor.one") {
            if self.lose_mode_after_first_query && self.mode_queries > 1 {
                Ok(Vec::new())
            } else {
                Ok(self.available_modes.clone())
            }
        } else if display.eq_ignore_ascii_case("monitor.two") {
            Ok(self.second_available_modes.clone())
        } else {
            Err(DisplayError::new(
                "unsupported_display",
                "display is inactive",
            ))
        }
    }
    fn prepare(&mut self, configuration: &Configuration) -> DisplayResult<Snapshot> {
        if self.virtual_layout_unsupported {
            return Err(DisplayError::new(
                "unsupported_topology",
                "virtual CCD layout needs desktop-image recovery",
            ));
        }
        self.prepared_configuration = Some(configuration.clone());
        let display = &configuration.displays[0];
        self.prepared_modes = configuration
            .displays
            .iter()
            .map(|display| {
                (
                    display.id.clone(),
                    display.width,
                    display.height,
                    display.refresh,
                )
            })
            .collect();
        if display.hdr_enabled == Some(true) && (!self.hdr_supported || self.inactive) {
            return Err(DisplayError::new("unsupported_display", "HDR unavailable"));
        }
        self.prepared_hdr = Some(display.hdr_enabled);
        self.prepared_mode = Some((display.width, display.height, display.refresh));
        let mut before = if let Some(current) = &self.current_snapshot {
            current.clone()
        } else if self.reorder_readback || configuration.displays.len() == 2 {
            two_display_snapshot()
        } else {
            snapshot()
        };
        if self.inactive {
            before.paths[0].id = "monitor.other".into();
        }
        if self.topology_changed {
            before.paths[0].source.width += 1;
        }
        if self.drift_on_prepare {
            before.paths[0].source.width += 1;
        }
        before.paths[0].hdr_enabled = self.hdr_supported.then_some(self.hdr_state);
        self.current_snapshot = Some(before.clone());
        Ok(before)
    }
    fn prepare_preserving(
        &mut self,
        configuration: &Configuration,
        selected: &str,
        route: Option<&RouteBinding>,
        shape: TopologyShape,
    ) -> DisplayResult<Snapshot> {
        if route.is_some_and(|route| !route.id.eq_ignore_ascii_case(selected)) {
            return Err(DisplayError::new("topology_changed", "route mismatch"));
        }
        if shape == TopologyShape::OnlySelected && self.restoration_unavailable {
            return Err(DisplayError::new(
                "unsupported_topology",
                "saved baseline cannot be rebound for recovery",
            ));
        }
        self.preserving_selected
            .push(format!("prepare:{selected}:{shape:?}"));
        self.prepare(configuration)
    }
    fn apply(
        &mut self,
        configuration: &Configuration,
        before: &Snapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        if self.reorder_readback {
            let mut readback = before.clone();
            readback.paths.reverse();
            if !before.equivalent(&readback) {
                return Err(DisplayError::new("topology_changed", "reordered readback"));
            }
        }
        assert_eq!(before.paths[0].hdr_enabled.unwrap_or(false), self.hdr_state);
        if self.topology_changed || self.drift_on_apply {
            return Err(DisplayError::new("topology_changed", "external change"));
        }
        guard()?;
        let display = &configuration.displays[0];
        let was_active = before
            .paths
            .iter()
            .any(|path| path.id.eq_ignore_ascii_case(&display.id));
        if self.inactive {
            self.inactive = false;
            self.hdr_state = self.inactive_activation_hdr;
        }
        self.applied_mode = Some((display.width, display.height, display.refresh));
        self.applied_modes = configuration
            .displays
            .iter()
            .map(|display| {
                (
                    display.id.clone(),
                    display.width,
                    display.height,
                    display.refresh,
                )
            })
            .collect();
        if let Some(enabled) = display
            .hdr_enabled
            .filter(|_| self.hdr_supported && was_active)
        {
            self.hdr_state = enabled;
            self.hdr_mutations += 1;
        }
        self.effects.push("apply");
        if self.scoped_readback {
            let mut readback = before.clone();
            readback.paths.clear();
            for (index, requested) in configuration.displays.iter().enumerate() {
                let mut path = before
                    .paths
                    .iter()
                    .find(|path| path.id == requested.id)
                    .cloned()
                    .unwrap_or_else(|| {
                        let mut path = before.paths[0].clone();
                        path.id = requested.id.clone();
                        path.source_name = format!("DISPLAY{}", index + 1);
                        path.source_address.id = index as u32 + 20;
                        path.target_address.id = index as u32 + 40;
                        path
                    });
                path.source.width = requested.width;
                path.source.height = requested.height;
                path.source.x = requested.x;
                path.source.y = requested.y;
                path.refresh = requested.refresh;
                path.signal.active_width = requested.width;
                path.signal.active_height = requested.height;
                path.signal.vertical_sync = requested.refresh;
                path.hdr_enabled = requested.hdr_enabled.or(path.hdr_enabled);
                readback.paths.push(path);
            }
            self.current_snapshot = Some(readback);
        }
        if display.hdr_enabled == Some(false) && !was_active && self.hdr_state {
            return Err(
                DisplayError::new("verification_failed", "new target enabled HDR").after_mutation(),
            );
        }
        if self.apply_failure {
            return Err(
                DisplayError::new("platform_error", "partial native failure").after_mutation(),
            );
        }
        if self.late_mode_drift {
            self.applied_mode = Some((1024, 768, display.refresh));
            return Err(DisplayError::new(
                "verification_failed",
                "HDR changed the requested mode after the first readback",
            )
            .after_mutation());
        }
        Ok(())
    }
    fn apply_preserving(
        &mut self,
        configuration: &Configuration,
        before: &Snapshot,
        selected: &str,
        route: Option<&RouteBinding>,
        shape: TopologyShape,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        if route.is_some() && self.inactive_route_changed {
            return Err(DisplayError::new(
                "topology_changed",
                "inactive route changed after journal",
            ));
        }
        self.preserving_selected
            .push(format!("apply:{selected}:{shape:?}"));
        self.apply(configuration, before, guard)
    }
    fn restore(
        &mut self,
        before: &Snapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        if self.inactive
            && before
                .paths
                .iter()
                .any(|path| path.hdr_enabled.is_none() && path.id == "monitor.one")
        {
            return Err(DisplayError::new(
                "recovery_blocked",
                "old journal target inactive",
            ));
        }
        if before.legacy_hdr_target_conflicts(
            "monitor.one",
            self.hdr_supported.then_some(self.hdr_state),
        ) {
            return Err(DisplayError::new(
                "recovery_blocked",
                "external HDR state changed",
            ));
        }
        guard()?;
        self.effects.push("restore");
        if self.restore_failure {
            return Err(DisplayError::new("platform_error", "restore failed").after_mutation());
        }
        if self.reorder_readback {
            let mut readback = before.clone();
            readback.paths.reverse();
            if !before.equivalent(&readback) {
                return Err(
                    DisplayError::new("verification_failed", "reordered restore").after_mutation(),
                );
            }
        }
        if let Some(enabled) = before.paths[0].hdr_enabled {
            self.hdr_state = enabled;
        }
        self.applied_mode = Some((
            before.paths[0].source.width,
            before.paths[0].source.height,
            before.paths[0].refresh,
        ));
        self.applied_modes = before
            .paths
            .iter()
            .map(|path| {
                (
                    path.id.clone(),
                    path.source.width,
                    path.source.height,
                    path.refresh,
                )
            })
            .collect();
        self.inactive = before.paths[0].id != "monitor.one";
        if self.scoped_readback {
            self.current_snapshot = Some(before.clone());
        }
        Ok(())
    }
}

#[test]
fn remapped_mode_survives_reordered_readback_and_restores() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            reorder_readback: true,
            ..Default::default()
        },
        true,
    );
    let applied = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action":"apply", "configuration":configuration(),
            "mode_policy":{
                "display_id":"monitor.one",
                "context":{
                    "resolution_is_automatic":true,
                    "refresh_rate_is_automatic":true,
                    "client_display_mode_override":false,
                    "optimize_game_settings":true
                },
                "remapping":{"mixed":[{
                    "requested_resolution":"1280x720",
                    "requested_fps":"60",
                    "final_resolution":"1920x1080",
                    "final_refresh_rate":"120"
                }]}
            }
        }),
    );
    assert_eq!(applied["applied"], true);
    assert_eq!(
        pipeline.platform.prepared_mode,
        pipeline.platform.applied_mode
    );
    assert_eq!(pipeline.platform.prepared_mode.unwrap().0, 1920);
    assert_eq!(pipeline.platform.prepared_mode.unwrap().2.numerator, 120);
    assert!(state.journal.is_some());

    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert_eq!(pipeline.platform.effects, ["apply", "restore"]);
    assert_eq!(
        pipeline.platform.applied_mode,
        Some((
            1920,
            1080,
            Rational {
                numerator: 60,
                denominator: 1
            }
        ))
    );
    assert!(state.journal.is_none());
}

#[test]
fn reordered_readback_still_detects_a_changed_refresh_rate() {
    let before = two_display_snapshot();
    let mut readback = before.clone();
    readback.paths.reverse();
    readback.paths[0].refresh.numerator = 75;
    assert!(!before.equivalent(&readback));
}

#[test]
fn reordered_readback_rejects_duplicate_identity() {
    let before = two_display_snapshot();
    let mut readback = before.clone();
    readback.paths[1] = readback.paths[0].clone();
    assert!(!before.equivalent(&readback));
}

#[test]
fn exact_restore_rejects_changed_path_flags_and_output_technology() {
    let baseline = two_display_snapshot();
    for change in 0..2 {
        let mut readback = baseline.clone();
        match change {
            0 => readback.paths[1].path_flags ^= 2,
            _ => readback.paths[1].output_technology += 1,
        }
        assert!(!baseline.equivalent(&readback));
    }
}

#[derive(Default)]
struct State {
    scoped_verifier_response: Option<Value>,
    lose_scoped_commit: bool,
    revision: u64,
    journal: Option<Value>,
    fail_save: bool,
    fail_clear: bool,
    cancel_ack: bool,
    deny_guard: bool,
    lose_set_acceptance: bool,
    lose_apply_completion: bool,
}
impl State {
    fn run(
        &mut self,
        pipeline: &mut Pipeline<FakePlatform>,
        caller: &str,
        payload: Value,
    ) -> Value {
        let native_mutation = !matches!(
            payload["action"].as_str(),
            Some("borrow_target" | "release_target")
        );
        let mut input = Some(payload);
        let mut state_reply = None;
        let mut result = None;
        for _ in 0..16 {
            pipeline.tick(|command| {
                assert!(serde_json::to_vec(&command).unwrap().len() < 16_384);
                Ok(match command {
                    Command::Poll {} => input.take().map_or(Response::Pending, |payload| Response::Request { id:7, caller:caller.into(), initiator:"original.initiator".into(), service:SERVICE.into(), payload, remaining_ms:30_000 }),
                    Command::Request { service, payload, parent, .. } => {
                        assert_eq!(parent, Some(7));
                        if service == amaterasu_module_sdk::virtual_display::SERVICE {
                            assert_eq!(payload["action"],"verify_owner_target");
                            assert_eq!(payload["owner"],caller);
                            state_reply = Some(self.scoped_verifier_response.clone().unwrap_or_else(|| json!({"ok":true,"owned":true,"target_id":"monitor.remote","specification":{"identity":payload["identity"],"name":"Remote Monitor","hdr_enabled":false,"width":1280,"height":720,"refresh_millihz":60000},"active":true})));
                            return Ok(Response::Accepted {id:20});
                        }
                        assert_eq!(service,STATE_SERVICE);
                        assert_eq!(payload["key"], JOURNAL_KEY);
                        state_reply = Some(match payload["action"].as_str().unwrap() {
                            "get" => json!({"ok":true,"revision":self.revision,"found":self.journal.is_some(),"value":self.journal}),
                            "set" if self.fail_save => failure("storage_error", "injected failure before durable snapshot"),
                            "set" => {
                                assert_eq!(payload["expected_revision"], self.revision);
                                self.journal = Some(payload["value"].clone());
                                self.revision += 1;
                                json!({"ok":true,"revision":self.revision})
                            }
                            "delete" if self.fail_clear => failure("storage_error", "injected journal clear failure"),
                            "delete" => {
                                assert_eq!(payload["expected_revision"], self.revision);
                                self.journal = None;
                                self.revision += 1;
                                json!({"ok":true,"revision":self.revision})
                            }
                            action => panic!("unexpected state action {action}"),
                        });
                        if self.lose_scoped_commit && payload["action"] == "set" && payload["value"].get("scoped_pending").is_none() {
                            self.lose_scoped_commit = false;
                            Response::error("transport_error", "scoped commit accepted but reply lost")
                        } else if self.lose_set_acceptance && payload["action"] == "set" {
                            self.lose_set_acceptance = false;
                            Response::error("transport_error", "set accepted but reply lost")
                        } else {
                            Response::Accepted { id:20 }
                        }
                    }
                    Command::TakeResult { .. } => {
                        if self.cancel_ack && self.journal.is_some() { Response::error("cancelled", "parent cancelled before acknowledgement") }
                        else { Response::Result { payload:state_reply.take().unwrap() } }
                    }
                    Command::CheckRequest { request } => {
                        assert_eq!(request, 7);
                        assert!(!native_mutation || self.journal.is_some(), "native effect requires durable journal");
                        if self.deny_guard { Response::error("request_ended", "cancelled at native boundary") } else { Response::Ok }
                    }
                    Command::Complete { payload, .. } => {
                        if self.lose_apply_completion && payload["applied"] == true {
                            self.lose_apply_completion = false;
                            result = Some(json!({"reply_lost":true}));
                            Response::error("request_ended", "apply reply lost after native effect")
                        } else {
                            result = Some(payload);
                            Response::Ok
                        }
                    }
                    command => panic!("unexpected command: {command:?}"),
                })
            }).unwrap();
            if let Some(result) = result {
                return result;
            }
        }
        panic!("display pipeline stalled")
    }
}
#[test]
fn snapshot_returns_complete_multi_monitor_configuration_and_stable_revision() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            reorder_readback: true,
            ..Default::default()
        },
        true,
    );
    let first = state.run(&mut pipeline, "owner", json!({"action":"snapshot"}));
    let second = state.run(&mut pipeline, "owner", json!({"action":"snapshot"}));
    assert_eq!(
        first["configuration"]["displays"].as_array().unwrap().len(),
        2
    );
    assert_eq!(first["topology_revision"], second["topology_revision"]);
    assert!(state.journal.is_none());
    let mut reordered = two_display_snapshot();
    let before = reordered.revision().unwrap();
    reordered.paths.reverse();
    assert_eq!(before, reordered.revision().unwrap());
    reordered.paths[0].source.width += 1;
    assert_ne!(before, reordered.revision().unwrap());
}

#[test]
fn disabled_configuration_option_has_no_state_or_native_effect_even_when_mutation_disabled() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), false);
    let reply = state.run(
        &mut pipeline,
        "owner",
        configuration_option_request("disabled", "monitor.one", configuration()),
    );
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["applied"], false);
    assert!(reply["lease"].is_null());
    assert_eq!(state.revision, 0);
    assert!(pipeline.platform.prepared_modes.is_empty());
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn verify_only_requires_active_target_without_journal_or_mode_resolution() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), false);
    let reply = state.run(
        &mut pipeline,
        "owner",
        configuration_option_request("verify_only", "monitor.one", configuration()),
    );
    assert_eq!(reply["verified"], true);
    assert_eq!(reply["applied"], false);
    assert!(reply["lease"].is_null());
    assert_eq!(state.revision, 0);
    assert!(pipeline.platform.prepared_modes.is_empty());
    let mut inactive = snapshot();
    inactive.paths[0].id = "monitor.other".into();
    pipeline.platform.current_snapshot = Some(inactive);
    let rejected = state.run(
        &mut pipeline,
        "owner",
        configuration_option_request("verify_only", "monitor.one", configuration()),
    );
    assert_eq!(rejected["error"]["code"], "unsupported_display");
    assert_eq!(state.revision, 0);
}

#[test]
fn mutating_configuration_options_require_revision_without_effect() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    for option in ["ensure_active", "ensure_primary", "ensure_only_display"] {
        let reply = state.run(
            &mut pipeline,
            "owner",
            configuration_option_request(option, "monitor.one", configuration()),
        );
        assert_eq!(reply["error"]["code"], "invalid_request");
    }
    assert_eq!(state.revision, 0);
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn ensure_active_uses_preserving_path_for_other_native_monitor() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            reorder_readback: true,
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let baseline = state.run(&mut pipeline, "owner", json!({"action":"snapshot"}));
    let applied = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            "ensure_active",
            "monitor.one",
            two_display_configuration(),
            &baseline["topology_revision"],
        ),
    );
    assert_eq!(applied["applied"], true, "{applied}");
    assert_eq!(
        pipeline.platform.preserving_selected,
        [
            "prepare:monitor.one:PreserveActive",
            "apply:monitor.one:PreserveActive"
        ]
    );
    let prepared = pipeline.platform.prepared_configuration.as_ref().unwrap();
    assert_eq!(prepared.displays.len(), 2);
    assert_eq!(prepared.displays[1].width, 1920);
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert_eq!(pipeline.platform.applied_modes.len(), 2);
}

#[test]
fn ensure_primary_translates_two_monitor_topology_through_preserving_path() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            reorder_readback: true,
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let baseline = state.run(&mut pipeline, "owner", json!({"action":"snapshot"}));
    let applied = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            "ensure_primary",
            "monitor.two",
            two_display_configuration(),
            &baseline["topology_revision"],
        ),
    );
    assert_eq!(applied["applied"], true, "{applied}");
    assert_eq!(
        pipeline.platform.preserving_selected,
        [
            "prepare:monitor.two:PrimaryActive",
            "apply:monitor.two:PrimaryActive"
        ]
    );
    let prepared = pipeline.platform.prepared_configuration.as_ref().unwrap();
    assert_eq!(prepared.displays[0].x, -1920);
    assert_eq!(prepared.displays[1].x, 0);
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
}

#[test]
fn preserving_primary_hotplug_and_partial_failure_keep_exact_recovery_rules() {
    for (drift, expected_effects) in [(true, vec![]), (false, vec!["apply", "restore"])] {
        let mut state = State::default();
        let mut pipeline = Pipeline::new(
            FakePlatform {
                reorder_readback: true,
                hdr_supported: true,
                drift_on_apply: drift,
                apply_failure: !drift,
                ..Default::default()
            },
            true,
        );
        let baseline = state.run(&mut pipeline, "owner", json!({"action":"snapshot"}));
        let result = state.run(
            &mut pipeline,
            "owner",
            mutating_option_request(
                "ensure_primary",
                "monitor.two",
                two_display_configuration(),
                &baseline["topology_revision"],
            ),
        );
        assert_eq!(
            result["error"]["code"],
            if drift {
                "topology_changed"
            } else {
                "platform_error"
            }
        );
        assert_eq!(pipeline.platform.effects, expected_effects);
        assert_eq!(
            pipeline.platform.preserving_selected,
            [
                "prepare:monitor.two:PrimaryActive",
                "apply:monitor.two:PrimaryActive"
            ]
        );
        assert!(state.journal.is_none());
        if !drift {
            assert_eq!(pipeline.platform.applied_modes.len(), 2);
        }
    }
}

#[test]
fn ensure_active_and_primary_accept_already_active_sole_target() {
    for option in ["ensure_active", "ensure_primary"] {
        let mut state = State::default();
        let mut pipeline = Pipeline::new(
            FakePlatform {
                hdr_supported: true,
                ..Default::default()
            },
            true,
        );
        let baseline = state.run(&mut pipeline, "owner", json!({"action":"snapshot"}));
        let applied = state.run(
            &mut pipeline,
            "owner",
            mutating_option_request(
                option,
                "monitor.one",
                configuration(),
                &baseline["topology_revision"],
            ),
        );
        assert_eq!(applied["applied"], true, "{option}: {applied}");
        assert_eq!(
            pipeline
                .platform
                .prepared_configuration
                .as_ref()
                .unwrap()
                .displays
                .len(),
            1
        );
        let restored = state.run(
            &mut pipeline,
            "owner",
            json!({"action":"restore","lease":applied["lease"]}),
        );
        assert_eq!(restored["restored"], true);
    }
}

#[test]
fn ensure_only_display_drops_other_and_exactly_restores_baseline() {
    let option = "ensure_only_display";
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            reorder_readback: true,
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let baseline = state.run(&mut pipeline, "owner", json!({"action":"snapshot"}));
    let mut request = mutating_option_request(
        option,
        "monitor.two",
        two_display_configuration(),
        &baseline["topology_revision"],
    );
    request["operation_id"] = json!("topology-retry");
    let applied = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(applied["applied"], true, "{option}: {applied}");
    let prepared = pipeline.platform.prepared_configuration.as_ref().unwrap();
    assert_eq!(prepared.displays.len(), 1);
    assert_eq!(prepared.displays[0].id, "monitor.two");
    assert_eq!(prepared.displays[0].x, 0);
    assert_eq!(
        state.journal.as_ref().unwrap()["snapshot"]["paths"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let effects = pipeline.platform.effects.len();
    let retry = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(retry["state"], "recovery_required");
    assert_eq!(retry["lease"], applied["lease"]);
    assert_eq!(pipeline.platform.effects.len(), effects);
    assert_eq!(pipeline.platform.applied_modes.len(), 1);
    request["mode_policy"]["configuration_option"] = json!("ensure_primary");
    assert_eq!(
        state.run(&mut pipeline, "owner", request)["error"]["code"],
        "operation_conflict"
    );
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert_eq!(pipeline.platform.applied_modes.len(), 2);
}

#[test]
fn ensure_active_inactive_target_persists_route_and_restores_baseline() {
    let mut state = State::default();
    let mut baseline = snapshot();
    baseline.paths[0].id = "monitor.other".into();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            inactive: true,
            hdr_supported: true,
            current_snapshot: Some(baseline.clone()),
            ..Default::default()
        },
        true,
    );
    let revision = json!(baseline.revision().unwrap());
    let applied = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request("ensure_active", "monitor.one", configuration(), &revision),
    );
    assert_eq!(applied["applied"], true, "{applied}");
    assert_eq!(
        state.journal.as_ref().unwrap()["pending_route"]["id"],
        "monitor.one"
    );
    let prepared = pipeline.platform.prepared_configuration.as_ref().unwrap();
    assert_eq!(prepared.displays.len(), 2);
    assert_eq!(prepared.displays[0].id, "monitor.other");
    assert_eq!(prepared.displays[1].id, "monitor.one");
    assert_eq!(prepared.displays[1].x, 1920);
    assert_eq!(
        pipeline.platform.preserving_selected,
        [
            "prepare:monitor.one:PreserveActive",
            "apply:monitor.one:PreserveActive"
        ]
    );
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert!(pipeline.platform.inactive);
}

#[test]
fn ensure_only_inactive_target_replaces_two_paths_and_restores_them() {
    let mut baseline = two_display_snapshot();
    baseline.paths[0].id = "monitor.other".into();
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            inactive: true,
            hdr_supported: true,
            current_snapshot: Some(baseline.clone()),
            ..Default::default()
        },
        true,
    );
    let applied = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            "ensure_only_display",
            "monitor.one",
            configuration(),
            &json!(baseline.revision().unwrap()),
        ),
    );
    assert_eq!(applied["applied"], true, "{applied}");
    assert_eq!(
        state.journal.as_ref().unwrap()["pending_route"]["id"],
        "monitor.one"
    );
    assert_eq!(
        state.journal.as_ref().unwrap()["snapshot"]["paths"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let requested = pipeline.platform.prepared_configuration.as_ref().unwrap();
    assert_eq!(requested.displays.len(), 1);
    assert_eq!(requested.displays[0].id, "monitor.one");
    assert_eq!((requested.displays[0].x, requested.displays[0].y), (0, 0));
    assert_eq!(
        pipeline.platform.preserving_selected,
        [
            "prepare:monitor.one:OnlySelected",
            "apply:monitor.one:OnlySelected"
        ]
    );
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert_eq!(pipeline.platform.applied_modes.len(), 2);
    assert!(pipeline.platform.inactive);
}

#[test]
fn ensure_only_inactive_route_drift_and_failed_apply_keep_recovery_rules() {
    let mut baseline = two_display_snapshot();
    baseline.paths[0].id = "monitor.other".into();
    let revision = json!(baseline.revision().unwrap());
    for (route_changed, apply_failure, expected) in [
        (true, false, "topology_changed"),
        (false, true, "platform_error"),
    ] {
        let mut state = State::default();
        let mut pipeline = Pipeline::new(
            FakePlatform {
                current_snapshot: Some(baseline.clone()),
                inactive: true,
                hdr_supported: true,
                inactive_route_changed: route_changed,
                apply_failure,
                ..Default::default()
            },
            true,
        );
        let reply = state.run(
            &mut pipeline,
            "owner",
            mutating_option_request(
                "ensure_only_display",
                "monitor.one",
                configuration(),
                &revision,
            ),
        );
        assert_eq!(reply["error"]["code"], expected);
        assert_eq!(
            pipeline.platform.effects,
            if route_changed {
                vec![]
            } else {
                vec!["apply", "restore"]
            }
        );
        assert!(state.journal.is_none());
        assert_eq!(
            pipeline.platform.applied_modes.len(),
            if route_changed { 0 } else { 2 }
        );
    }
}

#[test]
fn ensure_only_inactive_rejects_unrestorable_baseline_before_journal() {
    let mut baseline = two_display_snapshot();
    baseline.paths[0].id = "monitor.other".into();
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            inactive: true,
            restoration_unavailable: true,
            ..Default::default()
        },
        true,
    );
    let reply = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            "ensure_only_display",
            "monitor.one",
            configuration(),
            &json!(baseline.revision().unwrap()),
        ),
    );
    assert_eq!(reply["error"]["code"], "unsupported_topology");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn ensure_only_rejects_unknown_hdr_on_a_display_it_would_deactivate() {
    let mut baseline = two_display_snapshot();
    baseline.paths[1].hdr_enabled = None;
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let reply = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            "ensure_only_display",
            "monitor.one",
            configuration(),
            &json!(baseline.revision().unwrap()),
        ),
    );
    assert_eq!(reply["error"]["code"], "unsupported_topology");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn ensure_only_inactive_restores_cloned_baseline() {
    let mut baseline = two_display_snapshot();
    baseline.paths[0].id = "monitor.other".into();
    baseline.paths[1].source_address = baseline.paths[0].source_address.clone();
    baseline.paths[1].source_name = baseline.paths[0].source_name.clone();
    baseline.paths[1].source.x = 0;
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            inactive: true,
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let reply = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            "ensure_only_display",
            "monitor.one",
            configuration(),
            &json!(baseline.revision().unwrap()),
        ),
    );
    assert_eq!(reply["applied"], true);
    assert!(state.journal.is_some());
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":reply["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert!(state.journal.is_none());
}

#[test]
fn ensure_only_inactive_restart_restores_saved_paths_without_pending_route() {
    let mut baseline = two_display_snapshot();
    baseline.paths[0].id = "monitor.other".into();
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            inactive: true,
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let applied = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            "ensure_only_display",
            "monitor.one",
            configuration(),
            &json!(baseline.revision().unwrap()),
        ),
    );
    assert_eq!(applied["applied"], true);
    let mut restarted = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline),
            inactive_route_changed: true,
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    assert_eq!(
        state.run(&mut restarted, "owner", json!({"action":"status"}))["state"],
        "recovery_required"
    );
    let recovered = state.run(
        &mut restarted,
        "owner",
        json!({"action":"recover","lease":applied["lease"]}),
    );
    assert_eq!(recovered["restored"], true);
    assert_eq!(restarted.platform.effects, ["restore"]);
    assert_eq!(restarted.platform.applied_modes.len(), 2);
    assert!(state.journal.is_none());
}

#[test]
fn inactive_route_ambiguity_hdr_and_post_journal_drift_fail_closed() {
    let mut baseline = snapshot();
    baseline.paths[0].id = "monitor.other".into();
    for (ambiguous, changed, expected) in [
        (true, false, "unsupported_topology"),
        (false, true, "topology_changed"),
    ] {
        let mut state = State::default();
        let mut pipeline = Pipeline::new(
            FakePlatform {
                current_snapshot: Some(baseline.clone()),
                inactive: true,
                hdr_supported: true,
                inactive_route_ambiguous: ambiguous,
                inactive_route_changed: changed,
                ..Default::default()
            },
            true,
        );
        let revision = json!(baseline.revision().unwrap());
        let reply = state.run(
            &mut pipeline,
            "owner",
            mutating_option_request("ensure_active", "monitor.one", configuration(), &revision),
        );
        assert_eq!(reply["error"]["code"], expected);
        assert!(state.journal.is_none());
        assert!(pipeline.platform.effects.is_empty());
        assert_eq!(state.revision, if changed { 2 } else { 0 });
    }
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            inactive: true,
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let mut hdr = configuration();
    hdr["displays"][0]["hdr_enabled"] = json!(true);
    let reply = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            "ensure_active",
            "monitor.one",
            hdr,
            &json!(baseline.revision().unwrap()),
        ),
    );
    assert_eq!(reply["error"]["code"], "unsupported_display");
    assert_eq!(state.revision, 0);
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn pending_inactive_route_restart_recovers_only_saved_active_baseline() {
    let mut baseline = snapshot();
    baseline.paths[0].id = "monitor.other".into();
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            inactive: true,
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let applied = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            "ensure_active",
            "monitor.one",
            configuration(),
            &json!(baseline.revision().unwrap()),
        ),
    );
    assert_eq!(applied["applied"], true);
    let saved = state.journal.as_ref().unwrap().clone();
    assert_eq!(saved["pending_route"]["id"], "monitor.one");
    assert_eq!(saved["snapshot"]["paths"][0]["id"], "monitor.other");
    let mut restarted = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline),
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let status = state.run(&mut restarted, "owner", json!({"action":"status"}));
    assert_eq!(status["state"], "recovery_required");
    let recovered = state.run(
        &mut restarted,
        "owner",
        json!({"action":"recover","lease":applied["lease"]}),
    );
    assert_eq!(recovered["restored"], true);
    assert_eq!(restarted.platform.effects, ["restore"]);
    assert!(state.journal.is_none());
}

#[test]
fn ensure_primary_places_inactive_target_left_of_translated_desktop_and_restores() {
    let mut baseline = two_display_snapshot();
    baseline.paths[0].source.x = -1920;
    baseline.paths[0].source.y = 800;
    baseline.paths[1].source.x = 0;
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let revision = json!(baseline.revision().unwrap());
    let mut selected = configuration();
    selected["displays"][0]["id"] = json!("monitor.new");
    let applied = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request("ensure_primary", "monitor.new", selected, &revision),
    );
    assert_eq!(applied["applied"], true, "{applied}");
    let prepared = pipeline.platform.prepared_configuration.as_ref().unwrap();
    assert_eq!(prepared.displays.len(), 3);
    assert_eq!(
        (
            prepared.displays[0].x,
            prepared.displays[1].x,
            prepared.displays[2].x
        ),
        (1280, 3200, 0)
    );
    assert_eq!(
        (
            prepared.displays[0].y,
            prepared.displays[1].y,
            prepared.displays[2].y
        ),
        (0, -800, 0)
    );
    assert_eq!(
        state.journal.as_ref().unwrap()["pending_route"]["id"],
        "monitor.new"
    );
    assert_eq!(
        pipeline.platform.preserving_selected,
        [
            "prepare:monitor.new:TranslateActive",
            "apply:monitor.new:TranslateActive"
        ]
    );
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert_eq!(pipeline.platform.applied_modes.len(), 2);
}

#[test]
fn ensure_primary_inactive_overflow_and_route_drift_have_no_native_effect() {
    let mut baseline = two_display_snapshot();
    baseline.paths[0].source.x = -65_536;
    baseline.paths[1].source.x = 0;
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let mut selected = configuration();
    selected["displays"][0]["id"] = json!("monitor.new");
    let reply = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            "ensure_primary",
            "monitor.new",
            selected.clone(),
            &json!(baseline.revision().unwrap()),
        ),
    );
    assert_eq!(reply["error"]["code"], "invalid_request");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());

    let mut baseline = two_display_snapshot();
    baseline.paths[0].source.x = -1920;
    baseline.paths[1].source.x = 0;
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            hdr_supported: true,
            inactive_route_changed: true,
            ..Default::default()
        },
        true,
    );
    let reply = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            "ensure_primary",
            "monitor.new",
            selected,
            &json!(baseline.revision().unwrap()),
        ),
    );
    assert_eq!(reply["error"]["code"], "topology_changed");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn ensure_primary_inactive_partial_apply_and_restart_restore_old_desktop() {
    let mut baseline = two_display_snapshot();
    baseline.paths[0].source.x = -1920;
    baseline.paths[0].source.y = 800;
    baseline.paths[1].source.x = 0;
    let mut selected = configuration();
    selected["displays"][0]["id"] = json!("monitor.new");
    let request = mutating_option_request(
        "ensure_primary",
        "monitor.new",
        selected,
        &json!(baseline.revision().unwrap()),
    );
    let mut state = State::default();
    let mut failing = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            hdr_supported: true,
            apply_failure: true,
            ..Default::default()
        },
        true,
    );
    let failed = state.run(&mut failing, "owner", request.clone());
    assert_eq!(failed["error"]["code"], "platform_error");
    assert_eq!(failing.platform.effects, ["apply", "restore"]);
    assert_eq!(failing.platform.applied_modes.len(), 2);
    assert!(state.journal.is_none());

    let mut applying = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline.clone()),
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let applied = state.run(&mut applying, "owner", request);
    assert_eq!(applied["applied"], true);
    let mut restarted = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(baseline),
            hdr_supported: true,
            inactive_route_changed: true,
            ..Default::default()
        },
        true,
    );
    let restored = state.run(
        &mut restarted,
        "owner",
        json!({"action":"recover","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert_eq!(restarted.platform.effects, ["restore"]);
    assert_eq!(restarted.platform.applied_modes.len(), 2);
    assert!(state.journal.is_none());
}

#[test]
fn ensure_only_rejects_hotplug_before_journal_and_rolls_back_partial_apply() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            reorder_readback: true,
            hdr_supported: true,
            drift_on_prepare: true,
            ..Default::default()
        },
        true,
    );
    let baseline = state.run(&mut pipeline, "owner", json!({"action":"snapshot"}));
    let request = mutating_option_request(
        "ensure_only_display",
        "monitor.two",
        two_display_configuration(),
        &baseline["topology_revision"],
    );
    let rejected = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(rejected["error"]["code"], "topology_changed");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
    pipeline.platform.drift_on_prepare = false;
    pipeline.platform.current_snapshot = None;
    pipeline.platform.apply_failure = true;
    let failed = state.run(&mut pipeline, "owner", request);
    assert_eq!(failed["error"]["code"], "platform_error");
    assert_eq!(pipeline.platform.effects, ["apply", "restore"]);
    assert!(state.journal.is_none());
    assert_eq!(pipeline.platform.applied_modes.len(), 2);
}

#[test]
fn ensure_only_hotplug_after_journal_does_not_mutate_or_keep_lease() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            reorder_readback: true,
            hdr_supported: true,
            drift_on_apply: true,
            ..Default::default()
        },
        true,
    );
    let baseline = state.run(&mut pipeline, "owner", json!({"action":"snapshot"}));
    let reply = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            "ensure_only_display",
            "monitor.two",
            two_display_configuration(),
            &baseline["topology_revision"],
        ),
    );
    assert_eq!(reply["error"]["code"], "topology_changed");
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_none());
    assert_eq!(state.revision, 2);
}

#[test]
fn stale_topology_revision_rejects_apply_without_journal_or_native_effect() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let current = state.run(&mut pipeline, "owner", json!({"action":"snapshot"}));
    pipeline.platform.topology_changed = true;
    let response = state.run(&mut pipeline, "owner", json!({"action":"apply","configuration":configuration(),"expected_topology_revision":current["topology_revision"],"operation_id":"session-1"}));
    assert_eq!(response["error"]["code"], "topology_changed");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn revisioned_apply_rejects_inactive_id_and_ambiguous_snapshot() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            inactive: true,
            ..Default::default()
        },
        true,
    );
    let mut active = snapshot();
    active.paths[0].id = "monitor.other".into();
    active.paths[0].hdr_enabled = None;
    let revision = active.revision().unwrap();
    pipeline.platform.current_snapshot = Some(active);
    let inactive = state.run(&mut pipeline, "owner", json!({"action":"apply","configuration":configuration(),"expected_topology_revision":revision}));
    assert_eq!(inactive["error"]["code"], "unsupported_display");
    assert!(state.journal.is_none());
    let mut ambiguous = two_display_snapshot();
    ambiguous.paths[1].id = "MONITOR.ONE".into();
    assert_eq!(ambiguous.validate().unwrap_err().code, "invalid_snapshot");
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn revisioned_apply_cannot_omit_active_monitor() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            reorder_readback: true,
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let topology = state.run(&mut pipeline, "owner", json!({"action":"snapshot"}));
    let response = state.run(&mut pipeline, "owner", json!({"action":"apply","configuration":configuration(),"expected_topology_revision":topology["topology_revision"]}));
    assert_eq!(response["error"]["code"], "unsupported_display");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn durable_operation_identity_prevents_same_caller_adoption_and_survives_restart() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let applied = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"apply","configuration":configuration(),"operation_id":"session-1"}),
    );
    assert_eq!(applied["applied"], true);
    assert_eq!(state.journal.as_ref().unwrap()["operation_id"], "session-1");
    let retry = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"apply","configuration":configuration(),"operation_id":"session-1"}),
    );
    assert_eq!(retry["lease"], applied["lease"]);
    assert_eq!(retry["applied"], false);
    assert_eq!(pipeline.platform.effects, ["apply"]);
    let other = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"apply","configuration":configuration(),"operation_id":"session-2"}),
    );
    assert_eq!(other["error"]["code"], "display_busy");
    let changed = state.run(&mut pipeline, "owner", json!({"action":"apply","configuration":two_display_configuration(),"operation_id":"session-1"}));
    assert_eq!(changed["error"]["code"], "operation_conflict");
    let changed_revision = state.run(&mut pipeline, "owner", json!({"action":"apply","configuration":configuration(),"operation_id":"session-1","expected_topology_revision":"00000000000000000000000000000000"}));
    assert_eq!(changed_revision["error"]["code"], "operation_conflict");
    let foreign = state.run(&mut pipeline, "foreign", json!({"action":"status"}));
    assert!(foreign["operation_id"].is_null());
    assert!(foreign["lease"].is_null());
    let mut restarted = Pipeline::new(FakePlatform::default(), true);
    let owner = state.run(&mut restarted, "owner", json!({"action":"status"}));
    assert_eq!(owner["operation_id"], "session-1");
    assert_eq!(owner["state"], "recovery_required");
    let retry = state.run(
        &mut restarted,
        "owner",
        json!({"action":"apply","configuration":configuration(),"operation_id":"session-1"}),
    );
    assert_eq!(retry["state"], "recovery_required");
    assert_eq!(retry["applied"], false);
    assert!(restarted.platform.effects.is_empty());
}

#[test]
fn policy_retry_uses_original_request_after_mode_and_hdr_apply() {
    let mut state = State {
        lose_apply_completion: true,
        ..Default::default()
    };
    let mut pipeline = Pipeline::new(
        FakePlatform {
            available_modes: vec![crate::model::AvailableMode {
                width: 1920,
                height: 1080,
                bits_per_pixel: 32,
                nominal_refresh_hz: 120,
                exact_current: false,
                refresh: Rational {
                    numerator: 120,
                    denominator: 1,
                },
            }],
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let request = json!({
        "action":"apply", "configuration":configuration(), "operation_id":"policy-lost-reply",
        "mode_policy":{
            "display_id":"monitor.one",
            "context":{"resolution_is_automatic":true,"refresh_rate_is_automatic":false,
                "client_display_mode_override":false,"optimize_game_settings":true},
            "remapping":{"resolution_only":[{"requested_resolution":"1280x720",
                "final_resolution":"1920x1080"}]},
            "resolution_option":"auto", "manual_resolution":"1920x1080",
            "refresh_rate_option":"prefer_highest",
            "hdr_option":"auto", "effective_client_hdr_request":true
        }
    });
    let applied = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(applied["reply_lost"], true, "{applied}");
    let lease = state.journal.as_ref().unwrap()["lease"].clone();
    assert_eq!(pipeline.platform.applied_mode.unwrap().0, 1920);
    assert_eq!(pipeline.platform.applied_mode.unwrap().2.numerator, 120);
    assert!(pipeline.platform.hdr_state);
    assert_eq!(pipeline.platform.hdr_mutations, 1);
    assert_eq!(pipeline.platform.mode_queries, 2);
    assert_eq!(
        pipeline
            .platform
            .prepared_configuration
            .as_ref()
            .unwrap()
            .displays[0]
            .width,
        1920
    );

    pipeline.platform.available_modes.clear();
    let retry = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(retry["state"], "recovery_required");
    assert_eq!(retry["applied"], false);
    assert_eq!(retry["lease"], lease);
    assert_eq!(pipeline.platform.mode_queries, 2);
    assert_eq!(pipeline.platform.hdr_mutations, 1);
    assert_eq!(pipeline.platform.effects, ["apply"]);

    for field in [
        "manual_resolution",
        "refresh_rate_option",
        "hdr_option",
        "effective_client_hdr_request",
    ] {
        let mut changed = request.clone();
        changed["mode_policy"][field] = json!(match field {
            "manual_resolution" => "1280x720",
            "refresh_rate_option" => "auto",
            "hdr_option" => "disabled",
            _ => "false",
        });
        if field == "effective_client_hdr_request" {
            changed["mode_policy"][field] = json!(false);
        }
        let rejected = state.run(&mut pipeline, "owner", changed);
        assert_eq!(
            rejected["error"]["code"], "operation_conflict",
            "{field}: {rejected}"
        );
    }
    let mut changed_revision = request.clone();
    changed_revision["expected_topology_revision"] = json!("00000000000000000000000000000000");
    assert_eq!(
        state.run(&mut pipeline, "owner", changed_revision)["error"]["code"],
        "operation_conflict"
    );
    let foreign = state.run(&mut pipeline, "foreign", request.clone());
    assert_eq!(foreign["error"]["code"], "display_busy");
    assert!(foreign["lease"].is_null());

    let mut restarted = Pipeline::new(FakePlatform::default(), true);
    let recovered = state.run(&mut restarted, "owner", request);
    assert_eq!(recovered["state"], "recovery_required");
    assert_eq!(recovered["lease"], lease);
    assert!(restarted.platform.effects.is_empty());
    let readback = state.run(
        &mut restarted,
        "owner",
        json!({"action":"recover","lease":lease}),
    );
    assert_eq!(readback["ok"], true, "{readback}");
    assert!(state.journal.is_none());
    assert_eq!(restarted.platform.effects, ["restore"]);
}

#[test]
fn legacy_journal_without_request_identity_keeps_exact_plain_retry() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let request = json!({"action":"apply","configuration":configuration(),"operation_id":"legacy"});
    let applied = state.run(&mut pipeline, "owner", request.clone());
    state
        .journal
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("request_identity");
    state.journal.as_mut().unwrap()["requested_configuration"] = configuration();
    let mut restarted = Pipeline::new(FakePlatform::default(), true);
    let retry = state.run(&mut restarted, "owner", request);
    assert_eq!(retry["state"], "recovery_required");
    assert_eq!(retry["lease"], applied["lease"]);
    assert!(restarted.platform.effects.is_empty());
    let restored = state.run(
        &mut restarted,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
}

#[test]
fn legacy_policy_journal_cannot_be_adopted_without_original_request() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let request = json!({"action":"apply","configuration":configuration(),
        "operation_id":"legacy-policy", "mode_policy":policy("monitor.one", "1920x1080", "120")});
    let applied = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(applied["applied"], true);
    state
        .journal
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("request_identity");
    let retry = state.run(&mut pipeline, "owner", request);
    assert_eq!(retry["error"]["code"], "operation_conflict");
    assert_eq!(pipeline.platform.effects, ["apply"]);
}

#[test]
fn durable_policy_journal_recovers_when_set_acceptance_is_lost() {
    let mut state = State {
        lose_set_acceptance: true,
        ..Default::default()
    };
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let request = json!({
        "action":"apply", "configuration":configuration(), "operation_id":"lost-set",
        "mode_policy":policy("monitor.one", "1920x1080", "120")
    });
    let lost = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(lost["error"]["code"], "transport_error");
    assert!(state.journal.is_some());
    assert!(pipeline.platform.effects.is_empty());
    let retried = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(retried["state"], "recovery_required");
    assert_eq!(retried["applied"], false);
    assert_eq!(retried["lease"], state.journal.as_ref().unwrap()["lease"]);
    assert!(pipeline.platform.effects.is_empty());
    let mut changed = request.clone();
    changed["mode_policy"]["remapping"]["mixed"][0]["final_refresh_rate"] = json!("144");
    assert_eq!(
        state.run(&mut pipeline, "owner", changed)["error"]["code"],
        "operation_conflict"
    );
    assert_eq!(
        state.run(&mut pipeline, "foreign", request)["error"]["code"],
        "display_busy"
    );
}
#[test]
fn durable_journal_precedes_effects_and_only_owner_can_restore() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let applied = state.run(
        &mut pipeline,
        "owner.one",
        json!({"action":"apply","configuration":configuration()}),
    );
    assert_eq!(applied["ok"], true);
    assert_eq!(pipeline.platform.effects, ["apply"]);
    assert_eq!(state.journal.as_ref().unwrap()["owner"], "owner.one");
    let blocked = state.run(
        &mut pipeline,
        "owner.two",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(blocked["error"]["code"], "permission_denied");
    assert_eq!(pipeline.platform.effects, ["apply"]);
    let busy = state.run(
        &mut pipeline,
        "owner.one",
        json!({"action":"apply","configuration":configuration()}),
    );
    assert_eq!(busy["error"]["code"], "display_busy");
    let restored = state.run(
        &mut pipeline,
        "owner.one",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert_eq!(pipeline.platform.effects, ["apply", "restore"]);
    assert!(state.journal.is_none());
}
#[test]
fn hdr_auto_enables_after_journal_and_restore_recovers_sdr() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let applied = state.run(&mut pipeline, "owner", hdr_apply("auto", Some(true)));
    assert_eq!(applied["applied"], true);
    assert_eq!(pipeline.platform.prepared_hdr, Some(Some(true)));
    assert!(pipeline.platform.hdr_state);
    assert_eq!(
        state.journal.as_ref().unwrap()["snapshot"]["paths"][0]["hdr_enabled"],
        false
    );
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert!(!pipeline.platform.hdr_state);
}

#[test]
fn hdr_auto_disables_and_restore_recovers_hdr() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            hdr_supported: true,
            hdr_state: true,
            ..Default::default()
        },
        true,
    );
    let applied = state.run(&mut pipeline, "owner", hdr_apply("auto", Some(false)));
    assert_eq!(applied["applied"], true);
    assert!(!pipeline.platform.hdr_state);
    assert_eq!(
        state.journal.as_ref().unwrap()["snapshot"]["paths"][0]["hdr_enabled"],
        true
    );
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert!(pipeline.platform.hdr_state);
}

#[test]
fn disabled_hdr_policy_leaves_hdr_untouched() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            hdr_supported: true,
            hdr_state: true,
            ..Default::default()
        },
        true,
    );
    let applied = state.run(&mut pipeline, "owner", hdr_apply("disabled", Some(false)));
    assert_eq!(applied["applied"], true);
    assert_eq!(pipeline.platform.prepared_hdr, Some(None));
    assert!(pipeline.platform.hdr_state);
}

#[test]
fn required_sdr_source_overrides_hdr_client_and_disabled_display_option() {
    for option in ["auto", "disabled"] {
        let mut state = State::default();
        let mut pipeline = Pipeline::new(
            FakePlatform {
                hdr_supported: true,
                hdr_state: true,
                ..Default::default()
            },
            true,
        );
        let mut request = hdr_apply(option, Some(true));
        request["mode_policy"]["source_hdr_required"] = json!(false);
        let applied = state.run(&mut pipeline, "owner", request);
        assert_eq!(applied["applied"], true, "{applied}");
        assert_eq!(pipeline.platform.prepared_hdr, Some(Some(false)));
        assert!(!pipeline.platform.hdr_state);
        let restored = state.run(
            &mut pipeline,
            "owner",
            json!({"action":"restore","lease":applied["lease"]}),
        );
        assert_eq!(restored["restored"], true);
        assert!(pipeline.platform.hdr_state);
    }
}

#[test]
fn unsupported_hdr_and_missing_client_request_cannot_claim_success() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let unsupported = state.run(&mut pipeline, "owner", hdr_apply("auto", Some(true)));
    assert_eq!(unsupported["error"]["code"], "unsupported_display");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
    let missing = state.run(&mut pipeline, "owner", hdr_apply("auto", None));
    assert_eq!(missing["error"]["code"], "invalid_request");
    assert!(state.journal.is_none());
}

#[test]
fn hdr_auto_sdr_request_is_a_noop_on_unsupported_active_target() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let applied = state.run(&mut pipeline, "owner", hdr_apply("auto", Some(false)));
    assert_eq!(applied["applied"], true);
    assert_eq!(pipeline.platform.prepared_hdr, Some(Some(false)));
    assert!(!pipeline.platform.hdr_state);
    assert_eq!(pipeline.platform.hdr_mutations, 0);
    assert_eq!(pipeline.platform.effects, ["apply"]);
    assert!(state.journal.is_some());
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert!(!pipeline.platform.hdr_state);
}

#[test]
fn hdr_auto_sdr_request_activates_inactive_target_without_unsaved_hdr_mutation() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            inactive: true,
            ..Default::default()
        },
        true,
    );
    let applied = state.run(&mut pipeline, "owner", hdr_apply("auto", Some(false)));
    assert_eq!(applied["applied"], true);
    assert!(!pipeline.platform.inactive);
    assert_eq!(pipeline.platform.hdr_mutations, 0);
    assert_eq!(
        state.journal.as_ref().unwrap()["snapshot"]["paths"][0]["id"],
        "monitor.other"
    );
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert!(pipeline.platform.inactive);
}

#[test]
fn inactive_target_activating_with_hdr_rolls_back_without_changing_unsaved_hdr() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            inactive: true,
            hdr_supported: true,
            inactive_activation_hdr: true,
            ..Default::default()
        },
        true,
    );
    let failed = state.run(&mut pipeline, "owner", hdr_apply("auto", Some(false)));
    assert_eq!(failed["error"]["code"], "verification_failed");
    assert_eq!(pipeline.platform.effects, ["apply", "restore"]);
    assert_eq!(pipeline.platform.hdr_mutations, 0);
    assert!(pipeline.platform.inactive);
    assert!(state.journal.is_none());
}

#[test]
fn old_sdr_journal_refuses_external_hdr_before_restore_mutation() {
    let mut state = State {
        revision: 1,
        journal: Some(json!({
            "owner":"owner",
            "lease":"legacy-lease",
            "snapshot":snapshot()
        })),
        ..Default::default()
    };
    state.journal.as_mut().unwrap()["snapshot"]["paths"][0]
        .as_object_mut()
        .unwrap()
        .remove("hdr_enabled");
    let mut pipeline = Pipeline::new(
        FakePlatform {
            hdr_supported: true,
            hdr_state: true,
            ..Default::default()
        },
        true,
    );
    let blocked = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"recover","lease":"legacy-lease"}),
    );
    assert_eq!(blocked["error"]["code"], "recovery_blocked");
    assert_eq!(blocked["recovery_required"], true);
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_some());
    pipeline.platform.hdr_state = false;
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"recover","lease":"legacy-lease"}),
    );
    assert_eq!(restored["restored"], true);
    assert_eq!(pipeline.platform.effects, ["restore"]);
}

#[test]
fn old_sdr_journal_refuses_inactive_saved_target_before_restore_mutation() {
    let mut old = serde_json::to_value(snapshot()).unwrap();
    old["paths"][0]
        .as_object_mut()
        .unwrap()
        .remove("hdr_enabled");
    let mut state = State {
        revision: 1,
        journal: Some(json!({"owner":"owner","lease":"legacy-lease","snapshot":old})),
        ..Default::default()
    };
    let mut pipeline = Pipeline::new(
        FakePlatform {
            inactive: true,
            ..Default::default()
        },
        true,
    );
    let blocked = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"recover","lease":"legacy-lease"}),
    );
    assert_eq!(blocked["error"]["code"], "recovery_blocked");
    assert_eq!(blocked["recovery_required"], true);
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_some());
}

#[test]
fn failed_hdr_apply_rolls_back_baseline_before_clearing_journal() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            hdr_supported: true,
            apply_failure: true,
            ..Default::default()
        },
        true,
    );
    let result = state.run(&mut pipeline, "owner", hdr_apply("auto", Some(true)));
    assert_eq!(result["error"]["code"], "platform_error");
    assert_eq!(pipeline.platform.effects, ["apply", "restore"]);
    assert!(!pipeline.platform.hdr_state);
    assert!(state.journal.is_none());
}

#[test]
fn late_hdr_mode_drift_rolls_back_or_retains_recovery_journal() {
    for restore_failure in [false, true] {
        let mut state = State::default();
        let mut pipeline = Pipeline::new(
            FakePlatform {
                hdr_supported: true,
                late_mode_drift: true,
                restore_failure,
                ..Default::default()
            },
            true,
        );
        let result = state.run(&mut pipeline, "owner", hdr_apply("auto", Some(true)));
        assert_eq!(result["error"]["code"], "verification_failed");
        assert_eq!(pipeline.platform.effects, ["apply", "restore"]);
        assert_eq!(state.journal.is_some(), restore_failure);
        if restore_failure {
            assert_eq!(result["recovery_required"], true);
            assert!(pipeline.platform.hdr_state);
        } else {
            assert!(!pipeline.platform.hdr_state);
            assert_eq!(pipeline.platform.applied_mode.unwrap().0, 1920);
        }
    }
}

#[test]
fn failed_hdr_restore_keeps_recovery_lease_until_verified() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            hdr_supported: true,
            restore_failure: true,
            ..Default::default()
        },
        true,
    );
    let applied = state.run(&mut pipeline, "owner", hdr_apply("auto", Some(true)));
    assert_eq!(applied["applied"], true);
    let failed = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(failed["error"]["code"], "platform_error");
    assert_eq!(failed["recovery_required"], true);
    assert!(pipeline.platform.hdr_state);
    assert!(state.journal.is_some());
    pipeline.platform.restore_failure = false;
    let recovered = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"recover","lease":applied["lease"]}),
    );
    assert_eq!(recovered["restored"], true);
    assert!(!pipeline.platform.hdr_state);
    assert!(state.journal.is_none());
}
#[test]
fn remapped_mode_reaches_prepare_and_durable_apply() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action":"apply",
            "configuration":configuration(),
            "mode_policy":{
                "display_id":"MONITOR.ONE",
                "context":{
                    "resolution_is_automatic":true,
                    "refresh_rate_is_automatic":true,
                    "client_display_mode_override":false,
                    "optimize_game_settings":true
                },
                "remapping":{
                    "mixed":[{
                        "requested_resolution":"1280x720",
                        "requested_fps":"60",
                        "final_resolution":"1920x1080",
                        "final_refresh_rate":"120"
                    }]
                }
            }
        }),
    );
    assert_eq!(result["applied"], true);
    let mode = Some((
        1920,
        1080,
        Rational {
            numerator: 120,
            denominator: 1,
        },
    ));
    assert_eq!(pipeline.platform.prepared_mode, mode);
    assert_eq!(pipeline.platform.applied_mode, mode);
    assert!(state.journal.is_some());
}

#[test]
fn unsupported_virtual_layout_rejects_mode_and_hdr_policy_before_journal_or_native_effect() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            virtual_layout_unsupported: true,
            hdr_supported: true,
            ..Default::default()
        },
        true,
    );
    let mut request = json!({
        "action":"apply",
        "configuration":configuration(),
        "mode_policy":policy("monitor.one", "1920x1080", "120")
    });
    request["mode_policy"]["hdr_option"] = json!("auto");
    request["mode_policy"]["effective_client_hdr_request"] = json!(true);
    let rejected = state.run(&mut pipeline, "owner", request);
    assert_eq!(rejected["error"]["code"], "unsupported_topology");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.prepared_configuration.is_none());
    assert!(pipeline.platform.effects.is_empty());
    assert_eq!(pipeline.platform.hdr_mutations, 0);
}

#[test]
fn two_mode_policies_reach_one_durable_apply_and_restore() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            current_snapshot: Some(two_display_snapshot()),
            ..Default::default()
        },
        true,
    );
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action":"apply",
            "configuration":two_display_configuration(),
            "mode_policies":[
                policy("monitor.one", "1920x1080", "120"),
                policy("MONITOR.TWO", "1600x900", "75")
            ]
        }),
    );
    assert_eq!(result["applied"], true, "{result}");
    let expected = vec![
        (
            "monitor.one".into(),
            1920,
            1080,
            Rational {
                numerator: 120,
                denominator: 1,
            },
        ),
        (
            "monitor.two".into(),
            1600,
            900,
            Rational {
                numerator: 75,
                denominator: 1,
            },
        ),
    ];
    assert_eq!(pipeline.platform.prepared_modes, expected);
    assert_eq!(pipeline.platform.applied_modes, expected);
    assert_eq!(
        state.journal.as_ref().unwrap()["snapshot"]["paths"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action":"restore", "lease":result["lease"]
        }),
    );
    assert_eq!(restored["restored"], true);
    assert_eq!(pipeline.platform.effects, ["apply", "restore"]);
    assert_eq!(pipeline.platform.applied_modes.len(), 2);
    assert_eq!(pipeline.platform.applied_modes[0].1, 1920);
    assert_eq!(pipeline.platform.applied_modes[1].1, 1920);
    assert!(state.journal.is_none());
}

#[test]
fn highest_refresh_queries_each_policy_target() {
    let mode = |hz| crate::model::AvailableMode {
        width: 1280,
        height: 720,
        refresh: Rational {
            numerator: hz,
            denominator: 1,
        },
        nominal_refresh_hz: hz,
        bits_per_pixel: 32,
        exact_current: false,
    };
    let mut pipeline = Pipeline::new(
        FakePlatform {
            available_modes: vec![mode(120)],
            second_available_modes: vec![mode(75)],
            current_snapshot: Some(two_display_snapshot()),
            ..Default::default()
        },
        true,
    );
    let mut state = State::default();
    let policy = |display_id| {
        json!({
            "display_id": display_id,
            "remapping": {},
            "context": {
                "resolution_is_automatic": true,
                "refresh_rate_is_automatic": true,
                "client_display_mode_override": false,
                "optimize_game_settings": false
            },
            "refresh_rate_option": "prefer_highest"
        })
    };
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action": "apply",
            "configuration": two_display_configuration(),
            "mode_policies": [policy("monitor.one"), policy("monitor.two")]
        }),
    );
    assert_eq!(result["applied"], true, "{result}");
    assert_eq!(
        pipeline.platform.queried_displays,
        ["monitor.one", "monitor.two", "monitor.one", "monitor.two"]
    );
    assert_eq!(pipeline.platform.prepared_modes[0].3.numerator, 120);
    assert_eq!(pipeline.platform.prepared_modes[1].3.numerator, 75);
}

#[test]
fn invalid_second_mode_policy_has_no_prepare_journal_or_native_mutation() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action":"apply",
            "configuration":two_display_configuration(),
            "mode_policies":[
                policy("monitor.one", "1920x1080", "120"),
                {"display_id":"monitor.two", "context":{
                    "resolution_is_automatic":true,
                    "refresh_rate_is_automatic":true,
                    "client_display_mode_override":false,
                    "optimize_game_settings":true
                }, "remapping":{"mixed":[{"final_resolution":"0x0"}]}}
            ]
        }),
    );
    assert_eq!(result["error"]["code"], "invalid_request");
    assert!(pipeline.platform.prepared_modes.is_empty());
    assert!(pipeline.platform.applied_modes.is_empty());
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_none());
}

#[test]
fn conflicting_duplicate_and_unknown_policy_targets_fail_before_prepare() {
    for policies in [
        json!({"mode_policy":policy("monitor.one", "1920x1080", "120"),
               "mode_policies":[]}),
        json!({"mode_policies":[policy("monitor.one", "1920x1080", "120"),
                                 policy("MONITOR.ONE", "1600x900", "75")]}),
        json!({"mode_policies":[policy("monitor.one", "1920x1080", "120"),
                                 policy("monitor.missing", "1600x900", "75")]}),
    ] {
        let mut state = State::default();
        let mut pipeline = Pipeline::new(FakePlatform::default(), true);
        let mut request = json!({"action":"apply", "configuration":two_display_configuration()});
        request
            .as_object_mut()
            .unwrap()
            .extend(policies.as_object().unwrap().clone());
        let result = state.run(&mut pipeline, "owner", request);
        assert_eq!(result["error"]["code"], "invalid_request");
        assert!(pipeline.platform.prepared_modes.is_empty());
        assert_eq!(pipeline.platform.mode_queries, 0);
        assert!(pipeline.platform.effects.is_empty());
        assert!(state.journal.is_none());
    }
}

#[test]
fn invalid_remapped_mode_never_prepares_provider_or_writes_journal() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action":"apply",
            "configuration":configuration(),
            "mode_policy":{
                "display_id":"monitor.one",
                "context":{
                    "resolution_is_automatic":true,
                    "refresh_rate_is_automatic":true,
                    "client_display_mode_override":false,
                    "optimize_game_settings":true
                },
                "remapping":{"mixed":[{"final_resolution":"0x0"}]}
            }
        }),
    );
    assert_eq!(result["error"]["code"], "invalid_request");
    assert!(pipeline.platform.prepared_mode.is_none());
    assert!(pipeline.platform.applied_mode.is_none());
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_none());
}
#[test]
fn manual_mode_options_reach_provider_before_journaled_apply() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action":"apply",
            "configuration":configuration(),
            "mode_policy":{
                "display_id":"monitor.one",
                "context":{
                    "resolution_is_automatic":true,
                    "refresh_rate_is_automatic":true,
                    "client_display_mode_override":false,
                    "optimize_game_settings":true
                },
                "remapping":{},
                "resolution_option":"manual",
                "manual_resolution":"1920x1080",
                "refresh_rate_option":"manual",
                "manual_refresh_rate":"59.94"
            }
        }),
    );
    assert_eq!(result["applied"], true);
    let mode = Some((
        1920,
        1080,
        Rational {
            numerator: 2997,
            denominator: 50,
        },
    ));
    assert_eq!(pipeline.platform.prepared_mode, mode);
    assert_eq!(pipeline.platform.applied_mode, mode);
    assert!(state.journal.is_some());
}

#[test]
fn explicit_client_mode_override_preserves_concrete_configuration() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action":"apply",
            "configuration":configuration(),
            "mode_policy":{
                "display_id":"monitor.one",
                "context":{
                    "resolution_is_automatic":true,
                    "refresh_rate_is_automatic":true,
                    "client_display_mode_override":true,
                    "optimize_game_settings":true
                },
                "remapping":{"mixed":[{"final_resolution":"1920x1080"}]},
                "resolution_option":"manual",
                "manual_resolution":"bad",
                "refresh_rate_option":"prefer_highest"
            }
        }),
    );
    assert_eq!(result["applied"], true);
    let mode = Some((
        1280,
        720,
        Rational {
            numerator: 60,
            denominator: 1,
        },
    ));
    assert_eq!(pipeline.platform.prepared_mode, mode);
    assert_eq!(pipeline.platform.applied_mode, mode);
    assert_eq!(pipeline.platform.mode_queries, 0);
}

#[test]
fn highest_refresh_uses_final_resolution_and_reaches_durable_apply() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    pipeline.platform.available_modes = vec![
        crate::model::AvailableMode {
            width: 1280,
            height: 720,
            bits_per_pixel: 32,
            nominal_refresh_hz: 240,
            exact_current: false,
            refresh: Rational {
                numerator: 240,
                denominator: 1,
            },
        },
        crate::model::AvailableMode {
            width: 1920,
            height: 1080,
            bits_per_pixel: 32,
            nominal_refresh_hz: 60,
            exact_current: false,
            refresh: Rational {
                numerator: 60,
                denominator: 1,
            },
        },
        crate::model::AvailableMode {
            width: 1920,
            height: 1080,
            bits_per_pixel: 32,
            nominal_refresh_hz: 120,
            exact_current: false,
            refresh: Rational {
                numerator: 120,
                denominator: 1,
            },
        },
        crate::model::AvailableMode {
            width: 1920,
            height: 1080,
            bits_per_pixel: 32,
            nominal_refresh_hz: 120,
            exact_current: true,
            refresh: Rational {
                numerator: 120000,
                denominator: 1001,
            },
        },
    ];
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action":"apply",
            "configuration":configuration(),
            "mode_policy":{
                "display_id":"monitor.one",
                "context":{
                    "resolution_is_automatic":true,
                    "refresh_rate_is_automatic":false,
                    "client_display_mode_override":false,
                    "optimize_game_settings":true
                },
                "remapping":{"resolution_only":[{
                    "requested_resolution":"1280x720",
                    "final_resolution":"1920x1080"
                }]},
                "refresh_rate_option":"prefer_highest"
            }
        }),
    );
    assert_eq!(result["applied"], true);
    let selected = Some((
        1920,
        1080,
        Rational {
            numerator: 120000,
            denominator: 1001,
        },
    ));
    assert_eq!(pipeline.platform.mode_queries, 2);
    assert_eq!(pipeline.platform.prepared_mode, selected);
    assert_eq!(pipeline.platform.applied_mode, selected);
    assert!(state.journal.is_some());
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":result["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert_eq!(pipeline.platform.effects, ["apply", "restore"]);
    assert!(state.journal.is_none());
}

#[test]
fn mode_selection_rejects_physical_route_drift_before_prepare_or_journal() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            available_modes: vec![mode(120)],
            drift_after_mode_query: true,
            ..Default::default()
        },
        true,
    );
    let result = state.run(&mut pipeline, "owner", highest_refresh_request());
    assert_eq!(result["error"]["code"], "topology_changed");
    assert_eq!(pipeline.platform.mode_queries, 1);
    assert!(pipeline.platform.prepared_configuration.is_none());
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_none());
}

#[test]
fn mode_selection_rejects_disappeared_capability_before_prepare_or_journal() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            available_modes: vec![mode(120)],
            lose_mode_after_first_query: true,
            ..Default::default()
        },
        true,
    );
    let result = state.run(&mut pipeline, "owner", highest_refresh_request());
    assert_eq!(result["error"]["code"], "topology_changed");
    assert_eq!(pipeline.platform.mode_queries, 2);
    assert!(pipeline.platform.prepared_configuration.is_none());
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_none());
}

fn highest_refresh_request() -> Value {
    json!({
        "action": "apply",
        "configuration": configuration(),
        "mode_policy": {
            "display_id": "monitor.one",
            "context": {
                "resolution_is_automatic": false,
                "refresh_rate_is_automatic": false,
                "client_display_mode_override": false,
                "optimize_game_settings": true
            },
            "remapping": {},
            "refresh_rate_option": "prefer_highest"
        }
    })
}

fn mode(refresh_hz: u32) -> crate::model::AvailableMode {
    crate::model::AvailableMode {
        width: 1280,
        height: 720,
        refresh: Rational {
            numerator: refresh_hz,
            denominator: 1,
        },
        nominal_refresh_hz: refresh_hz,
        bits_per_pixel: 32,
        exact_current: false,
    }
}

#[test]
fn missing_highest_mode_rejects_before_prepare_and_journal() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    pipeline.platform.available_modes = vec![crate::model::AvailableMode {
        width: 1920,
        height: 1080,
        bits_per_pixel: 32,
        nominal_refresh_hz: 240,
        exact_current: false,
        refresh: Rational {
            numerator: 240,
            denominator: 1,
        },
    }];
    let request = json!({
        "action":"apply", "configuration":configuration(),
        "mode_policy":{
            "display_id":"monitor.one",
            "context":{
                "resolution_is_automatic":false,
                "refresh_rate_is_automatic":false,
                "client_display_mode_override":false,
                "optimize_game_settings":true
            },
            "remapping":{}, "refresh_rate_option":"prefer_highest"
        }
    });
    let result = state.run(&mut pipeline, "owner", request);
    assert_eq!(result["error"]["code"], "unsupported_display");
    assert_eq!(pipeline.platform.mode_queries, 1);
    assert!(pipeline.platform.prepared_mode.is_none());
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_none());
}

#[test]
fn highest_refresh_ignores_modes_incompatible_with_native_32bpp_apply() {
    let request = json!({
        "action":"apply", "configuration":configuration(),
        "mode_policy":{
            "display_id":"monitor.one",
            "context":{
                "resolution_is_automatic":false,
                "refresh_rate_is_automatic":false,
                "client_display_mode_override":false,
                "optimize_game_settings":true
            },
            "remapping":{}, "refresh_rate_option":"prefer_highest"
        }
    });
    let incompatible = crate::model::AvailableMode {
        width: 1280,
        height: 720,
        nominal_refresh_hz: 240,
        refresh: Rational {
            numerator: 240,
            denominator: 1,
        },
        bits_per_pixel: 16,
        exact_current: false,
    };
    let compatible = crate::model::AvailableMode {
        width: 1280,
        height: 720,
        nominal_refresh_hz: 120,
        refresh: Rational {
            numerator: 120,
            denominator: 1,
        },
        bits_per_pixel: 32,
        exact_current: false,
    };
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    pipeline.platform.available_modes = vec![incompatible, compatible];
    let result = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(result["applied"], true);
    assert_eq!(
        pipeline.platform.prepared_mode,
        Some((1280, 720, compatible.refresh))
    );
    assert_eq!(
        pipeline.platform.applied_mode,
        pipeline.platform.prepared_mode
    );
    assert!(state.journal.is_some());

    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    pipeline.platform.available_modes = vec![incompatible];
    let result = state.run(&mut pipeline, "owner", request);
    assert_eq!(result["error"]["code"], "unsupported_display");
    assert!(pipeline.platform.prepared_mode.is_none());
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_none());
}

#[test]
fn inactive_display_rejects_highest_before_prepare_and_journal() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    pipeline.platform.inactive = true;
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action":"apply", "configuration":configuration(),
            "mode_policy":{
                "display_id":"monitor.one",
                "context":{
                    "resolution_is_automatic":false,
                    "refresh_rate_is_automatic":false,
                    "client_display_mode_override":false,
                    "optimize_game_settings":true
                },
                "remapping":{}, "refresh_rate_option":"prefer_highest"
            }
        }),
    );
    assert_eq!(result["error"]["code"], "unsupported_display");
    assert_eq!(pipeline.platform.mode_queries, 1);
    assert!(pipeline.platform.prepared_mode.is_none());
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_none());
}
#[test]
fn disabled_optimization_keeps_concrete_resolution() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({
            "action":"apply",
            "configuration":configuration(),
            "mode_policy":{
                "display_id":"monitor.one",
                "context":{
                    "resolution_is_automatic":false,
                    "refresh_rate_is_automatic":false,
                    "client_display_mode_override":false,
                    "optimize_game_settings":false
                },
                "remapping":{},
                "resolution_option":"manual",
                "manual_resolution":"malformed"
            }
        }),
    );
    assert_eq!(result["applied"], true);
    assert_eq!(
        pipeline.platform.applied_mode,
        Some((
            1280,
            720,
            Rational {
                numerator: 60,
                denominator: 1
            }
        ))
    );
}
#[test]
fn failed_persistence_or_cancelled_ack_cannot_change_displays() {
    for state in [
        State {
            fail_save: true,
            ..Default::default()
        },
        State {
            cancel_ack: true,
            ..Default::default()
        },
        State {
            deny_guard: true,
            ..Default::default()
        },
    ] {
        let mut state = state;
        let mut pipeline = Pipeline::new(FakePlatform::default(), true);
        let response = state.run(
            &mut pipeline,
            "owner",
            json!({"action":"apply","configuration":configuration()}),
        );
        assert_eq!(response["ok"], false);
        assert!(pipeline.platform.effects.is_empty());
    }
}
#[test]
fn restart_exposes_owner_recovery_and_failed_clear_keeps_lease() {
    let mut state = State::default();
    let applied = state.run(
        &mut Pipeline::new(FakePlatform::default(), true),
        "owner",
        json!({"action":"apply","configuration":configuration()}),
    );
    let mut restarted = Pipeline::new(FakePlatform::default(), true);
    let status = state.run(&mut restarted, "owner", json!({"action":"status"}));
    assert_eq!(status["state"], "recovery_required");
    assert_eq!(status["lease"], applied["lease"]);
    state.fail_clear = true;
    let failure = state.run(
        &mut restarted,
        "owner",
        json!({"action":"recover","lease":applied["lease"]}),
    );
    assert_eq!(failure["ok"], false);
    assert!(state.journal.is_some());
    state.fail_clear = false;
    let result = state.run(
        &mut restarted,
        "owner",
        json!({"action":"recover","lease":applied["lease"]}),
    );
    assert_eq!(result["restored"], true);
    assert!(state.journal.is_none());
    assert_eq!(restarted.platform.effects, ["restore", "restore"]);
}
#[test]
fn partial_native_failure_rolls_back_and_failed_rollback_retains_recovery() {
    for restore_failure in [false, true] {
        let mut pipeline = Pipeline::new(
            FakePlatform {
                apply_failure: true,
                restore_failure,
                ..Default::default()
            },
            true,
        );
        let mut state = State::default();
        let reply = state.run(
            &mut pipeline,
            "owner",
            json!({"action":"apply","configuration":configuration()}),
        );
        assert_eq!(reply["ok"], false);
        assert_eq!(pipeline.platform.effects, ["apply", "restore"]);
        assert_eq!(state.journal.is_some(), restore_failure);
        if restore_failure {
            assert_eq!(reply["recovery_required"], true);
        }
    }
}
#[test]
fn external_changes_before_apply_are_not_rolled_back() {
    let mut pipeline = Pipeline::new(
        FakePlatform {
            topology_changed: true,
            ..Default::default()
        },
        true,
    );
    let mut state = State::default();
    let reply = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"apply","configuration":configuration()}),
    );
    assert_eq!(reply["error"]["code"], "topology_changed");
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_none());
}
#[test]
fn startup_policy_and_request_validation_prevent_effects() {
    let mut pipeline = Pipeline::new(FakePlatform::default(), false);
    let mut state = State::default();
    let reply = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"apply","configuration":configuration()}),
    );
    assert_eq!(reply["error"]["code"], "mutation_disabled");
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_none());
    assert!(
        serde_json::from_value::<Operation>(
            json!({"action":"recover","lease":"any","owner":"other"})
        )
        .is_err()
    );
    let mut invalid = configuration();
    invalid["displays"][0]["refresh"]["denominator"] = 0.into();
    assert!(
        serde_json::from_value::<Configuration>(invalid)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut overlap = configuration();
    let mut second = overlap["displays"][0].clone();
    second["id"] = "monitor.two".into();
    second["x"] = 100.into();
    overlap["displays"].as_array_mut().unwrap().push(second);
    assert!(
        serde_json::from_value::<Configuration>(overlap)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn expired_native_boundary_keeps_recovery_without_effects() {
    let journal = Journal {
        normal_active: true,
        scoped_targets: Vec::new(),
        scoped_pending: None,
        scoped_origin_shift: (0, 0),
        owner: "owner".into(),
        lease: "display-0000000000000001".into(),
        snapshot: RecoveryBaseline::Ccd(snapshot()),
        operation_id: None,
        request_identity: None,
        requested_configuration: None,
        expected_topology_revision: None,
        pending_route: None,
    };
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    pipeline.pending = Some(Pending {
        context: Context {
            verified_scoped_target: None,
            inherited_scoped: Vec::new(),
            request: 7,
            caller: "owner".into(),
            deadline: Instant::now(),
        },
        state_request: 20,
        stage: Stage::Armed {
            journal,
            configuration: serde_json::from_value(configuration()).unwrap(),
            native_target: None,
        },
    });
    let mut completed = false;
    pipeline
        .tick(|command| {
            Ok(match command {
                Command::TakeResult { .. } => Response::Result {
                    payload: json!({"ok":true,"revision":1}),
                },
                Command::Complete { payload, .. } => {
                    assert_eq!(payload["error"]["code"], "deadline_exceeded");
                    completed = true;
                    Response::error("request_ended", "expired")
                }
                Command::Poll {} => Response::Pending,
                other => panic!(
                    "expired request must not authorize a mutation or clear its journal: {other:?}"
                ),
            })
        })
        .unwrap();
    assert!(completed);
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn invalid_journals_and_stale_leases_are_never_overwritten() {
    let mut state = State {
        journal: Some(
            json!({"owner":"owner","lease":"lease","snapshot":{"format_version":2,"paths":[]}}),
        ),
        ..Default::default()
    };
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"apply","configuration":configuration()}),
    );
    assert_eq!(result["error"]["code"], "invalid_snapshot");
    assert!(state.journal.is_some());
    assert!(pipeline.platform.effects.is_empty());
    state.journal = Some(
        serde_json::to_value(Journal {
            normal_active: true,
            scoped_targets: Vec::new(),
            scoped_pending: None,
            scoped_origin_shift: (0, 0),
            owner: "owner".into(),
            lease: "current".into(),
            snapshot: RecoveryBaseline::Ccd(snapshot()),
            operation_id: None,
            request_identity: None,
            requested_configuration: None,
            expected_topology_revision: None,
            pending_route: None,
        })
        .unwrap(),
    );
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"recover","lease":"stale"}),
    );
    assert_eq!(result["error"]["code"], "permission_denied");
    assert!(state.journal.is_some());
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn versioned_kscreen_journal_roundtrips_without_breaking_legacy_ccd_records() {
    let legacy_value = json!({
        "owner": "owner",
        "lease": "legacy-lease",
        "snapshot": serde_json::to_value(snapshot()).unwrap()
    });
    let legacy: Journal = serde_json::from_value(legacy_value.clone()).unwrap();
    assert!(matches!(&legacy.snapshot, RecoveryBaseline::Ccd(_)));
    legacy.validate().unwrap();
    assert_eq!(serde_json::to_value(&legacy).unwrap(), legacy_value);

    let journal = Journal {
        normal_active: true,
        scoped_targets: Vec::new(),
        scoped_pending: None,
        scoped_origin_shift: (0, 0),
        owner: "owner".into(),
        lease: "kscreen-lease".into(),
        snapshot: RecoveryBaseline::KScreen(kscreen_snapshot()),
        operation_id: None,
        requested_configuration: None,
        request_identity: None,
        expected_topology_revision: None,
        pending_route: None,
    };
    journal.validate().unwrap();
    let encoded = serde_json::to_value(&journal).unwrap();
    let bytes = serde_json::to_vec(&journal).unwrap();
    assert!(bytes.len() <= 12_000);
    assert!(encoded.get("kscreen").is_none());
    assert!(encoded["snapshot"].get("format_version").is_none());
    assert_eq!(encoded["snapshot"]["version"], 1);
    let decoded: Journal = serde_json::from_value(encoded.clone()).unwrap();
    decoded.validate().unwrap();
    match decoded.snapshot {
        RecoveryBaseline::KScreen(snapshot) => {
            assert_eq!(snapshot.outputs[0].mode.as_ref().unwrap().id, "25")
        }
        RecoveryBaseline::Ccd(_) => panic!("KScreen recovery baseline decoded as CCD"),
    }

    let mut unknown_version = encoded.clone();
    unknown_version["snapshot"]["version"] = 2.into();
    let unknown_version: Journal = serde_json::from_value(unknown_version).unwrap();
    assert_eq!(
        unknown_version.validate().unwrap_err().code,
        "unsupported_snapshot_version"
    );

    let mut unknown_field = encoded;
    unknown_field["snapshot"]["outputs"][0]["unexpected"] = true.into();
    assert!(serde_json::from_value::<Journal>(unknown_field).is_err());
}

#[test]
fn kscreen_snapshot_rejects_oversized_output_lists_before_journal_write() {
    let mut snapshot = kscreen_snapshot();
    while snapshot.outputs.len() <= 64 {
        let mut output = snapshot.outputs[1].clone();
        output.id = format!("DP-{}", snapshot.outputs.len() + 1);
        output.priority = 0;
        output.primary = false;
        snapshot.outputs.push(output);
    }
    assert_eq!(snapshot.validate().unwrap_err().code, "resource_exhausted");
}

#[test]
fn kscreen_snapshot_rejects_payloads_over_the_journal_byte_limit() {
    let mut snapshot = kscreen_snapshot();
    for index in 0..62 {
        snapshot.outputs.push(KScreenOutputSnapshot {
            id: format!("DISPLAY-{}-{index}", "x".repeat(105)),
            connected: false,
            enabled: false,
            mode: None,
            position: KScreenPosition { x: 0, y: 0 },
            scale: 1.0,
            rotation: 1,
            priority: 0,
            primary: false,
            hdr_enabled: None,
        });
    }
    assert_eq!(snapshot.validate().unwrap_err().code, "resource_exhausted");
}

#[test]
fn kscreen_readback_matching_is_identity_and_state_exact_but_order_independent() {
    let baseline = kscreen_snapshot();
    let mut restored = baseline.clone();
    restored.outputs.reverse();
    assert!(baseline.matches_restoration(&restored));

    let mut drifted = baseline.clone();
    drifted.outputs[0].scale = 1.75;
    assert!(!baseline.matches_restoration(&drifted));

    let mut disconnected_baseline = baseline.clone();
    disconnected_baseline.outputs.push(KScreenOutputSnapshot {
        id: "DP-3".into(),
        connected: false,
        enabled: false,
        mode: None,
        position: KScreenPosition { x: 0, y: 0 },
        scale: 1.0,
        rotation: 1,
        priority: 0,
        primary: false,
        hdr_enabled: None,
    });
    assert!(disconnected_baseline.matches_restoration(&baseline));

    let mut hotplugged = baseline.clone();
    hotplugged.outputs.push(KScreenOutputSnapshot {
        id: "DP-3".into(),
        connected: true,
        enabled: true,
        mode: Some(KScreenModeSnapshot {
            id: "1920x1080@60".into(),
            width: 1920,
            height: 1080,
            refresh_millihz: 60_000,
        }),
        position: KScreenPosition { x: 4480, y: 0 },
        scale: 1.0,
        rotation: 1,
        priority: 3,
        primary: false,
        hdr_enabled: Some(false),
    });
    assert!(!baseline.matches_restoration(&hotplugged));
}

#[test]
fn kscreen_baseline_is_persisted_and_apply_retry_is_idempotent() {
    let request = json!({
        "action":"apply",
        "configuration":kscreen_configuration(),
        "operation_id":"kscreen-lease-operation"
    });
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            kscreen_baseline: Some(kscreen_snapshot()),
            ..Default::default()
        },
        true,
    );
    let applied = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(applied["applied"], true, "{applied}");
    let journal: Journal = serde_json::from_value(state.journal.clone().unwrap()).unwrap();
    assert!(matches!(
        journal.snapshot,
        RecoveryBaseline::KScreen(ref snapshot) if *snapshot == kscreen_snapshot()
    ));
    assert_eq!(pipeline.platform.kscreen_capture_count, 4);
    assert_eq!(pipeline.platform.effects, ["apply_kscreen"]);

    let retry = state.run(&mut pipeline, "owner", request);
    assert_eq!(retry["applied"], false, "{retry}");
    assert_eq!(retry["lease"], applied["lease"]);
    assert_eq!(pipeline.platform.effects, ["apply_kscreen"]);
}

#[test]
fn kscreen_topology_drift_before_journal_arm_has_no_native_effects() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            kscreen_baseline: Some(kscreen_snapshot()),
            kscreen_drift_on_second_capture: true,
            ..Default::default()
        },
        true,
    );
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"apply","configuration":kscreen_configuration()}),
    );
    assert_eq!(result["error"]["code"], "topology_changed");
    assert!(pipeline.platform.effects.is_empty());
    assert!(state.journal.is_none());
}

#[test]
fn kscreen_apply_rejects_added_outputs_and_failed_journal_cas_before_effects() {
    let mut configuration = kscreen_configuration();
    configuration["displays"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id":"DP-3", "width":1920, "height":1080,
            "refresh":{"numerator":60,"denominator":1}, "x":3200, "y":0
        }));
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            kscreen_baseline: Some(kscreen_snapshot()),
            ..Default::default()
        },
        true,
    );
    let added = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"apply","configuration":configuration}),
    );
    assert_eq!(added["error"]["code"], "unsupported_topology");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());

    let mut failed_state = State {
        fail_save: true,
        ..Default::default()
    };
    let mut failed_pipeline = Pipeline::new(
        FakePlatform {
            kscreen_baseline: Some(kscreen_snapshot()),
            ..Default::default()
        },
        true,
    );
    let not_armed = failed_state.run(
        &mut failed_pipeline,
        "owner",
        json!({"action":"apply","configuration":kscreen_configuration()}),
    );
    assert_eq!(not_armed["ok"], false);
    assert!(failed_pipeline.platform.effects.is_empty());
}

#[test]
fn partial_kscreen_apply_rolls_back_from_journal_or_retains_recovery() {
    for kscreen_restore_failure in [false, true] {
        let mut state = State::default();
        let mut pipeline = Pipeline::new(
            FakePlatform {
                kscreen_baseline: Some(kscreen_snapshot()),
                apply_failure: true,
                kscreen_restore_failure,
                ..Default::default()
            },
            true,
        );
        let result = state.run(
            &mut pipeline,
            "owner",
            json!({"action":"apply","configuration":kscreen_configuration()}),
        );
        assert_eq!(result["ok"], false);
        if kscreen_restore_failure {
            assert_eq!(result["recovery_required"], true);
        }
        assert_eq!(
            pipeline.platform.effects,
            ["apply_kscreen", "restore_kscreen"]
        );
        assert_eq!(state.journal.is_some(), kscreen_restore_failure);
    }
}

#[test]
fn kscreen_apply_readback_mismatch_rolls_back_and_clears_only_after_exact_restore() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(
        FakePlatform {
            kscreen_baseline: Some(kscreen_snapshot()),
            kscreen_apply_readback_failure: true,
            ..Default::default()
        },
        true,
    );
    let result = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"apply","configuration":kscreen_configuration()}),
    );
    assert_eq!(result["error"]["code"], "verification_failed");
    assert_eq!(result.get("recovery_required"), None);
    assert!(state.journal.is_none());
    assert_eq!(
        pipeline.platform.effects,
        ["apply_kscreen", "restore_kscreen"]
    );
}

#[test]
fn interrupted_kscreen_lease_holds_on_hotplug_and_readback_failure() {
    let journal = Journal {
        normal_active: true,
        scoped_targets: Vec::new(),
        scoped_pending: None,
        scoped_origin_shift: (0, 0),
        owner: "owner".into(),
        lease: "display-kscreen-interrupted".into(),
        snapshot: RecoveryBaseline::KScreen(kscreen_snapshot()),
        operation_id: Some("kscreen-interrupted-operation".into()),
        request_identity: None,
        requested_configuration: None,
        expected_topology_revision: None,
        pending_route: None,
    };
    let lease = journal.lease.clone();
    let mut state = State {
        // This is the durable state left by a process interruption after
        // journal acknowledgement and before a native apply completed.
        journal: Some(serde_json::to_value(journal).unwrap()),
        ..Default::default()
    };
    let mut restarted = Pipeline::new(
        FakePlatform {
            kscreen_restore_blocked: true,
            ..Default::default()
        },
        true,
    );
    let hotplug_hold = state.run(
        &mut restarted,
        "owner",
        json!({"action":"recover","lease":lease}),
    );
    assert_eq!(hotplug_hold["recovery_required"], true);
    assert!(state.journal.is_some());
    assert!(restarted.platform.effects.is_empty());

    restarted.platform.kscreen_restore_blocked = false;
    restarted.platform.kscreen_restore_readback_failure = true;
    let readback_hold = state.run(
        &mut restarted,
        "owner",
        json!({"action":"recover","lease":lease}),
    );
    assert_eq!(readback_hold["recovery_required"], true);
    assert!(state.journal.is_some());
    assert_eq!(restarted.platform.effects, ["restore_kscreen"]);

    restarted.platform.kscreen_restore_readback_failure = false;
    let recovered = state.run(
        &mut restarted,
        "owner",
        json!({"action":"recover","lease":lease}),
    );
    assert_eq!(recovered["restored"], true);
    assert!(state.journal.is_none());
    assert_eq!(
        restarted.platform.effects,
        ["restore_kscreen", "restore_kscreen"]
    );
    let duplicate = state.run(
        &mut restarted,
        "owner",
        json!({"action":"recover","lease":lease}),
    );
    assert_eq!(duplicate["error"]["code"], "not_found");
    assert_eq!(restarted.platform.kscreen_restore_calls, 3);
}

#[test]
fn physical_domain_tracks_durable_lease_revision_and_rejects_stale_plans() {
    let mut state = State::default();
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    let status = state.run(&mut pipeline, "owner", json!({"action":"status"}));
    let domain = &status["mutation_domain"];
    assert!(
        domain["id"]
            .as_str()
            .unwrap()
            .starts_with("physical_provider_session:")
    );
    assert_eq!(domain["revision"], 0);
    assert_eq!(domain["active_leases"], 0);
    assert_eq!(domain["scope"], "provider_lease_only");
    assert_eq!(domain["external_borrowers_known"], false);
    let mut request = json!({"action":"apply","configuration":configuration(),"operation_id":"domain-1","expected_mutation_domain_id":domain["id"],"expected_mutation_domain_revision":domain["revision"]});
    let applied = state.run(&mut pipeline, "owner", request.clone());
    assert_eq!(applied["applied"], true);
    let during = state.run(&mut pipeline, "foreign", json!({"action":"status"}));
    assert_eq!(during["mutation_domain"]["active_leases"], 1);
    assert_eq!(during["mutation_domain"]["revision"], 1);
    assert!(during["lease"].is_null());
    assert_eq!(
        state.run(&mut pipeline, "owner", request.clone())["state"],
        "recovery_required"
    );
    assert_eq!(
        state.run(
            &mut pipeline,
            "owner",
            json!({"action":"restore","lease":applied["lease"]})
        )["restored"],
        true
    );
    request["operation_id"] = "domain-2".into();
    assert_eq!(
        state.run(&mut pipeline, "owner", request.clone())["error"]["code"],
        "mutation_domain_changed"
    );
    request["expected_mutation_domain_revision"] = json!(2);
    let restarted = &mut Pipeline::new(FakePlatform::default(), true);
    assert_eq!(
        state.run(restarted, "owner", request)["error"]["code"],
        "mutation_domain_changed"
    );
}

mod scoped;
mod targets;
