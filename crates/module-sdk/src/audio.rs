//! Typed native audio interfaces shared by audio providers and consumers.
//!
//! These identifiers describe the interface family independently of a service
//! alias. Signed composition still controls provider selection and authorization.
use crate::{ModuleResult, NativeBinding, RuntimeClient, abi::NativeServiceFactory};
use amaterasu_media::{AudioCaptureApi, AudioEncoderApi, INTERFACE_MAJOR};

pub const CAPTURE_CONTRACT: &str = "amaterasu.audio-capture.native/1";
pub const ENCODER_CONTRACT: &str = "amaterasu.audio-encoder.native/1";

/// Register the PCM capture table for a selected service alias.
///
/// # Safety
/// The factory must implement `AudioCaptureApi` and satisfy the native SDK's
/// factory lifetime, callback, ownership, and concurrency requirements.
pub unsafe fn register_capture(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, CAPTURE_CONTRACT, factory) }
}

/// Register the encoded audio table for a selected service alias.
///
/// # Safety
/// The factory must implement `AudioEncoderApi` and satisfy the native SDK's
/// factory lifetime, callback, ownership, and concurrency requirements.
pub unsafe fn register_encoder(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, ENCODER_CONTRACT, factory) }
}

/// Bind a provider promising the PCM capture contract.
///
/// # Safety
/// The runtime and provider must outlive the returned binding and all calls.
/// The caller must obey the audio interface's ownership and threading rules.
pub unsafe fn bind_capture(runtime: &RuntimeClient, service: &str) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            CAPTURE_CONTRACT,
            INTERFACE_MAJOR,
            0,
            size_of::<AudioCaptureApi>() as u32,
        )
    }
}

/// Bind a provider promising the encoded audio contract.
///
/// # Safety
/// The runtime and provider must outlive the returned binding and all calls.
/// The caller must obey the audio interface's ownership and threading rules.
pub unsafe fn bind_encoder(runtime: &RuntimeClient, service: &str) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            ENCODER_CONTRACT,
            INTERFACE_MAJOR,
            0,
            size_of::<AudioEncoderApi>() as u32,
        )
    }
}
