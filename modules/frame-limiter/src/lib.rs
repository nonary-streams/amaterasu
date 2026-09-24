//! Validated frame-limiter settings and pure launch-policy decisions.
mod model;
mod policy;
mod runtime;
mod service;

pub use model::{
    AutoVirtualFramegenMode, EffectiveSettings, FpsLimit, FrameGenerationProvider,
    FrameLimiterOverrides, FrameLimiterProvider, GlobalSettings, MangoHudLimiterMethod,
    MangoHudPreset, ParsedGlobalSettings, Platform, RtssFrameLimitType, SettingsDiagnostic,
    SettingsError,
};
pub use policy::{
    EffectiveFrameLimit, FallbackCondition, FrameLimitSource, FrameLimiterDecision, PolicyContext,
    PolicyDiagnostic, ProviderAttempt, ProviderBackend, ProviderPlan, RtssSyncLimiterDecision,
    RtssSyncLimiterReason, evaluate_policy, plan_provider, resolve_effective_limit,
    resolve_rtss_sync_limiter, should_consider_apply,
};
pub use runtime::FrameLimiterModule;
pub use service::{
    ApplyRequest, CONTRACT_VERSION, GameFrameGenerationMode, Operation, OperationResponse,
    PolicyOnlyPlan, Presence, ProviderAvailability, ProviderCandidate, ResponseState,
    RestoreRequest, ServiceError, SettingsInput, StatusRequest, build_response, handle_operation,
    handle_payload, parse_operation,
};

#[cfg(test)]
mod tests;

amaterasu_module_sdk::export_module!(FrameLimiterModule);
