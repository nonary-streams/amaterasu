//! Native transport interface identities and adapters.
//!
//! Providers must register the appropriate family even under custom service
//! aliases. Untyped legacy registrations are rejected for every alias; a
//! matching version and table size do not identify the interface.
use crate::{ModuleResult, NativeBinding, RuntimeClient, abi::NativeServiceFactory};
use amaterasu_media::{ControlTransportApi, MediaTransportApi, MediaTransportApiV1_1};

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

/// Return the timed-video tail if this provider advertises media transport
/// v1.1. Older providers retain their original, untimed API.
///
/// # Safety
/// The binding must be a live media transport binding; table references and
/// callbacks cannot outlive it.
pub unsafe fn media_timed(binding: &NativeBinding) -> ModuleResult<Option<&MediaTransportApiV1_1>> {
    // SAFETY: caller guarantees this binding is the exact media transport family.
    let prefix = unsafe { binding.table::<MediaTransportApi>() }?;
    if prefix.base.header.minor < 1 {
        return Ok(None);
    }
    // SAFETY: the advertised v1.1 tail is checked for size by table().
    unsafe { binding.table::<MediaTransportApiV1_1>() }.map(Some)
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
