//! ABI adapters shared by Rust modules. Calls are serialized by the host.
use amaterasu_abi::{HostApi, STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{ManagedModule, ModuleResult, RuntimeClient};

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
        unsafe { *output = Box::into_raw(Box::new(module)).cast() };
        Ok(())
    })
}

/// # Safety
/// `instance` must refer to a live M created by this library, exclusively borrowed.
pub unsafe extern "C" fn tick<M: ManagedModule>(instance: *mut c_void) -> Status {
    if instance.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: lifecycle calls are serialized and the instance belongs to this module.
    contain_failure(|| unsafe { &mut *instance.cast::<M>() }.tick())
}

/// # Safety
/// `instance` must refer to a live M created by this library.
pub unsafe extern "C" fn health<M: ManagedModule>(instance: *mut c_void) -> Status {
    if instance.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the instance remains alive throughout the lifecycle call.
    contain_failure(|| unsafe { &*instance.cast::<M>() }.health())
}

/// # Safety
/// Consume a live M exactly once, after all its other calls have finished.
pub unsafe extern "C" fn destroy<M: ManagedModule>(instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let _ = contain_failure(|| {
        // SAFETY: this library allocated the instance and the host relinquishes ownership.
        drop(unsafe { Box::from_raw(instance.cast::<M>()) });
        Ok(())
    });
}
