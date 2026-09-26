use crate::commands::{DetachedCommandRequest, prepare_detached_requests};
use crate::containment::{ContainedChild, DetachedChild};
use crate::lifecycle::ApplicationLifecycle;
use crate::protocol::{
    Action, LaunchSpec, MAX_EXIT_TIMEOUT_SECONDS, PreparationSpec, ProcessState, TerminalOutcome,
};
use crate::steam_tracking::{SteamGameLiveness, SteamProcessTracker};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const REAP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PREPARATION_SHUTDOWN: Duration = Duration::from_secs(8 * 2 * 30);

fn spawn_detached_commands_with(
    spec: &LaunchSpec,
    mut spawn: impl FnMut(
        &mut Command,
        bool,
        Option<&str>,
        Option<&str>,
    ) -> std::io::Result<DetachedChild>,
) -> Vec<DetachedChild> {
    let mut children = Vec::new();
    for detached in &spec.detached {
        let mut command = Command::new(&detached.executable);
        command
            .args(&detached.args)
            .env_clear()
            .envs(&detached.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(cwd) = detached.cwd.as_ref().or(spec.cwd.as_ref()) {
            command.current_dir(cwd);
        } else if let Some(cwd) = std::path::Path::new(&detached.executable)
            .parent()
            .filter(|directory| !directory.as_os_str().is_empty())
        {
            command.current_dir(cwd);
        }

        // Detached launchers are best-effort. Their failure cannot prevent the
        // primary application or a later detached command from starting.
        if let Ok(child) = spawn(
            &mut command,
            detached.elevated.unwrap_or(spec.elevated),
            detached.stdout.as_deref(),
            detached.stderr.as_deref(),
        ) {
            children.push(child);
        }
    }
    children
}

fn check_detached_output_support(
    spec: &LaunchSpec,
    mut supported: impl FnMut(bool) -> std::io::Result<bool>,
) -> Result<(), &'static str> {
    for detached in &spec.detached {
        if detached.stdout.is_some() || detached.stderr.is_some() {
            let elevated = detached.elevated.unwrap_or(spec.elevated);
            if !supported(elevated).map_err(|_| "detached_output_unavailable")? {
                return Err("detached_output_unsupported");
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
enum Completion {
    ExitedPending,
    Exited {
        exit_code: Option<i32>,
        success: bool,
    },
    LaunchFailed {
        os_error: Option<i32>,
    },
    Cancelled,
}

impl Completion {
    fn outcome(&self) -> TerminalOutcome {
        match self {
            Self::ExitedPending | Self::Exited { .. } => TerminalOutcome::Exited,
            Self::LaunchFailed { .. } => TerminalOutcome::LaunchFailed,
            Self::Cancelled => TerminalOutcome::Cancelled,
        }
    }

    fn into_state(self) -> ProcessState {
        match self {
            Self::Exited { exit_code, success } => ProcessState::Exited { exit_code, success },
            Self::LaunchFailed { os_error } => ProcessState::LaunchFailed { os_error },
            Self::Cancelled => ProcessState::Cancelled,
            Self::ExitedPending => {
                unreachable!("an exited process must be reaped before lifecycle cleanup")
            }
        }
    }
}

#[derive(Serialize)]
struct ProcessRecord {
    process_id: u64,
    operation_id: String,
    pid: Option<u32>,
    #[serde(flatten)]
    state: ProcessState,
    #[serde(flatten)]
    lifecycle: Option<ApplicationLifecycle>,
    #[serde(skip)]
    owner: String,
    #[serde(skip)]
    spec: LaunchSpec,
    #[serde(skip)]
    detached_requests: Vec<DetachedCommandRequest>,
    #[serde(skip)]
    child: Option<ContainedChild>,
    #[serde(skip)]
    stop_deadline: Option<Instant>,
    #[serde(skip)]
    completion: Option<Completion>,
    #[serde(skip)]
    application_started_at: Option<Instant>,
    #[serde(skip)]
    auto_detached: bool,
    #[serde(skip)]
    detached_children: Vec<DetachedChild>,
    #[serde(skip)]
    steam_launcher: Option<DetachedChild>,
    #[serde(skip)]
    steam_tracker: Option<SteamProcessTracker>,
    #[serde(skip_serializing_if = "Option::is_none")]
    steam_game_liveness: Option<SteamGameLiveness>,
}

impl ProcessRecord {
    fn new(
        process_id: u64,
        caller: &str,
        operation_id: String,
        spec: LaunchSpec,
        detached_requests: Vec<DetachedCommandRequest>,
    ) -> Self {
        let lifecycle = ApplicationLifecycle::new_with_context(
            spec.preparation.clone(),
            spec.environment.clone(),
            spec.preparation_cwd.clone(),
        );
        let mut record = Self {
            process_id,
            operation_id,
            pid: None,
            state: ProcessState::Preparing,
            lifecycle,
            owner: caller.into(),
            spec,
            detached_requests,
            child: None,
            stop_deadline: None,
            completion: None,
            application_started_at: None,
            auto_detached: false,
            detached_children: Vec::new(),
            steam_launcher: None,
            steam_tracker: None,
            steam_game_liveness: None,
        };
        if record.lifecycle.is_none() {
            record.spawn_application();
        }
        record
    }

    fn spawn_application(&mut self) {
        self.spawn_detached_commands();

        let mut command = Command::new(&self.spec.executable);
        command
            .args(&self.spec.args)
            .env_clear()
            .envs(&self.spec.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let working_directory = self.spec.cwd.as_deref().map(Path::new).unwrap_or_else(|| {
            Path::new(&self.spec.executable)
                .parent()
                .expect("validated absolute executable path has a parent")
        });
        command.current_dir(working_directory);
        if self.is_steam_metadata_launch() {
            self.steam_tracker = self
                .spec
                .steam_install_dir
                .as_deref()
                .and_then(SteamProcessTracker::begin);
            self.steam_game_liveness = Some(if self.steam_tracker.is_some() {
                SteamGameLiveness::Searching
            } else {
                SteamGameLiveness::Unknown
            });
            match DetachedChild::spawn(&mut command, self.spec.elevated) {
                Ok(child) => {
                    self.pid = Some(child.id());
                    self.steam_launcher = Some(child);
                    self.application_started_at = Some(Instant::now());
                    self.state = ProcessState::Running;
                }
                Err(error) => self.complete_or_clean(Completion::LaunchFailed {
                    os_error: error.raw_os_error(),
                }),
            }
            return;
        }
        match ContainedChild::spawn(&mut command, self.spec.elevated) {
            Ok(child) => {
                self.pid = Some(child.id());
                self.child = Some(child);
                self.application_started_at = Some(Instant::now());
                self.state = ProcessState::Running;
            }
            Err(error) => self.complete_or_clean(Completion::LaunchFailed {
                os_error: error.raw_os_error(),
            }),
        }
    }

    fn spawn_detached_commands(&mut self) {
        self.detached_children.extend(spawn_detached_commands_with(
            &self.spec,
            DetachedChild::spawn_with_output,
        ));
    }

    fn reap_detached_commands(&mut self) {
        self.detached_children
            .retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_))));
        if self
            .steam_launcher
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))))
        {
            self.steam_launcher = None;
        }
    }

    fn complete_or_clean(&mut self, completion: Completion) {
        if let Some(lifecycle) = &mut self.lifecycle {
            self.completion = Some(completion);
            lifecycle.begin_cleanup();
            self.state = ProcessState::Stopping;
            self.reconcile_lifecycle();
        } else {
            self.state = completion.into_state();
        }
    }

    fn begin_main_stop(
        &mut self,
        completion: Completion,
        now: Instant,
    ) -> Result<(), &'static str> {
        let child = self.child.as_mut().ok_or("process_wait_failed")?;
        let exit_timeout = Duration::from_secs(self.spec.exit_timeout_seconds);
        let should_wait = if exit_timeout.is_zero() {
            false
        } else {
            child.request_stop().map_err(|_| "process_kill_failed")?
        };
        let group_running = child.group_running().unwrap_or(true);
        let grace = if should_wait && group_running {
            exit_timeout
        } else {
            Duration::ZERO
        };
        self.stop_deadline = Some(now + grace);
        self.completion = Some(completion);
        self.state = ProcessState::Stopping;
        Ok(())
    }

    fn observe_running(&mut self, now: Instant) {
        if self.is_steam_metadata_launch() {
            self.observe_steam_running(now);
            return;
        }
        let observation = self
            .child
            .as_mut()
            .expect("running process has child")
            .successful_exit();
        match observation {
            Ok(None) => {}
            Ok(Some(success)) => {
                if self.spec.wait_all {
                    match self
                        .child
                        .as_mut()
                        .expect("running process has child")
                        .group_running()
                    {
                        Ok(true) => return,
                        Ok(false) => {}
                        Err(_) => {
                            self.completion = Some(Completion::ExitedPending);
                            self.mark_cleanup_failed();
                            return;
                        }
                    }
                }
                let exited_quickly = self.application_started_at.is_some_and(|started_at| {
                    now.saturating_duration_since(started_at) < Duration::from_secs(5)
                });
                if self.spec.auto_detach && success && exited_quickly {
                    self.auto_detached = true;
                    return;
                }
                if self
                    .begin_main_stop(Completion::ExitedPending, now)
                    .is_err()
                {
                    self.completion = Some(Completion::ExitedPending);
                    self.mark_cleanup_failed();
                }
            }
            Err(_) => {
                // Preserve the owned handle and require an explicit retry before
                // signaling a tree whose wait ownership could not be confirmed.
                self.completion = Some(Completion::ExitedPending);
                self.mark_cleanup_failed();
            }
        }
    }

    fn observe_steam_running(&mut self, now: Instant) {
        let Some(tracker) = &mut self.steam_tracker else {
            self.steam_game_liveness = Some(SteamGameLiveness::Unknown);
            return;
        };
        let liveness = tracker.observe(now);
        let finished = tracker.was_associated() && liveness == SteamGameLiveness::Unknown;
        self.steam_game_liveness = Some(liveness);
        if finished {
            self.complete_or_clean(Completion::Exited {
                exit_code: None,
                success: true,
            });
        }
    }

    fn advance_main_stop(&mut self, now: Instant) {
        let deadline = self.stop_deadline.expect("stopping process has deadline");
        let child = self.child.as_mut().expect("stopping process has child");
        if now < deadline && child.group_running().unwrap_or(true) {
            return;
        }
        if child.force_stop().is_err() {
            self.mark_cleanup_failed();
            return;
        }
        let status = match child.try_reap() {
            Ok(Some(status)) => status,
            Ok(None) if now >= deadline + REAP_TIMEOUT => {
                self.mark_cleanup_failed();
                return;
            }
            Ok(None) => return,
            Err(_) => {
                self.mark_cleanup_failed();
                return;
            }
        };
        self.child = None;
        self.stop_deadline = None;
        let completion = match self
            .completion
            .take()
            .expect("stopping process has outcome")
        {
            Completion::ExitedPending => Completion::Exited {
                exit_code: status.code(),
                success: status.success(),
            },
            completion => completion,
        };
        self.complete_or_clean(completion);
    }

    fn advance_preparation(&mut self, now: Instant) {
        let lifecycle = self.lifecycle.as_mut().expect("preparing has lifecycle");
        lifecycle.tick(now);
        let ready = lifecycle.is_ready();
        let failed = lifecycle.preparation_failed();
        if ready {
            self.spawn_application();
        } else if failed {
            self.completion
                .get_or_insert(Completion::LaunchFailed { os_error: None });
            self.state = ProcessState::Stopping;
            self.reconcile_lifecycle();
        }
    }

    fn advance_lifecycle_cleanup(&mut self, now: Instant) {
        if let Some(lifecycle) = &mut self.lifecycle {
            lifecycle.tick(now);
            self.reconcile_lifecycle();
        } else {
            let completion = self
                .completion
                .take()
                .expect("stopping without lifecycle has an outcome");
            self.state = completion.into_state();
        }
    }

    fn reconcile_lifecycle(&mut self) {
        let Some(lifecycle) = &self.lifecycle else {
            return;
        };
        if lifecycle.cleanup_failed() {
            self.mark_cleanup_failed();
        } else if lifecycle.is_cleanup_complete() {
            let completion = self
                .completion
                .take()
                .expect("completed cleanup has an outcome");
            self.state = completion.into_state();
        }
    }

    fn mark_cleanup_failed(&mut self) {
        let outcome = self
            .completion
            .as_ref()
            .expect("cleanup failure has a retained outcome")
            .outcome();
        self.state = ProcessState::CleanupFailed { outcome };
    }

    fn advance(&mut self) -> Result<(), &'static str> {
        let now = Instant::now();
        self.reap_detached_commands();
        match self.state {
            ProcessState::Preparing => self.advance_preparation(now),
            ProcessState::Running if !self.auto_detached => self.observe_running(now),
            ProcessState::Running => {}
            ProcessState::Stopping if self.child.is_some() => self.advance_main_stop(now),
            ProcessState::Stopping
                if self
                    .steam_tracker
                    .as_ref()
                    .is_some_and(SteamProcessTracker::is_stopping) =>
            {
                if !self
                    .steam_tracker
                    .as_mut()
                    .expect("stopping Steam tracker exists")
                    .advance_stop(now)
                {
                    self.steam_game_liveness = Some(SteamGameLiveness::Unknown);
                    let completion = self
                        .completion
                        .take()
                        .expect("stopping Steam process has an outcome");
                    self.complete_or_clean(completion);
                }
            }
            ProcessState::Stopping => self.advance_lifecycle_cleanup(now),
            ProcessState::CleanupFailed { .. }
            | ProcessState::Exited { .. }
            | ProcessState::LaunchFailed { .. }
            | ProcessState::Cancelled => {}
        }
        Ok(())
    }

    fn request_termination(&mut self) -> Result<(), &'static str> {
        if self.is_steam_metadata_launch() {
            return self.request_steam_termination();
        }
        match self.state {
            ProcessState::Preparing => {
                self.completion = Some(Completion::Cancelled);
                self.lifecycle
                    .as_mut()
                    .expect("preparing has lifecycle")
                    .begin_cleanup();
                self.state = ProcessState::Stopping;
            }
            ProcessState::Running => {
                self.begin_main_stop(Completion::Cancelled, Instant::now())?;
            }
            ProcessState::CleanupFailed { .. } if self.child.is_some() => {
                if self.stop_deadline.is_some() {
                    self.state = ProcessState::Stopping;
                } else {
                    let completion = self
                        .completion
                        .as_ref()
                        .expect("failed main cleanup has an outcome")
                        .clone();
                    self.begin_main_stop(completion, Instant::now())?;
                }
            }
            ProcessState::CleanupFailed { .. } => {
                self.lifecycle
                    .as_mut()
                    .expect("cleanup failure without main child has lifecycle")
                    .retry_cleanup();
                self.state = ProcessState::Stopping;
            }
            ProcessState::Stopping
            | ProcessState::Exited { .. }
            | ProcessState::LaunchFailed { .. }
            | ProcessState::Cancelled => {}
        }
        Ok(())
    }

    fn request_steam_termination(&mut self) -> Result<(), &'static str> {
        match self.state {
            ProcessState::Preparing => {
                self.completion = Some(Completion::Cancelled);
                self.lifecycle
                    .as_mut()
                    .expect("preparing has lifecycle")
                    .begin_cleanup();
                self.state = ProcessState::Stopping;
            }
            ProcessState::Running => {
                let now = Instant::now();
                let tracking = self
                    .steam_tracker
                    .as_mut()
                    .is_some_and(|tracker| tracker.request_stop(now));
                if tracking {
                    self.completion = Some(Completion::Cancelled);
                    self.steam_game_liveness = Some(SteamGameLiveness::Stopping);
                    self.state = ProcessState::Stopping;
                } else {
                    // The Steam launcher is intentionally outside the managed
                    // job/group. Without attributed game PIDs, release the lease
                    // without signaling the Steam client or an unrelated process.
                    self.steam_game_liveness = Some(SteamGameLiveness::Unknown);
                    self.complete_or_clean(Completion::Cancelled);
                }
            }
            ProcessState::CleanupFailed { .. } if self.child.is_some() => {
                if self.stop_deadline.is_some() {
                    self.state = ProcessState::Stopping;
                } else {
                    let completion = self
                        .completion
                        .as_ref()
                        .expect("failed main cleanup has an outcome")
                        .clone();
                    self.begin_main_stop(completion, Instant::now())?;
                }
            }
            ProcessState::CleanupFailed { .. } => {
                if let Some(lifecycle) = &mut self.lifecycle {
                    lifecycle.retry_cleanup();
                    self.state = ProcessState::Stopping;
                }
            }
            ProcessState::Stopping
            | ProcessState::Exited { .. }
            | ProcessState::LaunchFailed { .. }
            | ProcessState::Cancelled => {}
        }
        Ok(())
    }

    fn is_steam_metadata_launch(&self) -> bool {
        self.spec.steam_id.is_some() || self.spec.steam_install_dir.is_some()
    }

    fn abort_active_lifecycle_command(&mut self) {
        if let Some(lifecycle) = &mut self.lifecycle {
            lifecycle.abort_active_command();
        }
    }

    fn owns_active_child(&self) -> bool {
        self.child.is_some()
            || self
                .lifecycle
                .as_ref()
                .is_some_and(ApplicationLifecycle::has_active_command)
    }

    fn settled_for_shutdown(&self) -> bool {
        !self.owns_active_child()
            && matches!(
                self.state,
                ProcessState::CleanupFailed { .. }
                    | ProcessState::Exited { .. }
                    | ProcessState::LaunchFailed { .. }
                    | ProcessState::Cancelled
            )
    }

    fn prepare_shutdown(&mut self) -> Result<(), &'static str> {
        match self.state {
            ProcessState::Preparing | ProcessState::Running => self.request_termination()?,
            ProcessState::CleanupFailed { .. } if self.owns_active_child() => {
                self.request_termination()?;
            }
            _ => {}
        }
        Ok(())
    }
}

struct PreparationLease {
    operation_id: String,
    owner: String,
    spec: PreparationSpec,
    lifecycle: ApplicationLifecycle,
    stopping: bool,
}

#[derive(Clone, Copy)]
enum PreparationAction {
    Status,
    Stop,
    Retry,
}

impl PreparationLease {
    fn new(caller: &str, operation_id: String, spec: PreparationSpec) -> Self {
        let lifecycle = ApplicationLifecycle::new_with_context(
            spec.preparation.clone(),
            spec.environment.clone(),
            spec.cwd.clone(),
        )
        .expect("validated preparation lease is nonempty");
        Self {
            operation_id,
            owner: caller.into(),
            spec,
            lifecycle,
            stopping: false,
        }
    }

    fn advance(&mut self) {
        self.lifecycle.tick(Instant::now());
    }

    fn begin_stop(&mut self) {
        self.stopping = true;
        self.lifecycle.begin_cleanup();
        self.advance();
    }

    fn retry_cleanup(&mut self) {
        self.lifecycle.retry_cleanup();
        self.advance();
    }

    fn state(&self) -> &'static str {
        if self.lifecycle.cleanup_failed() {
            "cleanup_failed"
        } else if self.lifecycle.is_cleanup_complete() {
            if self.lifecycle.preparation_failed() {
                "preparation_failed"
            } else {
                "complete"
            }
        } else if self.lifecycle.is_ready() {
            "ready"
        } else if self.stopping || self.lifecycle.preparation_failed() {
            "cleaning"
        } else {
            "preparing"
        }
    }

    fn response(&self) -> Value {
        json!({
            "ok":true,
            "preparation": {
                "operation_id":self.operation_id,
                "state":self.state(),
                "preparation":self.lifecycle.preparation,
                "cleanup":self.lifecycle.cleanup,
            }
        })
    }

    fn settled_for_shutdown(&self) -> bool {
        self.lifecycle.is_cleanup_complete() || self.lifecycle.cleanup_failed()
    }

    fn prepare_shutdown(&mut self) {
        if !self.stopping {
            self.begin_stop();
        } else {
            self.advance();
        }
    }
}

/// Owns application process trees and lifetime-scoped idempotency records.
#[derive(Default)]
pub struct ProcessManager {
    records: BTreeMap<u64, ProcessRecord>,
    preparations: BTreeMap<(String, String), PreparationLease>,
    next_id: u64,
}

impl ProcessManager {
    pub fn handle(&mut self, caller: &str, payload: Value) -> Value {
        self.handle_using_environment_lookup(caller, payload, &|name| std::env::var(name).ok())
    }

    #[cfg(test)]
    pub(crate) fn handle_with_environment_lookup(
        &mut self,
        caller: &str,
        payload: Value,
        host_environment_lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Value {
        self.handle_using_environment_lookup(caller, payload, host_environment_lookup)
    }

    fn handle_using_environment_lookup(
        &mut self,
        caller: &str,
        payload: Value,
        host_environment_lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Value {
        if caller.is_empty() || caller.len() > 256 {
            return error("invalid_caller");
        }
        if serde_json::to_vec(&payload).map_or(true, |bytes| bytes.len() > 16_384) {
            return error("request_too_large");
        }
        let action = match serde_json::from_value(payload) {
            Ok(action) => action,
            Err(_) => return error("invalid_request"),
        };
        match self.execute(caller, action, host_environment_lookup) {
            Ok(value) => value,
            Err(code) => error(code),
        }
    }

    fn execute(
        &mut self,
        caller: &str,
        action: Action,
        host_environment_lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Value, &'static str> {
        match action {
            Action::Launch {
                operation_id,
                executable,
                args,
                environment,
                cwd,
                preparation_cwd,
                elevated,
                auto_detach,
                wait_all,
                exit_timeout_seconds,
                steam_id,
                steam_install_dir,
                detached,
                preparation,
            } => {
                let detached_requests = detached
                    .into_iter()
                    .map(DetachedCommandRequest::from)
                    .collect::<Vec<_>>();
                let cached_detached = self
                    .records
                    .values()
                    .find(|record| {
                        record.owner == caller
                            && record.operation_id == operation_id
                            && record.detached_requests.as_slice() == detached_requests.as_slice()
                    })
                    .map(|record| record.spec.detached.clone());
                let detached = match cached_detached {
                    Some(detached) => detached,
                    None => prepare_detached_requests(
                        &detached_requests,
                        &environment,
                        host_environment_lookup,
                    )
                    .map_err(|_| "invalid_launch")?,
                };
                let spec = LaunchSpec {
                    executable,
                    args,
                    environment,
                    cwd,
                    preparation_cwd,
                    elevated,
                    auto_detach,
                    wait_all,
                    exit_timeout_seconds,
                    steam_id,
                    steam_install_dir,
                    detached,
                    preparation: preparation.into_vec(),
                };
                spec.validate(&operation_id).map_err(|_| "invalid_launch")?;
                if !self
                    .records
                    .values()
                    .any(|record| record.owner == caller && record.operation_id == operation_id)
                {
                    check_detached_output_support(
                        &spec,
                        DetachedChild::output_redirection_supported,
                    )?;
                }
                self.launch(caller, operation_id, spec, detached_requests)
            }
            Action::Lookup { operation_id } => {
                let record = self
                    .records
                    .values_mut()
                    .find(|record| record.owner == caller && record.operation_id == operation_id)
                    .ok_or("process_not_found")?;
                record.advance()?;
                Ok(json!({"ok":true,"process":record}))
            }
            Action::Prepare {
                operation_id,
                environment,
                cwd,
                preparation,
            } => {
                let spec = PreparationSpec {
                    preparation: preparation.into_vec(),
                    environment,
                    cwd,
                };
                spec.validate(&operation_id)
                    .map_err(|_| "invalid_preparation")?;
                self.prepare(caller, operation_id, spec)
            }
            Action::PreparationStatus { operation_id } => {
                self.preparation_action(caller, &operation_id, PreparationAction::Status)
            }
            Action::PreparationStop { operation_id } => {
                self.preparation_action(caller, &operation_id, PreparationAction::Stop)
            }
            Action::PreparationRetry { operation_id } => {
                self.preparation_action(caller, &operation_id, PreparationAction::Retry)
            }
            Action::Status { process_id } | Action::Terminate { process_id } => {
                let record = self
                    .records
                    .get_mut(&process_id)
                    .filter(|record| record.owner == caller)
                    .ok_or("process_not_found")?;
                if matches!(action, Action::Terminate { .. }) {
                    record.request_termination()?;
                } else {
                    record.advance()?;
                }
                Ok(json!({"ok":true,"process":record}))
            }
            Action::List { offset, limit } => self.list(caller, offset, limit),
        }
    }

    fn launch(
        &mut self,
        caller: &str,
        operation_id: String,
        spec: LaunchSpec,
        detached_requests: Vec<DetachedCommandRequest>,
    ) -> Result<Value, &'static str> {
        if self
            .preparations
            .contains_key(&(caller.to_owned(), operation_id.clone()))
        {
            return Err("operation_conflict");
        }
        if let Some(record) = self
            .records
            .values_mut()
            .find(|record| record.owner == caller && record.operation_id == operation_id)
        {
            if record.spec != spec || record.detached_requests != detached_requests {
                return Err("operation_conflict");
            }
            record.advance()?;
            return Ok(json!({"ok":true,"process":record}));
        }
        if self.records.len() >= 128
            || self
                .records
                .values()
                .filter(|record| record.owner == caller)
                .count()
                >= 32
        {
            return Err("process_limit");
        }
        let process_id = self.next_id.checked_add(1).ok_or("process_limit")?;
        let record = ProcessRecord::new(process_id, caller, operation_id, spec, detached_requests);
        let response = json!({"ok":true,"process":record});
        self.records.insert(process_id, record);
        self.next_id = process_id;
        Ok(response)
    }

    fn prepare(
        &mut self,
        caller: &str,
        operation_id: String,
        spec: PreparationSpec,
    ) -> Result<Value, &'static str> {
        let key = (caller.to_owned(), operation_id.clone());
        if let Some(lease) = self.preparations.get_mut(&key) {
            if lease.spec != spec {
                return Err("operation_conflict");
            }
            lease.advance();
            return Ok(lease.response());
        }
        if self
            .records
            .values()
            .any(|record| record.owner == caller && record.operation_id == operation_id)
        {
            return Err("operation_conflict");
        }
        if self.records.len() + self.preparations.len() >= 128
            || self
                .records
                .values()
                .filter(|record| record.owner == caller)
                .count()
                + self
                    .preparations
                    .values()
                    .filter(|lease| lease.owner == caller)
                    .count()
                >= 32
        {
            return Err("process_limit");
        }
        let mut lease = PreparationLease::new(caller, operation_id, spec);
        lease.advance();
        let response = lease.response();
        self.preparations.insert(key, lease);
        Ok(response)
    }

    fn preparation_action(
        &mut self,
        caller: &str,
        operation_id: &str,
        action: PreparationAction,
    ) -> Result<Value, &'static str> {
        if operation_id.is_empty()
            || operation_id.len() > 128
            || !operation_id.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err("invalid_request");
        }
        let lease = self
            .preparations
            .get_mut(&(caller.to_owned(), operation_id.to_owned()))
            .ok_or("preparation_not_found")?;
        match action {
            PreparationAction::Status => lease.advance(),
            PreparationAction::Stop => lease.begin_stop(),
            PreparationAction::Retry => lease.retry_cleanup(),
        }
        Ok(lease.response())
    }

    fn list(&mut self, caller: &str, offset: usize, limit: usize) -> Result<Value, &'static str> {
        if !(1..=16).contains(&limit) {
            return Err("invalid_limit");
        }
        let mut owned = self
            .records
            .values_mut()
            .filter(|record| record.owner == caller)
            .skip(offset);
        let mut processes = Vec::new();
        for record in owned.by_ref().take(limit) {
            record.advance()?;
            processes.push(json!(record));
        }
        let next_offset = if owned.next().is_some() {
            offset.checked_add(processes.len())
        } else {
            None
        };
        Ok(json!({"ok":true,"processes":processes,"next_offset":next_offset}))
    }

    /// Advance every process and lifecycle command once without waiting.
    pub fn reap_exited(&mut self) -> Result<(), &'static str> {
        let mut failure = None;
        for record in self.records.values_mut() {
            if let Err(error) = record.advance() {
                failure = Some(error);
            }
        }
        for lease in self.preparations.values_mut() {
            lease.advance();
        }
        failure.map_or(Ok(()), Err)
    }

    /// Stop every owned child against one shared deadline. Active preparation is
    /// cancelled, owed undo is allowed to finish, and cleanup failures stay retryable.
    pub fn shutdown(&mut self) -> Result<(), &'static str> {
        let deadline = Instant::now() + self.shutdown_budget();
        let mut failure = None;
        loop {
            for record in self.records.values_mut() {
                if let Err(error) = record.prepare_shutdown() {
                    failure = Some(error);
                }
                if let Err(error) = record.advance() {
                    failure = Some(error);
                }
            }
            for lease in self.preparations.values_mut() {
                lease.prepare_shutdown();
            }
            if self
                .records
                .values()
                .all(ProcessRecord::settled_for_shutdown)
                && self
                    .preparations
                    .values()
                    .all(PreparationLease::settled_for_shutdown)
            {
                let cleanup_failed = self
                    .records
                    .values()
                    .any(|record| matches!(record.state, ProcessState::CleanupFailed { .. }))
                    || self
                        .preparations
                        .values()
                        .any(|lease| lease.lifecycle.cleanup_failed());
                return if cleanup_failed {
                    Err(failure.unwrap_or("process_cleanup_failed"))
                } else {
                    failure.map_or(Ok(()), Err)
                };
            }
            if Instant::now() >= deadline {
                return Err(failure.unwrap_or("process_wait_timeout"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Cancel lifecycle commands only after graceful shutdown has exhausted its
    /// shared deadline, then make a final bounded attempt to settle every owned
    /// child before the manager itself is dropped.
    fn force_settle_owned_children(&mut self) {
        let deadline = Instant::now() + self.shutdown_budget();
        loop {
            for record in self.records.values_mut() {
                let _ = record.prepare_shutdown();
                record.abort_active_lifecycle_command();
                let _ = record.advance();
            }
            for lease in self.preparations.values_mut() {
                lease.prepare_shutdown();
                lease.lifecycle.abort_active_command();
                lease.advance();
            }
            if self
                .records
                .values()
                .all(ProcessRecord::settled_for_shutdown)
                && self
                    .preparations
                    .values()
                    .all(PreparationLease::settled_for_shutdown)
                || Instant::now() >= deadline
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn shutdown_budget(&self) -> Duration {
        let longest_grace = self
            .records
            .values()
            .map(|record| record.spec.exit_timeout_seconds)
            .max()
            .unwrap_or_default()
            .min(MAX_EXIT_TIMEOUT_SECONDS);
        (Duration::from_secs(longest_grace) + REAP_TIMEOUT).max(MAX_PREPARATION_SHUTDOWN)
    }
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        if self.shutdown().is_err() {
            self.force_settle_owned_children();
        }
    }
}

fn error(code: &str) -> Value {
    json!({"ok":false,"code":code})
}

#[cfg(test)]
mod detached_elevation_tests {
    use super::*;
    use crate::commands::DetachedCommand;

    fn launch_spec(elevated: bool, policies: &[Option<bool>]) -> LaunchSpec {
        LaunchSpec {
            executable: "primary".into(),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cwd: None,
            preparation_cwd: None,
            elevated,
            auto_detach: false,
            wait_all: true,
            exit_timeout_seconds: 10,
            steam_id: None,
            steam_install_dir: None,
            detached: policies
                .iter()
                .map(|policy| DetachedCommand {
                    executable: "detached".into(),
                    args: Vec::new(),
                    environment: BTreeMap::new(),
                    cwd: None,
                    stdout: None,
                    stderr: None,
                    elevated: *policy,
                })
                .collect(),
            preparation: Vec::new(),
        }
    }

    #[test]
    fn detached_spawner_receives_inherited_and_explicit_elevation_per_child() {
        let spec = launch_spec(true, &[None, Some(false), Some(true)]);
        let mut forwarded = Vec::new();
        let children = spawn_detached_commands_with(&spec, |_, elevated, _, _| {
            forwarded.push(elevated);
            Err(std::io::Error::other("mock spawn"))
        });
        assert!(children.is_empty());
        assert_eq!(forwarded, [true, false, true]);
        assert!(spec.elevated);

        let mut forwarded = Vec::new();
        spawn_detached_commands_with(
            &launch_spec(false, &[None, Some(true)]),
            |_, elevated, _, _| {
                forwarded.push(elevated);
                Err(std::io::Error::other("mock spawn"))
            },
        );
        assert_eq!(forwarded, [false, true]);
    }

    #[test]
    fn redirected_children_check_their_own_effective_elevation_before_spawn() {
        let mut spec = launch_spec(false, &[Some(true), Some(false)]);
        spec.detached[0].stdout = Some("output".into());
        spec.detached[1].stderr = Some("error".into());
        let mut checked = Vec::new();
        check_detached_output_support(&spec, |elevated| {
            checked.push(elevated);
            Ok(true)
        })
        .unwrap();
        assert_eq!(checked, [true, false]);
        assert_eq!(
            check_detached_output_support(&spec, |_| Ok(false)),
            Err("detached_output_unsupported")
        );
    }
}
