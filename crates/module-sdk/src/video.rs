//! Typed native video output interface.
use crate::{ModuleResult, NativeBinding, RuntimeClient, abi::NativeServiceFactory};
use amaterasu_media::VideoApi;

/// Interface family independent of the selected service alias.
pub const CONTRACT: &str = "amaterasu.video.native/1";

/// # Safety
/// The factory implements VideoApi and remains live until all bindings are released.
pub unsafe fn register(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, CONTRACT, factory) }
}

/// # Safety
/// The runtime and provider must outlive the binding and its calls.
pub unsafe fn bind(runtime: &RuntimeClient, service: &str) -> ModuleResult<NativeBinding> {
    unsafe { runtime.bind_native_typed(service, CONTRACT, 1, 0, size_of::<VideoApi>() as u32) }
}
