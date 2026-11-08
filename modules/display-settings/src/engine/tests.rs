use super::*;
#[derive(Clone)]
struct Fake {
    values: Vec<Observation>,
    fail: bool,
}
impl Backend for Fake {
    fn enumerate(&mut self) -> Result<Vec<Observation>> {
        Ok(self.values.clone())
    }
    fn apply(&mut self, target: &Target, patch: &Adjustment) -> Result<()> {
        if self.fail {
            return Err("injected_failure".into());
        }
        let v = self
            .values
            .iter_mut()
            .find(|v| &v.target == target)
            .unwrap();
        macro_rules! apply {
            ($f:ident) => {
                if patch.$f.is_some() {
                    v.settings.$f = patch.$f;
                }
            };
        }
        apply!(hdr);
        apply!(wide_color_gamut);
        apply!(resolution);
        apply!(refresh_millihz);
        apply!(scale);
        apply!(position);
        Ok(())
    }
}
#[derive(Default)]
struct Memory {
    saved: State,
    fail: bool,
}
impl Store for Memory {
    fn save(&mut self, state: &State) -> Result<()> {
        if self.fail {
            return Err("storage_failed".into());
        }
        self.saved = state.clone();
        Ok(())
    }
}
fn setup() -> Engine<Fake, Memory> {
    Engine::new(
        Fake {
            values: vec![Observation {
                target: Target {
                    id: "display".into(),
                    generation: "generation-1".into(),
                },
                source: "Virtual-1".into(),
                settings: Adjustment {
                    hdr: Some(amaterasu_module_sdk::display_settings::HdrMode::Enabled),
                    wide_color_gamut: Some(true),
                    resolution: Some(amaterasu_module_sdk::display_settings::Resolution {
                        width: 1920,
                        height: 1080,
                    }),
                    ..Default::default()
                },
            }],
            fail: false,
        },
        Memory::default(),
        State::default(),
        ["session".into()].into(),
        ["first".into(), "replacement".into()].into(),
    )
    .unwrap()
}
fn scope(e: &mut Engine<Fake, Memory>) -> Scope {
    serde_json::from_value(
        e.execute(
            "session",
            Operation::OpenScope {
                operation_id: "scope".into(),
                source: "Virtual-1".into(),
                consumers: vec!["first".into(), "replacement".into()],
            },
        )
        .unwrap()["scope"]
            .clone(),
    )
    .unwrap()
}
fn acquire(
    e: &mut Engine<Fake, Memory>,
    caller: &str,
    scope: Scope,
    adjustment: Adjustment,
) -> Result<Lease> {
    Ok(serde_json::from_value(
        e.execute(
            caller,
            Operation::Acquire {
                operation_id: caller.into(),
                scope,
                adjustment,
            },
        )?["lease"]
            .clone(),
    )
    .unwrap())
}
#[test]
fn both_plugins_use_same_contract_and_restore_after_last_lease() {
    let mut e = setup();
    let s = scope(&mut e);
    let patch = Adjustment::new().hdr(amaterasu_module_sdk::display_settings::HdrMode::Disabled);
    let first = acquire(&mut e, "first", s.clone(), patch.clone()).unwrap();
    let other = acquire(&mut e, "replacement", s, patch).unwrap();
    e.execute("first", Operation::Release { lease: first })
        .unwrap();
    assert_eq!(
        e.backend.values[0].settings.hdr,
        Some(amaterasu_module_sdk::display_settings::HdrMode::Disabled)
    );
    e.execute("replacement", Operation::Release { lease: other })
        .unwrap();
    assert_eq!(
        e.backend.values[0].settings.hdr,
        Some(amaterasu_module_sdk::display_settings::HdrMode::Enabled)
    );
}
#[test]
fn sdr_lease_enforces_hdr_and_gamut_and_restores_both_after_recovery() {
    use amaterasu_module_sdk::display_settings::HdrMode;
    let mut e = setup();
    let s = scope(&mut e);
    let patch = Adjustment::new()
        .hdr(HdrMode::Disabled)
        .wide_color_gamut(false);
    // HDR off alone is not the sRGB source the plugin requested.
    let hdr_only = Adjustment::new()
        .hdr(HdrMode::Disabled)
        .wide_color_gamut(true);
    assert!(!matches_patch(&hdr_only, &patch));
    let lease = acquire(&mut e, "first", s, patch.clone()).unwrap();
    assert!(matches_patch(&e.backend.values[0].settings, &patch));
    // Persisted leases retain both properties across provider restart.
    let mut restarted = Engine::new(
        e.backend,
        Memory::default(),
        e.store.saved,
        ["session".into()].into(),
        ["first".into()].into(),
    )
    .unwrap();
    restarted.backend.values[0].settings.wide_color_gamut = Some(true);
    restarted.reconcile().unwrap();
    assert!(matches_patch(&restarted.backend.values[0].settings, &patch));
    restarted
        .execute("first", Operation::Release { lease })
        .unwrap();
    assert_eq!(
        restarted.backend.values[0].settings.hdr,
        Some(HdrMode::Enabled)
    );
    assert_eq!(
        restarted.backend.values[0].settings.wide_color_gamut,
        Some(true)
    );
}

#[test]
fn hdr_only_lease_preserves_unleased_gamut() {
    use amaterasu_module_sdk::display_settings::HdrMode;
    let mut e = setup();
    let s = scope(&mut e);
    let lease = acquire(&mut e, "first", s, Adjustment::new().hdr(HdrMode::Disabled)).unwrap();
    e.backend.values[0].settings.wide_color_gamut = Some(false);
    e.execute("first", Operation::Release { lease }).unwrap();
    assert_eq!(e.backend.values[0].settings.hdr, Some(HdrMode::Enabled));
    assert_eq!(e.backend.values[0].settings.wide_color_gamut, Some(false));
}
#[test]
fn rejects_conflicts_and_ungranted_callers() {
    let mut e = setup();
    let s = scope(&mut e);
    acquire(
        &mut e,
        "first",
        s.clone(),
        Adjustment::new().hdr(amaterasu_module_sdk::display_settings::HdrMode::Disabled),
    )
    .unwrap();
    assert!(
        acquire(
            &mut e,
            "replacement",
            s.clone(),
            Adjustment::new().hdr(amaterasu_module_sdk::display_settings::HdrMode::Enabled)
        )
        .is_err()
    );
    assert!(
        acquire(
            &mut e,
            "untrusted",
            s,
            Adjustment::new().hdr(amaterasu_module_sdk::display_settings::HdrMode::Enabled)
        )
        .is_err()
    );
}
#[test]
fn failed_mutation_recovers_from_persisted_intent() {
    let mut e = setup();
    let s = scope(&mut e);
    e.backend.fail = true;
    assert!(
        acquire(
            &mut e,
            "first",
            s,
            Adjustment::new().hdr(amaterasu_module_sdk::display_settings::HdrMode::Disabled)
        )
        .is_err()
    );
    let state = e.store.saved.clone();
    e.backend.fail = false;
    let mut restarted = Engine::new(
        e.backend,
        Memory::default(),
        state,
        ["session".into()].into(),
        ["first".into()].into(),
    )
    .unwrap();
    restarted.reconcile().unwrap();
    assert_eq!(
        restarted.backend.values[0].settings.hdr,
        Some(amaterasu_module_sdk::display_settings::HdrMode::Disabled)
    );
    restarted
        .execute(
            "session",
            Operation::CloseScope {
                operation_id: "scope".into(),
            },
        )
        .unwrap();
    assert_eq!(
        restarted.backend.values[0].settings.hdr,
        Some(amaterasu_module_sdk::display_settings::HdrMode::Enabled)
    );
}
#[test]
fn storage_failure_cannot_change_native_settings() {
    let mut e = setup();
    let s = scope(&mut e);
    e.store.fail = true;
    assert!(
        acquire(
            &mut e,
            "first",
            s,
            Adjustment::new().hdr(amaterasu_module_sdk::display_settings::HdrMode::Disabled)
        )
        .is_err()
    );
    assert_eq!(
        e.backend.values[0].settings.hdr,
        Some(amaterasu_module_sdk::display_settings::HdrMode::Enabled)
    );
}
#[test]
fn enforcement_repairs_external_changes() {
    let mut e = setup();
    let s = scope(&mut e);
    acquire(
        &mut e,
        "first",
        s,
        Adjustment::new().hdr(amaterasu_module_sdk::display_settings::HdrMode::Disabled),
    )
    .unwrap();
    e.backend.values[0].settings.hdr =
        Some(amaterasu_module_sdk::display_settings::HdrMode::Enabled);
    e.reconcile().unwrap();
    assert_eq!(
        e.backend.values[0].settings.hdr,
        Some(amaterasu_module_sdk::display_settings::HdrMode::Disabled)
    );
}

#[test]
fn retrying_failed_release_must_restore_before_reporting_success() {
    let mut e = setup();
    let s = scope(&mut e);
    let lease = acquire(
        &mut e,
        "first",
        s,
        Adjustment::new().hdr(amaterasu_module_sdk::display_settings::HdrMode::Disabled),
    )
    .unwrap();
    e.backend.fail = true;
    assert!(
        e.execute(
            "first",
            Operation::Release {
                lease: lease.clone()
            }
        )
        .is_err()
    );
    assert!(
        e.execute(
            "first",
            Operation::Release {
                lease: lease.clone()
            }
        )
        .is_err()
    );
    e.backend.fail = false;
    e.execute("first", Operation::Release { lease }).unwrap();
    assert_eq!(
        e.backend.values[0].settings.hdr,
        Some(amaterasu_module_sdk::display_settings::HdrMode::Enabled)
    );
}
#[test]
fn stale_target_is_never_restored_onto_a_replacement_output() {
    let mut e = setup();
    let s = scope(&mut e);
    acquire(
        &mut e,
        "first",
        s,
        Adjustment::new().hdr(amaterasu_module_sdk::display_settings::HdrMode::Disabled),
    )
    .unwrap();
    e.backend.values[0].target.generation = "replacement".into();
    e.execute(
        "session",
        Operation::CloseScope {
            operation_id: "scope".into(),
        },
    )
    .unwrap();
    assert_eq!(
        e.backend.values[0].settings.hdr,
        Some(amaterasu_module_sdk::display_settings::HdrMode::Disabled)
    );
}

#[test]
fn retired_scope_receipts_are_bounded_and_stale_handles_never_revive() {
    let mut e = setup();
    let old = scope(&mut e);
    e.execute(
        "session",
        Operation::CloseScope {
            operation_id: "scope".into(),
        },
    )
    .unwrap();
    for n in 0..1050 {
        let op = format!("scope-{n}");
        e.execute(
            "session",
            Operation::OpenScope {
                operation_id: op.clone(),
                source: "Virtual-1".into(),
                consumers: vec!["first".into()],
            },
        )
        .unwrap();
        e.execute("session", Operation::CloseScope { operation_id: op })
            .unwrap();
    }
    assert!(e.state.closed_scopes.len() <= 1024);
    assert!(e.state.targets.is_empty());
    assert!(
        acquire(
            &mut e,
            "first",
            old,
            Adjustment::new().hdr(amaterasu_module_sdk::display_settings::HdrMode::Disabled)
        )
        .is_err()
    );
}

struct Replacement {
    display: Observation,
}
impl Backend for Replacement {
    fn enumerate(&mut self) -> Result<Vec<Observation>> {
        Ok(vec![self.display.clone()])
    }
    fn apply(&mut self, target: &Target, patch: &Adjustment) -> Result<()> {
        if self.display.target != *target {
            return Err("wrong_target".into());
        }
        if patch.resolution.is_some()
            || patch.position.is_some()
            || patch.refresh_millihz.is_some()
            || patch.scale.is_some()
        {
            return Err("unsupported_setting".into());
        }
        self.display.settings.hdr = patch.hdr.or(self.display.settings.hdr);
        Ok(())
    }
}
fn unchanged_sdk_consumer<B: Backend>(provider: &mut Engine<B, Memory>) {
    use amaterasu_module_sdk::{Command, display_settings, requests::RequestOptions};
    fn call<B: Backend>(
        provider: &mut Engine<B, Memory>,
        caller: &str,
        operation: Operation,
    ) -> Value {
        let Command::CallApi {
            method,
            interface_major,
            interface_minor,
            payload,
            ..
        } = operation
            .command(RequestOptions {
                service: display_settings::SERVICE,
                operation_id: "request",
                timeout_ms: 5000,
                parent: None,
            })
            .unwrap()
        else {
            unreachable!()
        };
        provider
            .execute(caller, display_settings::decode(json!({"method":method,"interface_major":interface_major,"interface_minor":interface_minor,"arguments":payload})).unwrap())
            .unwrap()
    }
    let response = call(
        provider,
        "session",
        Operation::OpenScope {
            operation_id: "source".into(),
            source: "Virtual-1".into(),
            consumers: vec!["first".into()],
        },
    );
    let scope: Scope = serde_json::from_value(response["scope"].clone()).unwrap();
    let response = call(
        provider,
        "first",
        Operation::Acquire {
            operation_id: "sdr".into(),
            scope: scope.clone(),
            adjustment: Adjustment::new()
                .hdr(amaterasu_module_sdk::display_settings::HdrMode::Disabled),
        },
    );
    let lease = display_settings::lease_from_reply(response, &scope).unwrap();
    assert_eq!(
        call(
            provider,
            "first",
            Operation::Read {
                target: scope.target.clone()
            }
        )["settings"]["hdr"],
        "disabled"
    );
    call(provider, "first", Operation::Release { lease });
    assert_eq!(
        call(
            provider,
            "first",
            Operation::Read {
                target: scope.target
            }
        )["settings"]["hdr"],
        "enabled"
    );
}
#[test]
fn sdk_consumer_works_unchanged_with_independent_display_providers() {
    let mut first = setup();
    let mut second = Engine::new(
        Replacement {
            display: first.backend.values[0].clone(),
        },
        Memory::default(),
        State::default(),
        ["session".into()].into(),
        ["first".into()].into(),
    )
    .unwrap();
    unchanged_sdk_consumer(&mut first);
    unchanged_sdk_consumer(&mut second);
}

#[test]
fn sdk_calls_use_real_broker_approvals_and_reject_unapproved_or_raw_calls() {
    use amaterasu_contracts::{ApiMethodApproval, Grants, RuntimeConfig};
    use amaterasu_module_sdk::{Command, Response, display_settings, requests::RequestOptions};
    let mut config = RuntimeConfig::default();
    for caller in ["session", "first", "outsider"] {
        config.principals.insert(
            caller.into(),
            Grants {
                call: [display_settings::SERVICE.into()].into(),
                ..Default::default()
            },
        );
    }
    config.principals.insert(
        "display-provider".into(),
        Grants {
            provide: [display_settings::SERVICE.into()].into(),
            ..Default::default()
        },
    );
    for method in display_settings::methods(display_settings::SERVICE) {
        config.api_methods.push(ApiMethodApproval {
            provider: "display-provider".into(),
            callers: ["session".into(), "first".into()].into(),
            method,
        });
    }
    let mut broker = amaterasu_runtime::Broker::new(config).unwrap();
    for method in display_settings::methods(display_settings::SERVICE) {
        assert!(matches!(
            broker.dispatch("display-provider", Command::RegisterApi { method }),
            Response::Ok
        ));
    }
    let mut engine = setup();
    let operation = Operation::OpenScope {
        operation_id: "wire-source".into(),
        source: "Virtual-1".into(),
        consumers: vec!["first".into()],
    };
    let command = operation
        .command(RequestOptions {
            service: display_settings::SERVICE,
            operation_id: "wire-request",
            timeout_ms: 5000,
            parent: None,
        })
        .unwrap();
    assert!(matches!(
        broker.dispatch("outsider", command.clone()),
        Response::Error { .. }
    ));
    assert!(matches!(
        broker.dispatch(
            "session",
            Command::Request {
                service: display_settings::SERVICE.into(),
                payload: json!({"action":"open_scope"}),
                operation_id: "raw".into(),
                timeout_ms: 5000,
                parent: None
            }
        ),
        Response::Error { .. }
    ));
    let Response::Accepted { id } = broker.dispatch("session", command) else {
        panic!("SDK API admission failed")
    };
    let Response::Request {
        caller, payload, ..
    } = broker.dispatch("display-provider", Command::Poll {})
    else {
        panic!("missing provider delivery")
    };
    let reply = engine
        .execute(&caller, display_settings::decode(payload).unwrap())
        .unwrap();
    assert!(matches!(
        broker.dispatch(
            "display-provider",
            Command::Complete {
                request: id,
                payload: reply
            }
        ),
        Response::Ok
    ));
    let Response::Result { payload } =
        broker.dispatch("session", Command::TakeResult { request: id })
    else {
        panic!("missing SDK result")
    };
    assert_eq!(payload["ok"], true);
    assert!(matches!(
        broker.dispatch("session", Command::ReleaseResult { request: id }),
        Response::Ok
    ));
}
