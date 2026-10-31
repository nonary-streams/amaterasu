//! Native transport interface identities and adapters.
//!
//! Providers must register the appropriate family even under custom service
//! aliases. Untyped legacy registrations are rejected for every alias; a
//! matching version and table size do not identify the interface.
use crate::{ModuleResult, NativeBinding, RuntimeClient, abi::NativeServiceFactory};
use amaterasu_media::{ControlTransportApi, MediaTransportApi};

pub const MEDIA_CONTRACT: &str = "amaterasu.media-transport.native/1";
pub const CONTROL_CONTRACT: &str = "amaterasu.control-transport.native/1";

/// # Safety
/// The factory implements the media transport interface and remains live through all bindings.
pub unsafe fn register_media(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, MEDIA_CONTRACT, factory) }
}
/// # Safety
/// The factory implements the control transport interface and remains live through all bindings.
pub unsafe fn register_control(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, CONTROL_CONTRACT, factory) }
}
/// # Safety
/// The runtime and provider must outlive the binding and its calls.
pub unsafe fn bind_media(runtime: &RuntimeClient, service: &str) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            MEDIA_CONTRACT,
            1,
            0,
            size_of::<MediaTransportApi>() as u32,
        )
    }
}
/// # Safety
/// The runtime and provider must outlive the binding and its calls.
pub unsafe fn bind_control(runtime: &RuntimeClient, service: &str) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            CONTROL_CONTRACT,
            1,
            0,
            size_of::<ControlTransportApi>() as u32,
        )
    }
}
