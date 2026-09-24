//! Incremental execution for trusted, configured application lifecycle commands.

use crate::containment::ContainedChild;
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
#[serde(deny_unknown_fields)]
pub(crate) struct PreparationStep {
    pub run: ConfiguredCommand,
    pub undo: Option<ConfiguredCommand>,
}

impl PreparationStep {
    pub(crate) fn validate_all(steps: &[Self]) -> Result<(), &'static str> {
        if steps.len() > MAX_PREPARATION_STEPS {
            return Err("too many preparation steps");
        }
        for step in steps {
            step.run.validate()?;
            if let Some(undo) = &step.undo {
                undo.validate()?;
            }
        }
        Ok(())
    }
}

fn valid_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1_024
        && !value.contains('\0')
        && Path::new(value).is_absolute()
}

fn validate_environment(environment: &BTreeMap<String, String>) -> Result<(), &'static str> {
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
    Finished(Result<(), CommandFailure>),
}

struct CommandTask {
    child: ContainedChild,
    execution_deadline: Instant,
    stop_requested_at: Option<Instant>,
    force_requested: bool,
    observed_exit: bool,
    outcome: Option<CommandFailure>,
}

impl CommandTask {
    fn spawn(spec: &ConfiguredCommand, now: Instant) -> Result<Self, std::io::Error> {
        let mut command = Command::new(&spec.executable);
        command
            .args(&spec.args)
            .env_clear()
            .envs(&spec.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        Ok(Self {
            child: ContainedChild::spawn(&mut command, false)?,
            execution_deadline: now + Duration::from_millis(spec.timeout_ms),
            stop_requested_at: None,
            force_requested: false,
            observed_exit: false,
            outcome: None,
        })
    }

    fn cancel(&mut self) {
        if self.outcome.is_none() {
            self.outcome = Some(CommandFailure::new(CommandFailureKind::Cancelled));
        }
    }

    /// Perform at most one observation or signal for this tick. The owned child
    /// remains in this task until both the root and its contained tree settle.
    fn poll(&mut self, now: Instant) -> CommandPoll {
        if !self.observed_exit && self.outcome.is_none() {
            match self.child.exited() {
                Ok(true) => self.observed_exit = true,
                Ok(false) if now >= self.execution_deadline => {
                    self.outcome = Some(CommandFailure::new(CommandFailureKind::TimedOut));
                }
                Ok(false) => return CommandPoll::Running,
                Err(error) => {
                    return CommandPoll::Blocked(CommandFailure::containment(&error));
                }
            }
        }
        if self.stop_requested_at.is_none() {
            match self.child.request_stop() {
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
        match self.child.try_reap() {
            Ok(Some(status)) => {
                let result = self.outcome.take().map_or_else(
                    || {
                        if status.success() {
                            Ok(())
                        } else {
                            Err(CommandFailure::exit(status))
                        }
                    },
                    Err,
                );
                CommandPoll::Finished(result)
            }
            Ok(None)
                if now.saturating_duration_since(stop_requested_at)
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
    pub failure: Option<CommandFailure>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CleanupReport {
    pub state: CleanupState,
    pub remaining: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<CommandFailure>,
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
    next_preparation: usize,
    #[serde(skip)]
    pending_undo: Vec<usize>,
    #[serde(skip)]
    active: Option<ActiveCommand>,
    #[serde(skip)]
    phase: Phase,
}

impl ApplicationLifecycle {
    pub(crate) fn new(steps: Vec<PreparationStep>) -> Option<Self> {
        (!steps.is_empty()).then_some(Self {
            preparation: PreparationReport {
                completed: 0,
                failure: None,
            },
            cleanup: CleanupReport {
                state: CleanupState::Deferred,
                remaining: 0,
                failures: Vec::new(),
            },
            steps,
            next_preparation: 0,
            pending_undo: Vec::new(),
            active: None,
            phase: Phase::Preparing,
        })
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
        if let Some(active) = &mut self.active {
            let purpose = active.purpose;
            match active.task.poll(now) {
                CommandPoll::Running => return,
                CommandPoll::Blocked(failure) => {
                    self.block_command(purpose, failure);
                    return;
                }
                CommandPoll::Finished(result) => {
                    self.active = None;
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
        if self.next_preparation == self.steps.len() {
            self.phase = Phase::Ready;
            return;
        }
        let index = self.next_preparation;
        match CommandTask::spawn(&self.steps[index].run, now) {
            Ok(task) => {
                // A started command may have changed external state even when it
                // later times out or exits unsuccessfully, so its undo is owed.
                self.pending_undo.push(index);
                self.cleanup.remaining = self.pending_undo.len();
                self.active = Some(ActiveCommand {
                    purpose: CommandPurpose::Preparation(index),
                    task,
                });
            }
            Err(error) => {
                self.preparation.failure =
                    Some(CommandFailure::spawn(&error).at("preparation", index));
                self.begin_cleanup();
            }
        }
    }

    fn start_next_undo(&mut self, now: Instant) {
        let Some(&index) = self.pending_undo.last() else {
            self.phase = Phase::Complete;
            self.cleanup.state = CleanupState::Complete;
            self.cleanup.remaining = 0;
            return;
        };
        let Some(command) = &self.steps[index].undo else {
            self.complete_undo(index);
            return;
        };
        match CommandTask::spawn(command, now) {
            Ok(task) => {
                self.active = Some(ActiveCommand {
                    purpose: CommandPurpose::Undo(index),
                    task,
                });
            }
            Err(error) => {
                self.cleanup.failures = vec![CommandFailure::spawn(&error).at("undo", index)];
                self.cleanup.state = CleanupState::Failed;
                self.phase = Phase::CleanupFailed;
            }
        }
    }

    fn finish_command(&mut self, purpose: CommandPurpose, result: Result<(), CommandFailure>) {
        match purpose {
            CommandPurpose::Preparation(index) if self.phase == Phase::Preparing => match result {
                Ok(()) => {
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
                Ok(()) => self.complete_undo(index),
                Err(failure) => {
                    self.cleanup.failures = vec![failure.at("undo", index)];
                    self.cleanup.state = CleanupState::Failed;
                    self.phase = Phase::CleanupFailed;
                }
            },
        }
    }

    fn block_command(&mut self, purpose: CommandPurpose, failure: CommandFailure) {
        let (phase, index) = match purpose {
            CommandPurpose::Preparation(index) => ("preparation", index),
            CommandPurpose::Undo(index) => ("undo", index),
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

    fn complete_undo(&mut self, index: usize) {
        debug_assert_eq!(self.pending_undo.last(), Some(&index));
        self.pending_undo.pop();
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
        if let Some(active) = &mut self.active {
            active.task.cancel();
        }
        self.cleanup.failures.clear();
        self.cleanup.state = CleanupState::Pending;
        self.cleanup.remaining = self.pending_undo.len();
        self.phase = Phase::Cleaning;
    }

    pub(crate) fn retry_cleanup(&mut self) {
        if self.phase == Phase::CleanupFailed {
            self.cleanup.failures.clear();
            self.cleanup.state = CleanupState::Pending;
            self.phase = Phase::Cleaning;
        }
    }

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
}
