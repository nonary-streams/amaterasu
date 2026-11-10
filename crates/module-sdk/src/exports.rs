//! ABI adapters shared by Rust modules. Calls are serialized by the host.
use amaterasu_abi::{
    BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT, STATUS_OK,
    STATUS_UNAVAILABLE, Status,
};
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::ThreadId;

use crate::{DirectCall, DirectService, ManagedModule, ModuleResult, RuntimeClient};

// The public ABI owns an opaque instance. Keep lifecycle diagnostics here so
// third-party SDK modules receive the same observations as built-in modules.
// Declaration order destroys the module before its borrowed host endpoint.
struct State<M> {
    module: M,
    runtime: RuntimeClient,
    timing: crate::diagnostics::ModuleTiming,
}

/// The host serializes lifecycle calls, but a direct call arrives from inside
/// another module's call and could find this module already on the stack.
/// `entered` turns that into a refusal instead of two mutable borrows.
struct Instance<M> {
    entered: AtomicBool,
    /// The thread that created the module; the only one allowed to enter it.
    thread: ThreadId,
    state: UnsafeCell<State<M>>,
}

/// Exclusive access to the module for one call into it.
struct Entered<'a, M> {
    instance: &'a Instance<M>,
}

impl<M> Instance<M> {
    fn enter(&self) -> Option<Entered<'_, M>> {
        self.entered
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        Some(Entered { instance: self })
    }
}

impl<M> Entered<'_, M> {
    fn state(&mut self) -> &mut State<M> {
        // SAFETY: `entered` admits one `Entered` at a time, and every access to
        // the state goes through one, so this is the only live reference.
        unsafe { &mut *self.instance.state.get() }
    }
}

impl<M> Drop for Entered<'_, M> {
    fn drop(&mut self) {
        self.instance.entered.store(false, Ordering::Release);
    }
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
            entered: AtomicBool::new(false),
            thread: std::thread::current().id(),
            state: UnsafeCell::new(State {
                module,
                runtime,
                timing: crate::diagnostics::ModuleTiming::new(std::time::Instant::now()),
            }),
        };
        unsafe { *output = Box::into_raw(Box::new(instance)).cast() };
        Ok(())
    })
}

/// # Safety
/// `instance` must be a live result of this library's `create::<M>`.
unsafe fn enter<'a, M>(instance: *mut c_void) -> Option<Entered<'a, M>> {
    // SAFETY: the allocation is live; only shared access is created here, and
    // the guard grants the single mutable access to its state.
    unsafe { &*instance.cast::<Instance<M>>() }.enter()
}

/// # Safety
/// `instance` must be returned by this library's `create::<M>`, exclusively borrowed.
pub unsafe extern "C" fn tick<M: ManagedModule>(instance: *mut c_void) -> Status {
    if instance.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the host lends the live instance for this lifecycle call.
    let Some(mut entered) = (unsafe { enter::<M>(instance) }) else {
        return STATUS_INTERNAL_ERROR;
    };
    let state = entered.state();
    let started = std::time::Instant::now();
    state.timing.begin(started);
    let status = contain_failure(|| state.module.tick());
    if let Some(mut summary) =
        state
            .timing
            .finish(started, std::time::Instant::now(), status != STATUS_OK)
    {
        summary["module_type"] = std::any::type_name::<M>().into();
        crate::diagnostics::log_window(&state.runtime, "module.timing", &summary);
    }
    status
}

/// # Safety
/// `instance` must be returned by this library's `create::<M>`, exclusively borrowed.
pub unsafe extern "C" fn health<M: ManagedModule>(instance: *mut c_void) -> Status {
    if instance.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the host lends the live instance for this lifecycle call.
    let Some(mut entered) = (unsafe { enter::<M>(instance) }) else {
        return STATUS_INTERNAL_ERROR;
    };
    let state = entered.state();
    let started = std::time::Instant::now();
    let status = contain_failure(|| state.module.health());
    state
        .timing
        .observe_health(started, std::time::Instant::now(), status != STATUS_OK);
    status
}

/// # Safety
/// `instance` must be returned by this library's `create::<M>`. The borrowed
/// byte ranges and the reply sink must stay valid until this returns.
pub unsafe extern "C" fn direct<M: ManagedModule>(
    instance: *mut c_void,
    service: BorrowedBytes,
    caller: BorrowedBytes,
    request: BorrowedBytes,
    reply: ReplySink,
) -> Status {
    if instance.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the allocation is live; these fields are never mutated.
    if unsafe { &*instance.cast::<Instance<M>>() }.thread != std::thread::current().id() {
        return STATUS_UNAVAILABLE;
    }
    // A module already on the stack declines; the caller falls back to a request.
    // SAFETY: the instance is live for this call.
    let Some(mut entered) = (unsafe { enter::<M>(instance) }) else {
        return STATUS_UNAVAILABLE;
    };
    // SAFETY: the runtime lends these readable ranges for this call.
    let (Ok(service), Ok(caller), Ok(request)) =
        (unsafe { (service.read(), caller.read(), request.read()) })
    else {
        return STATUS_INVALID_ARGUMENT;
    };
    let (Ok(service), Ok(caller), Ok(payload)) = (
        std::str::from_utf8(service),
        std::str::from_utf8(caller),
        serde_json::from_slice::<serde_json::Value>(request),
    ) else {
        return STATUS_INVALID_ARGUMENT;
    };
    let state = entered.state();
    let started = std::time::Instant::now();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        state.module.serve_direct(DirectCall {
            caller,
            service,
            payload,
        })
    }));
    state.timing.observe_direct(started);
    match outcome {
        Ok(DirectService::Replied(payload)) => match serde_json::to_vec(&payload) {
            Ok(bytes) => {
                // SAFETY: the sink is valid for this call and copies the bytes.
                unsafe { reply.send(&bytes) };
                STATUS_OK
            }
            Err(_) => STATUS_INTERNAL_ERROR,
        },
        Ok(DirectService::Declined) => STATUS_UNAVAILABLE,
        // Only "declined" may claim that nothing happened.
        Ok(DirectService::Failed) | Err(_) => STATUS_INTERNAL_ERROR,
    }
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
    let Some(mut entered) = (unsafe { enter::<M>(instance) }) else {
        return STATUS_INTERNAL_ERROR;
    };
    contain_failure(|| entered.state().module.prepare_shutdown(remaining_ms))
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
    // SAFETY: the instance is live for this lifecycle call.
    let Some(mut entered) = (unsafe { enter::<M>(instance) }) else {
        return STATUS_INTERNAL_ERROR;
    };
    contain_failure(|| {
        let complete = entered.state().module.quiesce()?;
        // SAFETY: the host supplied writable storage for this call.
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
