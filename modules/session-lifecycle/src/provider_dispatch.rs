//! Tick-driven calls to one already-selected, installed provider method.
//!
//! The owner must durably allocate unique operation IDs and only construct new
//! work after all lifecycle admission and resource fencing checks succeed. An
//! armed record is never automatically replayed after restart.
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_TIMEOUT_MS: u64 = 30_000;
const MAX_WIRE_BYTES: usize = 16_384;

/// Immutable selection captured from trusted installed approval, not a request payload.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProviderCall {
    pub service: String,
    pub method: String,
    pub interface_major: u32,
    pub interface_minor: u32,
    pub approval: Value,
    pub arguments: Value,
    pub timeout_ms: u64,
    pub operation_id: String,
}

/// `OutcomeUnknown` means the effect might have happened and remains fenced.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DispatchState {
    Ready,
    OutcomeUnknown,
    Completed { payload: Value },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DispatchRecord {
    pub call: ProviderCall,
    pub state: DispatchState,
}

/// Must atomically persist the entire record or fail closed. A failure may itself
/// be ambiguous; callers must fence the journal until it has been recovered.
pub trait DispatchJournal {
    fn persist(&mut self, record: &DispatchRecord) -> ModuleResult<()>;
}

/// A single active call; broker request IDs are transient and never restored.
pub struct ProviderDispatcher {
    record: DispatchRecord,
    request: Option<u64>,
    validated: bool,
    journal_failed: bool,
    release_terminal: bool,
}

impl ProviderDispatcher {
    /// `Ready` must be a newly admitted record, durably reserved by the owner.
    /// Recovered `OutcomeUnknown` is exposed but cannot be dispatched again.
    pub fn new(record: DispatchRecord) -> ModuleResult<Self> {
        validate_call(&record.call)?;
        Ok(Self {
            record,
            request: None,
            validated: false,
            journal_failed: false,
            release_terminal: false,
        })
    }

    pub fn has_request(&self) -> bool {
        self.request.is_some()
    }

    pub fn record(&self) -> &DispatchRecord {
        &self.record
    }

    /// Runs at most one broker command per tick and never waits for the provider.
    pub fn advance(
        &mut self,
        journal: &mut impl DispatchJournal,
        send: &mut (impl FnMut(Command) -> ModuleResult<Response> + ?Sized),
    ) -> ModuleResult<()> {
        if self.journal_failed {
            return Err("provider journal requires recovery".into());
        }
        if let Some(request) = self.request {
            if self.release_terminal || matches!(self.record.state, DispatchState::Completed { .. })
            {
                match send(Command::ReleaseResult { request }) {
                    Ok(Response::Ok) => self.request = None,
                    Ok(Response::Error { code, .. }) if code == "not_found" => self.request = None,
                    Ok(_) | Err(_) => return Err("provider result release not confirmed".into()),
                }
                return Ok(());
            }
            return self.poll(request, journal, send);
        }
        if self.record.state != DispatchState::Ready {
            return Ok(());
        }
        if !self.validated {
            let call = &self.record.call;
            match send(Command::QueryApiApproval {
                service: call.service.clone(),
                method: call.method.clone(),
                interface_major: call.interface_major,
                min_minor: call.interface_minor,
            })? {
                Response::Result { payload } if payload == call.approval => self.validated = true,
                _ => return Err("provider approval unavailable or changed".into()),
            }
            return Ok(());
        }
        // A crash or lost acknowledgement after this durable boundary fences the
        // operation, including the case where submission never actually arrived.
        let mut next = self.record.clone();
        next.state = DispatchState::OutcomeUnknown;
        if let Err(error) = journal.persist(&next) {
            self.journal_failed = true;
            return Err(error);
        }
        self.record = next;
        let call = &self.record.call;
        match send(Command::CallApi {
            service: call.service.clone(),
            method: call.method.clone(),
            interface_major: call.interface_major,
            interface_minor: call.interface_minor,
            payload: call.arguments.clone(),
            timeout_ms: call.timeout_ms,
            operation_id: call.operation_id.clone(),
            parent: None,
        }) {
            Ok(Response::Accepted { id }) => self.request = Some(id),
            Ok(_) | Err(_) => return Err("provider submission outcome unknown".into()),
        }
        Ok(())
    }

    fn poll(
        &mut self,
        request: u64,
        journal: &mut impl DispatchJournal,
        send: &mut (impl FnMut(Command) -> ModuleResult<Response> + ?Sized),
    ) -> ModuleResult<()> {
        match send(Command::TakeResult { request }) {
            Ok(Response::Pending) => Ok(()),
            Ok(Response::Result { payload }) => {
                let mut next = self.record.clone();
                next.state = DispatchState::Completed { payload };
                if let Err(error) = journal.persist(&next) {
                    self.journal_failed = true;
                    return Err(error);
                }
                self.record = next;
                Ok(())
            }
            // These errors are stored terminal results in the broker. The
            // previously persisted OutcomeUnknown remains the effect outcome.
            Ok(Response::Error { code, .. })
                if code == "deadline_exceeded" || code == "cancelled" =>
            {
                self.release_terminal = true;
                Ok(())
            }
            Ok(Response::Error { code, .. }) if code == "not_found" => {
                self.request = None;
                Err("provider result expired; reconciliation required".into())
            }
            // A transport error does not establish whether the request is still
            // pending or has a result. Keep its ID for the next tick.
            Ok(_) | Err(_) => Err("provider result unavailable; retry required".into()),
        }
    }
}

fn validate_call(call: &ProviderCall) -> ModuleResult<()> {
    if call.service.is_empty()
        || call.method.is_empty()
        || call.operation_id.is_empty()
        || call.operation_id.len() > 128
        || call.interface_major == 0
        || call.timeout_ms == 0
        || call.timeout_ms > MAX_TIMEOUT_MS
        || !call.arguments.is_object()
        || serde_json::to_vec(&call.arguments)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_WIRE_BYTES
    {
        return Err("invalid provider call".into());
    }
    let Some(method) = call.approval.get("method") else {
        return Err("invalid provider approval".into());
    };
    if call
        .approval
        .get("provider")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
        || method.get("service").and_then(Value::as_str) != Some(call.service.as_str())
        || method.get("method").and_then(Value::as_str) != Some(call.method.as_str())
        || method.get("interface_major").and_then(Value::as_u64)
            != Some(u64::from(call.interface_major))
        || method.get("interface_minor").and_then(Value::as_u64)
            != Some(u64::from(call.interface_minor))
    {
        return Err("provider approval does not match selected method".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Journal {
        records: Vec<DispatchRecord>,
        fail: bool,
    }
    impl DispatchJournal for Journal {
        fn persist(&mut self, record: &DispatchRecord) -> ModuleResult<()> {
            if self.fail {
                return Err("write failed".into());
            }
            self.records.push(record.clone());
            Ok(())
        }
    }
    fn record() -> DispatchRecord {
        DispatchRecord {
            call: ProviderCall {
                service: "provider/1".into(),
                method: "prepare".into(),
                interface_major: 1,
                interface_minor: 0,
                approval: json!({"provider":"provider", "method":{
                    "service":"provider/1", "method":"prepare", "interface_major":1, "interface_minor":0
                }}),
                arguments: json!({"resource":"display"}),
                timeout_ms: 5000,
                operation_id: "op-1".into(),
            },
            state: DispatchState::Ready,
        }
    }
    struct Broker {
        replies: VecDeque<ModuleResult<Response>>,
        commands: Vec<Command>,
    }
    impl Broker {
        fn send(&mut self, command: Command) -> ModuleResult<Response> {
            self.commands.push(command);
            self.replies.pop_front().expect("unexpected broker command")
        }
    }
    fn advance(
        dispatcher: &mut ProviderDispatcher,
        journal: &mut Journal,
        broker: &mut Broker,
    ) -> ModuleResult<()> {
        dispatcher.advance(journal, &mut |command| broker.send(command))
    }
    fn selected() -> Response {
        Response::Result {
            payload: record().call.approval,
        }
    }

    #[test]
    fn persists_before_submission_and_terminal_before_release() {
        let mut dispatcher = ProviderDispatcher::new(record()).unwrap();
        let mut journal = Journal::default();
        let mut broker = Broker {
            replies: VecDeque::from([
                Ok(selected()),
                Ok(Response::Accepted { id: 4 }),
                Ok(Response::Pending),
                Ok(Response::Result {
                    payload: json!({"status":"pending", "receipt":"r"}),
                }),
                Ok(Response::Ok),
            ]),
            commands: vec![],
        };
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        assert_eq!(journal.records[0].state, DispatchState::OutcomeUnknown);
        assert!(matches!(
            broker.commands[1],
            Command::CallApi { parent: None, .. }
        ));
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        assert_eq!(
            journal.records[1].state,
            DispatchState::Completed {
                payload: json!({"status":"pending", "receipt":"r"})
            }
        );
        assert_eq!(broker.commands.len(), 4);
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        assert!(matches!(
            broker.commands[4],
            Command::ReleaseResult { request: 4 }
        ));
    }

    #[test]
    fn unknown_submission_and_restart_never_replay() {
        let mut dispatcher = ProviderDispatcher::new(record()).unwrap();
        let mut journal = Journal::default();
        let mut broker = Broker {
            replies: VecDeque::from([Ok(selected()), Err("lost ack".into())]),
            commands: vec![],
        };
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        assert!(advance(&mut dispatcher, &mut journal, &mut broker).is_err());
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        let mut recovered = ProviderDispatcher::new(journal.records[0].clone()).unwrap();
        advance(&mut recovered, &mut journal, &mut broker).unwrap();
        assert_eq!(broker.commands.len(), 2);
    }

    #[test]
    fn failed_terminal_persistence_fences_and_keeps_result() {
        let mut dispatcher = ProviderDispatcher::new(record()).unwrap();
        let mut journal = Journal::default();
        let mut broker = Broker {
            replies: VecDeque::from([
                Ok(selected()),
                Ok(Response::Accepted { id: 7 }),
                Ok(Response::Result {
                    payload: json!({"ok":true}),
                }),
                Ok(Response::Ok),
            ]),
            commands: vec![],
        };
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        journal.fail = true;
        assert!(advance(&mut dispatcher, &mut journal, &mut broker).is_err());
        assert_eq!(dispatcher.record().state, DispatchState::OutcomeUnknown);
        journal.fail = false;
        assert!(advance(&mut dispatcher, &mut journal, &mut broker).is_err());
        assert_eq!(broker.commands.len(), 3);
        assert_eq!(dispatcher.record().state, DispatchState::OutcomeUnknown);
    }

    #[test]
    fn changed_approval_or_failed_intent_write_never_submit() {
        let mut dispatcher = ProviderDispatcher::new(record()).unwrap();
        let mut journal = Journal::default();
        let mut changed = Broker {
            replies: VecDeque::from([Ok(Response::Result { payload: json!({}) })]),
            commands: vec![],
        };
        assert!(advance(&mut dispatcher, &mut journal, &mut changed).is_err());
        assert_eq!(changed.commands.len(), 1);
        let mut broker = Broker {
            replies: VecDeque::from([Ok(selected())]),
            commands: vec![],
        };
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        journal.fail = true;
        assert!(advance(&mut dispatcher, &mut journal, &mut broker).is_err());
        assert_eq!(broker.commands.len(), 1);
    }
    #[test]
    fn poll_transport_failure_retains_request_and_recovers_result() {
        let mut dispatcher = ProviderDispatcher::new(record()).unwrap();
        let mut journal = Journal::default();
        let mut broker = Broker {
            replies: VecDeque::from([
                Ok(selected()),
                Ok(Response::Accepted { id: 8 }),
                Err("transport failed".into()),
                Ok(Response::Pending),
                Ok(Response::Result {
                    payload: json!({"ok":true}),
                }),
                Ok(Response::Ok),
            ]),
            commands: vec![],
        };
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        assert!(advance(&mut dispatcher, &mut journal, &mut broker).is_err());
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
        assert_eq!(
            dispatcher.record().state,
            DispatchState::Completed {
                payload: json!({"ok":true})
            }
        );
        assert!(
            broker.commands[2..5]
                .iter()
                .all(|command| matches!(command, Command::TakeResult { request: 8 }))
        );
        advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
    }

    #[test]
    fn terminal_error_releases_result_after_unknown_and_retries_release() {
        for terminal in ["deadline_exceeded", "cancelled"] {
            let mut dispatcher = ProviderDispatcher::new(record()).unwrap();
            let mut journal = Journal::default();
            let mut broker = Broker {
                replies: VecDeque::from([
                    Ok(selected()),
                    Ok(Response::Accepted { id: 9 }),
                    Ok(Response::error(terminal, "terminal")),
                    Err("release acknowledgement lost".into()),
                    Ok(Response::error("not_found", "already released")),
                ]),
                commands: vec![],
            };
            advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
            advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
            advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
            assert_eq!(journal.records[0].state, DispatchState::OutcomeUnknown);
            assert!(advance(&mut dispatcher, &mut journal, &mut broker).is_err());
            advance(&mut dispatcher, &mut journal, &mut broker).unwrap();
            assert!(matches!(
                broker.commands[3],
                Command::ReleaseResult { request: 9 }
            ));
            assert!(matches!(
                broker.commands[4],
                Command::ReleaseResult { request: 9 }
            ));
            assert_eq!(dispatcher.record().state, DispatchState::OutcomeUnknown);
        }
    }
}
