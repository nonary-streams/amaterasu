//! Real broker and State integration for the host's optional retirement path.
use super::{FeatureLibrary, LoadedComponents, RuntimeLibrary};
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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Harness {
    broker: Mutex<Broker>,
    store: TestStateStore,
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
            let result = harness.store.handle(&caller, payload);
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
fn setup_broker(instance_digit: &str) -> (Broker, ProviderIncarnation) {
    let service = "example.effects/1";
    let mut config = RuntimeConfig {
        host_lifecycle_control: true,
        ..RuntimeConfig::default()
    };
    config
        .principals
        .insert(HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Grants::default());
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
    let selected = serde_json::from_value(payload).unwrap();
    let effect = GuardedEffect {
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
    assert!(matches!(
        scoped(
            &mut broker,
            "authority",
            &lease,
            EffectFenceCommand::ArmSelected {
                effect: effect.clone(),
                expected: Box::new(selected)
            }
        ),
        Response::Ok
    ));
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
    (broker, instance)
}

fn exercise(mode: Mode) {
    let path = std::env::temp_dir().join(format!(
        "host-retirement-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = TestStateStore::open(path.clone()).unwrap();
    let (broker, incarnation) = setup_broker("a");
    let harness = Arc::new(Harness {
        broker: Mutex::new(broker),
        store,
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
    let mut loaded = LoadedComponents::load_features(
        runtime,
        None,
        endpoint,
        false,
        ["state", "provider"],
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
    loaded.retirement_providers = vec![("provider".into(), incarnation)];
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
    if matches!(mode, Mode::Delayed) {
        loaded.retirement_providers.clear();
    }
    drop(loaded);
    let saved = harness.store.handle(
        HOST_LIFECYCLE_CONTROL_PRINCIPAL,
        json!({"action":"get","key":"host/effect-retirement/v1/00"}),
    );
    assert_eq!(saved["found"], matches!(mode, Mode::Delayed));
    if matches!(mode, Mode::Delayed) {
        assert_eq!(saved["value"]["receipts"].as_array().unwrap().len(), 1);
    }
    drop(harness);
    let reopened = TestStateStore::open(path.clone()).unwrap();
    let saved_again = reopened.handle(
        HOST_LIFECYCLE_CONTROL_PRINCIPAL,
        json!({"action":"get","key":"host/effect-retirement/v1/00"}),
    );
    assert_eq!(saved_again["found"], matches!(mode, Mode::Delayed));
    if matches!(mode, Mode::Delayed) {
        let (mut restarted, replacement_instance) = setup_broker("b");
        assert_ne!(
            saved_again["value"]["receipts"][0]["provider_incarnation"]["id"],
            replacement_instance.id
        );
        let Response::Accepted { id } = restarted.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::Request {
                service: "amaterasu.state/1".into(),
                payload: json!({"action":"get","key":"host/effect-retirement/v1/00"}),
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
    exercise(Mode::Delayed);
}
#[test]
fn panicking_retirement_never_persists_a_receipt() {
    exercise(Mode::Panic);
}
#[test]
fn timeout_never_persists_a_late_receipt() {
    exercise(Mode::Timeout);
}
#[test]
fn success_returning_after_deadline_never_persists_a_receipt() {
    exercise(Mode::LateSuccess);
}
#[test]
fn swallowed_destructor_panic_never_persists_a_receipt() {
    exercise(Mode::DestructorPanic);
}
#[test]
fn checked_destruction_after_deadline_never_persists_a_receipt() {
    exercise(Mode::LateDestroy);
}
#[test]
fn legacy_module_never_persists_a_receipt() {
    exercise(Mode::Unsupported);
}
