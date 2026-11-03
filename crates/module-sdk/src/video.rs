//! Typed native video output interface.
use crate::{ModuleResult, NativeBinding, RuntimeClient, abi::NativeServiceFactory};
use amaterasu_media::{VideoApi, VideoApiV1_3};

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

/// Return the timed-output tail if the provider advertises video v1.3.
/// Older providers remain usable but cannot supply a separate processing time.
///
/// # Safety
/// The binding must be a live video binding and all callback lifetimes must be
/// respected. The returned reference must not outlive the binding.
pub unsafe fn timed(binding: &NativeBinding) -> ModuleResult<Option<&VideoApiV1_3>> {
    // SAFETY: caller guarantees this binding is the exact video family.
    let prefix = unsafe { binding.table::<VideoApi>() }?;
    if prefix.base.header.minor < 3 {
        return Ok(None);
    }
    // SAFETY: v1.3 advertises this additive table; size is checked by table().
    unsafe { binding.table::<VideoApiV1_3>() }.map(Some)
}
