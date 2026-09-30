use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const MAX_PREPARATION_STEPS: usize = 8;
const MAX_RAW_PREPARATION_COMMAND_BYTES: usize = 2_048;
const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 10_000;
const MAX_COMMAND_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_AUTO_DETACH: bool = true;
const DEFAULT_WAIT_ALL: bool = true;
const DEFAULT_EXECUTABLE_EXIT_TIMEOUT_SECONDS: u64 = 10;
const DEFAULT_PLAYNITE_EXIT_TIMEOUT_SECONDS: u64 = 10;
const MAX_SEARCH_PATH_BYTES: usize = 16_384;
const MAX_SEARCH_PATH_ENTRIES: usize = 128;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    PreflightExport {
        operation_id: String,
        app_id: String,
        #[serde(default)]
        global_preparation: Vec<PreparationStep>,
        #[serde(default)]
        legacy_global_preparation: Vec<LegacyPreparationCommand>,
    },
    Launch {
        operation_id: String,
        app_id: String,
        #[serde(default)]
        global_preparation: Vec<PreparationStep>,
        #[serde(default)]
        legacy_global_preparation: Vec<LegacyPreparationCommand>,
    },
    LaunchExport {
        operation_id: String,
        app_id: String,
        #[serde(default)]
        preflight_token: Option<String>,
        #[serde(default)]
        display_admission_token: Option<String>,
        #[serde(default)]
        global_preparation: Vec<PreparationStep>,
        #[serde(default)]
        legacy_global_preparation: Vec<LegacyPreparationCommand>,
        #[serde(default)]
        frame_limiter_settings: Option<BTreeMap<String, Value>>,
        #[serde(default)]
        client_frame_limiter_overrides: Option<BTreeMap<String, Value>>,
        #[serde(default)]
        application_display_overrides: Option<BTreeMap<String, Value>>,
    },
    PrepareDisplayAdmission {
        application_operation_id: String,
        global_revision: Option<u64>,
        global_overrides: BTreeMap<String, Value>,
        catalog_revision: u64,
        application_display_overrides: BTreeMap<String, Value>,
        certificate_id: String,
        client_display_overrides: BTreeMap<String, Value>,
        rtsp_operation_id: String,
        request_sha256: String,
    },
    VerifyDisplayAdmission {
        token: String,
        application_operation_id: String,
        rtsp_operation_id: String,
        request_sha256: String,
        display_policy_snapshot: Value,
        certificate_id: String,
        catalog_revision: u64,
    },
    VerifyFrameLimiterSnapshot {
        application_operation_id: String,
        app_id: String,
        catalog_revision: u64,
        frame_limiter_settings: BTreeMap<String, Value>,
        frame_limiter_executable_path: String,
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
            Self::PreflightExport {
                operation_id,
                app_id,
                global_preparation,
                legacy_global_preparation,
            }
            | Self::Launch {
                operation_id,
                app_id,
                global_preparation,
                legacy_global_preparation,
            }
            | Self::LaunchExport {
                operation_id,
                app_id,
                global_preparation,
                legacy_global_preparation,
                ..
            } => {
                if !valid_operation_id(operation_id) || !valid_app_id(app_id) {
                    return Err("invalid_request");
                }
                validate_preparation(global_preparation)?;
                validate_legacy_preparation(legacy_global_preparation)?;
            }
            Self::Lookup { operation_id } if !valid_operation_id(operation_id) => {
                return Err("invalid_request");
            }
            Self::VerifyFrameLimiterSnapshot {
                application_operation_id,
                app_id,
                ..
            } if !valid_operation_id(application_operation_id) || !valid_app_id(app_id) => {
                return Err("invalid_request");
            }
            Self::PrepareDisplayAdmission {
                application_operation_id,
                global_revision: _,
                global_overrides,
                catalog_revision: _,
                application_display_overrides,
                certificate_id,
                client_display_overrides,
                rtsp_operation_id,
                request_sha256,
            } => {
                if !valid_operation_id(application_operation_id)
                    || !valid_operation_id(rtsp_operation_id)
                    || !valid_sha256(certificate_id)
                    || !valid_sha256(request_sha256)
                    || !valid_application_display_overrides(global_overrides)
                    || !valid_application_display_overrides(application_display_overrides)
                    || !valid_application_display_overrides(client_display_overrides)
                {
                    return Err("invalid_request");
                }
            }
            Self::VerifyDisplayAdmission {
                token,
                application_operation_id,
                rtsp_operation_id,
                request_sha256,
                display_policy_snapshot,
                certificate_id,
                catalog_revision: _,
            } => {
                if !valid_sha256(token)
                    || !valid_operation_id(application_operation_id)
                    || !valid_operation_id(rtsp_operation_id)
                    || !valid_sha256(request_sha256)
                    || !valid_sha256(certificate_id)
                    || !display_policy_snapshot.is_object()
                    || !serde_json::to_vec(display_policy_snapshot)
                        .is_ok_and(|bytes| bytes.len() <= 32_768)
                {
                    return Err("invalid_request");
                }
            }
            Self::List { limit, .. } if !(1..=16).contains(limit) => return Err("invalid_limit"),
            _ => {}
        }
        if let Self::LaunchExport {
            display_admission_token: Some(token),
            ..
        } = self
            && !valid_sha256(token)
        {
            return Err("invalid_request");
        }
        Ok(())
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
        detached: Vec<DetachedCommand>,
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
        #[serde(default, alias = "working-dir")]
        working_directory: Option<String>,
        #[serde(default)]
        preparation: Vec<PreparationStep>,
    },
    PlayniteFullscreen {
        #[serde(default = "default_playnite_exit_timeout_seconds")]
        exit_timeout_seconds: u64,
        #[serde(default, alias = "working-dir")]
        working_directory: Option<String>,
        #[serde(default)]
        preparation: Vec<PreparationStep>,
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
pub struct LegacyPreparationCommand {
    #[serde(default, rename = "do")]
    pub do_command: String,
    #[serde(default)]
    pub undo: String,
    #[serde(default)]
    pub elevated: bool,
}

/// Application-facing preparation preserves structured plans and the raw
/// Vibeshine command strings as distinct, untagged wire variants.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum PlannedPreparationStep {
    Structured(PreparationStep),
    Legacy(LegacyPreparationCommand),
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

/// Catalog entries retain both the original string and the typed per-command form.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DetachedCommand {
    Legacy(String),
    Configured(DetachedCommandSettings),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DetachedCommandSettings {
    pub command: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "working_directory"
    )]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

fn valid_detached_command(command: &DetachedCommand) -> bool {
    let settings = match command {
        DetachedCommand::Legacy(command) => return valid_detached_text(command),
        DetachedCommand::Configured(settings) => settings,
    };
    if !valid_detached_text(&settings.command)
        || settings.environment.len() > 32
        || settings
            .environment
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 4_096
        || [
            settings.cwd.as_deref(),
            settings.stdout.as_deref(),
            settings.stderr.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|path| !valid_native_path(path) || path.chars().any(char::is_control))
    {
        return false;
    }
    let mut keys = BTreeSet::new();
    settings.environment.iter().all(|(key, value)| {
        valid_environment_binding(key, value) && keys.insert(key.to_ascii_uppercase())
    })
}

fn valid_detached_text(command: &str) -> bool {
    !command.is_empty()
        && command.len() <= MAX_DETACHED_COMMAND_BYTES
        && !command.chars().any(char::is_control)
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
    /// The app-configured directory inherited by raw prep commands. This is
    /// separate because `working_directory` can also be inferred for launch.
    pub preparation_working_directory: Option<String>,
    pub steam_id: Option<String>,
    pub steam_install_dir: Option<String>,
    pub auto_detach: bool,
    pub wait_all: bool,
    pub exit_timeout_seconds: u64,
    pub elevated: bool,
    pub detached: Vec<DetachedCommand>,
    pub preparation: Vec<PlannedPreparationStep>,
}

#[derive(Clone, Debug)]
pub enum LaunchTarget {
    Executable(Executable),
    Playnite {
        game_id: String,
        exit_timeout_seconds: u64,
        working_directory: Option<String>,
        preparation: Vec<PlannedPreparationStep>,
    },
    PlayniteFullscreen {
        exit_timeout_seconds: u64,
        working_directory: Option<String>,
        preparation: Vec<PlannedPreparationStep>,
    },
}

fn default_playnite_exit_timeout_seconds() -> u64 {
    DEFAULT_PLAYNITE_EXIT_TIMEOUT_SECONDS
}

impl LaunchTarget {
    #[cfg(test)]
    pub fn from_catalog_entry(
        entry: &Value,
        global_preparation: &[PreparationStep],
    ) -> Result<Self, &'static str> {
        Self::from_catalog_entry_with_environment(entry, global_preparation, &BTreeMap::new())
    }

    #[cfg(test)]
    pub fn from_catalog_entry_with_environment(
        entry: &Value,
        global_preparation: &[PreparationStep],
        environment: &BTreeMap<String, String>,
    ) -> Result<Self, &'static str> {
        Self::from_catalog_entry_with_preparations(entry, global_preparation, &[], environment)
    }

    pub fn from_catalog_entry_with_preparations(
        entry: &Value,
        global_preparation: &[PreparationStep],
        legacy_global_preparation: &[LegacyPreparationCommand],
        environment: &BTreeMap<String, String>,
    ) -> Result<Self, &'static str> {
        let launch = entry.get("launch").ok_or("invalid_catalog_response")?;
        let elevated = catalog_elevated(entry)?;
        let exclude_global_preparation = catalog_excludes_global_preparation(entry)?;
        let legacy_application_preparation = catalog_legacy_preparation(entry)?;
        validate_preparation(global_preparation)?;
        validate_legacy_preparation(legacy_global_preparation)?;
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
        let application_working_directory = catalog_working_directory(launch, environment)?;
        match Self::from_catalog_with_environment(launch, environment)? {
            Self::Executable(mut executable) => {
                executable.preparation_working_directory = application_working_directory.clone();
                executable.preparation = merge_preparation_plan(
                    global_preparation,
                    legacy_global_preparation,
                    executable.preparation,
                    legacy_application_preparation,
                    !exclude_global_preparation,
                    application_working_directory.as_deref(),
                )?;
                executable.steam_id = steam_id;
                executable.steam_install_dir = steam_install_dir;
                executable.elevated = elevated;
                Ok(Self::Executable(executable))
            }
            Self::Playnite { .. } if elevated => Err("invalid_catalog_response"),
            Self::PlayniteFullscreen { .. } if elevated => Err("invalid_catalog_response"),
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
                working_directory,
                preparation,
            } => {
                let preparation = merge_preparation_plan(
                    global_preparation,
                    legacy_global_preparation,
                    preparation,
                    legacy_application_preparation,
                    !exclude_global_preparation,
                    working_directory.as_deref(),
                )?;
                Ok(Self::Playnite {
                    game_id,
                    exit_timeout_seconds,
                    working_directory,
                    preparation,
                })
            }
            Self::PlayniteFullscreen {
                exit_timeout_seconds,
                working_directory,
                preparation,
            } => {
                let preparation = merge_preparation_plan(
                    global_preparation,
                    legacy_global_preparation,
                    preparation,
                    legacy_application_preparation,
                    !exclude_global_preparation,
                    working_directory.as_deref(),
                )?;
                Ok(Self::PlayniteFullscreen {
                    exit_timeout_seconds,
                    working_directory,
                    preparation,
                })
            }
        }
    }

    #[cfg(test)]
    pub fn from_catalog(value: &Value) -> Result<Self, &'static str> {
        Self::from_catalog_with_environment(value, &BTreeMap::new())
    }

    fn from_catalog_with_environment(
        value: &Value,
        environment: &BTreeMap<String, String>,
    ) -> Result<Self, &'static str> {
        let launch: CatalogLaunch =
            serde_json::from_value(value.clone()).map_err(|_| "invalid_catalog_response")?;
        match launch {
            CatalogLaunch::Executable { .. } => {
                Executable::from_catalog_with_environment(value, environment).map(Self::Executable)
            }
            CatalogLaunch::Playnite {
                game_id,
                exit_timeout_seconds,
                working_directory,
                preparation,
            } => {
                let working_directory = working_directory
                    .as_deref()
                    .map(|directory| resolve_working_directory(directory, environment))
                    .transpose()?
                    .flatten();
                if !valid_guid(&game_id)
                    || !(1..=MAX_EXIT_TIMEOUT_SECONDS).contains(&exit_timeout_seconds)
                    || !valid_preparation(&preparation)
                {
                    return Err("invalid_catalog_response");
                }
                Ok(Self::Playnite {
                    game_id: game_id.to_ascii_lowercase(),
                    exit_timeout_seconds,
                    working_directory,
                    preparation: preparation
                        .into_iter()
                        .map(PlannedPreparationStep::Structured)
                        .collect(),
                })
            }
            CatalogLaunch::PlayniteFullscreen {
                exit_timeout_seconds,
                working_directory,
                preparation,
            } => {
                let working_directory = working_directory
                    .as_deref()
                    .map(|directory| resolve_working_directory(directory, environment))
                    .transpose()?
                    .flatten();
                if exit_timeout_seconds > MAX_EXIT_TIMEOUT_SECONDS
                    || !valid_preparation(&preparation)
                {
                    return Err("invalid_catalog_response");
                }
                Ok(Self::PlayniteFullscreen {
                    exit_timeout_seconds,
                    working_directory,
                    preparation: preparation
                        .into_iter()
                        .map(PlannedPreparationStep::Structured)
                        .collect(),
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

fn catalog_legacy_preparation(
    entry: &Value,
) -> Result<Vec<LegacyPreparationCommand>, &'static str> {
    let canonical = entry.get("prep_cmd");
    let compatibility = entry.get("prep-cmd");
    let Some(value) = (match (canonical, compatibility) {
        (Some(_), Some(_)) => return Err("invalid_catalog_response"),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }) else {
        return Ok(Vec::new());
    };
    let preparation: Vec<LegacyPreparationCommand> =
        serde_json::from_value(value.clone()).map_err(|_| "invalid_catalog_response")?;
    validate_legacy_preparation(&preparation).map_err(|_| "invalid_catalog_response")?;
    Ok(preparation)
}

fn merge_preparation_plan(
    global_preparation: &[PreparationStep],
    legacy_global_preparation: &[LegacyPreparationCommand],
    application_preparation: Vec<PlannedPreparationStep>,
    legacy_application_preparation: Vec<LegacyPreparationCommand>,
    include_global_preparation: bool,
    application_working_directory: Option<&str>,
) -> Result<Vec<PlannedPreparationStep>, &'static str> {
    let application_step_count =
        application_preparation.len() + legacy_application_preparation.len();
    let global_step_count = if include_global_preparation {
        global_preparation.len() + legacy_global_preparation.len()
    } else {
        0
    };
    if application_step_count + global_step_count > MAX_PREPARATION_STEPS {
        return Err("preparation_limit_exceeded");
    }

    let mut plan = Vec::with_capacity(application_step_count + global_step_count);
    if include_global_preparation {
        let mut structured_global = global_preparation.to_vec();
        apply_preparation_working_directories(
            &mut structured_global,
            application_working_directory,
        );
        plan.extend(
            structured_global
                .into_iter()
                .map(PlannedPreparationStep::Structured),
        );
        plan.extend(
            legacy_global_preparation
                .iter()
                .cloned()
                .map(PlannedPreparationStep::Legacy),
        );
    }
    plan.extend(application_preparation.into_iter().map(|mut step| {
        if let PlannedPreparationStep::Structured(structured_step) = &mut step {
            apply_preparation_working_directories(
                std::slice::from_mut(structured_step),
                application_working_directory,
            );
        }
        step
    }));
    plan.extend(
        legacy_application_preparation
            .into_iter()
            .map(PlannedPreparationStep::Legacy),
    );
    Ok(plan)
}

fn validate_legacy_preparation(steps: &[LegacyPreparationCommand]) -> Result<(), &'static str> {
    if steps.len() > MAX_PREPARATION_STEPS
        || steps.iter().any(|step| {
            [step.do_command.as_str(), step.undo.as_str()]
                .iter()
                .any(|command| {
                    command.len() > MAX_RAW_PREPARATION_COMMAND_BYTES || command.contains('\0')
                })
        })
    {
        return Err("invalid_legacy_preparation");
    }
    Ok(())
}

fn catalog_elevated(entry: &Value) -> Result<bool, &'static str> {
    match entry.get("elevated") {
        None => Ok(false),
        Some(Value::Bool(elevated)) => Ok(*elevated),
        Some(_) => Err("invalid_catalog_response"),
    }
}

/// Output is resolved once for the retained launch before any session or child
/// mutation. Relative filenames use the host directory, as the reference's
/// append-open does, independently of the application's working directory.
pub(crate) fn resolve_catalog_output(
    entry: &Value,
    environment: &BTreeMap<String, String>,
) -> Result<Option<String>, &'static str> {
    let Some(configured) = entry.get("output").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let configured = configured.as_str().ok_or("invalid_catalog_response")?;
    if configured.len() > 1_024 || configured.chars().any(char::is_control) {
        return Err("invalid_catalog_response");
    }
    let expanded = expand_environment_references(configured, environment)?;
    if expanded.is_empty() || expanded == "null" {
        return Ok(None);
    }
    if expanded.chars().any(char::is_control) {
        return Err("unsupported_launch_spec");
    }
    let path = std::path::Path::new(&expanded);
    let resolved = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|_| "unsupported_launch_spec")?
            .join(path)
    };
    let resolved = resolved.to_str().ok_or("unsupported_launch_spec")?;
    if resolved.len() > 1_024 || resolved.chars().any(char::is_control) {
        return Err("unsupported_launch_spec");
    }
    Ok(Some(resolved.to_owned()))
}

fn catalog_working_directory(
    launch: &Value,
    environment: &BTreeMap<String, String>,
) -> Result<Option<String>, &'static str> {
    let canonical = launch.get("working_directory");
    let compatibility = launch.get("working-dir");
    let value = match (canonical, compatibility) {
        (Some(_), Some(_)) => return Err("invalid_catalog_response"),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(directory)) => resolve_working_directory(directory, environment),
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

fn resolve_executable(
    configured_value: &str,
    environment: &BTreeMap<String, String>,
    working_directory: Option<&str>,
) -> Result<String, &'static str> {
    let expanded = expand_environment_references(configured_value, environment)?;
    let executable = trim_path_quotes(&expanded);
    if executable.is_empty() || executable.len() > 1_024 || executable.chars().any(char::is_control)
    {
        return Err("unsupported_launch_spec");
    }

    let executable_path = Path::new(executable);
    let resolved = if executable_path.is_absolute() {
        executable.to_owned()
    } else if is_bare_command(executable) {
        find_path_executable(executable, environment, working_directory)
            .and_then(|path| path.to_str().map(str::to_owned))
            .ok_or("unsupported_launch_spec")?
    } else {
        return Err("unsupported_launch_spec");
    };
    if !valid_native_path(&resolved) || resolved.chars().any(char::is_control) {
        return Err("unsupported_launch_spec");
    }
    Ok(resolved)
}

fn resolve_working_directory(
    configured_value: &str,
    environment: &BTreeMap<String, String>,
) -> Result<Option<String>, &'static str> {
    let expanded = expand_environment_references(configured_value, environment)?;
    let directory = trim_path_quotes(&expanded);
    if directory.is_empty() {
        return Ok(None);
    }
    if !valid_native_path(directory) || directory.chars().any(char::is_control) {
        return Err("unsupported_launch_spec");
    }
    Ok(Some(directory.to_owned()))
}

fn expand_environment_references(
    value: &str,
    configured_environment: &BTreeMap<String, String>,
) -> Result<String, &'static str> {
    let mut expanded = String::new();
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '$' {
            let mut buffer = [0; 4];
            append_bounded(&mut expanded, character.encode_utf8(&mut buffer))?;
            continue;
        }

        match characters.peek() {
            Some('(') => {
                characters.next();
                let mut name = String::new();
                let mut closed = false;
                for character in characters.by_ref() {
                    match character {
                        ')' => {
                            closed = true;
                            break;
                        }
                        '(' => return Err("unsupported_launch_spec"),
                        _ => {
                            name.push(character);
                            if name.len() > 128 {
                                return Err("unsupported_launch_spec");
                            }
                        }
                    }
                }
                if !closed
                    || name.is_empty()
                    || name.len() > 128
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    return Err("unsupported_launch_spec");
                }
                let value = environment_value(&name, configured_environment).unwrap_or_default();
                append_bounded(&mut expanded, &value)?;
            }
            Some('$') => {
                characters.next();
                append_bounded(&mut expanded, "$")?;
            }
            _ => append_bounded(&mut expanded, "$")?,
        }
    }
    Ok(expanded)
}

fn append_bounded(output: &mut String, value: &str) -> Result<(), &'static str> {
    if output.len().saturating_add(value.len()) > 1_024 {
        return Err("unsupported_launch_spec");
    }
    output.push_str(value);
    Ok(())
}

fn environment_value(
    name: &str,
    configured_environment: &BTreeMap<String, String>,
) -> Option<String> {
    #[cfg(windows)]
    let configured = configured_environment
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone());
    #[cfg(not(windows))]
    let configured = configured_environment.get(name).cloned();

    configured.or_else(|| std::env::var(name).ok())
}

fn path_environment_value(configured_environment: &BTreeMap<String, String>) -> Option<OsString> {
    #[cfg(windows)]
    let configured = configured_environment
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| OsString::from(value));
    #[cfg(not(windows))]
    let configured = configured_environment.get("PATH").map(OsString::from);

    configured.or_else(|| std::env::var_os("PATH"))
}

fn find_path_executable(
    executable: &str,
    configured_environment: &BTreeMap<String, String>,
    working_directory: Option<&str>,
) -> Option<PathBuf> {
    let search_path = path_environment_value(configured_environment)?;
    if search_path.len() > MAX_SEARCH_PATH_BYTES {
        return None;
    }
    let directories = std::env::split_paths(&search_path)
        .take(MAX_SEARCH_PATH_ENTRIES + 1)
        .collect::<Vec<_>>();
    if directories.len() > MAX_SEARCH_PATH_ENTRIES {
        return None;
    }
    let candidates = path_executable_names(executable);
    let relative_directory = working_directory
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());

    directories.into_iter().find_map(|directory| {
        // Resolve relative and empty entries once, before process execution,
        // so later directory changes cannot select a different executable.
        let directory = if directory.is_absolute() {
            directory
        } else {
            relative_directory.as_ref()?.join(directory)
        };
        candidates.iter().find_map(|name| {
            let candidate = directory.join(name);
            if is_executable_file(&candidate) {
                if candidate.to_str().is_some_and(|path| path.len() <= 1_024) {
                    return Some(candidate);
                }
                None
            } else {
                None
            }
        })
    })
}

#[cfg(windows)]
fn path_executable_names(executable: &str) -> Vec<OsString> {
    if Path::new(executable).extension().is_some() {
        vec![OsString::from(executable)]
    } else {
        [".com", ".exe"]
            .into_iter()
            .map(|extension| OsString::from(format!("{executable}{extension}")))
            .collect()
    }
}

#[cfg(not(windows))]
fn path_executable_names(executable: &str) -> Vec<OsString> {
    vec![OsString::from(executable)]
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(any(windows, unix)))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn is_bare_command(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains(':')
}

fn trim_path_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'"' | b'\'') && bytes.last() == Some(&bytes[0]) {
        &value[1..value.len() - 1]
    } else {
        value
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
    #[cfg(test)]
    pub fn from_catalog(value: &Value) -> Result<Self, &'static str> {
        Self::from_catalog_with_environment(value, &BTreeMap::new())
    }

    fn from_catalog_with_environment(
        value: &Value,
        environment: &BTreeMap<String, String>,
    ) -> Result<Self, &'static str> {
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
                let working_directory = working_directory
                    .as_deref()
                    .map(|directory| resolve_working_directory(directory, environment))
                    .transpose()?
                    .flatten();
                let program =
                    resolve_executable(&program, environment, working_directory.as_deref())?;
                if !valid_native_path(&program)
                    || !valid_arguments(&arguments)
                    || exit_timeout_seconds > MAX_EXIT_TIMEOUT_SECONDS
                    || detached.len() > MAX_DETACHED_COMMANDS
                    || detached
                        .iter()
                        .any(|command| !valid_detached_command(command))
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
                    preparation_working_directory: None,
                    steam_id: None,
                    steam_install_dir: None,
                    auto_detach,
                    wait_all,
                    exit_timeout_seconds,
                    elevated: false,
                    detached,
                    preparation: preparation
                        .into_iter()
                        .map(PlannedPreparationStep::Structured)
                        .collect(),
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

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationConfigOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sw_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sw_tune: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder: Option<amaterasu_module_sdk::EncoderBackend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fec_percentage: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bitrate: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_fps_target: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_max_batch_size_kb: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_mode_remapping: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_resolution_option: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_manual_resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_refresh_rate_option: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_manual_refresh_rate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_hdr_option: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_configuration_option: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dd_hdr_request_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_audio: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sink_capture_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back_button_timeout: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amd_av1_latency_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvenc_spatial_aq: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_pen_touch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamepad: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high_resolution_scrolling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always_send_scancodes: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_rightalt_to_key_win: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keybindings: Option<amaterasu_input_policy::KeyBindings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_repeat_delay: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_repeat_frequency: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_enable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_fps_limit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_limiter_disable_vsync: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_auto_virtual_framegen"
    )]
    pub frame_limiter_auto_virtual_framegen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lossless_scaling_framegen: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lossless_scaling_target_fps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_generation_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_generation_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtss_frame_limit_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ds4_back_as_touchpad_click: Option<bool>,
}

fn deserialize_auto_virtual_framegen<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(Value::Bool(enabled)) => Ok(Some(if enabled {
            "enabled".into()
        } else {
            "disabled".into()
        })),
        Some(Value::String(value)) if value == "true" => Ok(Some("enabled".into())),
        Some(Value::String(value)) if value == "false" => Ok(Some("disabled".into())),
        Some(Value::String(value))
            if matches!(value.as_str(), "enabled" | "vrr" | "legacy" | "disabled") =>
        {
            Ok(Some(value))
        }
        _ => Err(serde::de::Error::custom(
            "frame_limiter_auto_virtual_framegen has an invalid mode",
        )),
    }
}

impl ApplicationConfigOverrides {
    pub fn validate(&self) -> Result<(), &'static str> {
        amaterasu_module_sdk::validate_software_options(
            self.sw_preset.as_deref(),
            self.sw_tune.as_deref(),
        )
        .map_err(|_| "invalid_config_overrides")?;
        if self
            .key_repeat_delay
            .is_some_and(|delay| delay > i32::MAX as u32)
            || self
                .key_repeat_frequency
                .is_some_and(|frequency| !frequency.is_finite() || frequency < 0.0)
        {
            return Err("invalid_catalog_response");
        }
        if self.fec_percentage == Some(0) {
            return Err("invalid_catalog_response");
        }
        if !valid_application_display_overrides(&self.display_overrides()) {
            return Err("invalid_catalog_response");
        }
        if self
            .minimum_fps_target
            .is_some_and(|target| !target.is_finite() || !(0.0..=1000.0).contains(&target))
            || self.video_max_batch_size_kb.is_some_and(|size| size > 64)
        {
            return Err("invalid_catalog_response");
        }
        if self
            .frame_limiter_fps_limit
            .is_some_and(|limit| !valid_millihertz_value(limit))
            || self
                .frame_limiter_provider
                .as_deref()
                .is_some_and(|provider| !valid_frame_limiter_provider(provider))
            || self
                .frame_limiter_auto_virtual_framegen
                .as_deref()
                .is_some_and(|mode| !matches!(mode, "enabled" | "vrr" | "legacy" | "disabled"))
            || self
                .rtss_frame_limit_type
                .as_deref()
                .is_some_and(|limit_type| {
                    !cfg!(windows)
                        || !matches!(
                            limit_type,
                            "async" | "front edge sync" | "back edge sync" | "nvidia reflex"
                        )
                })
        {
            return Err("invalid_catalog_response");
        }
        if self
            .lossless_scaling_target_fps
            .is_some_and(|fps| !(1..=1000).contains(&fps))
        {
            return Err("invalid_catalog_response");
        }
        if self
            .frame_generation_provider
            .as_deref()
            .is_some_and(|provider| {
                !matches!(
                    provider,
                    "lossless-scaling" | "game-provided" | "nvidia-smooth-motion"
                )
            })
            || self.frame_generation_mode.as_deref().is_some_and(|mode| {
                !matches!(
                    mode,
                    "off" | "lossless-scaling" | "game-provided" | "nvidia-smooth-motion"
                )
            })
        {
            return Err("invalid_catalog_response");
        }
        if self
            .audio_sink
            .as_deref()
            .is_some_and(|endpoint| endpoint.len() > 4096 || endpoint.chars().any(char::is_control))
        {
            return Err("invalid_catalog_response");
        }
        if self
            .gamepad
            .as_deref()
            .is_some_and(|profile| profile.len() > 32 || !valid_gamepad_profile(profile))
        {
            return Err("invalid_catalog_response");
        }
        if self.amd_av1_latency_mode.as_deref().is_some_and(|mode| {
            !matches!(
                mode,
                "auto" | "none" | "power_saving" | "realtime" | "lowest"
            )
        }) {
            return Err("invalid_catalog_response");
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.sw_preset.is_none()
            && self.sw_tune.is_none()
            && self.encoder.is_none()
            && self.fec_percentage.is_none()
            && self.max_bitrate.is_none()
            && self.minimum_fps_target.is_none()
            && self.video_max_batch_size_kb.is_none()
            && self.dd_mode_remapping.is_none()
            && self.dd_resolution_option.is_none()
            && self.dd_manual_resolution.is_none()
            && self.dd_refresh_rate_option.is_none()
            && self.dd_manual_refresh_rate.is_none()
            && self.dd_hdr_option.is_none()
            && self.dd_configuration_option.is_none()
            && self.dd_hdr_request_override.is_none()
            && self.stream_audio.is_none()
            && self.audio_sink.is_none()
            && self.audio_sink_capture_only.is_none()
            && self.back_button_timeout.is_none()
            && self.amd_av1_latency_mode.is_none()
            && self.nvenc_spatial_aq.is_none()
            && self.native_pen_touch.is_none()
            && self.controller.is_none()
            && self.gamepad.is_none()
            && self.keyboard.is_none()
            && self.mouse.is_none()
            && self.high_resolution_scrolling.is_none()
            && self.always_send_scancodes.is_none()
            && self.key_rightalt_to_key_win.is_none()
            && self.keybindings.is_none()
            && self.key_repeat_delay.is_none()
            && self.key_repeat_frequency.is_none()
            && self.frame_limiter_enable.is_none()
            && self.frame_limiter_fps_limit.is_none()
            && self.frame_limiter_provider.is_none()
            && self.frame_limiter_disable_vsync.is_none()
            && self.frame_limiter_auto_virtual_framegen.is_none()
            && self.lossless_scaling_framegen.is_none()
            && self.lossless_scaling_target_fps.is_none()
            && self.frame_generation_provider.is_none()
            && self.frame_generation_mode.is_none()
            && self.rtss_frame_limit_type.is_none()
            && self.ds4_back_as_touchpad_click.is_none()
    }
}

impl ApplicationConfigOverrides {
    pub fn display_overrides(&self) -> BTreeMap<String, Value> {
        let value = serde_json::to_value(self).expect("serializable application overrides");
        let object = value.as_object().expect("application overrides object");
        object
            .iter()
            .filter(|(key, _)| is_display_override_key(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

fn is_display_override_key(key: &str) -> bool {
    matches!(
        key,
        "dd_mode_remapping"
            | "dd_resolution_option"
            | "dd_manual_resolution"
            | "dd_refresh_rate_option"
            | "dd_manual_refresh_rate"
            | "dd_hdr_option"
            | "dd_configuration_option"
            | "dd_hdr_request_override"
    )
}

pub fn valid_application_display_overrides(overrides: &BTreeMap<String, Value>) -> bool {
    if overrides.keys().any(|key| !is_display_override_key(key))
        || overrides.values().any(Value::is_null)
    {
        return false;
    }
    let value = serde_json::to_value(overrides).expect("serializable display map");
    let Ok(fields) = serde_json::from_value::<ApplicationConfigOverrides>(value) else {
        return false;
    };
    if fields
        .dd_refresh_rate_option
        .as_deref()
        .is_some_and(|value| !matches!(value, "auto" | "disabled" | "manual" | "prefer_highest"))
        || fields
            .dd_hdr_request_override
            .as_deref()
            .is_some_and(|value| !matches!(value, "auto" | "force_on" | "force_off"))
        || fields
            .dd_resolution_option
            .as_deref()
            .is_some_and(|value| !matches!(value, "auto" | "disabled" | "manual"))
        || fields
            .dd_hdr_option
            .as_deref()
            .is_some_and(|value| !matches!(value, "auto" | "disabled"))
        || fields
            .dd_configuration_option
            .as_deref()
            .is_some_and(|value| {
                !matches!(
                    value,
                    "disabled"
                        | "verify_only"
                        | "ensure_active"
                        | "ensure_primary"
                        | "ensure_only_display"
                )
            })
    {
        return false;
    }
    if fields.dd_manual_resolution.as_deref().is_some_and(|value| {
        if value.len() > 32 {
            return true;
        }
        let value = value.trim();
        if value.is_empty() {
            return false;
        }
        value.split_once(['x', '×']).is_none_or(|(width, height)| {
            width.parse::<u32>().is_err() || height.parse::<u32>().is_err()
        })
    }) || fields
        .dd_manual_refresh_rate
        .as_deref()
        .is_some_and(|value| !valid_manual_refresh_rate(value))
    {
        return false;
    }
    let Some(remapping) = fields.dd_mode_remapping else {
        return true;
    };
    let Some(groups) = remapping.as_object() else {
        return false;
    };
    groups.len() <= 3
        && groups.iter().all(|(key, group)| {
            matches!(
                key.as_str(),
                "mixed" | "resolution_only" | "refresh_rate_only"
            ) && group.as_array().is_some_and(|entries| {
                entries.len() <= 128
                    && entries.iter().all(|entry| {
                        entry.as_object().is_some_and(|fields| {
                            fields.len() <= 4
                                && fields.iter().all(|(key, value)| {
                                    matches!(
                                        key.as_str(),
                                        "requested_resolution"
                                            | "requested_fps"
                                            | "final_resolution"
                                            | "final_refresh_rate"
                                    ) && value.as_str().is_some_and(|text| {
                                        text.len() <= 64 && !text.chars().any(char::is_control)
                                    })
                                })
                                && valid_remapping_entry(key, fields)
                        })
                    })
            })
        })
}

fn valid_remapping_entry(kind: &str, fields: &serde_json::Map<String, Value>) -> bool {
    let field = |name| fields.get(name).and_then(Value::as_str).unwrap_or("");
    let final_resolution = field("final_resolution");
    let final_refresh_rate = field("final_refresh_rate");
    valid_remapping_resolution(field("requested_resolution"))
        && valid_remapping_resolution(final_resolution)
        && valid_remapping_rate(field("requested_fps"), false)
        && valid_remapping_rate(final_refresh_rate, true)
        && match kind {
            "mixed" => !final_resolution.trim().is_empty() || !final_refresh_rate.trim().is_empty(),
            "resolution_only" => !final_resolution.trim().is_empty(),
            "refresh_rate_only" => !final_refresh_rate.trim().is_empty(),
            _ => false,
        }
}

fn valid_remapping_resolution(value: &str) -> bool {
    let value = value.trim().replace('×', "x");
    if value.is_empty() {
        return true;
    }
    value.split_once('x').is_some_and(|(width, height)| {
        !width.is_empty()
            && !height.is_empty()
            && width.bytes().all(|byte| byte.is_ascii_digit())
            && height.bytes().all(|byte| byte.is_ascii_digit())
            && width.parse::<u32>().is_ok()
            && height.parse::<u32>().is_ok()
    })
}

fn valid_remapping_rate(value: &str, allow_fraction: bool) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    let Some((whole, fraction)) = value.split_once('.') else {
        return value.bytes().all(|byte| byte.is_ascii_digit()) && value.parse::<u32>().is_ok();
    };
    if !allow_fraction
        || whole.is_empty()
        || fraction.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        return whole.parse::<u32>().is_ok();
    }
    format!("{whole}{fraction}").parse::<u32>().is_ok()
        && u32::try_from(fraction.len())
            .ok()
            .and_then(|length| 10_u32.checked_pow(length))
            .is_some()
}

fn valid_manual_refresh_rate(value: &str) -> bool {
    if value.len() > 64 {
        return false;
    }
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || (!fraction.is_empty() && !fraction.bytes().all(|byte| byte.is_ascii_digit()))
        || (value.contains('.') && fraction.is_empty())
    {
        return false;
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let Ok(integer) = whole.parse::<u32>() else {
        return false;
    };
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        return true;
    }
    if fraction.len() > 31 {
        return false;
    }
    let (Ok(numerator), Some(denominator)) = (
        fraction.parse::<u128>(),
        10_u128.checked_pow(fraction.len() as u32),
    ) else {
        return false;
    };
    let mut left = numerator;
    let mut right = denominator;
    while right != 0 {
        (left, right) = (right, left % right);
    }
    let numerator = numerator / left;
    let denominator = denominator / left;
    u32::try_from(denominator).is_ok()
        && u128::from(integer)
            .checked_mul(denominator)
            .and_then(|scaled| scaled.checked_add(numerator))
            .is_some_and(|combined| u32::try_from(combined).is_ok())
}

fn valid_millihertz_value(value: f64) -> bool {
    if !value.is_finite() || !(0.0..=1000.0).contains(&value) {
        return false;
    }
    let millihertz = value * 1000.0;
    let rounding_tolerance = f64::EPSILON * millihertz.abs().max(1.0) * 4.0;
    (millihertz - millihertz.round()).abs() <= rounding_tolerance
}

fn valid_frame_limiter_provider(provider: &str) -> bool {
    #[cfg(windows)]
    {
        matches!(provider, "auto" | "rtss" | "nvidia-control-panel" | "none")
    }
    #[cfg(target_os = "linux")]
    {
        matches!(
            provider,
            "auto" | "mangohud" | "proton" | "mangohud-proton" | "none"
        )
    }
    #[cfg(target_os = "macos")]
    {
        provider == "auto"
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        provider == "auto"
    }
}

fn valid_gamepad_profile(profile: &str) -> bool {
    #[cfg(windows)]
    {
        matches!(
            profile,
            "auto"
                | "x360"
                | "vhf"
                | "vhf_xbox"
                | "vhf_xbox_one"
                | "vhf_ds4"
                | "vhf_ds5"
                | "vhf_switch"
                | "ds4"
        )
    }
    #[cfg(target_os = "linux")]
    {
        matches!(profile, "auto" | "xone" | "ds4" | "ds5" | "switch")
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        profile == "auto"
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
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn display_remapping_rejects_rules_that_cannot_resolve() {
        for rules in [
            json!({"mixed":[{}]}),
            json!({"resolution_only":[{"final_refresh_rate":"60"}]}),
            json!({"refresh_rate_only":[{"final_resolution":"1920x1080"}]}),
            json!({"mixed":[{"requested_fps":"59.94","final_resolution":"1920x1080"}]}),
            json!({"mixed":[{"requested_resolution":"bad","final_resolution":"1920x1080"}]}),
            json!({"mixed":[{"final_refresh_rate":"4294967296"}]}),
        ] {
            let overrides: ApplicationConfigOverrides =
                serde_json::from_value(json!({"dd_mode_remapping":rules})).unwrap();
            assert_eq!(overrides.validate(), Err("invalid_catalog_response"));
        }
        let valid: ApplicationConfigOverrides = serde_json::from_value(json!({
            "dd_mode_remapping":{"mixed":[{"requested_fps":"60","final_resolution":"1920x1080","final_refresh_rate":"59.94"}]}
        })).unwrap();
        valid.validate().unwrap();
    }

    #[test]
    fn manual_refresh_rate_requires_an_exact_representable_rational() {
        for rate in ["0.0000000001", "4294967296", "59.1234567890", "1."] {
            let overrides: ApplicationConfigOverrides =
                serde_json::from_value(json!({"dd_manual_refresh_rate":rate})).unwrap();
            assert_eq!(
                overrides.validate(),
                Err("invalid_catalog_response"),
                "{rate}"
            );
        }
        for rate in ["", "59.94", "60", "00060.000"] {
            let overrides: ApplicationConfigOverrides =
                serde_json::from_value(json!({"dd_manual_refresh_rate":rate})).unwrap();
            overrides.validate().unwrap();
        }
    }

    fn structured_step(step: &PlannedPreparationStep) -> &PreparationStep {
        match step {
            PlannedPreparationStep::Structured(step) => step,
            PlannedPreparationStep::Legacy(_) => panic!("expected a structured preparation step"),
        }
    }

    fn unique_test_directory(label: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("{label}-{}-{timestamp}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        directory
    }

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
        let Operation::Launch {
            legacy_global_preparation,
            ..
        } = omitted
        else {
            panic!("expected launch operation");
        };
        assert!(legacy_global_preparation.is_empty());

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

        let legacy: Operation = serde_json::from_value(json!({
            "action":"launch",
            "operation_id":"raw-configured",
            "app_id":"app-0000000000000001",
            "legacy_global_preparation":[{"do":"  raw command  ","undo":"undo", "elevated":true}],
        }))
        .unwrap();
        legacy.validate().unwrap();
        let Operation::Launch {
            legacy_global_preparation,
            ..
        } = legacy
        else {
            panic!("expected launch operation");
        };
        assert_eq!(legacy_global_preparation[0].do_command, "  raw command  ");
        assert_eq!(legacy_global_preparation[0].undo, "undo");
        assert!(legacy_global_preparation[0].elevated);

        let oversized_legacy: Operation = serde_json::from_value(json!({
            "action":"launch",
            "operation_id":"raw-oversized",
            "app_id":"app-0000000000000001",
            "legacy_global_preparation":[{"do":"x".repeat(MAX_RAW_PREPARATION_COMMAND_BYTES + 1)}],
        }))
        .unwrap();
        assert_eq!(
            oversized_legacy.validate(),
            Err("invalid_legacy_preparation")
        );
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
    fn application_video_overrides_preserve_sparse_values_and_validate_bounds() {
        let inherited = ApplicationConfigOverrides::default();
        assert!(inherited.is_empty());
        let null_values: ApplicationConfigOverrides = serde_json::from_value(json!({
            "max_bitrate":null,
            "minimum_fps_target":null,
            "video_max_batch_size_kb":null
        }))
        .unwrap();
        assert!(null_values.is_empty());
        assert_eq!(serde_json::to_value(null_values).unwrap(), json!({}));

        let selected: ApplicationConfigOverrides = serde_json::from_value(json!({
            "max_bitrate":0,
            "minimum_fps_target":59.94,
            "video_max_batch_size_kb":0
        }))
        .unwrap();
        selected.validate().unwrap();
        assert_eq!(selected.max_bitrate, Some(0));
        assert_eq!(selected.minimum_fps_target, Some(59.94));
        assert_eq!(selected.video_max_batch_size_kb, Some(0));
        assert_eq!(
            serde_json::to_value(selected).unwrap(),
            json!({
                "max_bitrate":0,
                "minimum_fps_target":59.94,
                "video_max_batch_size_kb":0
            })
        );

        for max_bitrate in [i32::MIN, -1] {
            ApplicationConfigOverrides {
                max_bitrate: Some(max_bitrate),
                ..ApplicationConfigOverrides::default()
            }
            .validate()
            .unwrap();
        }
        for minimum_fps_target in [-0.1, 1000.1, f64::NAN, f64::INFINITY] {
            assert_eq!(
                ApplicationConfigOverrides {
                    minimum_fps_target: Some(minimum_fps_target),
                    ..ApplicationConfigOverrides::default()
                }
                .validate(),
                Err("invalid_catalog_response")
            );
        }
        assert_eq!(
            ApplicationConfigOverrides {
                video_max_batch_size_kb: Some(65),
                ..ApplicationConfigOverrides::default()
            }
            .validate(),
            Err("invalid_catalog_response")
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
    fn application_amd_av1_latency_mode_preserves_sparse_exact_tokens() {
        let missing = ApplicationConfigOverrides::default();
        let null: ApplicationConfigOverrides =
            serde_json::from_value(json!({"amd_av1_latency_mode":null})).unwrap();
        assert!(missing.is_empty());
        assert!(null.is_empty());
        assert_eq!(missing.amd_av1_latency_mode, None);
        assert_eq!(null.amd_av1_latency_mode, None);
        assert_eq!(serde_json::to_value(missing).unwrap(), json!({}));
        assert_eq!(serde_json::to_value(null).unwrap(), json!({}));

        for mode in ["auto", "none", "power_saving", "realtime", "lowest"] {
            let configured: ApplicationConfigOverrides = serde_json::from_value(json!({
                "amd_av1_latency_mode":mode
            }))
            .unwrap();
            configured.validate().unwrap();
            assert_eq!(configured.amd_av1_latency_mode.as_deref(), Some(mode));
            assert!(!configured.is_empty());
            assert_eq!(
                serde_json::to_value(configured).unwrap(),
                json!({"amd_av1_latency_mode":mode})
            );
        }

        let explicit_auto: ApplicationConfigOverrides =
            serde_json::from_value(json!({"amd_av1_latency_mode":"auto"})).unwrap();
        assert_eq!(explicit_auto.amd_av1_latency_mode.as_deref(), Some("auto"));
        assert!(!explicit_auto.is_empty());

        for invalid in [
            "AUTO",
            "Power_saving",
            "power_Saving",
            "power_saving ",
            "unknown",
        ] {
            let configured: ApplicationConfigOverrides = serde_json::from_value(json!({
                "amd_av1_latency_mode":invalid
            }))
            .unwrap();
            assert_eq!(configured.validate(), Err("invalid_catalog_response"));
        }
        for invalid in [json!(1), json!(true), json!([]), json!({})] {
            assert!(
                serde_json::from_value::<ApplicationConfigOverrides>(json!({
                    "amd_av1_latency_mode":invalid
                }))
                .is_err(),
                "accepted non-string mode {invalid}"
            );
        }
    }

    #[test]
    fn application_nvenc_spatial_aq_override_preserves_sparse_boolean() {
        let missing = ApplicationConfigOverrides::default();
        let null: ApplicationConfigOverrides =
            serde_json::from_value(json!({"nvenc_spatial_aq":null})).unwrap();
        assert!(missing.is_empty());
        assert!(null.is_empty());
        assert_eq!(missing.nvenc_spatial_aq, None);
        assert_eq!(null.nvenc_spatial_aq, None);
        assert_eq!(serde_json::to_value(missing).unwrap(), json!({}));
        assert_eq!(serde_json::to_value(null).unwrap(), json!({}));

        let disabled: ApplicationConfigOverrides =
            serde_json::from_value(json!({"nvenc_spatial_aq":false})).unwrap();
        disabled.validate().unwrap();
        assert_eq!(disabled.nvenc_spatial_aq, Some(false));
        assert!(!disabled.is_empty());
        assert_eq!(
            serde_json::to_value(disabled).unwrap(),
            json!({"nvenc_spatial_aq":false})
        );

        for invalid in [json!("false"), json!(0), json!([]), json!({})] {
            assert!(
                serde_json::from_value::<ApplicationConfigOverrides>(json!({
                    "nvenc_spatial_aq":invalid
                }))
                .is_err(),
                "accepted non-boolean NVENC spatial AQ value {invalid}"
            );
        }
    }

    #[test]
    fn frame_limiter_overrides_preserve_sparse_values_and_validate_contract_domains() {
        let inherited = ApplicationConfigOverrides::default();
        assert!(inherited.is_empty());
        let null_values: ApplicationConfigOverrides = serde_json::from_value(json!({
            "frame_limiter_enable":null,
            "frame_limiter_fps_limit":null,
            "frame_limiter_provider":null,
            "frame_limiter_disable_vsync":null,
            "frame_limiter_auto_virtual_framegen":null,
            "lossless_scaling_framegen":null,
            "lossless_scaling_target_fps":null,
            "frame_generation_provider":null,
            "frame_generation_mode":null,
            "rtss_frame_limit_type":null,
            "ds4_back_as_touchpad_click":null,
        }))
        .unwrap();
        assert!(null_values.is_empty());
        assert_eq!(serde_json::to_value(null_values).unwrap(), json!({}));

        let configured: ApplicationConfigOverrides = serde_json::from_value(json!({
            "frame_limiter_enable":false,
            "frame_limiter_fps_limit":0,
            "frame_limiter_provider":"auto",
            "frame_limiter_disable_vsync":false,
            "frame_limiter_auto_virtual_framegen":true,
            "lossless_scaling_framegen":false,
            "lossless_scaling_target_fps":60,
            "frame_generation_provider":"game-provided",
            "frame_generation_mode":"nvidia-smooth-motion",
            "ds4_back_as_touchpad_click":false,
        }))
        .unwrap();
        configured.validate().unwrap();
        assert_eq!(configured.frame_limiter_enable, Some(false));
        assert_eq!(configured.frame_limiter_fps_limit, Some(0.0));
        assert_eq!(configured.frame_limiter_provider.as_deref(), Some("auto"));
        assert_eq!(configured.frame_limiter_disable_vsync, Some(false));
        assert_eq!(
            configured.frame_limiter_auto_virtual_framegen.as_deref(),
            Some("enabled")
        );
        assert_eq!(configured.lossless_scaling_framegen, Some(false));
        assert_eq!(configured.lossless_scaling_target_fps, Some(60));
        assert_eq!(
            configured.frame_generation_provider.as_deref(),
            Some("game-provided")
        );
        assert_eq!(
            configured.frame_generation_mode.as_deref(),
            Some("nvidia-smooth-motion")
        );
        assert_eq!(configured.ds4_back_as_touchpad_click, Some(false));
        assert_eq!(
            serde_json::to_value(configured).unwrap(),
            json!({
                "frame_limiter_enable":false,
                "frame_limiter_fps_limit":0.0,
                "frame_limiter_provider":"auto",
                "frame_limiter_disable_vsync":false,
                "frame_limiter_auto_virtual_framegen":"enabled",
                "lossless_scaling_framegen":false,
                "lossless_scaling_target_fps":60,
                "frame_generation_provider":"game-provided",
                "frame_generation_mode":"nvidia-smooth-motion",
                "ds4_back_as_touchpad_click":false,
            })
        );

        let enabled: ApplicationConfigOverrides = serde_json::from_value(json!({
            "lossless_scaling_framegen":true
        }))
        .unwrap();
        enabled.validate().unwrap();
        assert_eq!(enabled.lossless_scaling_framegen, Some(true));
        assert!(!enabled.is_empty());
        assert_eq!(
            serde_json::to_value(enabled).unwrap(),
            json!({"lossless_scaling_framegen":true})
        );

        for invalid in [json!("true"), json!(0)] {
            assert!(
                serde_json::from_value::<ApplicationConfigOverrides>(json!({
                    "lossless_scaling_framegen":invalid
                }))
                .is_err(),
                "accepted non-boolean Lossless Scaling frame generation value {invalid}"
            );
        }

        for (field, invalid) in [
            ("frame_generation_provider", json!("unknown")),
            ("frame_generation_provider", json!(false)),
            ("frame_generation_mode", json!("disabled")),
            ("frame_generation_mode", json!(0)),
        ] {
            let parsed =
                serde_json::from_value::<ApplicationConfigOverrides>(json!({(field):invalid}));
            assert!(
                parsed.is_err() || parsed.unwrap().validate().is_err(),
                "accepted invalid {field} value {invalid}"
            );
        }

        for fps_limit in [0.0, 1.234, 1000.0] {
            ApplicationConfigOverrides {
                frame_limiter_fps_limit: Some(fps_limit),
                ..ApplicationConfigOverrides::default()
            }
            .validate()
            .unwrap();
        }
        for target_fps in [0, 1001] {
            assert!(
                ApplicationConfigOverrides {
                    lossless_scaling_target_fps: Some(target_fps),
                    ..ApplicationConfigOverrides::default()
                }
                .validate()
                .is_err()
            );
        }
        for fps_limit in [-0.001, 1.2345, 1000.001, f64::NAN, f64::INFINITY] {
            assert_eq!(
                ApplicationConfigOverrides {
                    frame_limiter_fps_limit: Some(fps_limit),
                    ..ApplicationConfigOverrides::default()
                }
                .validate(),
                Err("invalid_catalog_response")
            );
        }
        for mode in ["true", "false", "enabled", "vrr", "legacy", "disabled"] {
            let parsed: ApplicationConfigOverrides = serde_json::from_value(json!({
                "frame_limiter_auto_virtual_framegen":mode
            }))
            .unwrap();
            parsed.validate().unwrap();
        }
        assert!(
            serde_json::from_value::<ApplicationConfigOverrides>(json!({
                "frame_limiter_auto_virtual_framegen":"invalid"
            }))
            .is_err()
        );

        let valid_provider = if cfg!(windows) {
            "rtss"
        } else if cfg!(target_os = "linux") {
            "mangohud-proton"
        } else {
            "auto"
        };
        ApplicationConfigOverrides {
            frame_limiter_provider: Some(valid_provider.into()),
            ..ApplicationConfigOverrides::default()
        }
        .validate()
        .unwrap();
        assert_eq!(
            ApplicationConfigOverrides {
                frame_limiter_provider: Some("unsupported".into()),
                ..ApplicationConfigOverrides::default()
            }
            .validate(),
            Err("invalid_catalog_response")
        );
        assert_eq!(
            ApplicationConfigOverrides {
                rtss_frame_limit_type: Some("nvidia reflex".into()),
                ..ApplicationConfigOverrides::default()
            }
            .validate(),
            if cfg!(windows) {
                Ok(())
            } else {
                Err("invalid_catalog_response")
            }
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
        assert_eq!(executable.exit_timeout_seconds, 10);
        let command = &structured_step(&executable.preparation[0]).run;
        assert_eq!(
            executable.working_directory.as_deref(),
            Some(inferred_directory.as_str())
        );
        assert_eq!(command.timeout_ms, DEFAULT_COMMAND_TIMEOUT_MS);
        assert!(command.args.is_empty());
        assert!(command.environment.is_empty());
        assert_eq!(command.cwd.as_deref(), Some(inferred_directory.as_str()));
        assert!(structured_step(&executable.preparation[0]).undo.is_none());

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
    fn ordinary_executable_defaults_to_ten_and_preserves_explicit_zero_and_playnite_ten() {
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
            10
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

    #[test]
    fn elevated_is_an_optional_top_level_executable_policy() {
        let program = std::env::current_exe().unwrap();
        let entry = json!({
            "name":"Elevated app",
            "launch":{"kind":"executable","program":program}
        });
        let LaunchTarget::Executable(defaulted) =
            LaunchTarget::from_catalog_entry(&entry, &[]).unwrap()
        else {
            panic!("expected executable target")
        };
        assert!(!defaulted.elevated);

        let entry = json!({
            "name":"Elevated app",
            "elevated":true,
            "launch":{"kind":"executable","program":program}
        });
        let LaunchTarget::Executable(configured) =
            LaunchTarget::from_catalog_entry(&entry, &[]).unwrap()
        else {
            panic!("expected executable target")
        };
        assert!(configured.elevated);

        for invalid in [
            json!({"name":"bad","elevated":"true","launch":{"kind":"executable","program":program}}),
            json!({"name":"bad","elevated":true,"launch":{"kind":"playnite_fullscreen"}}),
        ] {
            assert_eq!(
                LaunchTarget::from_catalog_entry(&invalid, &[]).unwrap_err(),
                "invalid_catalog_response"
            );
        }
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

    fn legacy_preparation_step(
        do_command: &str,
        undo: &str,
        elevated: bool,
    ) -> LegacyPreparationCommand {
        LegacyPreparationCommand {
            do_command: do_command.into(),
            undo: undo.into(),
            elevated,
        }
    }

    #[test]
    fn global_preparation_precedes_app_steps_and_obeys_exclusion_alias() {
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let application_directory = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let global = preparation_step(&executable, "global");
        let app = preparation_step(&executable, "app");
        let entry = json!({
            "launch": {
                "kind":"executable",
                "program":executable,
                "working_directory":application_directory,
                "preparation":[app],
            },
        });
        let LaunchTarget::Executable(merged) =
            LaunchTarget::from_catalog_entry(&entry, std::slice::from_ref(&global)).unwrap()
        else {
            panic!("expected executable target");
        };
        assert_eq!(
            merged.preparation_working_directory.as_deref(),
            Some(application_directory.as_str())
        );
        assert_eq!(structured_step(&merged.preparation[0]).run.args, ["global"]);
        assert_eq!(structured_step(&merged.preparation[1]).run.args, ["app"]);

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
        assert_eq!(structured_step(&excluded.preparation[0]).run.args, ["app"]);
    }

    #[test]
    fn structured_and_legacy_preparation_merge_in_order_and_exclusion_skips_both_globals() {
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let global_structured = preparation_step(&executable, "global-structured");
        let app_structured = preparation_step(&executable, "app-structured");
        let global_legacy = legacy_preparation_step("global raw", "global undo", true);
        let entry = json!({
            "prep_cmd":[{"do":"app raw", "undo":"app undo", "elevated":false}],
            "launch": {
                "kind":"executable",
                "program":executable,
                "preparation":[app_structured],
            },
        });
        let LaunchTarget::Executable(merged) = LaunchTarget::from_catalog_entry_with_preparations(
            &entry,
            std::slice::from_ref(&global_structured),
            std::slice::from_ref(&global_legacy),
            &BTreeMap::new(),
        )
        .unwrap() else {
            panic!("expected executable target");
        };
        assert_eq!(merged.preparation.len(), 4);
        assert_eq!(
            structured_step(&merged.preparation[0]).run.args,
            ["global-structured"]
        );
        assert_eq!(
            merged.preparation[1],
            PlannedPreparationStep::Legacy(global_legacy.clone())
        );
        assert_eq!(
            structured_step(&merged.preparation[2]).run.args,
            ["app-structured"]
        );
        assert_eq!(
            merged.preparation[3],
            PlannedPreparationStep::Legacy(legacy_preparation_step("app raw", "app undo", false))
        );

        let excluded_entry = json!({
            "exclude_global_prep_cmd":true,
            "prep_cmd":[{"do":"app raw", "undo":"app undo", "elevated":false}],
            "launch": {
                "kind":"executable",
                "program":std::env::current_exe().unwrap(),
                "preparation":[app_structured],
            },
        });
        let LaunchTarget::Executable(excluded) =
            LaunchTarget::from_catalog_entry_with_preparations(
                &excluded_entry,
                std::slice::from_ref(&global_structured),
                std::slice::from_ref(&global_legacy),
                &BTreeMap::new(),
            )
            .unwrap()
        else {
            panic!("expected executable target");
        };
        assert_eq!(excluded.preparation.len(), 2);
        assert_eq!(
            structured_step(&excluded.preparation[0]).run.args,
            ["app-structured"]
        );
        assert!(matches!(
            &excluded.preparation[1],
            PlannedPreparationStep::Legacy(step) if step.do_command == "app raw"
        ));

        let alias_entry = json!({
            "prep-cmd":[{"do":"alias raw"}],
            "launch":{"kind":"executable","program":std::env::current_exe().unwrap()},
        });
        let LaunchTarget::Executable(alias) =
            LaunchTarget::from_catalog_entry(&alias_entry, &[]).unwrap()
        else {
            panic!("expected executable target");
        };
        assert!(matches!(
            &alias.preparation[0],
            PlannedPreparationStep::Legacy(step) if step.do_command == "alias raw"
        ));
        let ambiguous_aliases = json!({
            "prep_cmd":[],
            "prep-cmd":[],
            "launch":{"kind":"executable","program":std::env::current_exe().unwrap()},
        });
        assert_eq!(
            LaunchTarget::from_catalog_entry(&ambiguous_aliases, &[]).unwrap_err(),
            "invalid_catalog_response"
        );
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
        let LaunchTarget::Playnite { preparation, .. } = prepared else {
            panic!("expected Playnite target");
        };
        assert_eq!(preparation.len(), 1);
        assert_eq!(structured_step(&preparation[0]).run.args, ["global"]);
        assert_eq!(
            structured_step(&preparation[0]).run.cwd.as_deref(),
            Path::new(&executable).parent().and_then(Path::to_str)
        );
        let fullscreen = json!({
            "launch":{"kind":"playnite_fullscreen"}
        });
        let prepared_fullscreen =
            LaunchTarget::from_catalog_entry(&fullscreen, std::slice::from_ref(&global)).unwrap();
        let LaunchTarget::PlayniteFullscreen { preparation, .. } = prepared_fullscreen else {
            panic!("expected Playnite Fullscreen target");
        };
        assert_eq!(preparation.len(), 1);
        assert_eq!(structured_step(&preparation[0]).run.args, ["global"]);
        assert_eq!(
            structured_step(&preparation[0]).run.cwd.as_deref(),
            Path::new(&executable).parent().and_then(Path::to_str)
        );
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
    fn playnite_working_directory_is_resolved_and_inherited_by_preparation() {
        let application_directory = std::env::current_dir()
            .unwrap()
            .join("playnite working directory")
            .to_string_lossy()
            .into_owned();
        let configured_directory = "\"$(AMATERASU_PLAYNITE_DIRECTORY)\"";
        let environment = BTreeMap::from([(
            "AMATERASU_PLAYNITE_DIRECTORY".into(),
            application_directory.clone(),
        )]);
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let global = preparation_step(&executable, "global");
        let legacy_global = legacy_preparation_step("global raw", "", false);

        for launch in [
            json!({
                "kind":"playnite",
                "game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab",
                "working_directory":configured_directory,
                "preparation":[{"run":{"executable":executable.clone(),"args":["app"]}}],
            }),
            json!({
                "kind":"playnite_fullscreen",
                "working_directory":configured_directory,
                "preparation":[{"run":{"executable":executable.clone(),"args":["app"]}}],
            }),
        ] {
            let entry = json!({"launch":launch});
            let target = LaunchTarget::from_catalog_entry_with_preparations(
                &entry,
                std::slice::from_ref(&global),
                std::slice::from_ref(&legacy_global),
                &environment,
            )
            .unwrap();
            let (working_directory, preparation) = match target {
                LaunchTarget::Playnite {
                    working_directory,
                    preparation,
                    ..
                }
                | LaunchTarget::PlayniteFullscreen {
                    working_directory,
                    preparation,
                    ..
                } => (working_directory, preparation),
                _ => panic!("expected typed Playnite target"),
            };

            assert_eq!(
                working_directory.as_deref(),
                Some(application_directory.as_str())
            );
            assert_eq!(preparation.len(), 3);
            assert_eq!(
                structured_step(&preparation[0]).run.cwd.as_deref(),
                Some(application_directory.as_str())
            );
            assert!(matches!(
                &preparation[1],
                PlannedPreparationStep::Legacy(step) if step.do_command == "global raw"
            ));
            assert_eq!(
                structured_step(&preparation[2]).run.cwd.as_deref(),
                Some(application_directory.as_str())
            );
            assert_eq!(structured_step(&preparation[2]).run.args, ["app"]);
        }

        let alias = json!({
            "launch":{
                "kind":"playnite_fullscreen",
                "working-dir":application_directory,
            }
        });
        assert!(matches!(
            LaunchTarget::from_catalog_entry(&alias, &[]).unwrap(),
            LaunchTarget::PlayniteFullscreen {
                working_directory: Some(_),
                ..
            }
        ));

        let ambiguous = json!({
            "launch":{
                "kind":"playnite",
                "game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab",
                "working_directory":application_directory,
                "working-dir":application_directory,
            }
        });
        assert_eq!(
            LaunchTarget::from_catalog_entry(&ambiguous, &[]).unwrap_err(),
            "invalid_catalog_response"
        );
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
            structured_step(&executable.preparation[0])
                .run
                .cwd
                .as_deref(),
            Some(command_directory.to_str().unwrap())
        );
        assert_eq!(
            structured_step(&executable.preparation[0])
                .undo
                .as_ref()
                .unwrap()
                .cwd
                .as_deref(),
            Some(undo_directory.to_str().unwrap())
        );
    }

    #[test]
    fn executable_and_working_directory_expand_environment_and_trim_wrapping_quotes() {
        let source_executable = std::env::current_exe().unwrap();
        let directory = unique_test_directory("amaterasu path expansion test");
        let executable = directory.join(source_executable.file_name().unwrap());
        std::fs::copy(source_executable, &executable).unwrap();
        let filename = executable.file_name().unwrap().to_str().unwrap();
        let configured_program = format!(
            "\"$(AMATERASU_TEST_EXECUTABLE_DIR){}{}\"",
            std::path::MAIN_SEPARATOR,
            filename
        );
        let configured_directory = "\"$(AMATERASU_TEST_EXECUTABLE_DIR)\"";
        let environment = BTreeMap::from([(
            "AMATERASU_TEST_EXECUTABLE_DIR".to_owned(),
            directory.to_string_lossy().into_owned(),
        )]);
        let launch = json!({
            "kind":"executable",
            "program":configured_program,
            "arguments":["$(AMATERASU_TEST_EXECUTABLE_DIR)","$$(LITERAL)"],
            "working_directory":configured_directory,
        });

        let parsed = Executable::from_catalog_with_environment(&launch, &environment).unwrap();
        assert_eq!(parsed.program, executable.to_string_lossy());
        assert_eq!(parsed.working_directory.as_deref(), directory.to_str());
        assert_eq!(
            parsed.arguments,
            ["$(AMATERASU_TEST_EXECUTABLE_DIR)", "$$(LITERAL)"]
        );

        let global = preparation_step(executable.to_str().unwrap(), "global");
        let entry = json!({"launch":launch});
        let LaunchTarget::Executable(merged) = LaunchTarget::from_catalog_entry_with_environment(
            &entry,
            std::slice::from_ref(&global),
            &environment,
        )
        .unwrap() else {
            panic!("expected executable target");
        };
        assert_eq!(
            structured_step(&merged.preparation[0]).run.cwd.as_deref(),
            directory.to_str()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unset_and_malformed_environment_references_fail_or_expand_without_shelling() {
        let environment = BTreeMap::from([("ROOT".to_owned(), "/safe/game folder".to_owned())]);
        assert_eq!(
            expand_environment_references("prefix-$(AMATERASU_MISSING_92831)-suffix", &environment)
                .unwrap(),
            "prefix--suffix"
        );
        assert_eq!(
            expand_environment_references("$$(ROOT);$(ROOT)", &environment).unwrap(),
            "$(ROOT);/safe/game folder"
        );
        assert_eq!(
            expand_environment_references("$(ROOT);touch should-not-run", &environment).unwrap(),
            "/safe/game folder;touch should-not-run"
        );
        assert_eq!(
            resolve_executable("$(AMATERASU_MISSING_92831)", &environment, None),
            Err("unsupported_launch_spec")
        );
        assert_eq!(
            resolve_working_directory("$(AMATERASU_MISSING_92831)", &environment),
            Ok(None)
        );
        for malformed in ["$(ROOT", "$(ROOT(NESTED))", "$()"] {
            assert_eq!(
                expand_environment_references(malformed, &environment),
                Err("unsupported_launch_spec")
            );
        }
        let oversized_environment = BTreeMap::from([("ROOT".to_owned(), "x".repeat(1_025))]);
        assert_eq!(
            expand_environment_references("$(ROOT)", &oversized_environment),
            Err("unsupported_launch_spec")
        );
    }

    #[test]
    fn catalog_output_relative_path_uses_host_directory_without_app_cwd_or_text_trimming() {
        let host_directory = std::env::current_dir().unwrap();
        let application_directory = host_directory.join("different-application-directory");
        let entry = json!({
            "output":" logs/$$(LITERAL)-capture é.log ",
            "launch":{
                "kind":"executable",
                "program":std::env::current_exe().unwrap(),
                "working_directory":application_directory,
            },
        });
        let filename = " logs/$(LITERAL)-capture é.log ";
        let expected = host_directory.join(filename);
        assert_eq!(
            resolve_catalog_output(&entry, &BTreeMap::new()),
            Ok(Some(expected.to_str().unwrap().to_owned()))
        );
        assert_ne!(expected, application_directory.join(filename));
    }

    #[test]
    fn catalog_output_absence_and_discard_values_normalize_after_expansion() {
        let environment = BTreeMap::from([
            ("EMPTY_OUTPUT".to_owned(), String::new()),
            ("NULL_OUTPUT".to_owned(), "null".to_owned()),
        ]);
        assert_eq!(resolve_catalog_output(&json!({}), &environment), Ok(None));
        for output in [
            Value::Null,
            json!(""),
            json!("null"),
            json!("$(EMPTY_OUTPUT)"),
            json!("$(NULL_OUTPUT)"),
        ] {
            assert_eq!(
                resolve_catalog_output(&json!({"output":output}), &environment),
                Ok(None)
            );
        }
        let expected = std::env::current_dir().unwrap().join(" null ");
        assert_eq!(
            resolve_catalog_output(&json!({"output":" null "}), &environment),
            Ok(Some(expected.to_str().unwrap().to_owned()))
        );
    }

    #[test]
    fn catalog_output_rejects_expanded_control_characters() {
        for expanded in ["bad\0path", "bad\npath", "bad\u{85}path"] {
            let environment = BTreeMap::from([("OUTPUT_PATH".to_owned(), expanded.to_owned())]);
            assert_eq!(
                resolve_catalog_output(&json!({"output":"$(OUTPUT_PATH)"}), &environment),
                Err("unsupported_launch_spec")
            );
        }
    }

    #[test]
    fn catalog_output_enforces_utf8_byte_bounds_before_and_after_absolute_resolution() {
        let host_directory = std::env::current_dir().unwrap();
        let prefix = format!(
            "{}{}",
            host_directory.to_str().unwrap(),
            std::path::MAIN_SEPARATOR
        );
        let remaining_bytes = 1_024 - prefix.len();
        let mut boundary = prefix + &"é".repeat(remaining_bytes / 2);
        if remaining_bytes % 2 == 1 {
            boundary.push('x');
        }
        assert_eq!(boundary.len(), 1_024);
        let mut environment = BTreeMap::from([("OUTPUT_PATH".to_owned(), boundary.clone())]);
        assert_eq!(
            resolve_catalog_output(&json!({"output":"$(OUTPUT_PATH)"}), &environment),
            Ok(Some(boundary.clone()))
        );
        for overflow in [boundary + "é", "é".repeat(512)] {
            environment.insert("OUTPUT_PATH".to_owned(), overflow);
            assert_eq!(
                resolve_catalog_output(&json!({"output":"$(OUTPUT_PATH)"}), &environment),
                Err("unsupported_launch_spec")
            );
        }
    }

    #[test]
    fn bare_executable_lookup_uses_configured_path_and_rejects_relative_programs() {
        let directory = unique_test_directory("amaterasu-launch-path-test");
        let executable_name = if cfg!(windows) {
            "amaterasu-path-probe.exe"
        } else {
            "amaterasu-path-probe"
        };
        let executable = directory.join(executable_name);
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let path_name = if cfg!(windows) {
            "amaterasu-path-probe"
        } else {
            executable_name
        };
        let environment =
            BTreeMap::from([("PATH".to_owned(), directory.to_string_lossy().into_owned())]);

        let resolved = resolve_executable(path_name, &environment, None).unwrap();
        assert_eq!(Path::new(&resolved), executable);
        assert_eq!(
            inferred_working_directory(&resolved).as_deref(),
            directory.to_str()
        );
        assert_eq!(
            resolve_executable("subfolder/amaterasu-path-probe", &environment, None),
            Err("unsupported_launch_spec")
        );
        assert!(path_executable_names("untrusted")
            .iter()
            .all(|name| !matches!(name.to_str(), Some(value) if value.ends_with(".bat") || value.ends_with(".cmd"))));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn relative_path_entries_use_expanded_application_directory_in_search_order() {
        let directory = unique_test_directory("amaterasu-relative-launch-path");
        let first = directory.join("first");
        let second = directory.join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let executable_name = if cfg!(windows) { "probe.exe" } else { "probe" };
        for target in [&first, &second] {
            std::fs::copy(
                std::env::current_exe().unwrap(),
                target.join(executable_name),
            )
            .unwrap();
        }
        let search_path =
            std::env::join_paths([Path::new("missing"), Path::new("second"), &first]).unwrap();
        let environment = BTreeMap::from([
            ("PATH".into(), search_path.to_str().unwrap().into()),
            ("APP_DIRECTORY".into(), directory.to_str().unwrap().into()),
            ("APP_EXECUTABLE".into(), executable_name.into()),
        ]);
        let launch = json!({
            "kind":"executable",
            "program":"$(APP_EXECUTABLE)",
            "working_directory":"$(APP_DIRECTORY)",
        });

        let parsed = Executable::from_catalog_with_environment(&launch, &environment).unwrap();
        assert_eq!(Path::new(&parsed.program), second.join(executable_name));
        assert_eq!(parsed.working_directory.as_deref(), directory.to_str());
        assert!(Path::new(&parsed.program).is_absolute());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn empty_path_entry_selects_executable_in_application_directory() {
        let directory = unique_test_directory("amaterasu-empty-launch-path");
        let executable_name = if cfg!(windows) { "probe.exe" } else { "probe" };
        let executable = directory.join(executable_name);
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let environment = BTreeMap::from([("PATH".into(), String::new())]);

        let resolved =
            resolve_executable(executable_name, &environment, directory.to_str()).unwrap();
        assert_eq!(Path::new(&resolved), executable);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn relative_path_without_application_directory_uses_host_directory_snapshot() {
        let directory = unique_test_directory("amaterasu-host-relative-launch-path");
        let host_directory = std::env::current_dir().unwrap();
        let relative_name = directory.file_name().unwrap();
        let relative_directory = host_directory.join(relative_name);
        std::fs::create_dir(&relative_directory).unwrap();
        let executable_name = if cfg!(windows) { "probe.exe" } else { "probe" };
        let executable = relative_directory.join(executable_name);
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let environment = BTreeMap::from([("PATH".into(), relative_name.to_str().unwrap().into())]);

        let resolved = resolve_executable(executable_name, &environment, None).unwrap();
        assert_eq!(Path::new(&resolved), executable);
        assert_eq!(std::env::current_dir().unwrap(), host_directory);
        std::fs::remove_dir_all(relative_directory).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
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
            structured_step(&executable.preparation[0])
                .run
                .cwd
                .as_deref(),
            Some(application_directory.as_str())
        );
        assert_eq!(
            structured_step(&executable.preparation[0])
                .undo
                .as_ref()
                .unwrap()
                .cwd
                .as_deref(),
            Some(application_directory.as_str())
        );
        assert_eq!(
            structured_step(&executable.preparation[1])
                .run
                .cwd
                .as_deref(),
            Some(run_directory.as_str())
        );
        assert_eq!(
            structured_step(&executable.preparation[1])
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

#[cfg(test)]
mod advanced_input_override_tests {
    use super::ApplicationConfigOverrides;
    use serde_json::json;

    #[test]
    fn application_projection_preserves_advanced_input_false_zero_and_inheritance() {
        let selected = json!({
            "high_resolution_scrolling":false,"always_send_scancodes":false,
            "key_rightalt_to_key_win":true,"key_repeat_delay":0,"key_repeat_frequency":0.0,
        });
        let overrides: ApplicationConfigOverrides =
            serde_json::from_value(selected.clone()).unwrap();
        assert!(overrides.validate().is_ok());
        assert!(!overrides.is_empty());
        assert_eq!(serde_json::to_value(overrides).unwrap(), selected);
        let inherited: ApplicationConfigOverrides = serde_json::from_value(json!({
            "high_resolution_scrolling":null,"always_send_scancodes":null,
            "key_rightalt_to_key_win":null,"key_repeat_delay":null,"key_repeat_frequency":null,
        }))
        .unwrap();
        assert!(inherited.is_empty());
        assert_eq!(serde_json::to_value(inherited).unwrap(), json!({}));
    }

    #[test]
    fn application_projection_rejects_invalid_advanced_input_policy() {
        for (key, value) in [
            ("high_resolution_scrolling", json!("false")),
            ("always_send_scancodes", json!(1)),
            ("key_rightalt_to_key_win", json!([])),
            ("key_repeat_delay", json!(-1)),
            ("key_repeat_delay", json!(0.5)),
            ("key_repeat_delay", json!(2147483648_u64)),
            ("key_repeat_frequency", json!(-0.1)),
            ("key_repeat_frequency", json!("31.25")),
        ] {
            assert!(
                serde_json::from_value::<ApplicationConfigOverrides>(json!({key:value}))
                    .map(|overrides| overrides.validate().is_err())
                    .unwrap_or(true)
            );
        }
        let invalid = ApplicationConfigOverrides {
            key_repeat_frequency: Some(f64::INFINITY),
            ..Default::default()
        };
        assert!(invalid.validate().is_err());
    }
}

#[cfg(test)]
mod software_encoder_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn software_fields_are_sparse_exact_and_bounded() {
        let absent: ApplicationConfigOverrides =
            serde_json::from_value(json!({"sw_preset":null,"sw_tune":null,"encoder":null}))
                .unwrap();
        assert!(absent.is_empty());
        let selected: ApplicationConfigOverrides = serde_json::from_value(
            json!({"sw_preset":"","sw_tune":"film,zerolatency","encoder":"software"}),
        )
        .unwrap();
        assert!(selected.validate().is_ok());
        assert!(!selected.is_empty());
        assert_eq!(
            serde_json::to_value(selected).unwrap(),
            json!({"sw_preset":"","sw_tune":"film,zerolatency","encoder":"software"})
        );
        for invalid in [
            json!({"sw_preset":false}),
            json!({"encoder":"nvenc"}),
            json!({"encoder":""}),
        ] {
            assert!(serde_json::from_value::<ApplicationConfigOverrides>(invalid).is_err());
        }
        for invalid in [
            json!({"sw_preset":"x".repeat(129)}),
            json!({"sw_tune":"film\n"}),
        ] {
            let options: ApplicationConfigOverrides = serde_json::from_value(invalid).unwrap();
            assert!(options.validate().is_err());
        }
    }
}

#[cfg(test)]
mod keybindings_override_tests {
    use super::ApplicationConfigOverrides;
    use serde_json::{Value, json};

    #[test]
    fn ordered_keybindings_are_sparse_and_explicit_empty_replaces_inheritance() {
        for inherited in [json!({}), json!({"keybindings":null})] {
            let overrides: ApplicationConfigOverrides = serde_json::from_value(inherited).unwrap();
            assert!(overrides.keybindings.is_none());
            assert!(overrides.is_empty());
            assert_eq!(serde_json::to_value(overrides).unwrap(), json!({}));
        }
        for list in [
            json!([]),
            json!([0, 255, 65, 66, 65, 67]),
            json!(vec![0_u8; 512]),
        ] {
            let value = json!({"keybindings":list});
            let overrides: ApplicationConfigOverrides =
                serde_json::from_value(value.clone()).unwrap();
            assert!(overrides.validate().is_ok());
            assert!(!overrides.is_empty());
            assert_eq!(serde_json::to_value(overrides).unwrap(), value);
        }
    }

    #[test]
    fn malformed_keybindings_reject_exact_types_byte_bounds_and_pair_count() {
        for invalid in [
            json!([65]),
            json!([-1, 0]),
            json!([0, 256]),
            json!([65.0, 66]),
            json!([true, 66]),
            json!(["65", 66]),
            json!([[65, 66]]),
            json!({"65":66}),
            json!("65,66"),
            json!(vec![0_u8; 514]),
        ] {
            assert!(
                serde_json::from_value::<ApplicationConfigOverrides>(
                    json!({"keybindings":invalid})
                )
                .is_err(),
                "{invalid}"
            );
        }
        assert!(serde_json::from_value::<ApplicationConfigOverrides>(Value::Null).is_err());
    }
}
