//! Effect-owner ticket lifecycle for the pairing authority contract.
//!
//! The owner transports requests through the selected broker service with direct
//! caller/initiator identity. This state is deliberately non-durable: on unknown
//! outcome or worker loss, keep the authority fenced and recover actual workers
//! and queues before calling authority `recover`. Never replay an effect merely
//! because a response was lost. A ticket is a secret and is not logged here.
use super::{Request, Source, StreamRetirement, StreamScope};
use crate::transport::control_retirement::Receipt;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    AwaitAdmission,
    Ready,
    Running,
    AwaitTransportRetirement,
    HoldingTransport,
    NeedsCompletion,
    NeedsAcknowledgment,
    Done,
}

/// Exact whole-stream retirement operation, resolved by trusted composition.
#[derive(Clone)]
pub struct TransportRetirement {
    pub stream: u64,
    pub operation_id: String,
    pub provider_epoch: String,
}

/// One admitted operation; dropping it does not complete or cancel the ticket.
#[must_use = "an unresolved effect must remain fenced until reconciled"]
pub struct EffectTicket {
    epoch: String,
    source: Source,
    operation_id: String,
    sequence: u64,
    ticket: Option<String>,
    state: State,
    cancel_requested: bool,
    retirement: Option<TransportRetirement>,
}

fn valid_token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
fn valid_ticket(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl EffectTicket {
    pub fn new(
        epoch: String,
        source: Source,
        operation_id: String,
        sequence: u64,
    ) -> Result<Self, &'static str> {
        if !valid_ticket(&epoch) || !source.valid() || !valid_token(&operation_id) || sequence == 0
        {
            return Err("invalid_effect_operation");
        }
        Ok(Self {
            epoch,
            source,
            operation_id,
            sequence,
            ticket: None,
            state: State::AwaitAdmission,
            cancel_requested: false,
            retirement: None,
        })
    }
    pub fn state(&self) -> State {
        self.state
    }
    /// Reuse the identical payload and operation identity after uncertain broker admission.
    pub fn admit_request(&self) -> Result<Request, &'static str> {
        if self.state != State::AwaitAdmission {
            return Err("invalid_effect_state");
        }
        Ok(Request::Admit {
            epoch: self.epoch.clone(),
            source: self.source.clone(),
            operation_id: self.operation_id.clone(),
            sequence: self.sequence,
        })
    }
    pub fn lookup_request(&self) -> Result<Request, &'static str> {
        if self.state != State::AwaitAdmission {
            return Err("invalid_effect_state");
        }
        Ok(Request::Lookup {
            epoch: self.epoch.clone(),
            operation_id: self.operation_id.clone(),
        })
    }
    /// Accept only a response from the selected authority's authenticated broker route.
    pub fn accept_admission(&mut self, response: &Value) -> Result<(), &'static str> {
        if response.get("kind").is_some() {
            return Err("invalid_effect_response");
        }
        self.accept_ticket(response, false)
    }
    /// Fenced lookup results may only be completed; they cannot start an effect.
    pub fn accept_lookup(&mut self, response: &Value) -> Result<(), &'static str> {
        if response.get("kind").is_some() {
            return Err("invalid_effect_response");
        }
        self.accept_ticket(response, true)
    }
    fn accept_ticket(&mut self, response: &Value, lookup: bool) -> Result<(), &'static str> {
        if self.state != State::AwaitAdmission || !self.response_matches(response) {
            return Err("invalid_effect_response");
        }
        let ticket = response
            .get("ticket")
            .and_then(Value::as_str)
            .filter(|v| valid_ticket(v))
            .ok_or("invalid_effect_response")?;
        let fenced = if lookup {
            response
                .get("fenced")
                .and_then(Value::as_bool)
                .ok_or("invalid_effect_response")?
        } else {
            if response.get("fenced").is_some() {
                return Err("invalid_effect_response");
            }
            false
        };
        self.ticket = Some(ticket.into());
        self.state = if fenced || self.cancel_requested {
            State::NeedsCompletion
        } else {
            State::Ready
        };
        Ok(())
    }
    fn response_matches(&self, response: &Value) -> bool {
        response.get("ok").and_then(Value::as_bool) == Some(true)
            && response.get("epoch").and_then(Value::as_str) == Some(self.epoch.as_str())
            && response.get("operation_id").and_then(Value::as_str)
                == Some(self.operation_id.as_str())
    }
    /// Cancelling an uncertain admission still requires lookup/reconciliation.
    /// Running work or a possibly queued transport message must still retire.
    pub fn cancel(&mut self) {
        self.cancel_requested = true;
        if self.state == State::Ready {
            self.state = State::NeedsCompletion;
        }
    }
    /// Execute once while retaining the admitted ticket across the synchronous
    /// input effect. The caller must also account for any asynchronous provider
    /// work before completion. A panic leaves the ticket unresolved.
    pub fn run_local<T>(&mut self, effect: impl FnOnce() -> T) -> Result<T, &'static str> {
        if self.state != State::Ready {
            return Err("invalid_effect_state");
        }
        self.state = State::Running;
        let result = effect();
        self.state = State::NeedsCompletion;
        Ok(result)
    }
    /// Keep the ticket after a successful or uncertain transport admission.
    /// `Ok(false)` must mean the provider accepted nothing (e.g. BUSY).
    pub fn run_transport<E>(
        &mut self,
        retirement: TransportRetirement,
        send: impl FnOnce() -> Result<bool, E>,
    ) -> Result<Result<bool, E>, &'static str> {
        if self.state != State::Ready
            || retirement.stream == 0
            || !valid_token(&retirement.operation_id)
            || !valid_ticket(&retirement.provider_epoch)
        {
            return Err("invalid_effect_state");
        }
        self.retirement = Some(retirement);
        self.state = State::Running;
        let result = send();
        self.state = match &result {
            Ok(false) if self.cancel_requested => State::NeedsCompletion,
            Ok(false) => State::Ready,
            _ => State::AwaitTransportRetirement,
        };
        Ok(result)
    }
    /// Accept only a broker-authenticated receipt from the selected transport.
    pub fn transport_retired(&mut self, receipt: &Receipt) -> Result<(), &'static str> {
        let expected = self.retirement.as_ref().ok_or("invalid_effect_state")?;
        if self.state != State::AwaitTransportRetirement
            || !receipt.matches(
                expected.stream,
                &expected.operation_id,
                &expected.provider_epoch,
            )
        {
            return Err("invalid_retirement_receipt");
        }
        self.state = State::NeedsCompletion;
        Ok(())
    }
    pub fn completion_request(&self) -> Result<Request, &'static str> {
        if self.state != State::NeedsCompletion {
            return Err("invalid_effect_state");
        }
        Ok(Request::Complete {
            epoch: self.epoch.clone(),
            operation_id: self.operation_id.clone(),
            ticket: self.ticket.clone().ok_or("invalid_effect_state")?,
        })
    }
    pub fn confirm_completion(&mut self, response: &Value) -> Result<(), &'static str> {
        if self.state != State::NeedsCompletion || !self.response_matches(response) {
            return Err("invalid_effect_response");
        }
        self.state = State::NeedsAcknowledgment;
        Ok(())
    }
    pub fn acknowledgment_request(&self) -> Result<Request, &'static str> {
        if self.state != State::NeedsAcknowledgment {
            return Err("invalid_effect_state");
        }
        Ok(Request::Acknowledge {
            epoch: self.epoch.clone(),
            operation_id: self.operation_id.clone(),
            ticket: self.ticket.clone().ok_or("invalid_effect_state")?,
        })
    }
    pub fn confirm_acknowledgment(&mut self, response: &Value) -> Result<(), &'static str> {
        if self.state != State::NeedsAcknowledgment || !self.response_matches(response) {
            return Err("invalid_effect_response");
        }
        self.ticket = None;
        self.state = State::Done;
        Ok(())
    }
}

/// Exact stream lease admission and retirement handshake. The response must be
/// delivered by the selected pairing service; payload fields are not authority.
#[must_use = "an unresolved stream lease must remain fenced until reconciled"]
pub struct StreamLeaseTicket {
    effect: EffectTicket,
    scope: StreamScope,
    retirement: Option<StreamRetirement>,
}

impl StreamLeaseTicket {
    pub fn new(
        epoch: String,
        source: Source,
        operation_id: String,
        sequence: u64,
        scope: StreamScope,
    ) -> Result<Self, &'static str> {
        if !scope.valid() || !valid_ticket(&scope.transport_epoch) {
            return Err("invalid_stream_scope");
        }
        Ok(Self {
            effect: EffectTicket::new(epoch, source, operation_id, sequence)?,
            scope,
            retirement: None,
        })
    }
    pub fn state(&self) -> State {
        self.effect.state
    }
    pub fn admit_request(&self) -> Result<Request, &'static str> {
        if self.effect.state != State::AwaitAdmission {
            return Err("invalid_effect_state");
        }
        Ok(Request::AdmitStreamLease {
            epoch: self.effect.epoch.clone(),
            source: self.effect.source.clone(),
            operation_id: self.effect.operation_id.clone(),
            sequence: self.effect.sequence,
            scope: self.scope.clone(),
        })
    }
    pub fn lookup_request(&self) -> Result<Request, &'static str> {
        if self.effect.state != State::AwaitAdmission {
            return Err("invalid_effect_state");
        }
        Ok(Request::LookupStreamLease {
            epoch: self.effect.epoch.clone(),
            source: self.effect.source.clone(),
            operation_id: self.effect.operation_id.clone(),
            scope: self.scope.clone(),
        })
    }
    pub fn accept_admission(&mut self, response: &Value) -> Result<(), &'static str> {
        if response.get("kind").and_then(Value::as_str) != Some("stream_lease") {
            return Err("invalid_effect_response");
        }
        self.effect.accept_ticket(response, false)
    }
    pub fn accept_lookup(&mut self, response: &Value) -> Result<(), &'static str> {
        if response.get("kind").and_then(Value::as_str) != Some("stream_lease")
            || serde_json::from_value::<Source>(
                response
                    .get("source")
                    .cloned()
                    .ok_or("invalid_effect_response")?,
            )
            .ok()
            .as_ref()
                != Some(&self.effect.source)
            || serde_json::from_value::<StreamScope>(
                response
                    .get("scope")
                    .cloned()
                    .ok_or("invalid_effect_response")?,
            )
            .ok()
            .as_ref()
                != Some(&self.scope)
        {
            return Err("invalid_effect_response");
        }
        self.effect.accept_ticket(response, true)
    }
    /// Cancellation still requires retirement, even if admission was uncertain.
    pub fn cancel(&mut self) {
        self.effect.cancel();
    }
    pub fn completion_request(&self) -> Result<Request, &'static str> {
        if self.effect.state != State::NeedsCompletion {
            return Err("invalid_effect_state");
        }
        Ok(Request::CompleteStreamLease {
            epoch: self.effect.epoch.clone(),
            operation_id: self.effect.operation_id.clone(),
            ticket: self.effect.ticket.clone().ok_or("invalid_effect_state")?,
            retirement: self.retirement.clone().ok_or("invalid_effect_state")?,
        })
    }
    pub fn confirm_completion(&mut self, response: &Value) -> Result<(), &'static str> {
        self.effect.confirm_completion(response)
    }
    pub fn acknowledgment_request(&self) -> Result<Request, &'static str> {
        self.effect.acknowledgment_request()
    }
    pub fn confirm_acknowledgment(&mut self, response: &Value) -> Result<(), &'static str> {
        self.effect.confirm_acknowledgment(response)
    }
}

/// Canonical pairing scope identity for a local feedback schema and direction.
pub fn feedback_capability_identity(
    capability: &crate::input::extensions::Capability,
) -> Result<String, &'static str> {
    if !capability.valid() || capability.direction != crate::input::extensions::Direction::ToClient
    {
        return Err("invalid_feedback_capability");
    }
    let identity = format!("{}@{}:to_client", capability.schema, capability.version);
    if !valid_token(&identity) {
        return Err("invalid_feedback_capability");
    }
    Ok(identity)
}

/// A single admitted source/stream lease with a fixed negotiated capability set.
///
/// The selected owner must derive this immutable scope from authenticated
/// lifecycle and protocol state, never a client assertion. The pairing ledger
/// matches that scope at admission; the protocol must authenticate the source
/// join and capability negotiation before production composition opens.
#[must_use = "dropping a lease leaves its authority ticket unresolved"]
pub struct FeedbackLease {
    aggregate: Option<StreamLeaseTicket>,
    retirement: TransportRetirement,
    capabilities: std::collections::BTreeSet<crate::input::extensions::Capability>,
    controller_slot: u16,
    fenced: bool,
    last_sequence: u64,
    pending_retry: Option<PendingFeedback>,
}

struct PendingFeedback {
    sequence: u64,
    operation_id: String,
    capability: crate::input::extensions::Capability,
    bytes: Vec<u8>,
}

impl FeedbackLease {
    pub fn new(
        mut aggregate: StreamLeaseTicket,
        retirement: TransportRetirement,
        controller_slot: u16,
        capabilities: std::collections::BTreeSet<crate::input::extensions::Capability>,
    ) -> Result<Self, Box<(StreamLeaseTicket, &'static str)>> {
        use crate::input::extensions::{Direction, MAX_CAPABILITIES};
        let identities = capabilities
            .iter()
            .map(feedback_capability_identity)
            .collect::<Result<std::collections::BTreeSet<_>, _>>();
        if !matches!(
            aggregate.effect.state,
            State::Ready | State::NeedsCompletion
        ) || identities.as_ref().ok() != Some(&aggregate.scope.capabilities)
            || retirement.stream != aggregate.scope.transport_stream
            || retirement.provider_epoch != aggregate.scope.transport_epoch
            || retirement.stream == 0
            || !valid_token(&retirement.operation_id)
            || !valid_ticket(&retirement.provider_epoch)
            || capabilities.is_empty()
            || capabilities.len() > MAX_CAPABILITIES
            || capabilities
                .iter()
                .any(|cap| !cap.valid() || cap.direction != Direction::ToClient)
        {
            return Err(Box::new((aggregate, "invalid_effect_state")));
        }
        let fenced = aggregate.effect.state == State::NeedsCompletion;
        aggregate.effect.state = State::HoldingTransport;
        Ok(Self {
            aggregate: Some(aggregate),
            retirement,
            capabilities,
            controller_slot,
            fenced,
            last_sequence: 0,
            pending_retry: None,
        })
    }

    pub fn stream(&self) -> u64 {
        self.retirement.stream
    }
    pub fn controller_slot(&self) -> u16 {
        self.controller_slot
    }

    /// The owner calls this as soon as it observes the authority's revoke fence.
    pub fn fence(&mut self) {
        self.fenced = true;
    }

    /// Consume an authenticated authority status. Any epoch/owner change,
    /// malformed response or drain request closes this local lease permanently.
    /// The owner must also stop and retire its workers after this returns an
    /// error; polling latency is not itself a revocation barrier.
    pub fn observe_status(&mut self, response: &Value) -> Result<(), &'static str> {
        let Some(aggregate) = self.aggregate.as_ref() else {
            self.fenced = true;
            return Err("invalid_effect_state");
        };
        let current = response.get("ok").and_then(Value::as_bool) == Some(true)
            && response.get("epoch").and_then(Value::as_str)
                == Some(aggregate.effect.epoch.as_str())
            && response.get("owner_epoch").and_then(Value::as_str)
                == Some(aggregate.effect.source.effect_owner_epoch.as_str())
            && response.get("drain_required").and_then(Value::as_bool) == Some(false)
            && response
                .get("startup_recovery_required")
                .and_then(Value::as_bool)
                == Some(false);
        if !current || self.fenced {
            self.fenced = true;
            return Err("feedback_lease_fenced");
        }
        Ok(())
    }

    /// Perform ordered local work under the fixed lease scope. `Ok(false)` must
    /// prove no admission; it retains that exact message for retry. `Err` may
    /// mean admission occurred, so it is never replayed and the lease remains
    /// outstanding until the transport is retired.
    pub fn enqueue<E>(
        &mut self,
        capability: &crate::input::extensions::Capability,
        sequence: u64,
        operation_id: &str,
        exact_message: &[u8],
        send: impl FnOnce() -> Result<bool, E>,
    ) -> Result<Result<bool, E>, &'static str> {
        let aggregate = self.aggregate.as_ref().ok_or("invalid_effect_state")?;
        if self.fenced
            || aggregate.effect.state != State::HoldingTransport
            || !self.capabilities.contains(capability)
            || sequence == 0
            || !valid_token(operation_id)
            || exact_message.len() > crate::input::extensions::MAX_PAYLOAD_BYTES
        {
            return Err("invalid_effect_state");
        }
        match &self.pending_retry {
            Some(pending)
                if pending.sequence == sequence
                    && pending.operation_id == operation_id
                    && &pending.capability == capability
                    && pending.bytes == exact_message => {}
            Some(_) => return Err("feedback_retry_pending"),
            None if sequence <= self.last_sequence => return Err("feedback_sequence_replayed"),
            None => {
                self.pending_retry = Some(PendingFeedback {
                    sequence,
                    operation_id: operation_id.into(),
                    capability: capability.clone(),
                    bytes: exact_message.to_vec(),
                })
            }
        }
        let outcome = send();
        if !matches!(outcome, Ok(false)) {
            self.last_sequence = sequence;
            self.pending_retry = None;
        }
        Ok(outcome)
    }

    /// Explicitly discard only a known-busy message; this never recalls work.
    pub fn cancel_retry(&mut self, sequence: u64, operation_id: &str) -> Result<(), &'static str> {
        if self
            .pending_retry
            .as_ref()
            .map(|pending| (pending.sequence, pending.operation_id.as_str()))
            != Some((sequence, operation_id))
        {
            return Err("invalid_effect_state");
        }
        self.last_sequence = sequence;
        self.pending_retry = None;
        Ok(())
    }

    /// Call after local workers quiesce and the selected provider confirms whole
    /// stream retirement. Only then can the aggregate ticket be completed.
    pub fn retire(&mut self, receipt: &Receipt) -> Result<StreamLeaseTicket, &'static str> {
        if !self.fenced
            || self.pending_retry.is_some()
            || !receipt.matches(
                self.retirement.stream,
                &self.retirement.operation_id,
                &self.retirement.provider_epoch,
            )
        {
            return Err("invalid_retirement_receipt");
        }
        let mut aggregate = self.aggregate.take().ok_or("invalid_effect_state")?;
        aggregate.retirement = Some(StreamRetirement {
            ok: receipt.ok,
            service: aggregate.scope.transport_service.clone(),
            stream: receipt.stream,
            provider_epoch: receipt.provider_epoch.clone(),
            operation_id: receipt.operation_id.clone(),
            scope: receipt.scope.clone(),
            state: receipt.state.clone(),
        });
        aggregate.effect.state = State::NeedsCompletion;
        Ok(aggregate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source() -> Source {
        Source {
            version: 1,
            id: "a".repeat(32),
            generation: 2,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            stream_generation: 2,
            device: "device".into(),
            device_generation: 7,
            effect_owner_epoch: "owner".into(),
        }
    }
    fn ticket() -> EffectTicket {
        EffectTicket::new("e".repeat(32), source(), "operation".into(), 1).unwrap()
    }
    fn admission() -> Value {
        json!({"ok":true,"epoch":"e".repeat(32),"operation_id":"operation","ticket":"a".repeat(32)})
    }
    fn receipt() -> Receipt {
        Receipt {
            ok: true,
            state: "retired".into(),
            scope: "whole_control_stream".into(),
            stream: 7,
            operation_id: "retire".into(),
            provider_epoch: "c".repeat(32),
        }
    }
    fn retirement() -> TransportRetirement {
        TransportRetirement {
            stream: 7,
            operation_id: "retire".into(),
            provider_epoch: "c".repeat(32),
        }
    }
    fn confirmation() -> Value {
        json!({"ok":true,"epoch":"e".repeat(32),"operation_id":"operation"})
    }

    #[test]
    fn local_effect_runs_once_and_receipt_cleanup_requires_exact_confirmation() {
        let mut op = ticket();
        assert!(op.run_local(|| 1).is_err());
        let mut wrong = admission();
        wrong["epoch"] = json!("f".repeat(32));
        assert!(op.accept_admission(&wrong).is_err());
        op.accept_admission(&admission()).unwrap();
        assert_eq!(op.run_local(|| 4), Ok(4));
        assert!(op.run_local(|| panic!("must never replay")).is_err());
        assert!(op.acknowledgment_request().is_err());
        assert!(matches!(
            op.completion_request().unwrap(),
            Request::Complete { .. }
        ));
        op.confirm_completion(&confirmation()).unwrap();
        assert!(matches!(
            op.acknowledgment_request().unwrap(),
            Request::Acknowledge { .. }
        ));
        op.confirm_acknowledgment(&confirmation()).unwrap();
        assert_eq!(op.state(), State::Done);
    }

    #[test]
    fn queued_and_uncertain_sends_hold_ticket_until_exact_whole_stream_retirement() {
        for failed in [false, true] {
            let mut op = ticket();
            op.accept_admission(&admission()).unwrap();
            let result = op
                .run_transport(retirement(), || {
                    if failed { Err("uncertain") } else { Ok(true) }
                })
                .unwrap();
            assert_eq!(result.is_err(), failed);
            assert_eq!(op.state(), State::AwaitTransportRetirement);
            op.cancel();
            assert!(op.completion_request().is_err());
            let mut wrong = receipt();
            wrong.stream = 8;
            assert!(op.transport_retired(&wrong).is_err());
            op.transport_retired(&receipt()).unwrap();
            assert!(op.completion_request().is_ok());
        }
    }

    #[test]
    fn busy_does_not_send_and_cancellation_or_fenced_lookup_never_starts_work() {
        let mut op = ticket();
        op.accept_admission(&admission()).unwrap();
        assert_eq!(
            op.run_transport(retirement(), || Ok::<_, ()>(false))
                .unwrap(),
            Ok(false)
        );
        assert_eq!(op.state(), State::Ready);
        op.cancel();
        assert!(op.run_local(|| panic!("cancelled")).is_err());
        assert!(op.completion_request().is_ok());
        let mut lost = ticket();
        lost.cancel();
        assert!(lost.completion_request().is_err());
        let mut lookup = admission();
        lookup["fenced"] = json!(true);
        lost.accept_lookup(&lookup).unwrap();
        assert_eq!(lost.state(), State::NeedsCompletion);
        assert!(lost.run_local(|| panic!("fenced")).is_err());
        let mut bad = ticket();
        assert!(bad.accept_lookup(&admission()).is_err());
        assert!(bad.accept_admission(&lookup).is_err());
    }
    #[test]
    fn scoped_feedback_rejects_capability_expansion_and_changed_busy_retries() {
        use crate::input::extensions::{Capability, Direction};
        let capability = Capability {
            schema: "org.example.rumble".into(),
            version: 1,
            direction: Direction::ToClient,
        };
        let other = Capability {
            schema: "org.example.speaker".into(),
            version: 1,
            direction: Direction::ToClient,
        };
        let scope = StreamScope {
            capabilities: [feedback_capability_identity(&capability).unwrap()].into(),
            transport_service: "amaterasu.control-transport/1".into(),
            transport_stream: 7,
            transport_epoch: "c".repeat(32),
        };
        let mut aggregate =
            StreamLeaseTicket::new("e".repeat(32), source(), "operation".into(), 1, scope).unwrap();
        let mut response = admission();
        response["kind"] = json!("stream_lease");
        aggregate.accept_admission(&response).unwrap();
        let mut lease = FeedbackLease::new(aggregate, retirement(), 7, [capability.clone()].into())
            .unwrap_or_else(|_| panic!("lease"));
        assert!(
            lease
                .enqueue(&other, 1, "one", b"stop", || -> Result<bool, ()> {
                    panic!("unnegotiated")
                })
                .is_err()
        );
        assert_eq!(
            lease
                .enqueue(&capability, 1, "one", b"stop", || Ok::<_, ()>(false))
                .unwrap(),
            Ok(false)
        );
        assert!(
            lease
                .enqueue(&capability, 1, "one", b"start", || -> Result<bool, ()> {
                    panic!("changed retry")
                })
                .is_err()
        );
        assert!(
            lease
                .enqueue(&capability, 2, "two", b"stop", || -> Result<bool, ()> {
                    panic!("out of order")
                })
                .is_err()
        );
        assert!(lease.retire(&receipt()).is_err());
        lease.fence();
        assert!(lease.retire(&receipt()).is_err());
        lease.cancel_retry(1, "one").unwrap();
        let aggregate = lease
            .retire(&receipt())
            .unwrap_or_else(|_| panic!("retirement"));
        assert_eq!(aggregate.state(), State::NeedsCompletion);
    }
}
