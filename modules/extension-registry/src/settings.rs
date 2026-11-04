//! Pure, bounded settings operations. Persistence and caller authorization live at
//! the service boundary; this module never interprets a supplied scope as a grant.
use amaterasu_configuration_schema::{Fields, Values, defaults, validate_values};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Scope {
    Host,
    Application(String),
    PairedClient(String),
    Session(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub scope: Scope,
    pub values: Values,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub revision: u64,
    pub schema_revision: u64,
    pub layers: Vec<Layer>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectiveValue {
    pub value: Value,
    /// None denotes the descriptor's module default.
    pub source: Option<Scope>,
}

fn scope_kind(scope: &Scope) -> &'static str {
    match scope {
        Scope::Host => "host",
        Scope::Application(_) => "application",
        Scope::PairedClient(_) => "paired_client",
        Scope::Session(_) => "session",
    }
}

fn scope_valid(scope: &Scope) -> bool {
    match scope {
        Scope::Host => true,
        Scope::Application(id) | Scope::PairedClient(id) | Scope::Session(id) => {
            !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
        }
    }
}

pub fn validate_document(
    fields: &Fields,
    allowed: &BTreeMap<String, BTreeSet<String>>,
    document: &Document,
) -> Result<(), String> {
    defaults(fields)?;
    if document.layers.len() > 256 {
        return Err("too many settings layers".into());
    }
    let mut scopes = BTreeSet::new();
    for layer in &document.layers {
        if !scope_valid(&layer.scope) || !scopes.insert(&layer.scope) {
            return Err("invalid or duplicate settings scope".into());
        }
        validate_values(fields, &layer.values)?;
        for name in layer.values.keys() {
            if !allowed
                .get(name)
                .is_some_and(|scopes| scopes.contains(scope_kind(&layer.scope)))
            {
                return Err(format!("scope is not permitted for {name}"));
            }
        }
    }
    Ok(())
}

pub fn update(
    fields: &Fields,
    allowed: &BTreeMap<String, BTreeSet<String>>,
    current: &Document,
    expected_revision: u64,
    scope: Scope,
    values: Values,
) -> Result<Document, String> {
    if current.revision != expected_revision {
        return Err("revision_conflict".into());
    }
    validate_document(fields, allowed, current)?;
    if !scope_valid(&scope) {
        return Err("invalid scope".into());
    }
    let mut next = current.clone();
    next.layers.retain(|layer| layer.scope != scope);
    // An empty document removes this override; false, zero and empty strings are
    // explicit values and remain present.
    if !values.is_empty() {
        next.layers.push(Layer { scope, values });
    }
    next.revision = next.revision.checked_add(1).ok_or("revision_exhausted")?;
    validate_document(fields, allowed, &next)?;
    Ok(next)
}

pub fn resolve(
    fields: &Fields,
    allowed: &BTreeMap<String, BTreeSet<String>>,
    document: &Document,
    application: Option<&str>,
    paired_client: Option<&str>,
    session: Option<&str>,
) -> Result<BTreeMap<String, EffectiveValue>, String> {
    validate_document(fields, allowed, document)?;
    let mut result: BTreeMap<_, _> = defaults(fields)?
        .into_iter()
        .map(|(name, value)| {
            (
                name,
                EffectiveValue {
                    value,
                    source: None,
                },
            )
        })
        .collect();
    let mut layers = document
        .layers
        .iter()
        .filter(|layer| match &layer.scope {
            Scope::Host => true,
            Scope::Application(id) => application == Some(id.as_str()),
            Scope::PairedClient(id) => paired_client == Some(id.as_str()),
            Scope::Session(id) => session == Some(id.as_str()),
        })
        .collect::<Vec<_>>();
    layers.sort_by_key(|layer| &layer.scope);
    for layer in layers {
        for (name, value) in &layer.values {
            result.insert(
                name.clone(),
                EffectiveValue {
                    value: value.clone(),
                    source: Some(layer.scope.clone()),
                },
            );
        }
    }
    Ok(result)
}

/// A conservative response bound, covering any authorized selection of layers.
/// Each value and provenance uses the longest candidate independently, so the
/// constructed response is at least as large as every actual response.
pub fn responses_fit(
    fields: &Fields,
    live_options: &[String],
    document: &Document,
    limit: usize,
) -> bool {
    let Ok(defaults) = defaults(fields) else {
        return false;
    };
    let largest_scope = document
        .layers
        .iter()
        .map(|layer| serde_json::to_value(&layer.scope).unwrap_or(Value::Null))
        .max_by_key(|scope| serde_json::to_vec(scope).map_or(usize::MAX, |v| v.len()))
        .unwrap_or(Value::Null);
    let mut effective = BTreeMap::new();
    for (name, default) in defaults {
        let mut value = default;
        let mut size = serde_json::to_vec(&value).map_or(usize::MAX, |v| v.len());
        for layer in &document.layers {
            if let Some(candidate) = layer.values.get(&name) {
                let candidate_size = serde_json::to_vec(candidate).map_or(usize::MAX, |v| v.len());
                if candidate_size > size {
                    size = candidate_size;
                    value = candidate.clone();
                }
            }
        }
        effective.insert(
            name,
            serde_json::json!({"value":value,"source":largest_scope}),
        );
    }
    let values = document
        .layers
        .iter()
        .map(|layer| &layer.values)
        .max_by_key(|values| serde_json::to_vec(values).map_or(usize::MAX, |v| v.len()));
    let response = serde_json::json!({"ok":true,"status":"ok","revision":u64::MAX,"descriptor_revision":u64::MAX,"values":values.cloned().unwrap_or_default(),"effective":effective,"live_options":live_options});
    serde_json::to_vec(&response).is_ok_and(|bytes| bytes.len() <= limit)
}

/// Enumerate every effective combination represented by persisted layers. A
/// caller must run provider cross-field validation on each before committing.
/// Refuse growth beyond a fixed budget before making any validation calls.
pub fn validation_snapshots(
    fields: &Fields,
    allowed: &BTreeMap<String, BTreeSet<String>>,
    document: &Document,
    limit: usize,
) -> Result<Vec<Values>, String> {
    validate_document(fields, allowed, document)?;
    let mut apps = BTreeSet::new();
    let mut clients = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    for layer in &document.layers {
        match &layer.scope {
            Scope::Host => (),
            Scope::Application(id) => {
                apps.insert(id.as_str());
            }
            Scope::PairedClient(id) => {
                clients.insert(id.as_str());
            }
            Scope::Session(id) => {
                sessions.insert(id.as_str());
            }
        }
    }
    let count = (apps.len() + 1)
        .checked_mul(clients.len() + 1)
        .and_then(|count| count.checked_mul(sessions.len() + 1))
        .ok_or("validation budget exhausted")?;
    if count > limit {
        return Err("validation budget exhausted".into());
    }
    let apps = std::iter::once(None)
        .chain(apps.into_iter().map(Some))
        .collect::<Vec<_>>();
    let clients = std::iter::once(None)
        .chain(clients.into_iter().map(Some))
        .collect::<Vec<_>>();
    let sessions = std::iter::once(None)
        .chain(sessions.into_iter().map(Some))
        .collect::<Vec<_>>();
    let mut snapshots = Vec::with_capacity(count);
    for app in apps {
        for client in &clients {
            for session in &sessions {
                let values = resolve(fields, allowed, document, app, *client, *session)?
                    .into_iter()
                    .map(|(key, value)| (key, value.value))
                    .collect();
                snapshots.push(values);
            }
        }
    }
    Ok(snapshots)
}

/// Migrate all explicit overrides together; omitted old names require an
/// explicit removal. Renames are simultaneous and collisions are rejected.
pub struct Migration<'a> {
    pub old_fields: &'a Fields,
    pub new_fields: &'a Fields,
    pub old_allowed: &'a BTreeMap<String, BTreeSet<String>>,
    pub new_allowed: &'a BTreeMap<String, BTreeSet<String>>,
    pub expected_revision: u64,
    pub new_schema_revision: u64,
    pub renames: &'a BTreeMap<String, String>,
    pub removed: &'a BTreeSet<String>,
    pub replacements: &'a BTreeMap<Scope, Values>,
}

pub fn migrate(current: &Document, migration: Migration<'_>) -> Result<Document, String> {
    let Migration {
        old_fields,
        new_fields,
        old_allowed,
        new_allowed,
        expected_revision,
        new_schema_revision,
        renames,
        removed,
        replacements,
    } = migration;
    if current.revision != expected_revision {
        return Err("revision_conflict".into());
    }
    if new_schema_revision <= current.schema_revision {
        return Err("invalid_schema_revision".into());
    }
    validate_document(old_fields, old_allowed, current)?;
    for source in old_fields.keys() {
        let target = renames.get(source).unwrap_or(source);
        if removed.contains(source) {
            if renames.contains_key(source) {
                return Err("removed field is renamed".into());
            }
        } else if !new_fields.contains_key(target) {
            return Err(format!(
                "field requires explicit removal or rename: {source}"
            ));
        }
    }
    if renames
        .keys()
        .chain(removed.iter())
        .any(|name| !old_fields.contains_key(name))
    {
        return Err("unknown migration field".into());
    }
    let mut destinations = BTreeSet::new();
    for source in old_fields
        .keys()
        .filter(|source| !removed.contains(*source))
    {
        if !destinations.insert(renames.get(source).unwrap_or(source)) {
            return Err("migration collision".into());
        }
    }
    let mut next = current.clone();
    for layer in &mut next.layers {
        let mut values = Values::new();
        for (source, value) in std::mem::take(&mut layer.values) {
            if removed.contains(&source) {
                continue;
            }
            let target = renames.get(&source).unwrap_or(&source).clone();
            if values.insert(target, value).is_some() {
                return Err("migration collision".into());
            }
        }
        if let Some(replacement) = replacements.get(&layer.scope) {
            values.extend(replacement.clone());
        }
        layer.values = values;
    }
    for (scope, values) in replacements {
        if !next.layers.iter().any(|layer| &layer.scope == scope) {
            next.layers.push(Layer {
                scope: scope.clone(),
                values: values.clone(),
            });
        }
    }
    next.layers.retain(|layer| !layer.values.is_empty());
    next.schema_revision = new_schema_revision;
    next.revision = next.revision.checked_add(1).ok_or("revision_exhausted")?;
    validate_document(new_fields, new_allowed, &next)?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (Fields, BTreeMap<String, BTreeSet<String>>, Document) {
        let fields = serde_json::from_value(json!({
            "enabled": {"type":"boolean", "default":true},
            "count": {"type":"integer", "default":1, "minimum":0, "maximum":5}
        }))
        .unwrap();
        let allowed = ["enabled", "count"]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    ["host", "application", "paired_client", "session"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                )
            })
            .collect();
        (
            fields,
            allowed,
            Document {
                revision: 0,
                schema_revision: 1,
                layers: Vec::new(),
            },
        )
    }

    #[test]
    fn overlays_preserve_explicit_false_and_zero_and_report_provenance() {
        let (fields, allowed, current) = fixture();
        let host = update(
            &fields,
            &allowed,
            &current,
            0,
            Scope::Host,
            serde_json::from_value(json!({"enabled":false,"count":0})).unwrap(),
        )
        .unwrap();
        let app = update(
            &fields,
            &allowed,
            &host,
            1,
            Scope::Application("app".into()),
            serde_json::from_value(json!({"enabled":true})).unwrap(),
        )
        .unwrap();
        let resolved = resolve(&fields, &allowed, &app, Some("app"), None, None).unwrap();
        assert_eq!(resolved["enabled"].value, json!(true));
        assert_eq!(
            resolved["enabled"].source,
            Some(Scope::Application("app".into()))
        );
        assert_eq!(resolved["count"].value, json!(0));
        assert_eq!(resolved["count"].source, Some(Scope::Host));
        assert!(update(&fields, &allowed, &app, 1, Scope::Host, Values::new()).is_err());
        let cleared = update(
            &fields,
            &allowed,
            &app,
            2,
            Scope::Application("app".into()),
            Values::new(),
        )
        .unwrap();
        assert_eq!(
            resolve(&fields, &allowed, &cleared, Some("app"), None, None).unwrap()["enabled"].value,
            json!(false)
        );
    }

    #[test]
    fn response_budget_rejects_values_that_would_duplicate_past_limit() {
        let fields: Fields = serde_json::from_value(json!({
            "first":{"type":"string","default":"","max_length":4096},
            "second":{"type":"string","default":"","max_length":4096}
        }))
        .unwrap();
        let allowed = ["first", "second"]
            .into_iter()
            .map(|name| (name.into(), ["host".into()].into_iter().collect()))
            .collect();
        let current = Document {
            revision: 0,
            schema_revision: 1,
            layers: vec![],
        };
        let updated = update(
            &fields,
            &allowed,
            &current,
            0,
            Scope::Host,
            serde_json::from_value(json!({"first":"x".repeat(4096),"second":"x".repeat(4096)}))
                .unwrap(),
        )
        .unwrap();
        assert!(serde_json::to_vec(&updated).unwrap().len() < 12_000);
        assert!(!responses_fit(&fields, &[], &updated, 12_000));
    }

    #[test]
    fn response_bound_includes_live_option_metadata() {
        let (fields, _, document) = fixture();
        assert!(responses_fit(&fields, &[], &document, 1_000));
        assert!(!responses_fit(
            &fields,
            &["x".repeat(1_000)],
            &document,
            1_000
        ));
    }

    #[test]
    fn validation_enumerates_combinations_and_rejects_budget_overflow() {
        let (fields, allowed, current) = fixture();
        let current = update(
            &fields,
            &allowed,
            &current,
            0,
            Scope::Application("a".into()),
            serde_json::from_value(json!({"enabled":false})).unwrap(),
        )
        .unwrap();
        let current = update(
            &fields,
            &allowed,
            &current,
            1,
            Scope::PairedClient("c".into()),
            serde_json::from_value(json!({"count":0})).unwrap(),
        )
        .unwrap();
        let snapshots = validation_snapshots(&fields, &allowed, &current, 4).unwrap();
        assert_eq!(snapshots.len(), 4);
        assert!(
            snapshots
                .iter()
                .any(|values| values["enabled"] == json!(false) && values["count"] == json!(0))
        );
        assert!(validation_snapshots(&fields, &allowed, &current, 3).is_err());
    }

    #[test]
    fn migrations_require_explicit_removal_and_preserve_overrides() {
        let (fields, allowed, current) = fixture();
        let current = update(
            &fields,
            &allowed,
            &current,
            0,
            Scope::Host,
            serde_json::from_value(json!({"enabled":false})).unwrap(),
        )
        .unwrap();
        let mut new_fields = fields.clone();
        let enabled = new_fields.remove("enabled").unwrap();
        new_fields.insert("active".into(), enabled);
        let mut new_allowed = allowed.clone();
        let renamed_allowed = new_allowed.remove("enabled").unwrap();
        new_allowed.insert("active".into(), renamed_allowed);
        let renames = [("enabled".into(), "active".into())].into_iter().collect();
        let removed = BTreeSet::new();
        let replacements = BTreeMap::new();
        let empty_renames = BTreeMap::new();
        let request = |renames| Migration {
            old_fields: &fields,
            new_fields: &new_fields,
            old_allowed: &allowed,
            new_allowed: &new_allowed,
            expected_revision: 1,
            new_schema_revision: 2,
            renames,
            removed: &removed,
            replacements: &replacements,
        };
        assert!(migrate(&current, request(&empty_renames)).is_err());
        let migrated = migrate(&current, request(&renames)).unwrap();
        assert_eq!(
            resolve(&new_fields, &new_allowed, &migrated, None, None, None).unwrap()["active"]
                .value,
            json!(false)
        );
    }
}
