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
}
struct Runtime {
    endpoints: BTreeMap<String, Box<Endpoint>>,
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
        ));
        let principals: Vec<_> = config.principals.keys().cloned().collect();
        let broker = Arc::new(Mutex::new(
            Broker::new(config).map_err(|_| STATUS_INVALID_ARGUMENT)?,
        ));
        let endpoints = principals
            .into_iter()
            .map(|principal| {
                (
                    principal.clone(),
                    Box::new(Endpoint {
                        principal,
                        broker: broker.clone(),
                        native: native.clone(),
                    }),
                )
            })
            .collect();
        // SAFETY: The caller supplies writable output storage; ownership transfers to destroy.
        unsafe {
            *output = Box::into_raw(Box::new(Runtime { endpoints })) as *mut c_void;
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
        // SAFETY: Input bytes remain readable until this synchronous invocation returns.
        let response = match serde_json::from_slice::<Command>(unsafe { input.read()? }) {
            Ok(
                command @ (Command::NativeRegister { .. }
                | Command::NativeBind { .. }
                | Command::NativeRelease { .. }),
            ) => {
                // SAFETY: only explicitly native-granted endpoints may supply pointers;
                // provider allocations/callbacks obey the native ABI lifetime contract.
                unsafe { endpoint.native.dispatch(&endpoint.principal, command) }
            }
            Ok(command) => endpoint
                .broker
                .lock()
                .map_err(|_| STATUS_INTERNAL_ERROR)?
                .dispatch(&endpoint.principal, command),
            Err(_) => Response::error("invalid_request", "invalid control message"),
        };
        // The broker lock is gone before calling the host-owned reply callback.
        let bytes = serde_json::to_vec(&response).map_err(|_| STATUS_INTERNAL_ERROR)?;
        // SAFETY: The host keeps the callback/context alive; bytes are borrowed only by send.
        unsafe { reply.send(&bytes) };
        Ok(())
    })
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

#[cfg(test)]
mod tests;
