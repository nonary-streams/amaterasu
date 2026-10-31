//! Stages source input and commits a durably armed native delivery once.
use crate::backend::Backend;
use crate::controller::{Controller, PreparedBatch};
use crate::model::{Event, InputError, InputResult, MAX_EVENTS};
use crate::shortcut::{Disposition, Intent, Matcher};
use crate::shortcut_journal::{Admission, Record};
use crate::shortcut_store::ShortcutStore;
use serde_json::{Value, json};
use std::time::Instant;

/// Retains an in-memory native delivery. This value cannot be reconstructed
/// from a persisted receipt after a crash or an ambiguous result.
pub(crate) struct StagedBatch {
    prepared: PreparedBatch,
    matcher: Matcher,
    dispositions: Vec<Disposition>,
    intents: Vec<Intent>,
    authorized_target: Value,
    newly_armed: bool,
}

impl StagedBatch {
    /// Stage against a clone: reservation failures cannot mutate held state.
    pub fn prepare<B: Backend>(
        controller: &Controller<B>,
        matcher: &Matcher,
        caller: &str,
        token: &str,
        events: Vec<Event>,
        authorized_target: Value,
        now: Instant,
    ) -> InputResult<Self> {
        controller.authorize_batch_source(caller, token, now)?;
        if events.is_empty() || events.len() > MAX_EVENTS {
            return Err(InputError::new(
                "invalid_request",
                "input batch must contain 1..64 events",
            ));
        }
        let mut candidate = matcher.clone();
        let mut dispositions = Vec::with_capacity(events.len());
        let mut forwarded = Vec::new();
        let mut intents = Vec::new();
        for event in events {
            event.validate()?;
            let disposition = if !controller.shortcut_event_enabled(&event) {
                Disposition::Forward
            } else if let Event::Key { key, state } = &event {
                candidate
                    .edge(key, *state, |intent, _| {
                        intents.push(intent.clone());
                        Ok::<_, ()>(())
                    })
                    .map_err(|_| {
                        InputError::new(
                            "resource_exhausted",
                            "shortcut held-key capacity exhausted",
                        )
                    })?
            } else {
                Disposition::Forward
            };
            if disposition == Disposition::Forward {
                forwarded.push(event);
            }
            dispositions.push(disposition);
        }
        let prepared = controller.prepare_filtered_batch(caller, token, forwarded, now)?;
        Ok(Self {
            prepared,
            matcher: candidate,
            dispositions,
            intents,
            authorized_target,
            newly_armed: false,
        })
    }

    pub fn intents(&self) -> &[Intent] {
        &self.intents
    }

    /// A fully forwarded batch has no shortcut ownership or action effects and
    /// can use the lease's ordered, revocation-guarded native path directly.
    pub fn deliver_forwarded<B: Backend>(
        self,
        controller: &mut Controller<B>,
        now: Instant,
        guard: &mut impl FnMut() -> InputResult<()>,
    ) -> InputResult<Value> {
        if !self.intents.is_empty()
            || self.dispositions.iter().any(|d| *d != Disposition::Forward)
            || self
                .matcher
                .held_snapshot()
                .iter()
                .any(|(_, d)| *d == Disposition::Consume)
        {
            return Err(InputError::new(
                "invalid_sequence",
                "shortcut ownership requires durable admission",
            ));
        }
        Ok(controller.deliver_prepared_batch(self.prepared, now, guard))
    }

    /// Store the whole disposition and action intent with the input receipt.
    /// The supplied dispatch intents must already have approved, durable
    /// lifecycle/action reservations; unverified source claims are insufficient.
    pub fn arm(
        &mut self,
        store: &mut ShortcutStore,
        sequence: u64,
        fingerprint: Value,
        intents: Vec<crate::shortcut_journal::DispatchIntent>,
    ) -> Result<Admission, String> {
        if intents.len() != self.intents.len() {
            return Err("shortcut intent reservation count mismatch".into());
        }
        for (matched, reserved) in self.intents.iter().zip(&intents) {
            if reserved.request["action_id"] != matched.action_id
                || reserved.request["arguments"] != matched.arguments
                || reserved.request["target"] != self.authorized_target
                || reserved.request["input_reservation_id"]
                    .as_str()
                    .is_none_or(str::is_empty)
            {
                return Err("shortcut intent reservation mismatch".into());
            }
        }
        let record = Record {
            fingerprint,
            disposition: json!(self.dispositions),
            held: json!(self.matcher.held_snapshot()),
            forwarded_events: json!(self.prepared.events()),
            intents,
            phase: crate::shortcut_journal::Phase::Armed,
            result: None,
        };
        let admission = store
            .admit(sequence, record)
            .map_err(|error| format!("shortcut journal admission: {error:?}"))?;
        self.newly_armed = admission == Admission::New;
        Ok(admission)
    }

    /// Require confirmed durable admission before publishing held state or
    /// performing the native delivery. An existing receipt is status-only.
    pub fn deliver<B: Backend>(
        self,
        store: &ShortcutStore,
        sequence: u64,
        matcher: &mut Matcher,
        controller: &mut Controller<B>,
        now: Instant,
        guard: &mut impl FnMut() -> InputResult<()>,
    ) -> InputResult<Value> {
        if !self.newly_armed {
            return Err(InputError::new(
                "invalid_sequence",
                "existing shortcut receipt cannot be delivered again",
            ));
        }
        let record = store
            .journal()
            .and_then(|journal| journal.lookup(sequence))
            .ok_or_else(|| InputError::new("not_ready", "shortcut admission is not durable"))?;
        if record.phase != crate::shortcut_journal::Phase::Armed
            || record.disposition != json!(self.dispositions)
            || record.held != json!(self.matcher.held_snapshot())
            || record.forwarded_events != json!(self.prepared.events())
        {
            return Err(InputError::new(
                "invalid_sequence",
                "shortcut admission no longer matches staged input",
            ));
        }
        *matcher = self.matcher;
        let mut result = controller.deliver_prepared_batch(self.prepared, now, guard);
        if result["ok"] == true {
            let consumed = self
                .dispositions
                .iter()
                .filter(|disposition| **disposition == Disposition::Consume)
                .count();
            let suppressed = result["suppressed"].as_u64().unwrap_or(0);
            result["accepted"] = json!(self.dispositions.len());
            result["suppressed"] = json!(suppressed + consumed as u64);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Key, KeyState, Operation};
    use crate::shortcut_journal::{DispatchIntent, IntentPhase, Phase};
    use amaterasu_extension_api::{
        BindingPropagation, BindingSource, BindingTarget, KeyIdentity, Keybinding,
    };
    use amaterasu_module_sdk::{Command, ModuleResult, Response};
    use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

    struct BackendFake(Rc<RefCell<Vec<Event>>>);
    impl Backend for BackendFake {
        fn name(&self) -> &'static str {
            "test"
        }
        fn supported(&self) -> bool {
            true
        }
        fn absolute_pointer(&self) -> bool {
            true
        }
        fn key_encoding(&self) -> &'static str {
            "test"
        }
        fn validate(&self, event: &Event) -> InputResult<()> {
            event.validate()
        }
        fn start(&mut self) -> InputResult<()> {
            Ok(())
        }
        fn inject(&mut self, event: &Event) -> InputResult<()> {
            self.0.borrow_mut().push(event.clone());
            Ok(())
        }
        fn stop(&mut self) -> InputResult<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct State {
        revision: u64,
        value: Option<Value>,
        calls: BTreeMap<u64, Value>,
        next: u64,
    }
    impl State {
        fn send(&mut self, command: Command) -> ModuleResult<Response> {
            match command {
                Command::Request { payload, .. } => {
                    self.next += 1;
                    self.calls.insert(self.next, payload);
                    Ok(Response::Accepted { id: self.next })
                }
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                Command::TakeResult { request } => {
                    let request = self.calls.remove(&request).unwrap();
                    match request["action"].as_str().unwrap() {
                        "get" => Ok(Response::Result {
                            payload: json!({"ok":true,"revision":self.revision,"found":self.value.is_some(),"value":self.value}),
                        }),
                        "set" => {
                            assert_eq!(request["expected_revision"], self.revision);
                            self.value = Some(request["value"].clone());
                            self.revision += 1;
                            Ok(Response::Result {
                                payload: json!({"ok":true,"revision":self.revision}),
                            })
                        }
                        _ => panic!("unexpected state call"),
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

    fn key(code: u16, state: KeyState) -> Event {
        Event::Key {
            key: Key::LinuxEvdev { code },
            state,
        }
    }

    #[test]
    fn forwarded_fast_path_preserves_modifier_for_later_shortcut_and_obeys_guard() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let mut controller = Controller::new(BackendFake(trace.clone()), true);
        let now = Instant::now();
        let acquire: Operation =
            serde_json::from_value(json!({"action":"acquire","ttl_ms":30000})).unwrap();
        let lease = controller.handle("owner", acquire, now, &mut || Ok(()))["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        let matcher = Matcher::default();
        let denied = StagedBatch::prepare(
            &controller,
            &matcher,
            "owner",
            &lease,
            vec![key(1, KeyState::Down)],
            Value::Null,
            now,
        )
        .unwrap();
        let reply = denied
            .deliver_forwarded(&mut controller, now, &mut || {
                Err(InputError::new("revoked", "revoked"))
            })
            .unwrap();
        assert_eq!(reply["ok"], false);
        assert!(trace.borrow().is_empty());
        let acquire: Operation =
            serde_json::from_value(json!({"action":"acquire","ttl_ms":30000})).unwrap();
        let lease = controller.handle("owner", acquire, now, &mut || Ok(()))["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        let forwarded = StagedBatch::prepare(
            &controller,
            &matcher,
            "owner",
            &lease,
            vec![key(1, KeyState::Down)],
            Value::Null,
            now,
        )
        .unwrap();
        assert_eq!(
            forwarded
                .deliver_forwarded(&mut controller, now, &mut || Ok(()))
                .unwrap()["ok"],
            true
        );
        let held = controller
            .shortcut_source_held_keys("owner", &lease, now)
            .unwrap()
            .into_iter()
            .map(|key| (key, Disposition::Forward))
            .collect::<Vec<_>>();
        let mut matcher = Matcher::default();
        matcher.recover_held(&held).unwrap();
        matcher
            .replace_approved_bindings(
                &[Keybinding {
                    id: "binding".into(),
                    enabled: true,
                    source: BindingSource::SessionInput,
                    context: "game".into(),
                    trigger: KeyIdentity::LinuxEvdev { code: 2 },
                    modifiers: vec![KeyIdentity::LinuxEvdev { code: 1 }],
                    propagation: BindingPropagation::ConsumeTrigger,
                    action_id: "example/remove".into(),
                    arguments: Default::default(),
                    target: BindingTarget::CurrentSession,
                }],
                BindingSource::SessionInput,
                "game",
            )
            .unwrap();
        let matched = StagedBatch::prepare(
            &controller,
            &matcher,
            "owner",
            &lease,
            vec![key(2, KeyState::Down)],
            Value::Null,
            now,
        )
        .unwrap();
        assert_eq!(matched.intents().len(), 1);
        assert!(
            matched
                .deliver_forwarded(&mut controller, now, &mut || Ok(()))
                .is_err()
        );
        assert_eq!(trace.borrow().len(), 1);
        assert!(
            controller
                .shortcut_source_held_keys("other", &lease, now)
                .is_err()
        );
    }

    #[test]
    fn admitted_shortcut_consumes_trigger_only_after_durable_commit() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let mut controller = Controller::new(BackendFake(trace.clone()), true);
        let now = Instant::now();
        let acquire: Operation =
            serde_json::from_value(json!({"action":"acquire","ttl_ms":30000})).unwrap();
        let lease = controller.handle("owner", acquire, now, &mut || Ok(()))["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut matcher = Matcher::default();
        let binding = Keybinding {
            id: "binding".into(),
            enabled: true,
            source: BindingSource::SessionInput,
            context: "game".into(),
            trigger: KeyIdentity::LinuxEvdev { code: 2 },
            modifiers: vec![KeyIdentity::LinuxEvdev { code: 1 }],
            propagation: BindingPropagation::ConsumeTrigger,
            action_id: "example/remove".into(),
            arguments: Default::default(),
            target: BindingTarget::CurrentSession,
        };
        matcher
            .replace_approved_bindings(&[binding], BindingSource::SessionInput, "game")
            .unwrap();
        let target = json!({"kind":"session","host_epoch":"host","session_id":"session","expected_generation":1});
        let mut staged = StagedBatch::prepare(
            &controller,
            &matcher,
            "owner",
            &lease,
            vec![key(1, KeyState::Down), key(2, KeyState::Down)],
            target.clone(),
            now,
        )
        .unwrap();
        assert_eq!(staged.intents().len(), 1);
        let mut state = State::default();
        let mut store = ShortcutStore::new("input-epoch".into()).unwrap();
        state.pump(&mut store);
        let intent = DispatchIntent {
            operation_id: "action:1".into(),
            request: json!({"operation_id":"action:1","action_id":"example/remove","arguments":{},"target":target,"input_reservation_id":"approved"}),
            phase: IntentPhase::Pending,
            result: None,
        };
        assert_eq!(
            staged
                .arm(&mut store, 1, json!({"batch":1}), vec![intent])
                .unwrap(),
            Admission::New
        );
        assert!(trace.borrow().is_empty());
        state.pump(&mut store);
        let result = staged
            .deliver(
                &store,
                1,
                &mut matcher,
                &mut controller,
                now,
                &mut || Ok(()),
            )
            .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["accepted"], 2);
        assert_eq!(result["suppressed"], 1);
        assert_eq!(&*trace.borrow(), &[key(1, KeyState::Down)]);
        store.finish(1, Phase::Delivered, Some(result)).unwrap();
        state.pump(&mut store);
        assert_eq!(
            store.journal().unwrap().lookup(1).unwrap().phase,
            Phase::Delivered
        );
    }
}
