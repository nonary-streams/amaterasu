//! Broker calls retain exact admission identity until cancellation is confirmed.
use amaterasu_module_sdk::{Command, Response, RuntimeClient};
use serde_json::Value;
use std::time::{Duration, Instant};

pub enum Outcome {
    Waiting,
    Result(Value),
    SettledFailure,
    Uncertain,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Admission {
    Unsubmitted,
    Rejected,
    Possible,
}

pub struct Call {
    command: Command,
    request: Option<u64>,
    cancelling: bool,
    failures: u8,
    result: Option<Value>,
    admission: Admission,
    first_sent: Option<Instant>,
    read_only: bool,
}

impl Call {
    pub fn new(payload: Value, operation: String, parent: Option<u64>) -> Self {
        Self::for_service("amaterasu.stream-session/1", payload, operation, parent)
    }

    pub fn for_service(
        service: &str,
        payload: Value,
        operation: String,
        parent: Option<u64>,
    ) -> Self {
        // An empty operation denotes exhausted local ID allocation. Never let
        // it reach the broker or reuse the last successfully allocated ID.
        let admission = if operation.is_empty() {
            Admission::Rejected
        } else {
            Admission::Unsubmitted
        };
        Self {
            command: Command::Request {
                service: service.into(),
                payload,
                operation_id: operation,
                parent,
                timeout_ms: 15_000,
            },
            request: None,
            cancelling: false,
            failures: 0,
            result: None,
            admission,
            first_sent: None,
            read_only: false,
        }
    }

    pub fn cancel(&mut self) {
        self.cancelling = true;
        self.failures = 0;
    }

    /// The caller must use this only for an observation that cannot mutate
    /// provider resources. Broker cancellation then safely retires bookkeeping.
    pub fn cancel_read_only(&mut self) {
        self.read_only = true;
        self.cancel();
    }

    pub fn retry_settlement(&mut self) {
        self.cancelling = true;
        self.failures = 0;
    }

    /// Local proof about this exact call, never an inference from cancellation.
    /// A conflict or any ambiguous submit permanently prevents this proof.
    pub fn observed_result(&self) -> Option<&Value> {
        self.result.as_ref()
    }

    pub fn proven_not_dispatched(&self) -> bool {
        matches!(self.admission, Admission::Unsubmitted | Admission::Rejected)
    }

    pub fn poll(&mut self, runtime: &RuntimeClient) -> Outcome {
        if self.admission == Admission::Rejected {
            return Outcome::SettledFailure;
        }
        if self.cancelling && self.proven_not_dispatched() {
            return Outcome::SettledFailure;
        }
        if self.failures >= 3 {
            return Outcome::Uncertain;
        }
        let Some(request) = self.request else {
            let command = if self.admission == Admission::Possible {
                if self
                    .first_sent
                    .is_some_and(|sent| sent.elapsed() >= Duration::from_secs(300))
                {
                    return Outcome::Uncertain;
                }
                let Command::Request {
                    service,
                    payload,
                    operation_id,
                    parent,
                    ..
                } = &self.command
                else {
                    return Outcome::Uncertain;
                };
                Command::LookupRequest {
                    service: service.clone(),
                    payload: payload.clone(),
                    operation_id: operation_id.clone(),
                    parent: *parent,
                }
            } else {
                self.first_sent = Some(Instant::now());
                self.command.clone()
            };
            return match runtime.send(command) {
                Ok(Response::Accepted { id }) => {
                    self.request = Some(id);
                    self.admission = Admission::Possible;
                    self.failures = 0;
                    Outcome::Waiting
                }
                Ok(Response::Error { code, .. }) => {
                    if self.admission == Admission::Possible {
                        // The host invocation is synchronous and this Call is
                        // the only submitter/releaser of its unique operation.
                        // Recheck after lookup, which itself may have stalled.
                        if code == "not_found"
                            && self
                                .first_sent
                                .is_some_and(|sent| sent.elapsed() < Duration::from_secs(300))
                        {
                            self.admission = Admission::Rejected;
                            return Outcome::SettledFailure;
                        }
                        self.failures += 1;
                        return Outcome::Waiting;
                    }
                    self.admission = if self.admission == Admission::Unsubmitted
                        && matches!(
                            code.as_str(),
                            "permission_denied"
                                | "invalid_request"
                                | "unavailable"
                                | "resource_exhausted"
                        ) {
                        Admission::Rejected
                    } else {
                        Admission::Possible
                    };
                    if self.admission == Admission::Rejected {
                        Outcome::SettledFailure
                    } else {
                        self.cancelling = true;
                        Outcome::Waiting
                    }
                }
                _ => {
                    // The command may have reached the broker. Look up its
                    // identity without dispatching a new mutation.
                    self.admission = Admission::Possible;
                    self.cancelling = true;
                    self.failures += 1;
                    Outcome::Waiting
                }
            };
        };
        if self.cancelling && self.result.is_some() {
            // A terminal provider result is stronger evidence than broker
            // cancellation. Preserve it for the caller while retrying only its
            // release, including after a lost release acknowledgement.
            return match runtime.send(Command::ReleaseResult { request }) {
                Ok(Response::Ok) => Outcome::Result(self.result.clone().expect("result retained")),
                Ok(Response::Error { code, .. }) if code == "not_found" => {
                    Outcome::Result(self.result.clone().expect("result retained"))
                }
                _ => {
                    self.failures += 1;
                    Outcome::Waiting
                }
            };
        }
        if self.cancelling && self.read_only {
            for command in [
                Command::Cancel { request },
                Command::ReleaseResult { request },
            ] {
                match runtime.send(command) {
                    Ok(Response::Ok) => {}
                    Ok(Response::Error { code, .. }) if code == "not_found" => {}
                    _ => {
                        self.failures += 1;
                        return Outcome::Waiting;
                    }
                }
            }
            return Outcome::SettledFailure;
        }
        if self.result.is_none() {
            // Keep the payload before releasing its broker record. The SDK's
            // ephemeral helper releases first and can hide a known result when
            // only that bookkeeping operation fails.
            match runtime.send(Command::TakeResult { request }) {
                Ok(Response::Pending) => return Outcome::Waiting,
                Ok(Response::Result { payload }) => self.result = Some(payload),
                _ => {
                    self.cancelling = true;
                    self.failures += 1;
                    return Outcome::Waiting;
                }
            }
        }
        match runtime.send_ephemeral(Command::ReleaseResult { request }) {
            Ok(Response::Ok) => Outcome::Result(self.result.clone().expect("result retained")),
            Ok(Response::Error { code, .. }) if code == "not_found" => {
                Outcome::Result(self.result.clone().expect("result retained"))
            }
            _ => {
                self.failures += 1;
                Outcome::Waiting
            }
        }
    }

    pub fn shutdown(&mut self, runtime: &RuntimeClient) -> Outcome {
        self.cancel();
        for _ in 0..4 {
            let outcome = self.poll(runtime);
            if !matches!(outcome, Outcome::Waiting) {
                return outcome;
            }
        }
        Outcome::Uncertain
    }
}

pub fn active(runtime: &RuntimeClient, request: u64) -> bool {
    matches!(
        runtime.send_ephemeral(Command::CheckRequest { request }),
        Ok(Response::Ok)
    )
}
pub fn complete(runtime: &RuntimeClient, request: u64, payload: Value) -> bool {
    matches!(
        runtime.send_ephemeral(Command::Complete { request, payload }),
        Ok(Response::Ok)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};
    use serde_json::json;
    use std::ffi::c_void;

    #[derive(Default)]
    struct Endpoint {
        commands: Vec<Command>,
        lose_first_admission: bool,
        fail_settlement: bool,
        admission_rejection: Option<&'static str>,
        lookup_error: Option<&'static str>,
        take_result: Option<Value>,
    }

    impl Endpoint {
        fn dispatch(&self, command: &Command) -> Response {
            match command {
                Command::Request { .. } if self.admission_rejection.is_some() => {
                    Response::error(self.admission_rejection.unwrap(), "not admitted")
                }
                Command::Request { .. } => Response::Accepted { id: 41 },
                Command::LookupRequest { .. } if self.lookup_error.is_some() => {
                    Response::error(self.lookup_error.unwrap(), "lookup failed")
                }
                Command::LookupRequest { .. } => Response::Accepted { id: 41 },
                Command::TakeResult { .. } => {
                    self.take_result
                        .as_ref()
                        .map_or(Response::Pending, |payload| Response::Result {
                            payload: payload.clone(),
                        })
                }
                Command::Cancel { .. } | Command::ReleaseResult { .. } if self.fail_settlement => {
                    Response::error("unavailable", "deliberate settlement failure")
                }
                Command::Cancel { .. } | Command::ReleaseResult { .. } => Response::Ok,
                _ => Response::error("unexpected", "unexpected test command"),
            }
        }
    }

    unsafe extern "C" fn invoke(
        context: *mut c_void,
        input: BorrowedBytes,
        reply: ReplySink,
    ) -> abi::Status {
        // SAFETY: each test keeps Endpoint exclusively alive for the client lifetime.
        let endpoint = unsafe { &mut *context.cast::<Endpoint>() };
        let Ok(bytes) = (unsafe { input.read() }) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        endpoint.commands.push(command.clone());
        if endpoint.lose_first_admission
            && endpoint.commands.len() == 1
            && matches!(command, Command::Request { .. })
        {
            return abi::STATUS_INTERNAL_ERROR;
        }
        let output = serde_json::to_vec(&endpoint.dispatch(&command)).expect("test response");
        // SAFETY: the synchronous SDK reply sink copies these bytes before return.
        unsafe { reply.send(&output) };
        abi::STATUS_OK
    }

    fn client(endpoint: &mut Endpoint) -> RuntimeClient {
        let api = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (endpoint as *mut Endpoint).cast(),
            invoke,
        };
        // SAFETY: callers keep endpoint alive and use the client synchronously.
        unsafe { RuntimeClient::from_endpoint(api) }
    }

    #[test]
    fn cancelled_unsubmitted_call_never_dispatches_a_request() {
        let mut endpoint = Endpoint::default();
        let runtime = client(&mut endpoint);
        let mut call = Call::new(json!({"action":"prepare"}), "unsubmitted".into(), None);
        call.cancel();
        assert!(matches!(call.poll(&runtime), Outcome::SettledFailure));
        assert!(endpoint.commands.is_empty());
    }

    #[test]
    fn exhausted_operation_id_never_dispatches() {
        let mut endpoint = Endpoint::default();
        let runtime = client(&mut endpoint);
        let mut call = Call::new(json!({"action":"prepare"}), String::new(), None);
        assert!(matches!(call.poll(&runtime), Outcome::SettledFailure));
        assert!(call.proven_not_dispatched());
        assert!(endpoint.commands.is_empty());
    }

    #[test]
    fn first_admission_rejection_proves_no_dispatch_only_for_known_pre_enqueue_errors() {
        for code in [
            "permission_denied",
            "invalid_request",
            "unavailable",
            "resource_exhausted",
            "conflict",
            "unexpected",
        ] {
            let mut endpoint = Endpoint {
                admission_rejection: Some(code),
                ..Endpoint::default()
            };
            let runtime = client(&mut endpoint);
            let mut call = Call::new(json!({"action":"prepare"}), "rejected".into(), None);
            let outcome = call.poll(&runtime);
            assert!(if matches!(code, "conflict" | "unexpected") {
                matches!(outcome, Outcome::Waiting)
            } else {
                matches!(outcome, Outcome::SettledFailure)
            });
            assert_eq!(
                call.proven_not_dispatched(),
                !matches!(code, "conflict" | "unexpected")
            );
        }
    }

    #[test]
    fn rejection_after_lost_admission_cannot_prove_no_dispatch() {
        let mut endpoint = Endpoint {
            lose_first_admission: true,
            lookup_error: Some("unavailable"),
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        let mut call = Call::new(json!({"action":"prepare"}), "lost".into(), None);
        assert!(matches!(call.poll(&runtime), Outcome::Waiting));
        assert!(!call.proven_not_dispatched());
        call.cancel();
        assert!(matches!(call.poll(&runtime), Outcome::Waiting));
        assert!(!call.proven_not_dispatched());
    }

    #[test]
    fn lookup_after_retention_expiry_cannot_prove_nonadmission() {
        let mut endpoint = Endpoint {
            lose_first_admission: true,
            lookup_error: Some("not_found"),
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        let mut call = Call::new(json!({"action":"prepare"}), "lost".into(), None);
        assert!(matches!(call.poll(&runtime), Outcome::Waiting));
        call.first_sent = Some(Instant::now() - Duration::from_secs(300));
        assert!(matches!(call.poll(&runtime), Outcome::Uncertain));
        assert!(!call.proven_not_dispatched());
        assert_eq!(endpoint.commands.len(), 1);
    }

    #[test]
    fn lost_admission_recovers_the_same_operation_before_settlement() {
        let mut endpoint = Endpoint {
            lose_first_admission: true,
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        let payload = json!({"action":"prepare","operation_id":"allocation"});
        let mut call = Call::new(payload.clone(), "broker-operation".into(), Some(9));

        assert!(matches!(call.poll(&runtime), Outcome::Waiting));
        assert!(matches!(call.poll(&runtime), Outcome::Waiting));
        assert!(matches!(call.poll(&runtime), Outcome::Waiting));

        let requests: Vec<_> = endpoint
            .commands
            .iter()
            .filter_map(|command| match command {
                Command::Request {
                    payload,
                    operation_id,
                    parent,
                    ..
                }
                | Command::LookupRequest {
                    payload,
                    operation_id,
                    parent,
                    ..
                } => Some((payload, operation_id, parent)),
                _ => None,
            })
            .collect();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], requests[1]);
        assert_eq!(requests[0].0, &payload);
        assert_eq!(requests[0].1, "broker-operation");
        assert_eq!(*requests[0].2, Some(9));
        assert!(matches!(
            endpoint.commands[1],
            Command::LookupRequest { .. }
        ));
        assert!(matches!(
            endpoint.commands[2],
            Command::TakeResult { request: 41 }
        ));
        assert!(!endpoint.commands.iter().any(|command| matches!(
            command,
            Command::Cancel { .. } | Command::ReleaseResult { .. }
        )));
        endpoint.take_result = Some(json!({"ok":true,"session":23}));
        assert!(matches!(call.poll(&runtime), Outcome::Result(_)));
    }

    #[test]
    fn unconfirmed_settlement_is_retained_and_can_be_retried() {
        let mut endpoint = Endpoint {
            lookup_error: Some("unavailable"),
            lose_first_admission: true,
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        let mut call = Call::new(json!({"action":"stop"}), "stop-operation".into(), None);
        assert!(matches!(call.poll(&runtime), Outcome::Waiting));
        call.cancel();
        for _ in 0..3 {
            assert!(matches!(call.poll(&runtime), Outcome::Waiting));
        }
        assert!(matches!(call.poll(&runtime), Outcome::Uncertain));

        endpoint.lookup_error = None;
        endpoint.take_result = Some(json!({"ok":true}));
        call.retry_settlement();
        assert!(matches!(call.poll(&runtime), Outcome::Waiting));
        assert!(matches!(call.poll(&runtime), Outcome::Result(_)));
        assert_eq!(
            endpoint
                .commands
                .iter()
                .filter(|command| matches!(command, Command::Request { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn cancellation_preserves_observed_result_and_only_retries_release() {
        let mut endpoint = Endpoint {
            fail_settlement: true,
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        let mut call = Call::new(json!({"action":"stop"}), "stop".into(), None);
        assert!(matches!(call.poll(&runtime), Outcome::Waiting));
        call.result = Some(json!({"ok":true,"session":23}));
        call.cancel();
        assert!(matches!(call.poll(&runtime), Outcome::Waiting));
        endpoint.fail_settlement = false;
        assert!(matches!(call.poll(&runtime), Outcome::Result(_)));
        assert_eq!(
            call.observed_result(),
            Some(&json!({"ok":true,"session":23}))
        );
        assert!(
            !endpoint
                .commands
                .iter()
                .any(|command| matches!(command, Command::Cancel { .. }))
        );
    }
}
