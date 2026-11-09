//! Direct calls: a provider serves one request synchronously on its caller's
//! thread instead of through a queued broker request.
//!
//! The trusted host discovers `amaterasu_module_direct_v1` in a verified module
//! library and registers it with the runtime, so no module supplies a function
//! address through a control message. Authorization is unchanged: the runtime
//! checks the caller's `call` grant and routes only to the provider that
//! registered the broker service.
use crate::{AbiHeader, BorrowedBytes, ReplySink, Status};
use std::ffi::c_void;

/// `(instance, service, authenticated caller, request, reply)`.
///
/// `STATUS_OK` means exactly one reply was written. `STATUS_UNAVAILABLE` means
/// the provider did nothing and the caller should use an ordinary request.
/// The call must be bounded, must not block, and must not unwind.
pub type DirectCallHandler = unsafe extern "C" fn(
    *mut c_void,
    BorrowedBytes,
    BorrowedBytes,
    BorrowedBytes,
    ReplySink,
) -> Status;

/// Optional module contract, exported as `amaterasu_module_direct_v1`.
/// Invoked only on the thread that makes the module's lifecycle calls, and
/// never while another call into the same instance is active.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ModuleDirectApi {
    pub header: AbiHeader,
    pub call: DirectCallHandler,
}
pub type ModuleDirectEntry = unsafe extern "C" fn() -> *const ModuleDirectApi;

/// Optional runtime contract, exported as `amaterasu_runtime_direct_v1`.
/// Only the host calls it, with tables read from verified libraries.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RuntimeDirectApi {
    pub header: AbiHeader,
    /// `(runtime, principal, instance, handler)`. Binds the calling thread as
    /// the only thread allowed to reach this instance. The instance and its
    /// library must stay live until `unregister` returns.
    pub register:
        unsafe extern "C" fn(*mut c_void, BorrowedBytes, *mut c_void, DirectCallHandler) -> Status,
    /// `(runtime, principal)`. After it returns the handler is never invoked.
    pub unregister: unsafe extern "C" fn(*mut c_void, BorrowedBytes) -> Status,
}
pub type RuntimeDirectEntry = unsafe extern "C" fn() -> *const RuntimeDirectApi;
