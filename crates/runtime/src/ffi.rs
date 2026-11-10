//! The only unsafe boundary in the communication runtime.
//! Endpoint identity is bound here; JSON messages cannot choose their principal.
use crate::Broker;
use amaterasu_abi::*;
use amaterasu_contracts::{Command, Response, RuntimeConfig};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

struct Endpoint {
    principal: String,
    broker: Arc<Mutex<Broker>>,
    native: Arc<crate::native::NativeServices>,
    work: Arc<crate::work_signal::WorkSignal>,
    timing: Mutex<crate::invocation_timing::InvocationTiming>,
    timing_dropped: std::sync::atomic::AtomicU64,
    trace: Arc<crate::trace::TraceHub>,
    /// Created on this endpoint's first `trace_bind`; its table address is
    /// stable until the runtime is destroyed.
    trace_sink: std::sync::OnceLock<crate::trace::sink::Binding>,
    direct: Arc<crate::direct::DirectRegistry>,
}
struct Runtime {
    endpoints: BTreeMap<String, Box<Endpoint>>,
    work: Arc<crate::work_signal::WorkSignal>,
    broker: Arc<Mutex<Broker>>,
    trace: Arc<crate::trace::TraceHub>,
    direct: Arc<crate::direct::DirectRegistry>,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        // Every module is gone, so no producer can still be recording.
        self.trace.shutdown();
    }
}
fn guard(f: impl FnOnce() -> Result<(), Status>) -> Status {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => STATUS_OK,
        Ok(Err(e)) => e,
        Err(_) => STATUS_INTERNAL_ERROR,
    }
}
unsafe extern "C" fn create_runtime(config: BorrowedBytes, output: *mut *mut c_void) -> Status {
    guard(|| {
        if output.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: The bootstrap caller supplies readable configuration bytes for this call.
        let config: RuntimeConfig = serde_json::from_slice(unsafe { config.read()? })
            .map_err(|_| STATUS_INVALID_ARGUMENT)?;
        let native = Arc::new(crate::native::NativeServices::new(
            config.principals.clone(),
            config.legacy_native_contracts.clone(),
        ));
        let principals: Vec<_> = config.principals.keys().cloned().collect();
        let broker = Arc::new(Mutex::new(
            Broker::new(config).map_err(|_| STATUS_INVALID_ARGUMENT)?,
        ));
        let work = Arc::new(crate::work_signal::WorkSignal::default());
        let trace = Arc::new(crate::trace::TraceHub::new(principals.clone()));
        let direct = Arc::new(crate::direct::DirectRegistry::default());
        let endpoints = principals
            .into_iter()
            .map(|principal| {
                (
                    principal.clone(),
                    Box::new(Endpoint {
                        principal,
                        broker: broker.clone(),
                        native: native.clone(),
                        work: work.clone(),
                        timing: Mutex::new(Default::default()),
                        timing_dropped: std::sync::atomic::AtomicU64::new(0),
                        trace: trace.clone(),
                        trace_sink: std::sync::OnceLock::new(),
                        direct: direct.clone(),
                    }),
                )
            })
            .collect();
        // SAFETY: The caller supplies writable output storage; ownership transfers to destroy.
        unsafe {
            *output = Box::into_raw(Box::new(Runtime {
                endpoints,
                work,
                broker,
                trace,
                direct,
            })) as *mut c_void;
        }
        Ok(())
    })
}
unsafe extern "C" fn create_endpoint(
    runtime: *mut c_void,
    name: BorrowedBytes,
    output: *mut HostApi,
) -> Status {
    guard(|| {
        if runtime.is_null() || output.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: The caller supplies readable name bytes for this call.
        let name =
            std::str::from_utf8(unsafe { name.read()? }).map_err(|_| STATUS_INVALID_ARGUMENT)?;
        // SAFETY: The runtime handle remains live until all endpoint users have stopped.
        // Shared access permits endpoint lookup while existing endpoints are being invoked.
        let runtime = unsafe { &*(runtime as *const Runtime) };
        let endpoint = runtime.endpoints.get(name).ok_or(STATUS_INVALID_ARGUMENT)?;
        // SAFETY: Output is writable. Boxed endpoints have stable addresses until destruction.
        unsafe {
            *output = HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: &**endpoint as *const Endpoint as *mut c_void,
                invoke: invoke_command,
            };
        }
        Ok(())
    })
}
unsafe extern "C" fn invoke_command(
    endpoint: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> Status {
    guard(|| {
        if endpoint.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: The host keeps the runtime and its immutable endpoint alive for this call.
        let endpoint = unsafe { &*(endpoint as *const Endpoint) };
        let _hold_boundary = crate::tick_holds::enter_call(
            std::ptr::from_ref(endpoint) as usize,
            &endpoint.principal,
        );
        use crate::invocation_timing::{InvocationSample, micros};
        use std::{sync::atomic::Ordering, time::Instant};
        let started = Instant::now();
        let mut lock_us = None;
        let mut dispatch_us = None;
        let mut native_us = None;
        // SAFETY: Input bytes remain readable until this synchronous invocation returns.
        let command = serde_json::from_slice::<Command>(unsafe { input.read()? });
        let decode_us = micros(started);
        let response = match command {
            Ok(Command::NotifyWorkReady {}) => {
                endpoint.work.notify();
                Response::Ok
            }
            Ok(Command::TraceBind {}) => {
                let sink = endpoint.trace_sink.get_or_init(|| {
                    crate::trace::sink::Binding::new(endpoint.trace.clone(), &endpoint.principal)
                });
                endpoint.trace.ensure_collector(&endpoint.broker);
                Response::TraceBound {
                    address: sink.address(),
                }
            }
            Ok(Command::TraceSnapshot {}) => {
                let permitted = endpoint
                    .broker
                    .lock()
                    .map_err(|_| STATUS_INTERNAL_ERROR)?
                    .may_read_trace(&endpoint.principal);
                if permitted {
                    Response::Result {
                        payload: endpoint.trace.snapshot(),
                    }
                } else {
                    Response::error(
                        "permission_denied",
                        "operation not granted to this endpoint",
                    )
                }
            }
            Ok(Command::CallDirect {
                service,
                payload,
                forward,
            }) => {
                let at = Instant::now();
                let result = call_direct(endpoint, &service, &payload, forward);
                native_us = Some(micros(at));
                result
            }
            Ok(
                command @ (Command::NativeRegister { .. }
                | Command::NativeBind { .. }
                | Command::NativeRegisterTyped { .. }
                | Command::NativeBindTyped { .. }
                | Command::NativeRelease { .. }),
            ) => {
                // SAFETY: only explicitly native-granted endpoints may supply pointers;
                // provider allocations/callbacks obey the native ABI lifetime contract.
                let at = Instant::now();
                let result = unsafe { endpoint.native.dispatch(&endpoint.principal, command) };
                native_us = Some(micros(at));
                result
            }
            Ok(command) => {
                let lock_started = Instant::now();
                let mut broker = endpoint.broker.lock().map_err(|_| STATUS_INTERNAL_ERROR)?;
                lock_us = Some(micros(lock_started));
                let dispatch_started = Instant::now();
                let result = broker.dispatch(&endpoint.principal, command);
                dispatch_us = Some(micros(dispatch_started));
                result
            }
            Err(_) => Response::error("invalid_request", "invalid control message"),
        };
        // The broker lock is gone before calling the host-owned reply callback.
        let serialize_started = Instant::now();
        let bytes = serde_json::to_vec(&response).map_err(|_| STATUS_INTERNAL_ERROR)?;
        let serialize_us = micros(serialize_started);
        let reply_started = Instant::now();
        // SAFETY: The host keeps the callback/context alive; bytes are borrowed only by send.
        unsafe { reply.send(&bytes) };
        let sample = InvocationSample {
            decode_us,
            lock_us,
            dispatch_us,
            native_us,
            serialize_us,
            reply_us: micros(reply_started),
            total_us: micros(started),
        };
        // Diagnostics never wait for another observer or reenter the native endpoint.
        if let Ok(mut timing) = endpoint.timing.try_lock() {
            let dropped = endpoint.timing_dropped.swap(0, Ordering::Relaxed);
            if let Some(report) = timing.record(&endpoint.principal, sample, dropped) {
                if let Ok(mut broker) = endpoint.broker.try_lock() {
                    broker.record_diagnostic(false, "runtime.invocation", report);
                } else {
                    endpoint.timing_dropped.fetch_add(1, Ordering::Relaxed);
                }
            } else if dropped != 0 {
                endpoint
                    .timing_dropped
                    .fetch_add(dropped, Ordering::Relaxed);
            }
        } else {
            endpoint.timing_dropped.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    })
}
/// Runs the provider's handler on the calling thread. No runtime lock is held
/// while provider code runs, so the handler may issue its own runtime calls.
fn call_direct(
    endpoint: &Endpoint,
    service: &str,
    payload: &serde_json::Value,
    forward: bool,
) -> Response {
    let Ok(request) = serde_json::to_vec(payload) else {
        return Response::error("invalid_request", "payload cannot be encoded");
    };
    let route = {
        let Ok(broker) = endpoint.broker.lock() else {
            return Response::error("runtime_unavailable", "broker unavailable");
        };
        match broker.direct_call_route(&endpoint.principal, service, request.len()) {
            Ok(route) => route,
            Err(refusal) => return refusal,
        }
    };
    let unavailable = || {
        Response::error(
            amaterasu_contracts::DIRECT_UNAVAILABLE,
            "the provider does not serve this operation directly",
        )
    };
    let Some(handler) = endpoint.direct.for_current_thread(&route.provider) else {
        return unavailable();
    };
    let caller = if forward {
        // A layer passing on the direct call it is serving keeps that call's
        // caller, so the next hop authorizes the consumer and not the layer.
        // Nothing else can be forwarded, and the refusal precedes any effect.
        let served = route.caller_is_layer.then(|| {
            endpoint
                .direct
                .caller_being_served(&endpoint.principal, service)
        });
        match served.flatten() {
            Some(caller) => caller,
            None => {
                return Response::error(
                    "invalid_request",
                    "no direct call for this service is being served to forward",
                );
            }
        }
    } else {
        endpoint.principal.clone()
    };
    let _serving = endpoint.direct.serve(&route.provider, service, &caller);
    unsafe extern "C" fn copy(context: *mut c_void, bytes: BorrowedBytes) {
        // SAFETY: the context is the reply vector below, borrowed for this call.
        if let Ok(bytes) = unsafe { bytes.read() } {
            unsafe { &mut *context.cast::<Vec<u8>>() }.extend_from_slice(bytes);
        }
    }
    let mut reply = Vec::new();
    // SAFETY: the host registered this handler from a verified module library
    // and keeps the instance live until it unregisters on this same thread.
    // Every borrowed buffer outlives the synchronous call.
    let status = unsafe {
        (handler.call)(
            handler.instance,
            BorrowedBytes::new(service.as_bytes()),
            BorrowedBytes::new(caller.as_bytes()),
            BorrowedBytes::new(&request),
            ReplySink {
                context: std::ptr::from_mut(&mut reply).cast(),
                write: copy,
            },
        )
    };
    match status {
        STATUS_OK if reply.len() <= amaterasu_contracts::MAX_BROKER_RESULT_BYTES => {
            match serde_json::from_slice(&reply) {
                Ok(payload) => Response::Result { payload },
                Err(_) => Response::error("direct_failed", "provider reply is not valid"),
            }
        }
        STATUS_OK => Response::error("direct_failed", "provider reply is too large"),
        STATUS_UNAVAILABLE => unavailable(),
        _ => Response::error("direct_failed", "provider could not complete the call"),
    }
}
unsafe extern "C" fn register_direct(
    runtime: *mut c_void,
    principal: BorrowedBytes,
    instance: *mut c_void,
    handler: DirectCallHandler,
) -> Status {
    guard(|| {
        if runtime.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: the host lends readable principal bytes for this call.
        let principal = std::str::from_utf8(unsafe { principal.read()? })
            .map_err(|_| STATUS_INVALID_ARGUMENT)?;
        // SAFETY: the host retains its runtime instance for this call.
        let runtime = unsafe { &*runtime.cast::<Runtime>() };
        if !runtime.endpoints.contains_key(principal)
            || !runtime.direct.register(principal, instance, handler)
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    })
}
unsafe extern "C" fn unregister_direct(runtime: *mut c_void, principal: BorrowedBytes) -> Status {
    guard(|| {
        if runtime.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: the host lends readable principal bytes for this call.
        let principal = std::str::from_utf8(unsafe { principal.read()? })
            .map_err(|_| STATUS_INVALID_ARGUMENT)?;
        // SAFETY: the host retains its runtime instance for this call.
        unsafe { &*runtime.cast::<Runtime>() }
            .direct
            .unregister(principal);
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn amaterasu_runtime_direct_v1() -> *const RuntimeDirectApi {
    static DIRECT: RuntimeDirectApi = RuntimeDirectApi {
        header: AbiHeader::of::<RuntimeDirectApi>(),
        register: register_direct,
        unregister: unregister_direct,
    };
    &DIRECT
}
unsafe extern "C" fn destroy_runtime(runtime: *mut c_void) {
    if !runtime.is_null() {
        // SAFETY: The bootstrap owner calls destroy once, after all endpoint calls stop.
        drop(unsafe { Box::from_raw(runtime as *mut Runtime) });
    }
}
static API: RuntimeApi = RuntimeApi {
    header: AbiHeader::of::<RuntimeApi>(),
    create: create_runtime,
    endpoint: create_endpoint,
    destroy: destroy_runtime,
};
#[unsafe(no_mangle)]
pub extern "C" fn amaterasu_runtime_v1() -> *const RuntimeApi {
    &API
}

unsafe extern "C" fn observe_work(runtime: *mut c_void, output: *mut u64) -> Status {
    guard(|| {
        if runtime.is_null() || output.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: host retains its runtime instance and lends a writable result.
        unsafe {
            *output = (&*runtime.cast::<Runtime>()).work.observe();
        }
        Ok(())
    })
}
unsafe extern "C" fn wait_work(runtime: *mut c_void, observed: u64, timeout_us: u64) -> Status {
    guard(|| {
        if runtime.is_null() || timeout_us > 1_000_000 {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: the host retains this runtime until the synchronous wait returns.
        let runtime = unsafe { &*runtime.cast::<Runtime>() };
        if let Some(report) = crate::tick_holds::begin_wait()
            && let Ok(mut broker) = runtime.broker.lock()
        {
            broker.record_tick_holds(report);
        }
        runtime
            .work
            .wait(observed, std::time::Duration::from_micros(timeout_us));
        crate::tick_holds::end_wait();
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn amaterasu_runtime_wait_v1() -> *const RuntimeWaitApi {
    static WAIT: RuntimeWaitApi = RuntimeWaitApi {
        header: AbiHeader::of::<RuntimeWaitApi>(),
        observe: observe_work,
        wait: wait_work,
    };
    &WAIT
}

#[cfg(test)]
mod tests;
