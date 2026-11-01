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
}

fn fixture_instance(name: &'static str, trace: &DestructionTrace) -> *mut c_void {
    Box::into_raw(Box::new(FixtureInstance {
        name,
        trace: trace.clone(),
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
        api: ModuleApi {
            header: AbiHeader::of::<ModuleApi>(),
            create: create_module,
            tick: healthy,
            health: healthy,
            destroy,
        },
        retirement: None,
        retirement_failed: false,
        retirement_complete: false,
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
