use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Launch {
        operation_id: String,
        app_id: String,
    },
    LaunchExport {
        operation_id: String,
        app_id: String,
    },
    Lookup {
        operation_id: String,
    },
    Stop {
        launch_id: u64,
    },
    Status {
        launch_id: u64,
    },
    List {
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_limit")]
        limit: usize,
    },
}
fn page_limit() -> usize {
    16
}
impl Operation {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Launch {
                operation_id,
                app_id,
            }
            | Self::LaunchExport {
                operation_id,
                app_id,
            } => {
                if !valid_operation_id(operation_id) || !valid_app_id(app_id) {
                    return Err("invalid_request");
                }
            }
            Self::Lookup { operation_id } if !valid_operation_id(operation_id) => {
                return Err("invalid_request");
            }
            Self::List { limit, .. } if !(1..=16).contains(limit) => return Err("invalid_limit"),
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CatalogLaunch {
    Executable {
        program: String,
        #[serde(default)]
        arguments: Vec<String>,
        working_directory: Option<String>,
    },
    Application {
        path: String,
    },
}
#[derive(Clone)]
pub struct Executable {
    pub program: String,
    pub arguments: Vec<String>,
    pub working_directory: Option<String>,
}
impl Executable {
    pub fn from_catalog(value: &Value) -> Result<Self, &'static str> {
        let launch: CatalogLaunch =
            serde_json::from_value(value.clone()).map_err(|_| "invalid_catalog_response")?;
        match launch {
            CatalogLaunch::Executable {
                program,
                arguments,
                working_directory,
            } => {
                if !valid_native_path(&program)
                    || !valid_arguments(&arguments)
                    || working_directory
                        .as_deref()
                        .is_some_and(|cwd| !valid_native_path(cwd))
                {
                    return Err("unsupported_launch_spec");
                }
                Ok(Self {
                    program,
                    arguments,
                    working_directory,
                })
            }
            CatalogLaunch::Application { path } => {
                let _ = path;
                Err("unsupported_launch_kind")
            }
        }
    }
}
#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    Active,
    Stopping,
    Stopped,
    Cancelled,
    Failed,
    CleanupFailed,
}
impl State {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Cancelled | Self::Failed)
    }
}
#[derive(Clone, Copy)]
pub enum Outcome {
    Completed,
    Cancelled,
    Failed,
}
impl Outcome {
    pub fn state(self) -> State {
        match self {
            Self::Completed => State::Stopped,
            Self::Cancelled => State::Cancelled,
            Self::Failed => State::Failed,
        }
    }
    pub fn session_action(self) -> &'static str {
        match self {
            Self::Completed => "finish_stop",
            Self::Cancelled => "cancel",
            Self::Failed => "fail",
        }
    }
}
pub fn error(code: &str) -> Value {
    json!({"ok":false,"code":code})
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub environment: std::collections::BTreeMap<String, String>,
}
impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        let mut keys = std::collections::BTreeSet::new();
        if self.environment.len() > 32 {
            return Err("invalid_launch_settings");
        }
        let total_bytes: usize = self
            .environment
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum();
        if total_bytes > 4096 {
            return Err("invalid_launch_settings");
        }
        for (key, value) in &self.environment {
            if !valid_environment_binding(key, value) || !keys.insert(key.to_ascii_uppercase()) {
                return Err("invalid_launch_settings");
            }
        }
        Ok(())
    }
}

fn valid_operation_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_graphic())
}
fn valid_app_id(value: &str) -> bool {
    let Some(number) = value.strip_prefix("app-") else {
        return false;
    };
    number.len() == 16
        && number != "0000000000000000"
        && number
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
pub fn valid_catalog_principal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}
fn valid_native_path(value: &str) -> bool {
    std::path::Path::new(value).is_absolute() && value.len() <= 1024 && !value.contains('\0')
}
fn valid_arguments(arguments: &[String]) -> bool {
    arguments.len() <= 64
        && arguments.iter().map(String::len).sum::<usize>() <= 8192
        && arguments
            .iter()
            .all(|argument| argument.len() <= 1024 && !argument.contains('\0'))
}
fn valid_environment_binding(key: &str, value: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && value.len() <= 1024
        && !value.contains('\0')
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}
