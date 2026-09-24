use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PRODUCT_SETTINGS_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductSettings {
    pub enabled: bool,
}

#[allow(clippy::derivable_impls)] // The default follows Vibeshine's Linux policy.
impl Default for ProductSettings {
    fn default() -> Self {
        Self {
            enabled: cfg!(target_os = "linux"),
        }
    }
}

impl ProductSettings {
    pub fn for_host(enabled: bool) -> Self {
        Self {
            enabled: if cfg!(target_os = "linux") {
                true
            } else {
                enabled
            },
        }
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
                Ok((Self::for_host(settings.enabled), revision))
            }
            None => Err("product settings service omitted found".into()),
        }
    }

    pub fn product_update_payload(self, expected_revision: u64) -> Value {
        let settings = Self::for_host(self.enabled);
        json!({
            "action": "product_update",
            "expected_revision": expected_revision,
            "version": PRODUCT_SETTINGS_VERSION,
            "values": {"enabled": settings.enabled},
        })
    }

    pub fn management_response(self, revision: u64) -> Value {
        json!({
            "ok": true,
            "revision": revision,
            "values": {"enabled": self.enabled},
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
        assert_eq!(
            settings.product_update_payload(0),
            json!({
                "action":"product_update",
                "expected_revision":0,
                "version":1,
                "values":{"enabled":cfg!(target_os = "linux")}
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
            (ProductSettings { enabled: true }, 4)
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
            ProductSettings { enabled: true }.management_response(8),
            json!({"ok":true,"revision":8,"values":{"enabled":true}})
        );
    }
}
