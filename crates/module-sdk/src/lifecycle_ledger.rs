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
//! * [`LeaseTable`] hands out exclusive resources. An open holder is a real
//!   conflict. An ended holder is waited for until a fixed instant and then
//!   superseded, or reported as that one unavailable resource when the
//!   resource cannot be taken over. Either way the answer is bounded.
//!
//! What a successor does with a resource it took over is the resource owner's
//! business: see `amaterasu_extension_api::ResourceIsolation` for what a
//! provider declares.
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

/// Orders the holders of one resource. A holder's token is larger than every
/// token issued for that table before it, so a resource owner that remembers
/// the largest token it has served can reject anything older.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FenceToken(pub u64);

/// Whether a successor may take a resource an ended owner never released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Takeover {
    /// The successor gets its own instance, or the resource owner rejects
    /// the old holder's later mutations.
    Allowed,
    /// The resource cannot be replaced or fenced. A successor can only wait,
    /// and is then told this resource is unavailable.
    Refused,
}

/// One holder of one resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease<O, T> {
    pub owner: O,
    pub token: FenceToken,
    /// Present once the owner ended without releasing: the instant after
    /// which a successor stops waiting for it.
    pub release_by: Option<T>,
}

impl<O, T> Lease<O, T> {
    pub fn ended(&self) -> bool {
        self.release_by.is_some()
    }
}

/// The answer to one request for a resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Acquire<O, T> {
    Granted {
        token: FenceToken,
        /// The ended holder this grant displaced, if any.
        superseded: Option<Lease<O, T>>,
    },
    /// An open owner holds the resource: a present conflict.
    Held { by: O },
    /// An ended owner is still expected to release it. Ask again; the answer
    /// changes no later than `until`.
    Releasing { by: O, until: T },
    /// An ended owner never released it and it cannot be taken over.
    Unavailable { by: O },
}

/// Exclusive resources and who holds each.
///
/// `K` names a resource, `O` an owner and `T` is the layer's clock.
#[derive(Clone, Debug)]
pub struct LeaseTable<K, O, T> {
    leases: BTreeMap<K, Lease<O, T>>,
    issued: u64,
}

impl<K, O, T> Default for LeaseTable<K, O, T> {
    fn default() -> Self {
        Self {
            leases: BTreeMap::new(),
            issued: 0,
        }
    }
}

impl<K, O, T> LeaseTable<K, O, T>
where
    K: Ord + Clone,
    O: Clone + PartialEq,
    T: Copy + PartialOrd,
{
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for `resource` on behalf of the open owner `owner`.
    pub fn acquire(
        &mut self,
        resource: &K,
        owner: &O,
        takeover: Takeover,
        now: T,
    ) -> Acquire<O, T> {
        match self.leases.get(resource) {
            Some(lease) if lease.owner == *owner && !lease.ended() => {
                return Acquire::Granted {
                    token: lease.token,
                    superseded: None,
                };
            }
            Some(lease) => match lease.release_by {
                None => {
                    return Acquire::Held {
                        by: lease.owner.clone(),
                    };
                }
                Some(until) if now < until => {
                    return Acquire::Releasing {
                        by: lease.owner.clone(),
                        until,
                    };
                }
                Some(_) if takeover == Takeover::Refused => {
                    return Acquire::Unavailable {
                        by: lease.owner.clone(),
                    };
                }
                Some(_) => {}
            },
            None => {}
        }
        let token = self.next_token();
        let superseded = self.leases.insert(
            resource.clone(),
            Lease {
                owner: owner.clone(),
                token,
                release_by: None,
            },
        );
        Acquire::Granted { token, superseded }
    }

    /// End `owner`. Its leases stop authorizing mutation at once; a successor
    /// waits for each until `release_by` and no longer. Ending twice keeps
    /// the first instant, so a repeated end cannot extend the wait.
    pub fn end(&mut self, owner: &O, release_by: T) {
        for lease in self.leases.values_mut() {
            if lease.owner == *owner && lease.release_by.is_none() {
                lease.release_by = Some(release_by);
            }
        }
    }

    /// Release `resource` as the holder of `token`. A release under an older
    /// token changes nothing: the resource already belongs to a successor.
    pub fn release(&mut self, resource: &K, token: FenceToken) -> bool {
        if self
            .leases
            .get(resource)
            .is_some_and(|lease| lease.token == token)
        {
            self.leases.remove(resource);
            return true;
        }
        false
    }

    /// Release everything `owner` still holds. Leases a successor took over
    /// are no longer this owner's and are left alone.
    pub fn release_owner(&mut self, owner: &O) {
        self.leases.retain(|_, lease| lease.owner != *owner);
    }

    pub fn holder(&self, resource: &K) -> Option<&Lease<O, T>> {
        self.leases.get(resource)
    }

    /// Whether `token` may create or change `resource`: it must be the
    /// current lease and its owner must still be open.
    pub fn may_mutate(&self, resource: &K, token: FenceToken) -> bool {
        self.leases
            .get(resource)
            .is_some_and(|lease| lease.token == token && !lease.ended())
    }

    /// Whether `token` may still retire `resource`. An ended owner keeps this
    /// for exactly what it held, until a successor takes the resource over.
    pub fn may_retire(&self, resource: &K, token: FenceToken) -> bool {
        self.leases
            .get(resource)
            .is_some_and(|lease| lease.token == token)
    }

    /// Resources whose holder is open.
    pub fn held(&self) -> impl Iterator<Item = (&K, &Lease<O, T>)> {
        self.leases.iter().filter(|(_, lease)| !lease.ended())
    }

    /// Resources an ended owner has not released.
    pub fn releasing(&self) -> impl Iterator<Item = (&K, &Lease<O, T>)> {
        self.leases.iter().filter(|(_, lease)| lease.ended())
    }

    fn next_token(&mut self) -> FenceToken {
        self.issued = self
            .issued
            .checked_add(1)
            .expect("fence tokens outlast any host process");
        FenceToken(self.issued)
    }
}

#[cfg(test)]
mod tests;
