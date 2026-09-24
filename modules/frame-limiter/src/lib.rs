//! Validated frame-limiter settings and pure launch-policy decisions.
mod model;
mod policy;

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

#[cfg(test)]
mod tests;
