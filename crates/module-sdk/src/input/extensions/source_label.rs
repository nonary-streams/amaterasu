//! Parsing for the optional lifecycle source label. A label is not authority.
//!
//! Checkpoint: lifecycle `0fb7f221b918565fdad7764daa4fd453d90d12dc`,
//! `docs/input-binding-source-authority.md`. A successful observation does not
//! close the protocol revocation race, authorize an effect, install a route, or
//! replace the selected consumer's input binding and boot-epoch checks.

use amaterasu_extension_api::LifecycleTarget;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLabel {
    pub version: u32,
    pub id: String,
    pub generation: u64,
}

impl SourceLabel {
    pub(crate) fn valid_for(&self, stream_generation: u64) -> bool {
        self.version == 1
            && self.generation == stream_generation
            && stream_generation != 0
            && self.id.len() == 32
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}

/// Parse a negotiated `source_version: 1` lifecycle response against the exact
/// expected target. `None` means no source was attested. Callers must obtain the
/// response from the authenticated lifecycle service and independently enforce
/// protocol-owned admission and revoke/drain barriers before any source effect.
/// This pure parser is intentionally not connected to route installation.
pub fn observe(
    response: &Value,
    expected: &LifecycleTarget,
) -> Result<Option<SourceLabel>, &'static str> {
    let LifecycleTarget::Stream {
        host_epoch,
        session_id,
        stream_id,
        generation,
    } = expected
    else {
        return Err("invalid_stream_target");
    };
    if *generation == 0
        || [host_epoch, session_id, stream_id].iter().any(|value| {
            value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
        })
    {
        return Err("invalid_stream_target");
    }
    if response.get("status").and_then(Value::as_str) != Some("succeeded") {
        return Err("invalid_binding_response");
    }
    let target: LifecycleTarget = serde_json::from_value(
        response
            .get("target")
            .cloned()
            .ok_or("invalid_binding_target")?,
    )
    .map_err(|_| "invalid_binding_target")?;
    if &target != expected {
        return Err("binding_target_mismatch");
    }
    let active = response
        .get("active")
        .and_then(Value::as_bool)
        .ok_or("invalid_binding_response")?;
    let source = response
        .get("source")
        .ok_or("source_version_not_negotiated")?;
    if source.is_null() {
        return Ok(None);
    }
    let source: SourceLabel =
        serde_json::from_value(source.clone()).map_err(|_| "invalid_source_label")?;
    if !active || !source.valid_for(*generation) {
        return Err("invalid_source_label");
    }
    Ok(Some(source))
}
