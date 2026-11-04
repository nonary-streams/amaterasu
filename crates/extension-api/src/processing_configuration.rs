//! Configuration returned by the selected version-one processing-intent API.
use serde_json::Value;

pub const MAX_PROCESSING_CONFIGURATION_BYTES: usize = 16 * 1024;

/// A processing provider may bind additional context to its configuration, but
/// must preserve every field of the authoritative frozen settings snapshot.
/// Consumers independently validate the intent's target, revision, service and
/// capture/output range. Context is opaque provider configuration, not authority.
pub fn validate_processing_configuration(
    frozen: &Value,
    configuration: &Value,
) -> Result<(), String> {
    let frozen = frozen
        .as_object()
        .ok_or("invalid_frozen_processing_settings")?;
    let configured = configuration
        .as_object()
        .ok_or("invalid_processing_configuration")?;
    if !serde_json::to_vec(configuration)
        .is_ok_and(|bytes| bytes.len() <= MAX_PROCESSING_CONFIGURATION_BYTES)
    {
        return Err("processing_configuration_too_large".into());
    }
    if frozen
        .iter()
        .any(|(name, value)| configured.get(name) != Some(value))
    {
        return Err("frozen_processing_settings_changed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_context_preserves_frozen_values_without_provider_specific_fields() {
        let frozen = json!({"enabled":false,"parameters":{"amount":42},"optional":null});
        assert!(validate_processing_configuration(&frozen, &frozen).is_ok());
        for context in [
            json!({"foreground_target":{"kind":"stream","stream_id":"a"}}),
            json!({"replacement_context":{"selection":"b","generation":2}}),
        ] {
            let mut configuration = frozen.clone();
            configuration
                .as_object_mut()
                .unwrap()
                .extend(context.as_object().unwrap().clone());
            assert!(validate_processing_configuration(&frozen, &configuration).is_ok());
        }
        for configuration in [
            json!({"enabled":true,"parameters":{"amount":42},"optional":null}),
            json!({"enabled":false,"parameters":{"amount":43},"optional":null}),
            json!({"enabled":false,"parameters":{"amount":42}}),
            json!({"enabled":false,"parameters":{"amount":42,"extra":1},"optional":null}),
            Value::Null,
            json!([]),
        ] {
            assert!(validate_processing_configuration(&frozen, &configuration).is_err());
        }
        assert!(validate_processing_configuration(&Value::Null, &json!({})).is_err());
        assert!(
            validate_processing_configuration(
                &json!({}),
                &json!({"context":"x".repeat(MAX_PROCESSING_CONFIGURATION_BYTES)})
            )
            .is_err()
        );
    }
}
