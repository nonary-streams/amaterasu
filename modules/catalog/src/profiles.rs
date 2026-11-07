//! Narrow profile mutations in the existing exported application catalog.
use crate::model::{Document, Entry, Launch};
use crate::protocol::{Change, Operation, apply, error};
use serde_json::{Value, json};

fn valid_uuid(uuid: &str) -> bool {
    let bytes = uuid.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
        && bytes
            .iter()
            .any(|byte| matches!(byte,b'1'..=b'9'|b'a'..=b'f'|b'A'..=b'F'))
}

fn validate_desired(entry: &Entry) -> Result<(), Value> {
    entry
        .validate()
        .map_err(|message| error("invalid_profile", message))?;
    let inert = matches!(&entry.launch,Launch::Desktop {working_directory:None,preparation,..} if preparation.is_empty());
    if !entry.remote_monitor_profile
        || !inert
        || !entry.prep_cmd.is_empty()
        || entry.elevated
        || entry.exclude_global_prep_cmd
        || entry.output.is_some()
        || entry.artwork.is_some()
        || entry.steam_id.is_some()
        || entry.steam_install_dir.is_some()
        || !entry.id_aliases.is_empty()
        || entry.hidden
        || !entry.uuid.as_deref().is_some_and(valid_uuid)
    {
        return Err(error(
            "invalid_profile",
            "profile mutation requires a stable UUID and an inert Desktop target",
        ));
    }
    Ok(())
}

pub(crate) fn upsert(
    document: Document,
    id: String,
    mut desired: Entry,
    expected_revision: u64,
) -> Result<Change, Value> {
    validate_desired(&desired)?;
    if id.is_empty() {
        return apply(
            Operation::Create {
                entry: desired,
                expected_revision,
            },
            Some(document),
        );
    }
    let existing = document
        .entries
        .get(&id)
        .ok_or_else(|| error("not_found", "profile does not exist"))?;
    if !existing.remote_monitor_profile || existing.uuid != desired.uuid {
        return Err(error(
            "profile_identity_mismatch",
            "profile identity changed or selected application is not a profile",
        ));
    }
    // A profile editor owns its advertised mode fields, while the catalog
    // retains other approved overrides and its immutable identity history.
    let mut overrides = serde_json::to_value(&existing.config_overrides)
        .map_err(|_| error("invalid_profile", "profile overrides are malformed"))?;
    overrides
        .as_object_mut()
        .ok_or_else(|| error("invalid_profile", "profile overrides are malformed"))?
        .extend(
            serde_json::to_value(&desired.config_overrides)
                .map_err(|_| error("invalid_profile", "profile overrides are malformed"))?
                .as_object()
                .ok_or_else(|| error("invalid_profile", "profile overrides are malformed"))?
                .clone(),
        );
    desired.config_overrides = serde_json::from_value(overrides)
        .map_err(|_| error("invalid_profile", "profile overrides are malformed"))?;
    let mut properties = existing
        .properties
        .as_object()
        .cloned()
        .ok_or_else(|| error("invalid_profile", "profile properties are malformed"))?;
    properties.extend(
        desired
            .properties
            .as_object()
            .expect("validated profile properties")
            .clone(),
    );
    desired.properties = Value::Object(properties);
    desired.id_aliases = existing.id_aliases.clone();
    desired.hidden = existing.hidden;
    desired.description = existing.description.clone();
    desired.tags = existing.tags.clone();
    desired.display_output = existing.display_output.clone();
    desired.virtual_screen = existing.virtual_screen;
    desired.virtual_display_mode = existing.virtual_display_mode.clone();
    desired.virtual_display_layout = existing.virtual_display_layout.clone();
    desired
        .validate()
        .map_err(|message| error("invalid_profile", message))?;
    apply(
        Operation::Update {
            id,
            entry: desired,
            expected_revision,
        },
        Some(document),
    )
}

pub(crate) fn delete(
    document: Document,
    id: String,
    expected_revision: u64,
) -> Result<Change, Value> {
    if document
        .entries
        .get(&id)
        .is_none_or(|entry| !entry.remote_monitor_profile)
    {
        return Err(error("not_found", "profile does not exist"));
    }
    apply(
        Operation::Delete {
            id,
            expected_revision,
        },
        Some(document),
    )
}

pub(crate) fn project_reply(mut payload: Value) -> Value {
    if payload["ok"] == true
        && let (Some(id), Some(entry)) = (payload["id"].as_str(), payload.get("entry"))
        && let Ok(entry) = serde_json::from_value::<Entry>(entry.clone())
    {
        return match crate::identity::project(id, &entry) {
            Ok(profile) => json!({"ok":true,"revision":payload["revision"],"profile":profile}),
            Err(message) => error("invalid_profile", message),
        };
    }
    if let Some(object) = payload.as_object_mut() {
        object.remove("entry");
    }
    payload
}
