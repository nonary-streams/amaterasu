use crate::containment::ContainedChild;
use crate::protocol::{Action, LaunchSpec, ProcessState};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const TERMINATION_GRACE: Duration = Duration::from_millis(200);
const REAP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Serialize)]
struct ProcessRecord {
    process_id: u64,
    operation_id: String,
    pid: Option<u32>,
    #[serde(flatten)]
    state: ProcessState,
    #[serde(skip)]
    owner: String,
    #[serde(skip)]
    spec: LaunchSpec,
    #[serde(skip)]
    child: Option<ContainedChild>,
    #[serde(skip)]
    stop_deadline: Option<Instant>,
}

impl ProcessRecord {
    fn spawn(process_id: u64, caller: &str, operation_id: String, spec: LaunchSpec) -> Self {
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
        let (child, state) = match ContainedChild::spawn(&mut command) {
            Ok(child) => (Some(child), ProcessState::Running),
            Err(error) => (
                None,
                ProcessState::LaunchFailed {
                    os_error: error.raw_os_error(),
                },
            ),
        };
        Self {
            process_id,
            operation_id,
            pid: child.as_ref().map(ContainedChild::id),
            state,
            owner: caller.into(),
            spec,
            child,
            stop_deadline: None,
        }
    }

    fn record_exit(&mut self, status: ExitStatus) {
        self.state = ProcessState::Exited {
            exit_code: status.code(),
            success: status.success(),
        };
        self.child = None;
    }

    fn begin_stop(&mut self) -> Result<(), &'static str> {
        if self.stop_deadline.is_none()
            && let Some(child) = &mut self.child
        {
            child.request_stop().map_err(|_| "process_kill_failed")?;
            self.stop_deadline = Some(Instant::now() + TERMINATION_GRACE);
        }
        Ok(())
    }

    fn reap(&mut self) -> Result<(), &'static str> {
        if self.stop_deadline.is_none()
            && let Some(child) = &mut self.child
            && child.exited().map_err(|_| "process_wait_failed")?
        {
            // Natural leader exit also ends the owned tree. Unix retains the
            // zombie leader until group cleanup, reserving its numeric identity.
            self.begin_stop()?;
        }
        if self
            .stop_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
            && let Some(child) = &mut self.child
        {
            child.force_stop().map_err(|_| "process_kill_failed")?;
            if let Some(status) = child.try_reap().map_err(|_| "process_wait_failed")? {
                self.record_exit(status);
            }
        }
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), &'static str> {
        self.begin_stop()?;
        let deadline = Instant::now() + TERMINATION_GRACE + REAP_TIMEOUT;
        while self.child.is_some() {
            self.reap()?;
            if self.child.is_none() {
                break;
            }
            if Instant::now() >= deadline {
                return Err("process_wait_timeout");
            }
            std::thread::sleep(Duration::from_millis(5));
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
        if serde_json::to_vec(&payload).map_or(true, |bytes| bytes.len() > 16384) {
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
            } => {
                let spec = LaunchSpec {
                    executable,
                    args,
                    environment,
                    cwd,
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
                record.reap()?;
                Ok(json!({"ok":true,"process":record}))
            }
            Action::Status { process_id } | Action::Terminate { process_id } => {
                let record = self
                    .records
                    .get_mut(&process_id)
                    .filter(|record| record.owner == caller)
                    .ok_or("process_not_found")?;
                if matches!(action, Action::Terminate { .. }) {
                    record.terminate()?;
                } else {
                    record.reap()?;
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
            record.reap()?;
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
        let record = ProcessRecord::spawn(process_id, caller, operation_id, spec);
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
            record.reap()?;
            processes.push(json!(record));
        }
        let next_offset = if owned.next().is_some() {
            offset.checked_add(processes.len())
        } else {
            None
        };
        Ok(json!({"ok":true,"processes":processes,"next_offset":next_offset}))
    }

    /// Poll and reap every direct child without waiting for running children.
    pub fn reap_exited(&mut self) -> Result<(), &'static str> {
        let mut failure = None;
        for record in self.records.values_mut() {
            if let Err(error) = record.reap() {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Signal every owned tree first, then poll them against one shared bounded
    /// deadline. Failed records stay owned for a later shutdown/tick retry.
    pub fn shutdown(&mut self) -> Result<(), &'static str> {
        let deadline = Instant::now() + TERMINATION_GRACE + REAP_TIMEOUT;
        let mut failure = None;
        for record in self.records.values_mut() {
            if let Err(error) = record.begin_stop() {
                failure = Some(error);
            }
        }
        loop {
            for record in self.records.values_mut() {
                if let Err(error) = record.reap() {
                    failure = Some(error);
                }
            }
            if self.records.values().all(|record| record.child.is_none()) {
                return failure.map_or(Ok(()), Err);
            }
            if Instant::now() >= deadline {
                return Err(failure.unwrap_or("process_wait_timeout"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn error(code: &str) -> Value {
    json!({"ok":false,"code":code})
}
