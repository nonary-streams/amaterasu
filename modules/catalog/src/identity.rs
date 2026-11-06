//! Bounded identity-only export for installed presentation providers.
use crate::model::Entry;
use serde_json::{Value, json};

pub(crate) const MAX_PROPERTIES_BYTES: usize = 4_096;

/// Preserve approved presentation metadata and authoritative display overrides,
/// while leaving executable, preparation, log-path and provider-launch data out.
pub(crate) fn properties(entry: &Entry) -> Result<Value, &'static str> {
    let mut properties = entry
        .properties
        .as_object()
        .cloned()
        .ok_or("properties must be an object")?;
    if entry.remote_monitor_profile {
        properties.insert(
            "config_overrides".into(),
            serde_json::to_value(&entry.config_overrides)
                .map_err(|_| "invalid config overrides")?,
        );
    }
    for (key, value) in [
        (
            "display_output",
            entry.display_output.as_ref().map(|value| json!(value)),
        ),
        (
            "virtual_screen",
            entry.virtual_screen.map(|value| json!(value)),
        ),
        (
            "virtual_display_mode",
            entry
                .virtual_display_mode
                .as_ref()
                .map(|value| json!(value)),
        ),
        (
            "virtual_display_layout",
            entry
                .virtual_display_layout
                .as_ref()
                .map(|value| json!(value)),
        ),
    ] {
        if let Some(value) = value.filter(|_| entry.remote_monitor_profile) {
            properties.insert(key.into(), value);
        }
    }
    let result = Value::Object(properties);
    if serde_json::to_vec(&result)
        .map_err(|_| "invalid identity properties")?
        .len()
        > MAX_PROPERTIES_BYTES
    {
        return Err("combined presentation properties exceed 4096 bytes");
    }
    Ok(result)
}

pub(crate) fn project(id: &str, entry: &Entry) -> Result<Value, &'static str> {
    Ok(json!({
        "id":id,
        "uuid":entry.uuid.as_deref().unwrap_or(id),
        "title":entry.name,
        "aliases":entry.id_aliases,
        "visible":!entry.hidden,
        "monitor_profile":entry.remote_monitor_profile,
        "properties":properties(entry)?,
    }))
}
