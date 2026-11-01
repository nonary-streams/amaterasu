pub use amaterasu_module_sdk::effect_authority::{
    ConfigurationTransaction, ConfigurationTransactionState,
};
use serde::{Deserialize, Serialize};

pub const DOCUMENT_VERSION: u32 = 1;
pub const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MinLogLevel {
    ProductDefault,
    Verbose,
    Debug,
    Info,
    Warning,
    Error,
    Fatal,
    None,
}

impl MinLogLevel {
    pub fn for_product_channel(self, package_version: &str) -> Self {
        match self {
            Self::ProductDefault if is_prerelease_channel(package_version) => Self::Debug,
            Self::ProductDefault => Self::Info,
            level => level,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub file_state: String,
    pub vibeshine_file_state: String,
    #[serde(default)]
    pub credentials_file: String,
    pub pkey: String,
    pub cert: String,
    pub log_path: String,
    pub min_log_level: MinLogLevel,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            file_state: "sunshine_state.json".into(),
            vibeshine_file_state: "vibeshine_state.json".into(),
            credentials_file: String::new(),
            pkey: "credentials/cakey.pem".into(),
            cert: "credentials/cacert.pem".into(),
            log_path: "amaterasu.log".into(),
            min_log_level: MinLogLevel::ProductDefault,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        validate_path("file_state", &self.file_state, false)?;
        validate_path("vibeshine_file_state", &self.vibeshine_file_state, false)?;
        validate_path("credentials_file", &self.credentials_file, true)?;
        validate_path("pkey", &self.pkey, false)?;
        validate_path("cert", &self.cert, false)?;
        validate_path("log_path", &self.log_path, false)?;
        Ok(())
    }
}

fn validate_path(field: &str, value: &str, empty_allowed: bool) -> Result<(), String> {
    if value.is_empty() {
        return if empty_allowed {
            Ok(())
        } else {
            Err(format!("{field} must not be empty"))
        };
    }
    if value.len() > MAX_PATH_BYTES {
        return Err(format!("{field} exceeds {MAX_PATH_BYTES} UTF-8 bytes"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{field} contains a control character"));
    }

    let path = std::path::Path::new(value);
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("{field} must not contain parent traversal"));
    }
    #[cfg(windows)]
    if !path.is_absolute()
        && path
            .components()
            .any(|component| matches!(component, std::path::Component::Prefix(_)))
    {
        return Err(format!("{field} must not be drive-relative"));
    }
    #[cfg(not(windows))]
    if !path.is_absolute()
        && path
            .components()
            .any(|component| matches!(component, std::path::Component::RootDir))
    {
        return Err(format!("{field} has an invalid relative root"));
    }
    if !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::CurDir))
    {
        return Err(format!("{field} must name a path beneath the state root"));
    }
    Ok(())
}

pub fn is_prerelease_channel(package_version: &str) -> bool {
    let Some((_, prerelease)) = package_version.split_once('-') else {
        return false;
    };
    let identifier = prerelease.split(['.', '+']).next().unwrap_or_default();
    !identifier.is_empty() && !identifier.eq_ignore_ascii_case("stable")
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub version: u32,
    pub revision: u64,
    pub file_state: String,
    pub vibeshine_file_state: String,
    #[serde(default)]
    pub credentials_file: String,
    pub pkey: String,
    pub cert: String,
    pub log_path: String,
    pub min_log_level: MinLogLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<ConfigurationTransaction>,
}

impl Default for Document {
    fn default() -> Self {
        Self::from_settings(0, Settings::default())
    }
}

impl Document {
    pub fn from_settings(revision: u64, settings: Settings) -> Self {
        Self {
            version: DOCUMENT_VERSION,
            revision,
            file_state: settings.file_state,
            vibeshine_file_state: settings.vibeshine_file_state,
            credentials_file: settings.credentials_file,
            pkey: settings.pkey,
            cert: settings.cert,
            log_path: settings.log_path,
            min_log_level: settings.min_log_level,
            transaction: None,
        }
    }

    pub fn settings(&self) -> Settings {
        Settings {
            file_state: self.file_state.clone(),
            vibeshine_file_state: self.vibeshine_file_state.clone(),
            credentials_file: self.credentials_file.clone(),
            pkey: self.pkey.clone(),
            cert: self.cert.clone(),
            log_path: self.log_path.clone(),
            min_log_level: self.min_log_level,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != DOCUMENT_VERSION {
            return Err(format!("unsupported document version {}", self.version));
        }
        if self
            .transaction
            .as_ref()
            .is_some_and(|transaction| !transaction.valid_for_configuration_revision(self.revision))
        {
            return Err("invalid configuration transaction".into());
        }
        self.settings().validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_match_the_v1_contract() {
        let settings = Settings::default();
        assert_eq!(settings.file_state, "sunshine_state.json");
        assert_eq!(settings.vibeshine_file_state, "vibeshine_state.json");
        assert_eq!(settings.credentials_file, "");
        assert_eq!(settings.pkey, "credentials/cakey.pem");
        assert_eq!(settings.cert, "credentials/cacert.pem");
        assert_eq!(settings.log_path, "amaterasu.log");
        assert_eq!(settings.min_log_level, MinLogLevel::ProductDefault);
        assert_eq!(
            settings.min_log_level.for_product_channel("1.2.3-stable.1"),
            MinLogLevel::Info
        );
        assert_eq!(
            settings.min_log_level.for_product_channel("1.2.3-beta.1"),
            MinLogLevel::Debug
        );
    }

    #[test]
    fn document_has_version_revision_and_exactly_seven_settings() {
        let document = Document::default();
        let value = serde_json::to_value(&document).unwrap();
        assert_eq!(value["version"], json!(1));
        assert_eq!(value["revision"], json!(0));
        assert_eq!(value.as_object().unwrap().len(), 9);
        let mut missing_credentials = value.clone();
        missing_credentials
            .as_object_mut()
            .unwrap()
            .remove("credentials_file");
        assert_eq!(
            serde_json::from_value::<Document>(missing_credentials)
                .unwrap()
                .credentials_file,
            ""
        );
        assert!(
            serde_json::from_value::<Document>(json!({
                "version":1,"revision":0,"file_state":"sunshine_state.json",
                "vibeshine_file_state":"vibeshine_state.json","credentials_file":"",
                "pkey":"credentials/cakey.pem","cert":"credentials/cacert.pem",
                "log_path":"amaterasu.log","min_log_level":"product_default",
                "future_field":true
            }))
            .is_err()
        );
    }

    #[test]
    fn settings_reject_unsafe_or_unbounded_paths() {
        for path in [
            "",
            "../outside",
            "nested/../../outside",
            "name\0.json",
            "line\nbreak",
        ] {
            let settings = Settings {
                file_state: path.into(),
                ..Settings::default()
            };
            assert!(settings.validate().is_err(), "accepted {path:?}");
        }
        let too_long = "x".repeat(MAX_PATH_BYTES + 1);
        let settings = Settings {
            log_path: too_long,
            ..Settings::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn empty_credentials_path_is_the_inherit_marker() {
        let settings: Settings = serde_json::from_value(json!({
            "file_state":"state.json","vibeshine_file_state":"legacy.json",
            "credentials_file":"","pkey":"key.pem","cert":"cert.pem",
            "log_path":"service.log","min_log_level":"warning"
        }))
        .unwrap();
        assert!(settings.validate().is_ok());
        assert_eq!(settings.min_log_level, MinLogLevel::Warning);
    }
}
