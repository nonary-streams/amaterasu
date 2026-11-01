//! Versioned native pairing identity adapters.
use crate::{ModuleResult, NativeBinding, RuntimeClient, abi::NativeServiceFactory};
pub use amaterasu_identity::{CONTRACT, IdentityApi, IdentityApiV1_1};

/// # Safety
/// The factory implements the identity contract and outlives every binding.
pub unsafe fn register(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, amaterasu_identity::CONTRACT, factory) }
}

/// # Safety
/// The runtime and identity provider outlive the binding and all callback calls.
pub unsafe fn bind(runtime: &RuntimeClient, service: &str) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            amaterasu_identity::CONTRACT,
            amaterasu_identity::INTERFACE_MAJOR,
            amaterasu_identity::INTERFACE_MINOR_PERMISSIONS,
            size_of::<amaterasu_identity::IdentityApiV1_1>() as u32,
        )
    }
}
