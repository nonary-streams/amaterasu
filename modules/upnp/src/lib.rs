//! Explicit, caller-driven lifecycle management for IPv4 UPnP port mappings.
//!
//! The manager owns only mappings it successfully created. Call
//! [`MappingManager::refresh_owned`] periodically to reassert their recorded leases, and
//! call [`MappingManager::shutdown`] during orderly shutdown to remove mappings that
//! still match the exact recorded mapping.

mod igd;

pub use igd::{IgdGateway, SEARCH_TIMEOUT, discover_gateway};

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(test)]
mod tests;

static OWNER_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Recommended caller-driven interval for refreshing owned mappings.
///
/// A zero-duration lease request is the IGDv2 maximum of 604,800 seconds (one week).
/// Callers must refresh before that lease expires. This interval is a safety margin;
/// callers schedule it because this crate does not run a refresh timer.
pub const RECOMMENDED_REFRESH_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// The network protocol used by an IGD port mapping.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Protocol {
    Tcp,
    Udp,
}

/// The IGD lookup key for a wildcard-remote-host mapping.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MappingKey {
    pub external_port: u16,
    pub protocol: Protocol,
}

/// The complete mapping attributes managed by this crate.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Mapping {
    pub external_port: u16,
    pub protocol: Protocol,
    pub internal_address: Ipv4Addr,
    pub internal_port: u16,
    pub remote_host: String,
    pub enabled: bool,
    pub description: String,
    /// Requested lease on writes; IGDv2 queries report remaining seconds that count down.
    pub lease_duration_seconds: u32,
}

impl Mapping {
    /// Returns the IGD lookup key for this mapping.
    pub fn key(&self) -> MappingKey {
        MappingKey {
            external_port: self.external_port,
            protocol: self.protocol,
        }
    }

    /// Returns whether the stable attributes identifying this owned mapping match.
    ///
    /// IGDv2 reports the remaining lease duration, which counts down between reads.
    /// That dynamic field is excluded; every other mapping attribute must match.
    pub fn has_same_owned_attributes(&self, other: &Self) -> bool {
        self.external_port == other.external_port
            && self.protocol == other.protocol
            && self.internal_address == other.internal_address
            && self.internal_port == other.internal_port
            && self.remote_host == other.remote_host
            && self.enabled == other.enabled
            && self.description == other.description
    }
}

/// One configured listener supplied by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListenerEndpoint {
    pub address: IpAddr,
    pub port: u16,
    pub active: bool,
}

impl ListenerEndpoint {
    pub fn new(address: IpAddr, port: u16, active: bool) -> Self {
        Self {
            address,
            port,
            active,
        }
    }
}

/// A gateway adapter error. The message is preserved for the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayError {
    message: String,
}

impl GatewayError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for GatewayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GatewayError {}

/// Result of checking an owned mapping before removal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemovalOutcome {
    Removed,
    Missing,
    Changed,
}

/// Result of refreshing an owned mapping after checking its complete recorded value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshOutcome {
    Refreshed,
    Missing,
    Changed,
}

/// The adapter contract used by the lifecycle manager.
///
/// `create_if_absent` must refuse to replace any mapping with the same external
/// port and protocol. `remove_if_matches` and `refresh_if_matches` must compare
/// all stable mapping attributes, excluding only the IGDv2 remaining lease value,
/// before acting and report whether they removed/refreshed, found missing, or found
/// changed mappings.
pub trait Gateway {
    fn find_mappings(&mut self, key: MappingKey) -> Result<Vec<Mapping>, GatewayError>;

    fn create_if_absent(&mut self, mapping: &Mapping) -> Result<(), GatewayError>;

    fn remove_if_matches(&mut self, mapping: &Mapping) -> Result<RemovalOutcome, GatewayError>;

    fn refresh_if_matches(&mut self, mapping: &Mapping) -> Result<RefreshOutcome, GatewayError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingOperation {
    Inspect,
    Add,
    Remove,
    Refresh,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MappingFailure {
    pub operation: MappingOperation,
    pub mapping: MappingKey,
    pub source: GatewayError,
}

/// Lifecycle errors surfaced to the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpnpError {
    ExistingMapping {
        key: MappingKey,
        existing: Vec<Mapping>,
    },
    DuplicateDesiredMapping {
        key: MappingKey,
    },
    GatewayFailure(MappingFailure),
    AddFailed {
        failure: MappingFailure,
        rollback_failures: Vec<MappingFailure>,
    },
    CleanupFailed {
        failures: Vec<MappingFailure>,
    },
    ShutdownFailed {
        failures: Vec<MappingFailure>,
    },
    RefreshFailed {
        failures: Vec<MappingFailure>,
    },
}

impl fmt::Display for UpnpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExistingMapping { key, .. } => write!(
                formatter,
                "UPnP mapping {} {:?} already exists and will not be replaced",
                key.external_port, key.protocol
            ),
            Self::DuplicateDesiredMapping { key } => write!(
                formatter,
                "multiple listeners require UPnP mapping {} {:?}",
                key.external_port, key.protocol
            ),
            Self::GatewayFailure(failure) => write!(
                formatter,
                "UPnP {:?} failed for {} {:?}: {}",
                failure.operation,
                failure.mapping.external_port,
                failure.mapping.protocol,
                failure.source
            ),
            Self::AddFailed {
                failure,
                rollback_failures,
            } => write!(
                formatter,
                "UPnP add failed for {} {:?}: {}; {} rollback failure(s)",
                failure.mapping.external_port,
                failure.mapping.protocol,
                failure.source,
                rollback_failures.len()
            ),
            Self::CleanupFailed { failures } => write!(
                formatter,
                "UPnP reconcile left {} stale mapping cleanup failure(s)",
                failures.len()
            ),
            Self::ShutdownFailed { failures } => write!(
                formatter,
                "UPnP shutdown left {} mapping cleanup failure(s)",
                failures.len()
            ),
            Self::RefreshFailed { failures } => write!(
                formatter,
                "UPnP refresh failed for {} owned mapping(s)",
                failures.len()
            ),
        }
    }
}

impl std::error::Error for UpnpError {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReconcileReport {
    pub created: usize,
    pub removed: usize,
    pub already_active: usize,
    pub changed_mappings_preserved: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ShutdownReport {
    pub removed: usize,
    pub already_missing: usize,
    pub changed_mappings_preserved: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RefreshReport {
    pub refreshed: usize,
    pub missing: usize,
    pub changed_mappings_preserved: usize,
}

/// Reconciles a caller-provided set of active IPv4 listener endpoints.
pub struct MappingManager<G> {
    gateway: G,
    owned: BTreeMap<MappingKey, Mapping>,
    description: String,
}

impl<G: Gateway> MappingManager<G> {
    /// Creates a manager with a unique process-local mapping description.
    pub fn new(gateway: G) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let counter = OWNER_COUNTER.fetch_add(1, Ordering::Relaxed);
        let owner_id = format!("{}-{timestamp:x}-{counter:x}", process::id());
        Self::with_owner_id(gateway, &owner_id)
    }

    /// Creates a manager with a stable caller-provided owner identifier.
    ///
    /// This is useful when the caller needs deterministic mapping descriptions.
    /// The identifier must be short enough for the gateway's description limit.
    pub fn with_owner_id(gateway: G, owner_id: &str) -> Self {
        Self {
            gateway,
            owned: BTreeMap::new(),
            description: format!("Amaterasu UPnP {owner_id}"),
        }
    }

    /// Adds desired mappings before removing stale owned mappings.
    ///
    /// A failed add rolls back mappings created by this reconcile attempt and
    /// retains the previous owned set. A pre-existing mapping is never adopted
    /// or overwritten.
    pub fn reconcile(
        &mut self,
        endpoints: &[ListenerEndpoint],
    ) -> Result<ReconcileReport, UpnpError> {
        let desired = self.desired_mappings(endpoints)?;
        let mut additions = Vec::new();
        let mut report = ReconcileReport::default();

        for (key, expected_mapping) in &desired {
            let existing = self.gateway.find_mappings(*key).map_err(|source| {
                UpnpError::GatewayFailure(MappingFailure {
                    operation: MappingOperation::Inspect,
                    mapping: *key,
                    source,
                })
            })?;

            if existing.is_empty() {
                additions.push(expected_mapping.clone());
            } else if self.owned.get(key) == Some(expected_mapping)
                && existing
                    .iter()
                    .any(|mapping| mapping.has_same_owned_attributes(expected_mapping))
            {
                report.already_active += 1;
            } else {
                return Err(UpnpError::ExistingMapping {
                    key: *key,
                    existing,
                });
            }
        }

        let mut created_this_reconcile = Vec::new();
        for mapping in additions {
            let key = mapping.key();
            if let Err(source) = self.gateway.create_if_absent(&mapping) {
                let failure = MappingFailure {
                    operation: MappingOperation::Add,
                    mapping: key,
                    source,
                };
                let rollback_failures = self.rollback_created(&created_this_reconcile);
                return Err(UpnpError::AddFailed {
                    failure,
                    rollback_failures,
                });
            }

            self.owned.insert(key, mapping.clone());
            created_this_reconcile.push(mapping);
            report.created += 1;
        }

        let stale_mappings: Vec<Mapping> = self
            .owned
            .values()
            .filter(|owned_mapping| desired.get(&owned_mapping.key()) != Some(*owned_mapping))
            .cloned()
            .collect();

        let mut cleanup_failures = Vec::new();
        for mapping in stale_mappings {
            let key = mapping.key();
            match self.gateway.remove_if_matches(&mapping) {
                Ok(RemovalOutcome::Removed) => {
                    self.owned.remove(&key);
                    report.removed += 1;
                }
                Ok(RemovalOutcome::Missing) => {
                    self.owned.remove(&key);
                }
                Ok(RemovalOutcome::Changed) => {
                    self.owned.remove(&key);
                    report.changed_mappings_preserved += 1;
                }
                Err(source) => cleanup_failures.push(MappingFailure {
                    operation: MappingOperation::Remove,
                    mapping: key,
                    source,
                }),
            }
        }

        if !cleanup_failures.is_empty() {
            return Err(UpnpError::CleanupFailed {
                failures: cleanup_failures,
            });
        }

        Ok(report)
    }

    /// Removes only mappings this manager created that still match exactly.
    ///
    /// Call this explicitly during orderly shutdown. Cleanup failures are
    /// returned and their mappings remain tracked for a possible retry.
    pub fn shutdown(&mut self) -> Result<ShutdownReport, UpnpError> {
        let owned_mappings: Vec<Mapping> = self.owned.values().cloned().collect();
        let mut report = ShutdownReport::default();
        let mut failures = Vec::new();

        for mapping in owned_mappings {
            let key = mapping.key();
            match self.gateway.remove_if_matches(&mapping) {
                Ok(RemovalOutcome::Removed) => {
                    self.owned.remove(&key);
                    report.removed += 1;
                }
                Ok(RemovalOutcome::Missing) => {
                    self.owned.remove(&key);
                    report.already_missing += 1;
                }
                Ok(RemovalOutcome::Changed) => {
                    self.owned.remove(&key);
                    report.changed_mappings_preserved += 1;
                }
                Err(source) => failures.push(MappingFailure {
                    operation: MappingOperation::Remove,
                    mapping: key,
                    source,
                }),
            }
        }

        if failures.is_empty() {
            Ok(report)
        } else {
            Err(UpnpError::ShutdownFailed { failures })
        }
    }

    /// Refreshes each currently owned mapping after the gateway confirms it still matches.
    ///
    /// Missing and changed mappings are removed from ownership. Gateway errors are
    /// returned after attempting every mapping, and the affected mappings remain owned
    /// so a later call can retry them.
    pub fn refresh_owned(&mut self) -> Result<RefreshReport, UpnpError> {
        let owned_mappings: Vec<Mapping> = self.owned.values().cloned().collect();
        let mut report = RefreshReport::default();
        let mut failures = Vec::new();

        for mapping in owned_mappings {
            let key = mapping.key();
            match self.gateway.refresh_if_matches(&mapping) {
                Ok(RefreshOutcome::Refreshed) => report.refreshed += 1,
                Ok(RefreshOutcome::Missing) => {
                    self.owned.remove(&key);
                    report.missing += 1;
                }
                Ok(RefreshOutcome::Changed) => {
                    self.owned.remove(&key);
                    report.changed_mappings_preserved += 1;
                }
                Err(source) => failures.push(MappingFailure {
                    operation: MappingOperation::Refresh,
                    mapping: key,
                    source,
                }),
            }
        }

        if failures.is_empty() {
            Ok(report)
        } else {
            Err(UpnpError::RefreshFailed { failures })
        }
    }

    /// Returns the mappings this manager currently believes it owns.
    pub fn owned_mappings(&self) -> impl Iterator<Item = &Mapping> {
        self.owned.values()
    }

    fn desired_mappings(
        &self,
        endpoints: &[ListenerEndpoint],
    ) -> Result<BTreeMap<MappingKey, Mapping>, UpnpError> {
        let mut desired = BTreeMap::new();

        for endpoint in endpoints {
            if !endpoint.active || endpoint.port == 0 {
                continue;
            }

            let IpAddr::V4(internal_address) = endpoint.address else {
                continue;
            };

            for protocol in [Protocol::Tcp, Protocol::Udp] {
                let mapping = Mapping {
                    external_port: endpoint.port,
                    protocol,
                    internal_address,
                    internal_port: endpoint.port,
                    remote_host: String::new(),
                    enabled: true,
                    description: self.description.clone(),
                    lease_duration_seconds: 0,
                };
                let key = mapping.key();

                if let Some(previous) = desired.get(&key) {
                    if previous != &mapping {
                        return Err(UpnpError::DuplicateDesiredMapping { key });
                    }
                } else {
                    desired.insert(key, mapping);
                }
            }
        }

        Ok(desired)
    }

    fn rollback_created(&mut self, created: &[Mapping]) -> Vec<MappingFailure> {
        let mut failures = Vec::new();

        for mapping in created.iter().rev() {
            let key = mapping.key();
            match self.gateway.remove_if_matches(mapping) {
                Ok(_) => {
                    self.owned.remove(&key);
                }
                Err(source) => failures.push(MappingFailure {
                    operation: MappingOperation::Remove,
                    mapping: key,
                    source,
                }),
            }
        }

        failures
    }
}
