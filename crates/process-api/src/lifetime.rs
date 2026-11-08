//! Experimental host-to-provider lifetime ABI for the process interface.
//!
//! This is separate from a graph-bound consumer table. The trusted host keeps
//! the verified provider library and its instance alive across graph reloads;
//! provider workers must never retain graph callbacks, buffers, or endpoints.
//! A provider remains responsible for every accepted child until exact native
//! retirement, even after its last graph binding is released. No native grant
//! is a sandbox. This declaration is not a production composition.

use amaterasu_abi::{AbiHeader, BorrowedBytes, NativeServiceFactory, ReplySink, Status};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;

/// Entry-point symbol exported by a verified process provider component.
pub const ENTRY_SYMBOL: &[u8] = b"amaterasu_process_provider_v1\0";
/// The provider is still settling work, bindings, children, or workers.
pub const BUSY: Status = 103;
pub const MAX_LIMIT: u32 = 65_536;
pub const MAX_CONFIG_BYTES: usize = 4096;

/// Explicit host-approved upper bounds for one process-lifetime instance.
/// Exhaustion rejects new admission before allocating a child or other effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderLimits {
    pub bindings: u32,
    pub epochs: u32,
    pub operations: u32,
    pub queued_operations: u32,
    pub reservations: u32,
    pub processes: u32,
    pub sinks: u32,
}

impl ProviderLimits {
    pub fn validate(&self) -> Result<(), &'static str> {
        let limits = [
            self.bindings,
            self.epochs,
            self.operations,
            self.queued_operations,
            self.reservations,
            self.processes,
            self.sinks,
        ];
        if limits
            .into_iter()
            .all(|limit| (1..=MAX_LIMIT).contains(&limit))
            && self.queued_operations <= self.operations
            && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= MAX_CONFIG_BYTES)
        {
            Ok(())
        } else {
            Err("invalid_process_provider_limits")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPhase {
    Running,
    Retiring,
    Retired,
}

/// A bounded observation, not authority to unload the provider. In particular,
/// native retirement is established only by successful `try_destroy` below.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSnapshot {
    pub phase: ProviderPhase,
    pub bindings: u32,
    pub epochs: u32,
    pub operations: u32,
    pub reservations: u32,
    pub processes: u32,
    pub sinks: u32,
    pub workers: u32,
    pub retirement_failed: bool,
}

impl ProviderSnapshot {
    pub fn validate(&self) -> Result<(), &'static str> {
        let counts = [
            self.bindings,
            self.epochs,
            self.operations,
            self.reservations,
            self.processes,
            self.sinks,
            self.workers,
        ];
        if counts.into_iter().any(|count| count > MAX_LIMIT)
            || (self.phase == ProviderPhase::Retired
                && (self.retirement_failed || counts.into_iter().any(|count| count != 0)))
        {
            Err("invalid_process_provider_snapshot")
        } else {
            Ok(())
        }
    }
}

/// Returns the stable provider-lifetime table for the verified artifact.
pub type ProcessProviderEntry = unsafe extern "C" fn() -> *const ProcessProviderApi;

/// Version 1 host-to-provider table. Every callback is thread-safe, bounded,
/// nonblocking and must not unwind. Callbacks may borrow input and reply storage
/// only until the synchronous call returns. No graph callback may be retained.
///
/// The host validates this table and the exact selected component/digest before
/// calling it and retains the provider library until `try_destroy` succeeds.
/// The factory returned by `factory` is borrowed from the instance: its context
/// and code remain valid through all registrations and bindings. The host
/// guarantees those runtime registrations and all concurrent calls are gone
/// before attempting destruction.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessProviderApi {
    pub header: AbiHeader,
    /// Creates one instance using bounded JSON-encoded `ProviderLimits`. On OK,
    /// output is initialized to its unique non-null owner pointer; on failure
    /// no worker, native child, or other provider resource may be left behind.
    pub create: unsafe extern "C" fn(BorrowedBytes, *mut *mut c_void) -> Status,
    /// Returns a non-null borrowed factory implementing `super::CONTRACT`.
    /// No factory returned after retirement begins may create a new binding.
    pub factory: unsafe extern "C" fn(*mut c_void, *mut *const NativeServiceFactory) -> Status,
    /// Sends one bounded JSON-encoded `ProviderSnapshot` to the borrowed sink.
    pub inspect: unsafe extern "C" fn(*mut c_void, ReplySink) -> Status,
    /// Idempotently closes new binding and epoch admission and rejects new
    /// Reserve, OpenSink and Spawn requests. Existing bindings may still submit
    /// StopManaged, ForceManaged, CancelReservation, CloseSink and
    /// ReleaseObservation for resources they own, as well as settle already
    /// admitted work. This must not signal detached children or abandon an
    /// already accepted spawn, even if its result is not yet available.
    pub begin_retirement: unsafe extern "C" fn(*mut c_void) -> Status,
    /// Atomically destroys the unique instance only after retirement has been
    /// requested and every binding, worker and native ownership obligation has
    /// ended. OK consumes the instance exactly once; BUSY or any error retains
    /// the exact instance and all ownership, and the host must keep the library
    /// loaded. Snapshot observations alone never authorize unloading.
    pub try_destroy: unsafe extern "C" fn(*mut c_void) -> Status,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ProviderLimits {
        ProviderLimits {
            bindings: 4,
            epochs: 4,
            operations: 16,
            queued_operations: 8,
            reservations: 8,
            processes: 8,
            sinks: 8,
        }
    }

    #[test]
    fn limits_are_explicit_bounded_and_queue_fits_retained_operations() {
        assert_eq!(limits().validate(), Ok(()));
        let mut value = limits();
        value.processes = 0;
        assert!(value.validate().is_err());
        value = limits();
        value.bindings = MAX_LIMIT + 1;
        assert!(value.validate().is_err());
        value = limits();
        value.queued_operations = value.operations + 1;
        assert!(value.validate().is_err());
    }

    #[test]
    fn retired_snapshot_cannot_claim_live_ownership_or_failure() {
        let mut snapshot = ProviderSnapshot {
            phase: ProviderPhase::Retired,
            bindings: 0,
            epochs: 0,
            operations: 0,
            reservations: 0,
            processes: 0,
            sinks: 0,
            workers: 0,
            retirement_failed: false,
        };
        assert_eq!(snapshot.validate(), Ok(()));
        snapshot.processes = 1;
        assert!(snapshot.validate().is_err());
        snapshot.processes = 0;
        snapshot.retirement_failed = true;
        assert!(snapshot.validate().is_err());
        snapshot.phase = ProviderPhase::Retiring;
        assert_eq!(snapshot.validate(), Ok(()));
    }

    #[test]
    fn lifetime_table_has_a_versioned_prefix_and_fixed_callbacks() {
        let header = AbiHeader::of::<ProcessProviderApi>();
        assert!(header.supports::<ProcessProviderApi>());
        assert_eq!(std::mem::offset_of!(ProcessProviderApi, header), 0);
        assert_eq!(
            std::mem::size_of::<ProcessProviderApi>(),
            std::mem::offset_of!(ProcessProviderApi, try_destroy)
                + std::mem::size_of::<unsafe extern "C" fn(*mut c_void) -> Status>()
        );
    }
}
