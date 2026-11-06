//! Durable, caller-scoped manual application catalogs.
mod api;
mod model;
mod pipeline;
mod product;
mod protocol;
mod seed;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use pipeline::Pipeline;

const SERVICE: &str = "amaterasu.catalog/1";
const STATE_SERVICE: &str = "amaterasu.state/1";

#[derive(serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    /// Trusted library caller selected by the operator-approved package.
    library_principal: String,
    launch_principals: std::collections::BTreeSet<String>,
    export_principals: std::collections::BTreeMap<String, String>,
    steam_id_export_principals: std::collections::BTreeMap<String, String>,
    import_principals: std::collections::BTreeMap<String, String>,
    /// Authenticated importer principal -> configured provider key.
    provider_launch_principals: std::collections::BTreeMap<String, String>,
    /// Create one `Desktop` entry, once, in each catalog that
    /// `export_principals` serves to streaming clients.
    seed_desktop_entry: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            library_principal: "amaterasu.library".into(),
            launch_principals: Default::default(),
            export_principals: Default::default(),
            steam_id_export_principals: Default::default(),
            import_principals: Default::default(),
            provider_launch_principals: Default::default(),
            seed_desktop_entry: false,
        }
    }
}

impl Settings {
    fn validate(&self) -> ModuleResult<()> {
        if !valid_principal(&self.library_principal)
            || matches!(
                self.library_principal.as_str(),
                "amaterasu.nvhttp"
                    | "amaterasu.management"
                    | "amaterasu.launch"
                    | "amaterasu.catalog"
            )
            || self.launch_principals.contains(&self.library_principal)
            || self.launch_principals.len() > 16
            || self
                .launch_principals
                .iter()
                .any(|principal| !valid_principal(principal))
            || self.export_principals.len() > 16
            || self
                .export_principals
                .iter()
                .any(|(consumer, owner)| !valid_principal(consumer) || !valid_principal(owner))
            || self.steam_id_export_principals.len() > 16
            || self
                .steam_id_export_principals
                .iter()
                .any(|(consumer, owner)| !valid_principal(consumer) || !valid_principal(owner))
            || self.import_principals.len() > 16
            || self
                .import_principals
                .iter()
                .any(|(consumer, owner)| !valid_principal(consumer) || !valid_principal(owner))
            || self.provider_launch_principals.len() > 16
            || self
                .provider_launch_principals
                .iter()
                .any(|(principal, provider)| {
                    !valid_principal(principal)
                        || !model::valid_provider_key(provider)
                        || !self.import_principals.contains_key(principal)
                })
            || self
                .provider_launch_principals
                .values()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.provider_launch_principals.len()
        {
            return Err("catalog principal settings require 1..128 character identifiers and at most 16 entries each".into());
        }
        Ok(())
    }
}

fn valid_principal(principal: &str) -> bool {
    !principal.is_empty()
        && principal.len() <= 128
        && principal
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

struct CatalogModule {
    runtime: RuntimeClient,
    pipeline: Pipeline,
}

impl ManagedModule for CatalogModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("catalog settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid catalog settings")?;
        settings.validate()?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        amaterasu_module_sdk::operation_api::register_selected(&runtime, &api::methods())?;
        let seeded_owners: std::collections::BTreeSet<String> = if settings.seed_desktop_entry {
            settings.export_principals.values().cloned().collect()
        } else {
            Default::default()
        };
        Ok(Self {
            runtime,
            pipeline: Pipeline::new_with_steam_id_export_principals(
                settings.launch_principals,
                settings.export_principals,
                settings.steam_id_export_principals,
                settings.import_principals,
            )
            .with_library_principal(settings.library_principal)
            .with_provider_launch_principals(settings.provider_launch_principals)
            .with_desktop_seed(seeded_owners),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.pipeline
            .tick(|command| self.runtime.send_ephemeral(command))
    }
}

amaterasu_module_sdk::export_module!(CatalogModule);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_library_principal_is_validated_at_startup() {
        let defaults: Settings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(defaults.library_principal, "amaterasu.library");
        let selected: Settings =
            serde_json::from_value(serde_json::json!({"library_principal":"external.library"}))
                .unwrap();
        assert!(selected.validate().is_ok());
        for invalid in [
            "",
            "bad/name",
            "a b",
            "amaterasu.nvhttp",
            "amaterasu.management",
            "amaterasu.launch",
            "amaterasu.catalog",
        ] {
            let settings: Settings =
                serde_json::from_value(serde_json::json!({"library_principal":invalid})).unwrap();
            assert!(settings.validate().is_err(), "{invalid}");
        }
        let launch_overlap: Settings = serde_json::from_value(serde_json::json!({
            "library_principal":"external.library", "launch_principals":["external.library"]
        }))
        .unwrap();
        assert!(launch_overlap.validate().is_err());
        for invalid in [
            serde_json::json!(null),
            serde_json::json!(false),
            serde_json::json!(7),
        ] {
            assert!(
                serde_json::from_value::<Settings>(
                    serde_json::json!({"library_principal":invalid})
                )
                .is_err()
            );
        }
    }

    #[test]
    fn provider_launch_mapping_requires_unique_authorized_importers() {
        let valid: Settings = serde_json::from_value(serde_json::json!({
            "import_principals":{"importer":"owner"},
            "provider_launch_principals":{"importer":"provider-key"}
        }))
        .unwrap();
        assert!(valid.validate().is_ok());
        for value in [
            serde_json::json!({"provider_launch_principals":{"missing":"provider-key"}}),
            serde_json::json!({"import_principals":{"a":"owner","b":"owner"},"provider_launch_principals":{"a":"same","b":"same"}}),
            serde_json::json!({"import_principals":{"a":"owner"},"provider_launch_principals":{"a":"bad/key"}}),
        ] {
            let settings: Settings = serde_json::from_value(value).unwrap();
            assert!(settings.validate().is_err());
        }
    }

    #[test]
    fn desktop_seed_is_off_by_default_and_strictly_typed() {
        let defaults: Settings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!defaults.seed_desktop_entry);
        let enabled: Settings =
            serde_json::from_value(serde_json::json!({"seed_desktop_entry":true})).unwrap();
        assert!(enabled.seed_desktop_entry);
        assert!(enabled.validate().is_ok());
        for invalid in [
            serde_json::json!({"seed_desktop_entry":"yes"}),
            serde_json::json!({"seed_desktop_entry":null}),
            serde_json::json!({"seed-desktop-entry":true}),
        ] {
            assert!(serde_json::from_value::<Settings>(invalid).is_err());
        }
    }

    #[test]
    fn export_settings_validate_both_sides_and_capacity() {
        let defaults: Settings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(defaults.export_principals.is_empty());
        assert!(defaults.steam_id_export_principals.is_empty());
        assert!(defaults.import_principals.is_empty());
        let configured_steam_export: Settings = serde_json::from_value(serde_json::json!({
            "steam_id_export_principals": {
                "amaterasu.lutris": "amaterasu.management"
            }
        }))
        .unwrap();
        assert!(configured_steam_export.validate().is_ok());
        assert_eq!(
            configured_steam_export.steam_id_export_principals,
            std::collections::BTreeMap::from([(
                "amaterasu.lutris".into(),
                "amaterasu.management".into()
            )])
        );
        for (consumer, owner) in [
            ("", "owner"),
            ("consumer", "../owner"),
            ("consumer", ""),
            ("bad/name", "owner"),
        ] {
            let settings = Settings {
                export_principals: [(consumer.into(), owner.into())].into(),
                ..Default::default()
            };
            assert!(settings.validate().is_err());
        }
        let mut settings = Settings {
            export_principals: (0..16)
                .map(|id| (format!("consumer-{id}"), "owner.valid".into()))
                .collect(),
            ..Default::default()
        };
        assert!(settings.validate().is_ok());
        settings
            .export_principals
            .insert("extra".into(), "owner".into());
        assert!(settings.validate().is_err());

        let mut steam_id_exports = Settings {
            steam_id_export_principals: (0..16)
                .map(|id| (format!("steam-consumer-{id}"), "owner.valid".into()))
                .collect(),
            ..Default::default()
        };
        assert!(steam_id_exports.validate().is_ok());
        steam_id_exports
            .steam_id_export_principals
            .insert("extra".into(), "owner".into());
        assert!(steam_id_exports.validate().is_err());
        for (consumer, owner) in [
            ("", "owner"),
            ("consumer", "../owner"),
            ("consumer", ""),
            ("bad/name", "owner"),
        ] {
            let settings = Settings {
                steam_id_export_principals: [(consumer.into(), owner.into())].into(),
                ..Default::default()
            };
            assert!(settings.validate().is_err());
        }

        let mut imports = Settings {
            import_principals: std::collections::BTreeMap::from([(
                "amaterasu.steam".into(),
                "amaterasu.management".into(),
            )]),
            ..Default::default()
        };
        assert!(imports.validate().is_ok());
        imports
            .import_principals
            .insert("../steam".into(), "owner".into());
        assert!(imports.validate().is_err());
    }
}
