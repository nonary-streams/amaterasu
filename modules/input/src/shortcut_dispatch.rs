//! Dispatch an already reserved shortcut only after durable input delivery.
use crate::shortcut_journal::{IntentPhase, Phase};
use crate::shortcut_reservation::commit_request;
use crate::shortcut_store::ShortcutStore;
use amaterasu_extension_api::ActionInvoke;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const ACTIONS: &str = "amaterasu.extension-actions/1";
const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";

enum PhaseWork {
    MarkDispatching,
    WaitDispatching,
    Commit,
    CommitWait(u64),
    Status,
    StatusWait(u64),
    MarkResult {
        receipt: Value,
        uncertain: bool,
    },
    WaitResult {
        receipt: Value,
        uncertain: bool,
    },
    Finish {
        receipt: Value,
        uncertain: bool,
    },
    FinishWait {
        request: u64,
        receipt: Value,
        uncertain: bool,
    },
    MarkTerminal(Value),
    WaitTerminal,
    CloseEpoch,
    CloseWait(u64),
    RetireEpoch,
    RetireWait(u64),
    Done,
}

pub(crate) struct DispatchJob {
    sequence: u64,
    index: usize,
    invoke: ActionInvoke,
    instance: String,
    call_sequence: u64,
    phase: PhaseWork,
    deadline: Instant,
}

impl DispatchJob {
    pub fn new(store: &ShortcutStore, sequence: u64, index: usize) -> Result<Self, String> {
        let record = store
            .journal()
            .and_then(|journal| journal.lookup(sequence))
            .ok_or("input receipt is not durable")?;
        if record.phase != Phase::Delivered {
            return Err("input delivery is not confirmed".into());
        }
        let intent = record.intents.get(index).ok_or("action intent not found")?;
        let initial_phase = match intent.phase {
            IntentPhase::Pending => PhaseWork::MarkDispatching,
            IntentPhase::Finishing => PhaseWork::Finish {
                receipt: intent.result.clone().ok_or("missing action receipt")?,
                uncertain: false,
            },
            _ => return Err("action intent already attempted".into()),
        };
        let invoke: ActionInvoke = serde_json::from_value(intent.request.clone())
            .map_err(|_| "invalid persisted action")?;
        if invoke.operation_id != intent.operation_id
            || invoke
                .input_reservation_id
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err("invalid persisted action reservation".into());
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| "dispatch identity unavailable")?;
        Ok(Self {
            sequence,
            index,
            invoke,
            instance: random.iter().map(|b| format!("{b:02x}")).collect(),
            call_sequence: 0,
            phase: initial_phase,
            deadline: Instant::now() + Duration::from_secs(30),
        })
    }

    fn request(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        service: &str,
        payload: Value,
    ) -> Result<u64, String> {
        self.call_sequence = self
            .call_sequence
            .checked_add(1)
            .ok_or("dispatch request sequence exhausted")?;
        match send(Command::Request {
            service: service.into(),
            payload,
            timeout_ms: 5_000,
            operation_id: format!("shortcut-dispatch-{}-{}", self.instance, self.call_sequence),
            parent: None,
        }) {
            Ok(Response::Accepted { id }) => Ok(id),
            _ => Err("shortcut dispatch request unavailable".into()),
        }
    }

    /// Advances persistence and broker calls outside the input callback.
    /// Unknown commit outcomes are looked up, never committed again.
    pub fn advance(
        &mut self,
        store: &mut ShortcutStore,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Result<bool, String> {
        store.advance(send);
        if Instant::now() >= self.deadline {
            return Err("shortcut dispatch deadline exceeded; reconciliation required".into());
        }
        if store.fenced() {
            return Err("shortcut state is fenced".into());
        }
        let phase = std::mem::replace(&mut self.phase, PhaseWork::Done);
        self.phase = match phase {
            PhaseWork::MarkDispatching => {
                if !store.ready() {
                    PhaseWork::MarkDispatching
                } else {
                    store
                        .transition_intent(
                            self.sequence,
                            self.index,
                            IntentPhase::Dispatching,
                            None,
                        )
                        .map_err(|e| format!("action journal: {e:?}"))?;
                    PhaseWork::WaitDispatching
                }
            }
            PhaseWork::WaitDispatching => {
                if store.ready() {
                    PhaseWork::Commit
                } else {
                    PhaseWork::WaitDispatching
                }
            }
            PhaseWork::Commit => match self.request(send, ACTIONS, commit_request(&self.invoke)) {
                Ok(id) => PhaseWork::CommitWait(id),
                Err(_) => PhaseWork::Status,
            },
            PhaseWork::CommitWait(id) => match send(Command::TakeResult { request: id }) {
                Ok(Response::Pending) => PhaseWork::CommitWait(id),
                Ok(Response::Result { payload }) => {
                    if matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    ) {
                        self.assess(payload)
                    } else {
                        PhaseWork::Status
                    }
                }
                _ => PhaseWork::Status,
            },
            PhaseWork::Status => {
                let payload = json!({"operation":"status","epoch":self.invoke.epoch,"sequence":self.invoke.sequence});
                match self.request(send, ACTIONS, payload) {
                    Ok(id) => PhaseWork::StatusWait(id),
                    Err(_) => PhaseWork::MarkResult {
                        receipt: json!({"status":"recovery_required"}),
                        uncertain: true,
                    },
                }
            }
            PhaseWork::StatusWait(id) => match send(Command::TakeResult { request: id }) {
                Ok(Response::Pending) => PhaseWork::StatusWait(id),
                Ok(Response::Result { payload }) => {
                    if matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    ) {
                        self.assess(payload)
                    } else {
                        PhaseWork::MarkResult {
                            receipt: json!({"status":"recovery_required"}),
                            uncertain: true,
                        }
                    }
                }
                _ => PhaseWork::MarkResult {
                    receipt: json!({"status":"recovery_required"}),
                    uncertain: true,
                },
            },
            PhaseWork::MarkResult { receipt, uncertain } => {
                if !store.ready() {
                    PhaseWork::MarkResult { receipt, uncertain }
                } else {
                    let next = if uncertain {
                        IntentPhase::RecoveryRequired
                    } else {
                        IntentPhase::Finishing
                    };
                    store
                        .transition_intent(self.sequence, self.index, next, Some(receipt.clone()))
                        .map_err(|e| format!("action journal: {e:?}"))?;
                    PhaseWork::WaitResult { receipt, uncertain }
                }
            }
            PhaseWork::WaitResult { receipt, uncertain } => {
                if store.ready() {
                    PhaseWork::Finish { receipt, uncertain }
                } else {
                    PhaseWork::WaitResult { receipt, uncertain }
                }
            }
            PhaseWork::Finish { receipt, uncertain } => {
                let outcome = if uncertain {
                    "recovery_required"
                } else if receipt["status"] == "succeeded" {
                    "completed"
                } else {
                    "cancelled"
                };
                let payload = json!({"operation":"finish_input_action","operation_id":self.invoke.operation_id,
                    "reservation_id":self.invoke.input_reservation_id,"outcome":outcome});
                match self.request(send, LIFECYCLE, payload) {
                    Ok(request) => PhaseWork::FinishWait {
                        request,
                        receipt,
                        uncertain,
                    },
                    Err(_) => return Err("shortcut reservation finish unresolved".into()),
                }
            }
            PhaseWork::FinishWait {
                request,
                receipt,
                uncertain,
            } => match send(Command::TakeResult { request }) {
                Ok(Response::Pending) => PhaseWork::FinishWait {
                    request,
                    receipt,
                    uncertain,
                },
                Ok(Response::Result { payload })
                    if matches!(send(Command::ReleaseResult { request }), Ok(Response::Ok))
                        && payload["status"] == "succeeded" =>
                {
                    if uncertain {
                        return Err("action outcome requires recovery".into());
                    }
                    PhaseWork::MarkTerminal(receipt)
                }
                _ => return Err("shortcut reservation finish unresolved".into()),
            },
            PhaseWork::MarkTerminal(receipt) => {
                if !store.ready() {
                    PhaseWork::MarkTerminal(receipt)
                } else {
                    store
                        .transition_intent(
                            self.sequence,
                            self.index,
                            IntentPhase::Terminal,
                            Some(receipt),
                        )
                        .map_err(|e| format!("action journal: {e:?}"))?;
                    PhaseWork::WaitTerminal
                }
            }
            PhaseWork::WaitTerminal => {
                if store.ready() {
                    PhaseWork::CloseEpoch
                } else {
                    PhaseWork::WaitTerminal
                }
            }
            PhaseWork::CloseEpoch => {
                let payload = json!({"operation":"close_epoch","epoch":self.invoke.epoch});
                match self.request(send, ACTIONS, payload) {
                    Ok(id) => PhaseWork::CloseWait(id),
                    Err(_) => return Err("action epoch close unresolved".into()),
                }
            }
            PhaseWork::CloseWait(id) => match send(Command::TakeResult { request: id }) {
                Ok(Response::Pending) => PhaseWork::CloseWait(id),
                Ok(Response::Result { payload })
                    if matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    ) && payload["status"] == "ok" =>
                {
                    PhaseWork::RetireEpoch
                }
                _ => return Err("action epoch close unresolved".into()),
            },
            PhaseWork::RetireEpoch => {
                let payload = json!({"operation":"retire_epoch","epoch":self.invoke.epoch});
                match self.request(send, ACTIONS, payload) {
                    Ok(id) => PhaseWork::RetireWait(id),
                    Err(_) => return Err("action epoch retirement unresolved".into()),
                }
            }
            PhaseWork::RetireWait(id) => match send(Command::TakeResult { request: id }) {
                Ok(Response::Pending) => PhaseWork::RetireWait(id),
                Ok(Response::Result { payload })
                    if matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    ) && payload["status"] == "ok" =>
                {
                    PhaseWork::Done
                }
                _ => return Err("action epoch retirement unresolved".into()),
            },
            PhaseWork::Done => return Ok(true),
        };
        Ok(matches!(self.phase, PhaseWork::Done))
    }

    fn assess(&self, receipt: Value) -> PhaseWork {
        if receipt["operation_id"] != self.invoke.operation_id {
            return PhaseWork::MarkResult {
                receipt: json!({"status":"recovery_required"}),
                uncertain: true,
            };
        }
        match receipt["status"].as_str() {
            Some("queued" | "running" | "reserved") => PhaseWork::Status,
            Some("succeeded" | "rejected" | "cancelled" | "failed") => PhaseWork::MarkResult {
                receipt,
                uncertain: false,
            },
            _ => PhaseWork::MarkResult {
                receipt,
                uncertain: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcut_journal::{DispatchIntent, Record};
    use amaterasu_extension_api::ActionTarget;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Host {
        revision: u64,
        value: Option<Value>,
        calls: BTreeMap<u64, (String, Value)>,
        next: u64,
        actions: usize,
        lose_commit_reply: bool,
        status_calls: usize,
    }
    impl Host {
        fn send(&mut self, command: Command) -> ModuleResult<Response> {
            match command {
                Command::Request {
                    service, payload, ..
                } => {
                    self.next += 1;
                    self.calls.insert(self.next, (service, payload));
                    Ok(Response::Accepted { id: self.next })
                }
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                Command::TakeResult { request } => {
                    let (service, payload) = self.calls.remove(&request).unwrap();
                    let reply = if service == "amaterasu.state/1" {
                        match payload["action"].as_str().unwrap() {
                            "get" => {
                                json!({"ok":true,"revision":self.revision,"found":self.value.is_some(),"value":self.value})
                            }
                            "set" => {
                                assert_eq!(payload["expected_revision"], self.revision);
                                self.value = Some(payload["value"].clone());
                                self.revision += 1;
                                json!({"ok":true,"revision":self.revision})
                            }
                            _ => panic!("unexpected state request"),
                        }
                    } else if service == ACTIONS {
                        if payload["operation"] == "commit_input" {
                            self.actions += 1;
                            if self.lose_commit_reply {
                                return Err("lost reply".into());
                            }
                        } else if payload["operation"] == "status" {
                            self.status_calls += 1;
                        } else {
                            assert!(matches!(
                                payload["operation"].as_str(),
                                Some("close_epoch" | "retire_epoch")
                            ));
                            return Ok(Response::Result {
                                payload: json!({"status":"ok"}),
                            });
                        }
                        json!({"status":"succeeded","operation_id":"action:1"})
                    } else {
                        assert_eq!(service, LIFECYCLE);
                        assert_eq!(payload["outcome"], "completed");
                        json!({"status":"succeeded"})
                    };
                    Ok(Response::Result { payload: reply })
                }
                _ => panic!("unexpected command"),
            }
        }
        fn pump(&mut self, store: &mut ShortcutStore) {
            for _ in 0..8 {
                if store.ready() || store.fenced() {
                    return;
                }
                store.advance(&mut |c| self.send(c));
            }
            panic!("store timeout")
        }
    }

    #[test]
    fn action_commit_waits_for_durable_delivered_and_dispatching_receipts() {
        let invoke = ActionInvoke {
            epoch: "action".into(),
            sequence: 1,
            operation_id: "action:1".into(),
            action_id: "test/remove".into(),
            target: ActionTarget::Session {
                host_epoch: "host".into(),
                session_id: "session".into(),
                expected_generation: 1,
            },
            arguments: json!({}),
            expected_settings_revision: None,
            input_reservation_id: Some("permit".into()),
        };
        let record = Record {
            fingerprint: json!("batch"),
            disposition: json!([]),
            held: json!([]),
            forwarded_events: json!([]),
            intents: vec![DispatchIntent {
                operation_id: invoke.operation_id.clone(),
                request: serde_json::to_value(&invoke).unwrap(),
                phase: IntentPhase::Pending,
                result: None,
            }],
            phase: Phase::Armed,
            result: None,
        };
        let mut host = Host {
            lose_commit_reply: true,
            ..Host::default()
        };
        let mut store = ShortcutStore::new("input".into()).unwrap();
        host.pump(&mut store);
        store.admit(1, record).unwrap();
        host.pump(&mut store);
        assert!(DispatchJob::new(&store, 1, 0).is_err());
        store
            .finish(1, Phase::Delivered, Some(json!({"ok":true})))
            .unwrap();
        host.pump(&mut store);
        let mut job = DispatchJob::new(&store, 1, 0).unwrap();
        for _ in 0..30 {
            if job.advance(&mut store, &mut |c| host.send(c)).unwrap() {
                break;
            }
        }
        assert_eq!(host.actions, 1);
        assert_eq!(host.status_calls, 1);
        assert_eq!(
            store.journal().unwrap().lookup(1).unwrap().intents[0].phase,
            IntentPhase::Terminal
        );
        assert!(DispatchJob::new(&store, 1, 0).is_err());
    }
    #[test]
    fn restart_finishes_retained_reservation_without_recommitting_action() {
        let invoke = ActionInvoke {
            epoch: "action".into(),
            sequence: 1,
            operation_id: "action:1".into(),
            action_id: "test/remove".into(),
            target: ActionTarget::Session {
                host_epoch: "host".into(),
                session_id: "session".into(),
                expected_generation: 1,
            },
            arguments: json!({}),
            expected_settings_revision: None,
            input_reservation_id: Some("permit".into()),
        };
        let record = Record {
            fingerprint: json!("batch"),
            disposition: json!([]),
            held: json!([]),
            forwarded_events: json!([]),
            intents: vec![DispatchIntent {
                operation_id: invoke.operation_id.clone(),
                request: serde_json::to_value(invoke).unwrap(),
                phase: IntentPhase::Pending,
                result: None,
            }],
            phase: Phase::Armed,
            result: None,
        };
        let mut host = Host::default();
        let mut store = ShortcutStore::new("input".into()).unwrap();
        host.pump(&mut store);
        store.admit(1, record).unwrap();
        host.pump(&mut store);
        store
            .finish(1, Phase::Delivered, Some(json!({"ok":true})))
            .unwrap();
        host.pump(&mut store);
        store
            .transition_intent(1, 0, IntentPhase::Dispatching, None)
            .unwrap();
        host.pump(&mut store);
        let receipt = json!({"status":"succeeded","operation_id":"action:1"});
        store
            .transition_intent(1, 0, IntentPhase::Finishing, Some(receipt))
            .unwrap();
        host.pump(&mut store);
        let mut restarted = ShortcutStore::new("input".into()).unwrap();
        host.pump(&mut restarted);
        let mut job = DispatchJob::new(&restarted, 1, 0).unwrap();
        let mut done = false;
        for _ in 0..30 {
            if job
                .advance(&mut restarted, &mut |command| host.send(command))
                .unwrap()
            {
                done = true;
                break;
            }
        }
        assert!(done);
        assert_eq!(host.actions, 0);
        assert_eq!(
            restarted.journal().unwrap().lookup(1).unwrap().intents[0].phase,
            IntentPhase::Terminal
        );
    }
}
