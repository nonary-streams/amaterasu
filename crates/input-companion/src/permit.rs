//! Host-issued permission for one OS-identified interactive worker process.
//! Native code inside that worker remains trusted user code; this protects the
//! privileged companion from other processes and stale component sets.
use crate::{GrantRegistry, InputError, InputGrant, InputResult, PeerIdentity};
use std::time::{Duration, Instant};

const HOST_HEARTBEAT: Duration = Duration::from_secs(2);
const MAX_SESSION: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    pub principal: String,
    pub session: u32,
    pub process_id: u32,
    pub creation_time: u64,
}

impl ProcessIdentity {
    pub fn new(
        principal: String,
        session: u32,
        process_id: u32,
        creation_time: u64,
    ) -> InputResult<Self> {
        if principal.is_empty()
            || principal.len() > 256
            || session == u32::MAX
            || process_id == 0
            || creation_time == 0
        {
            return Err(InputError::InvalidPeer);
        }
        Ok(Self {
            principal,
            session,
            process_id,
            creation_time,
        })
    }

    pub fn matches_instance(&self, process_id: u32, creation_time: u64) -> bool {
        self.process_id == process_id && self.creation_time == creation_time && creation_time != 0
    }
}

struct HostPermit {
    process: ProcessIdentity,
    component_set_digest: String,
    keyboard: bool,
    mouse: bool,
    gamepad: bool,
    deadline: Instant,
}

pub struct WorkerLease {
    pub generation: u64,
    pub token: String,
    pub peer: PeerIdentity,
    pub grant: InputGrant,
}

/// The transport calls `refresh_from_host` only after proving the request came
/// from the live Amaterasu SCM service. `process` is independently inspected
/// from the worker's OS process, never deserialized from a worker packet.
pub struct PermitLedger {
    registry: GrantRegistry,
    authorized: Option<HostPermit>,
    active: Option<ProcessIdentity>,
}

impl PermitLedger {
    pub fn new(registry: GrantRegistry) -> Self {
        Self {
            registry,
            authorized: None,
            active: None,
        }
    }

    pub fn refresh_from_host(
        &mut self,
        process: ProcessIdentity,
        digest: String,
        keyboard: bool,
        mouse: bool,
        now: Instant,
    ) -> InputResult<()> {
        self.refresh_from_host_with_gamepad(process, digest, keyboard, mouse, false, now)
    }

    pub fn refresh_from_host_with_gamepad(
        &mut self,
        process: ProcessIdentity,
        digest: String,
        keyboard: bool,
        mouse: bool,
        gamepad: bool,
        now: Instant,
    ) -> InputResult<()> {
        let changed_authorization = self.authorized.as_ref().is_some_and(|previous| {
            previous.process == process
                && (previous.component_set_digest != digest
                    || previous.keyboard != keyboard
                    || previous.mouse != mouse
                    || previous.gamepad != gamepad)
        });
        if changed_authorization {
            self.revoke(&process);
            return Err(InputError::InvalidGrant);
        }
        let peer = PeerIdentity::from_verified_connection(
            process.principal.clone(),
            process.session,
            digest.clone(),
        )?;
        if !keyboard && !mouse && !gamepad || !peer.valid() {
            return Err(InputError::InvalidGrant);
        }
        self.authorized = Some(HostPermit {
            process,
            component_set_digest: digest,
            keyboard,
            mouse,
            gamepad,
            deadline: now + HOST_HEARTBEAT,
        });
        Ok(())
    }

    pub fn claim(&mut self, process: &ProcessIdentity, now: Instant) -> InputResult<WorkerLease> {
        if self.active.is_some() {
            return Err(InputError::ResourceExhausted);
        }
        let permit = self
            .authorized
            .as_ref()
            .ok_or(InputError::PermissionDenied)?;
        if now >= permit.deadline || &permit.process != process {
            return Err(InputError::PermissionDenied);
        }
        let peer = PeerIdentity::from_verified_connection(
            process.principal.clone(),
            process.session,
            permit.component_set_digest.clone(),
        )?;
        let token = amaterasu_storage::random_identifier().map_err(|_| InputError::Storage)?;
        let (generation, grant) = self.registry.issue_next_with_capabilities(
            peer.clone(),
            token.clone(),
            now + MAX_SESSION,
            permit.keyboard,
            permit.mouse,
            permit.gamepad,
        )?;
        self.active = Some(process.clone());
        Ok(WorkerLease {
            generation,
            token,
            peer,
            grant,
        })
    }

    pub fn is_current(&self, process: &ProcessIdentity, now: Instant) -> bool {
        self.active.as_ref() == Some(process)
            && self
                .authorized
                .as_ref()
                .is_some_and(|permit| &permit.process == process && now < permit.deadline)
    }

    pub fn may_open(&self, process: &ProcessIdentity, now: Instant) -> bool {
        self.authorized
            .as_ref()
            .is_some_and(|permit| &permit.process == process && now < permit.deadline)
    }

    pub fn release(&mut self, process: &ProcessIdentity) {
        if self.active.as_ref() == Some(process) {
            self.active = None;
        }
    }

    /// Immediately invalidates a host permit and its active worker lease.
    pub fn revoke(&mut self, process: &ProcessIdentity) {
        if self
            .authorized
            .as_ref()
            .is_some_and(|permit| &permit.process == process)
        {
            self.authorized = None;
        }
        if self.active.as_ref() == Some(process) {
            self.active = None;
        }
    }

    /// Revokes by the OS process identity captured when it was authorized.
    /// This also works after `/proc/<pid>` disappears on worker exit.
    pub fn revoke_instance(&mut self, process_id: u32, creation_time: u64) {
        let matches =
            |process: &ProcessIdentity| process.matches_instance(process_id, creation_time);
        let process = self
            .authorized
            .as_ref()
            .map(|permit| &permit.process)
            .filter(|process| matches(process))
            .or_else(|| self.active.as_ref().filter(|process| matches(process)))
            .cloned();
        if let Some(process) = process {
            self.revoke(&process);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(id: u32) -> ProcessIdentity {
        ProcessIdentity::new("S-1-5-21-test".into(), 3, id, 12345 + u64::from(id)).unwrap()
    }

    #[test]
    fn process_and_host_heartbeat_bound_the_grant() {
        let now = Instant::now();
        assert!(process(11).matches_instance(11, 12356));
        assert!(!process(11).matches_instance(11, 12357));
        let mut ledger = PermitLedger::new(GrantRegistry::synthetic());
        ledger
            .refresh_from_host(process(11), "a".repeat(64), true, false, now)
            .unwrap();
        assert!(ledger.may_open(&process(11), now));
        assert!(!ledger.may_open(&process(12), now));
        assert!(!ledger.may_open(&process(11), now + Duration::from_secs(2)));
        assert!(matches!(
            ledger.claim(&process(12), now),
            Err(InputError::PermissionDenied)
        ));
        let lease = ledger.claim(&process(11), now).unwrap();
        assert_eq!(lease.generation, 1);
        assert!(!lease.grant.gamepad_enabled());
        assert!(!ledger.is_current(&process(11), now + Duration::from_secs(2)));
        ledger.release(&process(11));
        assert!(matches!(
            ledger.claim(&process(11), now + Duration::from_secs(2)),
            Err(InputError::PermissionDenied)
        ));
    }

    #[test]
    fn restart_or_set_change_requires_new_host_authorization() {
        let now = Instant::now();
        let mut ledger = PermitLedger::new(GrantRegistry::synthetic());
        ledger
            .refresh_from_host(process(11), "a".repeat(64), true, true, now)
            .unwrap();
        let first = ledger.claim(&process(11), now).unwrap();
        assert_eq!(first.generation, 1);
        ledger.release(&process(11));
        assert_eq!(
            ledger.refresh_from_host(process(11), "b".repeat(64), true, true, now),
            Err(InputError::InvalidGrant)
        );
        ledger
            .refresh_from_host(process(12), "b".repeat(64), true, true, now)
            .unwrap();
        assert!(!ledger.is_current(&process(11), now));
        assert_eq!(ledger.claim(&process(12), now).unwrap().generation, 2);
    }

    #[test]
    fn host_revocation_invalidates_open_and_active_worker_lease() {
        let now = Instant::now();
        let process = process(11);
        let mut ledger = PermitLedger::new(GrantRegistry::synthetic());
        ledger
            .refresh_from_host(process.clone(), "a".repeat(64), true, true, now)
            .unwrap();
        let _lease = ledger.claim(&process, now).unwrap();

        ledger.revoke(&process);

        assert!(!ledger.may_open(&process, now));
        assert!(!ledger.is_current(&process, now));
        assert!(matches!(
            ledger.claim(&process, now),
            Err(InputError::PermissionDenied)
        ));
    }

    #[test]
    fn changed_same_worker_policy_immediately_revokes_the_previous_permit() {
        let now = Instant::now();
        let process = process(11);
        let mut ledger = PermitLedger::new(GrantRegistry::synthetic());
        ledger
            .refresh_from_host(process.clone(), "a".repeat(64), true, true, now)
            .unwrap();
        let _lease = ledger.claim(&process, now).unwrap();

        assert_eq!(
            ledger.refresh_from_host(process.clone(), "b".repeat(64), true, true, now),
            Err(InputError::InvalidGrant)
        );

        assert!(!ledger.is_current(&process, now));
        assert!(!ledger.may_open(&process, now));
    }

    #[test]
    fn exited_worker_can_be_revoked_using_its_captured_starttime() {
        let now = Instant::now();
        let process = process(11);
        let mut ledger = PermitLedger::new(GrantRegistry::synthetic());
        ledger
            .refresh_from_host(process.clone(), "a".repeat(64), true, true, now)
            .unwrap();
        let _lease = ledger.claim(&process, now).unwrap();

        ledger.revoke_instance(process.process_id, process.creation_time);

        assert!(!ledger.may_open(&process, now));
        assert!(!ledger.is_current(&process, now));
    }
}
