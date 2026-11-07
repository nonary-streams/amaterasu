//! Incremental execution for trusted, configured application lifecycle commands.

use crate::command_paths::resolve_command_executable;
use crate::commands::parse_detached_commands;
use crate::containment::ContainedChild;
use crate::output::SharedOutput;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const COMMAND_STOP_GRACE: Duration = Duration::from_millis(200);
const COMMAND_REAP_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const MAX_PREPARATION_STEPS: usize = 8;
pub(crate) const MAX_COMMAND_TIMEOUT_MS: u64 = 30_000;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfiguredCommand {
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<String>,
    #[serde(default = "default_command_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_command_timeout_ms() -> u64 {
    10_000
}

impl ConfiguredCommand {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !valid_path(&self.executable) || self.cwd.as_deref().is_some_and(|cwd| !valid_path(cwd))
        {
            return Err("command executable and cwd must be absolute paths");
        }
        if Path::new(&self.executable)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
            })
        {
            return Err("batch command targets require an explicit interpreter");
        }
        if self.args.len() > 64
            || self
                .args
                .iter()
                .any(|argument| argument.len() > 1_024 || argument.contains('\0'))
            || self.args.iter().map(String::len).sum::<usize>() > 8_192
        {
            return Err("command arguments exceed bounds");
        }
        if !(1..=MAX_COMMAND_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err("command timeout exceeds bounds");
        }
        validate_environment(&self.environment)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub(crate) enum PreparationStep {
    Structured(StructuredPreparationStep),
    Raw(RawPreparationStep),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct StructuredPreparationStep {
    pub run: ConfiguredCommand,
    #[serde(default)]
    pub undo: Option<ConfiguredCommand>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPreparationStep {
    #[serde(default, rename = "do")]
    pub do_command: String,
    #[serde(default)]
    pub undo: String,
    #[serde(default)]
    pub elevated: bool,
}

#[derive(Clone, Copy)]
enum PreparationCommand<'a> {
    Structured(&'a ConfiguredCommand),
    Raw { text: &'a str, elevated: bool },
}

impl PreparationStep {
    fn run_command(&self) -> Option<PreparationCommand<'_>> {
        match self {
            Self::Structured(step) => Some(PreparationCommand::Structured(&step.run)),
            Self::Raw(step) if !step.do_command.is_empty() => Some(PreparationCommand::Raw {
                text: &step.do_command,
                elevated: step.elevated,
            }),
            Self::Raw(_) => None,
        }
    }

    fn undo_command(&self) -> Option<PreparationCommand<'_>> {
        match self {
            Self::Structured(step) => step.undo.as_ref().map(PreparationCommand::Structured),
            Self::Raw(step) if !step.undo.is_empty() => Some(PreparationCommand::Raw {
                text: &step.undo,
                elevated: step.elevated,
            }),
            Self::Raw(_) => None,
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Structured(step) => {
                step.run.validate()?;
                if let Some(undo) = &step.undo {
                    undo.validate()?;
                }
            }
            Self::Raw(step) => {
                for command in [&step.do_command, &step.undo] {
                    if !command.is_empty() {
                        validate_environment_reference_syntax(command)
                            .map_err(|_| "raw preparation environment reference is not closed")?;
                        parse_detached_commands(vec![command.clone()])?;
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn validate_all(steps: &[Self]) -> Result<(), &'static str> {
        if steps.len() > MAX_PREPARATION_STEPS {
            return Err("too many preparation steps");
        }
        for step in steps {
            step.validate()?;
        }
        Ok(())
    }
}

impl PreparationCommand<'_> {
    fn execution_timeout(self, raw_timeout: Option<Duration>) -> Option<Duration> {
        match self {
            Self::Structured(command) => Some(Duration::from_millis(command.timeout_ms)),
            Self::Raw { .. } => raw_timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn raw_preparation_step_defaults_and_preserves_exact_command_text() {
        let defaults: PreparationStep = serde_json::from_value(json!({})).unwrap();
        let PreparationStep::Raw(defaults) = defaults else {
            panic!("empty preparation object must use the raw compatibility form");
        };
        assert_eq!(defaults.do_command, "");
        assert_eq!(defaults.undo, "");
        assert!(!defaults.elevated);

        let raw_text = "tool  --literal=$HOME \"quoted value\"  ";
        let selected: PreparationStep = serde_json::from_value(json!({
            "do":raw_text,
            "undo":"cleanup --flag",
            "elevated":true
        }))
        .unwrap();
        selected.validate().unwrap();
        let PreparationStep::Raw(selected_raw) = &selected else {
            panic!("raw fields must select the raw preparation form");
        };
        assert_eq!(selected_raw.do_command, raw_text);
        assert_eq!(selected_raw.undo, "cleanup --flag");
        assert!(selected_raw.elevated);
        assert!(matches!(
            selected.run_command(),
            Some(PreparationCommand::Raw {
                text,
                elevated: true
            }) if text == raw_text
        ));
        assert!(matches!(
            selected.undo_command(),
            Some(PreparationCommand::Raw {
                text: "cleanup --flag",
                elevated: true
            })
        ));

        let equivalent_tokens: PreparationStep = serde_json::from_value(json!({
            "do":"tool --literal=$HOME \"quoted value\"",
            "undo":"cleanup --flag",
            "elevated":true
        }))
        .unwrap();
        assert_ne!(
            selected, equivalent_tokens,
            "step identity must retain the exact command strings"
        );
    }

    #[test]
    fn raw_preparation_expands_environment_values_and_literal_dollar_escapes() {
        let environment = BTreeMap::from([
            ("ROOT".to_owned(), "/games/My Title".to_owned()),
            ("EMPTY".to_owned(), String::new()),
        ]);

        assert_eq!(
            expand_environment_references("$(ROOT)/$$config/$(EMPTY)", &environment).unwrap(),
            "/games/My Title/$config/"
        );
        assert_eq!(
            expand_environment_references("literal $HOME", &environment).unwrap(),
            "literal $HOME"
        );
        assert!(expand_environment_references("$(ROOT", &environment).is_err());
    }

    #[test]
    fn empty_raw_commands_are_skipped_and_nonempty_commands_are_bounded() {
        let empty: PreparationStep = serde_json::from_value(json!({
            "do":"",
            "undo":""
        }))
        .unwrap();
        empty.validate().unwrap();
        assert!(empty.run_command().is_none());
        assert!(empty.undo_command().is_none());
        let mut lifecycle = ApplicationLifecycle::new(vec![empty]).unwrap();
        lifecycle.tick(Instant::now());
        assert!(lifecycle.is_ready());
        assert_eq!(lifecycle.preparation.completed, 0);
        lifecycle.begin_cleanup();
        lifecycle.tick(Instant::now());
        assert!(lifecycle.is_cleanup_complete());

        let oversized_command = "tool ".repeat(500);
        for invalid_command in [
            "tool \"unclosed",
            oversized_command.as_str(),
            "tool\nargument",
        ] {
            let invalid: PreparationStep =
                serde_json::from_value(json!({"do":invalid_command})).unwrap();
            assert!(invalid.validate().is_err());
        }
    }
}

fn expand_environment_references(
    command: &str,
    environment: &BTreeMap<String, String>,
) -> Result<String, std::io::Error> {
    let mut expanded = String::with_capacity(command.len());
    let mut cursor = 0;

    while let Some(relative_dollar) = command[cursor..].find('$') {
        let dollar = cursor + relative_dollar;
        append_bounded(&mut expanded, &command[cursor..dollar])?;
        let after_dollar = dollar + 1;

        match command.as_bytes().get(after_dollar) {
            Some(b'$') => {
                append_bounded(&mut expanded, "$")?;
                cursor = after_dollar + 1;
            }
            Some(b'(') => {
                let closing_parenthesis = reference_end(command, after_dollar)?;
                let name = &command[after_dollar + 1..closing_parenthesis];
                let value = preparation_environment_value(name, environment);
                append_bounded(&mut expanded, value.as_deref().unwrap_or_default())?;
                cursor = closing_parenthesis + 1;
            }
            _ => {
                append_bounded(&mut expanded, "$")?;
                cursor = after_dollar;
            }
        }
    }

    append_bounded(&mut expanded, &command[cursor..])?;
    Ok(expanded)
}

fn validate_environment_reference_syntax(command: &str) -> Result<(), std::io::Error> {
    let mut cursor = 0;
    while let Some(relative_dollar) = command[cursor..].find('$') {
        let after_dollar = cursor + relative_dollar + 1;
        cursor = match command.as_bytes().get(after_dollar) {
            Some(b'$') => after_dollar + 1,
            Some(b'(') => reference_end(command, after_dollar)? + 1,
            _ => after_dollar,
        };
    }
    Ok(())
}

fn reference_end(command: &str, opening_parenthesis: usize) -> Result<usize, std::io::Error> {
    let mut depth = 0usize;
    for (offset, character) in command[opening_parenthesis..].char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(opening_parenthesis + offset);
                }
            }
            _ => {}
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "raw preparation environment reference is not closed",
    ))
}

fn append_bounded(output: &mut String, value: &str) -> Result<(), std::io::Error> {
    if output.len().saturating_add(value.len()) > 2_048 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "expanded raw preparation command exceeds bounds",
        ));
    }
    output.push_str(value);
    Ok(())
}

fn preparation_environment_value(
    name: &str,
    environment: &BTreeMap<String, String>,
) -> Option<String> {
    #[cfg(windows)]
    let configured = environment
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone());
    #[cfg(not(windows))]
    let configured = environment.get(name).cloned();

    configured.or_else(|| std::env::var(name).ok())
}

fn preparation_working_directory(
    resolved_executable: &Path,
    application_working_directory: Option<&str>,
) -> Option<std::path::PathBuf> {
    if let Some(directory) = application_working_directory {
        return Some(directory.into());
    }
    if resolved_executable.to_string_lossy().contains("://") {
        return None;
    }
    resolved_executable
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod raw_preparation_path_tests {
    use super::*;

    #[test]
    fn raw_preparation_uses_configured_path_target_for_execution_and_directory() {
        let current_executable = std::env::current_exe().unwrap();
        let executable_directory = current_executable.parent().unwrap();
        let executable_name = current_executable.file_name().unwrap().to_string_lossy();
        let environment = BTreeMap::from([(
            "PATH".to_owned(),
            executable_directory.to_string_lossy().into_owned(),
        )]);

        let resolved = resolve_command_executable(&executable_name, &environment, None).unwrap();
        assert_eq!(resolved, current_executable);
        assert_eq!(
            preparation_working_directory(&resolved, None).as_deref(),
            Some(executable_directory)
        );
    }
}

fn valid_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1_024
        && !value.contains('\0')
        && Path::new(value).is_absolute()
}

pub(crate) fn validate_environment(
    environment: &BTreeMap<String, String>,
) -> Result<(), &'static str> {
    if environment.len() > 32
        || environment.iter().any(|(key, value)| {
            key.is_empty()
                || key.len() > 128
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || value.len() > 1_024
                || value.contains('\0')
        })
        || environment
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 4_096
    {
        return Err("command environment exceeds bounds");
    }
    let mut names: Vec<_> = environment
        .keys()
        .map(|key| key.to_ascii_uppercase())
        .collect();
    names.sort();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("command environment names are ambiguous");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandFailureKind {
    OutputOpenFailed,
    Cancelled,
    SpawnFailed,
    ExitFailed,
    TimedOut,
    ContainmentFailed,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CommandFailure {
    pub phase: &'static str,
    pub step: usize,
    pub kind: CommandFailureKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_error: Option<i32>,
}

impl CommandFailure {
    fn new(kind: CommandFailureKind) -> Self {
        Self {
            phase: "",
            step: 0,
            kind,
            exit_code: None,
            os_error: None,
        }
    }

    fn at(mut self, phase: &'static str, step: usize) -> Self {
        self.phase = phase;
        self.step = step;
        self
    }

    fn spawn(error: &std::io::Error) -> Self {
        Self {
            os_error: error.raw_os_error(),
            ..Self::new(CommandFailureKind::SpawnFailed)
        }
    }

    fn exit(status: ExitStatus) -> Self {
        Self {
            exit_code: status.code(),
            ..Self::new(CommandFailureKind::ExitFailed)
        }
    }

    fn containment(error: &std::io::Error) -> Self {
        Self {
            os_error: error.raw_os_error(),
            ..Self::new(CommandFailureKind::ContainmentFailed)
        }
    }
}

enum CommandPoll {
    Running,
    Blocked(CommandFailure),
    Finished(Result<ExitStatus, CommandFailure>),
}

struct CommandTask {
    child: ContainedChild,
    execution_deadline: Option<Instant>,
    stop_requested_at: Option<Instant>,
    force_requested: bool,
    observed_exit: bool,
    outcome: Option<CommandFailure>,
    shutdown_deadline: Option<Instant>,
    #[cfg(test)]
    fail_next_exit_observation: bool,
    #[cfg(test)]
    fail_next_reap_observation: bool,
    #[cfg(test)]
    fail_next_stop_observation: bool,
}

struct TaskSpawnError {
    error: std::io::Error,
    task: Option<Box<CommandTask>>,
}

impl From<std::io::Error> for TaskSpawnError {
    fn from(error: std::io::Error) -> Self {
        Self { error, task: None }
    }
}

impl CommandTask {
    fn spawn(
        spec: PreparationCommand<'_>,
        environment: &BTreeMap<String, String>,
        application_working_directory: Option<&str>,
        execution_timeout: Option<Duration>,
        output: Option<&SharedOutput>,
        now: Instant,
    ) -> Result<Self, TaskSpawnError> {
        let (mut command, elevated) = match spec {
            PreparationCommand::Structured(spec) => {
                let mut command = Command::new(&spec.executable);
                command.args(&spec.args).env_clear().envs(&spec.environment);
                if let Some(cwd) = &spec.cwd {
                    command.current_dir(cwd);
                }
                (command, false)
            }
            PreparationCommand::Raw { text, elevated } => {
                let expanded = expand_environment_references(text, environment)?;
                let parsed = parse_detached_commands(vec![expanded]).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "invalid raw preparation command",
                    )
                })?;
                let parsed = parsed.into_iter().next().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "raw preparation command is empty",
                    )
                })?;
                let executable = resolve_command_executable(
                    &parsed.executable,
                    environment,
                    application_working_directory,
                )?;
                let mut command = Command::new(&executable);
                command.args(parsed.args).envs(environment);
                if let Some(working_directory) =
                    preparation_working_directory(&executable, application_working_directory)
                {
                    command.current_dir(working_directory);
                }
                (command, elevated)
            }
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(output) = output {
            output.attach(&mut command)?;
        }
        match ContainedChild::spawn(&mut command, elevated) {
            Ok(child) => Ok(Self {
                child,
                execution_deadline: execution_timeout.map(|timeout| now + timeout),
                stop_requested_at: None,
                force_requested: false,
                observed_exit: false,
                outcome: None,
                shutdown_deadline: None,
                #[cfg(test)]
                fail_next_exit_observation: false,
                #[cfg(test)]
                fail_next_reap_observation: false,
                #[cfg(test)]
                fail_next_stop_observation: false,
            }),
            Err(failure) => Err(TaskSpawnError {
                task: failure.child.map(|child| {
                    Box::new(Self {
                        child,
                        execution_deadline: Some(now),
                        stop_requested_at: Some(now.checked_sub(COMMAND_STOP_GRACE).unwrap_or(now)),
                        force_requested: false,
                        observed_exit: true,
                        outcome: Some(CommandFailure::spawn(&failure.error)),
                        shutdown_deadline: None,
                        #[cfg(test)]
                        fail_next_exit_observation: false,
                        #[cfg(test)]
                        fail_next_reap_observation: false,
                        #[cfg(test)]
                        fail_next_stop_observation: false,
                    })
                }),
                error: failure.error,
            }),
        }
    }

    fn prepare_shutdown(&mut self, deadline: Instant) {
        let deadline = self
            .shutdown_deadline
            .map_or(deadline, |current| current.min(deadline));
        self.shutdown_deadline = Some(deadline);
        let execution_deadline = deadline
            .checked_sub(COMMAND_STOP_GRACE + COMMAND_REAP_TIMEOUT)
            .unwrap_or(deadline);
        self.execution_deadline = Some(
            self.execution_deadline
                .map_or(execution_deadline, |current| {
                    current.min(execution_deadline)
                }),
        );
    }

    fn force_shutdown(&mut self) {
        self.cancel();
        if self.child.force_stop().is_ok() {
            self.force_requested = true;
            let _ = self.child.try_reap();
        }
    }

    fn cancel(&mut self) {
        if self.outcome.is_none() {
            self.outcome = Some(CommandFailure::new(CommandFailureKind::Cancelled));
        }
    }

    fn observe_exit(&mut self) -> std::io::Result<bool> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_exit_observation) {
            return Err(std::io::Error::other("injected exit observation failure"));
        }
        self.child.exited()
    }

    fn observe_retirement(&mut self) -> std::io::Result<Option<ExitStatus>> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_reap_observation) {
            return Err(std::io::Error::other("injected group observation failure"));
        }
        self.child.try_reap()
    }

    fn request_stop(&mut self) -> std::io::Result<bool> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_stop_observation) {
            return Err(std::io::Error::other("injected stop observation failure"));
        }
        self.child.request_stop()
    }

    /// Perform at most one observation or signal for this tick. The owned child
    /// remains in this task until both the root and its contained tree settle.
    fn poll(&mut self, now: Instant) -> CommandPoll {
        if !self.force_requested
            && self.shutdown_deadline.is_some_and(|deadline| {
                now >= deadline
                    .checked_sub(COMMAND_REAP_TIMEOUT)
                    .unwrap_or(deadline)
            })
        {
            if !self.observed_exit && self.outcome.is_none() {
                match self.observe_exit() {
                    Ok(true) => self.observed_exit = true,
                    Ok(false) => {
                        self.outcome = Some(CommandFailure::new(CommandFailureKind::TimedOut));
                    }
                    Err(error) => return CommandPoll::Blocked(CommandFailure::containment(&error)),
                }
            }
            match self.child.force_stop() {
                Ok(()) => {
                    self.force_requested = true;
                    self.stop_requested_at.get_or_insert(now);
                    return CommandPoll::Running;
                }
                Err(error) => return CommandPoll::Blocked(CommandFailure::containment(&error)),
            }
        }
        if !self.observed_exit && self.outcome.is_none() {
            match self.observe_exit() {
                Ok(true) => self.observed_exit = true,
                Ok(false)
                    if self
                        .execution_deadline
                        .is_some_and(|deadline| now >= deadline) =>
                {
                    self.outcome = Some(CommandFailure::new(CommandFailureKind::TimedOut));
                }
                Ok(false) => return CommandPoll::Running,
                Err(error) => {
                    return CommandPoll::Blocked(CommandFailure::containment(&error));
                }
            }
        }
        if self.stop_requested_at.is_none() {
            match self.request_stop() {
                Ok(_) => self.stop_requested_at = Some(now),
                Err(error) => {
                    return CommandPoll::Blocked(CommandFailure::containment(&error));
                }
            }
            return CommandPoll::Running;
        }
        let stop_requested_at = self.stop_requested_at.expect("set above");
        if !self.force_requested {
            if now.saturating_duration_since(stop_requested_at) < COMMAND_STOP_GRACE {
                return CommandPoll::Running;
            }
            match self.child.force_stop() {
                Ok(()) => self.force_requested = true,
                Err(error) => {
                    return CommandPoll::Blocked(CommandFailure::containment(&error));
                }
            }
            return CommandPoll::Running;
        }
        match self.observe_retirement() {
            Ok(Some(status)) => {
                let result = self.outcome.take().map_or_else(
                    || {
                        if status.success() {
                            Ok(status)
                        } else {
                            Err(CommandFailure::exit(status))
                        }
                    },
                    Err,
                );
                CommandPoll::Finished(result)
            }
            Ok(None)
                if self
                    .shutdown_deadline
                    .is_some_and(|deadline| now >= deadline)
                    || now.saturating_duration_since(stop_requested_at)
                        >= COMMAND_STOP_GRACE + COMMAND_REAP_TIMEOUT =>
            {
                CommandPoll::Blocked(CommandFailure::new(CommandFailureKind::ContainmentFailed))
            }
            Ok(None) => CommandPoll::Running,
            Err(error) => CommandPoll::Blocked(CommandFailure::containment(&error)),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CleanupState {
    Deferred,
    Pending,
    Failed,
    Complete,
}

#[derive(Debug, Serialize)]
pub(crate) struct PreparationReport {
    pub completed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_command: Option<CommandOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<CommandFailure>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CleanupReport {
    pub state: CleanupState,
    pub remaining: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_command: Option<CommandOutcome>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<CommandFailure>,
}

/// The most recently settled command in a phase, without trusted command text or
/// environment values. A failed wait never masquerades as a settled outcome.
#[derive(Debug, Serialize)]
pub(crate) struct CommandOutcome {
    pub step: usize,
    pub success: bool,
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<CommandFailureKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_error: Option<i32>,
}

impl CommandOutcome {
    fn settled(step: usize, result: &Result<ExitStatus, CommandFailure>) -> Self {
        match result {
            Ok(status) => Self {
                step,
                success: status.success(),
                exit_code: status.code(),
                kind: None,
                os_error: None,
            },
            Err(failure) => Self::failed(step, failure),
        }
    }

    fn failed(step: usize, failure: &CommandFailure) -> Self {
        Self {
            step,
            success: false,
            exit_code: failure.exit_code,
            kind: Some(failure.kind),
            os_error: failure.os_error,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Preparing,
    Ready,
    Cleaning,
    CleanupFailed,
    Complete,
}

#[derive(Clone, Copy)]
enum CommandPurpose {
    Preparation(usize),
    Undo(usize),
    UndoRetry(usize),
}

struct ActiveCommand {
    purpose: CommandPurpose,
    task: CommandTask,
}

#[derive(Serialize)]
pub(crate) struct ApplicationLifecycle {
    pub preparation: PreparationReport,
    pub cleanup: CleanupReport,
    #[serde(skip)]
    steps: Vec<PreparationStep>,
    #[serde(skip)]
    environment: BTreeMap<String, String>,
    #[serde(skip)]
    preparation_working_directory: Option<String>,
    #[serde(skip)]
    raw_undo_timeout: Duration,
    #[serde(skip)]
    output: Option<SharedOutput>,
    #[serde(skip)]
    next_preparation: usize,
    #[serde(skip)]
    pending_undo: Vec<usize>,
    #[serde(skip)]
    attempted_undo: Vec<usize>,
    #[serde(skip)]
    active: Option<ActiveCommand>,
    #[serde(skip)]
    phase: Phase,
    #[serde(skip)]
    shutdown_deadline: Option<Instant>,
}

impl ApplicationLifecycle {
    #[cfg(test)]
    pub(crate) fn new(steps: Vec<PreparationStep>) -> Option<Self> {
        Self::new_with_context(steps, BTreeMap::new(), None, 10)
    }

    pub(crate) fn new_with_context(
        steps: Vec<PreparationStep>,
        environment: BTreeMap<String, String>,
        preparation_working_directory: Option<String>,
        exit_timeout_seconds: u64,
    ) -> Option<Self> {
        (!steps.is_empty()).then(|| {
            Self::new_lease_with_context(
                steps,
                environment,
                preparation_working_directory,
                exit_timeout_seconds,
            )
        })
    }

    pub(crate) fn new_lease_with_context(
        steps: Vec<PreparationStep>,
        environment: BTreeMap<String, String>,
        preparation_working_directory: Option<String>,
        exit_timeout_seconds: u64,
    ) -> Self {
        Self {
            preparation: PreparationReport {
                completed: 0,
                last_command: None,
                failure: None,
            },
            cleanup: CleanupReport {
                state: CleanupState::Deferred,
                remaining: 0,
                last_command: None,
                failures: Vec::new(),
            },
            steps,
            environment,
            preparation_working_directory,
            raw_undo_timeout: Duration::from_secs(exit_timeout_seconds.max(15)),
            output: None,
            next_preparation: 0,
            pending_undo: Vec::new(),
            attempted_undo: Vec::new(),
            active: None,
            phase: Phase::Preparing,
            shutdown_deadline: None,
        }
    }

    pub(crate) fn prepare_shutdown(&mut self, deadline: Instant) {
        let deadline = self
            .shutdown_deadline
            .map_or(deadline, |current| current.min(deadline));
        self.shutdown_deadline = Some(deadline);
        if matches!(self.phase, Phase::Cleaning | Phase::CleanupFailed)
            && let Some(active) = &mut self.active
        {
            active.task.prepare_shutdown(deadline);
        }
    }

    pub(crate) fn force_shutdown(&mut self) {
        if let Some(active) = &mut self.active {
            active.task.force_shutdown();
        }
    }

    fn execution_budget_expired(&self, now: Instant) -> bool {
        self.shutdown_deadline.is_some_and(|deadline| {
            now >= deadline
                .checked_sub(COMMAND_STOP_GRACE + COMMAND_REAP_TIMEOUT)
                .unwrap_or(deadline)
        })
    }

    fn retain_command(&mut self, purpose: CommandPurpose, mut task: CommandTask) {
        if matches!(self.phase, Phase::Cleaning | Phase::CleanupFailed)
            && let Some(deadline) = self.shutdown_deadline
        {
            task.prepare_shutdown(deadline);
        }
        self.active = Some(ActiveCommand { purpose, task });
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.phase == Phase::Ready
    }

    pub(crate) fn preparation_failed(&self) -> bool {
        self.preparation.failure.is_some()
    }

    pub(crate) fn cleanup_failed(&self) -> bool {
        self.phase == Phase::CleanupFailed
    }

    pub(crate) fn is_cleanup_complete(&self) -> bool {
        self.phase == Phase::Complete
    }

    /// Advance one command by one nonblocking observation. At most one process is
    /// spawned or signaled per call, keeping the shared managed-module tick short.
    pub(crate) fn tick(&mut self, now: Instant) {
        if let Some(mut active) = self.active.take() {
            let purpose = active.purpose;
            match active.task.poll(now) {
                CommandPoll::Running => {
                    self.active = Some(active);
                    return;
                }
                CommandPoll::Blocked(failure) => match purpose {
                    CommandPurpose::Preparation(_) => {
                        self.active = Some(active);
                        self.block_command(purpose, failure);
                        return;
                    }
                    CommandPurpose::Undo(index) | CommandPurpose::UndoRetry(index) => {
                        // Keep the exact command and its native identity. A
                        // transient observation error does not prove that a
                        // running undo should be cancelled or repeated.
                        active.purpose = CommandPurpose::UndoRetry(index);
                        self.active = Some(active);
                        self.record_undo_failure(index, failure);
                        self.cleanup.state = CleanupState::Failed;
                        self.phase = Phase::CleanupFailed;
                        return;
                    }
                },
                CommandPoll::Finished(result) => {
                    self.finish_command(purpose, result);
                }
            }
        }
        match self.phase {
            Phase::Preparing => self.start_next_preparation(now),
            Phase::Cleaning => self.start_next_undo(now),
            Phase::Ready | Phase::CleanupFailed | Phase::Complete => {}
        }
    }

    fn start_next_preparation(&mut self, now: Instant) {
        loop {
            if self.next_preparation == self.steps.len() {
                self.phase = Phase::Ready;
                return;
            }
            let index = self.next_preparation;
            let Some(command) = self.steps[index].run_command() else {
                // A reached cleanup-only hook is an explicit end action even
                // though this step has no preparation process to start.
                if self.steps[index].undo_command().is_some() {
                    self.pending_undo.push(index);
                    self.cleanup.remaining = self.pending_undo.len();
                }
                self.next_preparation += 1;
                continue;
            };
            match CommandTask::spawn(
                command,
                &self.environment,
                self.preparation_working_directory.as_deref(),
                // The pinned raw preparation command waits until it exits; only
                // structured commands carry an explicit per-command timeout.
                command.execution_timeout(None),
                self.output.as_ref(),
                now,
            ) {
                Ok(task) => {
                    // A started command may have changed external state even when it
                    // later times out or exits unsuccessfully, so its undo is owed.
                    self.pending_undo.push(index);
                    self.cleanup.remaining = self.pending_undo.len();
                    self.retain_command(CommandPurpose::Preparation(index), task);
                }
                Err(failure) => {
                    let command_failure = CommandFailure::spawn(&failure.error);
                    if failure.task.is_none() {
                        self.preparation.last_command =
                            Some(CommandOutcome::failed(index, &command_failure));
                    }
                    self.preparation.failure = Some(command_failure.at("preparation", index));
                    if let Some(task) = failure.task {
                        self.pending_undo.push(index);
                        self.cleanup.remaining = self.pending_undo.len();
                        self.retain_command(CommandPurpose::Preparation(index), *task);
                    }
                    self.begin_cleanup();
                }
            }
            return;
        }
    }

    fn start_next_undo(&mut self, now: Instant) {
        let Some(index) = self
            .pending_undo
            .iter()
            .rev()
            .find(|index| !self.attempted_undo.contains(index))
            .copied()
        else {
            self.finish_cleanup_pass();
            return;
        };
        let Some(command) = self.steps[index].undo_command() else {
            self.complete_undo(index);
            return;
        };
        if self.execution_budget_expired(now) {
            self.record_undo_failure(index, CommandFailure::new(CommandFailureKind::TimedOut));
            self.cleanup.state = CleanupState::Failed;
            self.phase = Phase::CleanupFailed;
            return;
        }
        match CommandTask::spawn(
            command,
            &self.environment,
            self.preparation_working_directory.as_deref(),
            command.execution_timeout(Some(self.raw_undo_timeout)),
            self.output.as_ref(),
            now,
        ) {
            Ok(task) => {
                self.retain_command(CommandPurpose::Undo(index), task);
            }
            Err(failure) => {
                let command_failure = CommandFailure::spawn(&failure.error);
                if failure.task.is_none() {
                    self.cleanup.last_command =
                        Some(CommandOutcome::failed(index, &command_failure));
                }
                self.record_undo_failure(index, command_failure);
                if let Some(task) = failure.task {
                    self.retain_command(CommandPurpose::UndoRetry(index), *task);
                    self.cleanup.state = CleanupState::Failed;
                    self.phase = Phase::CleanupFailed;
                }
            }
        }
    }

    fn finish_command(
        &mut self,
        purpose: CommandPurpose,
        result: Result<ExitStatus, CommandFailure>,
    ) {
        match purpose {
            CommandPurpose::Preparation(index) => {
                self.preparation.last_command = Some(CommandOutcome::settled(index, &result));
            }
            CommandPurpose::Undo(index) | CommandPurpose::UndoRetry(index) => {
                self.cleanup.last_command = Some(CommandOutcome::settled(index, &result));
            }
        }
        match purpose {
            CommandPurpose::Preparation(index) if self.phase == Phase::Preparing => match result {
                Ok(_) => {
                    self.preparation.completed += 1;
                    self.next_preparation = index + 1;
                }
                Err(failure) => {
                    self.preparation.failure = Some(failure.at("preparation", index));
                    self.begin_cleanup();
                }
            },
            // Cancellation may move to Cleaning while the running preparation
            // command is still being contained. Its outcome no longer changes the
            // cancellation reason, but its undo remains required.
            CommandPurpose::Preparation(_) => {}
            CommandPurpose::Undo(index) => match result {
                Ok(_) => self.complete_undo(index),
                Err(failure) => self.record_undo_failure(index, failure),
            },
            // A blocked undo is retained with its child handle until an explicit
            // cleanup retry can observe it again. Once it settles, a failed result
            // remains owed for another explicit cleanup pass.
            CommandPurpose::UndoRetry(index) => match result {
                Ok(_) => self.complete_undo(index),
                Err(failure) => self.record_undo_failure(index, failure),
            },
        }
    }

    fn block_command(&mut self, purpose: CommandPurpose, failure: CommandFailure) {
        let (phase, index) = match purpose {
            CommandPurpose::Preparation(index) => ("preparation", index),
            CommandPurpose::Undo(index) | CommandPurpose::UndoRetry(index) => ("undo", index),
        };
        let failure = failure.at(phase, index);
        if matches!(purpose, CommandPurpose::Preparation(_)) && self.preparation.failure.is_none() {
            self.preparation.failure = Some(failure.clone());
            if let Some(active) = &mut self.active {
                active.task.cancel();
            }
        }
        self.cleanup.failures = vec![failure];
        self.cleanup.state = CleanupState::Failed;
        self.phase = Phase::CleanupFailed;
    }

    fn record_undo_failure(&mut self, index: usize, failure: CommandFailure) {
        // A retained live command can report the same wait failure on later ticks.
        if self.attempted_undo.contains(&index) {
            return;
        }
        self.cleanup.failures.push(failure.at("undo", index));
        self.attempted_undo.push(index);
    }

    fn finish_cleanup_pass(&mut self) {
        self.cleanup.remaining = self.pending_undo.len();
        if self.cleanup.failures.is_empty() {
            self.phase = Phase::Complete;
            self.cleanup.state = CleanupState::Complete;
            self.cleanup.remaining = 0;
            self.output = None;
        } else {
            self.phase = Phase::CleanupFailed;
            self.cleanup.state = CleanupState::Failed;
        }
    }

    fn complete_undo(&mut self, index: usize) {
        if let Some(position) = self
            .pending_undo
            .iter()
            .position(|pending_index| *pending_index == index)
        {
            self.pending_undo.remove(position);
        }
        self.cleanup.remaining = self.pending_undo.len();
    }

    pub(crate) fn begin_cleanup(&mut self) {
        if matches!(self.phase, Phase::Complete | Phase::Cleaning) {
            return;
        }
        if self.phase == Phase::Preparing && self.preparation.failure.is_none() {
            self.preparation.failure = Some(
                CommandFailure::new(CommandFailureKind::Cancelled)
                    .at("preparation", self.next_preparation),
            );
        }
        if let Some(active) = &mut self.active
            && matches!(active.purpose, CommandPurpose::Preparation(_))
        {
            active.task.cancel();
        }
        self.cleanup.failures.clear();
        self.attempted_undo.clear();
        self.cleanup.state = CleanupState::Pending;
        self.cleanup.remaining = self.pending_undo.len();
        self.phase = Phase::Cleaning;
        if let Some(deadline) = self.shutdown_deadline
            && let Some(active) = &mut self.active
        {
            active.task.prepare_shutdown(deadline);
        }
    }

    pub(crate) fn retry_cleanup(&mut self) {
        if self.phase == Phase::CleanupFailed {
            self.cleanup.failures.clear();
            self.attempted_undo.clear();
            self.cleanup.state = CleanupState::Pending;
            self.phase = Phase::Cleaning;
        }
    }

    #[cfg(test)]
    pub(crate) fn abort_active_command(&mut self) {
        if self.phase == Phase::Preparing {
            self.begin_cleanup();
        } else if let Some(active) = &mut self.active {
            active.task.cancel();
        }
    }

    pub(crate) fn has_active_command(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn set_output(&mut self, output: Option<SharedOutput>) {
        self.output = output;
    }

    pub(crate) fn output_open_failed(&mut self, error: &std::io::Error) {
        self.preparation.failure = Some(CommandFailure {
            phase: "output",
            step: 0,
            kind: CommandFailureKind::OutputOpenFailed,
            exit_code: None,
            os_error: error.raw_os_error(),
        });
        self.cleanup.state = CleanupState::Complete;
        self.phase = Phase::Complete;
        self.output = None;
    }

    /// Budget only owed work. Shutdown cancels an active preparation command,
    /// then executes each remaining undo against its own bounded deadline.
    pub(crate) fn cleanup_budget(&self) -> Duration {
        let containment_budget = COMMAND_STOP_GRACE + COMMAND_REAP_TIMEOUT;
        self.pending_undo
            .iter()
            .filter_map(|index| self.steps[*index].undo_command())
            .map(|command| {
                command
                    .execution_timeout(Some(self.raw_undo_timeout))
                    .expect("undo commands always have a bounded cleanup timeout")
                    + containment_budget
            })
            .sum::<Duration>()
            + containment_budget
    }
}

#[cfg(test)]
mod undo_retry_tests {
    use super::*;

    fn preparation_step(index: usize) -> PreparationStep {
        let missing_directory = std::env::temp_dir().join(format!(
            "amaterasu-missing-undo-executables-{}",
            std::process::id()
        ));
        assert!(!missing_directory.exists());
        let executable = missing_directory.join(format!("undo-{index}.exe"));
        let command = ConfiguredCommand {
            executable: executable.to_string_lossy().into_owned(),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cwd: None,
            timeout_ms: 1_000,
        };
        PreparationStep::Structured(StructuredPreparationStep {
            run: command.clone(),
            undo: Some(command),
        })
    }

    fn cleanup_only_step(index: usize) -> PreparationStep {
        let PreparationStep::Structured(step) = preparation_step(index) else {
            unreachable!();
        };
        PreparationStep::Raw(RawPreparationStep {
            do_command: String::new(),
            undo: step.run.executable,
            elevated: false,
        })
    }

    #[test]
    fn cleanup_only_steps_are_owed_in_reverse_order_when_reached() {
        let empty = PreparationStep::Raw(RawPreparationStep {
            do_command: String::new(),
            undo: String::new(),
            elevated: false,
        });
        let mut lifecycle =
            ApplicationLifecycle::new(vec![cleanup_only_step(0), empty, cleanup_only_step(2)])
                .unwrap();
        lifecycle.tick(Instant::now());
        assert!(lifecycle.is_ready());
        assert_eq!(lifecycle.preparation.completed, 0);
        assert_eq!(lifecycle.pending_undo, [0, 2]);
        assert_eq!(lifecycle.cleanup.remaining, 2);

        lifecycle.begin_cleanup();
        lifecycle.tick(Instant::now());
        lifecycle.tick(Instant::now());
        lifecycle.tick(Instant::now());
        assert!(lifecycle.cleanup_failed());
        assert_eq!(lifecycle.attempted_undo, [2, 0]);
        assert_eq!(
            lifecycle
                .cleanup
                .failures
                .iter()
                .map(|failure| failure.step)
                .collect::<Vec<_>>(),
            [2, 0],
        );
        assert!(
            lifecycle
                .cleanup
                .failures
                .iter()
                .all(|failure| matches!(failure.kind, CommandFailureKind::SpawnFailed))
        );
    }

    #[test]
    fn cleanup_only_steps_after_a_failed_preparation_are_not_owed() {
        let mut lifecycle = ApplicationLifecycle::new(vec![
            cleanup_only_step(0),
            preparation_step(1),
            cleanup_only_step(2),
        ])
        .unwrap();
        lifecycle.tick(Instant::now());
        assert!(lifecycle.preparation_failed());
        assert_eq!(lifecycle.preparation.failure.as_ref().unwrap().step, 1);
        assert_eq!(lifecycle.pending_undo, [0]);
        lifecycle.tick(Instant::now());
        lifecycle.tick(Instant::now());
        assert!(lifecycle.cleanup_failed());
        assert_eq!(lifecycle.attempted_undo, [0]);
    }

    #[test]
    fn settled_retry_failure_is_reported_once_before_older_undo_is_attempted() {
        let mut lifecycle =
            ApplicationLifecycle::new(vec![preparation_step(0), preparation_step(1)]).unwrap();
        lifecycle.phase = Phase::CleanupFailed;
        lifecycle.cleanup.state = CleanupState::Failed;
        lifecycle.cleanup.remaining = 2;
        lifecycle.cleanup.failures =
            vec![CommandFailure::new(CommandFailureKind::ContainmentFailed).at("undo", 1)];
        lifecycle.pending_undo = vec![0, 1];
        lifecycle.attempted_undo.push(1);
        lifecycle.retry_cleanup();

        lifecycle.finish_command(
            CommandPurpose::UndoRetry(1),
            Err(CommandFailure::new(CommandFailureKind::ExitFailed)),
        );
        lifecycle.record_undo_failure(
            1,
            CommandFailure::new(CommandFailureKind::ContainmentFailed),
        );
        assert_eq!(lifecycle.cleanup.failures.len(), 1);
        assert_eq!(lifecycle.cleanup.failures[0].step, 1);
        assert!(matches!(
            lifecycle.cleanup.failures[0].kind,
            CommandFailureKind::ExitFailed
        ));

        lifecycle.start_next_undo(Instant::now());
        assert_eq!(lifecycle.attempted_undo, [1, 0]);
        assert_eq!(lifecycle.cleanup.failures.len(), 2);
        assert_eq!(lifecycle.cleanup.failures[1].step, 0);
        assert!(matches!(
            lifecycle.cleanup.failures[1].kind,
            CommandFailureKind::SpawnFailed
        ));

        lifecycle.finish_cleanup_pass();
        assert!(lifecycle.cleanup_failed());
        lifecycle.retry_cleanup();
        assert!(lifecycle.cleanup.failures.is_empty());
        lifecycle.record_undo_failure(
            1,
            CommandFailure::new(CommandFailureKind::ContainmentFailed),
        );
        assert_eq!(lifecycle.cleanup.failures.len(), 1);
    }
}

#[cfg(test)]
#[path = "preparation_timeout_tests.rs"]
mod preparation_timeout_tests;
