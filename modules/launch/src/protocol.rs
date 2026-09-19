use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const MAX_PREPARATION_STEPS: usize = 8;
const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 10_000;
const MAX_COMMAND_TIMEOUT_MS: u64 = 30_000;

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
        #[serde(default)]
        working_directory: Option<String>,
        #[serde(default)]
        preparation: Vec<PreparationStep>,
    },
    Application {
        path: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationStep {
    pub run: ConfiguredCommand,
    #[serde(default)]
    pub undo: Option<ConfiguredCommand>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredCommand {
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_command_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_command_timeout_ms() -> u64 {
    DEFAULT_COMMAND_TIMEOUT_MS
}

#[derive(Clone, Debug)]
pub struct Executable {
    pub program: String,
    pub arguments: Vec<String>,
    pub working_directory: Option<String>,
    pub preparation: Vec<PreparationStep>,
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
                preparation,
            } => {
                if !valid_native_path(&program)
                    || !valid_arguments(&arguments)
                    || working_directory
                        .as_deref()
                        .is_some_and(|cwd| !valid_native_path(cwd))
                    || !valid_preparation(&preparation)
                {
                    return Err("unsupported_launch_spec");
                }
                Ok(Self {
                    program,
                    arguments,
                    working_directory,
                    preparation,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationOutcome {
    Exited,
    LaunchFailed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationState {
    Preparing,
    Running,
    Stopping,
    CleanupFailed(ApplicationOutcome),
    Exited,
    LaunchFailed,
    Cancelled,
}

impl ApplicationOutcome {
    pub fn launch_outcome(self) -> Outcome {
        match self {
            Self::Exited => Outcome::Completed,
            Self::LaunchFailed => Outcome::Failed,
            Self::Cancelled => Outcome::Cancelled,
        }
    }
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
    pub environment: BTreeMap<String, String>,
}
impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        let mut keys = BTreeSet::new();
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
    Path::new(value).is_absolute() && value.len() <= 1024 && !value.contains('\0')
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

fn valid_preparation(steps: &[PreparationStep]) -> bool {
    steps.len() <= MAX_PREPARATION_STEPS
        && steps.iter().all(|step| {
            valid_configured_command(&step.run)
                && step.undo.as_ref().is_none_or(valid_configured_command)
        })
}

fn valid_configured_command(command: &ConfiguredCommand) -> bool {
    if !valid_native_path(&command.executable)
        || command
            .cwd
            .as_deref()
            .is_some_and(|cwd| !valid_native_path(cwd))
        || !valid_arguments(&command.args)
        || !(1..=MAX_COMMAND_TIMEOUT_MS).contains(&command.timeout_ms)
        || command.environment.len() > 32
        || command
            .environment
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 4096
    {
        return false;
    }
    if Path::new(&command.executable)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
        })
    {
        return false;
    }
    let mut keys = BTreeSet::new();
    command.environment.iter().all(|(key, value)| {
        valid_environment_binding(key, value) && keys.insert(key.to_ascii_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_launch(preparation: Value) -> Value {
        json!({
            "kind":"executable",
            "program":std::env::current_exe().unwrap(),
            "arguments":[],
            "working_directory":null,
            "preparation":preparation,
        })
    }

    #[test]
    fn catalog_preparation_is_strict_and_applies_defaults() {
        let executable = Executable::from_catalog(&catalog_launch(json!([{
            "run":{"executable":std::env::current_exe().unwrap()},
        }])))
        .unwrap();
        let command = &executable.preparation[0].run;
        assert_eq!(command.timeout_ms, DEFAULT_COMMAND_TIMEOUT_MS);
        assert!(command.args.is_empty());
        assert!(command.environment.is_empty());
        assert!(command.cwd.is_none());
        assert!(executable.preparation[0].undo.is_none());

        let mut unknown = catalog_launch(json!([{
            "run":{"executable":std::env::current_exe().unwrap(),"shell":true},
        }]));
        assert_eq!(
            Executable::from_catalog(&unknown).unwrap_err(),
            "invalid_catalog_response"
        );
        unknown["preparation"][0] = json!({"run":{"executable":"relative"}});
        assert_eq!(
            Executable::from_catalog(&unknown).unwrap_err(),
            "unsupported_launch_spec"
        );
    }

    #[test]
    fn catalog_preparation_rejects_ambiguous_environment_and_command_bounds() {
        let invalid = [
            json!([{"run":{"executable":std::env::current_exe().unwrap(),"timeout_ms":0}}]),
            json!([{"run":{"executable":std::env::current_exe().unwrap(),"environment":{"Path":"one","PATH":"two"}}}]),
            json!([{"run":{"executable":std::env::current_exe().unwrap(),"args":vec!["argument";65]}}]),
        ];
        for preparation in invalid {
            assert_eq!(
                Executable::from_catalog(&catalog_launch(preparation)).unwrap_err(),
                "unsupported_launch_spec"
            );
        }
    }
}
