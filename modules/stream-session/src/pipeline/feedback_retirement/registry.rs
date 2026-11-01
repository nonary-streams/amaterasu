//! Exact in-process ownership of source admissions and their feedback queues.
//! Registry slots are reserved before broker admission so an uncertain reply
//! cannot be forgotten. No wire payload passed here is an authority by itself.
use crate::plane::source_feedback::SourceFeedbackPending;
use amaterasu_extension_api::LifecycleTarget;
use amaterasu_module_sdk::{
    effect_authority::{
        FeedbackDirection, Request, Source, StreamScope,
        consumer::{FeedbackLease, StreamLeaseTicket, TransportRetirement},
    },
    input::extensions::{Capability, Direction},
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_SOURCES: usize = 64;
const MAX_RETIRED_TRANSPORTS: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct Transport {
    pub service: String,
    pub stream: u64,
    pub epoch: String,
}
impl Transport {
    fn of(scope: &StreamScope) -> Self {
        Self {
            service: scope.transport_service.clone(),
            stream: scope.transport_stream,
            epoch: scope.transport_epoch.clone(),
        }
    }
    pub fn retirement_operation(&self) -> String {
        format!("feedback-retire-{}-{}", self.epoch, self.stream)
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Binding {
    pub token: String,
    pub target: LifecycleTarget,
    pub input_boot_epoch: String,
}
impl Binding {
    fn valid_for(&self, source: &Source) -> bool {
        !self.token.is_empty()
            && self.token.len() <= 256
            && !self.token.chars().any(char::is_control)
            && self.input_boot_epoch.len() == 32
            && self
                .input_boot_epoch
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && self.target
                == (LifecycleTarget::Stream {
                    host_epoch: source.host_epoch.clone(),
                    session_id: source.session_id.clone(),
                    stream_id: source.stream_id.clone(),
                    generation: source.stream_generation,
                })
    }
}

enum LeaseState {
    Pending(Box<StreamLeaseTicket>),
    Active(Box<SourceFeedbackPending>),
    Fenced(Box<FeedbackLease>),
    Completed(Box<StreamLeaseTicket>),
    NoTicket,
    Poisoned,
}
pub(super) struct SourceRecord {
    pub epoch: String,
    pub source: Source,
    pub scope: StreamScope,
    pub binding: Binding,
    state: LeaseState,
    sent_sequence: Option<u64>,
    retired_receipt: Option<amaterasu_module_sdk::transport::control_retirement::Receipt>,
}
impl SourceRecord {
    pub fn fence_local(&mut self) -> Result<(), &'static str> {
        let old = std::mem::replace(&mut self.state, LeaseState::Poisoned);
        self.state = match old {
            LeaseState::Active(mut queue) => match queue.fence() {
                Ok(lease) => LeaseState::Fenced(Box::new(lease)),
                Err(_) => {
                    self.state = LeaseState::Active(queue);
                    return Err("source_fence_unconfirmed");
                }
            },
            LeaseState::Pending(mut ticket) => {
                ticket.cancel();
                LeaseState::Pending(ticket)
            }
            other => other,
        };
        if matches!(
            self.state,
            LeaseState::Pending(_)
                | LeaseState::Fenced(_)
                | LeaseState::Completed(_)
                | LeaseState::NoTicket
        ) {
            Ok(())
        } else {
            Err("source_admission_unresolved")
        }
    }
}

impl SourceRecord {
    pub fn retire_transport(
        &mut self,
        receipt: &amaterasu_module_sdk::transport::control_retirement::Receipt,
    ) -> Result<(), &'static str> {
        if matches!(self.state, LeaseState::Pending(_)) {
            self.retired_receipt = Some(
                amaterasu_module_sdk::transport::control_retirement::Receipt {
                    ok: receipt.ok,
                    state: receipt.state.clone(),
                    scope: receipt.scope.clone(),
                    stream: receipt.stream,
                    operation_id: receipt.operation_id.clone(),
                    provider_epoch: receipt.provider_epoch.clone(),
                },
            );
            return Ok(());
        }
        if matches!(self.state, LeaseState::Completed(_)) {
            return if self.retired_receipt.as_ref() == Some(receipt) {
                Ok(())
            } else {
                Err("source_receipt_conflict")
            };
        }
        let LeaseState::Fenced(lease) = &mut self.state else {
            return Err("source_fence_unconfirmed");
        };
        let ticket = lease.retire(receipt)?;
        self.retired_receipt = Some(
            amaterasu_module_sdk::transport::control_retirement::Receipt {
                ok: receipt.ok,
                state: receipt.state.clone(),
                scope: receipt.scope.clone(),
                stream: receipt.stream,
                operation_id: receipt.operation_id.clone(),
                provider_epoch: receipt.provider_epoch.clone(),
            },
        );
        self.state = LeaseState::Completed(Box::new(ticket));
        Ok(())
    }
    pub fn pending_lookup_request(&self) -> Option<Request> {
        match &self.state {
            LeaseState::Pending(ticket) => ticket.lookup_request().ok(),
            _ => None,
        }
    }
    pub fn has_no_ticket(&self) -> bool {
        matches!(self.state, LeaseState::NoTicket)
    }
    pub fn resolve_pending(&mut self, response: &Value) -> Result<(), &'static str> {
        if response["ok"] == false && response["code"] == "unknown_admission" {
            if !matches!(self.state, LeaseState::Pending(_)) {
                return Err("source_admission_conflict");
            }
            self.state = LeaseState::NoTicket;
            return Ok(());
        }
        let LeaseState::Pending(ticket) = &mut self.state else {
            return Err("source_admission_conflict");
        };
        ticket.accept_lookup(response)?;
        let old = std::mem::replace(&mut self.state, LeaseState::Poisoned);
        let LeaseState::Pending(ticket) = old else {
            unreachable!()
        };
        let capabilities = self
            .scope
            .capabilities
            .iter()
            .map(|capability| Capability {
                schema: capability.schema.clone(),
                version: capability.version,
                direction: Direction::ToClient,
            })
            .collect();
        let transport = Transport::of(&self.scope);
        let retirement = TransportRetirement {
            stream: transport.stream,
            operation_id: transport.retirement_operation(),
            provider_epoch: transport.epoch,
        };
        match FeedbackLease::new(
            *ticket,
            retirement,
            self.scope.controller_slot,
            capabilities,
        ) {
            Ok(lease) => self.state = LeaseState::Fenced(Box::new(lease)),
            Err(error) => {
                let (ticket, code) = *error;
                self.state = LeaseState::Pending(Box::new(ticket));
                return Err(code);
            }
        }
        let receipt = self
            .retired_receipt
            .take()
            .ok_or("source_transport_unconfirmed")?;
        self.retire_transport(&receipt)
    }
    pub fn completion_request(&self) -> Result<Request, &'static str> {
        let LeaseState::Completed(ticket) = &self.state else {
            return Err("source_transport_unconfirmed");
        };
        ticket.completion_request()
    }
    pub fn confirm_completion(&mut self, response: &Value) -> Result<(), &'static str> {
        let LeaseState::Completed(ticket) = &mut self.state else {
            return Err("source_transport_unconfirmed");
        };
        ticket.confirm_completion(response)
    }
    pub fn acknowledgment_request(&self) -> Result<Request, &'static str> {
        let LeaseState::Completed(ticket) = &self.state else {
            return Err("source_transport_unconfirmed");
        };
        ticket.acknowledgment_request()
    }
    pub fn confirm_acknowledgment(&mut self, response: &Value) -> Result<(), &'static str> {
        let LeaseState::Completed(ticket) = &mut self.state else {
            return Err("source_transport_unconfirmed");
        };
        ticket.confirm_acknowledgment(response)
    }
}

#[derive(Default)]
pub(super) struct Registry {
    entries: BTreeMap<Source, SourceRecord>,
    closed: BTreeSet<Transport>,
}
impl Registry {
    pub fn first_source(&self) -> Option<(String, String, Binding, Transport)> {
        self.entries.values().next().map(|entry| {
            (
                entry.epoch.clone(),
                entry.source.effect_owner_epoch.clone(),
                entry.binding.clone(),
                Transport::of(&entry.scope),
            )
        })
    }
    pub fn source_snapshots(&self) -> Vec<(String, Source, StreamScope, Binding, Transport)> {
        self.entries
            .values()
            .map(|entry| {
                (
                    entry.epoch.clone(),
                    entry.source.clone(),
                    entry.scope.clone(),
                    entry.binding.clone(),
                    Transport::of(&entry.scope),
                )
            })
            .collect()
    }
    pub fn affected_by_input(
        &self,
        target: &LifecycleTarget,
        input: &amaterasu_gamestream::control::Input<'_>,
    ) -> Option<(Binding, Transport)> {
        use amaterasu_gamestream::control::Input;
        self.entries
            .values()
            .find(|entry| {
                if &entry.binding.target != target {
                    return false;
                }
                let slot = entry.scope.controller_slot;
                match input {
                    Input::ControllerArrival(arrival) => u16::from(arrival.number) == slot,
                    Input::Controller(state) => {
                        slot < 16 && state.active_mask & (1u16 << slot) == 0
                    }
                    Input::EnableHaptics { enabled: false } => true,
                    _ => false,
                }
            })
            .map(|entry| (entry.binding.clone(), Transport::of(&entry.scope)))
    }
    pub fn fence_local_transport(&mut self, transport: &Transport) -> Result<(), &'static str> {
        if !self.closed.contains(transport) && self.closed.len() >= MAX_RETIRED_TRANSPORTS {
            return Err("transport_tombstone_capacity");
        }
        self.closed.insert(transport.clone());
        for entry in self
            .entries
            .values_mut()
            .filter(|entry| Transport::of(&entry.scope) == *transport)
        {
            entry.fence_local()?;
        }
        Ok(())
    }
    pub fn bindings_on(&self, transport: &Transport) -> usize {
        let mut bindings = Vec::new();
        for entry in self
            .entries
            .values()
            .filter(|entry| Transport::of(&entry.scope) == *transport)
        {
            if !bindings.contains(&entry.binding) {
                bindings.push(entry.binding.clone());
            }
        }
        bindings.len()
    }
    pub fn first_stale(
        &self,
        records: &BTreeMap<u64, crate::pipeline::record::Record>,
    ) -> Option<(Binding, Transport)> {
        self.entries
            .values()
            .find(|entry| {
                !records.values().any(|record| {
                    record
                        .start
                        .as_ref()
                        .and_then(|start| start.lifecycle_target.as_ref())
                        == Some(&entry.binding.target)
                        && record.client_haptics_requested
                        && record.input_controller_supported
                        && record.lifecycle_input_token.as_deref()
                            == Some(entry.binding.token.as_str())
                        && record
                            .feedback_device_observations
                            .get(&entry.scope.controller_slot)
                            .is_some_and(|(generation, _)| {
                                *generation == entry.source.device_generation
                            })
                        && record
                            .feedback_source_observations
                            .get(&entry.scope.controller_slot)
                            .is_some_and(|observation| {
                                observation["input_boot_epoch"] == entry.binding.input_boot_epoch
                                    && observation["source"]["id"] == entry.source.id
                            })
                })
            })
            .map(|entry| (entry.binding.clone(), Transport::of(&entry.scope)))
    }
    pub fn has_binding(&self, binding: &Binding) -> bool {
        self.entries.values().any(|entry| &entry.binding == binding)
    }
    pub fn has_slot(&self, binding: &Binding, slot: u16) -> bool {
        self.entries
            .values()
            .any(|entry| &entry.binding == binding && entry.scope.controller_slot == slot)
    }
    /// Reserve before sending bind/admission; errors or lost replies leave this
    /// slot intact until the selected owner reconciles or retires it.
    pub fn reserve(
        &mut self,
        epoch: String,
        source: Source,
        scope: StreamScope,
        binding: Binding,
        operation: String,
        sequence: u64,
    ) -> Result<Request, &'static str> {
        if !source.valid()
            || !scope.valid()
            || !binding.valid_for(&source)
            || self.entries.len() >= MAX_SOURCES
            || self.closed.contains(&Transport::of(&scope))
            || self.entries.contains_key(&source)
        {
            return Err("source_registry_unavailable");
        }
        if self.entries.values().any(|entry| {
            entry.source.host_epoch == source.host_epoch
                && entry.source.session_id == source.session_id
                && entry.scope.controller_slot == scope.controller_slot
        }) {
            return Err("source_slot_busy");
        }
        let ticket = StreamLeaseTicket::new(
            epoch.clone(),
            source.clone(),
            operation,
            sequence,
            scope.clone(),
        )?;
        let request = ticket.admit_request()?;
        self.entries.insert(
            source.clone(),
            SourceRecord {
                epoch,
                source,
                scope,
                binding,
                state: LeaseState::Pending(Box::new(ticket)),
                sent_sequence: None,
                retired_receipt: None,
            },
        );
        Ok(request)
    }
    /// The caller must obtain this response directly from the selected pairing
    /// broker service after NVHTTP verified the same source/scope join.
    pub fn accept_admission(
        &mut self,
        source: &Source,
        response: &Value,
    ) -> Result<(), &'static str> {
        let entry = self
            .entries
            .get_mut(source)
            .ok_or("source_registry_unavailable")?;
        let LeaseState::Pending(ticket) = &mut entry.state else {
            return Err("source_admission_conflict");
        };
        ticket.accept_admission(response)?;
        let old = std::mem::replace(&mut entry.state, LeaseState::Poisoned);
        let LeaseState::Pending(ticket) = old else {
            unreachable!()
        };
        let capabilities: BTreeSet<Capability> = entry
            .scope
            .capabilities
            .iter()
            .map(|capability| Capability {
                schema: capability.schema.clone(),
                version: capability.version,
                direction: match capability.direction {
                    FeedbackDirection::ToClient => Direction::ToClient,
                },
            })
            .collect();
        let transport = Transport::of(&entry.scope);
        let retirement = TransportRetirement {
            stream: transport.stream,
            operation_id: transport.retirement_operation(),
            provider_epoch: transport.epoch,
        };
        match FeedbackLease::new(
            *ticket,
            retirement,
            entry.scope.controller_slot,
            capabilities,
        ) {
            Ok(lease) => {
                entry.state = LeaseState::Active(Box::new(SourceFeedbackPending::new(lease)));
                Ok(())
            }
            Err(error) => {
                let (ticket, code) = *error;
                entry.state = LeaseState::Pending(Box::new(ticket));
                Err(code)
            }
        }
    }
    /// Route only an exact source/device generation through its retained lease.
    pub fn route_feedback(
        &mut self,
        binding: &Binding,
        slot: u16,
        generation: u64,
        sequence: u64,
        feedback: amaterasu_gamestream::control::HostFeedback,
        plane: &mut dyn crate::pipeline::Drive,
    ) -> Result<bool, &'static str> {
        use crate::plane::source_feedback::{Admission, Progress};
        let Some(entry) = self.entries.values_mut().find(|entry| {
            &entry.binding == binding
                && entry.scope.controller_slot == slot
                && entry.source.device_generation == generation
        }) else {
            return Ok(false);
        };
        let LeaseState::Active(queue) = &mut entry.state else {
            return Ok(false);
        };
        let operation = format!("feedback-{sequence}");
        match queue
            .enqueue(sequence, &operation, feedback)
            .map_err(|_| "source_feedback_rejected")?
        {
            Admission::AlreadySent | Admission::AlreadyDropped => {
                return Ok(entry.sent_sequence == Some(sequence));
            }
            Admission::Pending | Admission::AlreadyPending => {}
        }
        match plane.pump_source_feedback(queue)? {
            Progress::Sent(sent) if sent == sequence => {
                entry.sent_sequence = Some(sequence);
                Ok(true)
            }
            Progress::AwaitingAcknowledgment(sent) if sent == sequence => {
                Ok(entry.sent_sequence == Some(sequence))
            }
            Progress::Busy | Progress::Empty => Ok(false),
            _ => Err("source_feedback_sequence_conflict"),
        }
    }
    pub fn acknowledge_feedback(
        &mut self,
        binding: &Binding,
        through: u64,
    ) -> Result<(), &'static str> {
        for entry in self
            .entries
            .values_mut()
            .filter(|entry| &entry.binding == binding)
        {
            let Some(sequence) = entry.sent_sequence.filter(|sequence| *sequence <= through) else {
                continue;
            };
            let LeaseState::Active(queue) = &mut entry.state else {
                return Err("source_feedback_stopped");
            };
            queue
                .acknowledge(sequence, &format!("feedback-{sequence}"))
                .map_err(|_| "source_feedback_ack_conflict")?;
            entry.sent_sequence = None;
        }
        Ok(())
    }
    pub fn transport_for(&self, binding: &Binding) -> Result<Transport, &'static str> {
        let mut transports = self
            .entries
            .values()
            .filter(|entry| &entry.binding == binding)
            .map(|entry| Transport::of(&entry.scope));
        let transport = transports.next().ok_or("source_registry_unavailable")?;
        if transports.any(|candidate| candidate != transport) {
            return Err("ambiguous_source_transport");
        }
        Ok(transport)
    }
    /// Fences every source on the complete transport incarnation. An unresolved
    /// admission prevents extraction and remains tracked behind the closed key.
    #[cfg(test)]
    pub fn take_transport(
        &mut self,
        binding: &Binding,
    ) -> Result<(Transport, Vec<SourceRecord>), &'static str> {
        let transport = self.transport_for(binding)?;
        let records = self.take_exact_transport(&transport)?;
        Ok((transport, records))
    }
    pub fn take_exact_transport(
        &mut self,
        transport: &Transport,
    ) -> Result<Vec<SourceRecord>, &'static str> {
        if !self.closed.contains(transport) && self.closed.len() >= MAX_RETIRED_TRANSPORTS {
            return Err("transport_tombstone_capacity");
        }
        self.closed.insert(transport.clone());
        let keys: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, entry)| Transport::of(&entry.scope) == *transport)
            .map(|(source, _)| source.clone())
            .collect();
        for source in &keys {
            self.entries
                .get_mut(source)
                .expect("source retained")
                .fence_local()?;
        }
        let records = keys
            .into_iter()
            .map(|source| self.entries.remove(&source).expect("source retained"))
            .collect();
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::effect_authority::{FeedbackCapability, FeedbackDirection};
    use serde_json::json;
    fn source(slot: u16) -> Source {
        Source {
            version: 1,
            id: "a".repeat(32),
            generation: 2,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            stream_generation: 2,
            device: format!("device-{slot}"),
            device_generation: 1,
            effect_owner_epoch: "owner".into(),
        }
    }
    fn scope(slot: u16) -> StreamScope {
        StreamScope {
            capabilities: [FeedbackCapability {
                schema: "org.amaterasu.feedback.rumble".into(),
                version: 1,
                direction: FeedbackDirection::ToClient,
            }]
            .into(),
            controller_slot: slot,
            transport_service: "control/1".into(),
            transport_stream: 7,
            transport_epoch: "c".repeat(32),
        }
    }
    fn binding() -> Binding {
        Binding {
            token: "binding".into(),
            target: LifecycleTarget::Stream {
                host_epoch: "host".into(),
                session_id: "session".into(),
                stream_id: "stream".into(),
                generation: 2,
            },
            input_boot_epoch: "b".repeat(32),
        }
    }
    fn admission(slot: u16) -> Value {
        json!({"ok":true,"epoch":"e".repeat(32),"operation_id":format!("lease-{slot}"),"ticket":format!("{slot:032x}"),"kind":"stream_lease","source":source(slot),"scope":scope(slot)})
    }
    #[derive(Default)]
    struct FakePlane {
        sent: usize,
    }
    impl crate::pipeline::Drive for FakePlane {
        fn tick(&mut self, _: bool) -> Result<crate::plane::Progress, crate::plane::TickFailure> {
            Ok(Default::default())
        }
        fn send_rumble(&mut self, _: crate::plane::RumbleCommand) -> Result<bool, &'static str> {
            Ok(false)
        }
        fn stop(&mut self) {}
        fn is_drained(&self) -> bool {
            false
        }
        fn recover_audio(&mut self) {}
        fn enqueue_feedback(
            &mut self,
            _: amaterasu_gamestream::control::HostFeedback,
        ) -> Result<crate::plane::FeedbackEnqueueOutcome, crate::plane::FeedbackEnqueueError>
        {
            Ok(crate::plane::FeedbackEnqueueOutcome::Queued)
        }
        fn pump_source_feedback(
            &mut self,
            queue: &mut SourceFeedbackPending,
        ) -> Result<crate::plane::source_feedback::Progress, &'static str> {
            queue.pump(|lease, sequence, operation, feedback| {
                let (_, capability) = crate::plane::feedback_scope(&feedback);
                lease
                    .enqueue(&capability, sequence, operation, &[1, 2, 3], || {
                        self.sent += 1;
                        Ok::<bool, &'static str>(true)
                    })
                    .map_err(|_| "denied")?
            })
        }
    }
    #[test]
    fn replacement_plane_routes_only_exact_generation_and_retains_ack() {
        use amaterasu_gamestream::control::HostFeedback;
        let mut registry = Registry::default();
        registry
            .reserve(
                "e".repeat(32),
                source(1),
                scope(1),
                binding(),
                "lease-1".into(),
                1,
            )
            .unwrap();
        registry
            .accept_admission(&source(1), &admission(1))
            .unwrap();
        let mut plane = FakePlane::default();
        let feedback = HostFeedback::Rumble {
            controller: 1,
            low_frequency: 0,
            high_frequency: 0,
        };
        assert!(
            !registry
                .route_feedback(&binding(), 1, 2, 1, feedback.clone(), &mut plane)
                .unwrap()
        );
        assert!(
            registry
                .route_feedback(&binding(), 1, 1, 1, feedback.clone(), &mut plane)
                .unwrap()
        );
        assert!(
            registry
                .route_feedback(&binding(), 1, 1, 1, feedback.clone(), &mut plane)
                .unwrap()
        );
        assert_eq!(plane.sent, 1);
        registry.acknowledge_feedback(&binding(), 1).unwrap();
        assert!(
            registry
                .route_feedback(&binding(), 1, 1, 1, feedback, &mut plane)
                .is_err()
        );
    }

    #[test]
    fn source_retirement_detection_is_scoped_to_owned_slot_and_target() {
        use amaterasu_gamestream::control::{ControllerArrival, ControllerState, Input};
        let mut registry = Registry::default();
        registry
            .reserve(
                "e".repeat(32),
                source(1),
                scope(1),
                binding(),
                "lease-1".into(),
                1,
            )
            .unwrap();
        registry
            .accept_admission(&source(1), &admission(1))
            .unwrap();
        let arrival = |number| {
            Input::ControllerArrival(ControllerArrival {
                number,
                controller_type: 2,
                capabilities: 0,
                supported_buttons: 0,
            })
        };
        assert!(
            registry
                .affected_by_input(&binding().target, &arrival(2))
                .is_none()
        );
        assert!(
            registry
                .affected_by_input(&binding().target, &arrival(1))
                .is_some()
        );
        let state = |active_mask| {
            Input::Controller(ControllerState {
                number: 1,
                active_mask,
                buttons: 0,
                left_trigger: 0,
                right_trigger: 0,
                left_x: 0,
                left_y: 0,
                right_x: 0,
                right_y: 0,
            })
        };
        assert!(
            registry
                .affected_by_input(&binding().target, &state(2))
                .is_none()
        );
        assert!(
            registry
                .affected_by_input(&binding().target, &state(0))
                .is_some()
        );
        let other = LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "other".into(),
            stream_id: "stream".into(),
            generation: 2,
        };
        assert!(registry.affected_by_input(&other, &arrival(1)).is_none());
    }

    #[test]
    fn closes_shared_transport_and_keeps_uncertain_admissions() {
        let mut registry = Registry::default();
        for slot in [1, 2] {
            registry
                .reserve(
                    "e".repeat(32),
                    source(slot),
                    scope(slot),
                    binding(),
                    format!("lease-{slot}"),
                    1,
                )
                .unwrap();
        }
        let mut forged = admission(1);
        forged["scope"]["controller_slot"] = json!(2);
        assert!(registry.accept_admission(&source(1), &forged).is_err());
        registry
            .accept_admission(&source(1), &admission(1))
            .unwrap();
        let (transport, mut sources) = registry.take_transport(&binding()).unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(transport.stream, 7);
        assert!(
            registry
                .reserve(
                    "e".repeat(32),
                    source(3),
                    scope(3),
                    binding(),
                    "lease-3".into(),
                    1
                )
                .is_err()
        );
        assert!(sources[1].pending_lookup_request().is_some());
        assert!(!sources[0].has_no_ticket());
        assert!(
            sources[1]
                .resolve_pending(&json!({"ok":false,"code":"unknown_admission"}))
                .is_ok()
        );
        assert!(sources[1].has_no_ticket());
    }
}
