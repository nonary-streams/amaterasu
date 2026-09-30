//! Opaque bootstrap handoff for the independently built logging consumer.
use amaterasu_contracts::{DIAGNOSTICS_COMPONENT, RuntimeConfig};
use amaterasu_logging::ProductLogger;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostLoggingConfig {
    global: Value,
    state_root: String,
}

impl HostLoggingConfig {
    pub(crate) fn from_global(global: Option<&Value>, state_root: &Path) -> Result<Self, String> {
        let state_root = crate::loading::path_to_unicode_string(state_root)?;
        Ok(Self {
            global: global.cloned().unwrap_or(Value::Null),
            state_root,
        })
    }

    /// Passes global settings unchanged. Product keys and defaults are resolved
    /// by the independently built logging library and Diagnostics module.
    pub(crate) fn inject(&self, runtime_config: &mut RuntimeConfig) -> Result<(), String> {
        if !runtime_config
            .principals
            .contains_key(DIAGNOSTICS_COMPONENT)
        {
            return Ok(());
        }
        let settings = runtime_config
            .settings
            .entry(DIAGNOSTICS_COMPONENT.to_owned())
            .or_insert_with(|| json!({}));
        let settings = settings
            .as_object_mut()
            .ok_or("diagnostics component settings must be an object")?;
        settings.insert(
            "host_logging".into(),
            json!({
                "global": self.global,
                "state_root": self.state_root,
            }),
        );
        Ok(())
    }

    /// Starts the product-owned default policy. The host supplies only the
    /// canonical root and an opaque global settings value selected at bootstrap.
    pub(crate) fn start_product_logger(&self) -> Option<ProductLogger> {
        ProductLogger::start_global(Some(&self.global), PathBuf::from(&self.state_root)).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_settings_are_forwarded_opaque_for_diagnostics() {
        let global = json!({
            "min_log_level": "warning",
            "log_path": r"logs\amaterasu.log",
            "unrelated_product_setting": {"kept": true}
        });
        let config =
            HostLoggingConfig::from_global(Some(&global), Path::new(r"C:\private\state")).unwrap();

        let mut runtime = RuntimeConfig::default();
        runtime
            .principals
            .insert(DIAGNOSTICS_COMPONENT.into(), Default::default());
        runtime
            .settings
            .insert(DIAGNOSTICS_COMPONENT.into(), json!({"other":"kept"}));
        config.inject(&mut runtime).unwrap();

        assert_eq!(
            runtime.settings[DIAGNOSTICS_COMPONENT],
            json!({
                "other":"kept",
                "host_logging": {
                    "global": global,
                    "state_root": r"C:\private\state",
                }
            })
        );
    }

    #[test]
    fn absent_diagnostics_principal_receives_no_host_logging_settings() {
        let config = HostLoggingConfig::from_global(None, Path::new(r"C:\private\state")).unwrap();
        let mut runtime = RuntimeConfig::default();
        runtime
            .principals
            .insert("amaterasu.console".into(), Default::default());
        runtime
            .settings
            .insert("unexpected.principal".into(), json!({"kept": true}));

        config.inject(&mut runtime).unwrap();

        assert!(!runtime.settings.contains_key(DIAGNOSTICS_COMPONENT));
        assert_eq!(
            runtime.settings["unexpected.principal"],
            json!({"kept": true})
        );
    }
}
