use amaterasu_contracts::{
    ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Command, Grants, Response,
    RuntimeConfig,
    effect_fence::{
        EffectFenceCommand, EffectFencePolicy, EffectScope, EffectSource, GuardedEffect,
        ProtectedMethod,
    },
};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use std::sync::{Arc, Barrier, Mutex, mpsc};

fn configuration() -> RuntimeConfig {
    let service = "example.effects/1";
    let method = ApiMethod {
        service: service.into(),
        method: "apply".into(),
        interface_major: 1,
        interface_minor: 0,
        request: ApiSchema {
            required: [("value".into(), ApiFieldKind::Integer)].into(),
            optional: Default::default(),
            allow_extra: false,
        },
        response: ApiSchema {
            required: [("ok".into(), ApiFieldKind::Boolean)].into(),
            optional: Default::default(),
            allow_extra: false,
        },
        privilege: ApiPrivilege::User,
    };
    let mut config = RuntimeConfig::default();
    config
        .principals
        .insert("authority".into(), Grants::default());
    config.principals.insert(
        "issuer".into(),
        Grants {
            call: [service.into()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "outsider".into(),
        Grants {
            call: [service.into()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "provider".into(),
        Grants {
            provide: [service.into()].into(),
            ..Default::default()
        },
    );
    config.api_methods.push(ApiMethodApproval {
        provider: "provider".into(),
        callers: ["issuer".into()].into(),
        method,
    });
    config.effect_fence = Some(EffectFencePolicy {
        authority: "authority".into(),
        issuer: "issuer".into(),
        cleanup_methods: vec![],
        source_slots: false,
        methods: vec![ProtectedMethod {
            service: service.into(),
            method: "apply".into(),
            interface_major: 1,
        }],
    });
    config
}
fn scope() -> EffectScope {
    EffectScope {
        authority_epoch: "authority-epoch".into(),
        issuer_epoch: "issuer-epoch".into(),
        source: EffectSource {
            version: 1,
            id: "a".repeat(32),
            generation: 1,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            stream_generation: 1,
            device: "device".into(),
            device_generation: 1,
            effect_owner_epoch: "owner".into(),
        },
    }
}
fn effect(sequence: u64) -> GuardedEffect {
    GuardedEffect {
        scope: scope(),
        sequence,
        capability: "example.effect".into(),
        service: "example.effects/1".into(),
        method: "apply".into(),
        interface_major: 1,
        interface_minor: 0,
        schema_revision: 1,
        payload_sha256: "d".repeat(64),
        payload: json!({"value":sequence}),
        timeout_ms: 30_000,
        operation_id: format!("operation.{sequence}"),
        parent: None,
    }
}
fn invoke(broker: &mut Broker, caller: &str, command: EffectFenceCommand) -> Response {
    broker.dispatch(
        caller,
        Command::EffectFence {
            command: Box::new(command),
        },
    )
}
fn open(broker: &mut Broker, source: EffectScope, retired: u64) {
    assert!(matches!(
        invoke(
            broker,
            "authority",
            EffectFenceCommand::OpenSource {
                scope: source,
                retired_through: retired
            }
        ),
        Response::Ok
    ));
}
fn arm(broker: &mut Broker, effect: &GuardedEffect) {
    assert!(matches!(
        invoke(
            broker,
            "authority",
            EffectFenceCommand::Arm {
                effect: effect.clone()
            }
        ),
        Response::Ok
    ));
}
fn fence(broker: &mut Broker, effect: &GuardedEffect) {
    assert!(matches!(
        invoke(
            broker,
            "authority",
            EffectFenceCommand::Fence {
                effect: effect.clone()
            }
        ),
        Response::Ok
    ));
}
fn lookup(broker: &mut Broker, effect: &GuardedEffect) -> Value {
    let Response::Result { payload } = invoke(
        broker,
        "authority",
        EffectFenceCommand::Lookup {
            effect: effect.clone(),
        },
    ) else {
        panic!("missing status")
    };
    payload
}
fn code(response: Response) -> String {
    let Response::Error { code, .. } = response else {
        panic!("expected error: {response:?}")
    };
    code
}
fn setup() -> Broker {
    let config = configuration();
    let method = config.api_methods[0].method.clone();
    let mut broker = Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch("provider", Command::RegisterApi { method }),
        Response::Ok
    ));
    broker
}

#[test]
fn protected_call_checks_authority_exact_request_and_legacy_bypass() {
    let mut broker = setup();
    let effect = effect(1);
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::OpenSource {
                scope: scope(),
                retired_through: 0
            }
        )),
        "permission_denied"
    );
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::Dispatch {
                effect: effect.clone()
            }
        )),
        "recovery_required"
    );
    open(&mut broker, scope(), 0);
    assert_eq!(
        code(invoke(
            &mut broker,
            "outsider",
            EffectFenceCommand::Arm {
                effect: effect.clone()
            }
        )),
        "permission_denied"
    );
    arm(&mut broker, &effect);
    assert_eq!(
        code(broker.dispatch(
            "issuer",
            Command::CallApi {
                service: effect.service.clone(),
                method: effect.method.clone(),
                interface_major: 1,
                interface_minor: 0,
                payload: effect.payload.clone(),
                timeout_ms: effect.timeout_ms,
                operation_id: effect.operation_id.clone(),
                parent: None
            }
        )),
        "permission_denied"
    );
    assert_eq!(
        code(invoke(
            &mut broker,
            "outsider",
            EffectFenceCommand::Dispatch {
                effect: effect.clone()
            }
        )),
        "permission_denied"
    );
    let mut altered = effect.clone();
    altered.payload = json!({"value":9});
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::Dispatch { effect: altered }
        )),
        "effect_conflict"
    );
    let mut wrong_digest = effect.clone();
    wrong_digest.payload_sha256 = "e".repeat(64);
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::Dispatch {
                effect: wrong_digest
            }
        )),
        "effect_conflict"
    );
    let mut invalid_schema = effect.clone();
    invalid_schema.schema_revision = 0;
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::Dispatch {
                effect: invalid_schema
            }
        )),
        "invalid_request"
    );
    let Response::Accepted { id } = invoke(
        &mut broker,
        "issuer",
        EffectFenceCommand::Dispatch {
            effect: effect.clone(),
        },
    ) else {
        panic!("not admitted")
    };
    assert!(
        matches!(invoke(&mut broker, "issuer", EffectFenceCommand::Dispatch { effect:effect.clone() }), Response::Accepted { id:retry } if id == retry)
    );
    fence(&mut broker, &effect);
    assert_eq!(lookup(&mut broker, &effect)["status"], "retired");
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Pending
    ));
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::Dispatch { effect }
        )),
        "effect_fenced"
    );
}

#[test]
fn delivered_provider_work_remains_fenced_until_exact_worker_retirement() {
    let mut broker = setup();
    open(&mut broker, scope(), 0);
    let effect = effect(1);
    arm(&mut broker, &effect);
    let Response::Accepted { id } = invoke(
        &mut broker,
        "issuer",
        EffectFenceCommand::Dispatch {
            effect: effect.clone(),
        },
    ) else {
        panic!()
    };
    assert!(
        matches!(broker.dispatch("provider", Command::Poll {}), Response::Request { id:polled, .. } if id == polled)
    );
    // Even completing and releasing the ordinary result is not worker retirement.
    assert!(matches!(
        broker.dispatch(
            "provider",
            Command::Complete {
                request: id,
                payload: json!({"ok":true})
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch("issuer", Command::ReleaseResult { request: id }),
        Response::Ok
    ));
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::Dispatch {
                effect: effect.clone()
            }
        )),
        "effect_admission_expired"
    );
    fence(&mut broker, &effect);
    assert_eq!(lookup(&mut broker, &effect)["status"], "fenced");
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::Compact {
                scope: scope(),
                retired_through: 1
            }
        )),
        "effect_pending"
    );
    assert_eq!(
        code(invoke(
            &mut broker,
            "outsider",
            EffectFenceCommand::ProviderRetired { request: id }
        )),
        "permission_denied"
    );
    assert_eq!(
        code(invoke(
            &mut broker,
            "provider",
            EffectFenceCommand::ProviderRetired { request: id + 1 }
        )),
        "permission_denied"
    );
    assert!(matches!(
        invoke(
            &mut broker,
            "provider",
            EffectFenceCommand::ProviderRetired { request: id }
        ),
        Response::Ok
    ));
    assert_eq!(lookup(&mut broker, &effect)["status"], "retired");
    assert!(matches!(
        invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::Compact {
                scope: scope(),
                retired_through: 1
            }
        ),
        Response::Ok
    ));
    assert_eq!(lookup(&mut broker, &effect)["status"], "retired_sequence");
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::Dispatch { effect }
        )),
        "effect_retired"
    );
}

#[test]
fn broker_mutex_serializes_competing_fence_and_dispatch() {
    let mut broker = setup();
    open(&mut broker, scope(), 0);
    let broker = Arc::new(Mutex::new(broker));
    for sequence in 1..=100 {
        let effect = effect(sequence);
        arm(&mut broker.lock().unwrap(), &effect);
        let barrier = Arc::new(Barrier::new(3));
        let b = broker.clone();
        let gate = barrier.clone();
        let submitted = effect.clone();
        let dispatch = std::thread::spawn(move || {
            gate.wait();
            invoke(
                &mut b.lock().unwrap(),
                "issuer",
                EffectFenceCommand::Dispatch { effect: submitted },
            )
        });
        let b = broker.clone();
        let gate = barrier.clone();
        let fenced = effect.clone();
        let retire = std::thread::spawn(move || {
            gate.wait();
            invoke(
                &mut b.lock().unwrap(),
                "authority",
                EffectFenceCommand::Fence { effect: fenced },
            )
        });
        barrier.wait();
        let result = dispatch.join().unwrap();
        assert!(
            matches!(result, Response::Accepted { .. })
                || matches!(result, Response::Error { ref code, .. } if code == "effect_fenced")
        );
        assert!(matches!(retire.join().unwrap(), Response::Ok));
        let mut b = broker.lock().unwrap();
        assert!(matches!(
            b.dispatch("provider", Command::Poll {}),
            Response::Pending
        ));
        assert_eq!(lookup(&mut b, &effect)["status"], "retired");
        assert!(matches!(
            invoke(
                &mut b,
                "authority",
                EffectFenceCommand::Compact {
                    scope: scope(),
                    retired_through: sequence
                }
            ),
            Response::Ok
        ));
    }
}

#[test]
fn concurrent_worker_must_retire_after_fence() {
    let mut raw = setup();
    open(&mut raw, scope(), 0);
    let effect = effect(1);
    arm(&mut raw, &effect);
    let Response::Accepted { id } = invoke(
        &mut raw,
        "issuer",
        EffectFenceCommand::Dispatch {
            effect: effect.clone(),
        },
    ) else {
        panic!()
    };
    let broker = Arc::new(Mutex::new(raw));
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let provider = broker.clone();
    let worker = std::thread::spawn(move || {
        assert!(
            matches!(provider.lock().unwrap().dispatch("provider", Command::Poll {}), Response::Request { id:got, .. } if got == id)
        );
        started_tx.send(()).unwrap();
        finish_rx.recv().unwrap();
        invoke(
            &mut provider.lock().unwrap(),
            "provider",
            EffectFenceCommand::ProviderRetired { request: id },
        )
    });
    started_rx.recv().unwrap();
    {
        let mut b = broker.lock().unwrap();
        fence(&mut b, &effect);
        assert_eq!(lookup(&mut b, &effect)["status"], "fenced");
    }
    finish_tx.send(()).unwrap();
    assert!(matches!(worker.join().unwrap(), Response::Ok));
    assert_eq!(
        lookup(&mut broker.lock().unwrap(), &effect)["status"],
        "retired"
    );
}

#[test]
fn fence_before_arm_blocks_late_issue_and_recovery_starts_closed() {
    let mut broker = setup();
    let old = effect(1);
    open(&mut broker, scope(), 0);
    // Retirement may overtake an issuer holding a successful journal response.
    fence(&mut broker, &old);
    assert_eq!(lookup(&mut broker, &old)["status"], "retired");
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::Arm {
                effect: old.clone()
            }
        )),
        "effect_fenced"
    );
    assert!(matches!(
        invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::Compact {
                scope: scope(),
                retired_through: 1
            }
        ),
        Response::Ok
    ));
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::Arm {
                effect: old.clone()
            }
        )),
        "effect_retired"
    );
    // Restart loses volatile broker state, and cannot dispatch before the
    // authority's trusted, durable reconciliation and old-worker retirement.
    let mut restarted = setup();
    assert_eq!(
        code(invoke(
            &mut restarted,
            "issuer",
            EffectFenceCommand::Dispatch {
                effect: old.clone()
            }
        )),
        "recovery_required"
    );
    let mut wrong_owner = scope();
    wrong_owner.source.effect_owner_epoch = "other-owner".into();
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::OpenSource {
                scope: wrong_owner,
                retired_through: 0
            }
        )),
        "effect_conflict"
    );
    // Synthetic recovery: the selected authority asserts that it durably
    // reconciled through sequence 1, retired the old workers, and selected a
    // fresh issuer incarnation. Old and re-labelled calls remain closed.
    let mut recovered_scope = scope();
    recovered_scope.issuer_epoch = "fresh-issuer".into();
    open(&mut restarted, recovered_scope.clone(), 1);
    assert_eq!(
        code(invoke(
            &mut restarted,
            "issuer",
            EffectFenceCommand::Dispatch {
                effect: old.clone()
            }
        )),
        "recovery_required"
    );
    let mut relabelled = old;
    relabelled.scope = recovered_scope;
    assert_eq!(
        code(invoke(
            &mut restarted,
            "issuer",
            EffectFenceCommand::Dispatch { effect: relabelled }
        )),
        "effect_retired"
    );
}

#[test]
fn sources_and_out_of_order_retirements_remain_independent_and_bounded() {
    let mut broker = setup();
    open(&mut broker, scope(), 0);
    let mut other_scope = scope();
    other_scope.source.id = "b".repeat(32);
    other_scope.source.stream_id = "other-stream".into();
    open(&mut broker, other_scope.clone(), 0);
    let first = effect(1);
    let second = effect(2);
    let mut other = effect(1);
    other.scope = other_scope;
    arm(&mut broker, &first);
    arm(&mut broker, &second);
    arm(&mut broker, &other);
    fence(&mut broker, &second);
    fence(&mut broker, &other);
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::Compact {
                scope: scope(),
                retired_through: 2
            }
        )),
        "effect_pending"
    );
    let mut beyond = effect(33);
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::Arm {
                effect: beyond.clone()
            }
        )),
        "resource_exhausted"
    );
    fence(&mut broker, &first);
    assert!(matches!(
        invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::Compact {
                scope: scope(),
                retired_through: 2
            }
        ),
        Response::Ok
    ));
    assert_eq!(lookup(&mut broker, &other)["status"], "retired");
    // More than the window can pass through one live source without rotating
    // or fencing the unrelated source.
    for sequence in 3..=2002 {
        beyond = effect(sequence);
        arm(&mut broker, &beyond);
        fence(&mut broker, &beyond);
        assert!(matches!(
            invoke(
                &mut broker,
                "authority",
                EffectFenceCommand::Compact {
                    scope: scope(),
                    retired_through: sequence
                }
            ),
            Response::Ok
        ));
    }
    assert_eq!(lookup(&mut broker, &other)["status"], "retired");
}

#[test]
fn protected_dispatch_preserves_parent_initiator_and_grant_intersection() {
    let mut config = configuration();
    let effect_service = "example.effects/1";
    config.principals.insert(
        "client".into(),
        Grants {
            call: ["example.parent/1".into(), effect_service.into()].into(),
            ..Default::default()
        },
    );
    config
        .principals
        .get_mut("issuer")
        .unwrap()
        .provide
        .insert("example.parent/1".into());
    config.api_methods[0].callers.insert("client".into());
    let method = config.api_methods[0].method.clone();
    let mut broker = Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch("provider", Command::RegisterApi { method }),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch(
            "issuer",
            Command::Register {
                service: "example.parent/1".into()
            }
        ),
        Response::Ok
    ));
    let Response::Accepted { id: parent } = broker.dispatch(
        "client",
        Command::Request {
            service: "example.parent/1".into(),
            payload: json!({}),
            timeout_ms: 30_000,
            operation_id: "parent".into(),
            parent: None,
        },
    ) else {
        panic!()
    };
    assert!(
        matches!(broker.dispatch("issuer", Command::Poll {}), Response::Request { id, .. } if id == parent)
    );
    open(&mut broker, scope(), 0);
    let mut guarded = effect(1);
    guarded.parent = Some(parent);
    arm(&mut broker, &guarded);
    let Response::Accepted { id } = invoke(
        &mut broker,
        "issuer",
        EffectFenceCommand::Dispatch { effect: guarded },
    ) else {
        panic!()
    };
    assert!(
        matches!(broker.dispatch("provider", Command::Poll {}), Response::Request { id:actual, ref caller, ref initiator, .. } if actual == id && caller == "issuer" && initiator == "client")
    );
}

#[test]
fn protected_dispatch_rejects_unapproved_initiator_and_ended_parent() {
    let mut config = configuration();
    config.principals.insert(
        "unapproved".into(),
        Grants {
            call: ["example.parent/1".into(), "example.effects/1".into()].into(),
            ..Default::default()
        },
    );
    config
        .principals
        .get_mut("issuer")
        .unwrap()
        .provide
        .insert("example.parent/1".into());
    let method = config.api_methods[0].method.clone();
    let mut broker = Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch("provider", Command::RegisterApi { method }),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch(
            "issuer",
            Command::Register {
                service: "example.parent/1".into()
            }
        ),
        Response::Ok
    ));
    let Response::Accepted { id: parent } = broker.dispatch(
        "unapproved",
        Command::Request {
            service: "example.parent/1".into(),
            payload: json!({}),
            timeout_ms: 30_000,
            operation_id: "parent".into(),
            parent: None,
        },
    ) else {
        panic!()
    };
    assert!(
        matches!(broker.dispatch("issuer", Command::Poll {}), Response::Request { id, .. } if id == parent)
    );
    open(&mut broker, scope(), 0);
    let mut guarded = effect(1);
    guarded.parent = Some(parent);
    arm(&mut broker, &guarded);
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::Dispatch {
                effect: guarded.clone()
            }
        )),
        "permission_denied"
    );
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Pending
    ));
    assert!(matches!(
        broker.dispatch("unapproved", Command::Cancel { request: parent }),
        Response::Ok
    ));
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::Dispatch { effect: guarded }
        )),
        "permission_denied"
    );
}

#[test]
fn equal_operation_ids_on_distinct_sources_cannot_share_provider_work() {
    let mut broker = setup();
    open(&mut broker, scope(), 0);
    let mut other_scope = scope();
    other_scope.source.id = "b".repeat(32);
    other_scope.source.stream_id = "other".into();
    open(&mut broker, other_scope.clone(), 0);
    let first = effect(1);
    let mut second = first.clone();
    second.scope = other_scope;
    arm(&mut broker, &first);
    arm(&mut broker, &second);
    let Response::Accepted { id: first_id } = invoke(
        &mut broker,
        "issuer",
        EffectFenceCommand::Dispatch {
            effect: first.clone(),
        },
    ) else {
        panic!()
    };
    let Response::Accepted { id: second_id } = invoke(
        &mut broker,
        "issuer",
        EffectFenceCommand::Dispatch {
            effect: second.clone(),
        },
    ) else {
        panic!()
    };
    assert_ne!(first_id, second_id);
    assert!(
        matches!(broker.dispatch("provider", Command::Poll {}), Response::Request { id, .. } if id == first_id)
    );
    fence(&mut broker, &second);
    assert_eq!(lookup(&mut broker, &second)["status"], "retired");
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Pending
    ));
    fence(&mut broker, &first);
    assert_eq!(lookup(&mut broker, &first)["status"], "fenced");
}

#[test]
fn cancellation_and_mismatched_retirement_never_prove_delivered_work_stopped() {
    let mut broker = setup();
    open(&mut broker, scope(), 0);
    let effect = effect(1);
    arm(&mut broker, &effect);
    let Response::Accepted { id } = invoke(
        &mut broker,
        "issuer",
        EffectFenceCommand::Dispatch {
            effect: effect.clone(),
        },
    ) else {
        panic!()
    };
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Request { .. }
    ));
    assert!(matches!(
        broker.dispatch("issuer", Command::Cancel { request: id }),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch("issuer", Command::ReleaseResult { request: id }),
        Response::Ok
    ));
    fence(&mut broker, &effect);
    let mut wrong = effect.clone();
    wrong.scope.issuer_epoch = "stale-issuer".into();
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::Fence { effect: wrong }
        )),
        "recovery_required"
    );
    let mut wrong = effect.clone();
    wrong.operation_id = "other".into();
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::Fence { effect: wrong }
        )),
        "effect_conflict"
    );
    assert_eq!(lookup(&mut broker, &effect)["status"], "fenced");
    assert!(matches!(
        invoke(
            &mut broker,
            "provider",
            EffectFenceCommand::ProviderRetired { request: id }
        ),
        Response::Ok
    ));
    assert_eq!(lookup(&mut broker, &effect)["status"], "retired");
}

#[test]
fn other_methods_on_protected_service_require_explicit_cleanup_policy() {
    let mut config = configuration();
    let original = config.api_methods[0].clone();
    let mut cleanup = original.clone();
    cleanup.method.method = "cleanup".into();
    let mut unrelated = original.clone();
    unrelated.method.method = "other".into();
    config.api_methods.push(cleanup.clone());
    config.api_methods.push(unrelated.clone());
    config
        .effect_fence
        .as_mut()
        .unwrap()
        .cleanup_methods
        .push(ProtectedMethod {
            service: cleanup.method.service.clone(),
            method: cleanup.method.method.clone(),
            interface_major: 1,
        });
    let mut invalid = config.clone();
    invalid
        .effect_fence
        .as_mut()
        .unwrap()
        .cleanup_methods
        .push(ProtectedMethod {
            service: original.method.service.clone(),
            method: original.method.method.clone(),
            interface_major: 1,
        });
    assert!(Broker::new(invalid).is_err());
    let mut broker = Broker::new(config).unwrap();
    for method in [original.method, cleanup.method, unrelated.method] {
        assert!(matches!(
            broker.dispatch("provider", Command::RegisterApi { method }),
            Response::Ok
        ));
    }
    let call = |method: &str, operation: &str| Command::CallApi {
        service: "example.effects/1".into(),
        method: method.into(),
        interface_major: 1,
        interface_minor: 0,
        payload: json!({"value":1}),
        timeout_ms: 1000,
        operation_id: operation.into(),
        parent: None,
    };
    assert_eq!(
        code(broker.dispatch("issuer", call("other", "other"))),
        "permission_denied"
    );
    assert_eq!(
        code(broker.dispatch("outsider", call("cleanup", "outsider"))),
        "permission_denied"
    );
    assert!(matches!(
        broker.dispatch("issuer", call("cleanup", "cleanup")),
        Response::Accepted { .. }
    ));
}

#[test]
fn protected_provider_cannot_be_reached_through_a_raw_alias() {
    let mut config = configuration();
    config
        .principals
        .get_mut("provider")
        .unwrap()
        .provide
        .insert("example.alias/1".into());
    config
        .principals
        .get_mut("issuer")
        .unwrap()
        .call
        .insert("example.alias/1".into());
    let method = config.api_methods[0].method.clone();
    let mut broker = Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch("provider", Command::RegisterApi { method }),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch(
            "provider",
            Command::Register {
                service: "example.alias/1".into()
            }
        ),
        Response::Ok
    ));
    assert_eq!(
        code(broker.dispatch(
            "issuer",
            Command::Request {
                service: "example.alias/1".into(),
                payload: json!({}),
                timeout_ms: 1000,
                operation_id: "alias".into(),
                parent: None
            }
        )),
        "permission_denied"
    );
}

#[test]
fn same_fenced_consumer_uses_either_selected_provider() {
    for (provider, provider_result) in [("provider.a", true), ("provider.b", false)] {
        let mut config = configuration();
        let grants = config.principals.remove("provider").unwrap();
        config.principals.insert(provider.into(), grants);
        config.api_methods[0].provider = provider.into();
        let method = config.api_methods[0].method.clone();
        let mut broker = Broker::new(config).unwrap();
        assert!(matches!(
            broker.dispatch(provider, Command::RegisterApi { method }),
            Response::Ok
        ));
        open(&mut broker, scope(), 0);
        let effect = effect(1);
        arm(&mut broker, &effect);
        let Response::Accepted { id } = invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::Dispatch {
                effect: effect.clone(),
            },
        ) else {
            panic!()
        };
        assert!(
            matches!(broker.dispatch(provider, Command::Poll {}), Response::Request { id:got, .. } if got == id)
        );
        assert!(matches!(
            broker.dispatch(
                provider,
                Command::Complete {
                    request: id,
                    payload: json!({"ok":provider_result})
                }
            ),
            Response::Ok
        ));
        assert!(
            matches!(broker.dispatch("issuer", Command::TakeResult { request:id }), Response::Result { payload } if payload["ok"] == provider_result)
        );
        fence(&mut broker, &effect);
        assert_eq!(lookup(&mut broker, &effect)["status"], "fenced");
        assert!(matches!(
            invoke(
                &mut broker,
                provider,
                EffectFenceCommand::ProviderRetired { request: id }
            ),
            Response::Ok
        ));
        assert_eq!(lookup(&mut broker, &effect)["status"], "retired");
    }
}

#[test]
fn invalid_effect_policy_never_falls_back_to_unprotected_calls() {
    let mut missing_authority = configuration();
    missing_authority.principals.remove("authority");
    assert!(Broker::new(missing_authority).is_err());
    let mut same_authority_and_issuer = configuration();
    same_authority_and_issuer
        .effect_fence
        .as_mut()
        .unwrap()
        .authority = "issuer".into();
    assert!(Broker::new(same_authority_and_issuer).is_err());
    let mut same_provider_and_authority = configuration();
    same_provider_and_authority
        .effect_fence
        .as_mut()
        .unwrap()
        .authority = "provider".into();
    assert!(Broker::new(same_provider_and_authority).is_err());
    let mut missing_approval = configuration();
    missing_approval.api_methods.clear();
    assert!(Broker::new(missing_approval).is_err());
    let mut missing_issuer_grant = configuration();
    missing_issuer_grant
        .principals
        .get_mut("issuer")
        .unwrap()
        .call
        .clear();
    assert!(Broker::new(missing_issuer_grant).is_err());
}

fn slotted_setup() -> Broker {
    let mut config = configuration();
    config.effect_fence.as_mut().unwrap().source_slots = true;
    config.host_lifecycle_control = true;
    config.principals.insert(
        amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(),
        Grants::default(),
    );
    let method = config.api_methods[0].method.clone();
    let mut broker = Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch("provider", Command::RegisterApi { method }),
        Response::Ok
    ));
    broker
}
fn broker_epoch(broker: &mut Broker, principal: &str) -> String {
    let Response::Result { payload } =
        invoke(broker, principal, EffectFenceCommand::BrokerEpoch {})
    else {
        panic!("missing broker epoch")
    };
    payload["broker_epoch"].as_str().unwrap().to_owned()
}
fn recover_slots(broker: &mut Broker, epoch: &str, retired: Vec<u64>) {
    for principal in [
        "issuer",
        "provider",
        amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL,
    ] {
        assert!(matches!(
            invoke(
                broker,
                principal,
                EffectFenceCommand::RecoveryRetired {
                    broker_epoch: epoch.into()
                }
            ),
            Response::Ok
        ));
    }
    assert!(matches!(
        invoke(
            broker,
            "authority",
            EffectFenceCommand::RecoverSlots {
                broker_epoch: epoch.into(),
                retired_incarnations: retired
            }
        ),
        Response::Ok
    ));
}
fn slot_lease(broker: &mut Broker, slot: u8) -> amaterasu_contracts::effect_fence::SourceSlotLease {
    let Response::Result { payload } =
        invoke(broker, "authority", EffectFenceCommand::SlotStatus { slot })
    else {
        panic!("missing slot status")
    };
    assert_eq!(payload["ready"], true);
    serde_json::from_value(payload["lease"].clone()).unwrap()
}
fn slot_invoke(
    broker: &mut Broker,
    principal: &str,
    lease: &amaterasu_contracts::effect_fence::SourceSlotLease,
    command: EffectFenceCommand,
) -> Response {
    let command = match command {
        EffectFenceCommand::Arm { effect } => {
            let method = ProtectedMethod {
                service: effect.service.clone(),
                method: effect.method.clone(),
                interface_major: effect.interface_major,
            };
            let Response::Result { payload } = invoke(
                broker,
                "authority",
                EffectFenceCommand::SelectedMethod { method },
            ) else {
                panic!("missing selected protected method");
            };
            EffectFenceCommand::ArmSelected {
                effect,
                expected: Box::new(serde_json::from_value(payload).unwrap()),
            }
        }
        command => command,
    };
    invoke(
        broker,
        principal,
        EffectFenceCommand::SlotCommand {
            lease: lease.clone(),
            command: Box::new(command),
        },
    )
}
fn source_for(index: u64) -> EffectScope {
    let mut scope = scope();
    scope.source.id = format!("{index:032x}");
    scope.source.stream_id = format!("stream-{index}");
    scope
}
fn open_slot(
    broker: &mut Broker,
    lease: &amaterasu_contracts::effect_fence::SourceSlotLease,
    scope: &EffectScope,
) {
    assert!(matches!(
        invoke(
            broker,
            "authority",
            EffectFenceCommand::OpenSlot {
                lease: lease.clone(),
                scope: scope.clone(),
                retired_through: 0
            }
        ),
        Response::Ok
    ));
}
fn begin_close(
    broker: &mut Broker,
    lease: &amaterasu_contracts::effect_fence::SourceSlotLease,
    scope: &EffectScope,
) {
    assert!(matches!(
        invoke(
            broker,
            "authority",
            EffectFenceCommand::BeginCloseSlot {
                lease: lease.clone(),
                scope: scope.clone()
            }
        ),
        Response::Ok
    ));
}
fn retire_source_workers(
    broker: &mut Broker,
    lease: &amaterasu_contracts::effect_fence::SourceSlotLease,
    scope: &EffectScope,
) {
    for principal in [
        "issuer",
        amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL,
    ] {
        assert!(matches!(
            invoke(
                broker,
                principal,
                EffectFenceCommand::SourceRetired {
                    lease: lease.clone(),
                    scope: scope.clone()
                }
            ),
            Response::Ok
        ));
    }
}
fn finish_close(
    broker: &mut Broker,
    lease: &amaterasu_contracts::effect_fence::SourceSlotLease,
    scope: &EffectScope,
    retired: u64,
) -> Response {
    invoke(
        broker,
        "authority",
        EffectFenceCommand::FinishCloseSlot {
            lease: lease.clone(),
            scope: scope.clone(),
            retired_through: retired,
        },
    )
}

#[test]
fn completed_sources_reuse_slots_without_admitting_stale_opens_or_calls() {
    let mut broker = slotted_setup();
    let epoch = broker_epoch(&mut broker, "authority");
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::OpenSource {
                scope: scope(),
                retired_through: 0
            }
        )),
        "slot_required"
    );
    let Response::Result { payload } = invoke(
        &mut broker,
        "authority",
        EffectFenceCommand::SlotStatus { slot: 0 },
    ) else {
        panic!()
    };
    assert_eq!(payload["ready"], false);
    recover_slots(&mut broker, &epoch, vec![0; 64]);
    let mut floors = vec![0_u64; 64];
    let mut old = None;
    for index in 0..160 {
        let scope = source_for(index);
        let slot = (index % 64) as u8;
        let lease = slot_lease(&mut broker, slot);
        assert_eq!(lease.incarnation, floors[usize::from(slot)] + 1);
        open_slot(&mut broker, &lease, &scope);
        let mut guarded = effect(1);
        guarded.scope = scope.clone();
        assert!(matches!(
            slot_invoke(
                &mut broker,
                "authority",
                &lease,
                EffectFenceCommand::Arm {
                    effect: guarded.clone()
                }
            ),
            Response::Ok
        ));
        let Response::Accepted { id } = slot_invoke(
            &mut broker,
            "issuer",
            &lease,
            EffectFenceCommand::Dispatch {
                effect: guarded.clone(),
            },
        ) else {
            panic!()
        };
        assert!(
            matches!(broker.dispatch("provider", Command::Poll {}), Response::Request { id:got, .. } if got == id)
        );
        begin_close(&mut broker, &lease, &scope);
        assert_eq!(
            code(finish_close(&mut broker, &lease, &scope, 0)),
            "effect_pending"
        );
        assert_eq!(
            code(slot_invoke(
                &mut broker,
                "issuer",
                &lease,
                EffectFenceCommand::Dispatch {
                    effect: guarded.clone()
                }
            )),
            "effect_fenced"
        );
        assert!(matches!(
            broker.dispatch(
                "provider",
                Command::Complete {
                    request: id,
                    payload: json!({"ok":true})
                }
            ),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch("issuer", Command::ReleaseResult { request: id }),
            Response::Ok
        ));
        assert_eq!(
            code(invoke(
                &mut broker,
                "provider",
                EffectFenceCommand::ProviderRetired { request: id }
            )),
            "slot_required"
        );
        assert!(matches!(
            invoke(
                &mut broker,
                "provider",
                EffectFenceCommand::ProviderRetiredInEpoch {
                    broker_epoch: epoch.clone(),
                    request: id
                }
            ),
            Response::Ok
        ));
        assert!(matches!(
            slot_invoke(
                &mut broker,
                "authority",
                &lease,
                EffectFenceCommand::Compact {
                    scope: scope.clone(),
                    retired_through: 1
                }
            ),
            Response::Ok
        ));
        retire_source_workers(&mut broker, &lease, &scope);
        // The real authority must durably record this watermark before finish.
        floors[usize::from(slot)] = lease.incarnation;
        assert!(matches!(
            finish_close(&mut broker, &lease, &scope, 1),
            Response::Ok
        ));
        assert_eq!(
            code(invoke(
                &mut broker,
                "authority",
                EffectFenceCommand::OpenSlot {
                    lease: lease.clone(),
                    scope: scope.clone(),
                    retired_through: 0
                }
            )),
            "stale_source_slot"
        );
        assert_eq!(
            code(slot_invoke(
                &mut broker,
                "issuer",
                &lease,
                EffectFenceCommand::Dispatch {
                    effect: guarded.clone()
                }
            )),
            "stale_source_slot"
        );
        if index == 0 {
            old = Some((lease, scope, guarded, id));
        }
    }
    let (old_lease, old_scope, old_effect, old_request) = old.unwrap();
    let mut restarted = slotted_setup();
    let next_epoch = broker_epoch(&mut restarted, "authority");
    assert_ne!(epoch, next_epoch);
    assert_eq!(
        code(invoke(
            &mut restarted,
            "authority",
            EffectFenceCommand::RecoverSlots {
                broker_epoch: epoch.clone(),
                retired_incarnations: floors.clone()
            }
        )),
        "stale_source_slot"
    );
    assert_eq!(
        code(invoke(
            &mut restarted,
            "authority",
            EffectFenceCommand::OpenSlot {
                lease: old_lease.clone(),
                scope: old_scope.clone(),
                retired_through: 0
            }
        )),
        "stale_source_slot"
    );
    recover_slots(&mut restarted, &next_epoch, floors);
    let next = slot_lease(&mut restarted, 0);
    assert!(next.incarnation > old_lease.incarnation);
    assert_eq!(
        code(invoke(
            &mut restarted,
            "authority",
            EffectFenceCommand::OpenSlot {
                lease: old_lease.clone(),
                scope: old_scope.clone(),
                retired_through: 0
            }
        )),
        "stale_source_slot"
    );
    assert_eq!(
        code(slot_invoke(
            &mut restarted,
            "issuer",
            &old_lease,
            EffectFenceCommand::Dispatch {
                effect: old_effect.clone()
            }
        )),
        "stale_source_slot"
    );
    let new_scope = source_for(192); // different source, same deterministic slot
    open_slot(&mut restarted, &next, &new_scope);
    let mut new_effect = old_effect.clone();
    new_effect.scope = new_scope.clone();
    assert!(matches!(
        slot_invoke(
            &mut restarted,
            "authority",
            &next,
            EffectFenceCommand::Arm {
                effect: new_effect.clone()
            }
        ),
        Response::Ok
    ));
    let Response::Accepted { id } = slot_invoke(
        &mut restarted,
        "issuer",
        &next,
        EffectFenceCommand::Dispatch {
            effect: new_effect.clone(),
        },
    ) else {
        panic!()
    };
    assert_eq!(id, old_request); // IDs repeat across broker instances.
    assert!(matches!(
        restarted.dispatch("provider", Command::Poll {}),
        Response::Request { .. }
    ));
    begin_close(&mut restarted, &next, &new_scope);
    assert_eq!(
        code(invoke(
            &mut restarted,
            "provider",
            EffectFenceCommand::ProviderRetiredInEpoch {
                broker_epoch: epoch,
                request: id
            }
        )),
        "stale_source_slot"
    );
    let Response::Result { payload } = slot_invoke(
        &mut restarted,
        "authority",
        &next,
        EffectFenceCommand::Lookup { effect: new_effect },
    ) else {
        panic!()
    };
    assert_eq!(payload["status"], "fenced");
}

#[test]
fn closing_a_slot_fences_queued_work_and_waits_for_delivered_work() {
    let mut broker = slotted_setup();
    let epoch = broker_epoch(&mut broker, "provider");
    recover_slots(&mut broker, &epoch, vec![0; 64]);
    let lease = slot_lease(&mut broker, 0);
    let scope = source_for(0);
    open_slot(&mut broker, &lease, &scope);
    let mut first = effect(1);
    first.scope = scope.clone();
    let mut second = effect(2);
    second.scope = scope.clone();
    for item in [&first, &second] {
        assert!(matches!(
            slot_invoke(
                &mut broker,
                "authority",
                &lease,
                EffectFenceCommand::Arm {
                    effect: item.clone()
                }
            ),
            Response::Ok
        ));
        assert!(matches!(
            slot_invoke(
                &mut broker,
                "issuer",
                &lease,
                EffectFenceCommand::Dispatch {
                    effect: item.clone()
                }
            ),
            Response::Accepted { .. }
        ));
    }
    let Response::Request { id: delivered, .. } = broker.dispatch("provider", Command::Poll {})
    else {
        panic!()
    };
    begin_close(&mut broker, &lease, &scope);
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Pending
    ));
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::Arm {
                effect: first.clone()
            }
        )),
        "effect_fenced"
    );
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::Compact {
                scope: scope.clone(),
                retired_through: 2
            }
        )),
        "effect_pending"
    );
    assert_eq!(
        code(finish_close(&mut broker, &lease, &scope, 0)),
        "effect_pending"
    );
    assert_eq!(
        code(invoke(
            &mut broker,
            "outsider",
            EffectFenceCommand::ProviderRetiredInEpoch {
                broker_epoch: epoch.clone(),
                request: delivered
            }
        )),
        "permission_denied"
    );
    assert!(matches!(
        invoke(
            &mut broker,
            "provider",
            EffectFenceCommand::ProviderRetiredInEpoch {
                broker_epoch: epoch,
                request: delivered
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::Compact {
                scope: scope.clone(),
                retired_through: 2
            }
        ),
        Response::Ok
    ));
    retire_source_workers(&mut broker, &lease, &scope);
    assert_eq!(
        code(finish_close(&mut broker, &lease, &scope, 1)),
        "effect_pending"
    );
    assert!(matches!(
        finish_close(&mut broker, &lease, &scope, 2),
        Response::Ok
    ));
}

#[test]
fn slot_collisions_recovery_and_stale_tokens_fail_closed() {
    let mut broker = slotted_setup();
    let epoch = broker_epoch(&mut broker, "authority");
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::RecoverSlots {
                broker_epoch: epoch.clone(),
                retired_incarnations: vec![0; 64]
            }
        )),
        "permission_denied"
    );
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::RecoverSlots {
                broker_epoch: epoch.clone(),
                retired_incarnations: vec![0; 63]
            }
        )),
        "invalid_request"
    );
    recover_slots(&mut broker, &epoch, vec![0; 64]);
    let lease = slot_lease(&mut broker, 0);
    let scope = source_for(0);
    open_slot(&mut broker, &lease, &scope);
    let same_slot = source_for(64);
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::OpenSlot {
                lease: lease.clone(),
                scope: same_slot,
                retired_through: 0
            }
        )),
        "effect_conflict"
    );
    let wrong_slot = source_for(1);
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::OpenSlot {
                lease: lease.clone(),
                scope: wrong_slot,
                retired_through: 0
            }
        )),
        "stale_source_slot"
    );
    let mut wrong_lease = lease.clone();
    wrong_lease.incarnation += 1;
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::BeginCloseSlot {
                lease: wrong_lease,
                scope: scope.clone()
            }
        )),
        "stale_source_slot"
    );
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::BeginCloseSlot {
                lease: lease.clone(),
                scope: scope.clone()
            }
        )),
        "permission_denied"
    );
    begin_close(&mut broker, &lease, &scope);
    // A delayed retry of the original recovery cannot roll back live progress.
    recover_slots(&mut broker, &epoch, vec![0; 64]);
    retire_source_workers(&mut broker, &lease, &scope);
    assert!(matches!(
        finish_close(&mut broker, &lease, &scope, 0),
        Response::Ok
    ));
    recover_slots(&mut broker, &epoch, vec![0; 64]);
    assert_eq!(
        slot_lease(&mut broker, 0).incarnation,
        lease.incarnation + 1
    );
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::OpenSlot {
                lease,
                scope,
                retired_through: 0
            }
        )),
        "stale_source_slot"
    );
}

#[test]
fn concurrent_dispatch_and_source_close_are_serialized() {
    let mut raw = slotted_setup();
    let epoch = broker_epoch(&mut raw, "authority");
    recover_slots(&mut raw, &epoch, vec![0; 64]);
    let broker = Arc::new(Mutex::new(raw));
    for index in 0..100 {
        let source = source_for(index);
        let lease = slot_lease(&mut broker.lock().unwrap(), source.source.slot().unwrap());
        open_slot(&mut broker.lock().unwrap(), &lease, &source);
        let mut guarded = effect(1);
        guarded.scope = source.clone();
        assert!(matches!(
            slot_invoke(
                &mut broker.lock().unwrap(),
                "authority",
                &lease,
                EffectFenceCommand::Arm {
                    effect: guarded.clone()
                }
            ),
            Response::Ok
        ));
        let barrier = Arc::new(Barrier::new(3));
        let b = broker.clone();
        let gate = barrier.clone();
        let dispatch_lease = lease.clone();
        let dispatch_effect = guarded.clone();
        let dispatch = std::thread::spawn(move || {
            gate.wait();
            slot_invoke(
                &mut b.lock().unwrap(),
                "issuer",
                &dispatch_lease,
                EffectFenceCommand::Dispatch {
                    effect: dispatch_effect,
                },
            )
        });
        let b = broker.clone();
        let gate = barrier.clone();
        let close_lease = lease.clone();
        let close_scope = source.clone();
        let close = std::thread::spawn(move || {
            gate.wait();
            invoke(
                &mut b.lock().unwrap(),
                "authority",
                EffectFenceCommand::BeginCloseSlot {
                    lease: close_lease,
                    scope: close_scope,
                },
            )
        });
        barrier.wait();
        let result = dispatch.join().unwrap();
        assert!(
            matches!(result, Response::Accepted { .. })
                || matches!(result, Response::Error { ref code, .. } if code == "effect_fenced")
        );
        assert!(matches!(close.join().unwrap(), Response::Ok));
        let mut broker = broker.lock().unwrap();
        assert!(matches!(
            broker.dispatch("provider", Command::Poll {}),
            Response::Pending
        ));
        assert!(matches!(
            slot_invoke(
                &mut broker,
                "authority",
                &lease,
                EffectFenceCommand::Compact {
                    scope: source.clone(),
                    retired_through: 1
                }
            ),
            Response::Ok
        ));
        retire_source_workers(&mut broker, &lease, &source);
        assert!(matches!(
            finish_close(&mut broker, &lease, &source, 1),
            Response::Ok
        ));
    }
}

#[test]
fn slot_watermarks_survive_real_state_and_broker_restart() {
    use amaterasu_module_sdk::state::{StateLookup, StateMutation, StateRequest, decode_response};
    use amaterasu_state::test_support::TestStateStore;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn with_state() -> Broker {
        let mut config = configuration();
        config.effect_fence.as_mut().unwrap().source_slots = true;
        config.host_lifecycle_control = true;
        config.principals.insert(
            amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(),
            Grants::default(),
        );
        config
            .principals
            .get_mut("authority")
            .unwrap()
            .call
            .insert("amaterasu.state/1".into());
        config.principals.insert(
            "state".into(),
            Grants {
                provide: ["amaterasu.state/1".into()].into(),
                ..Default::default()
            },
        );
        let method = config.api_methods[0].method.clone();
        let mut broker = Broker::new(config).unwrap();
        assert!(matches!(
            broker.dispatch("provider", Command::RegisterApi { method }),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch(
                "state",
                Command::Register {
                    service: "amaterasu.state/1".into()
                }
            ),
            Response::Ok
        ));
        broker
    }
    fn state_request(
        broker: &mut Broker,
        store: &TestStateStore,
        request: StateRequest,
        operation: &str,
    ) -> Value {
        let Response::Accepted { id } = broker.dispatch(
            "authority",
            Command::Request {
                service: "amaterasu.state/1".into(),
                payload: request.payload(),
                timeout_ms: 30_000,
                operation_id: operation.into(),
                parent: None,
            },
        ) else {
            panic!("State admission failed")
        };
        let Response::Request {
            caller, payload, ..
        } = broker.dispatch("state", Command::Poll {})
        else {
            panic!("State request not delivered")
        };
        let response = store.handle(&caller, payload);
        assert!(matches!(
            broker.dispatch(
                "state",
                Command::Complete {
                    request: id,
                    payload: response
                }
            ),
            Response::Ok
        ));
        let Response::Result { payload } =
            broker.dispatch("authority", Command::TakeResult { request: id })
        else {
            panic!("State result missing")
        };
        assert!(matches!(
            broker.dispatch("authority", Command::ReleaseResult { request: id }),
            Response::Ok
        ));
        payload
    }
    let directory = std::env::temp_dir().join(format!(
        "broker-slots-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = TestStateStore::open(directory.clone()).unwrap();
    let mut broker = with_state();
    let epoch = broker_epoch(&mut broker, "authority");
    let initial = state_request(
        &mut broker,
        &store,
        StateRequest::Get {
            key: "slot-watermarks".into(),
        },
        "read.initial",
    );
    let initial = decode_response::<StateLookup>(initial).unwrap().unwrap();
    assert!(!initial.found);
    let mut floors = vec![0_u64; 64];
    let stored = state_request(
        &mut broker,
        &store,
        StateRequest::Set {
            key: "slot-watermarks".into(),
            value: json!({"floors":floors}),
            expected_revision: Some(initial.revision),
        },
        "write.initial",
    );
    let revision = decode_response::<StateMutation>(stored)
        .unwrap()
        .unwrap()
        .revision;
    recover_slots(&mut broker, &epoch, floors.clone());
    let lease = slot_lease(&mut broker, 0);
    let scope = source_for(0);
    // The authority durably reserves the selected incarnation before opening.
    let claim = state_request(
        &mut broker,
        &store,
        StateRequest::Set {
            key: "slot-watermarks".into(),
            value: json!({"floors":floors,"claimed_slot":0,"incarnation":lease.incarnation}),
            expected_revision: Some(revision),
        },
        "write.claim",
    );
    let revision = decode_response::<StateMutation>(claim)
        .unwrap()
        .unwrap()
        .revision;
    open_slot(&mut broker, &lease, &scope);
    let mut guarded = effect(1);
    guarded.scope = scope.clone();
    assert!(matches!(
        slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::Arm {
                effect: guarded.clone()
            }
        ),
        Response::Ok
    ));
    let Response::Accepted { id } = slot_invoke(
        &mut broker,
        "issuer",
        &lease,
        EffectFenceCommand::Dispatch {
            effect: guarded.clone(),
        },
    ) else {
        panic!()
    };
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Request { .. }
    ));
    begin_close(&mut broker, &lease, &scope);
    assert_eq!(
        code(finish_close(&mut broker, &lease, &scope, 0)),
        "effect_pending"
    );
    assert!(matches!(
        invoke(
            &mut broker,
            "provider",
            EffectFenceCommand::ProviderRetiredInEpoch {
                broker_epoch: epoch.clone(),
                request: id
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::Compact {
                scope: scope.clone(),
                retired_through: 1
            }
        ),
        Response::Ok
    ));
    retire_source_workers(&mut broker, &lease, &scope);
    floors[0] = lease.incarnation;
    // Drop the write reply, then read back the committed floor before reuse.
    let _lost_reply = state_request(
        &mut broker,
        &store,
        StateRequest::Set {
            key: "slot-watermarks".into(),
            value: json!({"floors":floors}),
            expected_revision: Some(revision),
        },
        "write.retired",
    );
    let readback = state_request(
        &mut broker,
        &store,
        StateRequest::Get {
            key: "slot-watermarks".into(),
        },
        "read.retired",
    );
    let readback = decode_response::<StateLookup>(readback).unwrap().unwrap();
    assert_eq!(readback.value["floors"][0], lease.incarnation);
    assert!(matches!(
        finish_close(&mut broker, &lease, &scope, 1),
        Response::Ok
    ));
    drop(broker);
    drop(store);
    let store = TestStateStore::open(directory.clone()).unwrap();
    let mut broker = with_state();
    let new_epoch = broker_epoch(&mut broker, "authority");
    let saved = state_request(
        &mut broker,
        &store,
        StateRequest::Get {
            key: "slot-watermarks".into(),
        },
        "read.after.restart",
    );
    let saved = decode_response::<StateLookup>(saved).unwrap().unwrap();
    let restored: Vec<u64> = serde_json::from_value(saved.value["floors"].clone()).unwrap();
    assert_eq!(restored[0], lease.incarnation);
    recover_slots(&mut broker, &new_epoch, restored.clone());
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::OpenSlot {
                lease: lease.clone(),
                scope: scope.clone(),
                retired_through: 0
            }
        )),
        "stale_source_slot"
    );
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "issuer",
            &lease,
            EffectFenceCommand::Dispatch { effect: guarded }
        )),
        "stale_source_slot"
    );
    let next = slot_lease(&mut broker, 0);
    assert_eq!(next.incarnation, lease.incarnation + 1);

    // Persist a payload-free unresolved identity, admit it, and simulate a
    // broker crash while its provider has not returned a result. The synthetic
    // provider owns no background thread; the recovery acknowledgments below
    // model the host joining the previous workers before reinitialization.
    let next_scope = source_for(64);
    let mut next_effect = effect(1);
    next_effect.scope = next_scope.clone();
    next_effect.payload = json!({"value":999987654});
    let identity = next_effect.identity("provider");
    let claim = state_request(
        &mut broker,
        &store,
        StateRequest::Set {
            key: "slot-watermarks".into(),
            value: json!({"floors":restored,"unresolved":[identity]}),
            expected_revision: Some(saved.revision),
        },
        "write.unresolved",
    );
    assert!(decode_response::<StateMutation>(claim).unwrap().is_ok());
    open_slot(&mut broker, &next, &next_scope);
    assert!(matches!(
        slot_invoke(
            &mut broker,
            "authority",
            &next,
            EffectFenceCommand::Arm {
                effect: next_effect.clone()
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        slot_invoke(
            &mut broker,
            "issuer",
            &next,
            EffectFenceCommand::Dispatch {
                effect: next_effect.clone()
            }
        ),
        Response::Accepted { .. }
    ));
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Request { .. }
    ));
    drop(broker);
    drop(store);

    let store = TestStateStore::open(directory.clone()).unwrap();
    let mut broker = with_state();
    let recovery_epoch = broker_epoch(&mut broker, "authority");
    let saved = state_request(
        &mut broker,
        &store,
        StateRequest::Get {
            key: "slot-watermarks".into(),
        },
        "read.unresolved",
    );
    let saved = decode_response::<StateLookup>(saved).unwrap().unwrap();
    assert!(!saved.value.to_string().contains("999987654"));
    let mut restored: Vec<u64> = serde_json::from_value(saved.value["floors"].clone()).unwrap();
    let identity: amaterasu_contracts::effect_fence::GuardedEffectIdentity =
        serde_json::from_value(saved.value["unresolved"][0].clone()).unwrap();
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::RecoverSlots {
                broker_epoch: recovery_epoch.clone(),
                retired_incarnations: restored.clone()
            }
        )),
        "recovery_required"
    );
    recover_slots(&mut broker, &recovery_epoch, restored.clone());
    let recovery_lease = slot_lease(&mut broker, 0);
    assert!(matches!(
        invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::OpenRecoverySlot {
                lease: recovery_lease.clone(),
                scope: next_scope.clone(),
                retired_through: 0,
                issued_through: 1
            }
        ),
        Response::Ok
    ));
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "issuer",
            &recovery_lease,
            EffectFenceCommand::Dispatch {
                effect: next_effect.clone()
            }
        )),
        "effect_fenced"
    );
    assert!(matches!(
        slot_invoke(
            &mut broker,
            "authority",
            &recovery_lease,
            EffectFenceCommand::RecoverFence {
                identity: identity.clone()
            }
        ),
        Response::Ok
    ));
    let Response::Result { payload } = slot_invoke(
        &mut broker,
        "authority",
        &recovery_lease,
        EffectFenceCommand::LookupIdentity { identity },
    ) else {
        panic!()
    };
    assert_eq!(payload["status"], "retired");
    assert!(matches!(
        slot_invoke(
            &mut broker,
            "authority",
            &recovery_lease,
            EffectFenceCommand::Compact {
                scope: next_scope.clone(),
                retired_through: 1
            }
        ),
        Response::Ok
    ));
    retire_source_workers(&mut broker, &recovery_lease, &next_scope);
    restored[0] = recovery_lease.incarnation;
    let settled = state_request(
        &mut broker,
        &store,
        StateRequest::Set {
            key: "slot-watermarks".into(),
            value: json!({"floors":restored,"unresolved":[]}),
            expected_revision: Some(saved.revision),
        },
        "write.settled",
    );
    assert!(decode_response::<StateMutation>(settled).unwrap().is_ok());
    assert!(matches!(
        finish_close(&mut broker, &recovery_lease, &next_scope, 1),
        Response::Ok
    ));
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "issuer",
            &next,
            EffectFenceCommand::Dispatch {
                effect: next_effect
            }
        )),
        "stale_source_slot"
    );
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn recovery_requires_separate_host_issuer_and_provider_retirement() {
    let mut broker = slotted_setup();
    let epoch = broker_epoch(&mut broker, "authority");
    let recover = || EffectFenceCommand::RecoverSlots {
        broker_epoch: epoch.clone(),
        retired_incarnations: vec![0; 64],
    };
    assert_eq!(
        code(invoke(&mut broker, "authority", recover())),
        "recovery_required"
    );
    assert_eq!(
        code(invoke(
            &mut broker,
            "outsider",
            EffectFenceCommand::RecoveryRetired {
                broker_epoch: epoch.clone()
            }
        )),
        "permission_denied"
    );
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::RecoveryRetired {
                broker_epoch: "old".into()
            }
        )),
        "stale_source_slot"
    );
    assert!(matches!(
        invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::RecoveryRetired {
                broker_epoch: epoch.clone()
            }
        ),
        Response::Ok
    ));
    assert_eq!(
        code(invoke(&mut broker, "authority", recover())),
        "recovery_required"
    );
    assert!(matches!(
        invoke(
            &mut broker,
            "provider",
            EffectFenceCommand::RecoveryRetired {
                broker_epoch: epoch.clone()
            }
        ),
        Response::Ok
    ));
    assert_eq!(
        code(invoke(&mut broker, "authority", recover())),
        "recovery_required"
    );
    assert!(matches!(
        invoke(
            &mut broker,
            amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            EffectFenceCommand::RecoveryRetired {
                broker_epoch: epoch.clone()
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        invoke(&mut broker, "authority", recover()),
        Response::Ok
    ));
}

#[test]
fn payload_free_recovery_never_reopens_or_reconstructs_the_request() {
    use amaterasu_contracts::effect_fence::GuardedEffectIdentity;
    let mut broker = slotted_setup();
    let epoch = broker_epoch(&mut broker, "authority");
    recover_slots(&mut broker, &epoch, vec![0; 64]);
    let lease = slot_lease(&mut broker, 0);
    let scope = source_for(0);
    assert!(matches!(
        invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::OpenRecoverySlot {
                lease: lease.clone(),
                scope: scope.clone(),
                retired_through: 0,
                issued_through: 1
            }
        ),
        Response::Ok
    ));
    retire_source_workers(&mut broker, &lease, &scope);
    // Even authenticated worker retirement cannot skip an issued journal entry.
    assert_eq!(
        code(finish_close(&mut broker, &lease, &scope, 0)),
        "effect_pending"
    );
    let mut guarded = effect(1);
    guarded.scope = scope.clone();
    guarded.payload = json!({"value":123456789});
    let identity = guarded.identity("provider");
    let mut outside_window = identity.clone();
    outside_window.sequence = 2;
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::RecoverFence {
                identity: outside_window
            }
        )),
        "invalid_request"
    );
    let encoded = serde_json::to_value(&identity).unwrap();
    assert!(encoded.get("payload").is_none());
    assert!(!encoded.to_string().contains("123456789"));
    assert_eq!(
        serde_json::from_value::<GuardedEffectIdentity>(encoded).unwrap(),
        identity
    );
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::Arm {
                effect: guarded.clone()
            }
        )),
        "effect_fenced"
    );
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "issuer",
            &lease,
            EffectFenceCommand::Dispatch {
                effect: guarded.clone()
            }
        )),
        "effect_fenced"
    );
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "issuer",
            &lease,
            EffectFenceCommand::RecoverFence {
                identity: identity.clone()
            }
        )),
        "permission_denied"
    );
    let mut wrong_provider = identity.clone();
    wrong_provider.provider = "outsider".into();
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::RecoverFence {
                identity: wrong_provider
            }
        )),
        "permission_denied"
    );
    assert!(matches!(
        slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::RecoverFence {
                identity: identity.clone()
            }
        ),
        Response::Ok
    ));
    let Response::Result { payload } = slot_invoke(
        &mut broker,
        "authority",
        &lease,
        EffectFenceCommand::LookupIdentity {
            identity: identity.clone(),
        },
    ) else {
        panic!()
    };
    assert_eq!(payload["status"], "retired");
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::Lookup {
                effect: guarded.clone()
            }
        )),
        "payload_unavailable"
    );
    let mut wrong_digest = identity;
    wrong_digest.payload_sha256 = "e".repeat(64);
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::LookupIdentity {
                identity: wrong_digest
            }
        )),
        "effect_conflict"
    );
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Pending
    ));
    assert!(matches!(
        slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::Compact {
                scope: scope.clone(),
                retired_through: 1
            }
        ),
        Response::Ok
    ));
    retire_source_workers(&mut broker, &lease, &scope);
    assert!(matches!(
        finish_close(&mut broker, &lease, &scope, 1),
        Response::Ok
    ));
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "issuer",
            &lease,
            EffectFenceCommand::Dispatch { effect: guarded }
        )),
        "stale_source_slot"
    );
}

#[test]
fn recovery_cannot_close_after_only_part_of_the_issued_journal() {
    let mut broker = slotted_setup();
    let epoch = broker_epoch(&mut broker, "authority");
    recover_slots(&mut broker, &epoch, vec![0; 64]);
    let lease = slot_lease(&mut broker, 0);
    let scope = source_for(0);
    assert!(matches!(
        invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::OpenRecoverySlot {
                lease: lease.clone(),
                scope: scope.clone(),
                retired_through: 0,
                issued_through: 2,
            }
        ),
        Response::Ok
    ));
    retire_source_workers(&mut broker, &lease, &scope);
    for sequence in 1..=2 {
        let mut guarded = effect(sequence);
        guarded.scope = scope.clone();
        assert!(matches!(
            slot_invoke(
                &mut broker,
                "authority",
                &lease,
                EffectFenceCommand::RecoverFence {
                    identity: guarded.identity("provider"),
                }
            ),
            Response::Ok
        ));
        assert!(matches!(
            slot_invoke(
                &mut broker,
                "authority",
                &lease,
                EffectFenceCommand::Compact {
                    scope: scope.clone(),
                    retired_through: sequence,
                }
            ),
            Response::Ok
        ));
        if sequence == 1 {
            assert_eq!(
                code(finish_close(&mut broker, &lease, &scope, sequence)),
                "effect_pending"
            );
        }
    }
    assert!(matches!(
        finish_close(&mut broker, &lease, &scope, 2),
        Response::Ok
    ));
}

#[test]
fn authority_binds_selected_provider_and_complete_schema_at_arm() {
    let method = ProtectedMethod {
        service: "example.effects/1".into(),
        method: "apply".into(),
        interface_major: 1,
    };
    let mut original = setup();
    let query = || EffectFenceCommand::SelectedMethod {
        method: method.clone(),
    };
    assert_eq!(
        code(invoke(&mut original, "issuer", query())),
        "permission_denied"
    );
    assert_eq!(
        code(invoke(&mut original, "outsider", query())),
        "permission_denied"
    );
    let Response::Result { payload } = invoke(&mut original, "authority", query()) else {
        panic!("authority cannot query selected provider")
    };
    let expected: amaterasu_contracts::effect_fence::SelectedEffectMethod =
        serde_json::from_value(payload).unwrap();
    assert_eq!(expected.provider, "provider");
    let mut config = configuration();
    let provider_grants = config.principals["provider"].clone();
    config
        .principals
        .insert("replacement".into(), provider_grants);
    config.api_methods[0].provider = "replacement".into();
    let selected_method = config.api_methods[0].method.clone();
    let mut replacement = Broker::new(config).unwrap();
    assert!(matches!(
        replacement.dispatch(
            "replacement",
            Command::RegisterApi {
                method: selected_method
            }
        ),
        Response::Ok
    ));
    open(&mut replacement, scope(), 0);
    let guarded = effect(1);
    let arm_selected = |expected| EffectFenceCommand::ArmSelected {
        effect: guarded.clone(),
        expected: Box::new(expected),
    };
    assert_eq!(
        code(invoke(
            &mut replacement,
            "authority",
            arm_selected(expected.clone())
        )),
        "incompatible_api"
    );
    assert_eq!(
        code(invoke(
            &mut replacement,
            "issuer",
            arm_selected(expected.clone())
        )),
        "permission_denied"
    );
    let Response::Result { payload } = invoke(&mut replacement, "authority", query()) else {
        panic!()
    };
    let selected: amaterasu_contracts::effect_fence::SelectedEffectMethod =
        serde_json::from_value(payload).unwrap();
    let mut wrong_schema = selected.clone();
    wrong_schema.method.request.allow_extra = true;
    assert_eq!(
        code(invoke(
            &mut replacement,
            "authority",
            arm_selected(wrong_schema)
        )),
        "incompatible_api"
    );
    assert!(matches!(
        invoke(&mut replacement, "authority", arm_selected(selected)),
        Response::Ok
    ));
    assert!(matches!(
        invoke(
            &mut replacement,
            "issuer",
            EffectFenceCommand::Dispatch { effect: guarded }
        ),
        Response::Accepted { .. }
    ));
    assert!(matches!(
        replacement.dispatch("replacement", Command::Poll {}),
        Response::Request { .. }
    ));
}

#[test]
fn slotted_admission_rejects_the_legacy_unbound_arm() {
    let mut broker = slotted_setup();
    let epoch = broker_epoch(&mut broker, "authority");
    recover_slots(&mut broker, &epoch, vec![0; 64]);
    let lease = slot_lease(&mut broker, 0);
    let scope = source_for(0);
    open_slot(&mut broker, &lease, &scope);
    let mut guarded = effect(1);
    guarded.scope = scope;
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::SlotCommand {
                lease: lease.clone(),
                command: Box::new(EffectFenceCommand::Arm {
                    effect: guarded.clone()
                }),
            }
        )),
        "expected_provider_required"
    );
    assert_eq!(
        code(slot_invoke(
            &mut broker,
            "issuer",
            &lease,
            EffectFenceCommand::Dispatch { effect: guarded }
        )),
        "effect_not_armed"
    );
}

#[test]
fn payload_free_live_fence_waits_for_authenticated_provider_retirement() {
    let mut broker = slotted_setup();
    let epoch = broker_epoch(&mut broker, "authority");
    recover_slots(&mut broker, &epoch, vec![0; 64]);
    let lease = slot_lease(&mut broker, 0);
    let scope = source_for(0);
    open_slot(&mut broker, &lease, &scope);
    let mut guarded = effect(1);
    guarded.scope = scope;
    assert!(matches!(
        slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::Arm {
                effect: guarded.clone()
            }
        ),
        Response::Ok
    ));
    let Response::Accepted { id } = slot_invoke(
        &mut broker,
        "issuer",
        &lease,
        EffectFenceCommand::Dispatch {
            effect: guarded.clone(),
        },
    ) else {
        panic!()
    };
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Request { .. }
    ));
    let identity = guarded.identity("provider");
    assert!(matches!(
        slot_invoke(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::RecoverFence {
                identity: identity.clone()
            }
        ),
        Response::Ok
    ));
    let Response::Result { payload } = slot_invoke(
        &mut broker,
        "authority",
        &lease,
        EffectFenceCommand::LookupIdentity {
            identity: identity.clone(),
        },
    ) else {
        panic!()
    };
    assert_eq!(payload["status"], "fenced");
    assert_eq!(
        code(invoke(
            &mut broker,
            "outsider",
            EffectFenceCommand::ProviderRetiredInEpoch {
                broker_epoch: epoch.clone(),
                request: id
            }
        )),
        "permission_denied"
    );
    assert!(matches!(
        invoke(
            &mut broker,
            "provider",
            EffectFenceCommand::ProviderRetiredInEpoch {
                broker_epoch: epoch,
                request: id
            }
        ),
        Response::Ok
    ));
    let Response::Result { payload } = slot_invoke(
        &mut broker,
        "authority",
        &lease,
        EffectFenceCommand::LookupIdentity { identity },
    ) else {
        panic!()
    };
    assert_eq!(payload["status"], "retired");
}

#[test]
fn closing_requires_host_and_issuer_confirmation_not_issuer_alone() {
    let mut broker = slotted_setup();
    let epoch = broker_epoch(&mut broker, "authority");
    recover_slots(&mut broker, &epoch, vec![0; 64]);
    let lease = slot_lease(&mut broker, 0);
    let scope = source_for(0);
    open_slot(&mut broker, &lease, &scope);
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::SourceRetired {
                lease: lease.clone(),
                scope: scope.clone()
            }
        )),
        "effect_pending"
    );
    begin_close(&mut broker, &lease, &scope);
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::SourceRetired {
                lease: lease.clone(),
                scope: scope.clone()
            }
        )),
        "permission_denied"
    );
    assert!(matches!(
        invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::SourceRetired {
                lease: lease.clone(),
                scope: scope.clone()
            }
        ),
        Response::Ok
    ));
    assert_eq!(
        code(finish_close(&mut broker, &lease, &scope, 0)),
        "effect_pending"
    );
    assert!(matches!(
        invoke(
            &mut broker,
            amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            EffectFenceCommand::SourceRetired {
                lease: lease.clone(),
                scope: scope.clone()
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        finish_close(&mut broker, &lease, &scope, 0),
        Response::Ok
    ));
    assert_eq!(
        code(invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::SourceRetired { lease, scope }
        )),
        "stale_source_slot"
    );
}

#[test]
fn slot_mode_without_host_retirement_authority_fails_startup() {
    let mut config = configuration();
    config.effect_fence.as_mut().unwrap().source_slots = true;
    assert!(Broker::new(config).is_err());
}
