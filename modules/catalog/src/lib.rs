//! Durable, caller-scoped manual application catalogs.
mod model;
mod pipeline;
mod product;
mod protocol;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use pipeline::Pipeline;

const SERVICE: &str = "amaterasu.catalog/1";
const STATE_SERVICE: &str = "amaterasu.state/1";

#[derive(Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    launch_principals: std::collections::BTreeSet<String>,
    export_principals: std::collections::BTreeMap<String, String>,
    import_principals: std::collections::BTreeMap<String, String>,
}

impl Settings {
    fn validate(&self) -> ModuleResult<()> {
        if self.launch_principals.len() > 16
            || self
                .launch_principals
                .iter()
                .any(|principal| !valid_principal(principal))
            || self.export_principals.len() > 16
            || self
                .export_principals
                .iter()
                .any(|(consumer, owner)| !valid_principal(consumer) || !valid_principal(owner))
            || self.import_principals.len() > 16
            || self
                .import_principals
                .iter()
                .any(|(consumer, owner)| !valid_principal(consumer) || !valid_principal(owner))
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
        Ok(Self {
            runtime,
            pipeline: Pipeline::new(
                settings.launch_principals,
                settings.export_principals,
                settings.import_principals,
            ),
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
    fn export_settings_validate_both_sides_and_capacity() {
        let defaults: Settings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(defaults.export_principals.is_empty());
        assert!(defaults.import_principals.is_empty());
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
