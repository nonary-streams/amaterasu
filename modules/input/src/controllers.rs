//! Bounded per-stream input state. Native devices and feedback stay owned by
//! one controller; selecting a context never transfers a lease to another one.
use crate::{
    backend::Backend,
    controller::Controller,
    model::{InputError, InputResult},
    product_settings::{InputPolicy, PolicyTarget},
};
use amaterasu_module_sdk::ModuleResult;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
    time::{Duration, Instant},
};

const MAX_CONTEXTS: usize = 128;
const MAX_LIVE_CONTEXTS: usize = if cfg!(target_os = "linux") { 8 } else { 1 };
const RETENTION: Duration = Duration::from_secs(300);

struct Context<B: Backend> {
    controller: Controller<B>,
    owner: String,
    operation: Option<String>,
    lease: Option<String>,
    consumed_held: bool,
    idle_since: Option<Instant>,
}

pub(crate) struct Controllers<B: Backend> {
    defaults: Controller<B>,
    contexts: BTreeMap<String, Context<B>>,
    selected: Option<String>,
    default_consumed_held: bool,
    factory: Box<dyn FnMut() -> ModuleResult<B>>,
    enabled: bool,
    policy: Option<InputPolicy>,
    shutdown_deadline: Option<Instant>,
}

impl<B: Backend> Controllers<B> {
    pub(crate) fn new(
        backend: B,
        enabled: bool,
        factory: impl FnMut() -> ModuleResult<B> + 'static,
    ) -> Self {
        Self {
            defaults: Controller::new(backend, enabled),
            contexts: BTreeMap::new(),
            selected: None,
            default_consumed_held: true,
            factory: Box::new(factory),
            enabled,
            policy: None,
            shutdown_deadline: None,
        }
    }

    pub(crate) fn select_default(&mut self) {
        self.selected = None;
    }

    pub(crate) fn select_binding(&mut self, binding: &str) {
        self.selected = self
            .contexts
            .contains_key(binding)
            .then(|| binding.to_owned());
    }

    pub(crate) fn select_request(&mut self, owner: &str, payload: &Value) {
        self.selected = if let Some(lease) = payload["lease"].as_str() {
            self.contexts
                .iter()
                .find(|(_, context)| {
                    context.owner == owner && context.lease.as_deref() == Some(lease)
                })
                .map(|(key, _)| key.clone())
        } else if let Some(binding) = payload["input_binding"]
            .as_str()
            .or_else(|| payload["lifecycle_binding_token"].as_str())
            .or_else(|| payload["token"].as_str())
        {
            self.contexts
                .contains_key(binding)
                .then(|| binding.to_owned())
        } else if let Some(operation) = payload["operation_id"]
            .as_str()
            .filter(|_| payload["action"] == "lookup")
        {
            self.contexts
                .iter()
                .find(|(_, context)| {
                    context.owner == owner && context.operation.as_deref() == Some(operation)
                })
                .map(|(key, _)| key.clone())
        } else {
            None
        };
    }

    pub(crate) fn legacy_acquisition_available(&self) -> bool {
        self.contexts
            .values()
            .all(|context| !context.controller.has_live_state())
    }

    /// Called only after lifecycle has authenticated the exact binding.
    pub(crate) fn prepare_binding(
        &mut self,
        binding: &str,
        owner: &str,
        payload: &Value,
        now: Instant,
    ) -> InputResult<()> {
        self.expire_all(now);
        if self.defaults.has_live_state() {
            return Err(InputError::new(
                "input_busy",
                "unbound input lease owns the devices exclusively",
            ));
        }
        if let Some(context) = self.contexts.get(binding) {
            if context.owner != owner
                || context.operation.as_deref() != payload["operation_id"].as_str()
            {
                return Err(InputError::new(
                    "operation_conflict",
                    "input context already belongs to another acquisition",
                ));
            }
            self.selected = Some(binding.to_owned());
            return Ok(());
        }
        if payload["operation_id"].as_str().is_some_and(|operation| {
            self.contexts.values().any(|context| {
                context.owner == owner && context.operation.as_deref() == Some(operation)
            })
        }) {
            return Err(InputError::new(
                "operation_conflict",
                "acquisition is bound to another input context",
            ));
        }
        if self.contexts.len() >= MAX_CONTEXTS
            || self
                .contexts
                .values()
                .filter(|context| context.controller.has_live_state())
                .count()
                >= MAX_LIVE_CONTEXTS
        {
            return Err(InputError::new(
                "input_busy",
                "input context capacity is exhausted",
            ));
        }
        let backend =
            (self.factory)().map_err(|error| InputError::new("backend_unavailable", error))?;
        let mut controller = Controller::new(backend, self.enabled);
        if let Some(deadline) = self.shutdown_deadline {
            controller.restrict_shutdown_deadline(deadline);
        }
        if let Some(policy) = &self.policy {
            policy.apply_to_controller(&mut controller);
        }
        self.contexts.insert(
            binding.to_owned(),
            Context {
                controller,
                owner: owner.to_owned(),
                operation: payload["operation_id"].as_str().map(str::to_owned),
                lease: None,
                consumed_held: true,
                idle_since: Some(now),
            },
        );
        self.selected = Some(binding.to_owned());
        Ok(())
    }

    pub(crate) fn note_lease(&mut self, reply: &Value) {
        if reply["ok"] == true
            && let Some(lease) = reply["lease"].as_str()
            && let Some(context) = self
                .selected
                .as_ref()
                .and_then(|key| self.contexts.get_mut(key))
        {
            context.lease = Some(lease.to_owned());
            context.idle_since = None;
        }
    }

    pub(crate) fn consumed_held(&self) -> bool {
        self.selected
            .as_ref()
            .and_then(|key| self.contexts.get(key))
            .map_or(self.default_consumed_held, |context| context.consumed_held)
    }
    pub(crate) fn set_consumed_held(&mut self, value: bool) {
        if let Some(context) = self
            .selected
            .as_ref()
            .and_then(|key| self.contexts.get_mut(key))
        {
            context.consumed_held = value;
        } else {
            self.default_consumed_held = value;
        }
    }

    pub(crate) fn synchronize_policy(&mut self, policy: &InputPolicy) -> bool {
        if self.policy.as_ref() != Some(policy) {
            policy.apply_to_controller(&mut self.defaults);
            for context in self.contexts.values_mut() {
                policy.apply_to_controller(&mut context.controller);
            }
            self.policy = Some(policy.clone());
        }
        self.defaults.cleanup_required()
            || self
                .contexts
                .values()
                .any(|context| context.controller.cleanup_required())
    }

    pub(crate) fn expire_all(&mut self, now: Instant) {
        self.defaults.expire(now);
        for context in self.contexts.values_mut() {
            context.controller.expire(now);
            if context.controller.has_live_state() {
                context.idle_since = None;
            } else {
                context.idle_since.get_or_insert(now);
            }
        }
        self.contexts.retain(|_, context| {
            context
                .idle_since
                .is_none_or(|since| now.saturating_duration_since(since) < RETENTION)
        });
        if self
            .selected
            .as_ref()
            .is_some_and(|key| !self.contexts.contains_key(key))
        {
            self.selected = None;
        }
    }

    pub(crate) fn quiesce_all(&mut self, now: Instant) -> bool {
        let mut ready = self.defaults.quiesce(now);
        for context in self.contexts.values_mut() {
            // Continue independent contexts even when another native release
            // fails. A single selected context cannot establish global drain.
            ready &= context.controller.quiesce(now);
        }
        ready
    }

    pub(crate) fn restrict_shutdown_deadline(&mut self, deadline: Instant) {
        let deadline = self
            .shutdown_deadline
            .map_or(deadline, |prior| prior.min(deadline));
        self.shutdown_deadline = Some(deadline);
        self.defaults.restrict_shutdown_deadline(deadline);
        for context in self.contexts.values_mut() {
            context.controller.restrict_shutdown_deadline(deadline);
        }
    }
}

impl<B: Backend> PolicyTarget for Controllers<B> {
    fn apply_input_policy(&mut self, policy: &InputPolicy) -> bool {
        self.synchronize_policy(policy)
    }
    fn input_policy_state(&self, policy: &InputPolicy) -> (bool, bool) {
        let (mut applied, mut cleanup) = self.defaults.input_policy_state(policy);
        for context in self.contexts.values() {
            let (context_applied, context_cleanup) = context.controller.input_policy_state(policy);
            applied &= context_applied;
            cleanup |= context_cleanup;
        }
        (applied && !cleanup, cleanup)
    }
}

impl<B: Backend> Deref for Controllers<B> {
    type Target = Controller<B>;
    fn deref(&self) -> &Self::Target {
        self.selected
            .as_ref()
            .and_then(|key| self.contexts.get(key))
            .map_or(&self.defaults, |context| &context.controller)
    }
}
impl<B: Backend> DerefMut for Controllers<B> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self
            .selected
            .as_ref()
            .and_then(|key| self.contexts.get_mut(key))
        {
            Some(context) => &mut context.controller,
            None => &mut self.defaults,
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::model::{Event, Key, KeyState, Operation};
    use serde_json::json;
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeSet,
        rc::Rc,
    };

    const OWNER: &str = "amaterasu.stream-session";
    #[derive(Default)]
    struct Trace {
        events: Vec<(usize, Event)>,
        stopped: Vec<usize>,
        fail_start: BTreeSet<usize>,
        fail_stop: BTreeSet<usize>,
        fail_key_release: BTreeSet<usize>,
    }
    struct FakeBackend {
        id: usize,
        trace: Rc<RefCell<Trace>>,
    }
    impl Backend for FakeBackend {
        fn name(&self) -> &'static str {
            "independent-test-device"
        }
        fn supported(&self) -> bool {
            true
        }
        fn absolute_pointer(&self) -> bool {
            true
        }
        fn key_encoding(&self) -> &'static str {
            "linux_evdev"
        }
        fn controller_input_supported(&self) -> bool {
            true
        }
        fn validate(&self, event: &Event) -> InputResult<()> {
            event.validate()
        }
        fn start(&mut self) -> InputResult<()> {
            if self.trace.borrow_mut().fail_start.remove(&self.id) {
                return Err(InputError::new(
                    "acquire_failed",
                    "scripted acquisition failure",
                ));
            }
            Ok(())
        }
        fn inject(&mut self, event: &Event) -> InputResult<()> {
            let mut trace = self.trace.borrow_mut();
            trace.events.push((self.id, event.clone()));
            if matches!(
                event,
                Event::Key {
                    state: KeyState::Up,
                    ..
                }
            ) && trace.fail_key_release.contains(&self.id)
            {
                Err(InputError::new(
                    "release_failed",
                    "scripted key release failure",
                ))
            } else {
                Ok(())
            }
        }
        fn stop(&mut self) -> InputResult<()> {
            let mut trace = self.trace.borrow_mut();
            trace.stopped.push(self.id);
            if trace.fail_stop.contains(&self.id) {
                Err(InputError::new(
                    "release_failed",
                    "scripted cleanup failure",
                ))
            } else {
                Ok(())
            }
        }
        fn poll_gamepad_feedback(
            &mut self,
        ) -> InputResult<Option<amaterasu_input_companion::GamepadFeedback>> {
            Ok(Some(amaterasu_input_companion::GamepadFeedback {
                slot: 0,
                strong_motor: self.id as u16,
                weak_motor: 0,
                duration_ms: 1,
            }))
        }
    }
    fn pool() -> (Controllers<FakeBackend>, Rc<RefCell<Trace>>) {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let factory_trace = Rc::clone(&trace);
        let id = Cell::new(0);
        let pool = Controllers::new(
            FakeBackend {
                id: 0,
                trace: Rc::clone(&trace),
            },
            true,
            move || {
                id.set(id.get() + 1);
                Ok(FakeBackend {
                    id: id.get(),
                    trace: Rc::clone(&factory_trace),
                })
            },
        );
        (pool, trace)
    }
    fn acquire(
        pool: &mut Controllers<FakeBackend>,
        binding: &str,
        mut payload: Value,
        now: Instant,
    ) -> Value {
        payload["action"] = json!("acquire");
        payload["operation_id"] = json!(format!("acquire-{binding}"));
        pool.prepare_binding(binding, OWNER, &payload, now).unwrap();
        let reply = pool.handle(
            OWNER,
            serde_json::from_value(payload).unwrap(),
            now,
            &mut || Ok(()),
        );
        pool.note_lease(&reply);
        reply
    }
    fn key(state: KeyState) -> Event {
        Event::Key {
            key: Key::LinuxEvdev { code: 30 },
            state,
        }
    }
    fn batch(
        pool: &mut Controllers<FakeBackend>,
        lease: &str,
        events: Vec<Event>,
        now: Instant,
    ) -> Value {
        pool.select_request(OWNER, &json!({"lease":lease}));
        pool.handle(
            OWNER,
            Operation::Batch {
                lease: lease.to_owned(),
                events,
            },
            now,
            &mut || Ok(()),
        )
    }

    #[test]
    fn clients_hold_the_same_key_and_retire_only_the_selected_devices() {
        let now = Instant::now();
        let (mut pool, trace) = pool();
        let first = acquire(&mut pool, "one", json!({}), now)["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        let second = acquire(&mut pool, "two", json!({}), now)["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(first, second);
        assert_eq!(
            batch(&mut pool, &first, vec![key(KeyState::Down)], now)["ok"],
            true
        );
        pool.set_consumed_held(false);
        assert_eq!(
            batch(&mut pool, &second, vec![key(KeyState::Down)], now)["ok"],
            true
        );
        assert!(pool.consumed_held());
        // The same controller number belongs to a different native device/feedback queue.
        for (lease, expected_motor) in [(&first, 1), (&second, 2)] {
            pool.select_request(OWNER, &json!({"lease":lease}));
            let feedback = pool.handle(
                OWNER,
                Operation::PollGamepadFeedback {
                    lease: lease.clone(),
                },
                now,
                &mut || Ok(()),
            );
            assert_eq!(feedback["feedback"]["slot"], 0);
            assert_eq!(feedback["feedback"]["strong_motor"], expected_motor);
        }
        pool.select_binding("one");
        assert!(!pool.consumed_held());
        pool.cancel_bound_lease(OWNER, &first).unwrap();
        assert!(
            pool.contexts["two"]
                .controller
                .bound_lease_active(OWNER, &second)
        );
        assert_eq!(
            trace.borrow().events,
            vec![
                (1, key(KeyState::Down)),
                (2, key(KeyState::Down)),
                (1, key(KeyState::Up))
            ]
        );
        assert_eq!(
            batch(&mut pool, &second, vec![key(KeyState::Up)], now)["ok"],
            true
        );
        assert_eq!(trace.borrow().stopped, vec![1]);
    }

    #[test]
    fn shutdown_releases_independent_contexts_and_retries_only_failed_native_ownership() {
        let now = Instant::now();
        let (mut pool, trace) = pool();
        let first = acquire(&mut pool, "one", json!({}), now)["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        let second = acquire(&mut pool, "two", json!({}), now)["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        batch(&mut pool, &first, vec![key(KeyState::Down)], now);
        batch(&mut pool, &second, vec![key(KeyState::Down)], now);
        trace.borrow_mut().fail_key_release.insert(1);
        trace.borrow_mut().fail_stop.insert(1);

        assert!(!pool.quiesce_all(now));
        assert!(pool.contexts["one"].controller.has_live_state());
        assert!(!pool.contexts["two"].controller.has_live_state());
        assert_eq!(trace.borrow().stopped, vec![1, 2]);
        let attempted_events = trace.borrow().events.len();
        assert!(!pool.quiesce_all(now + Duration::from_millis(50)));
        assert_eq!(trace.borrow().events.len(), attempted_events);

        trace.borrow_mut().fail_key_release.clear();
        trace.borrow_mut().fail_stop.clear();
        assert!(pool.quiesce_all(now + Duration::from_millis(100)));
        assert_eq!(trace.borrow().stopped, vec![1, 2, 1]);
        assert!(pool.quiesce_all(now + Duration::from_secs(1)));
        assert_eq!(trace.borrow().stopped, vec![1, 2, 1]);
    }

    #[test]
    fn shutdown_retires_a_direct_legacy_lease_and_its_pressed_keys() {
        let now = Instant::now();
        let (mut pool, trace) = pool();
        let operation: Operation = serde_json::from_value(json!({
            "action":"acquire","operation_id":"legacy"
        }))
        .unwrap();
        let lease = pool.handle(OWNER, operation, now, &mut || Ok(()))["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        batch(&mut pool, &lease, vec![key(KeyState::Down)], now);

        assert!(pool.quiesce_all(now));
        assert_eq!(
            trace.borrow().events,
            vec![(0, key(KeyState::Down)), (0, key(KeyState::Up))]
        );
        assert_eq!(trace.borrow().stopped, vec![0]);
        assert!(!pool.has_live_state());
    }

    #[test]
    fn legacy_unbound_lease_remains_exclusive_across_contexts() {
        let now = Instant::now();
        let (mut pool, _) = pool();
        let operation: Operation =
            serde_json::from_value(json!({"action":"acquire","operation_id":"legacy"})).unwrap();
        let legacy = pool.handle(OWNER, operation, now, &mut || Ok(()));
        assert_eq!(legacy["ok"], true);
        assert!(
            pool.prepare_binding("blocked", OWNER, &json!({"operation_id":"bound"}), now)
                .is_err()
        );
        pool.cancel_owner(OWNER);
        acquire(&mut pool, "bound", json!({}), now);
        assert!(!pool.legacy_acquisition_available());
    }

    #[test]
    fn failed_acquisition_and_lost_reply_cannot_cancel_another_context() {
        let now = Instant::now();
        let (mut pool, trace) = pool();
        let first = acquire(&mut pool, "one", json!({}), now)["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        trace.borrow_mut().fail_start.insert(2);
        assert_eq!(acquire(&mut pool, "failed", json!({}), now)["ok"], false);
        assert!(
            pool.contexts["one"]
                .controller
                .bound_lease_active(OWNER, &first)
        );
        let second = acquire(&mut pool, "two", json!({}), now)["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        // InputModule's lost-completion path cancels the selected controller only.
        pool.cancel_owner(OWNER);
        assert!(!pool.bound_lease_active(OWNER, &second));
        assert!(
            pool.contexts["one"]
                .controller
                .bound_lease_active(OWNER, &first)
        );
    }

    #[test]
    fn global_policy_updates_preserve_explicit_client_overrides() {
        let now = Instant::now();
        let (mut pool, _) = pool();
        pool.synchronize_policy(&InputPolicy::default());
        let explicit = acquire(&mut pool, "explicit", json!({"keyboard":true}), now)["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        let inherited = acquire(&mut pool, "inherited", json!({}), now)["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        let policy = InputPolicy {
            keyboard: false,
            ..Default::default()
        };
        pool.synchronize_policy(&policy);
        batch(&mut pool, &explicit, vec![key(KeyState::Down)], now);
        assert_eq!(
            pool.shortcut_source_held_keys(OWNER, &explicit, now)
                .unwrap()
                .len(),
            1
        );
        batch(&mut pool, &inherited, vec![key(KeyState::Down)], now);
        assert!(
            pool.shortcut_source_held_keys(OWNER, &inherited, now)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn global_policy_reports_cleanup_failures_from_any_client() {
        let now = Instant::now();
        let (mut pool, trace) = pool();
        pool.synchronize_policy(&InputPolicy::default());
        let first = acquire(&mut pool, "one", json!({}), now)["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        let second = acquire(&mut pool, "two", json!({}), now)["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        batch(&mut pool, &first, vec![key(KeyState::Down)], now);
        batch(&mut pool, &second, vec![key(KeyState::Down)], now);
        trace.borrow_mut().fail_key_release.insert(1);
        pool.select_default();
        let policy = InputPolicy {
            keyboard: false,
            ..Default::default()
        };
        assert!(pool.apply_input_policy(&policy));
        assert_eq!(pool.input_policy_state(&policy), (false, true));
        assert!(!pool.contexts["two"].controller.cleanup_required());
    }

    #[test]
    fn retired_contexts_are_bounded_and_cleanup_failures_remain_owned() {
        let now = Instant::now();
        let (mut pool, trace) = pool();
        let first = acquire(&mut pool, "uncertain", json!({}), now)["lease"]
            .as_str()
            .unwrap()
            .to_owned();
        trace.borrow_mut().fail_stop.insert(1);
        assert!(pool.cancel_bound_lease(OWNER, &first).is_err());
        for index in 1..MAX_CONTEXTS {
            pool.prepare_binding(
                &format!("retired-{index}"),
                OWNER,
                &json!({"operation_id":format!("retired-{index}")}),
                now,
            )
            .unwrap();
        }
        assert!(
            pool.prepare_binding("full", OWNER, &json!({"operation_id":"full"}), now)
                .is_err()
        );
        pool.expire_all(now + RETENTION + Duration::from_secs(1));
        assert_eq!(pool.contexts.len(), 1);
        assert!(pool.contexts["uncertain"].controller.cleanup_required());
        assert!(
            pool.prepare_binding(
                "new",
                OWNER,
                &json!({"operation_id":"new"}),
                now + RETENTION + Duration::from_secs(1)
            )
            .is_ok()
        );
    }
}
