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

// The test provider has no persistent effects; production must replace this
// fixture with proof from the selected provider's actual cleanup operation.
struct TestClean<'a>(&'a amaterasu_module_sdk::protocol_broker::BrokerRetirement);
impl IssuerRetirement for TestClean<'_> {
    fn verify_retired(
        &self,
        issuer: &Source,
        effect: &amaterasu_module_sdk::protocol::PendingEffect,
    ) -> Result<(), &'static str> {
        self.0.verify_retired(issuer, effect)
    }
    fn verify_cleanup(
        &self,
        issuer: &Source,
        effect: &amaterasu_module_sdk::protocol::PendingEffect,
        cleanup: &CleanupReference,
    ) -> Result<(), &'static str> {
        self.0.verify_retired(issuer, effect)?;
        if cleanup.id == "b".repeat(64) {
            Ok(())
        } else {
            Err("cleanup_unproven")
        }
    }
}
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
    setup_broker_with_slots(route, false)
}
fn setup_broker_with_slots(route: &Route, slots: bool) -> Broker {
    let service = route.method.service.clone();
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "authority".into(),
        Grants {
            call: if slots {
                ["amaterasu.state/1".into()].into()
            } else {
                Default::default()
            },
            ..Default::default()
        },
    );
    if slots {
        config.principals.insert(
            "state".into(),
            Grants {
                provide: ["amaterasu.state/1".into()].into(),
                ..Default::default()
            },
        );
    }
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
    config.host_lifecycle_control = slots;
    if slots {
        config.principals.insert(
            amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(),
            Grants::default(),
        );
    }
    config.effect_fence = Some(EffectFencePolicy {
        source_slots: slots,
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
    if slots {
        assert!(matches!(
            broker.dispatch(
                "state",
                Command::Register {
                    service: "amaterasu.state/1".into()
                }
            ),
            Response::Ok
        ));
    }
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
        &TestClean(&proof),
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
            &TestClean(&proof),
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

#[test]
fn unresolved_digest_only_restart_stays_closed_without_retirement_evidence() {
    use amaterasu_contracts::effect_fence::GuardedEffect;
    use amaterasu_module_sdk::protocol_sequence::IssuerRetirement;
    struct NoEvidence;
    impl IssuerRetirement for NoEvidence {}

    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let route = route();
    let router = ProtocolRouter::new(vec![route.clone()]).unwrap();
    let context = ctx();
    let source = SequencedSource::new("issuer", &context).unwrap();
    let op = source.operation_id(1).unwrap();
    let effect = {
        let secret = b"private-protocol-payload".to_vec();
        let request = DispatchRequest {
            context: &context,
            message_type: 0x553a,
            payload: &secret,
            operation_id: &op,
            issuer_epoch: "issuer.epoch",
            timeout_ms: 1000,
            parent: None,
        };
        let effect = router.prepare_effect("issuer", &request).unwrap();
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
        assert_eq!(invoke(&store, pending.request())["ok"], true);
        // Simulate a crash before the response is consumed or reconciliation runs.
        drop(pending);
        effect
    }; // original payload is no longer available to recovery
    drop(store);
    let store = TestStateStore::open(path.clone()).unwrap();
    let read = invoke(&store, &source.read_request());
    assert!(!read.to_string().contains("private-protocol-payload"));
    assert!(
        !read
            .to_string()
            .contains("cHJpdmF0ZS1wcm90b2NvbC1wYXlsb2Fk")
    );
    let snapshot = protocol_sequence::recovery_snapshot(&source, &read).unwrap();
    assert_eq!(
        (
            snapshot.retired_through,
            snapshot.issued_through,
            snapshot.unresolved.len()
        ),
        (0, 1, 1)
    );
    assert_eq!(snapshot.unresolved[0].effect, effect);
    assert_eq!(
        protocol_sequence::recovery_watermark(&source, &read),
        Err("reconciliation_required")
    );
    assert!(matches!(
        protocol_sequence::prepare_cleanup(
            &binding(),
            &source,
            &read,
            1,
            &effect,
            &reference(),
            &NoEvidence
        ),
        Err("issuer_retirement_unproven")
    ));
    let mut restarted = setup_broker(&route);
    // A digest is not a payload. Even a guessed request cannot be admitted while
    // the broker has no recovered, authenticated OpenSource assertion.
    let guessed = GuardedEffect {
        scope: scope("issuer.epoch"),
        sequence: 1,
        capability: effect.capability.clone(),
        service: effect.method.service.clone(),
        method: effect.method.method.clone(),
        interface_major: effect.method.interface_major,
        interface_minor: effect.method.interface_minor,
        schema_revision: 1,
        payload_sha256: effect.payload_sha256.clone(),
        payload: serde_json::json!({}),
        timeout_ms: effect.timeout_ms,
        operation_id: effect.operation_id.clone(),
        parent: None,
    };
    assert!(
        matches!(send(&mut restarted, "issuer", EffectFenceCommand::Dispatch { effect: guessed }), Response::Error { code, .. } if code == "recovery_required")
    );
    assert!(matches!(
        restarted.dispatch("provider", Command::Poll {}),
        Response::Pending
    ));
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

fn epoch(broker: &mut Broker) -> String {
    let Response::Result { payload } =
        send(broker, "authority", EffectFenceCommand::BrokerEpoch {})
    else {
        panic!("no epoch")
    };
    payload["broker_epoch"].as_str().unwrap().into()
}
fn recover(broker: &mut Broker, epoch: &str, floors: &[u64]) {
    for participant in [
        "issuer",
        "provider",
        amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL,
    ] {
        assert!(matches!(
            send(
                broker,
                participant,
                EffectFenceCommand::RecoveryRetired {
                    broker_epoch: epoch.into()
                }
            ),
            Response::Ok
        ));
    }
    assert!(matches!(
        send(
            broker,
            "authority",
            EffectFenceCommand::RecoverSlots {
                broker_epoch: epoch.into(),
                retired_incarnations: floors.to_vec()
            }
        ),
        Response::Ok
    ));
}
fn lease(broker: &mut Broker, slot: u8) -> amaterasu_contracts::effect_fence::SourceSlotLease {
    let Response::Result { payload } =
        send(broker, "authority", EffectFenceCommand::SlotStatus { slot })
    else {
        panic!("no slot")
    };
    assert_eq!(payload["ready"], true);
    serde_json::from_value(payload["lease"].clone()).unwrap()
}
// Route actual State requests through the real broker. TestStateStore runs the
// same persisted implementation as the State provider, but this harness pumps
// its provider messages directly and does not load the production State module.
fn state_exchange(broker: &mut Broker, store: &TestStateStore, operation: &StateRequest) -> Value {
    let operation_id = amaterasu_module_sdk::protocol_state::fresh_state_operation_id().unwrap();
    state_exchange_with_id(broker, store, operation, &operation_id)
}
fn state_exchange_with_id(
    broker: &mut Broker,
    store: &TestStateStore,
    operation: &StateRequest,
    operation_id: &str,
) -> Value {
    let id = match broker.dispatch(
        "authority",
        Command::Request {
            service: "amaterasu.state/1".into(),
            payload: operation.payload(),
            timeout_ms: 30_000,
            operation_id: operation_id.into(),
            parent: None,
        },
    ) {
        Response::Accepted { id } => id,
        other => panic!("State admission: {other:?}"),
    };
    let Response::Request {
        id: polled,
        caller,
        payload,
        ..
    } = broker.dispatch("state", Command::Poll {})
    else {
        panic!("State request missing")
    };
    assert_eq!(polled, id);
    let result = store.handle(&caller, payload);
    assert!(matches!(
        broker.dispatch(
            "state",
            Command::Complete {
                request: id,
                payload: result
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
fn slot_state_write(broker: &mut Broker, store: &TestStateStore, key: &str, value: Value) {
    let read = state_exchange(broker, store, &StateRequest::Get { key: key.into() });
    let revision = read["revision"].as_u64().unwrap();
    assert_eq!(
        state_exchange(
            broker,
            store,
            &StateRequest::Set {
                key: key.into(),
                value,
                expected_revision: Some(revision)
            }
        )["ok"],
        true
    );
}
fn slot_cmd(
    lease: &amaterasu_contracts::effect_fence::SourceSlotLease,
    command: EffectFenceCommand,
) -> EffectFenceCommand {
    EffectFenceCommand::SlotCommand {
        lease: lease.clone(),
        command: Box::new(command),
    }
}
fn poll_issuer_ack(
    worker: &mut amaterasu_module_sdk::protocol_broker::IssuerWorkerRetirement,
    broker: &mut Broker,
    lose_reply: bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut lose = lose_reply;
    loop {
        let result = worker.poll(|cmd| {
            let response = broker.dispatch("issuer", cmd);
            if lose {
                lose = false;
                assert!(matches!(response, Response::Ok));
                Err("lost reply".into())
            } else {
                Ok(response)
            }
        });
        match result {
            Ok(true) => break,
            Ok(false) => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            Err(error) if error == "lost reply" => {}
            Err(error) => panic!("issuer ack: {error}"),
        }
    }
}

#[test]
fn legacy_digest_only_recovery_requires_host_incarnation_and_stays_fenced() {
    use amaterasu_module_sdk::protocol_broker::{IssuerWorkerRetirement, JournaledIdentity};
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let route = route();
    let router = ProtocolRouter::new(vec![route.clone()]).unwrap();
    let context = ctx();
    let source = SequencedSource::new("issuer", &context).unwrap();
    let old_scope = scope("issuer.epoch");
    let slot = old_scope.source.slot().unwrap();
    let mut original = setup_broker_with_slots(&route, true);
    let old_epoch = epoch(&mut original);
    recover(&mut original, &old_epoch, &[0; 64]);
    let old_lease = lease(&mut original, slot);
    slot_state_write(
        &mut original,
        &store,
        "slot-active",
        serde_json::json!({"lease":old_lease,"scope":old_scope}),
    );
    assert!(matches!(
        send(
            &mut original,
            "authority",
            EffectFenceCommand::OpenSlot {
                lease: old_lease.clone(),
                scope: old_scope.clone(),
                retired_through: 0
            }
        ),
        Response::Ok
    ));
    let op = source.operation_id(1).unwrap();
    let secret = b"private-protocol-payload".to_vec();
    let request = DispatchRequest {
        context: &context,
        message_type: 0x553a,
        payload: &secret,
        operation_id: &op,
        issuer_epoch: "issuer.epoch",
        timeout_ms: 1000,
        parent: None,
    };
    let effect = router.prepare_effect("issuer", &request).unwrap();
    let read = state_exchange(&mut original, &store, &source.read_request());
    let pending = protocol_sequence::prepare_issue(
        &binding(),
        &source,
        &read,
        effect.clone(),
        self::source(),
        reference(),
    )
    .unwrap();
    let delayed = state_exchange_with_id(
        &mut original,
        &store,
        pending.request(),
        pending.state_operation_id(),
    );
    assert_eq!(delayed["ok"], true);
    let slotted = JournaledEffect::new(
        effect.clone(),
        self::source(),
        old_scope.clone(),
        route.clone(),
        1,
        &secret,
    )
    .unwrap()
    .in_slot(old_lease.clone())
    .unwrap();
    assert_eq!(
        slotted
            .arm(|_| panic!("legacy arm must not call the broker"))
            .unwrap_err(),
        "legacy_slot_record_requires_reconciliation"
    );
    // Recovery has no access to the original payload. The late issuer keeps its
    // own stale object only to exercise the broker's anti-replay fence later.
    drop(secret);
    drop(original);
    drop(store);
    let store = TestStateStore::open(path.clone()).unwrap();
    let mut restarted = setup_broker_with_slots(&route, true);
    let read = state_exchange(&mut restarted, &store, &source.read_request());
    assert!(!read.to_string().contains("private-protocol-payload"));
    assert!(
        !read
            .to_string()
            .contains("cHJpdmF0ZS1wcm90b2NvbC1wYXlsb2Fk")
    );
    let snapshot = protocol_sequence::recovery_snapshot(&source, &read).unwrap();
    assert_eq!(
        (
            snapshot.retired_through,
            snapshot.issued_through,
            snapshot.unresolved.len()
        ),
        (0, 1, 1)
    );
    let new_epoch = epoch(&mut restarted);
    assert!(
        matches!(send(&mut restarted,"authority",EffectFenceCommand::RecoverSlots {broker_epoch:new_epoch.clone(),retired_incarnations:vec![0;64]}),Response::Error{code,..} if code=="recovery_required")
    );
    let (go_tx, go_rx) = std::sync::mpsc::sync_channel::<()>(0);
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel::<()>(0);
    let handle = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        go_rx.recv().unwrap();
        Ok(())
    });
    started_rx.recv().unwrap();
    let mut old_worker = IssuerWorkerRetirement::for_recovery(new_epoch.clone(), handle).unwrap();
    assert!(
        !old_worker
            .poll(|cmd| Ok(restarted.dispatch("issuer", cmd)))
            .unwrap()
    );
    assert!(matches!(
        send(
            &mut restarted,
            "provider",
            EffectFenceCommand::RecoveryRetired {
                broker_epoch: new_epoch.clone()
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        send(
            &mut restarted,
            amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            EffectFenceCommand::RecoveryRetired {
                broker_epoch: new_epoch.clone()
            }
        ),
        Response::Ok
    ));
    assert!(
        matches!(send(&mut restarted,"authority",EffectFenceCommand::RecoverSlots {broker_epoch:new_epoch.clone(),retired_incarnations:vec![0;64]}),Response::Error{code,..} if code=="recovery_required")
    );
    go_tx.send(()).unwrap();
    poll_issuer_ack(&mut old_worker, &mut restarted, true);
    assert!(matches!(
        send(
            &mut restarted,
            "authority",
            EffectFenceCommand::RecoverSlots {
                broker_epoch: new_epoch.clone(),
                retired_incarnations: vec![0; 64]
            }
        ),
        Response::Ok
    ));
    let recovery_lease = lease(&mut restarted, slot);
    slot_state_write(
        &mut restarted,
        &store,
        "slot-active",
        serde_json::json!({"lease":recovery_lease,"scope":old_scope}),
    );
    assert!(matches!(
        send(
            &mut restarted,
            "authority",
            EffectFenceCommand::OpenRecoverySlot {
                lease: recovery_lease.clone(),
                scope: old_scope.clone(),
                retired_through: snapshot.retired_through,
                issued_through: snapshot.issued_through
            }
        ),
        Response::Ok
    ));
    let mut altered_route = route.clone();
    altered_route.approval.method.request.allow_extra = true;
    assert!(
        JournaledIdentity::new(
            &snapshot.unresolved[0],
            old_scope.clone(),
            &altered_route,
            recovery_lease.clone()
        )
        .is_err()
    );
    let mut altered_cleanup = route.clone();
    altered_cleanup.cleanup_approval.method.response.allow_extra = true;
    assert!(
        JournaledIdentity::new(
            &snapshot.unresolved[0],
            old_scope.clone(),
            &altered_cleanup,
            recovery_lease.clone()
        )
        .is_err()
    );
    let recovered = JournaledIdentity::new(
        &snapshot.unresolved[0],
        old_scope.clone(),
        &route,
        recovery_lease.clone(),
    )
    .unwrap();
    assert_eq!(
        recovered
            .fence(|cmd| Ok(restarted.dispatch("authority", cmd)))
            .unwrap_err(),
        "historical_host_receipt_required"
    );
    assert!(
        matches!(recovered.retirement(|cmd| Ok(restarted.dispatch("authority", cmd))), Err(error) if error == "historical_host_receipt_required")
    );
    // The legacy v4 record has no original provider incarnation, so recovery
    // cannot fabricate a retirement or advance its unresolved watermark.
    let read = state_exchange(&mut restarted, &store, &source.read_request());
    assert_eq!(
        protocol_sequence::recovery_watermark(&source, &read),
        Err("reconciliation_required")
    );
    let mut delayed_receipt = pending.confirmed(delayed).unwrap();
    assert_eq!(
        slotted
            .dispatch(&mut delayed_receipt, |_| panic!(
                "legacy dispatch must not call the broker"
            ))
            .unwrap_err(),
        "legacy_slot_record_requires_reconciliation"
    );
    assert!(matches!(
        restarted.dispatch("provider", Command::Poll {}),
        Response::Pending
    ));
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn completed_sources_reuse_a_slot_beyond_sixty_four_without_replaying_old_issuers() {
    use amaterasu_module_sdk::protocol_broker::IssuerWorkerRetirement;
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let route = route();
    let router = ProtocolRouter::new(vec![route.clone()]).unwrap();
    let mut broker = setup_broker_with_slots(&route, true);
    let boot = epoch(&mut broker);
    let mut floors = vec![0_u64; 64];
    recover(&mut broker, &boot, &floors);
    let mut last_source = None;
    for n in 0..70_u64 {
        let mut context = ctx();
        if let LifecycleTarget::Stream { stream_id, .. } = &mut context.target {
            *stream_id = format!("stream-{n}");
        }
        let mut issuer = self::source();
        issuer.id = format!("{:032x}", n * 64);
        issuer.stream_id = format!("stream-{n}");
        let source = SequencedSource::new("issuer", &context).unwrap();
        let scope = EffectScope {
            authority_epoch: context.authority_epoch.clone(),
            issuer_epoch: "issuer.epoch".into(),
            source: broker_effect_fence::source(&issuer).unwrap(),
        };
        let slot = scope.source.slot().unwrap();
        assert_eq!(slot, 0);
        let lease = lease(&mut broker, slot);
        slot_state_write(
            &mut broker,
            &store,
            "slot-active",
            serde_json::json!({"lease":lease,"scope":scope}),
        );
        assert!(matches!(
            send(
                &mut broker,
                "authority",
                EffectFenceCommand::OpenSlot {
                    lease: lease.clone(),
                    scope: scope.clone(),
                    retired_through: 0
                }
            ),
            Response::Ok
        ));
        let op = source.operation_id(1).unwrap();
        let effect = router
            .prepare_effect("issuer", &request(&context, &op, "issuer.epoch"))
            .unwrap();
        let read = state_exchange(&mut broker, &store, &source.read_request());
        let pending = protocol_sequence::prepare_issue(
            &binding(),
            &source,
            &read,
            effect.clone(),
            issuer.clone(),
            reference(),
        )
        .unwrap();
        let stored = state_exchange_with_id(
            &mut broker,
            &store,
            pending.request(),
            pending.state_operation_id(),
        );
        let _receipt = pending.confirmed(stored).unwrap();
        let slotted = JournaledEffect::new(
            effect.clone(),
            issuer,
            scope.clone(),
            route.clone(),
            1,
            b"hello",
        )
        .unwrap()
        .in_slot(lease.clone())
        .unwrap();
        // Exercise broker slot recycling directly. The legacy SDK adapter is
        // intentionally closed because its v4 record has no instance identity.
        let guarded = amaterasu_contracts::effect_fence::GuardedEffect {
            scope: scope.clone(),
            sequence: 1,
            capability: effect.capability.clone(),
            service: effect.method.service.clone(),
            method: effect.method.method.clone(),
            interface_major: effect.method.interface_major,
            interface_minor: effect.method.interface_minor,
            schema_revision: effect.method.schema_revision as u32,
            payload_sha256: effect.payload_sha256.clone(),
            payload: serde_json::json!({"context":effect.context,"capability":effect.capability,"message_type":effect.message_type,"payload_base64":"aGVsbG8="}),
            timeout_ms: effect.timeout_ms,
            operation_id: effect.operation_id.clone(),
            parent: effect.parent,
        };
        let selected = match send(
            &mut broker,
            "authority",
            EffectFenceCommand::SelectedMethod {
                method: ProtectedMethod {
                    service: guarded.service.clone(),
                    method: guarded.method.clone(),
                    interface_major: guarded.interface_major,
                },
            },
        ) {
            Response::Result { payload } => serde_json::from_value(payload).unwrap(),
            _ => panic!("selection unavailable"),
        };
        assert!(matches!(
            send(
                &mut broker,
                "authority",
                slot_cmd(
                    &lease,
                    EffectFenceCommand::ArmSelected {
                        effect: guarded.clone(),
                        expected: Box::new(selected)
                    }
                )
            ),
            Response::Ok
        ));
        let Response::Accepted { id } = send(
            &mut broker,
            "issuer",
            slot_cmd(
                &lease,
                EffectFenceCommand::Dispatch {
                    effect: guarded.clone(),
                },
            ),
        ) else {
            panic!("not admitted")
        };
        assert!(
            matches!(broker.dispatch("provider",Command::Poll {}),Response::Request{id:got,..} if got==id)
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
        slotted
            .fence(|cmd| Ok(broker.dispatch("authority", cmd)))
            .unwrap();
        assert!(
            slotted
                .retirement(|cmd| Ok(broker.dispatch("authority", cmd)))
                .is_err()
        );
        assert!(matches!(
            send(
                &mut broker,
                "provider",
                EffectFenceCommand::ProviderRetiredInEpoch {
                    broker_epoch: boot.clone(),
                    request: id
                }
            ),
            Response::Ok
        ));
        let proof = slotted
            .retirement(|cmd| Ok(broker.dispatch("authority", cmd)))
            .unwrap();
        let read = state_exchange(&mut broker, &store, &source.read_request());
        let cleanup = protocol_sequence::prepare_cleanup(
            &binding(),
            &source,
            &read,
            1,
            &effect,
            &reference(),
            &TestClean(&proof),
        )
        .unwrap();
        assert_eq!(state_exchange(&mut broker, &store, &cleanup)["ok"], true);
        assert!(matches!(
            send(
                &mut broker,
                "authority",
                slot_cmd(
                    &lease,
                    EffectFenceCommand::Compact {
                        scope: scope.clone(),
                        retired_through: 1
                    }
                )
            ),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch("issuer", Command::ReleaseResult { request: id }),
            Response::Ok
        ));
        assert!(matches!(
            send(
                &mut broker,
                "authority",
                EffectFenceCommand::BeginCloseSlot {
                    lease: lease.clone(),
                    scope: scope.clone()
                }
            ),
            Response::Ok
        ));
        let handle = std::thread::spawn(|| Ok(()));
        let mut worker = IssuerWorkerRetirement::new(lease.clone(), scope.clone(), handle).unwrap();
        poll_issuer_ack(&mut worker, &mut broker, false);
        assert!(matches!(
            send(
                &mut broker,
                amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL,
                EffectFenceCommand::SourceRetired {
                    lease: lease.clone(),
                    scope: scope.clone()
                }
            ),
            Response::Ok
        ));
        floors[slot as usize] = lease.incarnation;
        slot_state_write(
            &mut broker,
            &store,
            "slot-floors",
            serde_json::json!(floors),
        );
        assert!(matches!(
            send(
                &mut broker,
                "authority",
                EffectFenceCommand::FinishCloseSlot {
                    lease: lease.clone(),
                    scope: scope.clone(),
                    retired_through: 1
                }
            ),
            Response::Ok
        ));
        assert!(
            matches!(send(&mut broker,"issuer",slot_cmd(&lease, EffectFenceCommand::Dispatch { effect: guarded })),Response::Error {code,..} if code == "stale_source_slot")
        );
        last_source = Some((lease, scope));
    }
    assert_eq!(floors[0], 70);
    drop(broker);
    drop(store);
    let store = TestStateStore::open(path.clone()).unwrap();
    let mut restarted = setup_broker_with_slots(&route, true);
    let saved = state_exchange(
        &mut restarted,
        &store,
        &StateRequest::Get {
            key: "slot-floors".into(),
        },
    );
    let restored: Vec<u64> = serde_json::from_value(saved["value"].clone()).unwrap();
    assert_eq!(restored[0], 70);
    let new_boot = epoch(&mut restarted);
    recover(&mut restarted, &new_boot, &restored);
    assert_eq!(lease(&mut restarted, 0).incarnation, 71);
    let (old_lease, old_scope) = last_source.unwrap();
    assert!(
        matches!(send(&mut restarted, "authority", EffectFenceCommand::OpenSlot { lease: old_lease, scope: old_scope, retired_through: 1 }), Response::Error { code, .. } if code == "stale_source_slot")
    );
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn failed_issuer_worker_never_acknowledges_source_retirement() {
    use amaterasu_module_sdk::protocol_broker::IssuerWorkerRetirement;
    let route = route();
    let mut broker = setup_broker_with_slots(&route, true);
    let boot = epoch(&mut broker);
    recover(&mut broker, &boot, &[0; 64]);
    let source = scope("issuer.epoch");
    let slot = source.source.slot().unwrap();
    let lease = lease(&mut broker, slot);
    assert!(matches!(
        send(
            &mut broker,
            "authority",
            EffectFenceCommand::OpenSlot {
                lease: lease.clone(),
                scope: source.clone(),
                retired_through: 0
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        send(
            &mut broker,
            "authority",
            EffectFenceCommand::BeginCloseSlot {
                lease: lease.clone(),
                scope: source.clone()
            }
        ),
        Response::Ok
    ));
    let worker = std::thread::spawn(|| Err("worker cleanup failed".to_string()));
    let mut retirement =
        IssuerWorkerRetirement::new(lease.clone(), source.clone(), worker).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match retirement.poll(|cmd| Ok(broker.dispatch("issuer", cmd))) {
            Ok(false) => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            Err(error) => {
                assert_eq!(error, "issuer_worker_retirement_failed");
                break;
            }
            Ok(true) => panic!("failed worker was acknowledged"),
        }
    }
    assert!(matches!(
        send(
            &mut broker,
            amaterasu_contracts::HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            EffectFenceCommand::SourceRetired {
                lease: lease.clone(),
                scope: source.clone()
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        send(
            &mut broker,
            "authority",
            EffectFenceCommand::FinishCloseSlot {
                lease,
                scope: source,
                retired_through: 0
            }
        ),
        Response::Error { .. }
    ));
}

#[test]
fn legacy_slot_adapter_rejects_admission_before_broker_query() {
    let actual = route();
    let mut configured = actual.clone();
    configured.approval.method.request.allow_extra = true;
    let router = ProtocolRouter::new(vec![configured.clone()]).unwrap();
    let context = ctx();
    let source = SequencedSource::new("issuer", &context).unwrap();
    let op = source.operation_id(1).unwrap();
    let effect = router
        .prepare_effect("issuer", &request(&context, &op, "issuer.epoch"))
        .unwrap();
    let mut broker = setup_broker_with_slots(&actual, true);
    let boot = epoch(&mut broker);
    recover(&mut broker, &boot, &[0; 64]);
    let scope = scope("issuer.epoch");
    let lease = lease(&mut broker, scope.source.slot().unwrap());
    assert!(matches!(
        send(
            &mut broker,
            "authority",
            EffectFenceCommand::OpenSlot {
                lease: lease.clone(),
                scope: scope.clone(),
                retired_through: 0
            }
        ),
        Response::Ok
    ));
    let slotted = JournaledEffect::new(effect, self::source(), scope, configured, 1, b"hello")
        .unwrap()
        .in_slot(lease)
        .unwrap();
    assert_eq!(
        slotted
            .arm(|cmd| Ok(broker.dispatch("authority", cmd)))
            .unwrap_err(),
        "legacy_slot_record_requires_reconciliation"
    );
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Pending
    ));
}

#[test]
fn canonical_slot_selection_is_host_bound_and_cleanup_query_remains_closed() {
    use amaterasu_contracts::effect_fence::{ProviderIncarnation, SourceSlotLease};
    use amaterasu_module_sdk::protocol_slot_adapter::{SelectedSlotAction, SlotActionRequest};
    let route = route();
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
    config.host_context.provider_incarnations.insert(
        "provider".into(),
        ProviderIncarnation {
            version: 1,
            id: "d".repeat(32),
        },
    );
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
    let issuer = source();
    let scope = scope("issued.epoch");
    let mut context = ctx();
    context.peer_ref = issuer.id.clone();
    let operation_id =
        amaterasu_module_sdk::protocol_slot_state::operation_id("issuer", &scope, 1).unwrap();
    let request = DispatchRequest {
        context: &context,
        message_type: route.message_type,
        payload: b"hello",
        operation_id: &operation_id,
        issuer_epoch: &scope.issuer_epoch,
        timeout_ms: 1000,
        parent: None,
    };
    let pending = ProtocolRouter::new(vec![route.clone()])
        .unwrap()
        .prepare_effect("issuer", &request)
        .unwrap();
    let lease = SourceSlotLease {
        slot: scope.source.slot().unwrap(),
        incarnation: 1,
        broker_epoch: "e".repeat(32),
        token: "f".repeat(32),
    };
    let action = SelectedSlotAction::query(
        SlotActionRequest {
            pending: pending.clone(),
            issuer: &issuer,
            scope: scope.clone(),
            lease: lease.clone(),
            route: &route,
            sequence: 1,
            payload: b"hello",
        },
        |cmd| Ok(broker.dispatch("authority", cmd)),
    )
    .unwrap();
    assert_eq!(
        action
            .selected_method()
            .provider_incarnation
            .as_ref()
            .unwrap()
            .id,
        "d".repeat(32)
    );
    assert_eq!(action.prepare_issue(), Err("cleanup_selection_unavailable"));
    let mut missing_instance = action.selected_method().clone();
    missing_instance.provider_incarnation = None;
    assert!(matches!(SelectedSlotAction::query(
        SlotActionRequest { pending: pending.clone(), issuer: &issuer, scope: scope.clone(), lease: lease.clone(), route: &route, sequence: 1, payload: b"hello" },
        |_| Ok(Response::Result { payload: serde_json::to_value(&missing_instance).unwrap() })
    ), Err(error) if error == "host_provider_incarnation_required"));
    let mut unsupported_instance = action.selected_method().clone();
    unsupported_instance
        .provider_incarnation
        .as_mut()
        .unwrap()
        .version = 2;
    assert!(matches!(SelectedSlotAction::query(
        SlotActionRequest { pending: pending.clone(), issuer: &issuer, scope: scope.clone(), lease: lease.clone(), route: &route, sequence: 1, payload: b"hello" },
        |_| Ok(Response::Result { payload: serde_json::to_value(&unsupported_instance).unwrap() })
    ), Err(error) if error == "selection_mismatch"));
    let mut wrong_scope = scope.clone();
    wrong_scope.issuer_epoch = "forged".into();
    assert!(
        matches!(SelectedSlotAction::query(SlotActionRequest { pending: pending.clone(), issuer: &issuer, scope: wrong_scope, lease: lease.clone(), route: &route, sequence: 1, payload: b"hello" }, |_| panic!("invalid identity queried the broker")), Err(error) if error == "effect_identity_mismatch")
    );
    assert!(
        matches!(SelectedSlotAction::query(SlotActionRequest { pending, issuer: &issuer, scope, lease, route: &route, sequence: 1, payload: b"different" }, |_| panic!("invalid payload queried the broker")), Err(error) if error == "effect_identity_mismatch")
    );
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Pending
    ));
}
