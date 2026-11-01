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

#[test]
fn completed_source_histories_still_exhaust_the_broker_lifetime_limit() {
    let mut broker = setup();
    for index in 0..64_u64 {
        let mut source = scope();
        source.source.id = format!("{index:032x}");
        source.source.stream_id = format!("stream-{index}");
        open(&mut broker, source.clone(), 0);
        assert!(matches!(
            invoke(
                &mut broker,
                "authority",
                EffectFenceCommand::Compact {
                    scope: source,
                    retired_through: 0,
                }
            ),
            Response::Ok
        ));
    }
    let mut next = scope();
    next.source.id = format!("{:032x}", 64);
    next.source.stream_id = "stream-64".into();
    assert_eq!(
        code(invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::OpenSource {
                scope: next,
                retired_through: 0,
            }
        )),
        "resource_exhausted"
    );
}

#[test]
fn authority_cannot_verify_provider_before_arm_without_broker_support() {
    let mut broker = setup();
    let query = || Command::QueryApiApproval {
        service: "example.effects/1".into(),
        method: "apply".into(),
        interface_major: 1,
        min_minor: 0,
    };
    assert_eq!(
        code(broker.dispatch("authority", query())),
        "permission_denied"
    );
    let Response::Result { payload } = broker.dispatch("issuer", query()) else {
        panic!("issuer cannot query selected provider")
    };
    assert_eq!(payload["provider"], "provider");
    // Arm has no expected-provider field, and its response does not identify the
    // chosen provider. The authority cannot pin its selected provider here.
    let request = effect(1);
    open(&mut broker, scope(), 0);
    arm(&mut broker, &request);
    assert!(matches!(
        invoke(
            &mut broker,
            "issuer",
            EffectFenceCommand::Dispatch { effect: request }
        ),
        Response::Accepted { .. }
    ));
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Request { .. }
    ));
}
