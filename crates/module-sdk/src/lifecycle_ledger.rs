//! One admission rule for every lifecycle layer.
//!
//! A launch, a transport ticket, a stream session and a lifecycle session are
//! all *owners*: each holds resources for a while and then gives them back.
//! An owner has two independent facts.
//!
//! * [`Admission`]: whether it may still create or change live resources.
//!   Ending is a decision, it needs nobody's cooperation, and it cannot be
//!   undone.
//! * [`Retirement`]: what its cleanup still owes after it ended.
//!
//! Admission of new work reads only the first fact. The types here make that
//! a property of the code's shape rather than of its tests:
//!
//! * [`Live`] is the only view of a layer's records that an admission
//!   decision receives. It is built by this module and holds open owners
//!   only, so an ended owner cannot be counted, however its cleanup went.
//! * [`make_room`] keeps the retained records inside their bound by dropping
//!   ended owners, the most settled first. It never refuses a new owner
//!   because old ones are still recorded.
//!
//! Exclusive resources follow the same rule in each layer that has them: an
//! open holder is a real conflict, an ended holder is waited for a bounded
//! time and then displaced. NVHTTP's application slot, RTSP's media port
//! slots and the coordinator's mutation domains each apply it to their own
//! records; `amaterasu_extension_api::DOMAIN_TAKEOVER_CAPABILITY` is what a
//! provider is granted when it can hand a domain over within a host run.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Whether an owner may still create or change live resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Admission {
    Open,
    Ended,
}

/// What an ended owner's cleanup still owes. Declared from most to least
/// settled, which is the order records are dropped in when space is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retirement {
    /// Evidence shows every resource was released.
    Confirmed,
    /// A successor took over what this owner had not released.
    Superseded,
    /// Retries are exhausted or impossible. Nothing more will be attempted
    /// and the outcome stays reported as unconfirmed.
    Abandoned,
    /// Cleanup failed or its outcome is unknown, and a retry is still owed.
    Unconfirmed,
    /// Cleanup is in flight.
    Pending,
}

impl Retirement {
    /// Whether no further cleanup work will run for this owner.
    pub fn settled(self) -> bool {
        matches!(self, Self::Confirmed | Self::Superseded | Self::Abandoned)
    }
}

/// A record with the two lifecycle facts. Each layer implements this once for
/// its record type; every admission decision in that layer goes through it.
pub trait Owner {
    fn admission(&self) -> Admission;

    /// Read only for ended owners.
    fn retirement(&self) -> Retirement;
}

/// The open owners of one layer.
///
/// An admission decision takes a `Live` and no other access to the records,
/// so what it decides cannot depend on an ended owner.
pub struct Live<'a, R> {
    open: Vec<&'a R>,
}

impl<'a, R: Owner> Live<'a, R> {
    pub fn of(records: impl IntoIterator<Item = &'a R>) -> Self {
        Self {
            open: records
                .into_iter()
                .filter(|record| record.admission() == Admission::Open)
                .collect(),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &'a R> + '_ {
        self.open.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// Whether one more open owner fits under `limit`.
    pub fn admits(&self, limit: usize) -> bool {
        self.open.len() < limit
    }

    /// How many open owners satisfy `predicate`.
    pub fn count(&self, mut predicate: impl FnMut(&R) -> bool) -> usize {
        self.open.iter().filter(|record| predicate(record)).count()
    }
}

/// Make space for one more record in a bounded map.
///
/// Ended owners are dropped until the map is below `limit`: the most settled
/// first, then the oldest key. `may_drop` lets a layer keep an ended record
/// that something open still depends on. Each dropped record is handed to
/// `dropped` so the layer can settle what it retained and report the owner.
///
/// Returns whether a new record now fits. It can only be false when the map
/// is full of records that are open or pinned, which is a present-day limit.
pub fn make_room<K, R>(
    records: &mut BTreeMap<K, R>,
    limit: usize,
    mut may_drop: impl FnMut(&K, &R) -> bool,
    mut dropped: impl FnMut(K, R),
) -> bool
where
    K: Ord + Clone,
    R: Owner,
{
    while records.len() >= limit {
        let victim = records
            .iter()
            .filter(|(key, record)| record.admission() == Admission::Ended && may_drop(key, record))
            .min_by_key(|(_, record)| record.retirement())
            .map(|(key, _)| key.clone());
        let Some(key) = victim else {
            return false;
        };
        let record = records.remove(&key).expect("victim was just found");
        dropped(key, record);
    }
    true
}

#[cfg(test)]
mod tests;
