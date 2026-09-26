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
    completed: Option<Value>,
    attempts: u8,
    settling: bool,
    first_sent_at: Option<u64>,
    admission_rejected: bool,
    retry_uncertain_admission: bool,
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
            completed: None,
            attempts: 0,
            settling: false,
            first_sent_at: None,
            admission_rejected: false,
            retry_uncertain_admission: false,
        }
    }

    /// Broker operation IDs make replay of this command safe after a lost
    /// admission. The caller must use this only for an idempotent provider action.
    pub fn retry_uncertain_admission(mut self) -> Self {
        self.retry_uncertain_admission = true;
        self
    }

    pub fn cancel(&mut self) {
        self.settling = true;
        self.attempts = 0;
    }

    pub fn shutdown(&mut self, send: &mut Send<'_>) -> bool {
        self.cancel();
        if self.request.is_none() {
            if self.first_sent_at.is_none() {
                return true;
            }
            // Reuse the exact operation ID to recover a lost admission before
            // attempting cancellation and release during unload.
            for _ in 0..4 {
                match send(self.command.clone()) {
                    Ok(Response::Accepted { id }) => {
                        self.request = Some(id);
                        break;
                    }
                    Ok(Response::Error { .. }) => return true,
                    _ => {}
                }
            }
            if self.request.is_none() {
                return false;
            }
        }
        for _ in 0..4 {
            match self.poll(send, u64::MAX) {
                Progress::Failed => return true,
                Progress::Waiting => {}
                Progress::Result(_) => {
                    if self.release_result(send) {
                        return true;
                    }
                }
                Progress::Unresolved => return false,
            }
        }
        false
    }

    pub fn retry(&mut self) {
        self.attempts = 0;
    }

    pub fn was_admitted(&self) -> bool {
        self.request.is_some()
    }

    pub fn was_sent(&self) -> bool {
        self.first_sent_at.is_some()
    }

    pub fn admission_rejected(&self) -> bool {
        self.admission_rejected
    }

    /// Releases a result already retrieved by `poll`. Result ownership belongs
    /// to the caller once `Progress::Result` is returned.
    pub fn release_result(&mut self, send: &mut Send<'_>) -> bool {
        let Some(request) = self.request else {
            return true;
        };
        match send(Command::ReleaseResult { request }) {
            Ok(Response::Ok) => {
                self.request = None;
                self.completed = None;
                true
            }
            Ok(Response::Error { code, .. }) if code == "not_found" => {
                self.request = None;
                self.completed = None;
                true
            }
            _ => false,
        }
    }

    pub fn poll(&mut self, send: &mut Send<'_>, now: u64) -> Progress {
        if let Some(result) = &self.completed {
            return Progress::Result(result.clone());
        }
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
                Ok(Response::Error { .. }) => {
                    // An earlier lost admission may already have reached the
                    // provider; only the first explicit rejection proves no
                    // request was admitted.
                    self.admission_rejected = self.attempts == 0;
                    Progress::Failed
                }
                _ => {
                    if !self.retry_uncertain_admission {
                        self.settling = true;
                    }
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
            Ok(Response::Result { payload }) => {
                self.completed = Some(payload.clone());
                Progress::Result(payload)
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn later_admission_rejection_does_not_disprove_a_lost_first_send() {
        let mut pending = Pending::new("service", json!({}), "operation".into(), None);
        let mut calls = 0;
        let mut send = |_: Command| {
            calls += 1;
            if calls == 1 {
                Err("lost admission".into())
            } else {
                Ok(Response::Error {
                    code: "service_unavailable".into(),
                    message: String::new(),
                })
            }
        };
        assert!(matches!(pending.poll(&mut send, 1), Progress::Waiting));
        assert!(matches!(pending.poll(&mut send, 2), Progress::Failed));
        assert!(!pending.admission_rejected());

        let mut rejected = Pending::new("service", json!({}), "other".into(), None);
        assert!(matches!(rejected.poll(&mut send, 1), Progress::Failed));
        assert!(rejected.admission_rejected());
    }
}
