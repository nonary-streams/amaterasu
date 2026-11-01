//! Broker-serialized admission for selected protocol effects.
//!
//! Only trusted composition and the selected authority may install/open a
//! route. These wire helpers do not establish authentication, recovery, or
//! worker retirement; see `EffectFenceCommand` for those obligations.
use crate::effect_authority::Source;
pub use amaterasu_contracts::effect_fence::{
    EffectFenceCommand, EffectFencePolicy, EffectScope, EffectSource, GuardedEffect,
    GuardedEffectIdentity, ProtectedMethod, SOURCE_SLOT_COUNT, SelectedEffectMethod,
    SourceSlotLease,
};

/// Preserve the exact pairing source identity in the broker fence contract.
/// A structurally valid source is still not evidence of an authenticated join.
pub fn source(source: &Source) -> Result<EffectSource, &'static str> {
    if !source.valid() {
        return Err("invalid_source");
    }
    Ok(EffectSource {
        version: source.version,
        id: source.id.clone(),
        generation: source.generation,
        host_epoch: source.host_epoch.clone(),
        session_id: source.session_id.clone(),
        stream_id: source.stream_id.clone(),
        stream_generation: source.stream_generation,
        device: source.device.clone(),
        device_generation: source.device_generation,
        effect_owner_epoch: source.effect_owner_epoch.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn copies_only_a_valid_pairing_source_without_changing_the_wire() {
        let value = json!({"version":1,"id":"a".repeat(32),"generation":1,
            "host_epoch":"host","session_id":"session","stream_id":"stream",
            "stream_generation":1,"device":"device","device_generation":1,
            "effect_owner_epoch":"owner"});
        let input: Source = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(source(&input).unwrap()).unwrap(),
            value
        );
        let mut invalid = input;
        invalid.stream_generation = 2;
        assert!(source(&invalid).is_err());
    }
}
