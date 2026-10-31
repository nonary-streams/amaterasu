use super::*;
use crate::service_adapter::Backend;
use amaterasu_contracts::{ApiMethodApproval, Command, Grants, RuntimeConfig};
use amaterasu_module_sdk::{
    RuntimeClient,
    abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK},
    service::{CallOptions, ServiceMethod},
    virtual_display_service::{
        self as display, DisplayService, EffectState, Layout, Mode, ModeAcceptance, OwnerReference,
        Reply, Request,
    },
};
use amaterasu_runtime::Broker;
use std::{
    ffi::c_void,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

struct Endpoint {
    broker: Arc<Mutex<Broker>>,
    principal: &'static str,
}
unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, reply: ReplySink) -> i32 {
    // SAFETY: endpoint and input stay live during this synchronous invocation.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let result = endpoint
        .broker
        .lock()
        .unwrap()
        .dispatch(endpoint.principal, command);
    let bytes = serde_json::to_vec(&result).unwrap();
    // SAFETY: the sink copies the provided bytes before returning.
    unsafe { reply.send(&bytes) };
    STATUS_OK
}
struct CancelCompletionEndpoint {
    broker: Arc<Mutex<Broker>>,
    request_id: u64,
}
unsafe extern "C" fn cancel_completion_invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> i32 {
    // SAFETY: the endpoint and input remain valid for the synchronous invocation.
    let endpoint = unsafe { &*context.cast::<CancelCompletionEndpoint>() };
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let mut broker = endpoint.broker.lock().unwrap();
    if matches!(command, Command::Complete { request, .. } if request == endpoint.request_id) {
        assert!(matches!(
            broker.dispatch(
                "alice",
                Command::Cancel {
                    request: endpoint.request_id
                }
            ),
            amaterasu_contracts::Response::Ok
        ));
    }
    let result = broker.dispatch("provider", command);
    let bytes = serde_json::to_vec(&result).unwrap();
    // SAFETY: the sink copies the bytes before returning.
    unsafe { reply.send(&bytes) };
    STATUS_OK
}
fn cancellation_client(endpoint: &CancelCompletionEndpoint) -> RuntimeClient {
    let host = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *const CancelCompletionEndpoint)
            .cast_mut()
            .cast(),
        invoke: cancel_completion_invoke,
    };
    // SAFETY: the endpoint remains alive throughout the injected invocation.
    unsafe { RuntimeClient::from_endpoint(host) }
}
fn client(endpoint: &Endpoint) -> RuntimeClient {
    let host = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *const Endpoint).cast_mut().cast(),
        invoke,
    };
    // SAFETY: endpoints remain alive until all requests and clients finish.
    unsafe { RuntimeClient::from_endpoint(host) }
}
fn options(id: &str) -> CallOptions<'_> {
    CallOptions {
        timeout_ms: 5000,
        operation_id: id,
        parent: None,
    }
}
fn owner(id: &str) -> OwnerReference {
    OwnerReference {
        identity: id.into(),
        reference_id: "stream".into(),
    }
}
fn create(
    owner: OwnerReference,
    operation_id: &str,
    expected: display::TopologyRevision,
) -> Request {
    Request::Create {
        owner,
        operation_id: operation_id.into(),
        expected,
        mode: Mode {
            width: 1920,
            height: 1080,
            refresh_millihz: 60000,
            hdr: false,
        },
        acceptance: ModeAcceptance {
            max_refresh_delta_ppm: 0,
            require_hdr_match: true,
        },
        name: "Display".into(),
        layout: Layout::Extended,
        scale: Some(0),
    }
}

struct TransientInspection<'a> {
    manager: &'a mut Manager,
}
impl Backend for TransientInspection<'_> {
    type Admission = Admission;
    fn admit(&mut self, caller: &str, operation: Operation) -> (Value, Option<Admission>) {
        if matches!(
            operation,
            Operation::Status {
                identity: Some(_),
                ..
            }
        ) {
            (json!({"ok":false,"code":"temporary_failure"}), None)
        } else {
            self.manager.admit(caller, operation)
        }
    }
    fn completion_lost(&mut self, admission: Option<Admission>) {
        self.manager.completion_lost(admission);
    }
    fn remove_owned(
        &mut self,
        caller: &str,
        owner: &OwnerReference,
    ) -> crate::service_adapter::RemovalOutcome {
        self.manager
            .admit_removal(caller, &owner.identity, &owner.reference_id)
    }
}

struct CancelAfterCreate<'a> {
    manager: &'a mut Manager,
    client: &'a RuntimeClient,
    id: u64,
}
impl Backend for CancelAfterCreate<'_> {
    type Admission = Admission;
    fn admit(&mut self, caller: &str, operation: Operation) -> (Value, Option<Admission>) {
        let creating = matches!(operation, Operation::Create { .. });
        let result = self.manager.admit(caller, operation);
        if creating {
            self.client
                .require_success(Command::Cancel { request: self.id })
                .unwrap();
        }
        result
    }
    fn completion_lost(&mut self, admission: Option<Admission>) {
        self.manager.completion_lost(admission);
    }
    fn remove_owned(
        &mut self,
        caller: &str,
        owner: &OwnerReference,
    ) -> crate::service_adapter::RemovalOutcome {
        self.manager
            .admit_removal(caller, &owner.identity, &owner.reference_id)
    }
}

#[test]
fn production_linux_manager_routes_fenced_requests_to_fake_worker_via_broker() {
    let method = DisplayService::method();
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "provider".into(),
        Grants {
            provide: [method.service.clone(), crate::SERVICE.into()].into(),
            ..Default::default()
        },
    );
    for name in ["alice", "bob"] {
        config.principals.insert(
            name.into(),
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
    let mut manager = Manager::start(Settings::default()).unwrap();
    manager.settings.enabled = true;
    manager.ready = true;
    let (sender, jobs) = mpsc::sync_channel(16);
    let (events, receiver) = mpsc::sync_channel(16);
    let (seen, observed) = mpsc::channel();
    manager.sender = Some(sender);
    manager.receiver = Some(receiver);
    // This worker only acknowledges messages and records them; it touches no display or settings.
    manager.worker = Some(std::thread::spawn(move || {
        while let Ok(job) = jobs.recv() {
            match job {
                Work::Create { .. } => {
                    seen.send("create").unwrap();
                }
                Work::References { reply, .. } => {
                    reply.send(Ok(())).unwrap();
                }
                Work::Remove { .. } => {
                    seen.send("remove").unwrap();
                }
                _ => panic!("unexpected fake worker request"),
            }
        }
    }));
    let mut adapter = crate::service_adapter::Adapter::default();
    let exchange = |manager: &mut Manager,
                    adapter: &mut crate::service_adapter::Adapter,
                    consumer: &RuntimeClient,
                    id: &str,
                    request: Request| {
        let call = display::call(consumer, options(id), request).unwrap();
        let incoming = provider.poll_serialized().unwrap().unwrap();
        crate::handle_display_api(&provider, manager, adapter, incoming).unwrap();
        let reply = call.poll().unwrap().unwrap().reply;
        assert!(call.release().is_ok());
        reply
    };
    let caps = exchange(
        &mut manager,
        &mut adapter,
        &alice,
        "caps",
        Request::Capabilities,
    );
    let Reply::Capabilities { capabilities } = caps else {
        panic!("missing capabilities")
    };
    assert!(capabilities.create);
    let initial = capabilities.topology.unwrap();
    let first = owner("00000000-0000-0000-0000-000000000001");
    let request = create(first.clone(), "first", initial.clone());
    assert!(matches!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "create",
            request.clone()
        ),
        Reply::Effect {
            state: EffectState::Pending,
            ..
        }
    ));
    assert_eq!(
        observed.recv_timeout(Duration::from_secs(1)).unwrap(),
        "create"
    );
    assert!(matches!(
        exchange(&mut manager, &mut adapter, &alice, "retry", request),
        Reply::Effect {
            state: EffectState::Pending,
            ..
        }
    ));
    let call = display::call(
        &alice,
        options("recover-transient"),
        Request::Recover {
            owner: first.clone(),
            operation_id: "first".into(),
        },
    )
    .unwrap();
    let incoming = provider.poll_serialized().unwrap().unwrap();
    crate::handle_display_api(
        &provider,
        &mut TransientInspection {
            manager: &mut manager,
        },
        &mut adapter,
        incoming,
    )
    .unwrap();
    assert!(matches!(
        call.poll().unwrap().unwrap().reply,
        Reply::Effect {
            state: EffectState::Unknown,
            ..
        }
    ));
    assert!(call.release().is_ok());
    events
        .send(Event::Created {
            key: ("alice".into(), first.identity.clone()),
            result: Err("fake failure".into()),
        })
        .unwrap();
    assert!(matches!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "recover-failed",
            Request::Recover {
                owner: first.clone(),
                operation_id: "first".into()
            }
        ),
        Reply::Effect {
            state: EffectState::Failed,
            ..
        }
    ));
    let original_domain = manager.domain_id.clone();
    manager.domain_id = "different-provider-session".into();
    assert!(matches!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "recover-new-domain",
            Request::Recover {
                owner: first.clone(),
                operation_id: "first".into()
            }
        ),
        Reply::Effect {
            state: EffectState::Unknown,
            ..
        }
    ));
    manager.domain_id = original_domain;
    events
        .send(Event::Created {
            key: ("alice".into(), first.identity.clone()),
            result: Ok(Record {
                owner: "alice".into(),
                specification: Specification {
                    hdr_enabled: false,
                    identity: first.identity.clone(),
                    width: 1920,
                    height: 1080,
                    refresh_millihz: 60000,
                    name: "Display".into(),
                },
                output: "virtual-1".into(),
                layout: None,
                references: Some(["stream".into()].into()),
                transition: None,
            }),
        })
        .unwrap();
    let ready = exchange(
        &mut manager,
        &mut adapter,
        &alice,
        "ready",
        Request::Inspect {
            owner: first.clone(),
        },
    );
    assert!(matches!(
        ready,
        Reply::Effect {
            state: EffectState::Applied,
            observation: display::ModeObservation::NotObserved,
            ..
        }
    ));
    assert_eq!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "recover-ready",
            Request::Recover {
                owner: first.clone(),
                operation_id: "first".into()
            }
        ),
        ready
    );
    let mut conflicting = create(first.clone(), "first", initial.clone());
    if let Request::Create { name, .. } = &mut conflicting {
        *name = "Changed".into();
    }
    assert_eq!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "conflicting",
            conflicting
        ),
        Reply::Conflict
    );
    // A cancelled conflict never admitted new work and must not poison the
    // existing successful operation's recovery record.
    let mut cancelled_conflict = create(first.clone(), "first", initial.clone());
    if let Request::Create { name, .. } = &mut cancelled_conflict {
        *name = "Conflict again".into();
    }
    let call = display::call(&alice, options("cancel-conflict"), cancelled_conflict).unwrap();
    let incoming = provider.poll_serialized().unwrap().unwrap();
    let fault = CancelCompletionEndpoint {
        broker: p.broker.clone(),
        request_id: call.id(),
    };
    crate::handle_display_api(
        &cancellation_client(&fault),
        &mut manager,
        &mut adapter,
        incoming,
    )
    .unwrap();
    assert!(call.poll().is_err());
    assert!(call.release().is_ok());
    assert_eq!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "recover-after-conflict",
            Request::Recover {
                owner: first.clone(),
                operation_id: "first".into()
            }
        ),
        ready
    );
    assert!(observed.try_recv().is_err());
    assert_eq!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "unknown-recovery",
            Request::Recover {
                owner: first.clone(),
                operation_id: "unknown".into()
            }
        ),
        Reply::Effect {
            state: EffectState::Unknown,
            topology: None,
            observation: display::ModeObservation::NotObserved
        }
    );
    assert_eq!(
        exchange(
            &mut manager,
            &mut adapter,
            &bob,
            "foreign",
            Request::Inspect {
                owner: first.clone()
            }
        ),
        Reply::NotOwned
    );
    assert!(matches!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "stale",
            create(
                owner("00000000-0000-0000-0000-000000000002"),
                "stale",
                initial
            )
        ),
        Reply::StaleTopology { .. }
    ));
    let Reply::Capabilities { capabilities } = exchange(
        &mut manager,
        &mut adapter,
        &alice,
        "before-remove",
        Request::Capabilities,
    ) else {
        panic!("missing capabilities")
    };
    let borrow_id = "held-borrow".to_string();
    manager.borrows.insert(
        borrow_id.clone(),
        Borrow {
            key: ("alice".into(), first.identity.clone()),
            reference_id: first.reference_id.clone(),
            observation: observer::Observation {
                global: 1,
                width: 1920,
                height: 1080,
                refresh: 60000,
                generation: 1,
                compositor_id: [1; 32],
            },
            valid: true,
        },
    );
    let borrowed_request = Request::Remove {
        owner: first.clone(),
        operation_id: "remove-first".into(),
        expected: capabilities.topology.clone().unwrap(),
    };
    assert_eq!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "borrowed-remove",
            borrowed_request
        ),
        Reply::Unavailable
    );
    assert!(observed.try_recv().is_err());
    assert!(matches!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "recover-after-rejection",
            Request::Recover {
                owner: first.clone(),
                operation_id: "first".into()
            }
        ),
        Reply::Effect {
            state: EffectState::Applied,
            ..
        }
    ));
    let (release, _) = manager.admit(
        "alice",
        Operation::ReleaseBorrow {
            identity: first.identity.clone(),
            reference_id: first.reference_id.clone(),
            borrow_id,
        },
    );
    assert_eq!(release["ok"], true);
    let Reply::Capabilities { capabilities } = exchange(
        &mut manager,
        &mut adapter,
        &alice,
        "after-borrow",
        Request::Capabilities,
    ) else {
        panic!("missing capabilities")
    };
    let removal_topology = capabilities.topology.unwrap();
    let removal = Request::Remove {
        owner: first.clone(),
        operation_id: "remove-first".into(),
        expected: removal_topology.clone(),
    };
    assert!(matches!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "remove-first",
            removal.clone()
        ),
        Reply::Effect {
            state: EffectState::Pending,
            ..
        }
    ));
    assert_eq!(
        observed.recv_timeout(Duration::from_secs(1)).unwrap(),
        "remove"
    );
    assert_eq!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "release-pending",
            Request::ReleaseOperation {
                owner: first.clone(),
                operation_id: "remove-first".into()
            }
        ),
        Reply::OperationNotSettled
    );
    events
        .send(Event::Removed {
            key: ("alice".into(), first.identity.clone()),
            result: Err("fake cleanup failure".into()),
        })
        .unwrap();
    assert!(matches!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "failed-removal",
            Request::Recover {
                owner: first.clone(),
                operation_id: "remove-first".into()
            }
        ),
        Reply::Effect {
            state: EffectState::Failed,
            ..
        }
    ));
    assert_eq!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "release-failed",
            Request::ReleaseOperation {
                owner: first.clone(),
                operation_id: "remove-first".into()
            }
        ),
        Reply::OperationNotSettled
    );
    assert!(matches!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "retry-stale",
            Request::RetryCleanup {
                owner: first.clone(),
                operation_id: "remove-first".into(),
                expected: removal_topology
            }
        ),
        Reply::StaleTopology { .. }
    ));
    let Reply::Capabilities { capabilities } = exchange(
        &mut manager,
        &mut adapter,
        &alice,
        "retry-caps",
        Request::Capabilities,
    ) else {
        panic!("missing capabilities")
    };
    assert!(matches!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "retry-cleanup",
            Request::RetryCleanup {
                owner: first.clone(),
                operation_id: "remove-first".into(),
                expected: capabilities.topology.unwrap()
            }
        ),
        Reply::Effect {
            state: EffectState::Pending,
            ..
        }
    ));
    assert_eq!(
        observed.recv_timeout(Duration::from_secs(1)).unwrap(),
        "remove"
    );
    events
        .send(Event::Removed {
            key: ("alice".into(), first.identity.clone()),
            result: Ok(()),
        })
        .unwrap();
    assert!(matches!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "settled",
            Request::Recover {
                owner: first.clone(),
                operation_id: "remove-first".into()
            }
        ),
        Reply::Effect {
            state: EffectState::Completed,
            ..
        }
    ));
    assert_eq!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "release-settled",
            Request::ReleaseOperation {
                owner: first.clone(),
                operation_id: "remove-first".into()
            }
        ),
        Reply::OperationReleased
    );
    let Reply::Capabilities { capabilities } = exchange(
        &mut manager,
        &mut adapter,
        &alice,
        "caps2",
        Request::Capabilities,
    ) else {
        panic!("missing capabilities")
    };
    let second = owner("00000000-0000-0000-0000-000000000002");
    let call = display::call(
        &alice,
        options("cancel"),
        create(second.clone(), "uncertain", capabilities.topology.unwrap()),
    )
    .unwrap();
    let incoming = provider.poll_serialized().unwrap().unwrap();
    let mut cancelling = CancelAfterCreate {
        manager: &mut manager,
        client: &alice,
        id: call.id(),
    };
    crate::handle_display_api(&provider, &mut cancelling, &mut adapter, incoming).unwrap();
    assert!(call.poll().is_err());
    assert!(call.release().is_ok());
    assert_eq!(
        observed.recv_timeout(Duration::from_secs(1)).unwrap(),
        "create"
    );
    assert_eq!(
        observed.recv_timeout(Duration::from_secs(1)).unwrap(),
        "remove"
    );
    assert!(matches!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "recovery",
            Request::Recover {
                owner: second.clone(),
                operation_id: "uncertain".into()
            }
        ),
        Reply::Effect {
            state: EffectState::Pending,
            ..
        }
    ));
    assert_eq!(
        exchange(
            &mut manager,
            &mut adapter,
            &bob,
            "foreign-release",
            Request::ReleaseOperation {
                owner: second.clone(),
                operation_id: "uncertain".into()
            }
        ),
        Reply::NotOwned
    );
    assert_eq!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "release",
            Request::ReleaseOperation {
                owner: second.clone(),
                operation_id: "uncertain".into()
            }
        ),
        Reply::OperationNotSettled
    );
    events
        .send(Event::Removed {
            key: ("alice".into(), second.identity.clone()),
            result: Ok(()),
        })
        .unwrap();
    assert!(matches!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "cancel-settled",
            Request::Recover {
                owner: second.clone(),
                operation_id: "uncertain".into()
            }
        ),
        Reply::Effect {
            state: EffectState::Completed,
            ..
        }
    ));
    assert_eq!(
        exchange(
            &mut manager,
            &mut adapter,
            &alice,
            "release-cancel-settled",
            Request::ReleaseOperation {
                owner: second,
                operation_id: "uncertain".into()
            }
        ),
        Reply::OperationReleased
    );
    drop(manager); // Joins the fake worker without touching OS display state.
}
