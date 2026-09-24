use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const MAX_PREPARATION_STEPS: usize = 8;
const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 10_000;
const MAX_COMMAND_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_AUTO_DETACH: bool = true;
const DEFAULT_WAIT_ALL: bool = true;
const DEFAULT_EXECUTABLE_EXIT_TIMEOUT_SECONDS: u64 = 5;
const DEFAULT_PLAYNITE_EXIT_TIMEOUT_SECONDS: u64 = 10;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Launch {
        operation_id: String,
        app_id: String,
        #[serde(default)]
        global_preparation: Vec<PreparationStep>,
    },
    LaunchExport {
        operation_id: String,
        app_id: String,
        #[serde(default)]
        global_preparation: Vec<PreparationStep>,
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
                global_preparation,
            }
            | Self::LaunchExport {
                operation_id,
                app_id,
                global_preparation,
            } => {
                if !valid_operation_id(operation_id) || !valid_app_id(app_id) {
                    return Err("invalid_request");
                }
                validate_preparation(global_preparation)?;
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
        #[serde(default, alias = "auto-detach")]
        auto_detach: Option<bool>,
        #[serde(default, alias = "wait-all")]
        wait_all: Option<bool>,
        #[serde(default, alias = "exit-timeout")]
        exit_timeout_seconds: Option<u64>,
        #[serde(default)]
        detached: Vec<String>,
        #[serde(default)]
        preparation: Vec<PreparationStep>,
    },
    Application {
        path: String,
    },
    Playnite {
        game_id: String,
        #[serde(default = "default_playnite_exit_timeout_seconds")]
        exit_timeout_seconds: u64,
    },
    PlayniteFullscreen {
        #[serde(default = "default_playnite_exit_timeout_seconds")]
        exit_timeout_seconds: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationStep {
    pub run: ConfiguredCommand,
    #[serde(default)]
    pub undo: Option<ConfiguredCommand>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

const MAX_EXIT_TIMEOUT_SECONDS: u64 = 300;
const MAX_DETACHED_COMMANDS: usize = 16;
const MAX_DETACHED_COMMAND_BYTES: usize = 2048;

#[derive(Clone, Debug)]
pub struct Executable {
    pub program: String,
    pub arguments: Vec<String>,
    pub working_directory: Option<String>,
    pub steam_id: Option<String>,
    pub steam_install_dir: Option<String>,
    pub auto_detach: bool,
    pub wait_all: bool,
    pub exit_timeout_seconds: u64,
    pub detached: Vec<String>,
    pub preparation: Vec<PreparationStep>,
}

#[derive(Clone, Debug)]
pub enum LaunchTarget {
    Executable(Executable),
    Playnite {
        game_id: String,
        exit_timeout_seconds: u64,
        preparation: Vec<PreparationStep>,
    },
    PlayniteFullscreen {
        exit_timeout_seconds: u64,
        preparation: Vec<PreparationStep>,
    },
}

fn default_playnite_exit_timeout_seconds() -> u64 {
    DEFAULT_PLAYNITE_EXIT_TIMEOUT_SECONDS
}

impl LaunchTarget {
    pub fn from_catalog_entry(
        entry: &Value,
        global_preparation: &[PreparationStep],
    ) -> Result<Self, &'static str> {
        let launch = entry.get("launch").ok_or("invalid_catalog_response")?;
        let exclude_global_preparation = catalog_excludes_global_preparation(entry)?;
        let steam_id = optional_catalog_string(entry, "steam_id")?;
        let steam_install_dir = optional_catalog_string(entry, "steam_install_dir")?;
        if steam_id
            .as_deref()
            .is_some_and(|value| !valid_steam_id(value))
            || steam_install_dir
                .as_deref()
                .is_some_and(|value| !valid_steam_install_directory(value))
        {
            return Err("invalid_catalog_response");
        }
        match Self::from_catalog(launch)? {
            Self::Executable(mut executable) => {
                if !exclude_global_preparation {
                    if global_preparation.len() + executable.preparation.len()
                        > MAX_PREPARATION_STEPS
                    {
                        return Err("preparation_limit_exceeded");
                    }
                    let application_working_directory = catalog_working_directory(launch)?;
                    let mut preparation = global_preparation.to_vec();
                    apply_preparation_working_directories(
                        &mut preparation,
                        application_working_directory.as_deref(),
                    );
                    preparation.extend(executable.preparation);
                    executable.preparation = preparation;
                }
                executable.steam_id = steam_id;
                executable.steam_install_dir = steam_install_dir;
                Ok(Self::Executable(executable))
            }
            Self::Playnite { .. } if steam_id.is_some() || steam_install_dir.is_some() => {
                Err("invalid_catalog_response")
            }
            Self::PlayniteFullscreen { .. }
                if steam_id.is_some() || steam_install_dir.is_some() =>
            {
                Err("invalid_catalog_response")
            }
            Self::Playnite {
                game_id,
                exit_timeout_seconds,
                ..
            } => Ok(Self::Playnite {
                game_id,
                exit_timeout_seconds,
                preparation: if exclude_global_preparation {
                    Vec::new()
                } else {
                    global_preparation.to_vec()
                },
            }),
            Self::PlayniteFullscreen {
                exit_timeout_seconds,
                ..
            } => Ok(Self::PlayniteFullscreen {
                exit_timeout_seconds,
                preparation: if exclude_global_preparation {
                    Vec::new()
                } else {
                    global_preparation.to_vec()
                },
            }),
        }
    }

    pub fn from_catalog(value: &Value) -> Result<Self, &'static str> {
        let launch: CatalogLaunch =
            serde_json::from_value(value.clone()).map_err(|_| "invalid_catalog_response")?;
        match launch {
            CatalogLaunch::Executable { .. } => {
                Executable::from_catalog(value).map(Self::Executable)
            }
            CatalogLaunch::Playnite {
                game_id,
                exit_timeout_seconds,
            } => {
                if !valid_guid(&game_id)
                    || !(1..=MAX_EXIT_TIMEOUT_SECONDS).contains(&exit_timeout_seconds)
                {
                    return Err("invalid_catalog_response");
                }
                Ok(Self::Playnite {
                    game_id: game_id.to_ascii_lowercase(),
                    exit_timeout_seconds,
                    preparation: Vec::new(),
                })
            }
            CatalogLaunch::PlayniteFullscreen {
                exit_timeout_seconds,
            } => {
                if exit_timeout_seconds > MAX_EXIT_TIMEOUT_SECONDS {
                    return Err("invalid_catalog_response");
                }
                Ok(Self::PlayniteFullscreen {
                    exit_timeout_seconds,
                    preparation: Vec::new(),
                })
            }
            CatalogLaunch::Application { .. } => Err("unsupported_launch_kind"),
        }
    }
}

fn catalog_excludes_global_preparation(entry: &Value) -> Result<bool, &'static str> {
    let canonical = entry.get("exclude_global_prep_cmd");
    let compatibility = entry.get("exclude-global-prep-cmd");
    let Some(value) = (match (canonical, compatibility) {
        (Some(_), Some(_)) => return Err("invalid_catalog_response"),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }) else {
        return Ok(false);
    };
    value.as_bool().ok_or("invalid_catalog_response")
}

fn catalog_working_directory(launch: &Value) -> Result<Option<String>, &'static str> {
    let canonical = launch.get("working_directory");
    let compatibility = launch.get("working-dir");
    let value = match (canonical, compatibility) {
        (Some(_), Some(_)) => return Err("invalid_catalog_response"),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(directory)) => Ok(Some(directory.clone())),
        Some(_) => Err("invalid_catalog_response"),
    }
}

fn optional_catalog_string(entry: &Value, key: &str) -> Result<Option<String>, &'static str> {
    match entry.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err("invalid_catalog_response"),
    }
}

fn valid_steam_id(value: &str) -> bool {
    value
        .parse::<u32>()
        .is_ok_and(|id| id != 0 && id.to_string() == value)
}

fn valid_steam_install_directory(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 1_024
        || value.trim() != value
        || value.chars().any(char::is_control)
        || value
            .split(['/', '\\'])
            .any(|component| matches!(component, "." | ".."))
    {
        return false;
    }
    let components = value
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let drive_rooted = value.len() >= 3
        && value.as_bytes()[0].is_ascii_alphabetic()
        && value.as_bytes()[1] == b':'
        && matches!(value.as_bytes()[2], b'/' | b'\\');
    let unc_rooted = value.starts_with("\\\\") || value.starts_with("//");
    let posix_rooted = value.starts_with('/') && !unc_rooted;
    (drive_rooted && components.len() > 1)
        || (unc_rooted && components.len() >= 3)
        || (posix_rooted && !components.is_empty())
}

fn valid_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
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
                auto_detach,
                wait_all,
                exit_timeout_seconds,
                detached,
                mut preparation,
            } => {
                let auto_detach = auto_detach.unwrap_or(DEFAULT_AUTO_DETACH);
                let wait_all = wait_all.unwrap_or(DEFAULT_WAIT_ALL);
                let exit_timeout_seconds =
                    exit_timeout_seconds.unwrap_or(DEFAULT_EXECUTABLE_EXIT_TIMEOUT_SECONDS);
                let working_directory = working_directory.filter(|directory| !directory.is_empty());
                if !valid_native_path(&program)
                    || !valid_arguments(&arguments)
                    || exit_timeout_seconds > MAX_EXIT_TIMEOUT_SECONDS
                    || detached.len() > MAX_DETACHED_COMMANDS
                    || detached.iter().any(|command| {
                        command.is_empty()
                            || command.len() > MAX_DETACHED_COMMAND_BYTES
                            || command.chars().any(char::is_control)
                    })
                    || working_directory
                        .as_deref()
                        .is_some_and(|cwd| !valid_native_path(cwd))
                {
                    return Err("unsupported_launch_spec");
                }
                // Match Vibeshine's command-directory fallback for this structured executable.
                let resolved_working_directory = working_directory
                    .clone()
                    .or_else(|| inferred_working_directory(&program));
                apply_preparation_working_directories(
                    &mut preparation,
                    working_directory.as_deref(),
                );
                if !valid_preparation(&preparation) {
                    return Err("unsupported_launch_spec");
                }
                Ok(Self {
                    program,
                    arguments,
                    working_directory: resolved_working_directory,
                    steam_id: None,
                    steam_install_dir: None,
                    auto_detach,
                    wait_all,
                    exit_timeout_seconds,
                    detached,
                    preparation,
                })
            }
            CatalogLaunch::Application { path } => {
                let _ = path;
                Err("unsupported_launch_kind")
            }
            CatalogLaunch::Playnite { .. } => Err("unsupported_launch_kind"),
            CatalogLaunch::PlayniteFullscreen { .. } => Err("unsupported_launch_kind"),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationConfigOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fec_percentage: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sink_capture_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back_button_timeout: Option<i32>,
}

impl ApplicationConfigOverrides {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.fec_percentage == Some(0) {
            return Err("invalid_catalog_response");
        }
        if self
            .audio_sink
            .as_deref()
            .is_some_and(|endpoint| endpoint.len() > 4096 || endpoint.chars().any(char::is_control))
        {
            return Err("invalid_catalog_response");
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.fec_percentage.is_none()
            && self.audio_sink.is_none()
            && self.audio_sink_capture_only.is_none()
            && self.back_button_timeout.is_none()
    }
}

fn apply_preparation_working_directories(
    preparation: &mut [PreparationStep],
    working_directory: Option<&str>,
) {
    for step in preparation {
        apply_command_working_directory(&mut step.run, working_directory);
        if let Some(undo) = &mut step.undo {
            apply_command_working_directory(undo, working_directory);
        }
    }
}

fn apply_command_working_directory(
    command: &mut ConfiguredCommand,
    application_directory: Option<&str>,
) {
    if command.cwd.is_none() {
        command.cwd = application_directory
            .map(str::to_owned)
            .or_else(|| inferred_working_directory(&command.executable));
    }
}

fn inferred_working_directory(executable: &str) -> Option<String> {
    Path::new(executable)
        .parent()
        .and_then(Path::to_str)
        .map(str::to_owned)
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

fn validate_preparation(steps: &[PreparationStep]) -> Result<(), &'static str> {
    if steps.len() > MAX_PREPARATION_STEPS {
        return Err("preparation_limit_exceeded");
    }
    if !steps.iter().all(|step| {
        valid_configured_command(&step.run)
            && step.undo.as_ref().is_none_or(valid_configured_command)
    }) {
        return Err("invalid_preparation");
    }
    Ok(())
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
            "auto_detach":true,
            "preparation":preparation,
        })
    }

    #[test]
    fn launch_admission_defaults_and_validates_global_preparation() {
        let omitted: Operation = serde_json::from_value(json!({
            "action":"launch",
            "operation_id":"omitted",
            "app_id":"app-0000000000000001",
        }))
        .unwrap();
        omitted.validate().unwrap();

        let executable = std::env::current_exe().unwrap();
        let valid: Operation = serde_json::from_value(json!({
            "action":"launch_export",
            "operation_id":"configured",
            "app_id":"app-0000000000000001",
            "global_preparation":[{
                "run":{"executable":executable},
                "undo":{"executable":std::env::current_exe().unwrap()},
            }],
        }))
        .unwrap();
        valid.validate().unwrap();

        let repeated_step = json!({
            "run":{"executable":std::env::current_exe().unwrap()},
        });
        let oversized: Operation = serde_json::from_value(json!({
            "action":"launch",
            "operation_id":"oversized",
            "app_id":"app-0000000000000001",
            "global_preparation":vec![repeated_step; MAX_PREPARATION_STEPS + 1],
        }))
        .unwrap();
        assert_eq!(oversized.validate(), Err("preparation_limit_exceeded"));

        let invalid: Operation = serde_json::from_value(json!({
            "action":"launch",
            "operation_id":"invalid",
            "app_id":"app-0000000000000001",
            "global_preparation":[{"run":{"executable":"relative-command"}}],
        }))
        .unwrap();
        assert_eq!(invalid.validate(), Err("invalid_preparation"));
    }

    #[test]
    fn application_capture_only_override_preserves_absence_and_explicit_false() {
        let inherited = ApplicationConfigOverrides::default();
        assert!(inherited.is_empty());
        assert_eq!(inherited.audio_sink_capture_only, None);
        assert_eq!(serde_json::to_value(inherited).unwrap(), json!({}));

        let disabled: ApplicationConfigOverrides = serde_json::from_value(json!({
            "audio_sink_capture_only":false
        }))
        .unwrap();
        disabled.validate().unwrap();
        assert_eq!(disabled.audio_sink_capture_only, Some(false));
        assert!(!disabled.is_empty());
        assert_eq!(
            serde_json::to_value(disabled).unwrap(),
            json!({"audio_sink_capture_only":false})
        );
        assert!(
            serde_json::from_value::<ApplicationConfigOverrides>(json!({
                "audio_sink_capture_only":"no"
            }))
            .is_err()
        );
    }

    #[test]
    fn application_back_button_timeout_override_preserves_absence_and_disable() {
        let inherited = ApplicationConfigOverrides::default();
        assert!(inherited.is_empty());
        assert_eq!(inherited.back_button_timeout, None);
        assert_eq!(serde_json::to_value(inherited).unwrap(), json!({}));

        for timeout in [-1, 0, 1250] {
            let selected: ApplicationConfigOverrides = serde_json::from_value(json!({
                "back_button_timeout":timeout
            }))
            .unwrap();
            selected.validate().unwrap();
            assert_eq!(selected.back_button_timeout, Some(timeout));
            assert_eq!(
                serde_json::to_value(selected).unwrap(),
                json!({"back_button_timeout":timeout})
            );
        }
        assert!(
            serde_json::from_value::<ApplicationConfigOverrides>(json!({
                "back_button_timeout":2147483648i64
            }))
            .is_err()
        );
    }

    #[test]
    fn catalog_preparation_is_strict_and_applies_defaults() {
        let inferred_directory = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut catalog_value = catalog_launch(json!([{
            "run":{"executable":std::env::current_exe().unwrap()},
        }]));
        catalog_value["working_directory"] = json!("");
        let executable = Executable::from_catalog(&catalog_value).unwrap();
        assert_eq!(executable.exit_timeout_seconds, 5);
        let command = &executable.preparation[0].run;
        assert_eq!(
            executable.working_directory.as_deref(),
            Some(inferred_directory.as_str())
        );
        assert_eq!(command.timeout_ms, DEFAULT_COMMAND_TIMEOUT_MS);
        assert!(command.args.is_empty());
        assert!(command.environment.is_empty());
        assert_eq!(command.cwd.as_deref(), Some(inferred_directory.as_str()));
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
    fn ordinary_executable_defaults_to_five_but_preserves_explicit_zero_and_playnite_ten() {
        let executable = std::env::current_exe().unwrap();
        let mut ordinary = json!({
            "kind":"executable",
            "program":executable,
            "arguments":[],
        });
        assert_eq!(
            Executable::from_catalog(&ordinary)
                .unwrap()
                .exit_timeout_seconds,
            5
        );
        ordinary["exit_timeout_seconds"] = json!(0);
        assert_eq!(
            Executable::from_catalog(&ordinary)
                .unwrap()
                .exit_timeout_seconds,
            0
        );

        let playnite = LaunchTarget::from_catalog(&json!({
            "kind":"playnite",
            "game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab",
        }))
        .unwrap();
        assert!(matches!(
            playnite,
            LaunchTarget::Playnite {
                exit_timeout_seconds: 10,
                ..
            }
        ));

        let fullscreen = LaunchTarget::from_catalog(&json!({
            "kind":"playnite_fullscreen",
        }))
        .unwrap();
        assert!(matches!(
            fullscreen,
            LaunchTarget::PlayniteFullscreen {
                exit_timeout_seconds: 10,
                ..
            }
        ));
        assert!(matches!(
            LaunchTarget::from_catalog(&json!({
                "kind":"playnite_fullscreen",
                "exit_timeout_seconds":0
            }))
            .unwrap(),
            LaunchTarget::PlayniteFullscreen {
                exit_timeout_seconds: 0,
                ..
            }
        ));
        assert!(matches!(
            LaunchTarget::from_catalog(&json!({
                "kind":"playnite_fullscreen",
                "exit_timeout_seconds":300
            }))
            .unwrap(),
            LaunchTarget::PlayniteFullscreen {
                exit_timeout_seconds: 300,
                ..
            }
        ));
        assert_eq!(
            LaunchTarget::from_catalog(&json!({
                "kind":"playnite_fullscreen",
                "exit_timeout_seconds":301
            }))
            .unwrap_err(),
            "invalid_catalog_response"
        );
        assert_eq!(
            LaunchTarget::from_catalog(&json!({
                "kind":"playnite_fullscreen",
                "game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"
            }))
            .unwrap_err(),
            "invalid_catalog_response"
        );
    }

    #[test]
    fn steam_runtime_metadata_is_read_from_catalog_entry_and_strictly_validated() {
        let entry = json!({
            "steam_id":"42",
            "steam_install_dir":"/games/space game",
            "launch":catalog_launch(json!([])),
        });
        let LaunchTarget::Executable(executable) =
            LaunchTarget::from_catalog_entry(&entry, &[]).unwrap()
        else {
            panic!("expected executable target");
        };
        assert_eq!(executable.steam_id.as_deref(), Some("42"));
        assert_eq!(
            executable.steam_install_dir.as_deref(),
            Some("/games/space game")
        );

        for steam_id in ["0", "01", "4294967296"] {
            let invalid = json!({
                "steam_id":steam_id,
                "launch":catalog_launch(json!([])),
            });
            assert_eq!(
                LaunchTarget::from_catalog_entry(&invalid, &[]).unwrap_err(),
                "invalid_catalog_response"
            );
        }
        for install_dir in ["\\\\server\\share", "\\foo", "/", "/games/../other"] {
            let invalid = json!({
                "steam_install_dir":install_dir,
                "launch":catalog_launch(json!([])),
            });
            assert_eq!(
                LaunchTarget::from_catalog_entry(&invalid, &[]).unwrap_err(),
                "invalid_catalog_response"
            );
        }

        let playnite = json!({
            "steam_id":"42",
            "launch":{"kind":"playnite","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}
        });
        assert_eq!(
            LaunchTarget::from_catalog_entry(&playnite, &[]).unwrap_err(),
            "invalid_catalog_response"
        );
    }

    fn preparation_step(executable: &str, argument: &str) -> PreparationStep {
        PreparationStep {
            run: ConfiguredCommand {
                executable: executable.into(),
                args: vec![argument.into()],
                environment: BTreeMap::new(),
                cwd: None,
                timeout_ms: DEFAULT_COMMAND_TIMEOUT_MS,
            },
            undo: None,
        }
    }

    #[test]
    fn global_preparation_precedes_app_steps_and_obeys_exclusion_alias() {
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let global = preparation_step(&executable, "global");
        let app = preparation_step(&executable, "app");
        let entry = json!({
            "launch": {
                "kind":"executable",
                "program":executable,
                "preparation":[app],
            },
        });
        let LaunchTarget::Executable(merged) =
            LaunchTarget::from_catalog_entry(&entry, std::slice::from_ref(&global)).unwrap()
        else {
            panic!("expected executable target");
        };
        assert_eq!(merged.preparation[0].run.args, ["global"]);
        assert_eq!(merged.preparation[1].run.args, ["app"]);

        let excluded = json!({
            "exclude-global-prep-cmd":true,
            "launch": {
                "kind":"executable",
                "program":std::env::current_exe().unwrap(),
                "preparation":[app],
            },
        });
        let LaunchTarget::Executable(excluded) =
            LaunchTarget::from_catalog_entry(&excluded, std::slice::from_ref(&global)).unwrap()
        else {
            panic!("expected executable target");
        };
        assert_eq!(excluded.preparation.len(), 1);
        assert_eq!(excluded.preparation[0].run.args, ["app"]);
    }

    #[test]
    fn global_preparation_bounds_and_ambiguous_exclusion_fields_fail_closed() {
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let global = preparation_step(&executable, "global");
        let app_steps = vec![preparation_step(&executable, "app"); MAX_PREPARATION_STEPS];
        let entry = json!({
            "launch": {
                "kind":"executable",
                "program":executable,
                "preparation":app_steps,
            },
        });
        assert_eq!(
            LaunchTarget::from_catalog_entry(&entry, std::slice::from_ref(&global)).unwrap_err(),
            "preparation_limit_exceeded"
        );

        let ambiguous = json!({
            "exclude_global_prep_cmd":false,
            "exclude-global-prep-cmd":false,
            "launch":{"kind":"executable","program":std::env::current_exe().unwrap()},
        });
        assert_eq!(
            LaunchTarget::from_catalog_entry(&ambiguous, &[]).unwrap_err(),
            "invalid_catalog_response"
        );
    }

    #[test]
    fn global_preparation_runs_for_playnite_targets_unless_excluded() {
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let global = preparation_step(&executable, "global");
        let playnite = json!({
            "launch":{"kind":"playnite","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}
        });
        let prepared =
            LaunchTarget::from_catalog_entry(&playnite, std::slice::from_ref(&global)).unwrap();
        assert!(matches!(
            prepared,
            LaunchTarget::Playnite { preparation, .. } if preparation == [global.clone()]
        ));
        let fullscreen = json!({
            "launch":{"kind":"playnite_fullscreen"}
        });
        let prepared_fullscreen =
            LaunchTarget::from_catalog_entry(&fullscreen, std::slice::from_ref(&global)).unwrap();
        assert!(matches!(
            prepared_fullscreen,
            LaunchTarget::PlayniteFullscreen { preparation, .. } if preparation == [global.clone()]
        ));
        let excluded = json!({
            "exclude_global_prep_cmd":true,
            "launch":{"kind":"playnite","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}
        });
        assert!(matches!(
            LaunchTarget::from_catalog_entry(&excluded, std::slice::from_ref(&global)),
            Ok(LaunchTarget::Playnite { preparation, .. }) if preparation.is_empty()
        ));
    }

    #[test]
    fn absent_application_directory_is_inferred_for_each_preparation_command() {
        let command_directory = std::env::temp_dir().join("amaterasu-preparation-command-bin");
        let undo_directory = std::env::temp_dir().join("amaterasu-undo-command-bin");
        let command_executable = command_directory.join("prepare.exe");
        let undo_executable = undo_directory.join("undo.exe");
        let value = catalog_launch(json!([{
            "run":{"executable":command_executable},
            "undo":{"executable":undo_executable},
        }]));

        let executable = Executable::from_catalog(&value).unwrap();
        assert_eq!(
            executable.preparation[0].run.cwd.as_deref(),
            Some(command_directory.to_str().unwrap())
        );
        assert_eq!(
            executable.preparation[0]
                .undo
                .as_ref()
                .unwrap()
                .cwd
                .as_deref(),
            Some(undo_directory.to_str().unwrap())
        );
    }

    #[test]
    fn catalog_working_directory_inherits_into_preparation_without_overriding_step_cwd() {
        let application_directory = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let run_directory = std::env::temp_dir().to_string_lossy().into_owned();
        let undo_directory = std::env::temp_dir()
            .join("amaterasu-explicit-undo-directory")
            .to_string_lossy()
            .into_owned();
        let mut value = catalog_launch(json!([
            {
                "run":{"executable":std::env::current_exe().unwrap()},
                "undo":{"executable":std::env::current_exe().unwrap()},
            },
            {
                "run":{"executable":std::env::current_exe().unwrap(),"cwd":run_directory},
                "undo":{"executable":std::env::current_exe().unwrap(),"cwd":undo_directory},
            }
        ]));
        value["working_directory"] = json!(application_directory);

        let executable = Executable::from_catalog(&value).unwrap();
        assert_eq!(
            executable.preparation[0].run.cwd.as_deref(),
            Some(application_directory.as_str())
        );
        assert_eq!(
            executable.preparation[0]
                .undo
                .as_ref()
                .unwrap()
                .cwd
                .as_deref(),
            Some(application_directory.as_str())
        );
        assert_eq!(
            executable.preparation[1].run.cwd.as_deref(),
            Some(run_directory.as_str())
        );
        assert_eq!(
            executable.preparation[1]
                .undo
                .as_ref()
                .unwrap()
                .cwd
                .as_deref(),
            Some(undo_directory.as_str())
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
