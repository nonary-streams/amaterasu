//! Typed native audio interfaces shared by audio providers and consumers.
//!
//! These identifiers describe the interface family independently of a service
//! alias. Signed composition still controls provider selection and authorization.
mod negotiation;
mod ordered;
pub use negotiation::{
    AudioCandidate, AudioFormat, AudioNegotiationError, AudioSelection, configure, negotiate, probe,
};
pub use ordered::{OrderedAudioCandidate, OrderedAudioFormat, configure_ordered, probe_ordered};

use crate::{ModuleResult, NativeBinding, RuntimeClient, abi::NativeServiceFactory};
use amaterasu_media::{
    AudioCaptureApi, AudioEncoderApi, INTERFACE_MAJOR, NegotiatedAudioEncoderApi,
    OrderedAudioCaptureApiV2, OrderedAudioEncoderApiV2, OrderedAudioTransportApiV2,
};

pub const CAPTURE_CONTRACT: &str = "amaterasu.audio-capture.native/1";
pub const ENCODER_CONTRACT: &str = "amaterasu.audio-encoder.native/1";
/// Separate family; the legacy Opus table and behavior remain unchanged.
pub const NEGOTIATED_ENCODER_CONTRACT: &str = "amaterasu.audio-encoder-negotiated.native/1";
pub const ORDERED_CAPTURE_CONTRACT: &str = "amaterasu.audio-capture-ordered.native/2";
pub const ORDERED_ENCODER_CONTRACT: &str = "amaterasu.audio-encoder-ordered.native/2";
pub const ORDERED_TRANSPORT_CONTRACT: &str = "amaterasu.audio-transport-ordered.native/2";

/// # Safety
/// The factory implements the ordered transport table and outlives all bindings.
pub unsafe fn register_ordered_transport(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, ORDERED_TRANSPORT_CONTRACT, factory) }
}

/// # Safety
/// The runtime and provider outlive the binding and all calls.
pub unsafe fn bind_ordered_transport(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            ORDERED_TRANSPORT_CONTRACT,
            2,
            0,
            size_of::<OrderedAudioTransportApiV2>() as u32,
        )
    }
}

/// # Safety
/// The factory implements the ordered capture table and outlives all bindings.
pub unsafe fn register_ordered_capture(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, ORDERED_CAPTURE_CONTRACT, factory) }
}

/// # Safety
/// The runtime and provider outlive the binding and all calls.
pub unsafe fn bind_ordered_capture(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            ORDERED_CAPTURE_CONTRACT,
            2,
            0,
            size_of::<OrderedAudioCaptureApiV2>() as u32,
        )
    }
}

/// # Safety
/// The factory implements the ordered encoder table and outlives all bindings.
pub unsafe fn register_ordered_encoder(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, ORDERED_ENCODER_CONTRACT, factory) }
}

/// # Safety
/// The runtime and provider outlive the binding and all calls.
pub unsafe fn bind_ordered_encoder(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            ORDERED_ENCODER_CONTRACT,
            2,
            0,
            size_of::<OrderedAudioEncoderApiV2>() as u32,
        )
    }
}

/// # Safety
/// Factory implements the negotiated table and remains live through binding release.
pub unsafe fn register_negotiated_encoder(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, NEGOTIATED_ENCODER_CONTRACT, factory) }
}

/// # Safety
/// The runtime and provider outlive the binding and all stream operations.
pub unsafe fn bind_negotiated_encoder(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            NEGOTIATED_ENCODER_CONTRACT,
            1,
            0,
            size_of::<NegotiatedAudioEncoderApi>() as u32,
        )
    }
}

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
