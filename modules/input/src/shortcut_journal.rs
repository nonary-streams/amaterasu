//! Serializable shortcut admission state. The broker adapter persists each
//! proposed revision with state CAS before publishing any controller changes.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_RECORDS: usize = 32;
const MAX_JOURNAL_BYTES: usize = 12 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Journal {
    pub epoch: String,
    pub revision: u64,
    pub next_sequence: u64,
    pub receipts: BTreeMap<u64, Record>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    /// Exact admitted request identity; never a caller-provided grant.
    pub fingerprint: Value,
    pub disposition: Value,
    pub held: Value,
    pub forwarded_events: Value,
    pub intents: Vec<DispatchIntent>,
    pub phase: Phase,
    pub result: Option<Value>,
}

/// Exact action submission and its independently persisted admission state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchIntent {
    pub operation_id: String,
    pub request: Value,
    pub phase: IntentPhase,
    pub result: Option<Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IntentPhase {
    Pending,
    Dispatching,
    Terminal,
    RecoveryRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Armed,
    Delivered,
    RecoveryRequired,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Admission {
    /// Persist this journal revision before any native delivery.
    New,
    /// A prior identity is status only: never deliver its native events again.
    Existing(Record),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JournalError {
    Invalid,
    RevisionConflict,
    SequenceConflict,
    IdentityConflict,
    Capacity,
    InvalidTransition,
}

impl Journal {
    pub fn new(epoch: String) -> Result<Self, JournalError> {
        if epoch.is_empty() || epoch.len() > 256 || epoch.chars().any(char::is_control) {
            return Err(JournalError::Invalid);
        }
        Ok(Self {
            epoch,
            revision: 0,
            next_sequence: 1,
            receipts: BTreeMap::new(),
        })
    }

    /// Validate untrusted persisted state before its identity or delivery phase is used.
    pub fn validate(&self) -> Result<(), JournalError> {
        if self.epoch.is_empty()
            || self.epoch.len() > 256
            || self.epoch.chars().any(char::is_control)
            || self.receipts.len() > MAX_RECORDS
            || self.next_sequence != self.receipts.len() as u64 + 1
            || self.revision < self.receipts.len() as u64
            || self
                .receipts
                .iter()
                .enumerate()
                .any(|(index, (sequence, record))| {
                    *sequence != index as u64 + 1
                        || (record.phase == Phase::Armed && record.result.is_some())
                        || record.intents.len() > 64
                        || record.intents.iter().enumerate().any(|(i, intent)| {
                            intent.operation_id.is_empty()
                                || intent.operation_id.len() > 256
                                || intent.request.get("operation_id").and_then(Value::as_str)
                                    != Some(intent.operation_id.as_str())
                                || record.intents[..i]
                                    .iter()
                                    .any(|previous| previous.operation_id == intent.operation_id)
                                || (matches!(
                                    intent.phase,
                                    IntentPhase::Pending | IntentPhase::Dispatching
                                ) && intent.result.is_some())
                                || (intent.phase == IntentPhase::Terminal
                                    && intent.result.is_none())
                                || (record.phase != Phase::Delivered
                                    && intent.phase != IntentPhase::Pending)
                        })
                })
            || serde_json::to_vec(self)
                .map_err(|_| JournalError::Invalid)?
                .len()
                > MAX_JOURNAL_BYTES
        {
            return Err(JournalError::Invalid);
        }
        Ok(())
    }

    pub fn lookup(&self, sequence: u64) -> Option<&Record> {
        self.receipts.get(&sequence)
    }

    /// A duplicate exact fingerprint is reported but never re-arms delivery.
    pub fn admit(
        &mut self,
        expected_revision: u64,
        sequence: u64,
        record: Record,
    ) -> Result<Admission, JournalError> {
        self.validate()?;
        if self.revision != expected_revision {
            return Err(JournalError::RevisionConflict);
        }
        if let Some(existing) = self.receipts.get(&sequence) {
            return if existing.fingerprint == record.fingerprint {
                Ok(Admission::Existing(existing.clone()))
            } else {
                Err(JournalError::IdentityConflict)
            };
        }
        if sequence != self.next_sequence {
            return Err(JournalError::SequenceConflict);
        }
        if record.phase != Phase::Armed
            || record.result.is_some()
            || record
                .intents
                .iter()
                .any(|intent| intent.phase != IntentPhase::Pending || intent.result.is_some())
        {
            return Err(JournalError::Invalid);
        }
        if self.receipts.len() >= MAX_RECORDS {
            return Err(JournalError::Capacity);
        }
        let mut proposed = self.clone();
        proposed.next_sequence = sequence.checked_add(1).ok_or(JournalError::Capacity)?;
        proposed.revision = proposed
            .revision
            .checked_add(1)
            .ok_or(JournalError::Capacity)?;
        proposed.receipts.insert(sequence, record);
        self.replace_checked(proposed)?;
        Ok(Admission::New)
    }

    pub fn finish(
        &mut self,
        expected_revision: u64,
        sequence: u64,
        phase: Phase,
        result: Option<Value>,
    ) -> Result<(), JournalError> {
        self.validate()?;
        if self.revision != expected_revision {
            return Err(JournalError::RevisionConflict);
        }
        if !matches!(phase, Phase::Delivered | Phase::RecoveryRequired) {
            return Err(JournalError::InvalidTransition);
        }
        let mut proposed = self.clone();
        let record = proposed
            .receipts
            .get_mut(&sequence)
            .ok_or(JournalError::Invalid)?;
        if record.phase != Phase::Armed {
            return Err(JournalError::InvalidTransition);
        }
        record.phase = phase;
        record.result = result;
        proposed.revision = proposed
            .revision
            .checked_add(1)
            .ok_or(JournalError::Capacity)?;
        self.replace_checked(proposed)
    }

    /// Persist each dispatch transition before sending or reporting an action.
    pub fn transition_intent(
        &mut self,
        expected_revision: u64,
        sequence: u64,
        index: usize,
        phase: IntentPhase,
        result: Option<Value>,
    ) -> Result<(), JournalError> {
        self.validate()?;
        if self.revision != expected_revision {
            return Err(JournalError::RevisionConflict);
        }
        let mut proposed = self.clone();
        let record = proposed
            .receipts
            .get_mut(&sequence)
            .ok_or(JournalError::Invalid)?;
        if record.phase != Phase::Delivered {
            return Err(JournalError::InvalidTransition);
        }
        let intent = record.intents.get_mut(index).ok_or(JournalError::Invalid)?;
        let valid = matches!(
            (intent.phase, phase),
            (IntentPhase::Pending, IntentPhase::Dispatching)
                | (
                    IntentPhase::Dispatching,
                    IntentPhase::Terminal | IntentPhase::RecoveryRequired
                )
        );
        if !valid
            || (phase == IntentPhase::Terminal && result.is_none())
            || (phase == IntentPhase::Dispatching && result.is_some())
        {
            return Err(JournalError::InvalidTransition);
        }
        intent.phase = phase;
        intent.result = result;
        proposed.revision = proposed
            .revision
            .checked_add(1)
            .ok_or(JournalError::Capacity)?;
        self.replace_checked(proposed)
    }

    /// A restart cannot establish whether a previously armed native call ran.
    /// Fence every such record; reconciliation must not replay its events.
    pub fn recover(&mut self, expected_revision: u64) -> Result<bool, JournalError> {
        self.validate()?;
        if self.revision != expected_revision {
            return Err(JournalError::RevisionConflict);
        }
        let mut proposed = self.clone();
        let mut changed = false;
        for record in proposed.receipts.values_mut() {
            if record.phase == Phase::Armed {
                record.phase = Phase::RecoveryRequired;
                changed = true;
            }
            for intent in &mut record.intents {
                if intent.phase == IntentPhase::Dispatching {
                    intent.phase = IntentPhase::RecoveryRequired;
                    changed = true;
                }
            }
        }
        if changed {
            proposed.revision = proposed
                .revision
                .checked_add(1)
                .ok_or(JournalError::Capacity)?;
            self.replace_checked(proposed)?;
        }
        Ok(changed)
    }

    fn replace_checked(&mut self, proposed: Self) -> Result<(), JournalError> {
        if serde_json::to_vec(&proposed)
            .map_err(|_| JournalError::Invalid)?
            .len()
            > MAX_JOURNAL_BYTES
        {
            return Err(JournalError::Capacity);
        }
        proposed.validate()?;
        *self = proposed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn armed(value: &str) -> Record {
        Record {
            fingerprint: json!({"request":value}),
            disposition: json!(["consume"]),
            held: json!(["key"]),
            forwarded_events: json!([]),
            intents: vec![DispatchIntent {
                operation_id: "epoch:1".into(),
                request: json!({"operation_id":"epoch:1"}),
                phase: IntentPhase::Pending,
                result: None,
            }],
            phase: Phase::Armed,
            result: None,
        }
    }

    #[test]
    fn admission_is_atomic_cas_idempotent_and_never_rearms_duplicates() {
        let mut journal = Journal::new("epoch".into()).unwrap();
        assert_eq!(journal.admit(0, 1, armed("a")), Ok(Admission::New));
        let saved = journal.clone();
        assert_eq!(
            journal.admit(0, 2, armed("b")),
            Err(JournalError::RevisionConflict)
        );
        assert_eq!(
            journal.admit(1, 1, armed("b")),
            Err(JournalError::IdentityConflict)
        );
        assert_eq!(
            journal.admit(1, 3, armed("b")),
            Err(JournalError::SequenceConflict)
        );
        assert!(matches!(
            journal.admit(1, 1, armed("a")),
            Ok(Admission::Existing(_))
        ));
        assert_eq!(journal, saved);
    }

    #[test]
    fn recovery_fences_armed_delivery_without_replay_and_preserves_receipt() {
        let mut journal = Journal::new("epoch".into()).unwrap();
        journal.admit(0, 1, armed("a")).unwrap();
        let persisted = serde_json::to_vec(&journal).unwrap();
        let mut recovered: Journal = serde_json::from_slice(&persisted).unwrap();
        assert!(recovered.recover(1).unwrap());
        assert_eq!(recovered.lookup(1).unwrap().phase, Phase::RecoveryRequired);
        assert!(matches!(
            recovered.admit(2, 1, armed("a")),
            Ok(Admission::Existing(Record {
                phase: Phase::RecoveryRequired,
                ..
            }))
        ));
        assert_eq!(
            recovered.finish(2, 1, Phase::Delivered, None),
            Err(JournalError::InvalidTransition)
        );
        assert!(!recovered.recover(2).unwrap());
    }

    #[test]
    fn rejects_corrupt_persisted_sequences_and_armed_results() {
        let mut journal = Journal::new("epoch".into()).unwrap();
        journal.admit(0, 1, armed("a")).unwrap();
        let mut corrupt = journal.clone();
        corrupt.next_sequence = 4;
        assert_eq!(corrupt.validate(), Err(JournalError::Invalid));
        corrupt = journal.clone();
        corrupt.receipts.get_mut(&1).unwrap().result = Some(json!(true));
        assert_eq!(corrupt.validate(), Err(JournalError::Invalid));
        corrupt = journal;
        corrupt.revision = 0;
        assert_eq!(corrupt.validate(), Err(JournalError::Invalid));
    }

    #[test]
    fn dispatch_requires_confirmed_delivery_and_recovery_never_resends() {
        let mut journal = Journal::new("epoch".into()).unwrap();
        journal.admit(0, 1, armed("a")).unwrap();
        assert_eq!(
            journal.transition_intent(1, 1, 0, IntentPhase::Dispatching, None),
            Err(JournalError::InvalidTransition)
        );
        journal
            .finish(1, 1, Phase::Delivered, Some(json!({"ok":true})))
            .unwrap();
        journal
            .transition_intent(2, 1, 0, IntentPhase::Dispatching, None)
            .unwrap();
        assert!(journal.recover(3).unwrap());
        assert_eq!(
            journal.lookup(1).unwrap().intents[0].phase,
            IntentPhase::RecoveryRequired
        );
        assert_eq!(
            journal.transition_intent(4, 1, 0, IntentPhase::Dispatching, None),
            Err(JournalError::InvalidTransition)
        );
    }

    #[test]
    fn capacity_and_invalid_transitions_do_not_modify_persisted_candidate() {
        let mut journal = Journal::new("epoch".into()).unwrap();
        let initial = journal.clone();
        let mut large = armed("a");
        large.intents[0].request["arguments"] = json!("x".repeat(MAX_JOURNAL_BYTES));
        assert_eq!(journal.admit(0, 1, large), Err(JournalError::Capacity));
        assert_eq!(journal, initial);
        journal.admit(0, 1, armed("a")).unwrap();
        journal
            .finish(1, 1, Phase::Delivered, Some(json!({"ok":true})))
            .unwrap();
        assert_eq!(
            journal.finish(2, 1, Phase::RecoveryRequired, None),
            Err(JournalError::InvalidTransition)
        );
    }
}
