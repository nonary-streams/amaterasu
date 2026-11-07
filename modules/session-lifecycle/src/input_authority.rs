//! Coordinator-owned input binding and in-flight reservation state.
//!
//! This is not an authentication boundary by itself. The coordinator must use
//! broker-attested identities, resolve `ActiveBinding` exclusively from its own
//! validated active stream and exact consumer acknowledgements, and serialize
//! all calls with quiescence. Clone, mutate, and persist this state atomically
//! with the canonical stream before exposing any result or dispatching effects.
use crate::action_authority::ActionTarget;
use amaterasu_module_sdk::input::InputBindingRetirement;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_BINDINGS: usize = 64;
pub(crate) const MAX_RETAINED_INPUT_BINDINGS: usize = 128;
const MAX_RESERVATIONS: usize = 256;
// Terminal replies are retained without eviction; admission reserves capacity.
const MAX_RESERVATION_RECEIPTS: usize = 4096;
// Leave space in the 1 MiB coordinator journal for its other state. Live
// records also reserve headroom for a later transfer owner and terminal outcome.
const MAX_RESERVATION_BYTES: usize = 256 * 1024;
const TERMINAL_RECORD_HEADROOM: usize = MAX_TEXT + 32;
const MAX_ACTIONS: usize = 32;
const MAX_TEXT: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveBinding {
    pub host_epoch: String,
    pub session_id: String,
    pub stream_id: String,
    pub generation: u64,
    pub geometry_revision: u64,
    /// Broker identity of the approved input service, resolved from policy.
    pub input_consumer: String,
    pub allowed_actions: BTreeSet<String>,
}

fn target_matches_binding(target: &ActionTarget, binding: &ActiveBinding) -> bool {
    match target {
        ActionTarget::Session {
            host_epoch,
            session_id,
            expected_generation,
        } => {
            host_epoch == &binding.host_epoch
                && session_id == &binding.session_id
                && *expected_generation > 0
        }
        ActionTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            expected_generation,
        } => {
            host_epoch == &binding.host_epoch
                && session_id == &binding.session_id
                && stream_id == &binding.stream_id
                && *expected_generation == binding.generation
        }
        ActionTarget::Host { .. } => false,
    }
}

impl ActiveBinding {
    fn valid(&self) -> bool {
        [
            &self.host_epoch,
            &self.session_id,
            &self.stream_id,
            &self.input_consumer,
        ]
        .into_iter()
        .all(|s| valid_text(s))
            && self.generation > 0
            && self.geometry_revision > 0
            && self.allowed_actions.len() <= MAX_ACTIONS
            && self.allowed_actions.iter().all(|s| valid_text(s))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Running,
    Completed,
    Cancelled,
    RecoveryRequired,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    pub reservation_id: String,
    pub operation_id: String,
    pub token: String,
    pub caller: String,
    pub initiator: String,
    pub binding: ActiveBinding,
    pub action_id: String,
    /// Exact canonical target selected when the reservation was admitted.
    #[serde(default)]
    pub target: Option<ActionTarget>,
    pub outcome: Outcome,
    #[serde(default)]
    pub transferred_to: Option<String>,
}

impl std::fmt::Debug for Reservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Reservation")
            .field("reservation_id", &"[redacted]")
            .field("token", &"[redacted]")
            .field("operation_id", &self.operation_id)
            .field("caller", &self.caller)
            .field("initiator", &self.initiator)
            .field("binding", &self.binding)
            .field("action_id", &self.action_id)
            .field("outcome", &self.outcome)
            .finish()
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputAuthority {
    bindings: BTreeMap<String, ActiveBinding>,
    // Live entries include running, uncertain and unacknowledged terminal work.
    reservations: BTreeMap<String, Reservation>,
    // Historical receipts are never authority to reserve or dispatch again.
    #[serde(default)]
    terminal_receipts: BTreeMap<String, Reservation>,
    /// Native retirement is already proved. These metadata receipts survive
    /// restart until the selected input provider acknowledges forgetting them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    retirement_receipts: BTreeMap<String, InputBindingRetirement>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Unauthorized,
    Conflict,
    Capacity,
    RandomUnavailable,
    NotFound,
    UnknownOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReservationResult {
    Admitted(Reservation),
    Existing(Reservation),
}

impl InputAuthority {
    pub fn validate(&self) -> bool {
        self.bindings.len() <= MAX_BINDINGS
            && self.retirement_receipts.len() <= MAX_RETAINED_INPUT_BINDINGS
            && self.retirement_receipts.iter().all(|(token, receipt)| {
                token == &receipt.token
                    && receipt.validate().is_ok()
                    && !self.bindings.contains_key(token)
            })
            && self.reservations.len() <= MAX_RESERVATIONS
            && self.reservations.len() + self.terminal_receipts.len() <= MAX_RESERVATION_RECEIPTS
            && self.terminal_receipts.iter().all(|(key, receipt)| {
                matches!(receipt.outcome, Outcome::Completed | Outcome::Cancelled)
                    && !self.reservations.contains_key(key)
            })
            && self
                .bindings
                .iter()
                .all(|(token, binding)| valid_token(token) && binding.valid())
            && self
                .reservations
                .iter()
                .chain(&self.terminal_receipts)
                .all(|(key, r)| {
                    *key == operation_key(&r.caller, &r.initiator, &r.operation_id)
                        && valid_token(&r.reservation_id)
                        && valid_token(&r.token)
                        && valid_text(&r.operation_id)
                        && valid_text(&r.caller)
                        && valid_text(&r.initiator)
                        && r.binding.valid()
                        && r.caller == r.binding.input_consumer
                        && r.binding.allowed_actions.contains(&r.action_id)
                        && r.target
                            .as_ref()
                            .is_none_or(|target| target_matches_binding(target, &r.binding))
                        && r.transferred_to.as_deref().is_none_or(valid_text)
                })
            && self
                .reservations
                .values()
                .chain(self.terminal_receipts.values())
                .map(|r| &r.reservation_id)
                .collect::<BTreeSet<_>>()
                .len()
                == self.reservations.len() + self.terminal_receipts.len()
    }

    /// Only a coordinator-validated active binding and policy-selected consumer
    /// may be passed here. Persist the returned token before delivering it.
    pub fn issue_input_binding(&mut self, active: &ActiveBinding) -> Result<String, Error> {
        if !active.valid() {
            return Err(Error::Invalid);
        }
        if self.bindings.len() >= MAX_BINDINGS {
            return Err(Error::Capacity);
        }
        let token = unique_id(|id| {
            self.bindings.contains_key(id)
                || self.retirement_receipts.contains_key(id)
                || self
                    .reservations
                    .values()
                    .chain(self.terminal_receipts.values())
                    .any(|entry| entry.token == id)
        })?;
        self.bindings.insert(token.clone(), active.clone());
        Ok(token)
    }

    pub(crate) fn record_retirement(
        &mut self,
        receipt: InputBindingRetirement,
    ) -> Result<(), Error> {
        receipt.validate().map_err(|_| Error::Invalid)?;
        if self.bindings.contains_key(&receipt.token) {
            return Err(Error::Unauthorized);
        }
        if let Some(previous) = self.retirement_receipts.get(&receipt.token) {
            return if previous == &receipt {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        if self.retirement_receipts.len() >= MAX_RETAINED_INPUT_BINDINGS {
            return Err(Error::Capacity);
        }
        self.retirement_receipts
            .insert(receipt.token.clone(), receipt);
        Ok(())
    }

    pub(crate) fn pending_retirements(&self) -> impl Iterator<Item = &InputBindingRetirement> {
        self.retirement_receipts.values()
    }

    pub(crate) fn acknowledge_retirement(
        &mut self,
        receipt: &InputBindingRetirement,
    ) -> Result<(), Error> {
        if self.retirement_receipts.get(&receipt.token) != Some(receipt) {
            return Err(Error::Conflict);
        }
        self.retirement_receipts.remove(&receipt.token);
        Ok(())
    }

    /// The coordinator must independently resolve `active` for every call.
    pub fn validate_input_binding(
        &self,
        caller: &str,
        token: &str,
        active: &ActiveBinding,
    ) -> Result<ActiveBinding, Error> {
        let binding = self.bindings.get(token).ok_or(Error::Unauthorized)?;
        if caller != binding.input_consumer || active != binding || !active.valid() {
            return Err(Error::Unauthorized);
        }
        Ok(binding.clone())
    }

    /// Only `Admitted` permits dispatch, after persistence. Existing is a
    /// receipt lookup, never authority to dispatch the native effect again.
    pub fn reserve_input_action(
        &mut self,
        caller: &str,
        initiator: &str,
        operation_id: &str,
        token: &str,
        action_id: &str,
        active: &ActiveBinding,
    ) -> Result<ReservationResult, Error> {
        self.reserve_input_action_inner(
            caller,
            initiator,
            operation_id,
            token,
            action_id,
            active,
            None,
        )
    }

    /// Bind the reservation to the exact coordinator-validated target.
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_input_action_for_target(
        &mut self,
        caller: &str,
        initiator: &str,
        operation_id: &str,
        token: &str,
        action_id: &str,
        active: &ActiveBinding,
        target: &ActionTarget,
    ) -> Result<ReservationResult, Error> {
        self.reserve_input_action_inner(
            caller,
            initiator,
            operation_id,
            token,
            action_id,
            active,
            Some(target),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn reserve_input_action_inner(
        &mut self,
        caller: &str,
        initiator: &str,
        operation_id: &str,
        token: &str,
        action_id: &str,
        active: &ActiveBinding,
        target: Option<&ActionTarget>,
    ) -> Result<ReservationResult, Error> {
        if target.is_some_and(|target| !target_matches_binding(target, active))
            || !valid_text(caller)
            || !valid_text(initiator)
            || !valid_text(operation_id)
            || !valid_text(action_id)
        {
            return Err(Error::Invalid);
        }
        let key = operation_key(caller, initiator, operation_id);
        if self.terminal_receipts.contains_key(&key) {
            return Err(Error::Conflict);
        }
        if let Some(old) = self.reservations.get(&key) {
            return if old.token == token
                && old.action_id == action_id
                && old.binding == *active
                && old.target.as_ref() == target
                && caller == old.binding.input_consumer
            {
                Ok(ReservationResult::Existing(old.clone()))
            } else {
                Err(Error::Conflict)
            };
        }
        let binding = self.validate_input_binding(caller, token, active)?;
        if !binding.allowed_actions.contains(action_id) {
            return Err(Error::Unauthorized);
        }
        if self.reservations.len() >= MAX_RESERVATIONS
            || self.reservations.len() + self.terminal_receipts.len() >= MAX_RESERVATION_RECEIPTS
        {
            return Err(Error::Capacity);
        }
        let reservation_id = unique_id(|id| {
            self.reservations
                .values()
                .chain(self.terminal_receipts.values())
                .any(|r| r.reservation_id == id)
        })?;
        let reservation = Reservation {
            reservation_id,
            operation_id: operation_id.into(),
            token: token.into(),
            caller: caller.into(),
            initiator: initiator.into(),
            binding,
            action_id: action_id.into(),
            target: target.cloned(),
            outcome: Outcome::Running,
            transferred_to: None,
        };
        let current_bytes = serde_json::to_vec(&(&self.reservations, &self.terminal_receipts))
            .map_err(|_| Error::Invalid)?
            .len();
        let added_bytes = serde_json::to_vec(&(&key, &reservation))
            .map_err(|_| Error::Invalid)?
            .len();
        let headroom = (self.reservations.len() + 1).saturating_mul(TERMINAL_RECORD_HEADROOM);
        if current_bytes
            .saturating_add(added_bytes)
            .saturating_add(headroom)
            > MAX_RESERVATION_BYTES
        {
            return Err(Error::Capacity);
        }
        self.reservations.insert(key, reservation.clone());
        Ok(ReservationResult::Admitted(reservation))
    }

    pub fn lookup_input_action(
        &self,
        caller: &str,
        initiator: &str,
        operation_id: &str,
    ) -> Result<Reservation, Error> {
        let key = operation_key(caller, initiator, operation_id);
        self.reservations
            .get(&key)
            .or_else(|| self.terminal_receipts.get(&key))
            .cloned()
            .ok_or(Error::NotFound)
    }

    /// An uncertain effect cannot be completed by a normal finish call.
    pub fn finish_input_action(
        &mut self,
        caller: &str,
        initiator: &str,
        operation_id: &str,
        reservation_id: &str,
        outcome: Outcome,
    ) -> Result<Reservation, Error> {
        if outcome == Outcome::Running {
            return Err(Error::Invalid);
        }
        let key = operation_key(caller, initiator, operation_id);
        if let Some(receipt) = self.terminal_receipts.get(&key) {
            if receipt.reservation_id != reservation_id {
                return Err(Error::Unauthorized);
            }
            return if receipt.outcome == outcome {
                Ok(receipt.clone())
            } else {
                Err(Error::Conflict)
            };
        }
        let r = self.reservations.get_mut(&key).ok_or(Error::NotFound)?;
        if r.reservation_id != reservation_id {
            return Err(Error::Unauthorized);
        }
        if r.outcome == Outcome::RecoveryRequired {
            return if outcome == Outcome::RecoveryRequired {
                Ok(r.clone())
            } else {
                Err(Error::UnknownOutcome)
            };
        }
        if r.outcome != Outcome::Running && r.outcome != outcome {
            return Err(Error::Conflict);
        }
        r.outcome = outcome.clone();
        let result = r.clone();
        if matches!(outcome, Outcome::Completed | Outcome::Cancelled) {
            self.reservations.remove(&key);
            self.terminal_receipts.insert(key, result.clone());
        }
        Ok(result)
    }

    /// Lookup of an exact opaque reservation for authenticated coordinator use.
    pub fn resolve_reservation(&self, reservation_id: &str) -> Result<Reservation, Error> {
        if !self.validate() {
            return Err(Error::Invalid);
        }
        self.reservations
            .values()
            .chain(self.terminal_receipts.values())
            .find(|reservation| reservation.reservation_id == reservation_id)
            .cloned()
            .ok_or(Error::NotFound)
    }

    /// Resolve only a live reservation; the caller must authenticate the
    /// action service and independently validate the canonical session owner.
    pub fn resolve_running_reservation(&self, reservation_id: &str) -> Result<Reservation, Error> {
        if !self.validate() {
            return Err(Error::Invalid);
        }
        self.reservations
            .values()
            .find(|reservation| {
                reservation.reservation_id == reservation_id
                    && reservation.outcome == Outcome::Running
            })
            .cloned()
            .ok_or(Error::NotFound)
    }

    /// Transfer one exact running reservation to an authenticated action
    /// service. Persist before returning; retries by the same owner are safe.
    pub fn transfer_reservation(
        &mut self,
        reservation_id: &str,
        action_service: &str,
        operation_id: &str,
        action_id: &str,
    ) -> Result<Reservation, Error> {
        if !valid_text(action_service) {
            return Err(Error::Invalid);
        }
        let reservation = self
            .reservations
            .values_mut()
            .find(|entry| entry.reservation_id == reservation_id)
            .ok_or(Error::NotFound)?;
        if reservation.outcome != Outcome::Running
            || reservation.operation_id != operation_id
            || reservation.action_id != action_id
        {
            return Err(Error::Unauthorized);
        }
        if reservation
            .transferred_to
            .as_deref()
            .is_some_and(|owner| owner != action_service)
        {
            return Err(Error::Conflict);
        }
        reservation.transferred_to = Some(action_service.into());
        Ok(reservation.clone())
    }

    /// Record a trusted action-service cancellation only when the caller has
    /// independently established that no action permit was ever issued. This
    /// does not claim that the input batch itself was undelivered.
    pub fn cancel_transferred_reservation(
        &mut self,
        reservation_id: &str,
        service: &str,
        operation_id: &str,
        action_id: &str,
    ) -> Result<(), Error> {
        let reservation = self
            .reservations
            .values_mut()
            .find(|entry| entry.reservation_id == reservation_id)
            .ok_or(Error::NotFound)?;
        if reservation.transferred_to.as_deref() != Some(service)
            || reservation.operation_id != operation_id
            || reservation.action_id != action_id
        {
            return Err(Error::Unauthorized);
        }
        match reservation.outcome {
            Outcome::Running => {
                reservation.outcome = Outcome::Cancelled;
                Ok(())
            }
            Outcome::Cancelled => Ok(()),
            Outcome::RecoveryRequired => Err(Error::UnknownOutcome),
            Outcome::Completed => Err(Error::Conflict),
        }
    }

    /// Fence the exact generation, excluding only an exact running reservation
    /// already transferred to the supplied authenticated action service.
    pub fn fence_except(
        &mut self,
        active: &ActiveBinding,
        reservation_id: &str,
        action_service: &str,
    ) -> Result<usize, Error> {
        let reservation = self.resolve_running_reservation(reservation_id)?;
        if &reservation.binding != active
            || reservation.transferred_to.as_deref() != Some(action_service)
        {
            return Err(Error::Unauthorized);
        }
        self.bindings
            .retain(|_, binding| !same_generation(binding, active));
        Ok(self
            .reservations
            .values()
            .filter(|entry| {
                same_generation(&entry.binding, active)
                    && entry.reservation_id != reservation_id
                    && matches!(entry.outcome, Outcome::Running | Outcome::RecoveryRequired)
            })
            .count())
    }

    /// Called under the same coordinator lock as the canonical generation fence.
    /// A nonzero return means retirement must wait or report recovery required.
    pub fn fence(&mut self, active: &ActiveBinding) -> usize {
        self.bindings
            .retain(|_, binding| !same_generation(binding, active));
        self.reservations
            .values()
            .filter(|r| {
                same_generation(&r.binding, active)
                    && matches!(r.outcome, Outcome::Running | Outcome::RecoveryRequired)
            })
            .count()
    }

    /// Terminal reservations remain historical references even after their
    /// effects finish; root retirement must wait for separate replay-safe GC.
    #[allow(dead_code, reason = "awaits the atomic ended-session transaction")]
    pub(crate) fn has_any_session_reference(&self, session_id: &str) -> bool {
        self.bindings
            .values()
            .any(|binding| binding.session_id == session_id)
            || self.reservations.values().any(|entry| {
                entry.binding.session_id == session_id
                    || match &entry.target {
                        Some(ActionTarget::Session { session_id: id, .. })
                        | Some(ActionTarget::Stream { session_id: id, .. }) => id == session_id,
                        Some(ActionTarget::Host { .. }) => true,
                        None => false,
                    }
            })
    }

    /// Read-only checkpoint gate. The exact transferred action reservation
    /// remains durable and running; every other input in the session fences.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn checkpoint_allows_transferred_action(
        &self,
        host_epoch: &str,
        session_id: &str,
        stream_id: &str,
        generation: u64,
        reservation_id: &str,
        action_service: &str,
        action: &crate::action_authority::ActionBinding,
    ) -> bool {
        let Ok(reservation) = self.resolve_running_reservation(reservation_id) else {
            return false;
        };
        reservation.transferred_to.as_deref() == Some(action_service)
            && reservation.operation_id == action.operation_id
            && reservation.action_id == action.action_id
            && reservation.target.as_ref() == Some(&action.target)
            && reservation.binding.host_epoch == host_epoch
            && reservation.binding.session_id == session_id
            && reservation.binding.stream_id == stream_id
            && reservation.binding.generation == generation
            && !self
                .bindings
                .values()
                .any(|binding| binding.session_id == session_id)
            && !self.reservations.values().any(|entry| {
                entry.binding.session_id == session_id
                    && entry.reservation_id != reservation_id
                    && matches!(entry.outcome, Outcome::Running | Outcome::RecoveryRequired)
            })
    }

    pub fn has_unresolved_session(&self, _host_epoch: &str, session_id: &str) -> bool {
        self.bindings
            .values()
            .any(|binding| binding.session_id == session_id)
            || self.reservations.values().any(|entry| {
                entry.binding.session_id == session_id
                    && matches!(entry.outcome, Outcome::Running | Outcome::RecoveryRequired)
            })
    }

    pub fn has_unresolved_stream(
        &self,
        _host_epoch: &str,
        session_id: &str,
        stream_id: &str,
    ) -> bool {
        self.bindings
            .values()
            .any(|binding| binding.session_id == session_id && binding.stream_id == stream_id)
            || self.reservations.values().any(|entry| {
                entry.binding.session_id == session_id
                    && entry.binding.stream_id == stream_id
                    && matches!(entry.outcome, Outcome::Running | Outcome::RecoveryRequired)
            })
    }

    /// Prior tokens cannot cross a host epoch and running effects become unknown.
    pub fn recover_after_restart(&mut self) {
        self.bindings.clear();
        // Already-proved native retirement is not a new input authority and
        // must not be discarded with the previous epoch's live bindings.
        for r in self.reservations.values_mut() {
            if r.outcome == Outcome::Running {
                r.outcome = Outcome::RecoveryRequired;
            }
        }
    }
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_TEXT && !value.chars().any(char::is_control)
}
fn same_generation(a: &ActiveBinding, b: &ActiveBinding) -> bool {
    a.host_epoch == b.host_epoch
        && a.session_id == b.session_id
        && a.stream_id == b.stream_id
        && a.generation == b.generation
}
fn valid_token(token: &str) -> bool {
    token.len() == 32
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn operation_key(caller: &str, initiator: &str, operation: &str) -> String {
    serde_json::to_string(&(caller, initiator, operation)).expect("string tuple serializes")
}
fn unique_id(exists: impl Fn(&str) -> bool) -> Result<String, Error> {
    for _ in 0..4 {
        let id = amaterasu_storage::random_identifier().map_err(|_| Error::RandomUnavailable)?;
        if !exists(&id) {
            return Ok(id);
        }
    }
    Err(Error::RandomUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> ActiveBinding {
        ActiveBinding {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
            geometry_revision: 2,
            input_consumer: "input".into(),
            allowed_actions: BTreeSet::from(["display".into()]),
        }
    }
    #[test]
    fn finished_input_receipt_is_replayable_but_never_readmitted_or_live() {
        let mut book = InputAuthority::default();
        let active = binding();
        let token = book.issue_input_binding(&active).unwrap();
        let target = ActionTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            expected_generation: 1,
        };
        let ReservationResult::Admitted(reservation) = book
            .reserve_input_action_for_target(
                "input", "client", "op", &token, "display", &active, &target,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            book.finish_input_action("input", "client", "op", "wrong", Outcome::Completed),
            Err(Error::Unauthorized)
        );
        assert!(book.has_any_session_reference("session"));
        let terminal = book
            .finish_input_action(
                "input",
                "client",
                "op",
                &reservation.reservation_id,
                Outcome::Completed,
            )
            .unwrap();
        book.fence(&active);
        assert!(!book.has_any_session_reference("session"));
        let mut restored: InputAuthority =
            serde_json::from_slice(&serde_json::to_vec(&book).unwrap()).unwrap();
        assert!(restored.validate());
        assert_eq!(
            restored.lookup_input_action("input", "client", "op"),
            Ok(terminal.clone())
        );
        assert_eq!(
            restored.resolve_reservation(&reservation.reservation_id),
            Ok(terminal.clone())
        );
        assert_eq!(
            restored.finish_input_action(
                "input",
                "client",
                "op",
                &reservation.reservation_id,
                Outcome::Completed
            ),
            Ok(terminal)
        );
        assert_eq!(
            restored.finish_input_action(
                "input",
                "client",
                "op",
                &reservation.reservation_id,
                Outcome::Cancelled
            ),
            Err(Error::Conflict)
        );
        assert_eq!(
            restored.finish_input_action(
                "foreign",
                "client",
                "op",
                &reservation.reservation_id,
                Outcome::Completed
            ),
            Err(Error::NotFound)
        );
        assert_eq!(
            restored.reserve_input_action_for_target(
                "input", "client", "op", &token, "display", &active, &target
            ),
            Err(Error::Conflict)
        );
        assert_eq!(
            restored.resolve_running_reservation(&reservation.reservation_id),
            Err(Error::NotFound)
        );
        assert!(
            restored
                .transfer_reservation(&reservation.reservation_id, "actions", "op", "display")
                .is_err()
        );
    }

    #[test]
    fn acknowledged_receipts_do_not_exhaust_live_reservations() {
        let mut book = InputAuthority::default();
        let active = binding();
        let token = book.issue_input_binding(&active).unwrap();
        for index in 0..(MAX_RESERVATIONS + 1) {
            let operation = format!("op-{index}");
            let ReservationResult::Admitted(reservation) = book
                .reserve_input_action("input", "client", &operation, &token, "display", &active)
                .unwrap()
            else {
                panic!()
            };
            book.finish_input_action(
                "input",
                "client",
                &operation,
                &reservation.reservation_id,
                Outcome::Completed,
            )
            .unwrap();
        }
        assert!(book.reservations.is_empty());
        assert_eq!(book.terminal_receipts.len(), MAX_RESERVATIONS + 1);
        assert!(book.validate());
    }

    #[test]
    fn receipt_byte_capacity_rejects_before_admission_without_losing_replays() {
        let mut book = InputAuthority::default();
        let mut active = binding();
        active.allowed_actions = (0..32)
            .map(|index| format!("a{index:02}{}", "x".repeat(120)))
            .collect();
        let action = active.allowed_actions.iter().next().unwrap().clone();
        let token = book.issue_input_binding(&active).unwrap();
        let mut first = None;
        let mut failed = false;
        for index in 0..MAX_RESERVATIONS {
            let op = format!("op-{index}");
            match book.reserve_input_action("input", "client", &op, &token, &action, &active) {
                Ok(ReservationResult::Admitted(reservation)) => {
                    let receipt = book
                        .finish_input_action(
                            "input",
                            "client",
                            &op,
                            &reservation.reservation_id,
                            Outcome::Completed,
                        )
                        .unwrap();
                    if first.is_none() {
                        first = Some(receipt);
                    }
                }
                Err(Error::Capacity) => {
                    failed = true;
                    break;
                }
                other => panic!("unexpected admission: {other:?}"),
            }
        }
        assert!(failed);
        assert!(book.reservations.is_empty());
        assert_eq!(
            book.lookup_input_action("input", "client", "op-0"),
            Ok(first.unwrap())
        );
        assert!(book.validate());
    }

    #[test]
    fn uncertain_reservation_never_moves_to_terminal_receipts() {
        let mut book = InputAuthority::default();
        let active = binding();
        let token = book.issue_input_binding(&active).unwrap();
        let ReservationResult::Admitted(reservation) = book
            .reserve_input_action("input", "client", "op", &token, "display", &active)
            .unwrap()
        else {
            panic!()
        };
        let uncertain = book
            .finish_input_action(
                "input",
                "client",
                "op",
                &reservation.reservation_id,
                Outcome::RecoveryRequired,
            )
            .unwrap();
        assert_eq!(
            book.finish_input_action(
                "input",
                "client",
                "op",
                &reservation.reservation_id,
                Outcome::RecoveryRequired
            ),
            Ok(uncertain.clone())
        );
        let mut book: InputAuthority =
            serde_json::from_slice(&serde_json::to_vec(&book).unwrap()).unwrap();
        assert_eq!(
            book.finish_input_action(
                "input",
                "client",
                "op",
                &reservation.reservation_id,
                Outcome::RecoveryRequired
            ),
            Ok(uncertain)
        );
        assert_eq!(
            book.finish_input_action(
                "input",
                "client",
                "op",
                &reservation.reservation_id,
                Outcome::Cancelled
            ),
            Err(Error::UnknownOutcome)
        );
        book.fence(&active);
        assert!(book.has_any_session_reference("session"));
        assert!(book.terminal_receipts.is_empty());
        assert_eq!(
            book.finish_input_action(
                "input",
                "client",
                "op",
                &reservation.reservation_id,
                Outcome::Completed
            ),
            Err(Error::UnknownOutcome)
        );
    }

    #[test]
    fn terminal_reservation_remains_a_session_reference() {
        let mut authority = InputAuthority::default();
        let active = binding();
        let token = authority.issue_input_binding(&active).unwrap();
        let target = ActionTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            expected_generation: 1,
        };
        let ReservationResult::Admitted(reservation) = authority
            .reserve_input_action_for_target(
                "input", "client", "op", &token, "display", &active, &target,
            )
            .unwrap()
        else {
            panic!()
        };
        authority
            .reservations
            .values_mut()
            .find(|item| item.reservation_id == reservation.reservation_id)
            .unwrap()
            .outcome = Outcome::Completed;
        authority.fence(&active);
        assert!(authority.has_any_session_reference("session"));
        assert!(!authority.has_unresolved_session("host", "session"));
        assert!(!authority.has_any_session_reference("other"));
    }

    #[test]
    fn target_is_durable_exact_and_cannot_change_on_retry() {
        let mut authority = InputAuthority::default();
        let active = binding();
        let token = authority.issue_input_binding(&active).unwrap();
        let stream = ActionTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            expected_generation: 1,
        };
        let session = ActionTarget::Session {
            host_epoch: "host".into(),
            session_id: "session".into(),
            expected_generation: 7,
        };
        let ReservationResult::Admitted(reservation) = authority
            .reserve_input_action_for_target(
                "input", "client", "op", &token, "display", &active, &stream,
            )
            .unwrap()
        else {
            panic!()
        };
        let mut restored: InputAuthority =
            serde_json::from_slice(&serde_json::to_vec(&authority).unwrap()).unwrap();
        assert_eq!(
            restored
                .resolve_reservation(&reservation.reservation_id)
                .unwrap()
                .target,
            Some(stream.clone())
        );
        assert!(matches!(
            restored.reserve_input_action_for_target(
                "input", "client", "op", &token, "display", &active, &stream
            ),
            Ok(ReservationResult::Existing(_))
        ));
        assert_eq!(
            restored.reserve_input_action_for_target(
                "input", "client", "op", &token, "display", &active, &session
            ),
            Err(Error::Conflict)
        );
        let wrong = ActionTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "other".into(),
            expected_generation: 1,
        };
        assert_eq!(
            restored.reserve_input_action_for_target(
                "input", "client", "other", &token, "display", &active, &wrong
            ),
            Err(Error::Invalid)
        );
        assert!(matches!(
            restored.reserve_input_action_for_target(
                "input",
                "client",
                "session-op",
                &token,
                "display",
                &active,
                &session
            ),
            Ok(ReservationResult::Admitted(_))
        ));
    }

    #[test]
    fn checkpoint_exempts_only_the_exact_transferred_running_input() {
        let mut authority = InputAuthority::default();
        let active = binding();
        let token = authority.issue_input_binding(&active).unwrap();
        let target = ActionTarget::Stream {
            host_epoch: active.host_epoch.clone(),
            session_id: active.session_id.clone(),
            stream_id: active.stream_id.clone(),
            expected_generation: active.generation,
        };
        let action = crate::action_authority::ActionBinding {
            operation_id: "op".into(),
            action_id: "display".into(),
            target: target.clone(),
            plan_digest: "a".repeat(64),
            source: crate::action_authority::Principal {
                caller: "transport".into(),
                initiator: "client".into(),
            },
        };
        let ReservationResult::Admitted(reservation) = authority
            .reserve_input_action_for_target(
                "input", "client", "op", &token, "display", &active, &target,
            )
            .unwrap()
        else {
            panic!()
        };
        authority
            .transfer_reservation(&reservation.reservation_id, "actions", "op", "display")
            .unwrap();
        authority
            .fence_except(&active, &reservation.reservation_id, "actions")
            .unwrap();
        let allowed = |authority: &InputAuthority,
                       id: &str,
                       service: &str,
                       binding: &crate::action_authority::ActionBinding| {
            authority.checkpoint_allows_transferred_action(
                "host", "session", "stream", 1, id, service, binding,
            )
        };
        assert!(allowed(
            &authority,
            &reservation.reservation_id,
            "actions",
            &action
        ));
        assert!(!allowed(
            &authority,
            &reservation.reservation_id,
            "foreign",
            &action
        ));
        let mut wrong = action.clone();
        wrong.operation_id = "other".into();
        assert!(!allowed(
            &authority,
            &reservation.reservation_id,
            "actions",
            &wrong
        ));
        wrong = action.clone();
        wrong.target = ActionTarget::Session {
            host_epoch: "host".into(),
            session_id: "session".into(),
            expected_generation: 1,
        };
        assert!(!allowed(
            &authority,
            &reservation.reservation_id,
            "actions",
            &wrong
        ));
        assert!(!allowed(&authority, "other", "actions", &action));
        let mut concurrent = authority.clone();
        let second_token = concurrent.issue_input_binding(&active).unwrap();
        concurrent
            .reserve_input_action_for_target(
                "input",
                "client",
                "second",
                &second_token,
                "display",
                &active,
                &target,
            )
            .unwrap();
        assert!(!allowed(
            &concurrent,
            &reservation.reservation_id,
            "actions",
            &action
        ));
        authority.recover_after_restart();
        assert!(!allowed(
            &authority,
            &reservation.reservation_id,
            "actions",
            &action
        ));
    }

    #[test]
    fn transferred_cancellation_is_owner_bound_and_idempotent() {
        let mut authority = InputAuthority::default();
        let active = binding();
        let token = authority.issue_input_binding(&active).unwrap();
        let ReservationResult::Admitted(reservation) = authority
            .reserve_input_action("input", "client", "op", &token, "display", &active)
            .unwrap()
        else {
            panic!()
        };
        assert!(
            authority
                .cancel_transferred_reservation(
                    &reservation.reservation_id,
                    "actions",
                    "op",
                    "display"
                )
                .is_err()
        );
        authority
            .transfer_reservation(&reservation.reservation_id, "actions", "op", "display")
            .unwrap();
        assert!(
            authority
                .cancel_transferred_reservation(
                    &reservation.reservation_id,
                    "other",
                    "op",
                    "display"
                )
                .is_err()
        );
        assert!(
            authority
                .cancel_transferred_reservation(
                    &reservation.reservation_id,
                    "actions",
                    "op",
                    "other"
                )
                .is_err()
        );
        authority
            .cancel_transferred_reservation(&reservation.reservation_id, "actions", "op", "display")
            .unwrap();
        authority
            .cancel_transferred_reservation(&reservation.reservation_id, "actions", "op", "display")
            .unwrap();
        assert_eq!(
            authority
                .resolve_reservation(&reservation.reservation_id)
                .unwrap()
                .outcome,
            Outcome::Cancelled
        );
    }

    #[test]
    fn only_exact_transferred_input_reservation_is_exempted() {
        let mut authority = InputAuthority::default();
        let active = binding();
        let token = authority.issue_input_binding(&active).unwrap();
        let ReservationResult::Admitted(first) = authority
            .reserve_input_action("input", "client", "one", &token, "display", &active)
            .unwrap()
        else {
            panic!()
        };
        let ReservationResult::Admitted(second) = authority
            .reserve_input_action("input", "client", "two", &token, "display", &active)
            .unwrap()
        else {
            panic!()
        };
        assert!(
            authority
                .fence_except(&active, &first.reservation_id, "actions")
                .is_err()
        );
        assert!(
            authority
                .transfer_reservation(&first.reservation_id, "actions", "wrong", "display")
                .is_err()
        );
        authority
            .transfer_reservation(&first.reservation_id, "actions", "one", "display")
            .unwrap();
        assert_eq!(
            authority.fence_except(&active, &first.reservation_id, "actions"),
            Ok(1)
        );
        assert!(
            authority
                .validate_input_binding("input", &token, &active)
                .is_err()
        );
        authority
            .finish_input_action(
                "input",
                "client",
                "two",
                &second.reservation_id,
                Outcome::Completed,
            )
            .unwrap();
        assert_eq!(
            authority.fence_except(&active, &first.reservation_id, "actions"),
            Ok(0)
        );
        assert!(
            authority
                .fence_except(&active, &first.reservation_id, "other")
                .is_err()
        );
    }

    #[test]
    fn active_binding_and_inflight_fence_survive_restart() {
        let mut book = InputAuthority::default();
        let active = binding();
        let token = book.issue_input_binding(&active).unwrap();
        assert_eq!(
            book.validate_input_binding("foreign", &token, &active),
            Err(Error::Unauthorized)
        );
        let mut stale = active.clone();
        stale.geometry_revision += 1;
        assert_eq!(
            book.validate_input_binding("input", &token, &stale),
            Err(Error::Unauthorized)
        );
        let admitted = book
            .reserve_input_action("input", "client", "op", &token, "display", &active)
            .unwrap();
        let ReservationResult::Admitted(reservation) = admitted else {
            panic!()
        };
        let rendered = format!("{reservation:?}");
        assert!(!rendered.contains(&reservation.token));
        assert!(!rendered.contains(&reservation.reservation_id));
        assert!(matches!(
            book.reserve_input_action("input", "client", "op", &token, "display", &active),
            Ok(ReservationResult::Existing(_))
        ));
        assert_eq!(book.fence(&active), 1);
        assert_eq!(
            book.reserve_input_action("input", "client", "op2", &token, "display", &active),
            Err(Error::Unauthorized)
        );
        let mut restored: InputAuthority =
            serde_json::from_slice(&serde_json::to_vec(&book).unwrap()).unwrap();
        assert!(restored.validate());
        restored.recover_after_restart();
        assert_eq!(restored.fence(&active), 1);
        assert_eq!(
            restored.finish_input_action(
                "input",
                "client",
                "op",
                &reservation.reservation_id,
                Outcome::Completed
            ),
            Err(Error::UnknownOutcome)
        );
        assert_eq!(
            restored
                .lookup_input_action("input", "client", "op")
                .unwrap()
                .outcome,
            Outcome::RecoveryRequired
        );
    }
    #[test]
    fn finish_and_duplicate_conflict_do_not_reexecute() {
        let mut book = InputAuthority::default();
        let active = binding();
        let token = book.issue_input_binding(&active).unwrap();
        let ReservationResult::Admitted(r) = book
            .reserve_input_action("input", "client", "op", &token, "display", &active)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            book.reserve_input_action("input", "client", "op", &token, "different", &active),
            Err(Error::Conflict)
        );
        book.finish_input_action(
            "input",
            "client",
            "op",
            &r.reservation_id,
            Outcome::Completed,
        )
        .unwrap();
        assert_eq!(book.fence(&active), 0);
        assert_eq!(
            book.reserve_input_action("input", "client", "op", &token, "display", &active),
            Err(Error::Conflict)
        );
    }

    #[test]
    fn binding_without_actions_allows_input_but_no_action_reservation() {
        let mut book = InputAuthority::default();
        let mut active = binding();
        active.allowed_actions.clear();
        let token = book.issue_input_binding(&active).unwrap();
        assert_eq!(
            book.validate_input_binding("input", &token, &active),
            Ok(active.clone())
        );
        assert_eq!(
            book.reserve_input_action("input", "client", "op", &token, "display", &active),
            Err(Error::Unauthorized)
        );
        assert!(book.validate());
    }
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    use amaterasu_extension_api::LifecycleTarget;
    use amaterasu_module_sdk::input::INPUT_BINDING_RETIREMENT_VERSION;

    fn receipt(index: usize) -> InputBindingRetirement {
        InputBindingRetirement {
            version: INPUT_BINDING_RETIREMENT_VERSION,
            token: format!("binding-{index}"),
            target: LifecycleTarget::Stream {
                host_epoch: "host".into(),
                session_id: "session".into(),
                stream_id: format!("stream-{index}"),
                generation: 1,
            },
            revoke_operation_id: format!("revoke-{index}"),
            input_boot_epoch: "provider-boot".into(),
        }
    }

    #[test]
    fn native_retirement_metadata_is_bounded_and_never_evicted_on_restart() {
        let mut authority = InputAuthority::default();
        assert!(
            serde_json::to_value(&authority)
                .unwrap()
                .get("retirement_receipts")
                .is_none()
        );
        for index in 0..MAX_RETAINED_INPUT_BINDINGS {
            authority.record_retirement(receipt(index)).unwrap();
        }
        assert_eq!(
            authority.record_retirement(receipt(MAX_RETAINED_INPUT_BINDINGS)),
            Err(Error::Capacity)
        );
        authority.recover_after_restart();
        assert_eq!(
            authority.pending_retirements().count(),
            MAX_RETAINED_INPUT_BINDINGS
        );
        assert!(authority.validate());
        let mut wrong = receipt(0);
        wrong.input_boot_epoch = "another-provider".into();
        assert_eq!(
            authority.acknowledge_retirement(&wrong),
            Err(Error::Conflict)
        );
        authority.acknowledge_retirement(&receipt(0)).unwrap();
        authority
            .record_retirement(receipt(MAX_RETAINED_INPUT_BINDINGS))
            .unwrap();
        assert_eq!(
            authority.pending_retirements().count(),
            MAX_RETAINED_INPUT_BINDINGS
        );
    }
}
