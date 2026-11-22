//! Exact capture opens use `capture_binding` in place of `output`. The
//! selected provider interprets and validates the selector schema. Open
//! retains the ordinary caller-scoped operation and stream IDs. It must
//! publish `observed_capture` on success and refresh it on each Lookup.
//! Observations include provider-specific evidence and the common `width`,
//! `height`, and `refresh_millihz` fields. A provider honors an explicit
//! `source_format` by returning that format in its successful Open reply.
//! An unsupported schema, missing target or unavailable format is a refusal
//! for this binding. Lookup and Close keep their ordinary retirement contracts.
use crate::extension_api::{CaptureBinding, validate_capture_binding};
use serde_json::Value;

/// Decode an exact binding before interpreting the provider's selector schema.
pub fn decode_capture_binding(value: Value) -> Result<CaptureBinding, &'static str> {
    let binding = serde_json::from_value(value).map_err(|_| "invalid_capture_binding")?;
    validate_capture_binding(&binding).map_err(|_| "invalid_capture_binding")?;
    Ok(binding)
}

/// Read bounded evidence from the provider that opened this exact binding.
/// The lifecycle owner separately verifies the provider-specific identity.
pub fn decode_capture_observation(
    payload: &Value,
    binding: &CaptureBinding,
) -> Result<Value, &'static str> {
    let observed = &payload["observed_capture"];
    if !observed.is_object()
        || serde_json::to_vec(observed).map_or(true, |bytes| bytes.len() > 8192)
        || observed["width"] != binding.mode.width
        || observed["height"] != binding.mode.height
        || observed["refresh_millihz"] != binding.mode.refresh_millihz
    {
        return Err("invalid_capture_observation");
    }
    Ok(observed.clone())
}
