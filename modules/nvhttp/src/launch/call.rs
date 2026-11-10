use super::Send;
use amaterasu_module_sdk::{Command, Response};
use serde_json::Value;
use std::time::{Duration, Instant};

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
    result: Option<Value>,
    attempted: bool,
    first_sent: Option<Instant>,
}

impl Call {
    pub(super) fn from_command(command: Command) -> Self {
        Self {
            command,
            request: None,
            cancelling: false,
            failures: 0,
            result: None,
            attempted: false,
            first_sent: None,
        }
    }
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
            result: None,
            attempted: false,
            first_sent: None,
        }
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
        if let Some(payload) = self.result.as_ref() {
            let payload = payload.clone();
            return if self.release_result(send) {
                self.result = None;
                Outcome::Result(payload)
            } else {
                self.failures += 1;
                Outcome::Waiting
            };
        }
        let Some(request) = self.request else {
            if self.cancelling && !self.attempted {
                return Outcome::Settled;
            }
            let command = if self.cancelling {
                // A matching identifier may be reused once the broker's
                // retention expires; never attach to an unrelated operation.
                if self
                    .first_sent
                    .is_some_and(|sent| sent.elapsed() >= Duration::from_secs(300))
                {
                    return Outcome::Uncertain;
                }
                match lookup_command(&self.command) {
                    Some(command) => command,
                    None => return Outcome::Uncertain,
                }
            } else {
                self.attempted = true;
                self.first_sent.get_or_insert_with(Instant::now);
                self.command.clone()
            };
            let response = send(command);
            if self.cancelling
                && self
                    .first_sent
                    .is_some_and(|sent| sent.elapsed() >= Duration::from_secs(300))
            {
                return Outcome::Uncertain;
            }
            return match response {
                Ok(Response::Accepted { id }) => {
                    self.request = Some(id);
                    self.failures = 0;
                    Outcome::Waiting
                }
                Ok(Response::Error { .. }) if !self.cancelling => Outcome::Rejected,
                _ => {
                    // Admission may have committed. Only look up the exact
                    // operation; never dispatch a new mutation during cleanup.
                    self.cancelling = true;
                    self.failures += 1;
                    Outcome::Waiting
                }
            };
        };
        match send(Command::TakeResult { request }) {
            Ok(Response::Pending) => {
                if self.cancelling {
                    self.failures += 1;
                }
                Outcome::Waiting
            }
            Ok(Response::Result { payload }) => {
                self.result = Some(payload);
                self.poll(send)
            }
            _ => {
                // Broker cancellation or expiry does not establish that a
                // delivered provider mutation stopped. Keep the exact request.
                self.failures += 1;
                Outcome::Waiting
            }
        }
    }

    /// Give up on a read whose reply was lost. The request performs no
    /// mutation, so cancelling it and releasing any result leaves nothing owed.
    pub fn abandon_read(&mut self, send: &mut Send<'_>) {
        if let Some(request) = self.request.take() {
            let _ = send(Command::Cancel { request });
            let _ = send(Command::ReleaseResult { request });
        }
        self.result = None;
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

fn lookup_command(command: &Command) -> Option<Command> {
    match command {
        Command::Request {
            service,
            payload,
            operation_id,
            parent,
            ..
        } => Some(Command::LookupRequest {
            service: service.clone(),
            payload: payload.clone(),
            operation_id: operation_id.clone(),
            parent: *parent,
        }),
        Command::CallApi {
            service,
            method,
            interface_major,
            interface_minor,
            payload,
            operation_id,
            parent,
            ..
        } => Some(Command::LookupApiRequest {
            service: service.clone(),
            method: method.clone(),
            interface_major: *interface_major,
            interface_minor: *interface_minor,
            payload: payload.clone(),
            operation_id: operation_id.clone(),
            parent: *parent,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cancelled_unsubmitted_call_never_dispatches() {
        let mut call = Call::new("service/1", json!({}), "operation".into(), 1000);
        call.cancel();
        assert!(matches!(
            call.poll(&mut |_| panic!("must not dispatch")),
            Outcome::Settled
        ));
    }

    #[test]
    fn cancelled_admitted_call_waits_for_provider_result() {
        let mut call = Call::new("service/1", json!({}), "operation".into(), 1000);
        assert!(matches!(
            call.poll(&mut |_| Ok(Response::Accepted { id: 7 })),
            Outcome::Waiting
        ));
        call.cancel();
        assert!(matches!(
            call.poll(&mut |command| {
                assert!(matches!(command, Command::TakeResult { request: 7 }));
                Ok(Response::Pending)
            }),
            Outcome::Waiting
        ));
        let mut send = |command| match command {
            Command::TakeResult { request: 7 } => Ok(Response::Result {
                payload: json!({"owned": 42}),
            }),
            Command::ReleaseResult { request: 7 } => Ok(Response::Ok),
            _ => panic!("unexpected command"),
        };
        assert!(
            matches!(call.poll(&mut send), Outcome::Result(value) if value == json!({"owned": 42}))
        );
    }

    #[test]
    fn lost_admission_is_looked_up_without_replaying_mutation() {
        let mut call = Call::new("service/1", json!({"owner": 1}), "operation".into(), 1000);
        assert!(matches!(
            call.poll(&mut |_| Err("lost admission".into())),
            Outcome::Waiting
        ));
        assert!(matches!(call.poll(&mut |command| {
            assert!(matches!(command, Command::LookupRequest { operation_id, .. } if operation_id == "operation"));
            Ok(Response::Accepted { id: 7 })
        }), Outcome::Waiting));
    }

    #[test]
    fn expired_ambiguous_admission_is_not_attached_to_a_reused_operation() {
        let mut call = Call::new("service/1", json!({}), "operation".into(), 1000);
        assert!(matches!(
            call.poll(&mut |_| Err("lost admission".into())),
            Outcome::Waiting
        ));
        call.first_sent = Some(Instant::now() - Duration::from_secs(300));
        assert!(matches!(
            call.poll(&mut |_| panic!("expired lookup must not run")),
            Outcome::Uncertain
        ));
    }

    #[test]
    fn terminal_payload_survives_a_lost_release_acknowledgement() {
        let mut call = Call::new("service/1", json!({}), "operation".into(), 1000);
        let mut stage = 0;
        let mut send = |command| {
            stage += 1;
            match (stage, command) {
                (1, Command::Request { .. }) => Ok(Response::Accepted { id: 7 }),
                (2, Command::TakeResult { request: 7 }) => Ok(Response::Result {
                    payload: json!({"owned": 42}),
                }),
                (3, Command::ReleaseResult { request: 7 }) => Err("lost acknowledgement".into()),
                (4, Command::ReleaseResult { request: 7 }) => {
                    Ok(Response::error("not_found", "already released"))
                }
                _ => panic!("unexpected command"),
            }
        };
        assert!(matches!(call.poll(&mut send), Outcome::Waiting));
        assert!(matches!(call.poll(&mut send), Outcome::Waiting));
        assert!(
            matches!(call.poll(&mut send), Outcome::Result(value) if value == json!({"owned": 42}))
        );
    }
}
