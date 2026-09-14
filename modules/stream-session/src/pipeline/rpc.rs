use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::Value;

const ADMISSION_RECOVERY_MS: u64 = 300_000;

pub(super) type Send<'a> = dyn FnMut(Command) -> ModuleResult<Response> + 'a;

/// Retains the exact broker operation until admission, cancellation and release
/// are known. Retrying an uncertain admission must never invent a new operation.
#[derive(Clone)]
pub(super) struct Pending {
    command: Command,
    request: Option<u64>,
    attempts: u8,
    settling: bool,
    first_sent_at: Option<u64>,
}

pub(super) enum Progress {
    Waiting,
    Result(Value),
    Failed,
    Unresolved,
}

impl Pending {
    pub fn new(service: &str, payload: Value, operation: String, parent: Option<u64>) -> Self {
        Self {
            command: Command::Request {
                service: service.into(),
                payload,
                timeout_ms: 5000,
                operation_id: operation,
                parent,
            },
            request: None,
            attempts: 0,
            settling: false,
            first_sent_at: None,
        }
    }

    pub fn cancel(&mut self) {
        self.settling = true;
        self.attempts = 0;
    }

    pub fn shutdown(&mut self, send: &mut Send<'_>) -> bool {
        self.cancel();
        if self.request.is_none() {
            // An unsent command has no effect. An admission whose response was
            // lost cannot be recreated safely during unload.
            return self.first_sent_at.is_none();
        }
        for _ in 0..4 {
            match self.poll(send, u64::MAX) {
                Progress::Failed => return true,
                Progress::Waiting => {}
                Progress::Result(_) | Progress::Unresolved => return false,
            }
        }
        false
    }

    pub fn retry(&mut self) {
        self.attempts = 0;
    }

    pub fn poll(&mut self, send: &mut Send<'_>, now: u64) -> Progress {
        if self.attempts >= 3 {
            return Progress::Unresolved;
        }
        let Some(request) = self.request else {
            if self
                .first_sent_at
                .is_some_and(|started| now.saturating_sub(started) >= ADMISSION_RECOVERY_MS)
            {
                return Progress::Failed;
            }
            self.first_sent_at.get_or_insert(now);
            return match send(self.command.clone()) {
                Ok(Response::Accepted { id }) => {
                    self.request = Some(id);
                    self.attempts = 0;
                    Progress::Waiting
                }
                Ok(Response::Error { .. }) => Progress::Failed,
                _ => {
                    self.settling = true;
                    self.attempts += 1;
                    Progress::Waiting
                }
            };
        };
        if self.settling {
            if settle(send, request) {
                return Progress::Failed;
            }
            self.attempts += 1;
            return Progress::Waiting;
        }
        match send(Command::TakeResult { request }) {
            Ok(Response::Pending) => Progress::Waiting,
            Ok(Response::Result { payload }) => Progress::Result(payload),
            _ => {
                self.settling = true;
                Progress::Waiting
            }
        }
    }
}

fn settle(send: &mut Send<'_>, request: u64) -> bool {
    for command in [
        Command::Cancel { request },
        Command::ReleaseResult { request },
    ] {
        match send(command) {
            Ok(Response::Ok) => {}
            Ok(Response::Error { code, .. }) if code == "not_found" => {}
            _ => return false,
        }
    }
    true
}

pub(super) fn complete(send: &mut Send<'_>, request: u64, payload: Value) -> bool {
    matches!(
        send(Command::Complete { request, payload }),
        Ok(Response::Ok)
    )
}

pub(super) fn active(send: &mut Send<'_>, request: u64) -> bool {
    matches!(send(Command::CheckRequest { request }), Ok(Response::Ok))
}
