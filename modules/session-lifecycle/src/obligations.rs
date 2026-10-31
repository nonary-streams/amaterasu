//! Durable cleanup obligations for effects admitted by the lifecycle authority.
//!
//! The coordinator must persist a newly recorded obligation before dispatching its
//! effect, and persist every subsequent outcome before treating it as authoritative.
//! This type only models the ledger; it does not perform or certify native effects.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const MAX_OBLIGATIONS: usize = 256;
const MAX_ID_BYTES: usize = 256;

/// Whether native references held by the running session have been retired.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRetirement {
    Pending,
    Unknown,
    Retired,
}

/// Restoration can remain outstanding after runtime references are retired.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Restoration {
    NotRequired,
    Pending,
    Deferred,
    Unknown,
    Restored,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Obligation {
    pub id: String,
    pub provider: String,
    pub transition: String,
    pub resource: String,
    pub domain: String,
    /// Resources this effect depends on; these must outlive this obligation.
    pub dependencies: Vec<String>,
    pub runtime: RuntimeRetirement,
    pub restoration: Restoration,
}

impl Obligation {
    pub fn new(
        id: String,
        provider: String,
        transition: String,
        resource: String,
        domain: String,
        dependencies: Vec<String>,
        restoration_required: bool,
    ) -> Self {
        Self {
            id,
            provider,
            transition,
            resource,
            domain,
            dependencies,
            runtime: RuntimeRetirement::Pending,
            restoration: if restoration_required {
                Restoration::Pending
            } else {
                Restoration::NotRequired
            },
        }
    }
}

/// Serialized in admission order. Dependencies must refer to earlier records,
/// making the graph acyclic and reverse admission order deterministic.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Obligations {
    entries: Vec<Obligation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LedgerError {
    Capacity,
    InvalidIdentity,
    Duplicate,
    InvalidDependency,
    InvalidInitialState,
    NotFound,
    InvalidTransition,
}

impl Obligations {
    /// Validate persisted data on recovery before using it to authorize effects.
    pub fn validate(&self) -> Result<(), LedgerError> {
        let mut seen = HashSet::new();
        if self.entries.len() > MAX_OBLIGATIONS {
            return Err(LedgerError::Capacity);
        }
        for entry in &self.entries {
            validate_identity(entry)?;
            if seen.contains(entry.id.as_str()) {
                return Err(LedgerError::Duplicate);
            }
            let mut dependencies = HashSet::new();
            for dependency in &entry.dependencies {
                if !seen.contains(dependency.as_str()) || !dependencies.insert(dependency) {
                    return Err(LedgerError::InvalidDependency);
                }
            }
            if entry.runtime != RuntimeRetirement::Retired
                && matches!(
                    entry.restoration,
                    Restoration::Deferred | Restoration::Restored
                )
            {
                return Err(LedgerError::InvalidTransition);
            }
            seen.insert(entry.id.as_str());
        }
        for entry in &self.entries {
            if entry.runtime != RuntimeRetirement::Retired
                && entry.dependencies.iter().any(|dependency| {
                    self.entries.iter().any(|parent| {
                        parent.id == *dependency && parent.runtime == RuntimeRetirement::Retired
                    })
                })
            {
                return Err(LedgerError::InvalidTransition);
            }
        }
        Ok(())
    }

    /// Add an obligation before dispatching the effect; callers must persist it.
    pub fn record(&mut self, entry: Obligation) -> Result<(), LedgerError> {
        self.validate()?;
        if self.entries.len() == MAX_OBLIGATIONS {
            return Err(LedgerError::Capacity);
        }
        if entry.runtime != RuntimeRetirement::Pending
            || !matches!(
                entry.restoration,
                Restoration::Pending | Restoration::NotRequired
            )
        {
            return Err(LedgerError::InvalidInitialState);
        }
        self.entries.push(entry);
        if let Err(error) = self.validate() {
            self.entries.pop();
            return Err(error);
        }
        Ok(())
    }

    pub fn entries(&self) -> &[Obligation] {
        &self.entries
    }

    /// Return every independently ready runtime cleanup in reverse admission order.
    /// An unknown dependent remains a blocker, but does not block unrelated work.
    /// An unknown candidate requires reconciliation, not blind effect replay.
    pub fn cleanup_ready(&self) -> Result<Vec<&Obligation>, LedgerError> {
        self.validate()?;
        Ok(self
            .entries
            .iter()
            .rev()
            .filter(|candidate| {
                candidate.runtime != RuntimeRetirement::Retired
                    && !self.entries.iter().any(|other| {
                        other.runtime != RuntimeRetirement::Retired
                            && other.dependencies.contains(&candidate.id)
                    })
            })
            .collect())
    }

    /// Record an observed runtime outcome. Retired resources never reactivate.
    pub fn set_runtime(&mut self, id: &str, state: RuntimeRetirement) -> Result<(), LedgerError> {
        self.validate()?;
        if state == RuntimeRetirement::Retired
            && self.entries.iter().any(|other| {
                other.runtime != RuntimeRetirement::Retired
                    && other.dependencies.iter().any(|dependency| dependency == id)
            })
        {
            return Err(LedgerError::InvalidTransition);
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or(LedgerError::NotFound)?;
        if entry.runtime == RuntimeRetirement::Retired && state != RuntimeRetirement::Retired {
            return Err(LedgerError::InvalidTransition);
        }
        entry.runtime = state;
        Ok(())
    }

    /// Record a restoration outcome only after runtime retirement is confirmed.
    pub fn set_restoration(&mut self, id: &str, state: Restoration) -> Result<(), LedgerError> {
        self.validate()?;
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or(LedgerError::NotFound)?;
        if entry.runtime != RuntimeRetirement::Retired
            || entry.restoration == Restoration::NotRequired
            || state == Restoration::NotRequired
            || (entry.restoration == Restoration::Restored && state != Restoration::Restored)
        {
            return Err(LedgerError::InvalidTransition);
        }
        entry.restoration = state;
        Ok(())
    }

    pub fn complete(&self) -> Result<bool, LedgerError> {
        self.validate()?;
        Ok(self.entries.iter().all(|entry| {
            entry.runtime == RuntimeRetirement::Retired
                && matches!(
                    entry.restoration,
                    Restoration::NotRequired | Restoration::Restored
                )
        }))
    }
}

fn validate_identity(entry: &Obligation) -> Result<(), LedgerError> {
    for value in [
        &entry.id,
        &entry.provider,
        &entry.transition,
        &entry.resource,
        &entry.domain,
    ] {
        if value.is_empty() || value.len() > MAX_ID_BYTES || value.chars().any(char::is_control) {
            return Err(LedgerError::InvalidIdentity);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obligation(id: &str, dependencies: &[&str], restore: bool) -> Obligation {
        Obligation::new(
            id.into(),
            "provider".into(),
            "transition".into(),
            id.into(),
            "domain".into(),
            dependencies.iter().map(|id| (*id).into()).collect(),
            restore,
        )
    }

    #[test]
    fn reverse_dependencies_allow_independent_cleanup_when_one_outcome_is_unknown() {
        let mut ledger = Obligations::default();
        ledger.record(obligation("root", &[], true)).unwrap();
        ledger
            .record(obligation("child", &["root"], false))
            .unwrap();
        ledger.record(obligation("unrelated", &[], false)).unwrap();
        ledger
            .set_runtime("child", RuntimeRetirement::Unknown)
            .unwrap();
        assert_eq!(
            ledger.set_runtime("root", RuntimeRetirement::Retired),
            Err(LedgerError::InvalidTransition)
        );
        assert_eq!(
            ledger
                .cleanup_ready()
                .unwrap()
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["unrelated", "child"]
        );
        ledger
            .set_runtime("unrelated", RuntimeRetirement::Retired)
            .unwrap();
        ledger
            .set_runtime("child", RuntimeRetirement::Retired)
            .unwrap();
        assert_eq!(ledger.cleanup_ready().unwrap()[0].id, "root");
    }

    #[test]
    fn retirement_and_deferred_restoration_survive_roundtrip_without_false_completion() {
        let mut ledger = Obligations::default();
        ledger.record(obligation("display", &[], true)).unwrap();
        assert_eq!(
            ledger.set_restoration("display", Restoration::Restored),
            Err(LedgerError::InvalidTransition)
        );
        ledger
            .set_runtime("display", RuntimeRetirement::Retired)
            .unwrap();
        ledger
            .set_restoration("display", Restoration::Deferred)
            .unwrap();
        let encoded = serde_json::to_string(&ledger).unwrap();
        let mut recovered: Obligations = serde_json::from_str(&encoded).unwrap();
        assert!(!recovered.complete().unwrap());
        recovered
            .set_restoration("display", Restoration::Unknown)
            .unwrap();
        assert!(!recovered.complete().unwrap());
        recovered
            .set_restoration("display", Restoration::Restored)
            .unwrap();
        assert!(recovered.complete().unwrap());
        assert_eq!(
            recovered.set_runtime("display", RuntimeRetirement::Pending),
            Err(LedgerError::InvalidTransition)
        );
    }

    #[test]
    fn rejects_bad_dependencies_and_corrupt_recovery() {
        let mut ledger = Obligations::default();
        assert_eq!(
            ledger.record(obligation("child", &["missing"], false)),
            Err(LedgerError::InvalidDependency)
        );
        assert!(ledger.entries().is_empty());
        ledger.record(obligation("root", &[], false)).unwrap();
        assert_eq!(
            ledger.record(obligation("child", &["root", "root"], false)),
            Err(LedgerError::InvalidDependency)
        );
        assert_eq!(
            ledger.record(obligation("root", &[], false)),
            Err(LedgerError::Duplicate)
        );
        let mut corrupt = ledger.clone();
        corrupt.entries[0].dependencies.push("root".into());
        assert_eq!(corrupt.complete(), Err(LedgerError::InvalidDependency));
    }
}
