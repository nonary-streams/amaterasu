use super::*;
use crate::model::{Address, DisplayPath, Rational, Signal, Snapshot, SourceMode};

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
        }],
    }
}
fn configuration() -> Value {
    json!({"displays":[{"id":"monitor.one","width":1280,"height":720,"refresh":{"numerator":60,"denominator":1},"x":0,"y":0}]})
}
#[derive(Default)]
struct FakePlatform {
    effects: Vec<&'static str>,
    apply_failure: bool,
    restore_failure: bool,
    topology_changed: bool,
}
impl Platform for FakePlatform {
    fn name(&self) -> &'static str {
        "fake_platform_test_only"
    }
    fn supported(&self) -> bool {
        true
    }
    fn enumerate(&mut self, _: Option<&str>, _: usize, _: usize) -> DisplayResult<Value> {
        Ok(json!({"ok":true,"entries":[]}))
    }
    fn prepare(&mut self, _: &Configuration) -> DisplayResult<Snapshot> {
        Ok(snapshot())
    }
    fn apply(
        &mut self,
        _: &Configuration,
        before: &Snapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        assert!(before.equivalent(&snapshot()));
        if self.topology_changed {
            return Err(DisplayError::new("topology_changed", "external change"));
        }
        guard()?;
        self.effects.push("apply");
        if self.apply_failure {
            return Err(
                DisplayError::new("platform_error", "partial native failure").after_mutation(),
            );
        }
        Ok(())
    }
    fn restore(
        &mut self,
        before: &Snapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        assert!(before.equivalent(&snapshot()));
        guard()?;
        self.effects.push("restore");
        if self.restore_failure {
            return Err(DisplayError::new("platform_error", "restore failed").after_mutation());
        }
        Ok(())
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
