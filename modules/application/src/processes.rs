use crate::containment::ContainedChild;
use crate::lifecycle::ApplicationLifecycle;
use crate::protocol::{Action, LaunchSpec, ProcessState, TerminalOutcome};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TERMINATION_GRACE: Duration = Duration::from_millis(200);
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
        };
        if record.lifecycle.is_none() {
            record.spawn_application();
        }
        record
    }

    fn spawn_application(&mut self) {
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
        match ContainedChild::spawn(&mut command) {
            Ok(child) => {
                self.pid = Some(child.id());
                self.child = Some(child);
                self.state = ProcessState::Running;
            }
            Err(error) => self.complete_or_clean(Completion::LaunchFailed {
                os_error: error.raw_os_error(),
            }),
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
        child.request_stop().map_err(|_| "process_kill_failed")?;
        self.stop_deadline = Some(now + TERMINATION_GRACE);
        self.completion = Some(completion);
        self.state = ProcessState::Stopping;
        Ok(())
    }

    fn observe_running(&mut self, now: Instant) {
        let observation = self
            .child
            .as_mut()
            .expect("running process has child")
            .exited();
        match observation {
            Ok(false) => {}
            Ok(true) => {
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

    fn advance_main_stop(&mut self, now: Instant) {
        let deadline = self.stop_deadline.expect("stopping process has deadline");
        if now < deadline {
            return;
        }
        let child = self.child.as_mut().expect("stopping process has child");
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
        match self.state {
            ProcessState::Preparing => self.advance_preparation(now),
            ProcessState::Running => self.observe_running(now),
            ProcessState::Stopping if self.child.is_some() => self.advance_main_stop(now),
            ProcessState::Stopping => self.advance_lifecycle_cleanup(now),
            ProcessState::CleanupFailed { .. }
            | ProcessState::Exited { .. }
            | ProcessState::LaunchFailed { .. }
            | ProcessState::Cancelled => {}
        }
        Ok(())
    }

    fn request_termination(&mut self) -> Result<(), &'static str> {
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
                preparation,
            } => {
                let spec = LaunchSpec {
                    executable,
                    args,
                    environment,
                    cwd,
                    preparation,
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
        let deadline = Instant::now() + TERMINATION_GRACE + REAP_TIMEOUT;
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
        let deadline = Instant::now() + TERMINATION_GRACE + REAP_TIMEOUT;
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
