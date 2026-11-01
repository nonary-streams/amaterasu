use super::Send;
use amaterasu_module_sdk::{Command, Response};
use serde_json::Value;

pub enum Outcome {
    Waiting,
    Result(Value),
    Rejected,
    Settled,
    Uncertain,
}

pub struct Call {
    command: Command,
    request: Option<u64>,
    cancelling: bool,
    failures: u8,
}

impl Call {
    pub fn release_result(&mut self, send: &mut Send<'_>) -> bool {
        let Some(request) = self.request else {
            return false;
        };
        match send(Command::ReleaseResult { request }) {
            Ok(Response::Ok) => true,
            Ok(Response::Error { code, .. }) if code == "not_found" => true,
            _ => false,
        }
    }
    pub fn new(service: &str, payload: Value, operation_id: String, timeout_ms: u64) -> Self {
        Self {
            command: Command::Request {
                service: service.into(),
                payload,
                timeout_ms,
                operation_id,
                parent: None,
            },
            request: None,
            cancelling: false,
            failures: 0,
        }
    }

    /// Check the exact serialized broker payload without allocating a copy.
    pub fn payload_within_limit(&self, limit: usize) -> bool {
        struct Counter {
            used: usize,
            limit: usize,
        }
        impl std::io::Write for Counter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > self.limit - self.used {
                    return Err(std::io::Error::other("payload too large"));
                }
                self.used += bytes.len();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let Command::Request { payload, .. } = &self.command else {
            return false;
        };
        serde_json::to_writer(&mut Counter { used: 0, limit }, payload).is_ok()
    }

    pub fn cancel(&mut self) {
        if !self.cancelling {
            self.cancelling = true;
            self.failures = 0;
        }
    }

    pub fn retry_settlement(&mut self) {
        self.cancelling = true;
        self.failures = 0;
    }

    pub fn poll(&mut self, send: &mut Send<'_>) -> Outcome {
        if self.failures >= 3 {
            return Outcome::Uncertain;
        }
        let Some(request) = self.request else {
            return match send(self.command.clone()) {
                Ok(Response::Accepted { id }) => {
                    self.request = Some(id);
                    self.failures = 0;
                    Outcome::Waiting
                }
                Ok(Response::Error { .. }) => Outcome::Rejected,
                _ => {
                    // Admission may have committed. Resubmit the exact command
                    // only to recover its broker request ID, then settle it.
                    self.cancelling = true;
                    self.failures += 1;
                    Outcome::Waiting
                }
            };
        };
        if self.cancelling {
            if settle(request, send) {
                return Outcome::Settled;
            }
            self.failures += 1;
            return Outcome::Waiting;
        }
        match send(Command::TakeResult { request }) {
            Ok(Response::Pending) => Outcome::Waiting,
            Ok(Response::Result { payload }) => Outcome::Result(payload),
            _ => {
                self.cancelling = true;
                Outcome::Waiting
            }
        }
    }

    pub fn shutdown(&mut self, send: &mut Send<'_>) {
        self.cancel();
        for _ in 0..4 {
            if !matches!(self.poll(send), Outcome::Waiting) {
                break;
            }
        }
    }
}

fn settle(request: u64, send: &mut Send<'_>) -> bool {
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

#[cfg(test)]
mod payload_limit_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assembled_open_including_options_obeys_exact_broker_boundary() {
        let mut payload = json!({"action":"open", "settings":"", "launch_options":[{"namespace":"org.example.alpha","version":1,"value":{"enabled":true}}]});
        let overhead = serde_json::to_vec(&payload).unwrap().len();
        payload["settings"] = json!("x".repeat(16_384 - overhead));
        assert_eq!(serde_json::to_vec(&payload).unwrap().len(), 16_384);
        assert!(
            Call::new("amaterasu.rtsp/1", payload.clone(), "boundary".into(), 1000)
                .payload_within_limit(16_384)
        );
        payload["settings"] = json!("x".repeat(16_385 - overhead));
        assert_eq!(serde_json::to_vec(&payload).unwrap().len(), 16_385);
        assert!(
            !Call::new("amaterasu.rtsp/1", payload, "boundary".into(), 1000)
                .payload_within_limit(16_384)
        );
    }
}
