use super::*;
use crate::model::{Address, DisplayPath, ModePolicy, Rational, Signal, Snapshot, SourceMode};

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
    current_snapshot: Option<Snapshot>,
    prepared_configuration: Option<Configuration>,
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
    queried_displays: Vec<String>,
    inactive: bool,
    inactive_activation_hdr: bool,
    hdr_supported: bool,
    hdr_state: bool,
    hdr_mutations: usize,
    prepared_hdr: Option<Option<bool>>,
    reorder_readback: bool,
}
impl Platform for FakePlatform {
    fn name(&self) -> &'static str {
        "fake_platform_test_only"
    }
    fn supported(&self) -> bool {
        true
    }
    fn snapshot(&mut self) -> DisplayResult<Snapshot> {
        if let Some(current) = &self.current_snapshot {
            return Ok(current.clone());
        }
        let mut current = if self.reorder_readback {
            two_display_snapshot()
        } else {
            snapshot()
        };
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
        if self.inactive {
            return Err(DisplayError::new(
                "unsupported_display",
                "display is inactive",
            ));
        }
        if display.eq_ignore_ascii_case("monitor.one") {
            Ok(self.available_modes.clone())
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
    revision: u64,
    journal: Option<Value>,
    fail_save: bool,
    fail_clear: bool,
    cancel_ack: bool,
    deny_guard: bool,
}
impl State {
    fn run(
        &mut self,
        pipeline: &mut Pipeline<FakePlatform>,
        caller: &str,
        payload: Value,
    ) -> Value {
        let mut input = Some(payload);
        let mut state_reply = None;
        let mut result = None;
        for _ in 0..16 {
            pipeline.tick(|command| {
                assert!(serde_json::to_vec(&command).unwrap().len() < 16_384);
                Ok(match command {
                    Command::Poll {} => input.take().map_or(Response::Pending, |payload| Response::Request { id:7, caller:caller.into(), initiator:"original.initiator".into(), service:SERVICE.into(), payload, remaining_ms:30_000 }),
                    Command::Request { payload, parent, .. } => {
                        assert_eq!(parent, Some(7));
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
                        Response::Accepted { id:20 }
                    }
                    Command::TakeResult { .. } => {
                        if self.cancel_ack && self.journal.is_some() { Response::error("cancelled", "parent cancelled before acknowledgement") }
                        else { Response::Result { payload:state_reply.take().unwrap() } }
                    }
                    Command::CheckRequest { request } => {
                        assert_eq!(request, 7);
                        assert!(self.journal.is_some(), "native effect requires durable journal");
                        if self.deny_guard { Response::error("request_ended", "cancelled at native boundary") } else { Response::Ok }
                    }
                    Command::Complete { payload, .. } => { result = Some(payload); Response::Ok }
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
fn ensure_active_rejects_rebuilding_untouched_native_monitor() {
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
    assert_eq!(applied["error"]["code"], "unsupported_topology");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
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
    let applied = state.run(
        &mut pipeline,
        "owner",
        mutating_option_request(
            option,
            "monitor.two",
            two_display_configuration(),
            &baseline["topology_revision"],
        ),
    );
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
    let restored = state.run(
        &mut pipeline,
        "owner",
        json!({"action":"restore","lease":applied["lease"]}),
    );
    assert_eq!(restored["restored"], true);
    assert_eq!(pipeline.platform.applied_modes.len(), 2);
}

#[test]
fn ensure_active_inactive_target_requires_native_route_preservation() {
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
    assert_eq!(applied["error"]["code"], "unsupported_topology");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn ensure_primary_rejects_inactive_target_with_monitor_left_of_primary() {
    let mut baseline = two_display_snapshot();
    baseline.paths[0].source.x = -1920;
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
    assert_eq!(applied["error"]["code"], "unsupported_topology");
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
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
fn two_mode_policies_reach_one_durable_apply_and_restore() {
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
        ["monitor.one", "monitor.two"]
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
    assert_eq!(pipeline.platform.mode_queries, 1);
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
        owner: "owner".into(),
        lease: "display-0000000000000001".into(),
        snapshot: snapshot(),
        operation_id: None,
        requested_configuration: None,
        expected_topology_revision: None,
    };
    let mut pipeline = Pipeline::new(FakePlatform::default(), true);
    pipeline.pending = Some(Pending {
        context: Context {
            request: 7,
            caller: "owner".into(),
            deadline: Instant::now(),
        },
        state_request: 20,
        stage: Stage::Armed {
            journal,
            configuration: serde_json::from_value(configuration()).unwrap(),
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
            owner: "owner".into(),
            lease: "current".into(),
            snapshot: snapshot(),
            operation_id: None,
            requested_configuration: None,
            expected_topology_revision: None,
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
