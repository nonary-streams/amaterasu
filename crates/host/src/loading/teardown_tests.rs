//! Exercise the actual partial-load owner and ABI destructors without loading
//! external code or binding listeners. The executable handle keeps fixture tables live.
use super::{FeatureLibrary, LoadedComponents, RuntimeLibrary};
use amaterasu_abi::{
    AbiHeader, BorrowedBytes, HostApi, ModuleApi, ReplySink, RuntimeApi, STATUS_INTERNAL_ERROR,
    STATUS_OK, Status,
};
use libloading::Library;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};

type DestructionTrace = Arc<Mutex<Vec<&'static str>>>;

struct FixtureInstance {
    name: &'static str,
    trace: DestructionTrace,
    shutdown_budgets: Vec<u64>,
}

fn fixture_instance(name: &'static str, trace: &DestructionTrace) -> *mut c_void {
    Box::into_raw(Box::new(FixtureInstance {
        name,
        trace: trace.clone(),
        shutdown_budgets: Vec::new(),
    }))
    .cast()
}

unsafe extern "C" fn destroy(instance: *mut c_void) {
    // SAFETY: each fixture transfers exactly one boxed instance to its library owner.
    let instance = unsafe { Box::from_raw(instance.cast::<FixtureInstance>()) };
    instance.trace.lock().unwrap().push(instance.name);
}

unsafe extern "C" fn create_runtime(_: BorrowedBytes, _: *mut *mut c_void) -> Status {
    STATUS_INTERNAL_ERROR
}

unsafe extern "C" fn endpoint(_: *mut c_void, _: BorrowedBytes, _: *mut HostApi) -> Status {
    STATUS_INTERNAL_ERROR
}

unsafe extern "C" fn create_module(_: *const HostApi, _: *mut *mut c_void) -> Status {
    STATUS_INTERNAL_ERROR
}

unsafe extern "C" fn healthy(_: *mut c_void) -> Status {
    STATUS_OK
}

unsafe extern "C" fn invoke(_: *mut c_void, _: BorrowedBytes, _: ReplySink) -> Status {
    STATUS_INTERNAL_ERROR
}

fn lifecycle_endpoint() -> HostApi {
    HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: std::ptr::null_mut(),
        invoke,
    }
}

fn executable_library() -> Library {
    #[cfg(windows)]
    {
        libloading::os::windows::Library::this().unwrap().into()
    }
    #[cfg(unix)]
    {
        libloading::os::unix::Library::this().into()
    }
}

fn runtime(trace: &DestructionTrace) -> RuntimeLibrary {
    RuntimeLibrary {
        wait_api: None,
        api: RuntimeApi {
            header: AbiHeader::of::<RuntimeApi>(),
            create: create_runtime,
            endpoint,
            destroy,
        },
        instance: fixture_instance("runtime", trace),
        _library: executable_library(),
    }
}

fn feature(name: &'static str, trace: &DestructionTrace) -> FeatureLibrary {
    FeatureLibrary {
        id: name.into(),
        dependencies: Default::default(),
        quiesce_api: None,
        shutdown_api: None,
        shutdown_prepared: false,
        api: ModuleApi {
            header: AbiHeader::of::<ModuleApi>(),
            create: create_module,
            tick: healthy,
            health: healthy,
            destroy,
        },
        instance: fixture_instance(name, trace),
        _library: executable_library(),
    }
}

#[test]
fn later_module_failure_destroys_loaded_consumers_before_providers_and_runtime() {
    let trace = DestructionTrace::default();
    let result = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["provider", "consumer", "unavailable-listener"],
        |_, name| {
            if name == "unavailable-listener" {
                Err("tcp_bind_failed".into())
            } else {
                Ok(feature(name, &trace))
            }
        },
        &mut |_| Ok(()),
    );
    assert_eq!(result.err().as_deref(), Some("tcp_bind_failed"));
    assert_eq!(*trace.lock().unwrap(), ["consumer", "provider", "runtime"]);
}

#[test]
fn progress_failure_destroys_just_created_module_before_its_dependencies() {
    let trace = DestructionTrace::default();
    let result = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["provider", "consumer"],
        |_, name| Ok(feature(name, &trace)),
        &mut |count| {
            if count == 3 {
                Err("progress transport closed".into())
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(result.err().as_deref(), Some("progress transport closed"));
    assert_eq!(*trace.lock().unwrap(), ["consumer", "provider", "runtime"]);
}

#[test]
fn runtime_progress_failure_does_not_start_features_and_releases_runtime() {
    let trace = DestructionTrace::default();
    let result = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["provider"],
        |_, _| panic!("feature must not start after progress failure"),
        &mut |_| Err("progress transport closed".into()),
    );
    assert!(result.is_err());
    assert_eq!(*trace.lock().unwrap(), ["runtime"]);
}

#[test]
fn completed_load_retains_features_until_reverse_dependency_shutdown() {
    let trace = DestructionTrace::default();
    let mut progress = Vec::new();
    let loaded = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["provider", "consumer"],
        |_, name| Ok(feature(name, &trace)),
        &mut |count| {
            progress.push(count);
            Ok(())
        },
    )
    .unwrap();
    assert!(trace.lock().unwrap().is_empty());
    assert_eq!(progress, [1, 2, 3]);
    drop(loaded);
    assert_eq!(*trace.lock().unwrap(), ["consumer", "provider", "runtime"]);
}

#[test]
fn cooperative_shutdown_keeps_providers_ticking_until_consumer_releases() {
    unsafe extern "C" fn provider_tick(instance: *mut c_void) -> Status {
        // SAFETY: the fixture lends its live instance for this serialized call.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        instance.trace.lock().unwrap().push("provider-progress");
        STATUS_OK
    }
    unsafe extern "C" fn poll(instance: *mut c_void, ready: *mut u8) -> Status {
        // SAFETY: the fixture owns the instance and host provides a writable byte.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        unsafe {
            *ready = u8::from(
                instance
                    .trace
                    .lock()
                    .unwrap()
                    .contains(&"provider-progress"),
            )
        };
        STATUS_OK
    }
    let trace = DestructionTrace::default();
    let loaded = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["provider", "consumer"],
        |_, name| {
            let mut module = feature(name, &trace);
            if name == "provider" {
                module.api.tick = provider_tick;
            } else {
                module.quiesce_api = Some(amaterasu_abi::ModuleQuiesceApi {
                    header: AbiHeader::of::<amaterasu_abi::ModuleQuiesceApi>(),
                    poll,
                });
            }
            Ok(module)
        },
        &mut |_| Ok(()),
    )
    .unwrap();
    drop(loaded);
    assert_eq!(
        *trace.lock().unwrap(),
        ["provider-progress", "consumer", "provider", "runtime"]
    );
}

#[test]
fn cooperative_shutdown_continues_after_an_unrelated_quiesce_error() {
    unsafe extern "C" fn progress(instance: *mut c_void) -> Status {
        // SAFETY: the fixture lends its live instance for this serialized call.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        instance.trace.lock().unwrap().push("provider-progress");
        STATUS_OK
    }
    unsafe extern "C" fn consumer_poll(instance: *mut c_void, ready: *mut u8) -> Status {
        // SAFETY: the host borrows the live instance and supplies writable output.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        unsafe {
            *ready = u8::from(
                instance
                    .trace
                    .lock()
                    .unwrap()
                    .contains(&"provider-progress"),
            );
        }
        STATUS_OK
    }
    unsafe extern "C" fn fail_once(instance: *mut c_void, ready: *mut u8) -> Status {
        // SAFETY: the host borrows the live instance and supplies writable output.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        let mut trace = instance.trace.lock().unwrap();
        if !trace.contains(&"quiesce-failed") {
            trace.push("quiesce-failed");
            return STATUS_INTERNAL_ERROR;
        }
        unsafe { *ready = 1 };
        STATUS_OK
    }

    let trace = DestructionTrace::default();
    let mut loaded = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["failing", "provider", "consumer"],
        |_, name| {
            let mut module = feature(name, &trace);
            match name {
                "provider" => module.api.tick = progress,
                "consumer" => {
                    module.quiesce_api = Some(amaterasu_abi::ModuleQuiesceApi {
                        header: AbiHeader::of::<amaterasu_abi::ModuleQuiesceApi>(),
                        poll: consumer_poll,
                    });
                }
                "failing" => {
                    module.quiesce_api = Some(amaterasu_abi::ModuleQuiesceApi {
                        header: AbiHeader::of::<amaterasu_abi::ModuleQuiesceApi>(),
                        poll: fail_once,
                    });
                }
                _ => unreachable!(),
            }
            Ok(module)
        },
        &mut |_| Ok(()),
    )
    .unwrap();

    let result = loaded.quiesce(std::time::Duration::from_secs(1));
    assert!(result.is_ok(), "healthy cleanup was aborted: {result:?}");
    assert_eq!(
        *trace.lock().unwrap(),
        ["quiesce-failed", "provider-progress"]
    );
}

#[test]
fn cooperative_shutdown_ticks_providers_after_an_unrelated_tick_error() {
    unsafe extern "C" fn progress(instance: *mut c_void) -> Status {
        // SAFETY: the fixture lends its live instance for this serialized call.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        instance.trace.lock().unwrap().push("provider-progress");
        STATUS_OK
    }
    unsafe extern "C" fn consumer_poll(instance: *mut c_void, ready: *mut u8) -> Status {
        // SAFETY: the host borrows the live instance and supplies writable output.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        unsafe {
            *ready = u8::from(
                instance
                    .trace
                    .lock()
                    .unwrap()
                    .contains(&"provider-progress"),
            );
        }
        STATUS_OK
    }
    unsafe extern "C" fn fail_once(instance: *mut c_void) -> Status {
        // SAFETY: the fixture lends its live instance for this serialized call.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        let mut trace = instance.trace.lock().unwrap();
        if !trace.contains(&"tick-failed") {
            trace.push("tick-failed");
            STATUS_INTERNAL_ERROR
        } else {
            STATUS_OK
        }
    }

    let trace = DestructionTrace::default();
    let mut loaded = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["failing", "provider", "consumer"],
        |_, name| {
            let mut module = feature(name, &trace);
            match name {
                "provider" => module.api.tick = progress,
                "consumer" => {
                    module.quiesce_api = Some(amaterasu_abi::ModuleQuiesceApi {
                        header: AbiHeader::of::<amaterasu_abi::ModuleQuiesceApi>(),
                        poll: consumer_poll,
                    });
                }
                "failing" => module.api.tick = fail_once,
                _ => unreachable!(),
            }
            Ok(module)
        },
        &mut |_| Ok(()),
    )
    .unwrap();

    let result = loaded.quiesce(std::time::Duration::from_secs(1));
    assert!(result.is_ok(), "healthy cleanup was aborted: {result:?}");
    assert_eq!(*trace.lock().unwrap(), ["tick-failed", "provider-progress"]);
}

#[test]
fn cooperative_shutdown_deadline_retains_the_failing_module_identity() {
    unsafe extern "C" fn fail(_: *mut c_void, _: *mut u8) -> Status {
        STATUS_INTERNAL_ERROR
    }
    let trace = DestructionTrace::default();
    let mut loaded = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["failing"],
        |_, name| {
            let mut module = feature(name, &trace);
            module.quiesce_api = Some(amaterasu_abi::ModuleQuiesceApi {
                header: AbiHeader::of::<amaterasu_abi::ModuleQuiesceApi>(),
                poll: fail,
            });
            Ok(module)
        },
        &mut |_| Ok(()),
    )
    .unwrap();

    assert_eq!(
        loaded.quiesce(std::time::Duration::ZERO).unwrap_err(),
        "module drain deadline elapsed: quiesce failing failed with ABI status 2"
    );
    assert!(!loaded.quiesced);
    drop(loaded);
    assert_eq!(*trace.lock().unwrap(), ["failing", "runtime"]);
}

#[test]
fn cooperative_shutdown_drains_consumers_before_quiescing_shared_providers() {
    unsafe extern "C" fn progress(instance: *mut c_void) -> Status {
        // SAFETY: the fixture lends its live instance for this serialized call.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        instance.trace.lock().unwrap().push("transport-progress");
        STATUS_OK
    }
    unsafe extern "C" fn poll(instance: *mut c_void, ready: *mut u8) -> Status {
        // SAFETY: the host borrows the live instance and supplies writable output.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        let mut trace = instance.trace.lock().unwrap();
        let drained = match instance.name {
            "ingress" => {
                if trace.contains(&"transport-progress") {
                    trace.push("ingress-drained");
                    true
                } else {
                    false
                }
            }
            "launch" | "transport" => {
                if !trace.contains(&"ingress-drained") {
                    trace.push("provider-quiesced-too-early");
                }
                trace.push(instance.name);
                true
            }
            "display" => {
                if !trace.contains(&"launch") || !trace.contains(&"transport") {
                    trace.push("provider-quiesced-too-early");
                }
                trace.push("display");
                true
            }
            "independent" => {
                trace.push("independent-drained");
                true
            }
            _ => unreachable!(),
        };
        unsafe { *ready = u8::from(drained) };
        STATUS_OK
    }
    let trace = DestructionTrace::default();
    let mut loaded = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["display", "launch", "transport", "ingress", "independent"],
        |_, name| {
            let mut module = feature(name, &trace);
            module.dependencies = match name {
                "launch" | "transport" => ["display".into()].into(),
                "ingress" => ["launch".into(), "transport".into()].into(),
                _ => Default::default(),
            };
            module.quiesce_api = Some(amaterasu_abi::ModuleQuiesceApi {
                header: AbiHeader::of::<amaterasu_abi::ModuleQuiesceApi>(),
                poll,
            });
            if name == "transport" {
                module.api.tick = progress;
            }
            Ok(module)
        },
        &mut |_| Ok(()),
    )
    .unwrap();

    loaded.quiesce(std::time::Duration::from_secs(1)).unwrap();
    let trace = trace.lock().unwrap().clone();
    assert!(!trace.contains(&"provider-quiesced-too-early"), "{trace:?}");
    assert_eq!(trace[0], "independent-drained");
    assert!(trace.contains(&"transport-progress"));
    assert!(trace.contains(&"display"));
}

#[test]
fn failed_consumer_keeps_its_provider_live_while_an_independent_branch_drains() {
    unsafe extern "C" fn poll(instance: *mut c_void, ready: *mut u8) -> Status {
        // SAFETY: the host borrows the live instance and supplies writable output.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        instance.trace.lock().unwrap().push(instance.name);
        if instance.name == "failing-consumer" {
            return STATUS_INTERNAL_ERROR;
        }
        unsafe { *ready = 1 };
        STATUS_OK
    }
    let trace = DestructionTrace::default();
    let mut loaded = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["blocked-provider", "failing-consumer", "independent"],
        |_, name| {
            let mut module = feature(name, &trace);
            if name == "failing-consumer" {
                module.dependencies.insert("blocked-provider".into());
            }
            module.quiesce_api = Some(amaterasu_abi::ModuleQuiesceApi {
                header: AbiHeader::of::<amaterasu_abi::ModuleQuiesceApi>(),
                poll,
            });
            Ok(module)
        },
        &mut |_| Ok(()),
    )
    .unwrap();

    let error = loaded.quiesce(std::time::Duration::ZERO).unwrap_err();
    assert!(error.contains("quiesce failing-consumer failed"), "{error}");
    assert_eq!(*trace.lock().unwrap(), ["independent", "failing-consumer"]);
}

#[test]
fn explicit_shutdown_returns_drain_failure_and_destroys_without_a_second_budget() {
    unsafe extern "C" fn fail(instance: *mut c_void, _: *mut u8) -> Status {
        // SAFETY: the host lends the fixture's live instance for this call.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        instance.trace.lock().unwrap().push("quiesce-failed");
        STATUS_INTERNAL_ERROR
    }
    let trace = DestructionTrace::default();
    let loaded = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["failing"],
        |_, name| {
            let mut module = feature(name, &trace);
            module.quiesce_api = Some(amaterasu_abi::ModuleQuiesceApi {
                header: AbiHeader::of::<amaterasu_abi::ModuleQuiesceApi>(),
                poll: fail,
            });
            Ok(module)
        },
        &mut |_| Ok(()),
    )
    .unwrap();

    let error = loaded.shutdown(std::time::Duration::ZERO).unwrap_err();
    assert!(error.contains("quiesce failing failed"), "{error}");
    assert_eq!(
        *trace.lock().unwrap(),
        ["quiesce-failed", "failing", "runtime"]
    );
}

unsafe extern "C" fn announce_fixture_shutdown(instance: *mut c_void, remaining_ms: u64) -> Status {
    // SAFETY: the host serializes lifecycle calls to this live fixture instance.
    let instance = unsafe { &mut *instance.cast::<FixtureInstance>() };
    instance.shutdown_budgets.push(remaining_ms);
    instance.trace.lock().unwrap().push(match instance.name {
        "consumer" => "consumer-announced",
        _ => "provider-announced",
    });
    STATUS_OK
}

unsafe extern "C" fn shutdown_consumer_poll(instance: *mut c_void, ready: *mut u8) -> Status {
    // SAFETY: the host lends the live fixture and writable readiness output.
    let instance = unsafe { &*instance.cast::<FixtureInstance>() };
    let mut trace = instance.trace.lock().unwrap();
    if !trace.contains(&"consumer-announced") || !trace.contains(&"provider-announced") {
        trace.push("consumer-polled-before-announcement");
    }
    let drained = trace.contains(&"provider-progress");
    if drained {
        trace.push("consumer-drained");
    }
    unsafe { *ready = u8::from(drained) };
    STATUS_OK
}

unsafe extern "C" fn shutdown_provider_tick(instance: *mut c_void) -> Status {
    // SAFETY: the host lends the live fixture for this serialized tick.
    let instance = unsafe { &*instance.cast::<FixtureInstance>() };
    instance.trace.lock().unwrap().push("provider-progress");
    STATUS_OK
}

fn shutdown_fixture_budgets(module: &FeatureLibrary) -> Vec<u64> {
    // SAFETY: these tests create every feature from a boxed FixtureInstance;
    // the borrowed library retains that allocation and no call runs concurrently.
    unsafe { &*module.instance.cast::<FixtureInstance>() }
        .shutdown_budgets
        .clone()
}

#[test]
fn shutdown_announcement_precedes_consumer_drain_with_replacement_providers() {
    unsafe extern "C" fn replacement_tick(instance: *mut c_void) -> Status {
        // SAFETY: the host lends the live fixture for this serialized tick.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        let mut trace = instance.trace.lock().unwrap();
        if trace.contains(&"replacement-progress") {
            trace.push("provider-progress");
        } else {
            trace.push("replacement-progress");
        }
        STATUS_OK
    }
    unsafe extern "C" fn provider_poll(instance: *mut c_void, ready: *mut u8) -> Status {
        // SAFETY: the host lends the live fixture and writable readiness output.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        let mut trace = instance.trace.lock().unwrap();
        if !trace.contains(&"consumer-drained") {
            trace.push("provider-quiesced-too-early");
        }
        trace.push("provider-quiesced");
        unsafe { *ready = 1 };
        STATUS_OK
    }
    for (provider, tick) in [
        (
            "first-provider",
            shutdown_provider_tick as unsafe extern "C" fn(*mut c_void) -> Status,
        ),
        ("replacement-provider", replacement_tick),
    ] {
        let trace = DestructionTrace::default();
        let mut loaded = LoadedComponents::load_features(
            runtime(&trace),
            None,
            lifecycle_endpoint(),
            false,
            [provider, "consumer"],
            |_, name| {
                let mut module = feature(name, &trace);
                module.shutdown_api = Some(amaterasu_abi::ModuleShutdownApi {
                    header: AbiHeader::of::<amaterasu_abi::ModuleShutdownApi>(),
                    prepare: announce_fixture_shutdown,
                });
                let poll: unsafe extern "C" fn(*mut c_void, *mut u8) -> Status =
                    if name == "consumer" {
                        module.dependencies.insert(provider.into());
                        shutdown_consumer_poll
                    } else {
                        module.api.tick = tick;
                        provider_poll
                    };
                module.quiesce_api = Some(amaterasu_abi::ModuleQuiesceApi {
                    header: AbiHeader::of::<amaterasu_abi::ModuleQuiesceApi>(),
                    poll,
                });
                Ok(module)
            },
            &mut |_| Ok(()),
        )
        .unwrap();

        loaded.quiesce(std::time::Duration::from_secs(1)).unwrap();
        loaded.quiesce(std::time::Duration::from_secs(30)).unwrap();
        let provider_budgets = shutdown_fixture_budgets(&loaded.modules[0]);
        let consumer_budgets = shutdown_fixture_budgets(&loaded.modules[1]);
        assert_eq!(provider_budgets.len(), 1);
        assert_eq!(consumer_budgets.len(), 1);
        assert!(provider_budgets[0] <= consumer_budgets[0]);
        assert!(consumer_budgets[0] <= 1_000);
        let trace = trace.lock().unwrap().clone();
        assert!(
            !trace.contains(&"consumer-polled-before-announcement"),
            "{trace:?}"
        );
        assert!(!trace.contains(&"provider-quiesced-too-early"), "{trace:?}");
        assert!(trace.contains(&"consumer-drained"));
        assert!(trace.contains(&"provider-quiesced"));
    }
}

#[test]
fn shutdown_announcement_and_expired_budget_are_never_renewed() {
    let trace = DestructionTrace::default();
    let mut loaded = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["provider", "consumer"],
        |_, name| {
            let mut module = feature(name, &trace);
            module.shutdown_api = Some(amaterasu_abi::ModuleShutdownApi {
                header: AbiHeader::of::<amaterasu_abi::ModuleShutdownApi>(),
                prepare: announce_fixture_shutdown,
            });
            if name == "consumer" {
                module.dependencies.insert("provider".into());
                module.quiesce_api = Some(amaterasu_abi::ModuleQuiesceApi {
                    header: AbiHeader::of::<amaterasu_abi::ModuleQuiesceApi>(),
                    poll: shutdown_consumer_poll,
                });
            } else {
                module.api.tick = shutdown_provider_tick;
            }
            Ok(module)
        },
        &mut |_| Ok(()),
    )
    .unwrap();

    assert!(loaded.quiesce(std::time::Duration::ZERO).is_err());
    assert!(loaded.quiesce(std::time::Duration::from_secs(1)).is_err());
    for module in &loaded.modules {
        assert_eq!(shutdown_fixture_budgets(module), [0]);
    }
    assert!(!trace.lock().unwrap().contains(&"provider-progress"));
    assert!(!loaded.quiesced);
}

#[test]
fn failed_shutdown_announcement_does_not_prevent_other_consumers_draining() {
    unsafe extern "C" fn fail(instance: *mut c_void, _: u64) -> Status {
        // SAFETY: the host lends the live fixture for its single announcement.
        let instance = unsafe { &*instance.cast::<FixtureInstance>() };
        instance.trace.lock().unwrap().push("announcement-failed");
        STATUS_INTERNAL_ERROR
    }
    let trace = DestructionTrace::default();
    let mut loaded = LoadedComponents::load_features(
        runtime(&trace),
        None,
        lifecycle_endpoint(),
        false,
        ["provider", "consumer", "failing"],
        |_, name| {
            let mut module = feature(name, &trace);
            module.shutdown_api = Some(amaterasu_abi::ModuleShutdownApi {
                header: AbiHeader::of::<amaterasu_abi::ModuleShutdownApi>(),
                prepare: if name == "failing" {
                    fail
                } else {
                    announce_fixture_shutdown
                },
            });
            if name == "consumer" {
                module.dependencies.insert("provider".into());
                module.quiesce_api = Some(amaterasu_abi::ModuleQuiesceApi {
                    header: AbiHeader::of::<amaterasu_abi::ModuleQuiesceApi>(),
                    poll: shutdown_consumer_poll,
                });
            } else if name == "provider" {
                module.api.tick = shutdown_provider_tick;
            }
            Ok(module)
        },
        &mut |_| Ok(()),
    )
    .unwrap();

    let error = loaded
        .quiesce(std::time::Duration::from_secs(1))
        .unwrap_err();
    assert!(error.contains("prepare shutdown failing failed"), "{error}");
    assert!(!loaded.quiesced);
    let trace = trace.lock().unwrap().clone();
    assert_eq!(trace[0], "announcement-failed");
    assert!(trace.contains(&"consumer-drained"));
    assert!(
        !trace.contains(&"consumer-polled-before-announcement"),
        "{trace:?}"
    );
}
