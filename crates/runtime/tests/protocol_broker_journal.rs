//! Real broker plus the built-in durable State store. Pairing and authenticated
//! ingress are deliberately absent; this does not establish their integration.
use amaterasu_contracts::{
    ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Command, Grants, Response,
    RuntimeConfig,
    effect_fence::{EffectFenceCommand, EffectFencePolicy, EffectScope, ProtectedMethod},
};
use amaterasu_module_sdk::{
    broker_effect_fence,
    effect_authority::Source,
    extension_api::{LifecycleTarget, MethodRef},
    protocol::{DispatchRequest, ProtocolRouter, Route, SessionContext},
    protocol_broker::JournaledEffect,
    protocol_sequence::{self, IssuerRetirement, SequencedSource},
    protocol_state::{CleanupReference, DURABLE_CAS_CAPABILITY, DurableStateBinding},
    state::StateRequest,
};
use amaterasu_runtime::Broker;
use amaterasu_state::test_support::TestStateStore;
use serde_json::Value;
use std::path::PathBuf;

fn directory() -> PathBuf {
    std::env::temp_dir().join(format!(
        "protocol-broker-state-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
fn route() -> Route {
    let method = ApiMethod {
        service: "example.effects/1".into(),
        method: "receive".into(),
        interface_major: 1,
        interface_minor: 0,
        request: ApiSchema {
            required: [
                ("context".into(), ApiFieldKind::Object),
                ("capability".into(), ApiFieldKind::String),
                ("message_type".into(), ApiFieldKind::Integer),
                ("payload_base64".into(), ApiFieldKind::String),
            ]
            .into(),
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
    let reference = |name: &str| MethodRef {
        service: "example.effects/1".into(),
        method: name.into(),
        interface_major: 1,
        interface_minor: 0,
        schema_revision: 1,
    };
    let mut cleanup = method.clone();
    cleanup.method = "cleanup".into();
    Route {
        message_type: 0x553a,
        capability: "example.control/1".into(),
        approval: ApiMethodApproval {
            provider: "provider".into(),
            callers: ["issuer".into()].into(),
            method,
        },
        method: reference("receive"),
        cleanup: reference("cleanup"),
        cleanup_approval: ApiMethodApproval {
            provider: "provider".into(),
            callers: ["issuer".into()].into(),
            method: cleanup,
        },
        max_payload_bytes: 64,
    }
}
fn setup_broker(route: &Route) -> Broker {
    let service = route.method.service.clone();
    let mut config = RuntimeConfig::default();
    config
        .principals
        .insert("authority".into(), Grants::default());
    config.principals.insert(
        "issuer".into(),
        Grants {
            call: [service.clone()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "provider".into(),
        Grants {
            provide: [service.clone()].into(),
            ..Default::default()
        },
    );
    config
        .api_methods
        .extend([route.approval.clone(), route.cleanup_approval.clone()]);
    config.effect_fence = Some(EffectFencePolicy {
        source_slots: false,
        authority: "authority".into(),
        issuer: "issuer".into(),
        methods: vec![ProtectedMethod {
            service: service.clone(),
            method: "receive".into(),
            interface_major: 1,
        }],
        cleanup_methods: vec![ProtectedMethod {
            service,
            method: "cleanup".into(),
            interface_major: 1,
        }],
    });
    let mut broker = Broker::new(config).unwrap();
    for method in [&route.approval.method, &route.cleanup_approval.method] {
        assert!(matches!(
            broker.dispatch(
                "provider",
                Command::RegisterApi {
                    method: method.clone()
                }
            ),
            Response::Ok
        ));
    }
    broker
}
fn ctx() -> SessionContext {
    SessionContext {
        authority_epoch: "auth.epoch".into(),
        effect_owner_epoch: "owner.epoch".into(),
        target: LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        },
        peer_ref: "peer".into(),
    }
}
fn source() -> Source {
    Source {
        version: 1,
        id: "a".repeat(32),
        generation: 1,
        host_epoch: "host".into(),
        session_id: "session".into(),
        stream_id: "stream".into(),
        stream_generation: 1,
        device: "device".into(),
        device_generation: 1,
        effect_owner_epoch: "owner.epoch".into(),
    }
}
fn scope(issuer_epoch: &str) -> EffectScope {
    EffectScope {
        authority_epoch: "auth.epoch".into(),
        issuer_epoch: issuer_epoch.into(),
        source: broker_effect_fence::source(&source()).unwrap(),
    }
}
fn command(command: EffectFenceCommand) -> Command {
    Command::EffectFence {
        command: Box::new(command),
    }
}
fn send(broker: &mut Broker, caller: &str, command: EffectFenceCommand) -> Response {
    broker.dispatch(caller, self::command(command))
}
fn open(broker: &mut Broker, scope: EffectScope, retired: u64) {
    assert!(matches!(
        send(
            broker,
            "authority",
            EffectFenceCommand::OpenSource {
                scope,
                retired_through: retired
            }
        ),
        Response::Ok
    ));
}
fn binding() -> DurableStateBinding {
    DurableStateBinding::new("authority", "state", DURABLE_CAS_CAPABILITY).unwrap()
}
fn reference() -> CleanupReference {
    CleanupReference::new("b".repeat(64)).unwrap()
}
fn invoke(store: &TestStateStore, request: &StateRequest) -> Value {
    store.handle("authority", request.payload())
}
fn request<'a>(
    context: &'a SessionContext,
    operation: &'a str,
    issuer_epoch: &'a str,
) -> DispatchRequest<'a> {
    DispatchRequest {
        context,
        message_type: 0x553a,
        payload: b"hello",
        operation_id: operation,
        issuer_epoch,
        timeout_ms: 1000,
        parent: None,
    }
}

#[test]
fn delayed_cas_is_fenced_compacted_and_rejected_after_broker_restart() {
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let route = route();
    let router = ProtocolRouter::new(vec![route.clone()]).unwrap();
    let context = ctx();
    let source = SequencedSource::new("issuer", &context).unwrap();
    let op = source.operation_id(1).unwrap();
    let effect = router
        .prepare_effect("issuer", &request(&context, &op, "issuer.epoch"))
        .unwrap();
    let read = invoke(&store, &source.read_request());
    let pending = protocol_sequence::prepare_issue(
        &binding(),
        &source,
        &read,
        effect.clone(),
        self::source(),
        reference(),
    )
    .unwrap();
    let delayed = invoke(&store, pending.request());
    assert_eq!(delayed["ok"], true);
    let unresolved = invoke(&store, &source.read_request());
    assert_eq!(
        protocol_sequence::recovery_watermark(&source, &unresolved),
        Err("reconciliation_required")
    );
    let guarded = JournaledEffect::new(
        effect.clone(),
        self::source(),
        scope("issuer.epoch"),
        route.clone(),
        1,
        b"hello",
    )
    .unwrap();
    assert!(
        JournaledEffect::new(
            effect.clone(),
            self::source(),
            scope("wrong.issuer"),
            route.clone(),
            1,
            b"hello"
        )
        .is_err()
    );
    assert!(
        JournaledEffect::new(
            effect.clone(),
            self::source(),
            scope("issuer.epoch"),
            route.clone(),
            1,
            b"different"
        )
        .is_err()
    );
    let mut broker = setup_broker(&route);
    open(&mut broker, scope("issuer.epoch"), 0);
    // A real broker fences an exact not-yet-armed request before the old issuer
    // receives State's successful CAS result.
    guarded
        .fence(|cmd| Ok(broker.dispatch("authority", cmd)))
        .unwrap();
    let proof = guarded
        .retirement(|cmd| Ok(broker.dispatch("authority", cmd)))
        .unwrap();
    let mut other_incarnation = effect.clone();
    other_incarnation.issuer_epoch = "issuer.other".into();
    assert_eq!(
        proof.verify_retired(&self::source(), &other_incarnation),
        Err("issuer_retirement_unproven")
    );
    let read = invoke(&store, &source.read_request());
    let cleanup = protocol_sequence::prepare_cleanup(
        &binding(),
        &source,
        &read,
        1,
        &effect,
        &reference(),
        &proof,
    )
    .unwrap();
    assert_eq!(invoke(&store, &cleanup)["ok"], true);
    let read = invoke(&store, &source.read_request());
    let watermark = protocol_sequence::retired_through(&source, &read).unwrap();
    assert_eq!(watermark, 1);
    assert!(matches!(
        send(
            &mut broker,
            "authority",
            EffectFenceCommand::Compact {
                scope: scope("issuer.epoch"),
                retired_through: watermark
            }
        ),
        Response::Ok
    ));
    assert_eq!(
        guarded
            .arm(|cmd| Ok(broker.dispatch("authority", cmd)))
            .unwrap_err(),
        "effect_retired"
    );
    drop(broker);
    drop(store);
    let store = TestStateStore::open(path.clone()).unwrap();
    let read = invoke(&store, &source.read_request());
    assert_eq!(
        protocol_sequence::recovery_watermark(&source, &read).unwrap(),
        1
    );
    let mut restarted = setup_broker(&route);
    // This simulates the authority's trusted recovery assertion; production
    // must prove old worker retirement and authenticate its source first.
    open(&mut restarted, scope("issuer.new"), 1);
    let mut receipt = pending.confirmed(delayed).unwrap();
    assert!(
        matches!(guarded.dispatch(&mut receipt,|cmd| Ok(restarted.dispatch("issuer",cmd))).unwrap(),Response::Error {code,..} if code == "recovery_required")
    );
    assert!(matches!(
        restarted.dispatch("provider", Command::Poll {}),
        Response::Pending
    ));
    let next_context = ctx();
    let next_source = SequencedSource::new("issuer", &next_context).unwrap();
    let read = invoke(&store, &next_source.read_request());
    assert_eq!(
        protocol_sequence::next_sequence(&next_source, &read).unwrap(),
        2
    );
    let next_op = next_source.operation_id(2).unwrap();
    let next_effect = router
        .prepare_effect("issuer", &request(&next_context, &next_op, "issuer.new"))
        .unwrap();
    let next_pending = protocol_sequence::prepare_issue(
        &binding(),
        &next_source,
        &read,
        next_effect.clone(),
        self::source(),
        reference(),
    )
    .unwrap();
    let next_persisted = invoke(&store, next_pending.request());
    let mut next_receipt = next_pending.confirmed(next_persisted).unwrap();
    let next_guarded = JournaledEffect::new(
        next_effect,
        self::source(),
        scope("issuer.new"),
        route.clone(),
        2,
        b"hello",
    )
    .unwrap();
    next_guarded
        .arm(|cmd| Ok(restarted.dispatch("authority", cmd)))
        .unwrap();
    assert!(matches!(
        next_guarded
            .dispatch(&mut next_receipt, |cmd| Ok(
                restarted.dispatch("issuer", cmd)
            ))
            .unwrap(),
        Response::Accepted { .. }
    ));
    assert!(matches!(
        restarted.dispatch("provider", Command::Poll {}),
        Response::Request { .. }
    ));
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn exact_provider_retirement_allows_bounded_reclamation() {
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let route = route();
    let router = ProtocolRouter::new(vec![route.clone()]).unwrap();
    let context = ctx();
    let source = SequencedSource::new("issuer", &context).unwrap();
    let mut broker = setup_broker(&route);
    open(&mut broker, scope("issuer.epoch"), 0);
    for sequence in 1..=600 {
        let op = source.operation_id(sequence).unwrap();
        let effect = router
            .prepare_effect("issuer", &request(&context, &op, "issuer.epoch"))
            .unwrap();
        let read = invoke(&store, &source.read_request());
        assert_eq!(
            protocol_sequence::next_sequence(&source, &read).unwrap(),
            sequence
        );
        let pending = protocol_sequence::prepare_issue(
            &binding(),
            &source,
            &read,
            effect.clone(),
            self::source(),
            reference(),
        )
        .unwrap();
        let persisted = invoke(&store, pending.request());
        let mut receipt = pending.confirmed(persisted).unwrap();
        let guarded = JournaledEffect::new(
            effect.clone(),
            self::source(),
            scope("issuer.epoch"),
            route.clone(),
            sequence,
            b"hello",
        )
        .unwrap();
        guarded
            .arm(|cmd| Ok(broker.dispatch("authority", cmd)))
            .unwrap();
        let Response::Accepted { id } = guarded
            .dispatch(&mut receipt, |cmd| Ok(broker.dispatch("issuer", cmd)))
            .unwrap()
        else {
            panic!("not admitted")
        };
        assert!(
            matches!(broker.dispatch("provider",Command::Poll {}),Response::Request {id:got,..} if got==id)
        );
        assert!(matches!(
            broker.dispatch(
                "provider",
                Command::Complete {
                    request: id,
                    payload: serde_json::json!({"ok":true})
                }
            ),
            Response::Ok
        ));
        guarded
            .fence(|cmd| Ok(broker.dispatch("authority", cmd)))
            .unwrap();
        assert_eq!(
            guarded
                .retirement(|cmd| Ok(broker.dispatch("authority", cmd)))
                .err()
                .as_deref(),
            Some("issuer_retirement_unproven")
        );
        assert!(matches!(
            send(
                &mut broker,
                "provider",
                EffectFenceCommand::ProviderRetired { request: id }
            ),
            Response::Ok
        ));
        let proof = guarded
            .retirement(|cmd| Ok(broker.dispatch("authority", cmd)))
            .unwrap();
        let read = invoke(&store, &source.read_request());
        let cleanup = protocol_sequence::prepare_cleanup(
            &binding(),
            &source,
            &read,
            sequence,
            &effect,
            &reference(),
            &proof,
        )
        .unwrap();
        assert_eq!(invoke(&store, &cleanup)["ok"], true);
        assert!(matches!(
            send(
                &mut broker,
                "authority",
                EffectFenceCommand::Compact {
                    scope: scope("issuer.epoch"),
                    retired_through: sequence
                }
            ),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch("issuer", Command::ReleaseResult { request: id }),
            Response::Ok
        ));
    }
    let read = invoke(&store, &source.read_request());
    assert_eq!(
        protocol_sequence::retired_through(&source, &read).unwrap(),
        600
    );
    assert_eq!(
        protocol_sequence::next_sequence(&source, &read).unwrap(),
        601
    );
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}
