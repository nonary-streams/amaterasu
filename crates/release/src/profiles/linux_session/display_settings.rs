//! Product defaults for the built-in display-settings implementation.
use super::*;

pub(super) fn install_default_provider(
    policy: &mut RuntimeConfig,
    selection: &ProcessingSelection,
) -> PackageResult<()> {
    let owner = selection
        .stream_owner
        .as_ref()
        .ok_or("display settings require a stream owner")?;
    let grants = DISPLAY_SETTINGS.grants();
    if let Some(existing) = policy.principals.get(DISPLAY_SETTINGS.id)
        && !contains_grants(existing, &grants)
    {
        return Err("display settings provider conflicts with approved composition".into());
    }
    policy
        .principals
        .entry(DISPLAY_SETTINGS.id.into())
        .or_insert(grants);
    let settings = policy
        .settings
        .entry(DISPLAY_SETTINGS.id.into())
        .or_insert_with(|| json!({"enabled":true,"registrars":[],"consumers":[]}))
        .as_object_mut()
        .ok_or("invalid display settings provider configuration")?;
    if settings.get("enabled").and_then(Value::as_bool) != Some(true) {
        return Err("display settings selection conflicts".into());
    }
    for (key, principal) in [
        ("registrars", &owner.principal),
        ("consumers", &selection.processor.principal),
    ] {
        let principals = settings
            .entry(key)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("invalid display settings provider configuration")?;
        if !principals.contains(&json!(principal)) {
            principals.push(json!(principal));
        }
    }
    Ok(())
}
