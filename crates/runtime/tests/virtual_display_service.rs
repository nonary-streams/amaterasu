use amaterasu_contracts::{ApiMethodApproval, Command, Grants, RuntimeConfig};
use amaterasu_module_sdk::{
    RuntimeClient,
    abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK},
    service::{CallOptions, ServiceError, ServiceMethod},
    virtual_display_service::{
        self as display, Capabilities, DisplayService, EffectState, Layout, Mode, ModeAcceptance,
        ModeObservation, OwnerReference, Provider, Reply, Request, TopologyRevision,
    },
};
use amaterasu_runtime::Broker;
use std::{
    collections::BTreeMap,
    ffi::c_void,
    sync::{Arc, Mutex},
};

struct Endpoint {
    broker: Arc<Mutex<Broker>>,
    principal: &'static str,
}
unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, reply: ReplySink) -> i32 {
    // SAFETY: endpoint and input remain valid during this synchronous call.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    let command = serde_json::from_slice::<Command>(unsafe { input.read().unwrap() }).unwrap();
    let response = endpoint
        .broker
        .lock()
        .unwrap()
        .dispatch(endpoint.principal, command);
    let bytes = serde_json::to_vec(&response).unwrap();
    // SAFETY: the sink is valid for the duration of this invocation and copies bytes.
    unsafe { reply.send(&bytes) };
    STATUS_OK
}
fn client(endpoint: &Endpoint) -> RuntimeClient {
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *const Endpoint).cast_mut().cast(),
        invoke,
    };
    // SAFETY: test endpoints outlive their clients and all calls.
    unsafe { RuntimeClient::from_endpoint(api) }
}
fn topology(revision: u64) -> TopologyRevision {
    TopologyRevision {
        domain_id: "desktop".into(),
        revision,
    }
}
fn owner() -> OwnerReference {
    OwnerReference {
        identity: "display".into(),
        reference_id: "session".into(),
    }
}
fn create(id: &str, revision: u64) -> Request {
    Request::Create {
        owner: owner(),
        operation_id: id.into(),
        expected: topology(revision),
        mode: Mode {
            width: 1920,
            height: 1080,
            refresh_millihz: 60000,
            hdr: false,
        },
        acceptance: ModeAcceptance {
            max_refresh_delta_ppm: 20_000,
            require_hdr_match: true,
        },
        name: "Test".into(),
        layout: Layout::Extended,
        scale: None,
    }
}
fn options(id: &str) -> CallOptions<'_> {
    CallOptions {
        operation_id: id,
        timeout_ms: 5000,
        parent: None,
    }
}

#[derive(Default)]
struct FakeProvider {
    effects: BTreeMap<(String, String, String), EffectState>,
    outcomes: BTreeMap<(String, String), (Request, Reply)>,
    revision: u64,
    mutations: usize,
    completion_losses: usize,
    shutdown: bool,
    cancel_during_execute: Option<(RuntimeClient, u64)>,
}
impl Provider for FakeProvider {
    fn execute(&mut self, caller: &str, request: &Request) -> Reply {
        let key = |owner: &OwnerReference| {
            (
                caller.to_string(),
                owner.identity.clone(),
                owner.reference_id.clone(),
            )
        };
        let result = match request {
            Request::Capabilities => Reply::Capabilities {
                capabilities: Capabilities {
                    create: true,
                    reconfigure: false,
                    borrow: false,
                    recovery: true,
                    topology: Some(topology(self.revision)),
                    layouts: vec![Layout::Extended],
                },
            },
            Request::Create {
                owner,
                operation_id,
                expected,
                mode,
                ..
            } => {
                let operation_key = (caller.to_string(), operation_id.clone());
                if let Some((previous, reply)) = self.outcomes.get(&operation_key) {
                    if previous == request {
                        reply.clone()
                    } else {
                        Reply::Conflict
                    }
                } else if expected != &topology(self.revision) {
                    Reply::StaleTopology {
                        current: topology(self.revision),
                    }
                } else if self.outcomes.len() >= 16 {
                    Reply::ResourceExhausted
                } else {
                    self.mutations += 1;
                    self.revision += 1;
                    self.effects.insert(key(owner), EffectState::Applied);
                    let reply = Reply::Effect {
                        state: EffectState::Applied,
                        topology: Some(topology(self.revision)),
                        observation: ModeObservation::Observed {
                            mode: Mode {
                                refresh_millihz: mode.refresh_millihz.saturating_sub(60),
                                ..mode.clone()
                            },
                        },
                    };
                    self.outcomes
                        .insert(operation_key, (request.clone(), reply.clone()));
                    reply
                }
            }
            Request::Inspect { owner } => match self.effects.get(&key(owner)) {
                Some(state) => Reply::Effect {
                    state: *state,
                    topology: Some(topology(self.revision)),
                    observation: ModeObservation::NotObserved,
                },
                None => Reply::NotOwned,
            },
            Request::Recover {
                owner,
                operation_id,
            } => match self
                .outcomes
                .get(&(caller.to_string(), operation_id.clone()))
            {
                Some((
                    Request::Create {
                        owner: original, ..
                    },
                    reply,
                )) if original == owner => reply.clone(),
                _ => Reply::NotOwned,
            },
            _ => Reply::Unavailable,
        };
        if let Some((client, id)) = self.cancel_during_execute.take() {
            assert!(matches!(
                client.send(Command::Cancel { request: id }).unwrap(),
                amaterasu_contracts::Response::Ok
            ));
        }
        result
    }
    fn completion_lost(&mut self, _: &str, _: &Request, _: &Reply) {
        self.completion_losses += 1;
    }
    fn shutdown(&mut self) {
        self.shutdown = true;
    }
}
fn exchange(
    provider: &RuntimeClient,
    consumer: &RuntimeClient,
    implementation: &mut impl Provider,
    id: &str,
    request: Request,
) -> Reply {
    let call = display::call(consumer, options(id), request).unwrap();
    let incoming = provider.poll_serialized().unwrap().unwrap();
    display::handle(provider, implementation, &incoming).unwrap();
    let reply = call.poll().unwrap().unwrap().reply;
    assert!(call.release().is_ok());
    reply
}

#[test]
fn display_contract_is_caller_scoped_recoverable_and_cancellation_is_not_undo() {
    let method = DisplayService::method();
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "provider".into(),
        Grants {
            provide: [method.service.clone()].into(),
            ..Default::default()
        },
    );
    for caller in ["alice", "bob"] {
        config.principals.insert(
            caller.into(),
            Grants {
                call: [method.service.clone()].into(),
                ..Default::default()
            },
        );
    }
    config.api_methods.push(ApiMethodApproval {
        provider: "provider".into(),
        callers: ["alice".into(), "bob".into()].into(),
        method,
    });
    let broker = Arc::new(Mutex::new(Broker::new(config).unwrap()));
    let p = Endpoint {
        broker: broker.clone(),
        principal: "provider",
    };
    let a = Endpoint {
        broker: broker.clone(),
        principal: "alice",
    };
    let b = Endpoint {
        broker,
        principal: "bob",
    };
    let provider = client(&p);
    let alice = client(&a);
    let bob = client(&b);
    display::register(&provider).unwrap();
    let mut implementation = FakeProvider::default();
    assert!(
        matches!(exchange(&provider, &alice, &mut implementation, "caps", Request::Capabilities), Reply::Capabilities { capabilities } if !capabilities.borrow)
    );
    assert_eq!(
        exchange(
            &provider,
            &alice,
            &mut implementation,
            "borrow",
            Request::Borrow { owner: owner() }
        ),
        Reply::Unavailable
    );
    let mut invalid = create("invalid", 0);
    if let Request::Create { acceptance, .. } = &mut invalid {
        acceptance.max_refresh_delta_ppm = 1_000_001;
    }
    assert_eq!(
        exchange(&provider, &alice, &mut implementation, "invalid", invalid),
        Reply::InvalidRequest
    );
    assert_eq!(implementation.mutations, 0);
    let stale = exchange(
        &provider,
        &alice,
        &mut implementation,
        "stale",
        create("stale", 1),
    );
    assert_eq!(
        stale,
        Reply::StaleTopology {
            current: topology(0)
        }
    );
    let call = display::call(&alice, options("lost-admission"), create("logical", 0)).unwrap();
    let retry = display::call(&alice, options("lost-admission"), create("logical", 0)).unwrap();
    assert_eq!(call.id(), retry.id());
    let incoming = provider.poll_serialized().unwrap().unwrap();
    display::handle(&provider, &mut implementation, &incoming).unwrap();
    assert_eq!(call.poll().unwrap(), retry.poll().unwrap());
    assert!(matches!(call.poll().unwrap().unwrap().reply,
        Reply::Effect { observation: ModeObservation::Observed { mode }, .. }
        if mode.refresh_millihz == 59_940 && !mode.hdr));
    assert_eq!(implementation.mutations, 1);
    assert!(call.release().is_ok());
    assert_eq!(
        exchange(
            &provider,
            &bob,
            &mut implementation,
            "inspect-bob",
            Request::Inspect { owner: owner() }
        ),
        Reply::NotOwned
    );
    assert_eq!(
        exchange(
            &provider,
            &bob,
            &mut implementation,
            "recover-bob",
            Request::Recover {
                owner: owner(),
                operation_id: "logical".into()
            }
        ),
        Reply::NotOwned
    );
    assert!(matches!(
        exchange(
            &provider,
            &alice,
            &mut implementation,
            "retry-logical",
            create("logical", 0)
        ),
        Reply::Effect {
            state: EffectState::Applied,
            ..
        }
    ));
    assert_eq!(implementation.mutations, 1);
    assert_eq!(
        exchange(
            &provider,
            &alice,
            &mut implementation,
            "collision",
            create("logical", 1)
        ),
        Reply::Conflict
    );
    let cancelled = display::call(&alice, options("cancel"), create("uncertain", 1)).unwrap();
    let incoming = provider.poll_serialized().unwrap().unwrap();
    implementation.cancel_during_execute = Some((client(&a), cancelled.id()));
    assert!(
        matches!(display::handle(&provider, &mut implementation, &incoming), Err(ServiceError::Broker { code, .. }) if code == "request_ended")
    );
    assert_eq!(implementation.completion_losses, 1);
    assert!(
        matches!(cancelled.poll(), Err(ServiceError::Broker { code, .. }) if code == "cancelled")
    );
    assert!(cancelled.release().is_ok());
    implementation.shutdown();
    assert!(implementation.shutdown);
    // Model a restart with a retained journal. This does not test disk durability.
    let mut restarted = FakeProvider {
        effects: std::mem::take(&mut implementation.effects),
        outcomes: std::mem::take(&mut implementation.outcomes),
        revision: implementation.revision,
        mutations: implementation.mutations,
        ..Default::default()
    };
    assert!(matches!(
        exchange(
            &provider,
            &alice,
            &mut restarted,
            "recover",
            Request::Recover {
                owner: owner(),
                operation_id: "uncertain".into()
            }
        ),
        Reply::Effect {
            state: EffectState::Applied,
            ..
        }
    ));
    assert_eq!(restarted.mutations, 2);
}

// A second implementation uses an append-only bounded journal instead of maps.
#[derive(Default)]
struct JournalProvider {
    records: Vec<(String, Request, Reply)>,
    revision: u64,
}
impl Provider for JournalProvider {
    fn execute(&mut self, caller: &str, request: &Request) -> Reply {
        match request {
            Request::Create { owner: _, operation_id, expected, .. } => {
                if let Some((_, old, reply)) = self.records.iter().find(|(principal, old, _)| {
                    principal == caller && matches!(old, Request::Create { operation_id: id, .. } if id == operation_id)
                }) {
                    return if old == request { reply.clone() } else { Reply::Conflict };
                }
                if expected != &topology(self.revision) { return Reply::StaleTopology { current: topology(self.revision) }; }
                if self.records.len() == 16 { return Reply::ResourceExhausted; }
                self.revision += 1;
                let reply = Reply::Effect { state: EffectState::Applied, topology: Some(topology(self.revision)), observation: ModeObservation::NotObserved };
                self.records.push((caller.into(), request.clone(), reply.clone()));
                reply
            }
            Request::Inspect { owner } => {
                if self.records.iter().any(|(principal, request, _)| principal == caller && matches!(request, Request::Create { owner: saved, .. } if saved == owner)) {
                    Reply::Effect { state: EffectState::Applied, topology: Some(topology(self.revision)), observation: ModeObservation::NotObserved }
                } else { Reply::NotOwned }
            }
            Request::Recover { owner, operation_id } => self.records.iter().find_map(|(principal, request, reply)| {
                (principal == caller && matches!(request, Request::Create { owner: saved, operation_id: id, .. } if saved == owner && id == operation_id)).then(|| reply.clone())
            }).unwrap_or(Reply::NotOwned),
            _ => Reply::Unavailable,
        }
    }
    fn completion_lost(&mut self, _: &str, _: &Request, _: &Reply) {}
    fn shutdown(&mut self) {}
}

fn unchanged_consumer(implementation: &mut impl Provider, provider_name: &'static str) {
    let method = DisplayService::method();
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        provider_name.into(),
        Grants {
            provide: [method.service.clone()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "consumer".into(),
        Grants {
            call: [method.service.clone()].into(),
            ..Default::default()
        },
    );
    config.api_methods.push(ApiMethodApproval {
        provider: provider_name.into(),
        callers: ["consumer".into()].into(),
        method,
    });
    let broker = Arc::new(Mutex::new(Broker::new(config).unwrap()));
    let p = Endpoint {
        broker: broker.clone(),
        principal: provider_name,
    };
    let c = Endpoint {
        broker,
        principal: "consumer",
    };
    let provider = client(&p);
    let consumer = client(&c);
    display::register(&provider).unwrap();
    let first = exchange(
        &provider,
        &consumer,
        implementation,
        "create",
        create("one", 0),
    );
    assert!(matches!(
        first,
        Reply::Effect {
            state: EffectState::Applied,
            ..
        }
    ));
    assert_eq!(
        exchange(
            &provider,
            &consumer,
            implementation,
            "retry",
            create("one", 0)
        ),
        first
    );
    assert_eq!(
        exchange(
            &provider,
            &consumer,
            implementation,
            "recover",
            Request::Recover {
                owner: owner(),
                operation_id: "one".into()
            }
        ),
        first
    );
    assert_eq!(
        exchange(
            &provider,
            &consumer,
            implementation,
            "stale",
            create("two", 0)
        ),
        Reply::StaleTopology {
            current: topology(1)
        }
    );
}

#[test]
fn unchanged_consumer_uses_independent_startup_selected_providers() {
    unchanged_consumer(&mut FakeProvider::default(), "provider.first");
    unchanged_consumer(&mut JournalProvider::default(), "provider.second");
}

#[test]
fn provider_adapter_retires_the_provider_on_drop() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Retirement(Arc<AtomicUsize>);
    impl Provider for Retirement {
        fn execute(&mut self, _: &str, _: &Request) -> Reply {
            Reply::Unavailable
        }
        fn completion_lost(&mut self, _: &str, _: &Request, _: &Reply) {}
        fn shutdown(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let retired = Arc::new(AtomicUsize::new(0));
    {
        let _adapter = display::ProviderAdapter::new(Retirement(retired.clone()));
        assert_eq!(retired.load(Ordering::SeqCst), 0);
    }
    assert_eq!(retired.load(Ordering::SeqCst), 1);
}
