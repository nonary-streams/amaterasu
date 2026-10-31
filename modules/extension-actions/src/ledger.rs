//! Bounded, caller-bound action receipts. This ledger never executes effects.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

const MAX_EPOCHS: usize = 64;
const WINDOW: u64 = 64;
const MAX_VALUE_BYTES: usize = 12_000;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Principal {
    pub caller: String,
    pub initiator: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Running,
    Succeeded,
    Rejected,
    Cancelled,
    FailedClean,
    RecoveryRequired,
}

impl State {
    fn terminal(&self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }

    fn may_retire(&self) -> bool {
        self.terminal() && *self != Self::RecoveryRequired
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Receipt {
    pub epoch: String,
    pub sequence: u64,
    pub state: State,
    pub cancellation_requested: bool,
    pub result: Option<Value>,
}

struct Entry {
    request: Value,
    receipt: Receipt,
}

struct Epoch {
    principal: Principal,
    closed: bool,
    retired: u64,
    entries: BTreeMap<u64, Entry>,
}

#[derive(Default)]
pub struct Ledger {
    epochs: HashMap<String, Epoch>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Capacity,
    Entropy,
    UnknownEpoch,
    Closed,
    Expired,
    SequenceWindow,
    Conflict,
    NotFound,
    InvalidTransition,
    TooLarge,
    InvalidOperationId,
}

impl Ledger {
    pub fn open(&mut self, principal: Principal) -> Result<String, Error> {
        if self.epochs.len() >= MAX_EPOCHS {
            return Err(Error::Capacity);
        }
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| Error::Entropy)?;
        let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        if self.epochs.contains_key(&id) {
            return Err(Error::Entropy);
        }
        self.epochs.insert(
            id.clone(),
            Epoch {
                principal,
                closed: false,
                retired: 0,
                entries: BTreeMap::new(),
            },
        );
        Ok(id)
    }

    fn epoch(&self, principal: &Principal, id: &str) -> Result<&Epoch, Error> {
        self.epochs
            .get(id)
            .filter(|epoch| &epoch.principal == principal)
            .ok_or(Error::UnknownEpoch)
    }

    fn epoch_mut(&mut self, principal: &Principal, id: &str) -> Result<&mut Epoch, Error> {
        self.epochs
            .get_mut(id)
            .filter(|epoch| &epoch.principal == principal)
            .ok_or(Error::UnknownEpoch)
    }

    /// Duplicate requests are recognized before the closed check so callers may
    /// recover a previously admitted receipt even after closing an epoch.
    pub fn admit(
        &mut self,
        principal: &Principal,
        id: &str,
        sequence: u64,
        request: Value,
    ) -> Result<Receipt, Error> {
        let expected_id = format!("{id}:{sequence}");
        if request.get("operation_id").and_then(Value::as_str) != Some(expected_id.as_str()) {
            return Err(Error::InvalidOperationId);
        }
        if serde_json::to_vec(&request).map_or(true, |bytes| bytes.len() > MAX_VALUE_BYTES) {
            return Err(Error::TooLarge);
        }
        let epoch = self.epoch_mut(principal, id)?;
        if sequence <= epoch.retired {
            return Err(Error::Expired);
        }
        if let Some(entry) = epoch.entries.get(&sequence) {
            return if entry.request == request {
                Ok(entry.receipt.clone())
            } else {
                Err(Error::Conflict)
            };
        }
        if epoch.closed {
            return Err(Error::Closed);
        }
        // Reclaim only the contiguous terminal prefix. Missing or live receipts
        // prevent advancing the watermark and therefore cannot be replayed.
        while sequence > epoch.retired.saturating_add(WINDOW) {
            let next = epoch.retired + 1;
            if !epoch
                .entries
                .get(&next)
                .is_some_and(|entry| entry.receipt.state.may_retire())
            {
                return Err(Error::SequenceWindow);
            }
            epoch.entries.remove(&next);
            epoch.retired = next;
        }
        if epoch.entries.len() >= WINDOW as usize {
            return Err(Error::Capacity);
        }
        let receipt = Receipt {
            epoch: id.to_owned(),
            sequence,
            state: State::Queued,
            cancellation_requested: false,
            result: None,
        };
        epoch.entries.insert(
            sequence,
            Entry {
                request,
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    pub fn status(&self, principal: &Principal, id: &str, sequence: u64) -> Result<Receipt, Error> {
        let epoch = self.epoch(principal, id)?;
        if sequence <= epoch.retired {
            return Err(Error::Expired);
        }
        epoch
            .entries
            .get(&sequence)
            .map(|entry| entry.receipt.clone())
            .ok_or(Error::NotFound)
    }

    pub fn close(&mut self, principal: &Principal, id: &str) -> Result<(), Error> {
        self.epoch_mut(principal, id)?.closed = true;
        Ok(())
    }

    /// Release a closed epoch after all effects are known. A later lookup of
    /// this random identifier is unknown and can never admit a fresh effect.
    pub fn retire_closed(&mut self, principal: &Principal, id: &str) -> Result<(), Error> {
        let epoch = self.epoch(principal, id)?;
        if !epoch.closed
            || epoch
                .entries
                .values()
                .any(|entry| !entry.receipt.state.may_retire())
        {
            return Err(Error::InvalidTransition);
        }
        self.epochs.remove(id);
        Ok(())
    }

    pub fn cancel(
        &mut self,
        principal: &Principal,
        id: &str,
        sequence: u64,
    ) -> Result<Receipt, Error> {
        let entry = self.entry_mut(principal, id, sequence)?;
        if !entry.receipt.state.terminal() {
            entry.receipt.cancellation_requested = true;
            if entry.receipt.state == State::Queued {
                entry.receipt.state = State::Cancelled;
            }
        }
        Ok(entry.receipt.clone())
    }

    pub fn transition(
        &mut self,
        principal: &Principal,
        id: &str,
        sequence: u64,
        state: State,
        result: Option<Value>,
    ) -> Result<Receipt, Error> {
        if result.as_ref().is_some_and(|value| {
            serde_json::to_vec(value).map_or(true, |bytes| bytes.len() > MAX_VALUE_BYTES)
        }) {
            return Err(Error::TooLarge);
        }
        let entry = self.entry_mut(principal, id, sequence)?;
        let valid = matches!(
            (&entry.receipt.state, &state),
            (
                State::Queued,
                State::Running | State::Rejected | State::Cancelled
            ) | (
                State::Running,
                State::Succeeded | State::FailedClean | State::RecoveryRequired
            )
        );
        if !valid {
            return Err(Error::InvalidTransition);
        }
        entry.receipt.state = state;
        entry.receipt.result = result;
        Ok(entry.receipt.clone())
    }

    /// Reconciliation may resolve an uncertain effect only after its provider
    /// confirms the outcome; a timeout or broker cancellation is insufficient.
    pub fn reconcile(
        &mut self,
        principal: &Principal,
        id: &str,
        sequence: u64,
        state: State,
    ) -> Result<Receipt, Error> {
        if !matches!(state, State::Succeeded | State::FailedClean) {
            return Err(Error::InvalidTransition);
        }
        let entry = self.entry_mut(principal, id, sequence)?;
        if entry.receipt.state != State::RecoveryRequired {
            return Err(Error::InvalidTransition);
        }
        entry.receipt.state = state;
        Ok(entry.receipt.clone())
    }

    fn entry_mut(
        &mut self,
        principal: &Principal,
        id: &str,
        sequence: u64,
    ) -> Result<&mut Entry, Error> {
        let epoch = self.epoch_mut(principal, id)?;
        if sequence <= epoch.retired {
            return Err(Error::Expired);
        }
        epoch.entries.get_mut(&sequence).ok_or(Error::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn principal() -> Principal {
        Principal {
            caller: "input".into(),
            initiator: "transport".into(),
        }
    }

    fn request(epoch: &str, sequence: u64, value: Value) -> Value {
        let mut object = value.as_object().unwrap().clone();
        object.insert("operation_id".into(), json!(format!("{epoch}:{sequence}")));
        Value::Object(object)
    }

    #[test]
    fn duplicate_conflict_and_authority_are_fenced() {
        let mut ledger = Ledger::default();
        let owner = principal();
        let epoch = ledger.open(owner.clone()).unwrap();
        let original = ledger
            .admit(
                &owner,
                &epoch,
                1,
                request(&epoch, 1, json!({"action_id":"a/b"})),
            )
            .unwrap();
        assert_eq!(
            ledger
                .admit(
                    &owner,
                    &epoch,
                    1,
                    request(&epoch, 1, json!({"action_id":"a/b"}))
                )
                .unwrap(),
            original
        );
        assert_eq!(
            ledger.admit(
                &owner,
                &epoch,
                1,
                request(&epoch, 1, json!({"action_id":"a/c"}))
            ),
            Err(Error::Conflict)
        );
        let impostor = Principal {
            caller: owner.caller.clone(),
            initiator: "other".into(),
        };
        assert_eq!(
            ledger.status(&impostor, &epoch, 1),
            Err(Error::UnknownEpoch)
        );
    }

    #[test]
    fn retired_watermark_cannot_skip_missing_or_running_entries() {
        let mut ledger = Ledger::default();
        let owner = principal();
        let epoch = ledger.open(owner.clone()).unwrap();
        ledger
            .admit(&owner, &epoch, 2, request(&epoch, 2, json!({})))
            .unwrap();
        assert_eq!(
            ledger.admit(&owner, &epoch, 65, request(&epoch, 65, json!({}))),
            Err(Error::SequenceWindow)
        );
        ledger
            .admit(&owner, &epoch, 1, request(&epoch, 1, json!({})))
            .unwrap();
        assert_eq!(
            ledger.admit(&owner, &epoch, 65, request(&epoch, 65, json!({}))),
            Err(Error::SequenceWindow)
        );
        ledger.cancel(&owner, &epoch, 1).unwrap();
        ledger
            .admit(&owner, &epoch, 65, request(&epoch, 65, json!({})))
            .unwrap();
        assert_eq!(
            ledger.admit(&owner, &epoch, 1, request(&epoch, 1, json!({}))),
            Err(Error::Expired)
        );
        assert_eq!(
            ledger.admit(&owner, &epoch, 66, request(&epoch, 66, json!({}))),
            Err(Error::SequenceWindow)
        );
    }

    #[test]
    fn cancellation_never_asserts_running_effect_was_reverted() {
        let mut ledger = Ledger::default();
        let owner = principal();
        let epoch = ledger.open(owner.clone()).unwrap();
        ledger
            .admit(&owner, &epoch, 1, request(&epoch, 1, json!({})))
            .unwrap();
        ledger
            .transition(&owner, &epoch, 1, State::Running, None)
            .unwrap();
        let pending = ledger.cancel(&owner, &epoch, 1).unwrap();
        assert_eq!(pending.state, State::Running);
        assert!(pending.cancellation_requested);
        assert_eq!(
            ledger.transition(&owner, &epoch, 1, State::Cancelled, None),
            Err(Error::InvalidTransition)
        );
        ledger
            .transition(&owner, &epoch, 1, State::RecoveryRequired, None)
            .unwrap();
    }

    #[test]
    fn close_rejects_new_admission_but_keeps_existing_receipt() {
        let mut ledger = Ledger::default();
        let owner = principal();
        let epoch = ledger.open(owner.clone()).unwrap();
        let receipt = ledger
            .admit(&owner, &epoch, 1, request(&epoch, 1, json!({})))
            .unwrap();
        ledger.close(&owner, &epoch).unwrap();
        assert_eq!(
            ledger
                .admit(&owner, &epoch, 1, request(&epoch, 1, json!({})))
                .unwrap(),
            receipt
        );
        assert_eq!(
            ledger.admit(&owner, &epoch, 2, request(&epoch, 2, json!({}))),
            Err(Error::Closed)
        );
    }
    #[test]
    fn operation_ids_and_record_sizes_are_bounded() {
        let mut ledger = Ledger::default();
        let owner = principal();
        let epoch = ledger.open(owner.clone()).unwrap();
        assert_eq!(
            ledger.admit(&owner, &epoch, 1, json!({"operation_id":"wrong"})),
            Err(Error::InvalidOperationId)
        );
        assert_eq!(
            ledger.admit(
                &owner,
                &epoch,
                1,
                request(&epoch, 1, json!({"data":"x".repeat(12_001)}))
            ),
            Err(Error::TooLarge)
        );
        ledger
            .admit(&owner, &epoch, 1, request(&epoch, 1, json!({})))
            .unwrap();
        assert_eq!(
            ledger.transition(
                &owner,
                &epoch,
                1,
                State::Rejected,
                Some(json!("x".repeat(12_001)))
            ),
            Err(Error::TooLarge)
        );
        assert_eq!(
            ledger.status(&owner, &epoch, 1).unwrap().state,
            State::Queued
        );
    }

    #[test]
    fn closed_epochs_are_only_retired_when_effects_are_known() {
        let mut ledger = Ledger::default();
        let owner = principal();
        let epoch = ledger.open(owner.clone()).unwrap();
        ledger
            .admit(&owner, &epoch, 1, request(&epoch, 1, json!({})))
            .unwrap();
        ledger
            .transition(&owner, &epoch, 1, State::Running, None)
            .unwrap();
        ledger.close(&owner, &epoch).unwrap();
        assert_eq!(
            ledger.retire_closed(&owner, &epoch),
            Err(Error::InvalidTransition)
        );
        ledger
            .transition(&owner, &epoch, 1, State::RecoveryRequired, None)
            .unwrap();
        assert_eq!(
            ledger.retire_closed(&owner, &epoch),
            Err(Error::InvalidTransition)
        );
        assert_eq!(
            ledger.admit(&owner, &epoch, 65, request(&epoch, 65, json!({}))),
            Err(Error::Closed)
        );
    }

    #[test]
    fn completed_closed_epoch_releases_capacity_safely() {
        let mut ledger = Ledger::default();
        let owner = principal();
        let epoch = ledger.open(owner.clone()).unwrap();
        ledger
            .admit(&owner, &epoch, 1, request(&epoch, 1, json!({})))
            .unwrap();
        ledger.cancel(&owner, &epoch, 1).unwrap();
        ledger.close(&owner, &epoch).unwrap();
        ledger.retire_closed(&owner, &epoch).unwrap();
        assert_eq!(
            ledger.admit(&owner, &epoch, 1, request(&epoch, 1, json!({}))),
            Err(Error::UnknownEpoch)
        );
    }
}
