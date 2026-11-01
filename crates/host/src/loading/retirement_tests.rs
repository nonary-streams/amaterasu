//! Real broker and State integration for the host's optional retirement path.
use super::{FeatureLibrary, LoadedComponents, RuntimeLibrary, retirement_component_order};
use amaterasu_abi::{
    AbiHeader, BorrowedBytes, HostApi, ModuleApi, ModuleRetirementApi, ReplySink, RuntimeApi,
    STATUS_INTERNAL_ERROR, STATUS_OK, Status,
};
use amaterasu_contracts::{
    ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Command, Grants,
    HOST_LIFECYCLE_CONTROL_PRINCIPAL, Response, RuntimeConfig, SelectedService,
    effect_fence::{
        EffectFenceCommand, EffectFencePolicy, EffectScope, EffectSource, GuardedEffect,
        ProtectedMethod, ProviderIncarnation, SourceSlotLease,
    },
};
use amaterasu_module_sdk::{
    ManagedModule, ModuleResult, RetirableModule, RetirementProgress, RuntimeClient,
};
use amaterasu_runtime::Broker;
use amaterasu_state::test_support::TestStateStore;
use libloading::Library;
use serde_json::json;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn manifest_component(id: &str) -> amaterasu_package::ComponentManifest {
    amaterasu_package::ComponentManifest {
        id: id.into(),
        version: "1".into(),
        file: String::new(),
        sha256: String::new(),
        size: 0,
        abi_major: 1,
        runtime: false,
        requires: Default::default(),
        service_provides: Vec::new(),
        service_requires: Vec::new(),
        requested: Grants::default(),
    }
}

#[test]
fn state_ordering_rejects_incompatible_dependency_cycles() {
    let mut state = manifest_component("state");
    state.requires.insert("provider".into(), "1".into());
    let instance = ProviderIncarnation {
        version: 1,
        id: "a".repeat(32),
    };
    assert!(
        retirement_component_order(
            &[manifest_component("provider"), state],
            &[("provider".into(), instance)],
            Some("state")
        )
        .is_err()
    );
}

struct Harness {
    broker: Mutex<Broker>,
    store: TestStateStore,
    fail_next_set: AtomicBool,
    destroyed: AtomicUsize,
}

unsafe extern "C" fn host_invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> Status {
    // SAFETY: the test retains the Arc and readable request throughout the call.
    let harness = unsafe { &*context.cast::<Harness>() };
    let Ok(bytes) = (unsafe { input.read() }) else {
        return STATUS_INTERNAL_ERROR;
    };
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
        return STATUS_INTERNAL_ERROR;
    };
    let response = harness
        .broker
        .lock()
        .unwrap()
        .dispatch(HOST_LIFECYCLE_CONTROL_PRINCIPAL, command);
    let bytes = serde_json::to_vec(&response).unwrap();
    // SAFETY: reply is borrowed for this synchronous host call.
    unsafe { reply.send(&bytes) };
    STATUS_OK
}
unsafe extern "C" fn state_tick(instance: *mut c_void) -> Status {
    // SAFETY: this fixture owns a boxed Arc until state_destroy.
    let harness = unsafe { &*instance.cast::<Arc<Harness>>() };
    let response = harness
        .broker
        .lock()
        .unwrap()
        .dispatch("state", Command::Poll {});
    match response {
        Response::Pending => STATUS_OK,
        Response::Request {
            id,
            caller,
            payload,
            ..
        } => {
            let result = if payload.get("action").and_then(serde_json::Value::as_str) == Some("set")
                && harness.fail_next_set.swap(false, Ordering::SeqCst)
            {
                json!({"ok":false,"error":{"code":"storage_error"}})
            } else {
                harness.store.handle(&caller, payload)
            };
            if matches!(
                harness.broker.lock().unwrap().dispatch(
                    "state",
                    Command::Complete {
                        request: id,
                        payload: result
                    }
                ),
                Response::Ok
            ) {
                STATUS_OK
            } else {
                STATUS_INTERNAL_ERROR
            }
        }
        _ => STATUS_INTERNAL_ERROR,
    }
}
unsafe extern "C" fn state_destroy(instance: *mut c_void) {
    // SAFETY: exactly one boxed Arc is transferred to the fixture.
    drop(unsafe { Box::from_raw(instance.cast::<Arc<Harness>>()) });
}
unsafe extern "C" fn runtime_destroy(instance: *mut c_void) {
    // SAFETY: exactly one boxed marker is transferred to the fixture.
    drop(unsafe { Box::from_raw(instance.cast::<()>()) });
}
unsafe extern "C" fn unused_runtime(_: BorrowedBytes, _: *mut *mut c_void) -> Status {
    STATUS_INTERNAL_ERROR
}
unsafe extern "C" fn unused_endpoint(_: *mut c_void, _: BorrowedBytes, _: *mut HostApi) -> Status {
    STATUS_INTERNAL_ERROR
}
unsafe extern "C" fn unused_create(_: *const HostApi, _: *mut *mut c_void) -> Status {
    STATUS_INTERNAL_ERROR
}
unsafe extern "C" fn healthy(_: *mut c_void) -> Status {
    STATUS_OK
}

fn executable() -> Library {
    #[cfg(unix)]
    {
        libloading::os::unix::Library::this().into()
    }
    #[cfg(windows)]
    {
        libloading::os::windows::Library::this().unwrap().into()
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Delayed,
    StateFailureOnce,
    Panic,
    Timeout,
    Unsupported,
    LateSuccess,
    DestructorPanic,
    LateDestroy,
}
struct WorkerModule {
    mode: Mode,
    worker: Option<std::thread::JoinHandle<()>>,
    harness: Arc<Harness>,
}
impl ManagedModule for WorkerModule {
    fn start(_: RuntimeClient) -> ModuleResult<Self> {
        Err("fixture uses explicit construction".into())
    }
    fn tick(&mut self) -> ModuleResult<()> {
        Ok(())
    }
}
impl RetirableModule for WorkerModule {
    fn poll_retirement(&mut self) -> ModuleResult<RetirementProgress> {
        if matches!(self.mode, Mode::Panic) {
            panic!("retirement fixture panic");
        }
        if matches!(self.mode, Mode::LateSuccess) {
            std::thread::sleep(Duration::from_millis(35));
        }
        let Some(worker) = self.worker.as_ref() else {
            return Ok(RetirementProgress::Complete);
        };
        if !worker.is_finished() {
            return Ok(RetirementProgress::Pending);
        }
        self.worker
            .take()
            .unwrap()
            .join()
            .map_err(|_| "worker panicked".to_string())?;
        Ok(RetirementProgress::Complete)
    }
}
impl Drop for WorkerModule {
    fn drop(&mut self) {
        self.harness.destroyed.fetch_add(1, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if matches!(self.mode, Mode::LateDestroy) {
            std::thread::sleep(Duration::from_millis(35));
        }
        if matches!(self.mode, Mode::DestructorPanic) {
            panic!("destructor fixture panic");
        }
    }
}

fn feature(
    id: &str,
    api: ModuleApi,
    instance: *mut c_void,
    retirement: Option<ModuleRetirementApi>,
) -> FeatureLibrary {
    FeatureLibrary {
        id: id.into(),
        api,
        retirement,
        retirement_failed: false,
        retirement_complete: false,
        instance,
        _library: executable(),
    }
}
fn broker_invoke(broker: &mut Broker, caller: &str, command: EffectFenceCommand) -> Response {
    broker.dispatch(
        caller,
        Command::EffectFence {
            command: Box::new(command),
        },
    )
}
fn scoped(
    broker: &mut Broker,
    caller: &str,
    lease: &SourceSlotLease,
    command: EffectFenceCommand,
) -> Response {
    broker_invoke(
        broker,
        caller,
        EffectFenceCommand::SlotCommand {
            lease: lease.clone(),
            command: Box::new(command),
        },
    )
}
fn setup_broker(instance_digit: &str, count: u64) -> (Broker, ProviderIncarnation) {
    setup_broker_with_open(instance_digit, count, true)
}
fn setup_broker_with_open(
    instance_digit: &str,
    count: u64,
    open: bool,
) -> (Broker, ProviderIncarnation) {
    assert!(open || count == 0);
    let service = "example.effects/1";
    let mut config = RuntimeConfig {
        host_lifecycle_control: true,
        ..RuntimeConfig::default()
    };
    config
        .principals
        .insert(HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Grants::default());
    config.principals.insert(
        "authority".into(),
        Grants {
            call: ["amaterasu.state/1".into()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "issuer".into(),
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
    config.principals.insert(
        "state".into(),
        Grants {
            provide: ["amaterasu.state/1".into()].into(),
            ..Default::default()
        },
    );
    config.service_selections.broker.insert(
        "amaterasu.state/1".into(),
        Some(SelectedService {
            provider: "state".into(),
            contract: "amaterasu.state/1".into(),
        }),
    );
    let instance = ProviderIncarnation {
        version: 1,
        id: instance_digit.repeat(32),
    };
    config
        .host_context
        .provider_incarnations
        .insert("provider".into(), instance.clone());
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
    config.api_methods.push(ApiMethodApproval {
        provider: "provider".into(),
        callers: ["issuer".into()].into(),
        method: method.clone(),
    });
    config.effect_fence = Some(EffectFencePolicy {
        authority: "authority".into(),
        issuer: "issuer".into(),
        source_slots: true,
        require_handoff: false,
        methods: vec![ProtectedMethod {
            service: service.into(),
            method: "apply".into(),
            interface_major: 1,
        }],
        cleanup_methods: vec![],
    });
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
    let Response::Result { payload } =
        broker_invoke(&mut broker, "authority", EffectFenceCommand::BrokerEpoch {})
    else {
        panic!()
    };
    let epoch = payload["broker_epoch"].as_str().unwrap().to_owned();
    for principal in [
        "issuer",
        "provider",
        "state",
        HOST_LIFECYCLE_CONTROL_PRINCIPAL,
    ] {
        assert!(matches!(
            broker_invoke(
                &mut broker,
                principal,
                EffectFenceCommand::RecoveryRetired {
                    broker_epoch: epoch.clone()
                }
            ),
            Response::Ok
        ));
    }
    assert!(matches!(
        broker_invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::RecoverSlots {
                broker_epoch: epoch,
                retired_incarnations: vec![0; 64]
            }
        ),
        Response::Ok
    ));
    let Response::Result { payload } = broker_invoke(
        &mut broker,
        "authority",
        EffectFenceCommand::SlotStatus { slot: 0 },
    ) else {
        panic!()
    };
    let lease: SourceSlotLease = serde_json::from_value(payload["lease"].clone()).unwrap();
    let scope = EffectScope {
        authority_epoch: "authority-epoch".into(),
        issuer_epoch: "issuer-epoch".into(),
        source: EffectSource {
            version: 1,
            id: "0".repeat(32),
            generation: 1,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            stream_generation: 1,
            device: "device".into(),
            device_generation: 1,
            effect_owner_epoch: "owner".into(),
        },
    };
    if open {
        assert!(matches!(
            broker_invoke(
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
    }
    let method = ProtectedMethod {
        service: service.into(),
        method: "apply".into(),
        interface_major: 1,
    };
    let Response::Result { payload } = broker_invoke(
        &mut broker,
        "authority",
        EffectFenceCommand::SelectedMethod { method },
    ) else {
        panic!()
    };
    let selected: amaterasu_contracts::effect_fence::SelectedEffectMethod =
        serde_json::from_value(payload).unwrap();
    let mut effect = GuardedEffect {
        scope,
        sequence: 1,
        capability: "example.effect".into(),
        service: service.into(),
        method: "apply".into(),
        interface_major: 1,
        interface_minor: 0,
        schema_revision: 1,
        payload_sha256: "d".repeat(64),
        payload: json!({"value":1}),
        timeout_ms: 30_000,
        operation_id: "operation.1".into(),
        parent: None,
    };
    for sequence in 1..=count {
        effect.sequence = sequence;
        effect.operation_id = format!("operation.{sequence}");
        assert!(matches!(
            scoped(
                &mut broker,
                "authority",
                &lease,
                EffectFenceCommand::ArmSelected {
                    effect: effect.clone(),
                    expected: Box::new(selected.clone())
                }
            ),
            Response::Ok
        ));
    }
    if count > 0 {
        effect.sequence = 1;
        effect.operation_id = "operation.1".into();
        assert!(matches!(
            scoped(
                &mut broker,
                "issuer",
                &lease,
                EffectFenceCommand::Dispatch { effect }
            ),
            Response::Accepted { .. }
        ));
        assert!(matches!(
            broker.dispatch("provider", Command::Poll {}),
            Response::Request { .. }
        ));
    }
    (broker, instance)
}

fn verify_receipt_cleanup(loaded: &mut LoadedComponents, harness: &Harness) {
    let deadline = Instant::now() + Duration::from_secs(2);
    let key = "host/effect-retirement/v2/00/1/32";
    let (host_read, host_id) = loaded
        .retirement_state_request(json!({"action":"get","key":key}), deadline, true)
        .unwrap();
    let receipt: amaterasu_contracts::effect_fence::HostProviderRetirement =
        serde_json::from_value(host_read["value"].clone()).unwrap();
    let revision = host_read["revision"].as_u64().unwrap();
    let journal = |retired| json!({"version":1,"slot":0,"retired_incarnation":retired,"active":null,"pending_close":null});
    assert_eq!(harness.store.handle("authority", json!({"action":"set","key":"pairing/protocol-slot/v1/00","value":journal(0),"expected_revision":0}))["ok"], true);
    let authority_read = |loaded: &mut LoadedComponents, harness: &Harness, operation: &str| {
        let Response::Accepted { id } = harness.broker.lock().unwrap().dispatch(
            "authority",
            Command::Request {
                service: "amaterasu.state/1".into(),
                payload: json!({"action":"get","key":"pairing/protocol-slot/v1/00"}),
                timeout_ms: 30_000,
                operation_id: operation.into(),
                parent: None,
            },
        ) else {
            panic!("authority read denied")
        };
        loaded.tick_retirement_state_provider().unwrap();
        assert!(matches!(
            harness
                .broker
                .lock()
                .unwrap()
                .dispatch("authority", Command::TakeResult { request: id }),
            Response::Result { .. }
        ));
        id
    };
    let old_read = authority_read(loaded, harness, "cleanup.old");
    let authorize = |id| EffectFenceCommand::AuthorizeHostReceiptCleanup {
        receipt: Box::new(receipt.clone()),
        authority_state_request: id,
        host_state_request: host_id,
    };
    assert!(
        matches!(broker_invoke(&mut harness.broker.lock().unwrap(), "issuer", authorize(old_read)), Response::Error { ref code, .. } if code == "permission_denied")
    );
    assert!(
        matches!(broker_invoke(&mut harness.broker.lock().unwrap(), "authority", authorize(old_read)), Response::Error { ref code, .. } if code == "recovery_required")
    );
    assert_eq!(harness.store.handle("authority", json!({"action":"set","key":"pairing/protocol-slot/v1/00","value":journal(1),"expected_revision":1}))["ok"], true);
    let fresh = authority_read(loaded, harness, "cleanup.fresh");
    assert!(
        matches!(broker_invoke(&mut harness.broker.lock().unwrap(), "authority", authorize(old_read)), Response::Error { ref code, .. } if code == "recovery_required")
    );
    assert!(
        matches!(broker_invoke(&mut harness.broker.lock().unwrap(), "authority", authorize(fresh)), Response::Error { ref code, .. } if code == "recovery_required")
    );
    let lease = receipt.issued_lease.clone();
    let scope = receipt.effect.scope.clone();
    assert!(matches!(
        broker_invoke(
            &mut harness.broker.lock().unwrap(),
            "authority",
            EffectFenceCommand::BeginCloseSlot {
                lease: lease.clone(),
                scope: scope.clone()
            }
        ),
        Response::Ok
    ));
    for principal in ["issuer", HOST_LIFECYCLE_CONTROL_PRINCIPAL] {
        assert!(matches!(
            broker_invoke(
                &mut harness.broker.lock().unwrap(),
                principal,
                EffectFenceCommand::SourceRetired {
                    lease: lease.clone(),
                    scope: scope.clone()
                }
            ),
            Response::Ok
        ));
    }
    assert!(matches!(
        scoped(
            &mut harness.broker.lock().unwrap(),
            "authority",
            &lease,
            EffectFenceCommand::Compact {
                scope: scope.clone(),
                retired_through: 32
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        broker_invoke(
            &mut harness.broker.lock().unwrap(),
            "authority",
            EffectFenceCommand::FinishCloseSlot {
                lease,
                scope,
                retired_through: 32
            }
        ),
        Response::Ok
    ));
    let mut forged = receipt.clone();
    forged.effect.sequence = 31;
    assert!(
        matches!(broker_invoke(&mut harness.broker.lock().unwrap(), "authority", EffectFenceCommand::AuthorizeHostReceiptCleanup { receipt: Box::new(forged), authority_state_request: fresh, host_state_request: host_id }), Response::Error { ref code, .. } if code == "recovery_required")
    );
    assert!(matches!(
        broker_invoke(
            &mut harness.broker.lock().unwrap(),
            "authority",
            authorize(fresh)
        ),
        Response::Ok
    ));
    loaded
        .retirement_state_request(
            json!({"action":"delete","key":key,"expected_revision":revision}),
            deadline,
            false,
        )
        .unwrap();
    assert_eq!(
        harness.store.handle(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            json!({"action":"get","key":key})
        )["found"],
        false
    );
    assert!(
        matches!(broker_invoke(&mut harness.broker.lock().unwrap(), "authority", authorize(fresh)), Response::Error { ref code, .. } if code == "recovery_required")
    );
}

fn exercise(mode: Mode, count: u64) {
    let path = std::env::temp_dir().join(format!(
        "host-retirement-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = TestStateStore::open(path.clone()).unwrap();
    let (mut broker, incarnation) = setup_broker("a", count);
    let Response::Result { payload: epoch } =
        broker_invoke(&mut broker, "authority", EffectFenceCommand::BrokerEpoch {})
    else {
        panic!("epoch unavailable")
    };
    let instance_key = format!(
        "host/provider-retirement/v1/{}/{}",
        epoch["broker_epoch"].as_str().unwrap(),
        incarnation.id
    );
    let prearm = if count == 0 {
        let Response::Result { payload } = broker_invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::SlotStatus { slot: 0 },
        ) else {
            panic!("slot missing")
        };
        let lease: SourceSlotLease = serde_json::from_value(payload["lease"].clone()).unwrap();
        let scope: EffectScope = serde_json::from_value(payload["active"].clone()).unwrap();
        let Response::Result { payload } = broker_invoke(
            &mut broker,
            "authority",
            EffectFenceCommand::SelectedMethod {
                method: ProtectedMethod {
                    service: "example.effects/1".into(),
                    method: "apply".into(),
                    interface_major: 1,
                },
            },
        ) else {
            panic!("selection missing")
        };
        let selected: amaterasu_contracts::effect_fence::SelectedEffectMethod =
            serde_json::from_value(payload).unwrap();
        let effect = GuardedEffect {
            scope: scope.clone(),
            sequence: 1,
            capability: "example.effect".into(),
            service: "example.effects/1".into(),
            method: "apply".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
            payload_sha256: "d".repeat(64),
            payload: json!({"value":1}),
            timeout_ms: 30_000,
            operation_id: "operation.1".into(),
            parent: None,
        };
        let identity = effect.identity("provider");
        let mut cleanup_method = selected.method.clone();
        cleanup_method.method = "cleanup".into();
        let approval = ApiMethodApproval {
            provider: "provider".into(),
            callers: ["issuer".into()].into(),
            method: cleanup_method,
        };
        let entry = json!({"identity":identity,"issued_lease":lease,"provider_incarnation":incarnation,"selected_method":selected,"cleanup_method":{"service":"example.effects/1","method":"cleanup","interface_major":1,"interface_minor":0,"schema_revision":1},"cleanup_approval":approval,"cleanup":{"id":"a".repeat(64)},"cleanup_confirmed":false});
        let journal = json!({"version":1,"slot":0,"retired_incarnation":0,"pending_close":null,"active":{"lease":lease,"scope":scope,"issuer":"issuer","retired_through":0,"issued_through":1,"entries":[entry]}});
        assert_eq!(store.handle("authority", json!({"action":"set","key":"pairing/protocol-slot/v1/00","value":journal,"expected_revision":0}))["ok"], true);
        Some((identity, entry, journal))
    } else {
        None
    };
    let harness = Arc::new(Harness {
        broker: Mutex::new(broker),
        store,
        fail_next_set: AtomicBool::new(matches!(mode, Mode::StateFailureOnce)),
        destroyed: AtomicUsize::new(0),
    });
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: Arc::as_ptr(&harness).cast_mut().cast(),
        invoke: host_invoke,
    };
    let runtime = RuntimeLibrary {
        api: RuntimeApi {
            header: AbiHeader::of::<RuntimeApi>(),
            create: unused_runtime,
            endpoint: unused_endpoint,
            destroy: runtime_destroy,
        },
        instance: Box::into_raw(Box::new(())).cast(),
        _library: executable(),
    };
    let delay = if matches!(mode, Mode::Timeout) {
        80
    } else {
        15
    };
    let mut worker = Some(std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(delay))
    }));
    // Deliberately put the independent State provider after the protected
    // provider in the manifest; host composition must reverse this ordering.
    let manifest = retirement_component_order(
        &[manifest_component("provider"), manifest_component("state")],
        &[("provider".into(), incarnation.clone())],
        Some("state"),
    )
    .unwrap();
    let ordered = amaterasu_package::ordered_components(&manifest).unwrap();
    let mut loaded = LoadedComponents::load_features(
        runtime,
        None,
        endpoint,
        false,
        ordered.into_iter().map(|component| component.id.as_str()),
        |_, name| {
            if name == "state" {
                Ok(feature(
                    name,
                    ModuleApi {
                        header: AbiHeader::of::<ModuleApi>(),
                        create: unused_create,
                        tick: state_tick,
                        health: healthy,
                        destroy: state_destroy,
                    },
                    Box::into_raw(Box::new(harness.clone())).cast(),
                    None,
                ))
            } else {
                let retirement =
                    (!matches!(mode, Mode::Unsupported)).then_some(ModuleRetirementApi {
                        header: AbiHeader::of::<ModuleRetirementApi>(),
                        poll_retirement: amaterasu_module_sdk::exports::poll_retirement::<
                            WorkerModule,
                        >,
                        destroy_retired: amaterasu_module_sdk::exports::destroy_retired::<
                            WorkerModule,
                        >,
                    });
                let module = WorkerModule {
                    mode,
                    worker: worker.take(),
                    harness: harness.clone(),
                };
                Ok(feature(
                    name,
                    ModuleApi {
                        header: AbiHeader::of::<ModuleApi>(),
                        create: unused_create,
                        tick: healthy,
                        health: healthy,
                        destroy: amaterasu_module_sdk::exports::destroy::<WorkerModule>,
                    },
                    Box::into_raw(Box::new(module)).cast(),
                    retirement,
                ))
            }
        },
        &mut |_| Ok(()),
    )
    .unwrap();
    loaded.retirement_providers = vec![("provider".into(), incarnation.clone())];
    loaded.retirement_state_provider = Some("state".into());
    let limit = if matches!(mode, Mode::Timeout | Mode::LateSuccess | Mode::LateDestroy) {
        Duration::from_millis(if matches!(mode, Mode::LateDestroy) {
            25
        } else {
            10
        })
    } else {
        Duration::from_secs(2)
    };
    let result = loaded.retire_effect_providers(Instant::now() + limit);
    assert_eq!(result.is_ok(), matches!(mode, Mode::Delayed));
    if matches!(mode, Mode::StateFailureOnce) {
        assert_eq!(harness.destroyed.load(Ordering::SeqCst), 1);
        assert!(
            loaded
                .retire_effect_providers(Instant::now() + Duration::from_secs(2))
                .is_ok()
        );
        assert_eq!(harness.destroyed.load(Ordering::SeqCst), 1);
    } else if !matches!(mode, Mode::Delayed) {
        assert!(
            loaded
                .retire_effect_providers(Instant::now() + Duration::from_millis(5))
                .is_err()
        );
    }
    let successful = matches!(mode, Mode::Delayed | Mode::StateFailureOnce);
    if successful {
        // Repeating persistence must replay the same snapshot without growing
        // State or replacing an existing, different receipt.
        loaded
            .persist_provider_retirements(
                "provider",
                &incarnation,
                Instant::now() + Duration::from_secs(2),
            )
            .unwrap();
        if count == 32 {
            for sequence in 1..=count {
                let record = harness.store.handle(HOST_LIFECYCLE_CONTROL_PRINCIPAL, json!({"action":"get","key":format!("host/effect-retirement/v2/00/1/{sequence}")}));
                assert_eq!(record["value"]["effect"]["sequence"], sequence);
            }
            verify_receipt_cleanup(&mut loaded, &harness);
        }
        loaded.retirement_providers.clear();
    }
    drop(loaded);
    let saved_instance = harness.store.handle(
        HOST_LIFECYCLE_CONTROL_PRINCIPAL,
        json!({"action":"get","key":instance_key}),
    );
    assert_eq!(saved_instance["found"], successful);
    if let Some((identity, entry, mut journal)) = prearm {
        let (mut restarted, _) = setup_broker_with_open("b", 0, false);
        let Response::Result { payload } = broker_invoke(
            &mut restarted,
            "authority",
            EffectFenceCommand::SlotStatus { slot: 0 },
        ) else {
            panic!("recovery slot unavailable")
        };
        let lease: SourceSlotLease = serde_json::from_value(payload["lease"].clone()).unwrap();
        journal["active"]["lease"] = serde_json::to_value(&lease).unwrap();
        assert_eq!(harness.store.handle("authority", json!({"action":"set","key":"pairing/protocol-slot/v1/00","value":journal,"expected_revision":1}))["ok"], true);
        assert!(matches!(
            broker_invoke(
                &mut restarted,
                "authority",
                EffectFenceCommand::OpenRecoverySlot {
                    lease: lease.clone(),
                    scope: identity.scope.clone(),
                    retired_through: 0,
                    issued_through: 1
                }
            ),
            Response::Ok
        ));
        let read = |broker: &mut Broker, caller: &str, key: &str, operation: &str| {
            let Response::Accepted { id } = broker.dispatch(
                caller,
                Command::Request {
                    service: "amaterasu.state/1".into(),
                    payload: json!({"action":"get","key":key}),
                    timeout_ms: 30_000,
                    operation_id: operation.into(),
                    parent: None,
                },
            ) else {
                panic!("recovery read denied")
            };
            let Response::Request {
                caller, payload, ..
            } = broker.dispatch("state", Command::Poll {})
            else {
                panic!("recovery read missing")
            };
            let result = harness.store.handle(&caller, payload);
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
            id
        };
        let journal_read = read(
            &mut restarted,
            "authority",
            "pairing/protocol-slot/v1/00",
            "prearm.journal",
        );
        let instance_read = read(
            &mut restarted,
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            &instance_key,
            "prearm.instance",
        );
        let receipt: amaterasu_contracts::effect_fence::HostProviderInstanceRetirement =
            serde_json::from_value(saved_instance["value"].clone()).unwrap();
        assert!(matches!(
            broker_invoke(
                &mut restarted,
                HOST_LIFECYCLE_CONTROL_PRINCIPAL,
                EffectFenceCommand::HostRecoverHistoricalInstance {
                    lease: lease.clone(),
                    receipt: Box::new(receipt),
                    identity: Box::new(identity.clone()),
                    expected_entry: entry,
                    state_request: journal_read,
                    host_receipt_state_request: instance_read
                }
            ),
            Response::Ok
        ));
        assert!(
            matches!(scoped(&mut restarted, "authority", &lease, EffectFenceCommand::Compact { scope: identity.scope, retired_through: 1 }), Response::Error { ref code, .. } if code == "effect_pending")
        );
    }
    let saved = harness.store.handle(
        HOST_LIFECYCLE_CONTROL_PRINCIPAL,
        json!({"action":"get","key":"host/effect-retirement/v2/00/1/1"}),
    );
    assert_eq!(saved["found"], successful && count > 0);
    if successful && count > 0 {
        for sequence in 1..=if count == 32 { 31 } else { count } {
            let record = harness.store.handle(
                HOST_LIFECYCLE_CONTROL_PRINCIPAL,
                json!({"action":"get","key":format!("host/effect-retirement/v2/00/1/{sequence}")}),
            );
            assert_eq!(record["value"]["effect"]["sequence"], sequence);
        }
    }
    if successful && count > 0 {
        assert_eq!(saved["value"]["effect"]["sequence"], 1);
    }
    drop(harness);
    let reopened = TestStateStore::open(path.clone()).unwrap();
    let saved_again = reopened.handle(
        HOST_LIFECYCLE_CONTROL_PRINCIPAL,
        json!({"action":"get","key":"host/effect-retirement/v2/00/1/1"}),
    );
    assert_eq!(saved_again["found"], successful && count > 0);
    if successful && count > 0 {
        let (mut restarted, replacement_instance) = setup_broker("b", 1);
        assert_ne!(
            saved_again["value"]["provider_incarnation"]["id"],
            replacement_instance.id
        );
        let Response::Accepted { id } = restarted.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::Request {
                service: "amaterasu.state/1".into(),
                payload: json!({"action":"get","key":"host/effect-retirement/v2/00/1/1"}),
                timeout_ms: 30_000,
                operation_id: "restart.read".into(),
                parent: None,
            },
        ) else {
            panic!("restarted host read denied")
        };
        let Response::Request {
            caller, payload, ..
        } = restarted.dispatch("state", Command::Poll {})
        else {
            panic!("restarted State request missing")
        };
        let result = reopened.handle(&caller, payload);
        assert!(matches!(
            restarted.dispatch(
                "state",
                Command::Complete {
                    request: id,
                    payload: result
                }
            ),
            Response::Ok
        ));
        let Response::Result { payload } = restarted.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::TakeResult { request: id },
        ) else {
            panic!("replayed host record missing")
        };
        assert_eq!(payload["value"], saved_again["value"]);
    }
    drop(reopened);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn delayed_worker_is_joined_before_durable_host_receipt() {
    exercise(Mode::Delayed, 1);
}
#[test]
fn joined_instance_is_recorded_even_without_any_armed_effect() {
    exercise(Mode::Delayed, 0);
}
#[test]
fn checked_retirement_persistence_is_retried_without_destroying_twice() {
    exercise(Mode::StateFailureOnce, 1);
}
#[test]
fn full_slot_is_persisted_in_stable_retryable_records() {
    exercise(Mode::Delayed, 32);
}
#[test]
fn panicking_retirement_never_persists_a_receipt() {
    exercise(Mode::Panic, 1);
}
#[test]
fn timeout_never_persists_a_late_receipt() {
    exercise(Mode::Timeout, 1);
}
#[test]
fn success_returning_after_deadline_never_persists_a_receipt() {
    exercise(Mode::LateSuccess, 1);
}
#[test]
fn swallowed_destructor_panic_never_persists_a_receipt() {
    exercise(Mode::DestructorPanic, 1);
}
#[test]
fn checked_destruction_after_deadline_never_persists_a_receipt() {
    exercise(Mode::LateDestroy, 1);
}
#[test]
fn legacy_module_never_persists_a_receipt() {
    exercise(Mode::Unsupported, 1);
}
