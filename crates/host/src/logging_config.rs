//! Host-owned snapshot of the global logging settings for one worker lifetime.
use amaterasu_contracts::RuntimeConfig;
use serde_json::{Value, json};
use std::path::{Component, Path};

const DIAGNOSTICS_COMPONENT: &str = "amaterasu.diagnostics";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum MinimumLogLevel {
    Verbose = 0,
    Debug = 1,
    Info = 2,
    Warning = 3,
    Error = 4,
    Fatal = 5,
    None = 6,
}

impl MinimumLogLevel {
    fn parse_source_value(value: &str, default: Self) -> Self {
        match value {
            "verbose" => Self::Verbose,
            "debug" => Self::Debug,
            "info" => Self::Info,
            "warning" => Self::Warning,
            "error" => Self::Error,
            "fatal" => Self::Fatal,
            "none" => Self::None,
            _ => match value.as_bytes().first() {
                Some(b'0'..=b'6') => match value.as_bytes()[0] - b'0' {
                    0 => Self::Verbose,
                    1 => Self::Debug,
                    2 => Self::Info,
                    3 => Self::Warning,
                    4 => Self::Error,
                    5 => Self::Fatal,
                    6 => Self::None,
                    _ => unreachable!(),
                },
                _ => default,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostLoggingConfig {
    pub(crate) minimum_level: MinimumLogLevel,
    pub(crate) log_path: String,
    pub(crate) state_root: String,
}

impl HostLoggingConfig {
    pub(crate) fn from_global(global: Option<&Value>, state_root: &Path) -> Self {
        let default_level = default_minimum_level(env!("CARGO_PKG_VERSION"));
        let minimum_level = global
            .and_then(|settings| settings.get("min_log_level"))
            .and_then(Value::as_str)
            .map_or(default_level, |value| {
                MinimumLogLevel::parse_source_value(value, default_level)
            });

        let default_log_path = Path::new("amaterasu.log");
        let configured_log_path = global
            .and_then(|settings| settings.get("log_path"))
            .and_then(Value::as_str)
            .filter(|path| valid_log_path(path));
        let log_path = configured_log_path
            .map(Path::new)
            .unwrap_or(default_log_path);

        Self {
            minimum_level,
            log_path: log_path.to_string_lossy().into_owned(),
            state_root: state_root.to_string_lossy().into_owned(),
        }
    }

    /// Injects trusted host configuration after package verification. Global
    /// policy is not a component principal and is normally absent at runtime.
    pub(crate) fn inject(&self, runtime_config: &mut RuntimeConfig) -> Result<(), String> {
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
                "min_log_level": self.minimum_level as u8,
                "log_path": self.log_path,
                "state_root": self.state_root,
            }),
        );
        Ok(())
    }
}

fn default_minimum_level(package_version: &str) -> MinimumLogLevel {
    if is_prerelease_channel(package_version) {
        MinimumLogLevel::Debug
    } else {
        MinimumLogLevel::Info
    }
}

/// Mirrors Vibeshine's semver channel rule: any non-empty prerelease is a
/// prerelease channel except one whose first identifier is `stable`.
fn is_prerelease_channel(version: &str) -> bool {
    let Some((_, prerelease)) = version.split_once('-') else {
        return false;
    };
    let first_identifier = prerelease.split(['.', '+']).next().unwrap_or_default();
    !first_identifier.is_empty() && !first_identifier.eq_ignore_ascii_case("stable")
}

fn valid_log_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.chars().any(char::is_control)
        && (Path::new(path).is_absolute()
            || Path::new(path)
                .components()
                .all(|component| matches!(component, Component::CurDir | Component::Normal(_))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_levels_and_defaults_are_preserved() {
        assert_eq!(default_minimum_level("1.2.3"), MinimumLogLevel::Info);
        assert_eq!(default_minimum_level("1.2.3-rc.2"), MinimumLogLevel::Debug);
        assert_eq!(
            default_minimum_level("1.2.3-stable.4"),
            MinimumLogLevel::Info
        );
        assert_eq!(default_minimum_level("1.2.3-StAbLe"), MinimumLogLevel::Info);

        for (source, expected) in [
            ("verbose", MinimumLogLevel::Verbose),
            ("debug", MinimumLogLevel::Debug),
            ("info", MinimumLogLevel::Info),
            ("warning", MinimumLogLevel::Warning),
            ("error", MinimumLogLevel::Error),
            ("fatal", MinimumLogLevel::Fatal),
            ("none", MinimumLogLevel::None),
            ("3suffix", MinimumLogLevel::Warning),
            ("6", MinimumLogLevel::None),
            ("INFO", MinimumLogLevel::Info),
            ("7", MinimumLogLevel::Info),
            ("unknown", MinimumLogLevel::Info),
        ] {
            assert_eq!(
                MinimumLogLevel::parse_source_value(source, MinimumLogLevel::Info),
                expected,
                "source setting {source}"
            );
        }
    }

    #[test]
    fn global_settings_are_normalized_and_injected_for_diagnostics() {
        let state_root = Path::new(r"C:\private\state");
        let global = json!({
            "min_log_level": "warning",
            "log_path": r"logs\amaterasu.log",
            "sunshine_name": "ignored here"
        });
        let config = HostLoggingConfig::from_global(Some(&global), state_root);
        assert_eq!(config.minimum_level, MinimumLogLevel::Warning);
        assert_eq!(config.log_path, r"logs\amaterasu.log");

        let mut runtime = RuntimeConfig::default();
        runtime
            .settings
            .insert(DIAGNOSTICS_COMPONENT.into(), json!({"other":"kept"}));
        config.inject(&mut runtime).unwrap();
        assert_eq!(
            runtime.settings[DIAGNOSTICS_COMPONENT],
            json!({
                "other":"kept",
                "host_logging": {
                    "min_log_level": 3,
                    "log_path": r"logs\amaterasu.log",
                    "state_root": r"C:\private\state",
                }
            })
        );
    }

    #[test]
    fn invalid_paths_use_the_private_default_and_missing_values_default_to_info() {
        let state_root = Path::new(r"C:\private\state");
        for invalid in ["", "../outside.log", "folder\u{0}name.log"] {
            let config =
                HostLoggingConfig::from_global(Some(&json!({"log_path":invalid})), state_root);
            assert_eq!(config.log_path, "amaterasu.log");
            assert_eq!(config.minimum_level, MinimumLogLevel::Info);
        }
    }
}
