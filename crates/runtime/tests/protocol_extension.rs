//! Real broker, two independent provider principals, one unchanged SDK consumer.
//! These test handlers are in-process fakes; no client ingress or device is used.
use amaterasu_contracts::{
    ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Command, Grants, Response,
    RuntimeConfig,
};
use amaterasu_module_sdk::{
    extension_api::{LifecycleTarget, MethodRef},
    protocol::{
        BrokerEndpoint, DispatchRequest, ObligationStore, PendingEffect, ProtocolRouter,
        Reservation, Route, SessionAuthority, SessionContext, validate_response,
    },
};
use amaterasu_runtime::Broker;
use std::collections::{BTreeMap, BTreeSet};

const CLIENT: &str = "example.consumer";
const KIND: u16 = 0x553a;

fn api(service: &str) -> ApiMethod {
    ApiMethod {
        service: service.into(),
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
            optional: BTreeMap::new(),
            allow_extra: false,
        },
        response: ApiSchema {
            required: [("provider".into(), ApiFieldKind::String)].into(),
            optional: BTreeMap::new(),
            allow_extra: false,
        },
        privilege: ApiPrivilege::User,
    }
}
fn route(provider: &str, service: &str) -> Route {
    let method = api(service);
    Route {
        message_type: KIND,
        capability: "example.control/1".into(),
        approval: ApiMethodApproval {
            provider: provider.into(),
            callers: [CLIENT.into()].into(),
            method,
        },
        method: MethodRef {
            service: service.into(),
            method: "receive".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        },
        cleanup: MethodRef {
            service: service.into(),
            method: "cleanup".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        },
        cleanup_approval: {
            let mut method = api(service);
            method.method = "cleanup".into();
            ApiMethodApproval {
                provider: provider.into(),
                callers: [CLIENT.into()].into(),
                method,
            }
        },
        max_payload_bytes: 64,
    }
}
fn context(peer: &str) -> SessionContext {
    SessionContext {
        authority_epoch: "authority.epoch".into(),
        effect_owner_epoch: "owner.epoch".into(),
        target: LifecycleTarget::Stream {
            host_epoch: "epoch".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        },
        peer_ref: peer.into(),
    }
}
fn issuer(ctx: &SessionContext) -> amaterasu_module_sdk::effect_authority::Source {
    let LifecycleTarget::Stream {
        host_epoch,
        session_id,
        stream_id,
        generation,
    } = &ctx.target
    else {
        panic!()
    };
    amaterasu_module_sdk::effect_authority::Source {
        version: 1,
        id: "a".repeat(32),
        generation: *generation,
        host_epoch: host_epoch.clone(),
        session_id: session_id.clone(),
        stream_id: stream_id.clone(),
        stream_generation: *generation,
        device: "device".into(),
        device_generation: 1,
        effect_owner_epoch: ctx.effect_owner_epoch.clone(),
    }
}
struct Authority {
    negotiated: bool,
    permitted: bool,
}
impl SessionAuthority for Authority {
    type AdmissionGuard = ();
    fn admit(
        &self,
        ctx: &SessionContext,
        cap: &str,
        _effect: &PendingEffect,
    ) -> Result<(), &'static str> {
        if *ctx == context("verified")
            && cap == "example.control/1"
            && self.negotiated
            && self.permitted
        {
            Ok(())
        } else {
            Err("unauthorized")
        }
    }
}
#[derive(Default)]
struct Ledger(BTreeMap<(String, String), PendingEffect>);
impl ObligationStore for Ledger {
    fn reserve(&mut self, effect: &PendingEffect) -> Result<Reservation, &'static str> {
        let key = (effect.caller.clone(), effect.operation_id.clone());
        match self.0.get(&key) {
            Some(existing) if existing == effect => Ok(Reservation::Existing),
            Some(_) => Err("conflict"),
            None if self.0.len() >= 4 => Err("capacity"),
            None => {
                self.0.insert(key, effect.clone());
                Ok(Reservation::New)
            }
        }
    }
}
fn setup(routes: &[Route]) -> Broker {
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        CLIENT.into(),
        Grants {
            call: routes.iter().map(|r| r.method.service.clone()).collect(),
            ..Default::default()
        },
    );
    for route in routes {
        for caller in &route.approval.callers {
            config
                .principals
                .entry(caller.clone())
                .or_default()
                .call
                .insert(route.method.service.clone());
        }
        config.principals.insert(
            route.approval.provider.clone(),
            Grants {
                provide: BTreeSet::from([route.method.service.clone()]),
                ..Default::default()
            },
        );
        config.api_methods.push(route.approval.clone());
        config.api_methods.push(route.cleanup_approval.clone());
    }
    let mut broker = Broker::new(config).unwrap();
    for route in routes {
        assert!(matches!(
            broker.dispatch(
                &route.approval.provider,
                Command::RegisterApi {
                    method: route.approval.method.clone()
                }
            ),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch(
                &route.approval.provider,
                Command::RegisterApi {
                    method: route.cleanup_approval.method.clone()
                }
            ),
            Response::Ok
        ));
    }
    broker
}
fn consume(
    router: &ProtocolRouter,
    authority: &Authority,
    ledger: &mut Ledger,
    broker: &mut Broker,
    ctx: &SessionContext,
    body: &[u8],
    operation: &str,
) -> Result<Response, String> {
    consume_as(
        CLIENT, router, authority, ledger, broker, ctx, body, operation,
    )
}

#[allow(clippy::too_many_arguments)]
fn consume_as(
    caller: &str,
    router: &ProtocolRouter,
    authority: &Authority,
    ledger: &mut Ledger,
    broker: &mut Broker,
    ctx: &SessionContext,
    body: &[u8],
    operation: &str,
) -> Result<Response, String> {
    router.dispatch(
        authority,
        ledger,
        DispatchRequest {
            context: ctx,
            message_type: KIND,
            payload: body,
            operation_id: operation,
            timeout_ms: 1000,
            parent: None,
        },
        BrokerEndpoint::new(caller, |cmd| Ok(broker.dispatch(caller, cmd))),
    )
}
fn reply(broker: &mut Broker, provider: &str, label: &str) -> u64 {
    let Response::Request { id, payload, .. } = broker.dispatch(provider, Command::Poll {}) else {
        panic!("provider not called")
    };
    assert_eq!(payload["arguments"]["payload_base64"], "aGVsbG8=");
    assert!(matches!(
        broker.dispatch(
            provider,
            Command::Complete {
                request: id,
                payload: serde_json::json!({"provider":label})
            }
        ),
        Response::Ok
    ));
    id
}
#[test]
fn unchanged_consumer_routes_to_two_registered_providers_and_retains_obligations() {
    let a = route("example.provider_a", "example.a/1");
    let b = route("example.provider_b", "example.b/1");
    let mut broker = setup(&[a.clone(), b.clone()]);
    let auth = Authority {
        negotiated: true,
        permitted: true,
    };
    for (selection, label) in [(a, "a"), (b, "b")] {
        let router = ProtocolRouter::new(vec![selection.clone()]).unwrap();
        let mut ledger = Ledger::default();
        let Response::Accepted { id } = consume(
            &router,
            &auth,
            &mut ledger,
            &mut broker,
            &context("verified"),
            b"hello",
            label,
        )
        .unwrap() else {
            panic!("not admitted")
        };
        assert_eq!(reply(&mut broker, &selection.approval.provider, label), id);
        assert!(
            matches!(broker.dispatch(CLIENT, Command::TakeResult { request:id }), Response::Result { payload } if payload["provider"] == label)
        );
        assert_eq!(
            ledger.0[&(CLIENT.into(), label.into())].provider,
            selection.approval.provider
        );
        assert_eq!(
            ledger.0[&(CLIENT.into(), label.into())].cleanup,
            selection.cleanup
        );
    }
}
#[test]
fn rejects_reserved_duplicate_unauthorized_oversized_and_conflicting_requests() {
    let a = route("example.provider_a", "example.a/1");
    let mut reserved = a.clone();
    reserved.message_type = 0x0206;
    assert!(ProtocolRouter::new(vec![reserved]).is_err());
    assert!(ProtocolRouter::new(vec![a.clone(), a.clone()]).is_err());
    let mut broker = setup(std::slice::from_ref(&a));
    let router = ProtocolRouter::new(vec![a]).unwrap();
    let mut ledger = Ledger::default();
    for (auth, ctx) in [
        (
            Authority {
                negotiated: false,
                permitted: true,
            },
            context("verified"),
        ),
        (
            Authority {
                negotiated: true,
                permitted: false,
            },
            context("verified"),
        ),
        (
            Authority {
                negotiated: true,
                permitted: true,
            },
            context("forged"),
        ),
    ] {
        assert_eq!(
            consume(
                &router,
                &auth,
                &mut ledger,
                &mut broker,
                &ctx,
                b"hello",
                "op"
            )
            .unwrap_err(),
            "unauthorized"
        );
    }
    let auth = Authority {
        negotiated: true,
        permitted: true,
    };
    assert!(
        consume(
            &router,
            &auth,
            &mut ledger,
            &mut broker,
            &context("verified"),
            &[0; 65],
            "op"
        )
        .is_err()
    );
    assert!(ledger.0.is_empty());
    assert!(matches!(
        consume(
            &router,
            &auth,
            &mut ledger,
            &mut broker,
            &context("verified"),
            b"hello",
            "op"
        )
        .unwrap(),
        Response::Accepted { .. }
    ));
    assert_eq!(
        consume(
            &router,
            &auth,
            &mut ledger,
            &mut broker,
            &context("verified"),
            b"different",
            "op"
        )
        .unwrap_err(),
        "conflict"
    );
    assert_eq!(ledger.0.len(), 1);
}
#[test]
fn uncertain_broker_result_never_discards_reserved_effect() {
    let a = route("example.provider_a", "example.a/1");
    let mut broker = setup(std::slice::from_ref(&a));
    let router = ProtocolRouter::new(vec![a]).unwrap();
    let mut ledger = Ledger::default();
    let auth = Authority {
        negotiated: true,
        permitted: true,
    };
    let mut calls = 0;
    let error = router
        .dispatch(
            &auth,
            &mut ledger,
            DispatchRequest {
                context: &context("verified"),
                message_type: KIND,
                payload: b"hello",
                operation_id: "lost",
                timeout_ms: 1000,
                parent: None,
            },
            BrokerEndpoint::new(CLIENT, |cmd| {
                calls += 1;
                if calls == 3 {
                    let _ = broker.dispatch(CLIENT, cmd);
                    Err("lost reply".into())
                } else {
                    Ok(broker.dispatch(CLIENT, cmd))
                }
            }),
        )
        .unwrap_err();
    assert_eq!(error, "lost reply");
    assert!(ledger.0.contains_key(&(CLIENT.into(), "lost".into())));
    assert!(matches!(
        broker.dispatch("example.provider_a", Command::Poll {}),
        Response::Request { .. }
    ));
    assert_eq!(
        consume(
            &router,
            &auth,
            &mut ledger,
            &mut broker,
            &context("verified"),
            b"hello",
            "lost"
        )
        .unwrap_err(),
        "operation_requires_reconciliation"
    );
    assert!(matches!(
        broker.dispatch("example.provider_a", Command::Poll {}),
        Response::Pending
    ));
    assert_eq!(ledger.0.len(), 1);
}

#[test]
#[ignore = "requires separately built runtime and protocol provider cdylibs"]
fn unchanged_router_consumer_uses_two_independent_module_artifacts() {
    use amaterasu_abi::{
        BorrowedBytes, HostApi, ModuleApi, ModuleEntry, RuntimeApi, RuntimeEntry, STATUS_OK,
    };
    use amaterasu_module_sdk::RuntimeClient;
    use libloading::Library;
    use std::{ffi::c_void, mem::MaybeUninit, path::PathBuf};

    struct Runtime {
        library: Library,
        api: RuntimeApi,
        instance: *mut c_void,
    }
    impl Runtime {
        fn new(path: &PathBuf, config: &RuntimeConfig) -> Self {
            // SAFETY: test keeps this exact library loaded through runtime destruction.
            let library = unsafe { Library::new(path) }.unwrap();
            // SAFETY: explicit test artifact exports the documented runtime entry.
            let api = unsafe {
                *library
                    .get::<RuntimeEntry>(b"amaterasu_runtime_v1\0")
                    .unwrap()()
            };
            assert!(api.header.supports::<RuntimeApi>());
            let input = serde_json::to_vec(config).unwrap();
            let mut instance = std::ptr::null_mut();
            // SAFETY: input and output are valid for the synchronous call.
            assert_eq!(
                unsafe { (api.create)(BorrowedBytes::new(&input), &mut instance) },
                STATUS_OK
            );
            Self {
                library,
                api,
                instance,
            }
        }
        fn endpoint(&self, name: &str) -> HostApi {
            let mut endpoint = MaybeUninit::uninit();
            // SAFETY: runtime is live, name and output are valid for this call.
            assert_eq!(
                unsafe {
                    (self.api.endpoint)(
                        self.instance,
                        BorrowedBytes::new(name.as_bytes()),
                        endpoint.as_mut_ptr(),
                    )
                },
                STATUS_OK
            );
            // SAFETY: STATUS_OK initializes the endpoint.
            unsafe { endpoint.assume_init() }
        }
    }
    impl Drop for Runtime {
        fn drop(&mut self) {
            // SAFETY: all modules and clients have stopped before runtime drop.
            unsafe { (self.api.destroy)(self.instance) };
            let _ = &self.library;
        }
    }
    struct Module {
        library: Library,
        api: ModuleApi,
        instance: *mut c_void,
    }
    impl Module {
        fn new(path: &PathBuf, endpoint: HostApi) -> Self {
            // SAFETY: the loaded fixture remains alive through module destruction.
            let library = unsafe { Library::new(path) }.unwrap();
            // SAFETY: the explicit fixture exports the documented module entry.
            let api = unsafe {
                *library
                    .get::<ModuleEntry>(b"amaterasu_module_v1\0")
                    .unwrap()()
            };
            assert!(api.header.supports::<ModuleApi>());
            let mut instance = std::ptr::null_mut();
            // SAFETY: endpoint and output are valid and the runtime outlives the module.
            assert_eq!(unsafe { (api.create)(&endpoint, &mut instance) }, STATUS_OK);
            Self {
                library,
                api,
                instance,
            }
        }
        fn tick(&self) {
            // SAFETY: module is live and this test ticks it serially.
            assert_eq!(unsafe { (self.api.tick)(self.instance) }, STATUS_OK);
        }
    }
    impl Drop for Module {
        fn drop(&mut self) {
            // SAFETY: called exactly once before unloading the fixture.
            unsafe { (self.api.destroy)(self.instance) };
            let _ = &self.library;
        }
    }
    fn artifact(name: &str) -> PathBuf {
        std::env::var_os(name)
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("set {name}"))
    }
    fn run(runtime_path: &PathBuf, module_path: &PathBuf, provider: &str, label: &str) {
        let selection = route(provider, "example.protocol/1");
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            CLIENT.into(),
            Grants {
                call: [selection.method.service.clone()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            provider.into(),
            Grants {
                provide: [selection.method.service.clone()].into(),
                ..Default::default()
            },
        );
        config.api_methods.extend([
            selection.approval.clone(),
            selection.cleanup_approval.clone(),
        ]);
        let runtime = Runtime::new(runtime_path, &config);
        let module = Module::new(module_path, runtime.endpoint(provider));
        // SAFETY: endpoint remains live through this client's final call.
        let client = unsafe { RuntimeClient::from_endpoint(runtime.endpoint(CLIENT)) };
        let router = ProtocolRouter::new(vec![selection]).unwrap();
        let mut ledger = Ledger::default();
        let auth = Authority {
            negotiated: true,
            permitted: true,
        };
        let ctx = context("verified");
        let response = router
            .dispatch(
                &auth,
                &mut ledger,
                DispatchRequest {
                    context: &ctx,
                    message_type: KIND,
                    payload: b"hello",
                    operation_id: "artifact",
                    timeout_ms: 1000,
                    parent: None,
                },
                BrokerEndpoint::new(CLIENT, |cmd| client.send(cmd)),
            )
            .unwrap();
        let Response::Accepted { id } = response else {
            panic!("not admitted")
        };
        module.tick();
        assert!(
            matches!(client.send(Command::TakeResult { request:id }).unwrap(), Response::Result { payload } if payload["provider"] == label)
        );
        assert_eq!(
            ledger.0[&(CLIENT.into(), "artifact".into())].provider,
            provider
        );
        drop(module);
        drop(runtime);
    }
    let runtime = artifact("PROTOCOL_TEST_RUNTIME");
    run(
        &runtime,
        &artifact("PROTOCOL_TEST_PROVIDER_A"),
        "example.provider_a",
        "a",
    );
    run(
        &runtime,
        &artifact("PROTOCOL_TEST_PROVIDER_B"),
        "example.provider_b",
        "b",
    );
}

#[test]
fn response_is_bounded_before_forwarding() {
    assert!(validate_response(&serde_json::json!({"message":"ok"})).is_ok());
    assert_eq!(
        validate_response(&serde_json::json!({"message":"x".repeat(4096)})),
        Err("response_too_large")
    );
}

#[test]
fn missing_cleanup_approval_never_admits_or_reserves() {
    let selection = route("example.provider_a", "example.a/1");
    let mut broker = setup(std::slice::from_ref(&selection));
    let router = ProtocolRouter::new(vec![selection]).unwrap();
    let mut ledger = Ledger::default();
    let ctx = context("verified");
    let mut calls = 0;
    let error = router
        .dispatch(
            &Authority {
                negotiated: true,
                permitted: true,
            },
            &mut ledger,
            DispatchRequest {
                context: &ctx,
                message_type: KIND,
                payload: b"hello",
                operation_id: "cleanup-missing",
                timeout_ms: 1000,
                parent: None,
            },
            BrokerEndpoint::new(CLIENT, |cmd| {
                calls += 1;
                if calls == 2 {
                    Ok(Response::error("not_found", "cleanup missing"))
                } else {
                    Ok(broker.dispatch(CLIENT, cmd))
                }
            }),
        )
        .unwrap_err();
    assert_eq!(error, "approval_unavailable");
    assert!(ledger.0.is_empty());
    assert!(matches!(
        broker.dispatch("example.provider_a", Command::Poll {}),
        Response::Pending
    ));
}

#[test]
fn default_authority_and_unregistered_routes_deny_before_provider_calls() {
    struct DefaultDeny;
    impl SessionAuthority for DefaultDeny {
        type AdmissionGuard = ();
    }
    let selection = route("example.provider_a", "example.a/1");
    let router = ProtocolRouter::new(vec![selection]).unwrap();
    let mut ledger = Ledger::default();
    let ctx = context("verified");
    let mut calls = 0;
    let mut send = |_| {
        calls += 1;
        Ok(Response::Ok)
    };
    assert_eq!(
        router
            .dispatch(
                &DefaultDeny,
                &mut ledger,
                DispatchRequest {
                    context: &ctx,
                    message_type: KIND,
                    payload: b"",
                    operation_id: "denied",
                    timeout_ms: 1000,
                    parent: None,
                },
                BrokerEndpoint::new(CLIENT, &mut send)
            )
            .unwrap_err(),
        "unauthorized"
    );
    assert_eq!(
        router
            .dispatch(
                &DefaultDeny,
                &mut ledger,
                DispatchRequest {
                    context: &ctx,
                    message_type: 0x553b,
                    payload: b"",
                    operation_id: "unknown",
                    timeout_ms: 1000,
                    parent: None,
                },
                BrokerEndpoint::new(CLIENT, &mut send)
            )
            .unwrap_err(),
        "unregistered_route"
    );
    assert_eq!(calls, 0);
    assert!(ledger.0.is_empty());
}

#[test]
fn identical_operations_from_distinct_approved_callers_keep_distinct_obligations() {
    let other = "example.consumer_two";
    let mut selection = route("example.provider_a", "example.a/1");
    selection.approval.callers.insert(other.into());
    selection.cleanup_approval.callers.insert(other.into());
    let mut broker = setup(std::slice::from_ref(&selection));
    let router = ProtocolRouter::new(vec![selection]).unwrap();
    let mut ledger = Ledger::default();
    let auth = Authority {
        negotiated: true,
        permitted: true,
    };
    let mut request_ids = Vec::new();
    for caller in [CLIENT, other] {
        let response = consume_as(
            caller,
            &router,
            &auth,
            &mut ledger,
            &mut broker,
            &context("verified"),
            b"hello",
            "same",
        )
        .unwrap();
        let Response::Accepted { id } = response else {
            panic!("not admitted")
        };
        request_ids.push(id);
    }
    assert_ne!(request_ids[0], request_ids[1]);
    assert_eq!(ledger.0.len(), 2);
    for caller in [CLIENT, other] {
        assert_eq!(ledger.0[&(caller.into(), "same".into())].caller, caller);
        assert!(matches!(
            broker.dispatch("example.provider_a", Command::Poll {}),
            Response::Request { .. }
        ));
    }
}

#[test]
fn retained_obligation_blocks_retry_after_broker_record_eviction_and_restart() {
    let selection = route("example.provider_a", "example.a/1");
    let mut broker = setup(std::slice::from_ref(&selection));
    let router = ProtocolRouter::new(vec![selection.clone()]).unwrap();
    let mut ledger = Ledger::default();
    let auth = Authority {
        negotiated: true,
        permitted: true,
    };
    let Response::Accepted { id } = consume(
        &router,
        &auth,
        &mut ledger,
        &mut broker,
        &context("verified"),
        b"hello",
        "retry",
    )
    .unwrap() else {
        panic!("not admitted")
    };
    assert_eq!(reply(&mut broker, "example.provider_a", "a"), id);
    // Release removes the record, simulating the same absence as retention expiry
    // without a five-minute sleep or changing the runtime's test clock.
    assert!(matches!(
        broker.dispatch(CLIENT, Command::ReleaseResult { request: id }),
        Response::Ok
    ));
    for after_restart in [false, true] {
        if after_restart {
            broker = setup(std::slice::from_ref(&selection));
        }
        assert_eq!(
            consume(
                &router,
                &auth,
                &mut ledger,
                &mut broker,
                &context("verified"),
                b"hello",
                "retry"
            )
            .unwrap_err(),
            "operation_requires_reconciliation"
        );
        assert!(matches!(
            broker.dispatch("example.provider_a", Command::Poll {}),
            Response::Pending
        ));
        assert_eq!(ledger.0.len(), 1);
    }
}

#[test]
fn uncertain_reservation_never_admits_provider_call() {
    struct UncertainStore;
    impl ObligationStore for UncertainStore {
        fn reserve(&mut self, _: &PendingEffect) -> Result<Reservation, &'static str> {
            Ok(Reservation::Uncertain)
        }
    }
    let selection = route("example.provider_a", "example.a/1");
    let mut broker = setup(std::slice::from_ref(&selection));
    let router = ProtocolRouter::new(vec![selection]).unwrap();
    let auth = Authority {
        negotiated: true,
        permitted: true,
    };
    let result = router.dispatch(
        &auth,
        &mut UncertainStore,
        DispatchRequest {
            context: &context("verified"),
            message_type: KIND,
            payload: b"hello",
            operation_id: "uncertain",
            timeout_ms: 1000,
            parent: None,
        },
        BrokerEndpoint::new(CLIENT, |cmd| Ok(broker.dispatch(CLIENT, cmd))),
    );
    assert_eq!(result.unwrap_err(), "operation_requires_reconciliation");
    assert!(matches!(
        broker.dispatch("example.provider_a", Command::Poll {}),
        Response::Pending
    ));
}

#[test]
fn state_reservation_receipt_matches_production_router_effect_and_is_one_use() {
    use amaterasu_module_sdk::protocol_state::{
        self, CleanupReference, DURABLE_CAS_CAPABILITY, DurableStateBinding, ReservationPlan,
    };
    let selection = route("example.provider_a", "example.a/1");
    let mut broker = setup(std::slice::from_ref(&selection));
    let router = ProtocolRouter::new(vec![selection]).unwrap();
    let auth = Authority {
        negotiated: true,
        permitted: true,
    };
    let ctx = context("verified");
    let request = || DispatchRequest {
        context: &ctx,
        message_type: KIND,
        payload: b"hello",
        operation_id: "persisted",
        timeout_ms: 1000,
        parent: None,
    };
    let effect = router.prepare_effect(CLIENT, &request()).unwrap();
    let binding = DurableStateBinding::new("owner", "state", DURABLE_CAS_CAPABILITY).unwrap();
    let ReservationPlan::Write(write) = protocol_state::prepare_reservation(
        &binding,
        &serde_json::json!({"ok":true,"revision":0,"found":false,"value":null}),
        effect,
        CleanupReference::new("a".repeat(64)).unwrap(),
    )
    .unwrap() else {
        panic!()
    };
    // The real State provider's persistence/restart semantics are exercised by
    // modules/state/tests/protocol_journal.rs; here the broker is real.
    let mut receipt = write
        .confirmed(serde_json::json!({"ok":true,"revision":1}))
        .unwrap();
    assert!(matches!(
        router
            .dispatch(
                &auth,
                &mut receipt,
                request(),
                BrokerEndpoint::new(CLIENT, |cmd| Ok(broker.dispatch(CLIENT, cmd)))
            )
            .unwrap(),
        Response::Accepted { .. }
    ));
    assert_eq!(
        router
            .dispatch(
                &auth,
                &mut receipt,
                request(),
                BrokerEndpoint::new(CLIENT, |cmd| Ok(broker.dispatch(CLIENT, cmd)))
            )
            .unwrap_err(),
        "reservation_mismatch"
    );
}

#[test]
fn stale_authority_epoch_is_rejected_before_provider_admission() {
    let selection = route("example.provider_a", "example.a/1");
    let mut broker = setup(std::slice::from_ref(&selection));
    let router = ProtocolRouter::new(vec![selection]).unwrap();
    let mut ledger = Ledger::default();
    let auth = Authority {
        negotiated: true,
        permitted: true,
    };
    let mut stale = context("verified");
    stale.authority_epoch = "retired.epoch".into();
    assert_eq!(
        consume(
            &router,
            &auth,
            &mut ledger,
            &mut broker,
            &stale,
            b"hello",
            "stale"
        )
        .unwrap_err(),
        "unauthorized"
    );
    let mut stale_generation = context("verified");
    if let LifecycleTarget::Stream { generation, .. } = &mut stale_generation.target {
        *generation = 2;
    }
    assert_eq!(
        consume(
            &router,
            &auth,
            &mut ledger,
            &mut broker,
            &stale_generation,
            b"hello",
            "stale-generation"
        )
        .unwrap_err(),
        "unauthorized"
    );
    assert!(ledger.0.is_empty());
    assert!(matches!(
        broker.dispatch("example.provider_a", Command::Poll {}),
        Response::Pending
    ));
}

#[test]
fn sequenced_journal_receipt_routes_through_unchanged_broker_consumer() {
    use amaterasu_module_sdk::{
        protocol_sequence::{self, SequencedSource},
        protocol_state::{CleanupReference, DURABLE_CAS_CAPABILITY, DurableStateBinding},
    };
    let selection = route("example.provider_a", "example.a/1");
    let mut broker = setup(std::slice::from_ref(&selection));
    let router = ProtocolRouter::new(vec![selection]).unwrap();
    let auth = Authority {
        negotiated: true,
        permitted: true,
    };
    let ctx = context("verified");
    let source = SequencedSource::new(CLIENT, &ctx).unwrap();
    let operation = source.operation_id(1).unwrap();
    let request = || DispatchRequest {
        context: &ctx,
        message_type: KIND,
        payload: b"hello",
        operation_id: &operation,
        timeout_ms: 1000,
        parent: None,
    };
    let effect = router.prepare_effect(CLIENT, &request()).unwrap();
    let binding = DurableStateBinding::new("owner", "state", DURABLE_CAS_CAPABILITY).unwrap();
    let initial = serde_json::json!({"ok":true,"revision":0,"found":false,"value":null});
    let pending = protocol_sequence::prepare_issue(
        &binding,
        &source,
        &initial,
        effect,
        issuer(&ctx),
        CleanupReference::new("a".repeat(64)).unwrap(),
    )
    .unwrap();
    let mut receipt = pending
        .confirmed(serde_json::json!({"ok":true,"revision":1}))
        .unwrap();
    assert!(matches!(
        router
            .dispatch(
                &auth,
                &mut receipt,
                request(),
                BrokerEndpoint::new(CLIENT, |cmd| Ok(broker.dispatch(CLIENT, cmd)))
            )
            .unwrap(),
        Response::Accepted { .. }
    ));
    assert_eq!(
        router
            .dispatch(
                &auth,
                &mut receipt,
                request(),
                BrokerEndpoint::new(CLIENT, |cmd| Ok(broker.dispatch(CLIENT, cmd)))
            )
            .unwrap_err(),
        "reservation_mismatch"
    );
}

#[test]
fn retired_issuer_cannot_dispatch_a_delayed_success_after_reconciliation() {
    use amaterasu_module_sdk::{
        effect_authority::Source,
        protocol_sequence::{self, IssuerRetirement, Lookup, SequencedSource},
        protocol_state::{CleanupReference, DURABLE_CAS_CAPABILITY, DurableStateBinding},
    };
    use std::sync::{Mutex, MutexGuard, mpsc};

    // This models the trusted authority/runtime contract, not a production
    // authority implementation. The guard serializes broker admission against
    // permanent retirement of the exact issuing continuation.
    struct Authority<'a> {
        retired: &'a Mutex<bool>,
        operation: &'a str,
    }
    impl<'a> SessionAuthority for Authority<'a> {
        type AdmissionGuard = MutexGuard<'a, bool>;
        fn admit(
            &self,
            _: &SessionContext,
            _: &str,
            effect: &PendingEffect,
        ) -> Result<Self::AdmissionGuard, &'static str> {
            let retired = self.retired.lock().unwrap();
            if *retired || effect.operation_id != self.operation {
                return Err("issuer_retired");
            }
            Ok(retired)
        }
    }
    struct RuntimeRetirement<'a> {
        retired: &'a Mutex<bool>,
        issuer: Source,
        effect: PendingEffect,
    }
    impl IssuerRetirement for RuntimeRetirement<'_> {
        fn verify_retired(
            &self,
            issuer: &Source,
            effect: &PendingEffect,
        ) -> Result<(), &'static str> {
            if issuer != &self.issuer || effect != &self.effect || !*self.retired.lock().unwrap() {
                return Err("issuer_retirement_unproven");
            }
            Ok(())
        }
    }
    struct NoEvidence;
    impl IssuerRetirement for NoEvidence {}

    let selection = route("example.provider_a", "example.a/1");
    let broker = setup(std::slice::from_ref(&selection));
    let router = ProtocolRouter::new(vec![selection]).unwrap();
    let ctx = context("verified");
    let source = SequencedSource::new(CLIENT, &ctx).unwrap();
    let operation = source.operation_id(1).unwrap();
    let request = || DispatchRequest {
        context: &ctx,
        message_type: KIND,
        payload: b"hello",
        operation_id: &operation,
        timeout_ms: 1000,
        parent: None,
    };
    let effect = router.prepare_effect(CLIENT, &request()).unwrap();
    let issuing_source = issuer(&ctx);
    let binding = DurableStateBinding::new("owner", "state", DURABLE_CAS_CAPABILITY).unwrap();
    let cleanup_ref = CleanupReference::new("a".repeat(64)).unwrap();
    let initial = serde_json::json!({"ok":true,"revision":0,"found":false,"value":null});
    let pending = protocol_sequence::prepare_issue(
        &binding,
        &source,
        &initial,
        effect.clone(),
        issuing_source.clone(),
        cleanup_ref.clone(),
    )
    .unwrap();
    // The State CAS has committed but its success reply is held by the live issuer.
    let stored = serde_json::json!({"ok":true,"revision":1,"found":true,"value":pending.request().payload()["value"]});
    let retired = Mutex::new(false);
    let retirement = RuntimeRetirement {
        retired: &retired,
        issuer: issuing_source,
        effect: effect.clone(),
    };
    let (continue_tx, continue_rx) = mpsc::sync_channel::<()>(0);
    let (started_tx, started_rx) = mpsc::sync_channel::<()>(0);
    std::thread::scope(|scope| {
        let retired_ref = &retired;
        let operation_ref = operation.as_str();
        let thread = scope.spawn(move || {
            let mut broker = broker;
            started_tx.send(()).unwrap();
            continue_rx.recv().unwrap();
            let mut receipt = pending
                .confirmed(serde_json::json!({"ok":true,"revision":1}))
                .unwrap();
            let result = router.dispatch(
                &Authority {
                    retired: retired_ref,
                    operation: operation_ref,
                },
                &mut receipt,
                request(),
                BrokerEndpoint::new(CLIENT, |cmd| Ok(broker.dispatch(CLIENT, cmd))),
            );
            (result, broker)
        });
        started_rx.recv().unwrap();
        assert!(matches!(
            protocol_sequence::prepare_cleanup(
                &binding,
                &source,
                &stored,
                1,
                &effect,
                &cleanup_ref,
                &NoEvidence
            ),
            Err("issuer_retirement_unproven")
        ));
        assert!(matches!(
            protocol_sequence::prepare_cleanup(
                &binding,
                &source,
                &stored,
                1,
                &effect,
                &cleanup_ref,
                &retirement
            ),
            Err("issuer_retirement_unproven")
        ));
        // Simulated existing authority fences the exact issuer. Its held lock
        // would wait for any broker admission already in progress to finish.
        *retired.lock().unwrap() = true;
        let cleanup = protocol_sequence::prepare_cleanup(
            &binding,
            &source,
            &stored,
            1,
            &effect,
            &cleanup_ref,
            &retirement,
        )
        .unwrap();
        let compacted = serde_json::json!({"ok":true,"revision":2,"found":true,"value":cleanup.payload()["value"]});
        assert_eq!(
            protocol_sequence::lookup(&source, &compacted, 1).unwrap(),
            Lookup::Retired
        );
        continue_tx.send(()).unwrap();
        let (result, mut broker) = thread.join().unwrap();
        assert_eq!(result.unwrap_err(), "issuer_retired");
        assert!(matches!(
            broker.dispatch("example.provider_a", Command::Poll {}),
            Response::Pending
        ));
    });
}
