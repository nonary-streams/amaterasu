//! Durable Lutris settings owned by the Lutris provider.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const PRODUCT_SETTINGS_VERSION: u32 = 1;
const MAX_EXCLUSIONS: usize = 256;
const MAX_EXCLUSION_BYTES: usize = 512;
const MAX_SETTINGS_BYTES: usize = 12_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Exclusion {
    Id(String),
    Metadata(ExclusionMetadata),
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExclusionMetadata {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

impl Exclusion {
    fn validate(&self) -> Result<String, &'static str> {
        match self {
            Self::Id(id) => {
                validate_id(id)?;
                Ok(format!("id:{}", id))
            }
            Self::Metadata(value) => {
                if !value.id.is_empty() {
                    validate_id(&value.id)?;
                    if !value.name.is_empty() {
                        validate_name(&value.name)?;
                    }
                    Ok(format!("id:{}", value.id))
                } else {
                    validate_name(&value.name)?;
                    Ok(format!("name:{}", value.name.to_lowercase()))
                }
            }
        }
    }

    pub fn matches(&self, id: i64, name: &str) -> bool {
        match self {
            Self::Id(excluded) => excluded == &id.to_string(),
            Self::Metadata(value) if !value.id.is_empty() => value.id == id.to_string(),
            Self::Metadata(value) => value.name.eq_ignore_ascii_case(name),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductSettings {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_auto_sync")]
    pub auto_sync: bool,
    #[serde(default = "default_remove_uninstalled")]
    pub autosync_remove_uninstalled: bool,
    #[serde(default)]
    pub include_steam: bool,
    #[serde(default)]
    pub exclude_games: Vec<Exclusion>,
}

fn default_enabled() -> bool {
    cfg!(target_os = "linux")
}

fn default_auto_sync() -> bool {
    true
}

fn default_remove_uninstalled() -> bool {
    true
}

impl Default for ProductSettings {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            auto_sync: default_auto_sync(),
            autosync_remove_uninstalled: default_remove_uninstalled(),
            include_steam: false,
            exclude_games: Vec::new(),
        }
    }
}

impl ProductSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.enabled && !cfg!(target_os = "linux") {
            return Err("Lutris is supported only on Linux");
        }
        if self.exclude_games.len() > MAX_EXCLUSIONS {
            return Err("Lutris exclusion list exceeds 256 entries");
        }
        let mut unique = BTreeSet::new();
        for exclusion in &self.exclude_games {
            if !unique.insert(exclusion.validate()?) {
                return Err("Lutris exclusions must be unique");
            }
        }
        if serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > MAX_SETTINGS_BYTES) {
            return Err("Lutris product settings exceed the 12000 byte limit");
        }
        Ok(())
    }

    pub fn from_product_document(payload: &Value) -> Result<(Self, u64), String> {
        if payload.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err("product settings service returned an error".into());
        }
        let object = payload
            .as_object()
            .ok_or_else(|| "product settings response is not an object".to_owned())?;
        let fields = ["ok", "found", "version", "revision", "values"];
        if object.len() != fields.len() || object.keys().any(|key| !fields.contains(&key.as_str()))
        {
            return Err("product settings response has unexpected fields".into());
        }
        let found = payload
            .get("found")
            .and_then(Value::as_bool)
            .ok_or_else(|| "product settings response omitted found".to_owned())?;
        let revision = payload
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or_else(|| "product settings response has an invalid revision".to_owned())?;
        if found {
            if payload.get("version").and_then(Value::as_u64)
                != Some(u64::from(PRODUCT_SETTINGS_VERSION))
                || revision == 0
            {
                return Err("product settings version or revision is invalid".into());
            }
            let settings: Self = serde_json::from_value(
                payload
                    .get("values")
                    .cloned()
                    .ok_or_else(|| "product settings response omitted values".to_owned())?,
            )
            .map_err(|error| format!("invalid Lutris product settings: {error}"))?;
            settings
                .validate()
                .map_err(|error| format!("invalid Lutris product settings: {error}"))?;
            Ok((settings, revision))
        } else if payload.get("version") == Some(&Value::Null)
            && revision == 0
            && payload
                .get("values")
                .and_then(Value::as_object)
                .is_some_and(|values| values.is_empty())
        {
            Ok((Self::default(), 0))
        } else {
            Err("absent Lutris product settings have an invalid document shape".into())
        }
    }

    pub fn product_update_payload(&self, expected_revision: u64) -> Value {
        json!({
            "action":"product_update",
            "expected_revision":expected_revision,
            "version":PRODUCT_SETTINGS_VERSION,
            "values":self,
        })
    }

    pub fn management_response(&self, revision: u64) -> Value {
        json!({"ok":true,"revision":revision,"values":self})
    }
}

fn validate_id(value: &str) -> Result<(), &'static str> {
    if value.is_empty()
        || value.len() > MAX_EXCLUSION_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || value.parse::<u64>().map_or(true, |id| id == 0)
    {
        return Err("Lutris exclusion IDs must be positive decimal integers");
    }
    Ok(())
}

fn validate_name(value: &str) -> Result<(), &'static str> {
    if value.trim().is_empty()
        || value.len() > MAX_EXCLUSION_BYTES
        || value.chars().any(char::is_control)
    {
        return Err("Lutris exclusion names must be printable text of at most 512 bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_pinned_linux_contract() {
        let settings = ProductSettings::default();
        assert_eq!(settings.enabled, cfg!(target_os = "linux"));
        assert!(settings.auto_sync);
        assert!(settings.autosync_remove_uninstalled);
        assert!(!settings.include_steam);
        assert!(settings.exclude_games.is_empty());
    }

    #[test]
    fn exclusion_forms_preserve_id_precedence_and_name_matching() {
        let by_id = Exclusion::Id("42".into());
        assert!(by_id.matches(42, "Any Name"));
        assert!(!by_id.matches(7, "42"));
        let by_metadata = Exclusion::Metadata(ExclusionMetadata {
            id: "42".into(),
            name: "Old Name".into(),
        });
        assert!(by_metadata.matches(42, "New Name"));
        assert!(!by_metadata.matches(7, "Old Name"));
        let by_name = Exclusion::Metadata(ExclusionMetadata {
            id: String::new(),
            name: "Example Game".into(),
        });
        assert!(by_name.matches(7, "example game"));
    }

    #[test]
    fn product_documents_are_strict_and_absent_values_default_safely() {
        let settings = ProductSettings::default();
        let response = json!({
            "ok":true,"found":true,"version":1,"revision":3,
            "values":settings
        });
        assert_eq!(
            ProductSettings::from_product_document(&response),
            Ok((settings, 3))
        );
        assert_eq!(
            ProductSettings::from_product_document(&json!({
                "ok":true,"found":false,"version":null,"revision":0,"values":{}
            }))
            .unwrap(),
            (ProductSettings::default(), 0)
        );
        for payload in [
            json!({"ok":true,"found":false,"version":null,"revision":0,"values":{"enabled":true}}),
            json!({"ok":true,"found":true,"version":2,"revision":1,"values":{}}),
            json!({"ok":true,"found":true,"version":1,"revision":0,"values":{}}),
            json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"unknown":true}}),
        ] {
            assert!(ProductSettings::from_product_document(&payload).is_err());
        }
    }

    #[test]
    fn settings_reject_invalid_and_duplicate_filters() {
        for exclusions in [
            vec![Exclusion::Id("0".into())],
            vec![Exclusion::Id("abc".into())],
            vec![Exclusion::Metadata(ExclusionMetadata::default())],
            vec![Exclusion::Metadata(ExclusionMetadata {
                name: "bad\nname".into(),
                ..ExclusionMetadata::default()
            })],
            vec![Exclusion::Id("7".into()), Exclusion::Id("7".into())],
        ] {
            assert!(
                ProductSettings {
                    exclude_games: exclusions,
                    ..ProductSettings::default()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            ProductSettings {
                exclude_games: vec![Exclusion::Id("7".into()); MAX_EXCLUSIONS + 1],
                ..ProductSettings::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn update_document_uses_the_domain_product_version() {
        let settings = ProductSettings::default();
        assert_eq!(
            settings.product_update_payload(8),
            json!({
                "action":"product_update",
                "expected_revision":8,
                "version":PRODUCT_SETTINGS_VERSION,
                "values":settings,
            })
        );
    }
}
