//! Declarative schema replacement: preserve values unless the request explicitly
//! renames, removes or replaces them. No executable migration hooks are involved.
use crate::protocol::{Document, error};
use amaterasu_configuration_schema::{Fields, Values, defaults, validate_values};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migration {
    pub expected_revision: u64,
    pub fields: Fields,
    #[serde(default)]
    pub renames: BTreeMap<String, String>,
    #[serde(default)]
    pub removed_fields: Vec<String>,
    #[serde(default)]
    pub values: Values,
}

pub fn migrate(request: Migration, existing: Option<Document>) -> Result<Document, Value> {
    let document =
        existing.ok_or_else(|| error("not_registered", "caller has no configuration schema"))?;
    if request.expected_revision != document.revision {
        return Err(json!({
            "ok": false,
            "error": {
                "code": "revision_conflict", "message": "configuration revision differs",
                "current_revision": document.revision,
            },
        }));
    }
    let mut values =
        defaults(&request.fields).map_err(|message| error("invalid_schema", message))?;
    let removed = validate_mapping(&request, &document.fields)?;
    let mut retained = preserve_values(&request, document.values, &removed)?;
    // Explicit replacements allow a caller to change a field's type or bounds
    // without requiring an implicit conversion or silently choosing a default.
    retained.extend(request.values);
    values.extend(retained);
    validate_values(&request.fields, &values)
        .map_err(|message| error("invalid_values", message))?;
    let revision = document
        .revision
        .checked_add(1)
        .ok_or_else(|| error("resource_exhausted", "revision exhausted"))?;
    Ok(Document {
        revision,
        fields: request.fields,
        values,
    })
}

fn validate_mapping(request: &Migration, existing: &Fields) -> Result<BTreeSet<String>, Value> {
    let mut removed = BTreeSet::new();
    for name in &request.removed_fields {
        if !existing.contains_key(name) || !removed.insert(name.clone()) {
            return Err(error(
                "invalid_migration",
                format!("unknown or duplicate removed field: {name}"),
            ));
        }
    }
    for (source, destination) in &request.renames {
        if !existing.contains_key(source) || !request.fields.contains_key(destination) {
            return Err(error(
                "invalid_migration",
                "renames must map existing fields to replacement fields",
            ));
        }
        if removed.contains(source) {
            return Err(error(
                "invalid_migration",
                "a field cannot be both renamed and removed",
            ));
        }
    }
    Ok(removed)
}

fn preserve_values(
    request: &Migration,
    existing: Values,
    removed: &BTreeSet<String>,
) -> Result<Values, Value> {
    let mut values = Values::new();
    for (source, value) in existing {
        if removed.contains(&source) {
            continue;
        }
        let destination = request.renames.get(&source).unwrap_or(&source);
        if !request.fields.contains_key(destination) {
            return Err(error(
                "invalid_migration",
                format!("field requires an explicit rename or removal: {source}"),
            ));
        }
        // Resolve all renames against the original document simultaneously.
        // This supports swaps without allowing two old values to overwrite one.
        if values.insert(destination.clone(), value).is_some() {
            return Err(error(
                "invalid_migration",
                format!("multiple fields map to: {destination}"),
            ));
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests;
