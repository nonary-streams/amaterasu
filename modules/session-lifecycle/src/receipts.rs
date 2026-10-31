//! Bounded, durable idempotency namespaces for lifecycle callers.
//!
//! Mutate a clone, durably persist it, then publish it before admitting effects.
//! Retire only results whose effects and recovery obligations are safely tracked
//! elsewhere. Closing or restart fences admission while preserving lookup.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_EPOCHS: usize = 64;
const MAX_RECEIPTS: usize = 64;
const MAX_VALUE_BYTES: usize = 4096;
const MAX_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptBook {
    epochs: BTreeMap<String, Epoch>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Epoch {
    caller: String,
    initiator: String,
    open: bool,
    retired: u64,
    latest: u64,
    receipts: BTreeMap<u64, Receipt>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    request: Value,
    response: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Admission {
    New,
    Replay(Value),
    Expired,
    Conflict,
    Closed,
    InvalidSequence,
    Capacity,
    UnknownEpoch,
    Invalid,
}

impl ReceiptBook {
    /// Check every invariant after deserializing and before trusting the book.
    pub fn validate(&self) -> bool {
        self.epochs.len() <= MAX_EPOCHS
            && self.receipt_count() <= MAX_RECEIPTS
            && self.epochs.iter().all(|(id, epoch)| {
                valid_id(id)
                    && valid_id(&epoch.caller)
                    && valid_id(&epoch.initiator)
                    && epoch.retired <= epoch.latest
                    && epoch.latest.checked_sub(epoch.retired) == Some(epoch.receipts.len() as u64)
                    && epoch
                        .receipts
                        .iter()
                        .enumerate()
                        .all(|(index, (sequence, receipt))| {
                            epoch.retired.checked_add(index as u64 + 1) == Some(*sequence)
                                && valid_value(&receipt.request)
                                && valid_value(&receipt.response)
                        })
            })
    }

    /// Reserve an unpredictable caller-scoped namespace. Persist before returning it.
    pub fn open(&mut self, caller: &str, initiator: &str) -> Result<String, &'static str> {
        if !valid_id(caller) || !valid_id(initiator) {
            return Err("invalid_identity");
        }
        if self.epochs.len() >= MAX_EPOCHS {
            return Err("epoch_capacity");
        }
        let id = amaterasu_storage::random_identifier().map_err(|_| "randomness_unavailable")?;
        if self.epochs.contains_key(&id) {
            return Err("epoch_collision");
        }
        self.epochs.insert(
            id.clone(),
            Epoch {
                caller: caller.into(),
                initiator: initiator.into(),
                open: true,
                retired: 0,
                latest: 0,
                receipts: BTreeMap::new(),
            },
        );
        Ok(id)
    }

    /// Closing prevents new admission but retains receipts and the tombstone.
    pub fn close(&mut self, caller: &str, initiator: &str, id: &str) -> bool {
        let Some(epoch) = self.owned_mut(caller, initiator, id) else {
            return false;
        };
        epoch.open = false;
        true
    }

    /// Call on recovery and persist before serving requests; old epochs cannot resume.
    pub fn close_after_restart(&mut self) {
        for epoch in self.epochs.values_mut() {
            epoch.open = false;
        }
    }

    /// Drop a closed, fully retired epoch only after its external obligations
    /// are durably resolved. Unknown epoch IDs never admit requests; a new
    /// epoch is issued with an independently random identifier.
    pub fn prune_closed_retired(&mut self, caller: &str, initiator: &str, id: &str) -> bool {
        let eligible = self.owned(caller, initiator, id).is_some_and(|epoch| {
            !epoch.open && epoch.retired == epoch.latest && epoch.receipts.is_empty()
        });
        if eligible {
            self.epochs.remove(id);
        }
        eligible
    }

    /// Classify a request without changing state. Sequences start at one and
    /// must be contiguous; this deliberately rejects all out-of-order admission.
    pub fn check(
        &self,
        caller: &str,
        initiator: &str,
        id: &str,
        sequence: u64,
        request: &Value,
    ) -> Admission {
        let Some(epoch) = self.owned(caller, initiator, id) else {
            return Admission::UnknownEpoch;
        };
        if !valid_value(request) {
            return Admission::Invalid;
        }
        if sequence == 0 {
            return Admission::InvalidSequence;
        }
        if sequence <= epoch.retired {
            return Admission::Expired;
        }
        if let Some(receipt) = epoch.receipts.get(&sequence) {
            return if &receipt.request == request {
                Admission::Replay(receipt.response.clone())
            } else {
                Admission::Conflict
            };
        }
        if !epoch.open {
            return Admission::Closed;
        }
        if epoch.latest.checked_add(1) != Some(sequence) {
            return Admission::InvalidSequence;
        }
        if self.receipt_count() >= MAX_RECEIPTS {
            return Admission::Capacity;
        }
        Admission::New
    }

    /// Insert the initial receipt in a candidate book, before performing effects.
    pub fn insert(
        &mut self,
        caller: &str,
        initiator: &str,
        id: &str,
        sequence: u64,
        request: Value,
        response: Value,
    ) -> Admission {
        let status = self.check(caller, initiator, id, sequence, &request);
        if status != Admission::New {
            return status;
        }
        if !valid_value(&response) {
            return Admission::Invalid;
        }
        let epoch = self
            .owned_mut(caller, initiator, id)
            .expect("checked owner");
        epoch.latest = sequence;
        epoch
            .receipts
            .insert(sequence, Receipt { request, response });
        Admission::New
    }

    pub fn lookup(&self, caller: &str, initiator: &str, id: &str, sequence: u64) -> Admission {
        let Some(epoch) = self.owned(caller, initiator, id) else {
            return Admission::UnknownEpoch;
        };
        if sequence == 0 {
            return Admission::InvalidSequence;
        }
        if sequence <= epoch.retired {
            return Admission::Expired;
        }
        epoch
            .receipts
            .get(&sequence)
            .map_or(Admission::InvalidSequence, |receipt| {
                Admission::Replay(receipt.response.clone())
            })
    }

    /// Replace the result as the admitted operation progresses. Persist the
    /// candidate before exposing the new result to callers.
    pub fn update(
        &mut self,
        caller: &str,
        initiator: &str,
        id: &str,
        sequence: u64,
        response: Value,
    ) -> bool {
        if !valid_value(&response) {
            return false;
        }
        let Some(receipt) = self
            .owned_mut(caller, initiator, id)
            .and_then(|epoch| epoch.receipts.get_mut(&sequence))
        else {
            return false;
        };
        receipt.response = response;
        true
    }

    /// Retire an existing contiguous prefix. The watermark prevents re-execution.
    pub fn retire_through(
        &mut self,
        caller: &str,
        initiator: &str,
        id: &str,
        sequence: u64,
    ) -> bool {
        let Some(epoch) = self.owned_mut(caller, initiator, id) else {
            return false;
        };
        if sequence < epoch.retired || sequence > epoch.latest {
            return false;
        }
        epoch.receipts.retain(|key, _| *key > sequence);
        epoch.retired = sequence;
        true
    }

    fn owned(&self, caller: &str, initiator: &str, id: &str) -> Option<&Epoch> {
        self.epochs
            .get(id)
            .filter(|epoch| epoch.caller == caller && epoch.initiator == initiator)
    }
    fn owned_mut(&mut self, caller: &str, initiator: &str, id: &str) -> Option<&mut Epoch> {
        self.epochs
            .get_mut(id)
            .filter(|epoch| epoch.caller == caller && epoch.initiator == initiator)
    }
    fn receipt_count(&self) -> usize {
        self.epochs.values().map(|epoch| epoch.receipts.len()).sum()
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES && !id.chars().any(char::is_control)
}
fn valid_value(value: &Value) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_VALUE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replay_conflict_retirement_and_restart_are_fenced() {
        let mut book = ReceiptBook::default();
        let id = book.open("caller", "initiator").unwrap();
        let request = json!({"action":"prepare"});
        let result = json!({"status":"pending"});
        assert_eq!(
            book.insert(
                "caller",
                "initiator",
                &id,
                1,
                request.clone(),
                result.clone()
            ),
            Admission::New
        );
        assert_eq!(
            book.check("caller", "initiator", &id, 1, &request),
            Admission::Replay(result)
        );
        assert_eq!(
            book.check("caller", "initiator", &id, 1, &json!({"action":"other"})),
            Admission::Conflict
        );
        assert_eq!(
            book.check("other", "initiator", &id, 1, &request),
            Admission::UnknownEpoch
        );
        assert_eq!(
            book.check("caller", "initiator", &id, 3, &request),
            Admission::InvalidSequence
        );
        assert!(book.retire_through("caller", "initiator", &id, 1));
        assert_eq!(
            book.check("caller", "initiator", &id, 1, &request),
            Admission::Expired
        );
        let bytes = serde_json::to_vec(&book).unwrap();
        let mut recovered: ReceiptBook = serde_json::from_slice(&bytes).unwrap();
        assert!(recovered.validate());
        recovered.close_after_restart();
        assert_eq!(
            recovered.check("caller", "initiator", &id, 2, &request),
            Admission::Closed
        );
        assert_eq!(
            recovered.lookup("caller", "initiator", &id, 1),
            Admission::Expired
        );
    }

    #[test]
    fn bounds_and_corrupt_recovery_fail_closed() {
        let mut book = ReceiptBook::default();
        let id = book.open("caller", "initiator").unwrap();
        for sequence in 1..=MAX_RECEIPTS as u64 {
            assert_eq!(
                book.insert(
                    "caller",
                    "initiator",
                    &id,
                    sequence,
                    json!(sequence),
                    json!(true)
                ),
                Admission::New
            );
        }
        assert_eq!(
            book.check("caller", "initiator", &id, 65, &json!(1)),
            Admission::Capacity
        );
        assert!(book.retire_through("caller", "initiator", &id, 1));
        assert_eq!(
            book.check("caller", "initiator", &id, 65, &json!(1)),
            Admission::New
        );
        let mut encoded = serde_json::to_value(book).unwrap();
        encoded["epochs"][&id]["retired"] = json!(3);
        let corrupt: ReceiptBook = serde_json::from_value(encoded).unwrap();
        assert!(!corrupt.validate());
    }

    #[test]
    fn only_closed_fully_retired_epochs_can_be_pruned() {
        let mut book = ReceiptBook::default();
        let id = book.open("caller", "initiator").unwrap();
        assert_eq!(
            book.insert("caller", "initiator", &id, 1, json!(1), json!(2)),
            Admission::New
        );
        assert!(!book.prune_closed_retired("caller", "initiator", &id));
        assert!(book.close("caller", "initiator", &id));
        assert!(!book.prune_closed_retired("caller", "initiator", &id));
        assert!(book.retire_through("caller", "initiator", &id, 1));
        assert!(book.prune_closed_retired("caller", "initiator", &id));
        assert_eq!(
            book.check("caller", "initiator", &id, 1, &json!(1)),
            Admission::UnknownEpoch
        );
        assert!(book.validate());
    }
}
