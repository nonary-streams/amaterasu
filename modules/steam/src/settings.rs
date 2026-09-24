use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const PRODUCT_SETTINGS_VERSION: u32 = 1;

const MAX_EXCLUSIONS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Exclusion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Exclusion {
    fn validate(&self) -> Result<(), &'static str> {
        if self.id.is_none() && self.name.is_none() {
            return Err("an exclusion requires an app ID or name");
        }
        if let Some(id) = &self.id {
            let parsed = id.parse::<u32>().map_err(|_| "invalid Steam app ID")?;
            if parsed == 0 || parsed.to_string() != *id {
                return Err("Steam app IDs must be canonical nonzero decimal values");
            }
        }
        if let Some(name) = &self.name
            && (name.trim().is_empty() || name.len() > 512 || name.chars().any(char::is_control))
        {
            return Err("exclusion names must contain 1..512 printable bytes");
        }
        Ok(())
    }

    pub fn matches(&self, app_id: u32, game_name: &str) -> bool {
        if let Some(id) = &self.id {
            return id == &app_id.to_string();
        }
        self.name
            .as_deref()
            .is_some_and(|name| name.to_lowercase() == game_name.to_lowercase())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductSettings {
    pub enabled: bool,
    #[serde(default)]
    pub auto_sync: bool,
    #[serde(default)]
    pub exclude_games: Vec<Exclusion>,
    #[serde(default)]
    pub include_tools: bool,
    #[serde(default)]
    pub sync_all_installed: bool,
    #[serde(default = "default_recent_games")]
    pub recent_games: u32,
    #[serde(default = "default_recent_max_age_days")]
    pub recent_max_age_days: u32,
}

pub(super) fn default_recent_games() -> u32 {
    10
}

pub(super) fn default_recent_max_age_days() -> u32 {
    30
}

#[allow(clippy::derivable_impls)] // The default follows Vibeshine's Linux policy.
impl Default for ProductSettings {
    fn default() -> Self {
        Self {
            enabled: cfg!(target_os = "linux"),
            auto_sync: false,
            exclude_games: Vec::new(),
            include_tools: false,
            sync_all_installed: false,
            recent_games: default_recent_games(),
            recent_max_age_days: default_recent_max_age_days(),
        }
    }
}

impl ProductSettings {
    pub fn for_host(mut settings: Self) -> Self {
        if cfg!(target_os = "linux") {
            settings.enabled = true;
        }
        settings
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.recent_games > 10_000 {
            return Err("recent Steam game limit cannot exceed 10000");
        }
        if self.recent_max_age_days > 36_500 {
            return Err("recent Steam age limit cannot exceed 36500 days");
        }
        if self.exclude_games.len() > MAX_EXCLUSIONS {
            return Err("at most 256 excluded Steam games are allowed");
        }
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        for exclusion in &self.exclude_games {
            exclusion.validate()?;
            if let Some(id) = &exclusion.id
                && !ids.insert(id.clone())
            {
                return Err("duplicate Steam app ID exclusion");
            }
            if exclusion.id.is_none()
                && let Some(name) = &exclusion.name
                && !names.insert(name.to_lowercase())
            {
                return Err("duplicate Steam name exclusion");
            }
        }
        Ok(())
    }

    pub fn from_product_document(payload: &Value) -> Result<(Self, u64), String> {
        if payload.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err("product settings service returned an error".into());
        }
        let fields = ["ok", "found", "version", "revision", "values"];
        let Some(object) = payload.as_object() else {
            return Err("product settings response is not an object".into());
        };
        if object.len() != fields.len() || object.keys().any(|key| !fields.contains(&key.as_str()))
        {
            return Err("product settings response has missing or unknown fields".into());
        }
        let revision = payload
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or("product settings revision is missing")?;
        match payload.get("found").and_then(Value::as_bool) {
            Some(false) => {
                if revision != 0
                    || !payload.get("version").is_some_and(Value::is_null)
                    || payload.get("values") != Some(&serde_json::json!({}))
                {
                    return Err("absent product settings document is malformed".into());
                }
                Ok((Self::default(), 0))
            }
            Some(true) => {
                if payload.get("version").and_then(Value::as_u64)
                    != Some(u64::from(PRODUCT_SETTINGS_VERSION))
                    || revision == 0
                {
                    return Err("unsupported or malformed product settings document".into());
                }
                let settings: Self = serde_json::from_value(
                    payload
                        .get("values")
                        .cloned()
                        .ok_or("product settings values are missing")?,
                )
                .map_err(|_| "product settings values are malformed")?;
                settings.validate().map_err(str::to_owned)?;
                Ok((Self::for_host(settings), revision))
            }
            None => Err("product settings service omitted found".into()),
        }
    }

    pub fn product_update_payload(&self, expected_revision: u64) -> Value {
        let settings = Self::for_host(self.clone());
        json!({
            "action": "product_update",
            "expected_revision": expected_revision,
            "version": PRODUCT_SETTINGS_VERSION,
            "values": settings,
        })
    }

    pub fn management_response(&self, revision: u64) -> Value {
        json!({
            "ok": true,
            "revision": revision,
            "values": self,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupSettings {
    #[serde(default)]
    pub roots: Vec<std::path::PathBuf>,
}

impl StartupSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.roots.len() > 32
            || self
                .roots
                .iter()
                .any(|root| !root.is_absolute() || root.as_os_str().len() > 4096)
        {
            return Err("Steam roots must be at most 32 bounded absolute paths");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_disabled_and_generates_a_versioned_product_update() {
        let settings = ProductSettings::default();
        assert_eq!(settings.enabled, cfg!(target_os = "linux"));
        assert!(!settings.auto_sync);
        assert!(settings.exclude_games.is_empty());
        assert_eq!(
            settings.product_update_payload(0),
            json!({
                "action":"product_update",
                "expected_revision":0,
                "version":1,
                "values":{"enabled":cfg!(target_os = "linux"),"auto_sync":false,"exclude_games":[],"include_tools":false,"sync_all_installed":false,"recent_games":10,"recent_max_age_days":30}
            })
        );
    }

    #[test]
    fn product_documents_require_the_supported_version_and_exact_values() {
        assert_eq!(
            ProductSettings::from_product_document(&json!({
                "ok":true,"found":false,"version":null,"revision":0,"values":{}
            }))
            .unwrap(),
            (ProductSettings::default(), 0)
        );
        assert_eq!(
            ProductSettings::from_product_document(&json!({
                "ok":true,"found":true,"version":1,"revision":4,"values":{"enabled":true}
            }))
            .unwrap(),
            (
                ProductSettings {
                    enabled: true,
                    auto_sync: false,
                    exclude_games: Vec::new(),
                    ..ProductSettings::default()
                },
                4
            )
        );
        for payload in [
            json!({"ok":true,"found":true,"version":2,"revision":1,"values":{"enabled":true}}),
            json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"enabled":true,"other":1}}),
            json!({"ok":true,"found":true,"version":1,"revision":1,"values":{}}),
            json!({"ok":true,"found":false,"version":null,"revision":1,"values":{}}),
            json!({"ok":true,"found":false,"version":1,"revision":0,"values":{}}),
            json!({"ok":true,"found":false,"version":null,"revision":0,"values":{"enabled":true}}),
            json!({"ok":true,"found":false,"version":null,"revision":0,"values":[]}),
            json!({"ok":true,"found":false,"revision":0,"values":{}}),
            json!({"ok":true,"found":false,"version":null,"revision":0,"values":{},"owner":"steam"}),
        ] {
            assert!(ProductSettings::from_product_document(&payload).is_err());
        }
    }

    #[test]
    fn product_settings_management_response_omits_storage_version() {
        assert_eq!(
            ProductSettings {
                enabled: true,
                auto_sync: true,
                exclude_games: vec![Exclusion {
                    id: Some("730".into()),
                    name: Some("Counter-Strike 2".into()),
                }],
                ..ProductSettings::default()
            }
            .management_response(8),
            json!({"ok":true,"revision":8,"values":{"enabled":true,"auto_sync":true,"exclude_games":[{"id":"730","name":"Counter-Strike 2"}],"include_tools":false,"sync_all_installed":false,"recent_games":10,"recent_max_age_days":30}})
        );
    }

    #[test]
    fn exclusions_are_bounded_validated_and_match_by_id_or_name_fallback() {
        let by_id = Exclusion {
            id: Some("730".into()),
            name: Some("Renamed display label".into()),
        };
        assert!(by_id.matches(730, "Counter-Strike 2"));
        assert!(!by_id.matches(731, "Renamed display label"));
        let by_name = Exclusion {
            id: None,
            name: Some("Example Game".into()),
        };
        assert!(by_name.matches(12, "example game"));

        for exclusions in [
            vec![Exclusion {
                id: None,
                name: None,
            }],
            vec![Exclusion {
                id: Some("0".into()),
                name: None,
            }],
            vec![Exclusion {
                id: Some("0730".into()),
                name: None,
            }],
            vec![Exclusion {
                id: None,
                name: Some("  ".into()),
            }],
            vec![
                Exclusion {
                    id: Some("730".into()),
                    name: None,
                },
                Exclusion {
                    id: Some("730".into()),
                    name: Some("Game".into()),
                },
            ],
            vec![
                Exclusion {
                    id: None,
                    name: Some("Game".into()),
                },
                Exclusion {
                    id: None,
                    name: Some("game".into()),
                },
            ],
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
        let too_many = vec![
            Exclusion {
                id: None,
                name: Some("Game".into())
            };
            MAX_EXCLUSIONS + 1
        ];
        assert!(
            ProductSettings {
                exclude_games: too_many,
                ..ProductSettings::default()
            }
            .validate()
            .is_err()
        );
    }
}
