use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const PRODUCT_SETTINGS_VERSION: u32 = 1;
const MAX_FILTERS: usize = 256;
const MAX_FILTER_BYTES: usize = 512;
const MAX_SETTINGS_BYTES: usize = 12_000;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductSettings {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_auto_sync")]
    pub auto_sync: bool,
    #[serde(default)]
    pub sync_all_installed: bool,
    #[serde(default = "default_autosync_remove_uninstalled")]
    pub autosync_remove_uninstalled: bool,
    #[serde(default = "default_recent_games")]
    pub recent_games: u32,
    #[serde(default = "default_recent_max_age_days")]
    pub recent_max_age_days: u32,
    #[serde(default)]
    pub sync_categories: Vec<String>,
    #[serde(default)]
    pub exclude_categories: Vec<String>,
    #[serde(default)]
    pub sync_plugins: Vec<String>,
    #[serde(default)]
    pub exclude_plugins: Vec<String>,
    #[serde(default)]
    pub exclude_games: Vec<String>,
}

fn default_enabled() -> bool {
    cfg!(windows)
}

fn default_auto_sync() -> bool {
    true
}

fn default_autosync_remove_uninstalled() -> bool {
    true
}

fn default_recent_games() -> u32 {
    10
}

fn default_recent_max_age_days() -> u32 {
    30
}

impl Default for ProductSettings {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            auto_sync: default_auto_sync(),
            sync_all_installed: false,
            autosync_remove_uninstalled: default_autosync_remove_uninstalled(),
            recent_games: default_recent_games(),
            recent_max_age_days: default_recent_max_age_days(),
            sync_categories: Vec::new(),
            exclude_categories: Vec::new(),
            sync_plugins: Vec::new(),
            exclude_plugins: Vec::new(),
            exclude_games: Vec::new(),
        }
    }
}

impl ProductSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.recent_games > 10_000 || self.recent_max_age_days > 36_500 {
            return Err("Playnite recent filters exceed their supported bounds");
        }
        validate_filter_values(&self.sync_categories, "category")?;
        validate_filter_values(&self.exclude_categories, "category")?;
        validate_filter_values(&self.sync_plugins, "plugin ID")?;
        validate_filter_values(&self.exclude_plugins, "plugin ID")?;
        validate_filter_values(&self.exclude_games, "game ID")?;
        if serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > MAX_SETTINGS_BYTES) {
            return Err("Playnite product settings exceed the 12000 byte limit");
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
            .map_err(|error| format!("invalid Playnite product settings: {error}"))?;
            settings
                .validate()
                .map_err(|error| format!("invalid Playnite product settings: {error}"))?;
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
            Err("absent Playnite product settings have an invalid document shape".into())
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

fn validate_filter_values(values: &[String], label: &'static str) -> Result<(), &'static str> {
    if values.len() > MAX_FILTERS {
        return Err("Playnite filter list exceeds 256 entries");
    }
    let mut unique = BTreeSet::new();
    for value in values {
        let normalized = value.to_lowercase();
        if value.trim().is_empty()
            || value.len() > MAX_FILTER_BYTES
            || value.chars().any(char::is_control)
            || !unique.insert(normalized)
        {
            return Err(match label {
                "category" => "category filters must be unique nonempty text of at most 512 bytes",
                "plugin ID" => "plugin filters must be unique nonempty text of at most 512 bytes",
                _ => "game filters must be unique nonempty text of at most 512 bytes",
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_defaults_match_the_playnite_host_contract() {
        let settings = ProductSettings::default();
        assert_eq!(settings.enabled, cfg!(windows));
        assert!(settings.auto_sync);
        assert!(!settings.sync_all_installed);
        assert!(settings.autosync_remove_uninstalled);
        assert_eq!(settings.recent_games, 10);
        assert_eq!(settings.recent_max_age_days, 30);
    }

    #[test]
    fn product_document_roundtrips_and_rejects_malformed_shapes() {
        let settings = ProductSettings {
            enabled: true,
            sync_categories: vec!["Arcade".into()],
            ..ProductSettings::default()
        };
        let payload = json!({
            "ok":true,"found":true,"version":1,"revision":7,
            "values":serde_json::to_value(&settings).unwrap()
        });
        assert_eq!(
            ProductSettings::from_product_document(&payload),
            Ok((settings, 7))
        );
        let mut legacy_values = serde_json::to_value(ProductSettings::default()).unwrap();
        legacy_values
            .as_object_mut()
            .unwrap()
            .remove("autosync_remove_uninstalled");
        assert!(
            ProductSettings::from_product_document(&json!({
                "ok":true,"found":true,"version":1,"revision":6,
                "values":legacy_values
            }))
            .unwrap()
            .0
            .autosync_remove_uninstalled
        );
        assert_eq!(
            ProductSettings::from_product_document(&json!({
                "ok":true,"found":false,"version":null,"revision":0,"values":{}
            }))
            .unwrap(),
            (ProductSettings::default(), 0)
        );
        for malformed in [
            json!({"ok":true,"found":true,"version":2,"revision":1,"values":{}}),
            json!({"ok":true,"found":true,"version":1,"revision":0,"values":{}}),
            json!({"ok":true,"found":false,"version":null,"revision":0,"values":{"enabled":true}}),
            json!({"ok":true,"found":false,"version":null,"revision":0,"values":[]}),
            json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"enabled":true,"unknown":false}}),
        ] {
            assert!(ProductSettings::from_product_document(&malformed).is_err());
        }
    }

    #[test]
    fn product_update_uses_complete_versioned_values() {
        let settings = ProductSettings::default();
        assert_eq!(
            settings.product_update_payload(3),
            json!({"action":"product_update","expected_revision":3,"version":1,"values":settings})
        );
    }

    #[test]
    fn filters_are_bounded_unique_and_case_insensitive() {
        for sync_categories in [
            vec!["".into()],
            vec!["Arcade".into(), "arcade".into()],
            vec!["x".repeat(MAX_FILTER_BYTES + 1)],
            vec!["invalid\ncategory".into()],
        ] {
            assert!(
                ProductSettings {
                    sync_categories,
                    ..ProductSettings::default()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            ProductSettings {
                recent_games: 10_001,
                ..ProductSettings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ProductSettings {
                exclude_games: vec!["game-id".repeat(512); 32],
                ..ProductSettings::default()
            }
            .validate()
            .is_err()
        );
    }
}
