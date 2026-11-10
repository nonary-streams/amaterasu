//! ABI adapters shared by Rust modules. Calls are serialized by the host.
use amaterasu_abi::{HostApi, STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{ManagedModule, ModuleResult, RuntimeClient};

// The public ABI owns an opaque instance. Keep lifecycle diagnostics here so
// third-party SDK modules receive the same observations as built-in modules.
// Declaration order destroys the module before its borrowed host endpoint.
struct Instance<M> {
    module: M,
    runtime: RuntimeClient,
    timing: crate::diagnostics::ModuleTiming,
}

fn contain_failure(operation: impl FnOnce() -> ModuleResult<()>) -> Status {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(())) => STATUS_OK,
        Ok(Err(error)) => {
            eprintln!("module operation failed: {error}");
            STATUS_INTERNAL_ERROR
        }
        Err(_) => STATUS_INTERNAL_ERROR,
    }
}

/// # Safety
/// The host must provide a valid endpoint table and writable instance pointer.
/// The runtime must outlive the module. No lifecycle calls may run concurrently.
pub unsafe extern "C" fn create<M: ManagedModule>(
    host: *const HostApi,
    output: *mut *mut c_void,
) -> Status {
    if host.is_null() || output.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }

    // SAFETY: the host provides a complete HostApi allocation and writable output slot.
    let endpoint = unsafe { *host };
    unsafe { *output = std::ptr::null_mut() };
    if !endpoint.header.supports::<HostApi>() {
        return STATUS_INVALID_ARGUMENT;
    }

    contain_failure(|| {
        // SAFETY: module destruction precedes runtime destruction under the host contract.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let module = M::start(runtime)?;
        // SAFETY: the same host lifetime covers this independent borrowed client.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let instance = Instance {
            module,
            runtime,
            timing: crate::diagnostics::ModuleTiming::new(std::time::Instant::now()),
        };
        unsafe { *output = Box::into_raw(Box::new(instance)).cast() };
        Ok(())
    })
}

/// # Safety
/// `instance` must be returned by this library's `create::<M>`, exclusively borrowed.
pub unsafe extern "C" fn tick<M: ManagedModule>(instance: *mut c_void) -> Status {
    if instance.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: lifecycle calls are serialized and the instance belongs to this module.
    let instance = unsafe { &mut *instance.cast::<Instance<M>>() };
    let started = std::time::Instant::now();
    instance.timing.begin(started);
    let status = contain_failure(|| instance.module.tick());
    if let Some(mut summary) =
        instance
            .timing
            .finish(started, std::time::Instant::now(), status != STATUS_OK)
    {
        summary["module_type"] = std::any::type_name::<M>().into();
        crate::diagnostics::log_window(&instance.runtime, "module.timing", &summary);
    }
    status
}

/// # Safety
/// `instance` must be returned by this library's `create::<M>`, exclusively borrowed.
pub unsafe extern "C" fn health<M: ManagedModule>(instance: *mut c_void) -> Status {
    if instance.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: lifecycle calls are serialized and the instance remains alive.
    let instance = unsafe { &mut *instance.cast::<Instance<M>>() };
    let started = std::time::Instant::now();
    let status = contain_failure(|| instance.module.health());
    instance
        .timing
        .observe_health(started, std::time::Instant::now(), status != STATUS_OK);
    status
}

/// # Safety
/// `instance` must be returned by this library's `create::<M>`, exclusively borrowed.
/// The host announces shutdown before beginning any module's quiesce operation.
pub unsafe extern "C" fn prepare_shutdown<M: ManagedModule>(
    instance: *mut c_void,
    remaining_ms: u64,
) -> Status {
    if instance.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the host serializes lifecycle calls and lends the live module for
    // this bounded announcement; allocation ownership remains with the library.
    contain_failure(|| {
        unsafe { &mut *instance.cast::<Instance<M>>() }
            .module
            .prepare_shutdown(remaining_ms)
    })
}

/// # Safety
/// `instance` is a live, exclusively borrowed result of `create::<M>`;
/// `ready` is writable for this call.
pub unsafe extern "C" fn quiesce<M: ManagedModule>(
    instance: *mut c_void,
    ready: *mut u8,
) -> Status {
    if instance.is_null() || ready.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: The host lends the live instance and a writable result byte. Clear
    // readiness before invoking product code so errors cannot authorize unload.
    unsafe { *ready = 0 };
    contain_failure(|| {
        let complete = unsafe { &mut *instance.cast::<Instance<M>>() }
            .module
            .quiesce()?;
        unsafe { *ready = u8::from(complete) };
        Ok(())
    })
}

/// # Safety
/// Consume a live result of `create::<M>` exactly once, after its other calls finish.
pub unsafe extern "C" fn destroy<M: ManagedModule>(instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let _ = contain_failure(|| {
        // SAFETY: this library allocated the instance and the host relinquishes ownership.
        drop(unsafe { Box::from_raw(instance.cast::<Instance<M>>()) });
        Ok(())
    });
}

#[cfg(test)]
mod tests;
