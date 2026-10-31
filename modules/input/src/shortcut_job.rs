//! One bounded, broker-driven shortcut batch. No native event is delivered
//! before the lifecycle/action reservations and journal CAS have succeeded.
use crate::{
    backend::Backend,
    controller::Controller,
    model::{Event, InputResult, Key},
    shortcut::{Disposition, Intent, Matcher},
    shortcut_bridge::StagedBatch,
    shortcut_cache::Snapshot,
    shortcut_discovery::{CanonicalContext, DiscoveryJob},
    shortcut_dispatch::DispatchJob,
    shortcut_journal::{Admission, DispatchIntent, IntentPhase, Phase},
    shortcut_reservation::ReservationJob,
    shortcut_store::ShortcutStore,
};
use amaterasu_extension_api::{ActionInvoke, ActionTarget, BindingSource, LifecycleTarget};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BatchRequest {
    action: String,
    lease: String,
    input_binding: String,
    operation_id: String,
    events: Vec<Event>,
}

#[derive(Deserialize)]
struct Authority {
    status: String,
    batch_epoch: String,
    target: LifecycleTarget,
    action_target: Option<ActionTarget>,
    active: bool,
    context: String,
    application: Option<String>,
    paired_client: Option<String>,
    allowed_actions: Vec<String>,
}

enum Step {
    Validate,
    ValidateWait(u64),
    Discover,
    Load,
    RecoverDispatch,
    OpenEpoch,
    OpenWait(u64),
    Reserve,
    Arm,
    WaitArm,
    Deliver,
    WaitDelivery,
    Dispatch,
    Done,
}

pub(crate) struct ShortcutJob {
    parent: u64,
    caller: String,
    deadline: Instant,
    request: BatchRequest,
    fingerprint: Value,
    epoch: String,
    journal_sequence: u64,
    step: Step,
    authority: Option<Authority>,
    expected_target: LifecycleTarget,
    input_boot_epoch: String,
    discovery: Option<DiscoveryJob>,
    cache_snapshot: Option<Snapshot>,
    binding_revisions: BTreeMap<String, u64>,
    matcher: Matcher,
    staged: Option<StagedBatch>,
    store: Option<ShortcutStore>,
    action_epoch: Option<String>,
    reservations: Vec<DispatchIntent>,
    reservation: Option<ReservationJob>,
    delivery: Option<Value>,
    dispatch: Option<DispatchJob>,
    dispatch_index: usize,
    armed: bool,
    transferred: bool,
    instance: String,
    call_sequence: u64,
}

impl ShortcutJob {
    pub fn new(
        parent: u64,
        caller: String,
        payload: Value,
        expected_target: LifecycleTarget,
        deadline: Instant,
        input_boot_epoch: String,
    ) -> Result<Self, String> {
        let request: BatchRequest =
            serde_json::from_value(payload.clone()).map_err(|_| "invalid shortcut batch")?;
        if request.action != "batch"
            || request.input_binding.is_empty()
            || caller != "amaterasu.stream-session"
        {
            return Err("unauthorized shortcut source".into());
        }
        let (epoch, sequence) = request
            .operation_id
            .rsplit_once(':')
            .ok_or("invalid shortcut operation id")?;
        let sequence = sequence
            .parse::<u64>()
            .map_err(|_| "invalid shortcut sequence")?;
        if epoch.is_empty()
            || epoch.len() > 128
            || !epoch
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || sequence == 0
            || sequence.to_string() != request.operation_id.rsplit_once(':').unwrap().1
        {
            return Err("invalid shortcut operation id".into());
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| "shortcut request identity unavailable")?;
        let instance = random.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self {
            instance,
            call_sequence: 0,
            parent,
            caller,
            deadline,
            epoch: epoch.to_owned(),
            journal_sequence: 0,
            request,
            fingerprint: payload,
            step: Step::Validate,
            authority: None,
            expected_target,
            input_boot_epoch,
            discovery: None,
            cache_snapshot: None,
            binding_revisions: BTreeMap::new(),
            matcher: Matcher::default(),
            staged: None,
            store: None,
            action_epoch: None,
            reservations: Vec::new(),
            reservation: None,
            delivery: None,
            dispatch: None,
            dispatch_index: 0,
            armed: false,
            transferred: false,
        })
    }
    pub fn binding_identity(&self) -> (&str, &str) {
        (&self.request.lease, &self.request.input_binding)
    }

    pub fn take_cache_snapshot(&mut self) -> Option<Snapshot> {
        self.cache_snapshot.take()
    }
    pub fn has_consumed_held(&self) -> bool {
        self.matcher
            .held_snapshot()
            .iter()
            .any(|(_, disposition)| *disposition == Disposition::Consume)
    }

    pub fn request_id(&self) -> u64 {
        self.parent
    }

    pub fn armed(&self) -> bool {
        self.armed
            || !self.reservations.is_empty()
            || self
                .reservation
                .as_ref()
                .is_some_and(ReservationJob::may_have_reservation)
    }

    /// Permit a coordinator-owned action to finish after its own source
    /// stream is fenced, only after native delivery has durably completed.
    pub fn can_transfer(
        &self,
        token: &str,
        target: &LifecycleTarget,
        operation_id: &str,
        reservation_id: &str,
    ) -> bool {
        if self.request.input_binding != token
            || &self.expected_target != target
            || !matches!(self.step, Step::Dispatch | Step::RecoverDispatch)
        {
            return false;
        }
        let Some((epoch, suffix)) = operation_id.rsplit_once(':') else {
            return false;
        };
        let Ok(sequence) = suffix.parse::<u64>() else {
            return false;
        };
        if sequence.to_string() != suffix
            || self
                .confirm_delivery(epoch, sequence, operation_id, reservation_id)
                .is_none()
        {
            return false;
        }
        true
    }

    pub fn activate_transfer(&mut self) {
        self.transferred = true;
    }

    pub fn is_transferred(&self) -> bool {
        self.transferred
    }

    /// Answer only from the state service's confirmed journal. This can be
    /// called by the authenticated action service while commit is in flight.
    pub fn confirm_delivery(
        &self,
        epoch: &str,
        sequence: u64,
        operation_id: &str,
        reservation_id: &str,
    ) -> Option<Value> {
        let journal = self.store.as_ref()?.journal()?;
        for (batch_sequence, record) in &journal.receipts {
            if record.phase != Phase::Delivered {
                continue;
            }
            for intent in &record.intents {
                if !matches!(
                    intent.phase,
                    IntentPhase::Dispatching | IntentPhase::Finishing
                ) || intent.operation_id != operation_id
                {
                    continue;
                }
                let invoke: ActionInvoke = serde_json::from_value(intent.request.clone()).ok()?;
                if invoke.epoch != epoch
                    || invoke.sequence != sequence
                    || invoke.operation_id != operation_id
                    || invoke.input_reservation_id.as_deref() != Some(reservation_id)
                {
                    continue;
                }
                return Some(
                    json!({"ok":true,"status":"delivered","epoch":epoch,"sequence":sequence,
                    "operation_id":operation_id,"reservation_id":reservation_id,"action_id":invoke.action_id,
                    "target":invoke.target,"batch_epoch":journal.epoch,"batch_sequence":record.fingerprint.get("operation_id").and_then(Value::as_str).and_then(|id| id.rsplit_once(':')).and_then(|(_, seq)|seq.parse::<u64>().ok())?,"journal_sequence":batch_sequence}),
                );
            }
        }
        None
    }

    fn request(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        service: &str,
        payload: Value,
        suffix: &str,
        parent: Option<u64>,
    ) -> Result<u64, String> {
        let timeout = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if timeout == 0 {
            return Err("shortcut deadline exceeded".into());
        }
        self.call_sequence = self
            .call_sequence
            .checked_add(1)
            .ok_or("shortcut transport sequence exhausted")?;
        match send(Command::Request {
            service: service.into(),
            payload,
            timeout_ms: timeout,
            operation_id: format!("shortcut-{suffix}-{}-{}", self.instance, self.call_sequence),
            parent,
        }) {
            Ok(Response::Accepted { id }) => Ok(id),
            _ => Err("shortcut service unavailable".into()),
        }
    }
    fn take(
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        id: u64,
    ) -> Result<Option<Value>, String> {
        match send(Command::TakeResult { request: id }) {
            Ok(Response::Pending) => Ok(None),
            Ok(Response::Result { payload }) => {
                if !matches!(
                    send(Command::ReleaseResult { request: id }),
                    Ok(Response::Ok)
                ) {
                    return Err("shortcut reply release failed".into());
                }
                Ok(Some(payload))
            }
            _ => Err("shortcut service reply unresolved".into()),
        }
    }
    pub fn advance<B: Backend>(
        &mut self,
        controller: &mut Controller<B>,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        guard: &mut impl FnMut() -> InputResult<()>,
    ) -> Result<Option<Value>, String> {
        if Instant::now() >= self.deadline {
            return Err("shortcut deadline exceeded; reconciliation may be required".into());
        }
        let step = std::mem::replace(&mut self.step, Step::Done);
        self.step = match step {
            Step::Validate => {
                controller
                    .authorize_batch_source(&self.caller, &self.request.lease, Instant::now())
                    .map_err(|_| "input lease unavailable")?;
                let payload = json!({"operation":"validate_input_binding", "operation_id":format!("shortcut-validate-{}", self.request.operation_id), "token":self.request.input_binding, "input_boot_epoch":self.input_boot_epoch});
                Step::ValidateWait(self.request(
                    send,
                    "amaterasu.session-lifecycle/1",
                    payload,
                    "validate",
                    Some(self.parent),
                )?)
            }
            Step::ValidateWait(id) => match Self::take(send, id)? {
                None => Step::ValidateWait(id),
                Some(payload) => {
                    let authority: Authority = serde_json::from_value(payload)
                        .map_err(|_| "invalid lifecycle authority")?;
                    let (host_epoch, session_id, stream_id, generation) = match &authority.target {
                        LifecycleTarget::Stream {
                            host_epoch,
                            session_id,
                            stream_id,
                            generation,
                        } => (host_epoch, session_id, stream_id, *generation),
                        _ => return Err("shortcut authority rejected".into()),
                    };
                    let action_matches = match &authority.action_target {
                        None => true,
                        Some(ActionTarget::Session {
                            host_epoch: host,
                            session_id: session,
                            ..
                        }) => host == host_epoch && session == session_id,
                        Some(ActionTarget::Stream {
                            host_epoch: host,
                            session_id: session,
                            stream_id: stream,
                            expected_generation,
                        }) => {
                            host == host_epoch
                                && session == session_id
                                && stream == stream_id
                                && *expected_generation == generation
                        }
                        Some(ActionTarget::Host { .. }) => false,
                    };
                    if authority.status != "succeeded"
                        || !authority.active
                        || authority.target != self.expected_target
                        || authority.batch_epoch != self.epoch
                        || !action_matches
                    {
                        return Err("shortcut authority rejected".into());
                    }
                    let context = CanonicalContext {
                        session_id: session_id.clone(),
                        target: authority.target.clone(),
                        input_binding: self.request.input_binding.clone(),
                        input_boot_epoch: self.input_boot_epoch.clone(),
                        application: authority.application.clone(),
                        paired_client: authority.paired_client.clone(),
                        context: authority.context.clone(),
                    };
                    let timeout = self
                        .deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64;
                    self.store = Some(ShortcutStore::new(self.epoch.clone())?);
                    self.discovery = Some(DiscoveryJob::new(self.parent, context, timeout)?);
                    self.authority = Some(authority);
                    Step::Discover
                }
            },
            Step::Discover => {
                self.store.as_mut().unwrap().advance(send);
                if self.store.as_ref().unwrap().fenced() {
                    return Err("shortcut journal unavailable".into());
                }
                match self.discovery.as_mut().unwrap().advance(send)? {
                    None => Step::Discover,
                    Some(approved) => {
                        let authority = self.authority.as_ref().unwrap();
                        let bindings = approved
                            .into_iter()
                            .filter(|entry| {
                                entry.action.id == entry.binding.action_id
                                    && authority.allowed_actions.contains(&entry.binding.action_id)
                            })
                            .map(|entry| {
                                self.binding_revisions
                                    .insert(entry.binding.id.clone(), entry.settings_revision);
                                entry.binding
                            })
                            .collect::<Vec<_>>();
                        let discovery = self.discovery.as_ref().unwrap();
                        self.cache_snapshot = Some(Snapshot {
                            context: authority.context.clone(),
                            approval_revision: discovery
                                .approval_revision()
                                .ok_or("missing approval revision")?,
                            settings_revisions: discovery.settings_revisions().clone(),
                            bindings: bindings.clone(),
                        });
                        self.matcher
                            .replace_approved_bindings(
                                &bindings,
                                BindingSource::SessionInput,
                                &authority.context,
                            )
                            .map_err(str::to_owned)?;
                        Step::Load
                    }
                }
            }
            Step::Load => {
                let store = self.store.as_mut().unwrap();
                store.advance(send);
                if store.fenced() {
                    return Err("shortcut journal unavailable".into());
                }
                if !store.ready() {
                    Step::Load
                } else {
                    let journal = store.journal().unwrap();
                    let recoverable = journal.receipts.iter().find_map(|(sequence, record)| {
                        (record.phase == Phase::Delivered)
                            .then(|| {
                                record
                                    .intents
                                    .iter()
                                    .position(|intent| {
                                        matches!(
                                            intent.phase,
                                            IntentPhase::Pending | IntentPhase::Finishing
                                        )
                                    })
                                    .map(|index| (*sequence, index))
                            })
                            .flatten()
                    });
                    if let Some((sequence, index)) = recoverable {
                        self.dispatch = Some(DispatchJob::new(store, sequence, index)?);
                        Step::RecoverDispatch
                    } else {
                        if let Some(existing) = journal.receipts.values().find(|record| {
                            record.fingerprint.get("operation_id")
                                == Some(&json!(self.request.operation_id))
                        }) {
                            if existing.fingerprint != self.fingerprint {
                                return Err("shortcut operation conflict".into());
                            }
                            if existing.phase == Phase::Delivered
                                && existing
                                    .intents
                                    .iter()
                                    .all(|intent| intent.phase == IntentPhase::Terminal)
                            {
                                let mut response =
                                    existing.result.clone().ok_or("missing delivery receipt")?;
                                response["operation_id"] = json!(self.request.operation_id);
                                return Ok(Some(response));
                            }
                            return Err("shortcut receipt requires recovery".into());
                        }
                        // Source sequencing is enforced by the active binding. The
                        // journal has its own sequence because fast-path mouse
                        // batches do not create durable shortcut receipts.
                        self.journal_sequence = journal.next_sequence;
                        if let Some((_, previous)) = journal.receipts.last_key_value() {
                            if previous.phase != Phase::Delivered
                                || previous
                                    .intents
                                    .iter()
                                    .any(|intent| intent.phase != IntentPhase::Terminal)
                            {
                                return Err("previous shortcut input requires recovery".into());
                            }
                            let held: Vec<(Key, Disposition)> =
                                serde_json::from_value(previous.held.clone())
                                    .map_err(|_| "invalid held shortcut state")?;
                            self.matcher.recover_held(&held).map_err(str::to_owned)?;
                        }
                        // Forwarded ownership can change on the keyboard fast path.
                        // Consumed keys only exist in the durable matcher snapshot.
                        let mut held = self
                            .matcher
                            .held_snapshot()
                            .into_iter()
                            .filter(|(_, disposition)| *disposition == Disposition::Consume)
                            .collect::<Vec<_>>();
                        held.extend(
                            controller
                                .shortcut_source_held_keys(
                                    &self.caller,
                                    &self.request.lease,
                                    Instant::now(),
                                )
                                .map_err(|_| "input lease unavailable")?
                                .into_iter()
                                .map(|key| (key, Disposition::Forward)),
                        );
                        self.matcher.recover_held(&held).map_err(str::to_owned)?;
                        let authority = self.authority.as_ref().unwrap();
                        let target = serde_json::to_value(&authority.action_target)
                            .map_err(|_| "invalid target")?;
                        self.staged = Some(
                            StagedBatch::prepare(
                                controller,
                                &self.matcher,
                                &self.caller,
                                &self.request.lease,
                                self.request.events.clone(),
                                target,
                                Instant::now(),
                            )
                            .map_err(|_| "invalid staged input")?,
                        );
                        if self.staged.as_ref().unwrap().intents().len() > 1 {
                            return Err("multiple shortcut actions per batch unavailable".into());
                        }
                        if self.staged.as_ref().unwrap().intents().is_empty() {
                            Step::Arm
                        } else {
                            Step::OpenEpoch
                        }
                    }
                }
            }
            Step::RecoverDispatch => {
                if self
                    .dispatch
                    .as_mut()
                    .unwrap()
                    .advance(self.store.as_mut().unwrap(), send)?
                {
                    self.dispatch = None;
                    if self.transferred {
                        return Err("input binding retired after action recovery".into());
                    }
                    Step::Load
                } else {
                    Step::RecoverDispatch
                }
            }
            Step::OpenEpoch => {
                let payload = json!({"operation":"open_epoch", "operation_id":format!("shortcut-open-{}", self.request.operation_id)});
                Step::OpenWait(self.request(
                    send,
                    "amaterasu.extension-actions/1",
                    payload,
                    "open",
                    None,
                )?)
            }
            Step::OpenWait(id) => match Self::take(send, id)? {
                None => Step::OpenWait(id),
                Some(payload) => {
                    if payload["status"] != "ok" {
                        return Err("action epoch unavailable".into());
                    }
                    self.action_epoch = Some(
                        payload["epoch"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .ok_or("invalid action epoch")?
                            .to_owned(),
                    );
                    Step::Reserve
                }
            },
            Step::Reserve => {
                if self.reservation.is_none() {
                    let intent: &Intent =
                        &self.staged.as_ref().unwrap().intents()[self.reservations.len()];
                    let epoch = self.action_epoch.as_ref().unwrap();
                    let invoke = ActionInvoke {
                        epoch: epoch.clone(),
                        sequence: 1,
                        operation_id: format!("{epoch}:1"),
                        action_id: intent.action_id.clone(),
                        target: self
                            .authority
                            .as_ref()
                            .unwrap()
                            .action_target
                            .clone()
                            .ok_or("action target unavailable")?,
                        arguments: intent.arguments.clone(),
                        expected_settings_revision: Some(
                            self.binding_revisions
                                .get(&intent.binding_id)
                                .ok_or("shortcut settings revision unavailable")?
                                .to_string(),
                        ),
                        input_reservation_id: None,
                    };
                    let timeout = self
                        .deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64;
                    self.reservation = Some(ReservationJob::new(
                        self.parent,
                        self.request.input_binding.clone(),
                        invoke,
                        timeout,
                    )?);
                }
                match self.reservation.as_mut().unwrap().advance(send)? {
                    None => Step::Reserve,
                    Some(approved) => {
                        let invoke = approved.invoke;
                        self.reservations.push(DispatchIntent {
                            operation_id: invoke.operation_id.clone(),
                            request: serde_json::to_value(invoke).map_err(|_| "invalid action")?,
                            phase: IntentPhase::Pending,
                            result: None,
                        });
                        Step::Arm
                    }
                }
            }
            Step::Arm => {
                self.armed = true;
                let admission = self.staged.as_mut().unwrap().arm(
                    self.store.as_mut().unwrap(),
                    self.journal_sequence,
                    self.fingerprint.clone(),
                    self.reservations.clone(),
                )?;
                if !matches!(admission, Admission::New) {
                    return Err("shortcut receipt already exists".into());
                }
                Step::WaitArm
            }
            Step::WaitArm => {
                let store = self.store.as_mut().unwrap();
                store.advance(send);
                if store.fenced() {
                    return Err("shortcut admission unresolved".into());
                }
                if store.ready() {
                    Step::Deliver
                } else {
                    Step::WaitArm
                }
            }
            Step::Deliver => {
                let result = self
                    .staged
                    .take()
                    .unwrap()
                    .deliver(
                        self.store.as_ref().unwrap(),
                        self.journal_sequence,
                        &mut self.matcher,
                        controller,
                        Instant::now(),
                        guard,
                    )
                    .map_err(|_| "shortcut delivery failed")?;
                let phase = if result["ok"] == true {
                    Phase::Delivered
                } else {
                    Phase::RecoveryRequired
                };
                self.store
                    .as_mut()
                    .unwrap()
                    .finish(self.journal_sequence, phase, Some(result.clone()))
                    .map_err(|_| "shortcut delivery receipt unavailable")?;
                self.delivery = Some(result);
                Step::WaitDelivery
            }
            Step::WaitDelivery => {
                let store = self.store.as_mut().unwrap();
                store.advance(send);
                if store.fenced() {
                    return Err("shortcut delivery receipt unresolved".into());
                }
                if !store.ready() {
                    Step::WaitDelivery
                } else if self.delivery.as_ref().unwrap()["ok"] != true {
                    return Err("shortcut delivery requires recovery".into());
                } else if self.reservations.is_empty() {
                    return Ok(self.delivery.take().map(|mut response| {
                        response["operation_id"] = json!(self.request.operation_id);
                        response
                    }));
                } else {
                    Step::Dispatch
                }
            }
            Step::Dispatch => {
                if self.dispatch.is_none() {
                    self.dispatch = Some(DispatchJob::new(
                        self.store.as_ref().unwrap(),
                        self.journal_sequence,
                        self.dispatch_index,
                    )?);
                }
                if self
                    .dispatch
                    .as_mut()
                    .unwrap()
                    .advance(self.store.as_mut().unwrap(), send)?
                {
                    self.dispatch_index += 1;
                    self.dispatch = None;
                    if self.dispatch_index == self.reservations.len() {
                        return Ok(self.delivery.take().map(|mut response| {
                            response["operation_id"] = json!(self.request.operation_id);
                            response
                        }));
                    }
                }
                Step::Dispatch
            }
            Step::Done => return Err("shortcut job already finished".into()),
        };
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Key, KeyState, Operation};
    use std::{cell::RefCell, collections::BTreeMap, rc::Rc, time::Duration};

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
    struct Host {
        next: u64,
        revision: u64,
        value: Option<Value>,
        requests: BTreeMap<u64, (String, Value)>,
        actionful: bool,
        commits: usize,
        registry_requests: usize,
    }
    impl Host {
        fn send(&mut self, command: Command) -> ModuleResult<Response> {
            match command {
                Command::Request {
                    service,
                    payload,
                    operation_id,
                    ..
                } => {
                    assert!(
                        operation_id.len() <= 128
                            && operation_id.bytes().all(|b| b.is_ascii_lowercase()
                                || b.is_ascii_digit()
                                || b"._-/".contains(&b))
                    );
                    self.next += 1;
                    self.requests.insert(self.next, (service, payload));
                    Ok(Response::Accepted { id: self.next })
                }
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                Command::TakeResult { request } => {
                    let (service, payload) = self.requests.remove(&request).unwrap();
                    let reply = match service.as_str() {
                        "amaterasu.session-lifecycle/1" => {
                            match payload["operation"].as_str().unwrap() {
                                "validate_input_binding" => {
                                    json!({"status":"succeeded","batch_epoch":"input-epoch","target":{"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"stream","generation":1},"action_target":if self.actionful {json!({"kind":"session","host_epoch":"host","session_id":"session","expected_generation":1})} else {Value::Null},"active":true,"context":if self.actionful {"active"} else {"game"},"application":null,"paired_client":null,"allowed_actions":if self.actionful {json!(["provider/action"])} else {json!([])}})
                                }
                                "reserve_input_action" => {
                                    json!({"status":"succeeded","operation_id":"action:1","action_id":"provider/action","reservation_id":"permit","target":{"kind":"session","host_epoch":"host","session_id":"session","expected_generation":1}})
                                }
                                "finish_input_action" => json!({"status":"succeeded"}),
                                _ => panic!("unexpected lifecycle operation"),
                            }
                        }
                        "amaterasu.extension-registry/1" => {
                            self.registry_requests += 1;
                            if payload["operation"] == "list" {
                                json!({"ok":true,"revision":1,"items":if self.actionful {vec![crate::shortcut_discovery::tests::descriptor()]} else {vec![]},"next":null})
                            } else {
                                json!({"ok":true,"revision":1,"effective":{"shortcut":{"value":crate::shortcut_discovery::tests::binding(),"source":null}}})
                            }
                        }
                        "amaterasu.extension-actions/1" => {
                            match payload["operation"].as_str().unwrap() {
                                "open_epoch" => json!({"status":"ok","epoch":"action"}),
                                "reserve_input" => {
                                    assert_eq!(payload["expected_settings_revision"], "1");
                                    json!({"status":"reserved","operation_id":"action:1"})
                                }
                                "close_epoch" | "retire_epoch" => json!({"status":"ok"}),
                                "commit_input" => {
                                    self.commits += 1;
                                    assert_eq!(
                                        self.value.as_ref().unwrap()["receipts"]["1"]["phase"],
                                        "delivered"
                                    );
                                    assert_eq!(
                                        self.value.as_ref().unwrap()["receipts"]["1"]["intents"][0]
                                            ["phase"],
                                        "dispatching"
                                    );
                                    json!({"status":"succeeded","operation_id":"action:1"})
                                }
                                _ => panic!("unexpected action operation"),
                            }
                        }
                        "amaterasu.state/1" => match payload["action"].as_str().unwrap() {
                            "get" => {
                                json!({"ok":true,"revision":self.revision,"found":self.value.is_some(),"value":self.value})
                            }
                            "set" => {
                                assert_eq!(payload["expected_revision"], self.revision);
                                self.value = Some(payload["value"].clone());
                                self.revision += 1;
                                json!({"ok":true,"revision":self.revision})
                            }
                            _ => panic!("unexpected state operation"),
                        },
                        _ => panic!("unexpected service"),
                    };
                    Ok(Response::Result { payload: reply })
                }
                _ => panic!("unexpected command"),
            }
        }
    }
    #[test]
    fn validated_batch_waits_for_durable_receipt_before_delivery() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let mut controller = Controller::new(BackendFake(trace.clone()), true);
        let now = Instant::now();
        let acquire: Operation =
            serde_json::from_value(json!({"action":"acquire","ttl_ms":30000})).unwrap();
        let lease =
            controller.handle("amaterasu.stream-session", acquire, now, &mut || Ok(()))["lease"]
                .as_str()
                .unwrap()
                .to_owned();
        let event = Event::Key {
            key: Key::LinuxEvdev { code: 2 },
            state: KeyState::Down,
        };
        let payload = json!({"action":"batch","lease":lease,"input_binding":"authority-token","operation_id":"input-epoch:1","events":[event]});
        let mut job = ShortcutJob::new(
            7,
            "amaterasu.stream-session".into(),
            payload.clone(),
            LifecycleTarget::Stream {
                host_epoch: "host".into(),
                session_id: "session".into(),
                stream_id: "stream".into(),
                generation: 1,
            },
            now + Duration::from_secs(20),
            "boot".into(),
        )
        .unwrap();
        let mut host = Host::default();
        let mut result = None;
        for _ in 0..50 {
            if let Some(value) = job
                .advance(
                    &mut controller,
                    &mut |command| host.send(command),
                    &mut || Ok(()),
                )
                .unwrap()
            {
                result = Some(value);
                break;
            }
        }
        let result = result.expect("batch completed");
        assert_eq!(result["ok"], true);
        assert_eq!(result["operation_id"], "input-epoch:1");
        assert_eq!(trace.borrow().len(), 1);
        assert_eq!(
            host.value.as_ref().unwrap()["receipts"]["1"]["phase"],
            "delivered"
        );
        let target = LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        };
        let mut duplicate = ShortcutJob::new(
            8,
            "amaterasu.stream-session".into(),
            payload,
            target,
            now + Duration::from_secs(20),
            "boot".into(),
        )
        .unwrap();
        let mut duplicate_result = None;
        for _ in 0..50 {
            if let Some(value) = duplicate
                .advance(
                    &mut controller,
                    &mut |command| host.send(command),
                    &mut || Ok(()),
                )
                .unwrap()
            {
                duplicate_result = Some(value);
                break;
            }
        }
        assert_eq!(duplicate_result.unwrap()["operation_id"], "input-epoch:1");
        assert_eq!(trace.borrow().len(), 1);
    }
    #[test]
    fn actionful_batch_reserves_before_consuming_and_dispatches_after_delivery_receipt() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let mut controller = Controller::new(BackendFake(trace.clone()), true);
        let now = Instant::now();
        let acquire: Operation =
            serde_json::from_value(json!({"action":"acquire","ttl_ms":30000})).unwrap();
        let lease =
            controller.handle("amaterasu.stream-session", acquire, now, &mut || Ok(()))["lease"]
                .as_str()
                .unwrap()
                .to_owned();
        let event = Event::Key {
            key: Key::LinuxEvdev { code: 30 },
            state: KeyState::Down,
        };
        let payload = json!({"action":"batch","lease":lease,"input_binding":"authority-token","operation_id":"input-epoch:1","events":[event]});
        let target = LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        };
        let mut job = ShortcutJob::new(
            7,
            "amaterasu.stream-session".into(),
            payload,
            target,
            now + Duration::from_secs(20),
            "boot".into(),
        )
        .unwrap();
        let mut host = Host {
            actionful: true,
            ..Host::default()
        };
        let mut result = None;
        let mut verified = false;
        assert!(
            job.confirm_delivery("action", 1, "action:1", "permit")
                .is_none()
        );
        for _ in 0..100 {
            if let Some(value) = job
                .advance(
                    &mut controller,
                    &mut |command| host.send(command),
                    &mut || Ok(()),
                )
                .unwrap()
            {
                result = Some(value);
                break;
            }
            if let Some(proof) = job.confirm_delivery("action", 1, "action:1", "permit") {
                assert_eq!(proof["action_id"], "provider/action");
                assert_eq!(proof["batch_sequence"], 1);
                assert!(
                    job.confirm_delivery("action", 1, "action:1", "wrong")
                        .is_none()
                );
                let target = LifecycleTarget::Stream {
                    host_epoch: "host".into(),
                    session_id: "session".into(),
                    stream_id: "stream".into(),
                    generation: 1,
                };
                assert!(!job.can_transfer("wrong", &target, "action:1", "permit"));
                assert!(!job.can_transfer("authority-token", &target, "action:1", "wrong"));
                assert!(job.can_transfer("authority-token", &target, "action:1", "permit"));
                verified = true;
            }
        }
        assert!(verified);
        assert_eq!(result.unwrap()["ok"], true);
        assert!(trace.borrow().is_empty());
        assert_eq!(host.commits, 1);
        assert!(host.registry_requests >= 2);
        assert_eq!(
            host.value.unwrap()["receipts"]["1"]["intents"][0]["phase"],
            "terminal"
        );
    }
}
