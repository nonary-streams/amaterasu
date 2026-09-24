use crate::commands::parse_detached_commands;
use crate::containment::ContainedChild;
use crate::lifecycle::ApplicationLifecycle;
use crate::protocol::{
    Action, LaunchSpec, MAX_EXIT_TIMEOUT_SECONDS, ProcessState, TerminalOutcome,
};
use crate::steam_tracking::{SteamGameLiveness, SteamProcessTracker};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const REAP_TIMEOUT: Duration = Duration::from_secs(2);

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
    detached_children: Vec<std::process::Child>,
    #[serde(skip)]
    steam_launcher: Option<std::process::Child>,
    #[serde(skip)]
    steam_tracker: Option<SteamProcessTracker>,
    #[serde(skip_serializing_if = "Option::is_none")]
    steam_game_liveness: Option<SteamGameLiveness>,
}

impl ProcessRecord {
    fn new(process_id: u64, caller: &str, operation_id: String, spec: LaunchSpec) -> Self {
        let lifecycle = ApplicationLifecycle::new(spec.preparation.clone());
        let mut record = Self {
            process_id,
            operation_id,
            pid: None,
            state: ProcessState::Preparing,
            lifecycle,
            owner: caller.into(),
            spec,
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
        if let Some(cwd) = &self.spec.cwd {
            command.current_dir(cwd);
        }
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
            match command.spawn() {
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
        match ContainedChild::spawn(&mut command) {
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
        for detached in &self.spec.detached {
            let mut command = Command::new(&detached.executable);
            command
                .args(&detached.args)
                .env_clear()
                .envs(&self.spec.environment)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(cwd) = &self.spec.cwd {
                command.current_dir(cwd);
            } else if let Some(cwd) = std::path::Path::new(&detached.executable)
                .parent()
                .filter(|directory| !directory.as_os_str().is_empty())
            {
                command.current_dir(cwd);
            }

            // The source treats these as best-effort launchers. Their spawn
            // failure must not prevent the primary application from starting.
            if let Ok(child) = command.spawn() {
                self.detached_children.push(child);
            }
        }
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

/// Owns application process trees and lifetime-scoped idempotency records.
#[derive(Default)]
pub struct ProcessManager {
    records: BTreeMap<u64, ProcessRecord>,
    next_id: u64,
}

impl ProcessManager {
    pub fn handle(&mut self, caller: &str, payload: Value) -> Value {
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
        match self.execute(caller, action) {
            Ok(value) => value,
            Err(code) => error(code),
        }
    }

    fn execute(&mut self, caller: &str, action: Action) -> Result<Value, &'static str> {
        match action {
            Action::Launch {
                operation_id,
                executable,
                args,
                environment,
                cwd,
                auto_detach,
                wait_all,
                exit_timeout_seconds,
                steam_id,
                steam_install_dir,
                detached,
                preparation,
            } => {
                let detached = parse_detached_commands(detached).map_err(|_| "invalid_launch")?;
                let spec = LaunchSpec {
                    executable,
                    args,
                    environment,
                    cwd,
                    auto_detach,
                    wait_all,
                    exit_timeout_seconds,
                    steam_id,
                    steam_install_dir,
                    detached,
                    preparation: preparation.into_vec(),
                };
                spec.validate(&operation_id).map_err(|_| "invalid_launch")?;
                self.launch(caller, operation_id, spec)
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
    ) -> Result<Value, &'static str> {
        if let Some(record) = self
            .records
            .values_mut()
            .find(|record| record.owner == caller && record.operation_id == operation_id)
        {
            if record.spec != spec {
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
        let record = ProcessRecord::new(process_id, caller, operation_id, spec);
        let response = json!({"ok":true,"process":record});
        self.records.insert(process_id, record);
        self.next_id = process_id;
        Ok(response)
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
            if self
                .records
                .values()
                .all(ProcessRecord::settled_for_shutdown)
            {
                let cleanup_failed = self
                    .records
                    .values()
                    .any(|record| matches!(record.state, ProcessState::CleanupFailed { .. }));
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
            if self
                .records
                .values()
                .all(ProcessRecord::settled_for_shutdown)
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
        Duration::from_secs(longest_grace) + REAP_TIMEOUT
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
