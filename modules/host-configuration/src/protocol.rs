use crate::settings::{MinLogLevel, Settings};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Readiness {},
    Get {},
    Update {
        expected_revision: u64,
        file_state: String,
        vibeshine_file_state: String,
        credentials_file: String,
        pkey: String,
        cert: String,
        log_path: String,
        min_log_level: MinLogLevel,
    },
}

impl Operation {
    pub fn settings(&self) -> Option<Settings> {
        match self {
            Self::Update {
                file_state,
                vibeshine_file_state,
                credentials_file,
                pkey,
                cert,
                log_path,
                min_log_level,
                ..
            } => Some(Settings {
                file_state: file_state.clone(),
                vibeshine_file_state: vibeshine_file_state.clone(),
                credentials_file: credentials_file.clone(),
                pkey: pkey.clone(),
                cert: cert.clone(),
                log_path: log_path.clone(),
                min_log_level: *min_log_level,
            }),
            Self::Readiness {} | Self::Get {} => None,
        }
    }
}

pub fn error(code: &str, message: impl Into<String>) -> Value {
    json!({"ok":false,"error":{"code":code,"message":message.into()}})
}

pub fn unavailable(message: impl Into<String>) -> Value {
    json!({
        "ok":false,
        "ready":false,
        "error":{"code":"configuration_unavailable","message":message.into()}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn update() -> Value {
        json!({
            "action":"update","expected_revision":0,
            "file_state":"sunshine_state.json",
            "vibeshine_file_state":"vibeshine_state.json",
            "credentials_file":"","pkey":"credentials/cakey.pem",
            "cert":"credentials/cacert.pem","log_path":"amaterasu.log",
            "min_log_level":"product_default"
        })
    }

    #[test]
    fn update_requires_full_typed_settings_and_rejects_unknown_fields() {
        let value = update();
        assert!(serde_json::from_value::<Operation>(value.clone()).is_ok());
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove("cert");
        assert!(serde_json::from_value::<Operation>(missing).is_err());
        let mut unknown = value;
        unknown["admin"] = json!(true);
        assert!(serde_json::from_value::<Operation>(unknown).is_err());
        let mut unsupported_level = update();
        unsupported_level["min_log_level"] = json!("trace");
        assert!(serde_json::from_value::<Operation>(unsupported_level).is_err());
    }

    #[test]
    fn read_operations_are_exact() {
        assert!(serde_json::from_value::<Operation>(json!({"action":"get"})).is_ok());
        assert!(serde_json::from_value::<Operation>(json!({"action":"readiness"})).is_ok());
        assert!(
            serde_json::from_value::<Operation>(
                json!({"action":"get","caller":"amaterasu.management"})
            )
            .is_err()
        );
    }
}
