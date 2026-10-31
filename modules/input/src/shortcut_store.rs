//! Asynchronous state-service persistence for shortcut input receipts.
//!
//! A proposed journal is never visible to the caller until the state service
//! confirms the CAS. Lost or ambiguous writes fence the store; they do not
//! authorize native input delivery.
use crate::shortcut_journal::{Admission, IntentPhase, Journal, JournalError, Phase, Record};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};

const SERVICE: &str = "amaterasu.state/1";
const TIMEOUT_MS: u64 = 5_000;

enum Pending {
    Submit {
        payload: Value,
        candidate: Option<Journal>,
    },
    Wait {
        request: u64,
        candidate: Option<Journal>,
    },
}

pub(crate) struct ShortcutStore {
    key: String,
    epoch: String,
    instance: String,
    operation: u64,
    storage_revision: u64,
    journal: Option<Journal>,
    pending: Option<Pending>,
    fenced: bool,
}

impl ShortcutStore {
    pub fn new(epoch: String) -> Result<Self, String> {
        Journal::new(epoch.clone()).map_err(|_| "invalid shortcut epoch")?;
        // State keys have a 256-byte bound; reject before issuing requests.
        let key = format!("shortcut-journal/{epoch}");
        if key.len() > 256 {
            return Err("shortcut epoch is too long".into());
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| "shortcut operation identity unavailable")?;
        let instance = random.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self {
            key: key.clone(),
            epoch,
            instance,
            operation: 0,
            storage_revision: 0,
            journal: None,
            pending: Some(Pending::Submit {
                payload: json!({"action":"get","key":key}),
                candidate: None,
            }),
            fenced: false,
        })
    }

    pub fn ready(&self) -> bool {
        !self.fenced && self.pending.is_none() && self.journal.is_some()
    }
    pub fn fenced(&self) -> bool {
        self.fenced
    }
    pub fn journal(&self) -> Option<&Journal> {
        self.ready().then_some(self.journal.as_ref()).flatten()
    }

    /// Stage an admission. Native delivery is permitted only after `ready()`
    /// returns true and the confirmed journal contains the new record.
    pub fn admit(&mut self, sequence: u64, record: Record) -> Result<Admission, JournalError> {
        let Some(current) = self.journal() else {
            return Err(JournalError::Invalid);
        };
        let mut candidate = current.clone();
        let admission = candidate.admit(current.revision, sequence, record)?;
        if admission == Admission::New {
            self.write(candidate);
        }
        Ok(admission)
    }

    pub fn finish(
        &mut self,
        sequence: u64,
        phase: Phase,
        result: Option<Value>,
    ) -> Result<(), JournalError> {
        let Some(current) = self.journal() else {
            return Err(JournalError::Invalid);
        };
        let mut candidate = current.clone();
        candidate.finish(current.revision, sequence, phase, result)?;
        self.write(candidate);
        Ok(())
    }

    pub fn transition_intent(
        &mut self,
        sequence: u64,
        index: usize,
        phase: IntentPhase,
        result: Option<Value>,
    ) -> Result<(), JournalError> {
        let Some(current) = self.journal() else {
            return Err(JournalError::Invalid);
        };
        let mut candidate = current.clone();
        candidate.transition_intent(current.revision, sequence, index, phase, result)?;
        self.write(candidate);
        Ok(())
    }

    fn write(&mut self, candidate: Journal) {
        self.pending = Some(Pending::Submit {
            payload: json!({"action":"set", "key":self.key, "value":candidate, "expected_revision":self.storage_revision}),
            candidate: Some(candidate),
        });
    }

    /// Advance one broker step outside the input callback.
    pub fn advance(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        match pending {
            Pending::Submit { payload, candidate } => {
                self.operation = match self.operation.checked_add(1) {
                    Some(value) => value,
                    None => {
                        self.fenced = true;
                        return;
                    }
                };
                let operation_id = format!("shortcut-state-{}-{}", self.instance, self.operation);
                match send(Command::Request {
                    service: SERVICE.into(),
                    payload,
                    timeout_ms: TIMEOUT_MS,
                    operation_id,
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => {
                        self.pending = Some(Pending::Wait {
                            request: id,
                            candidate,
                        })
                    }
                    _ => self.fenced = true,
                }
            }
            Pending::Wait { request, candidate } => match send(Command::TakeResult { request }) {
                Ok(Response::Pending) => self.pending = Some(Pending::Wait { request, candidate }),
                Ok(Response::Result { payload }) => {
                    if !matches!(send(Command::ReleaseResult { request }), Ok(Response::Ok)) {
                        self.fenced = true;
                        return;
                    }
                    if payload["ok"] != true {
                        self.fenced = true;
                        return;
                    }
                    let Some(revision) = payload["revision"].as_u64() else {
                        self.fenced = true;
                        return;
                    };
                    self.storage_revision = revision;
                    if let Some(candidate) = candidate {
                        self.journal = Some(candidate);
                    } else {
                        let Some(found) = payload["found"].as_bool() else {
                            self.fenced = true;
                            return;
                        };
                        let mut journal = if found {
                            match serde_json::from_value::<Journal>(payload["value"].clone()) {
                                Ok(value) => value,
                                Err(_) => {
                                    self.fenced = true;
                                    return;
                                }
                            }
                        } else {
                            match Journal::new(self.epoch.clone()) {
                                Ok(value) => value,
                                Err(_) => {
                                    self.fenced = true;
                                    return;
                                }
                            }
                        };
                        if journal.epoch != self.epoch || journal.validate().is_err() {
                            self.fenced = true;
                            return;
                        }
                        let previous = journal.revision;
                        match journal.recover(previous) {
                            Ok(true) => self.write(journal),
                            Ok(false) => self.journal = Some(journal),
                            Err(_) => self.fenced = true,
                        }
                    }
                }
                _ => self.fenced = true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcut_journal::{DispatchIntent, IntentPhase};
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct StateService {
        revision: u64,
        value: Option<Value>,
        requests: BTreeMap<u64, Value>,
        next: u64,
        lose_write_reply: bool,
    }

    impl StateService {
        fn send(&mut self, command: Command) -> ModuleResult<Response> {
            match command {
                Command::Request {
                    service, payload, ..
                } if service == SERVICE => {
                    self.next += 1;
                    self.requests.insert(self.next, payload);
                    Ok(Response::Accepted { id: self.next })
                }
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                Command::TakeResult { request } => {
                    let payload = self.requests.remove(&request).unwrap();
                    match payload["action"].as_str().unwrap() {
                        "get" => Ok(Response::Result {
                            payload: json!({"ok":true,"revision":self.revision,"found":self.value.is_some(),"value":self.value}),
                        }),
                        "set" => {
                            assert_eq!(payload["expected_revision"], self.revision);
                            self.value = Some(payload["value"].clone());
                            self.revision += 1;
                            if self.lose_write_reply {
                                return Err("lost reply".into());
                            }
                            Ok(Response::Result {
                                payload: json!({"ok":true,"revision":self.revision}),
                            })
                        }
                        _ => panic!("unexpected operation"),
                    }
                }
                _ => panic!("unexpected command"),
            }
        }
        fn pump(&mut self, store: &mut ShortcutStore) {
            for _ in 0..8 {
                if store.ready() || store.fenced() {
                    return;
                }
                store.advance(&mut |command| self.send(command));
            }
            panic!("store did not settle");
        }
    }

    fn record() -> Record {
        Record {
            fingerprint: json!({"batch":1}),
            disposition: json!(["consume"]),
            held: json!([]),
            forwarded_events: json!([]),
            intents: vec![DispatchIntent {
                operation_id: "action:1".into(),
                request: json!({"operation_id":"action:1"}),
                phase: IntentPhase::Pending,
                result: None,
            }],
            phase: Phase::Armed,
            result: None,
        }
    }

    #[test]
    fn admission_waits_for_state_commit_and_restart_never_rearms_delivery() {
        let mut state = StateService::default();
        let mut store = ShortcutStore::new("epoch".into()).unwrap();
        state.pump(&mut store);
        assert_eq!(store.admit(1, record()).unwrap(), Admission::New);
        assert!(!store.ready());
        state.pump(&mut store);
        assert_eq!(
            store.journal().unwrap().lookup(1).unwrap().phase,
            Phase::Armed
        );
        let mut restarted = ShortcutStore::new("epoch".into()).unwrap();
        state.pump(&mut restarted);
        assert_eq!(
            restarted.journal().unwrap().lookup(1).unwrap().phase,
            Phase::RecoveryRequired
        );
        assert!(matches!(
            restarted.admit(1, record()),
            Ok(Admission::Existing(_))
        ));
    }

    #[test]
    fn ambiguous_write_never_authorizes_native_delivery() {
        let mut state = StateService::default();
        let mut store = ShortcutStore::new("epoch".into()).unwrap();
        state.pump(&mut store);
        state.lose_write_reply = true;
        store.admit(1, record()).unwrap();
        state.pump(&mut store);
        assert!(store.fenced());
        assert!(store.journal().is_none());
        assert!(state.value.is_some());
    }
}
