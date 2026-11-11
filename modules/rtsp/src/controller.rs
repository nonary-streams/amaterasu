mod actions;
#[cfg(test)]
mod concurrent_ports_tests;
mod lanes;
#[cfg(test)]
mod lanes_tests;
mod lifecycle_binding;
mod network;
#[cfg(test)]
mod ownership_tests;
mod preparation;
mod quiescence;
#[cfg(test)]
mod quiescence_tests;
mod record;
mod recovery_status;
mod retirement;
#[cfg(test)]
mod tests;
mod workflow;

use crate::{
    model::{Open, Operation, Settings, valid_name},
    rpc::{self, Call, Outcome},
    transport::{Event, Transport},
};
use amaterasu_gamestream::rtsp::{
    Request,
    negotiation::{
        Action, Capabilities, Negotiation, Step as NegotiationStep, TrustedLaunch, UdpDiscovery,
    },
    serialize_response,
};
use amaterasu_module_sdk::lifecycle_ledger::{Admission, Live, Owner, Retirement, make_room};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient, network::Network};
use record::{Job, Phase, Record, State};
use retirement::{clean_terminal, confirm_terminal, preparation_source};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    net::{IpAddr, SocketAddr},
    thread,
    time::{Duration, Instant},
};

const PREPARE_BUDGET_MS: u64 = 5_000;
const STAGED_DISPLAY_PREPARE_MAX_MS: u64 = 40_000;
const CLEANUP_BUDGET_MS: u64 = 15_000;
const MAX_CLEANUP_RETRIES: u8 = 2;
const CLEANUP_RETRY_DELAYS_MS: [u64; 2] = [250, 1_000];
const STATUS_INTERVAL_MS: u64 = 250;
const MAX_RECORDS: usize = 128;
const MAX_STARTUP_RECOVERY_SESSIONS: usize = 32;
const MAX_PENDING_NETWORK: usize = 16;
const MAX_DISPLAY_CLEANUP: usize = 16;
const STARTUP_RECONCILIATION_TIMEOUT_MS: u64 = 45_000;
/// How long after start a new ticket waits for the previous runtime's
/// sessions to be retired. They may still hold the media ports and normally
/// stop within a second. Past this, or once reconciliation has given up,
/// tickets are admitted while that cleanup carries on.
const STARTUP_ADMISSION_GRACE_MS: u64 = 10_000;
// Legacy synchronous unload shares one bounded budget across all known streams.
// The managed host instead announces its cooperative shutdown deadline.
const ORDERLY_SESSION_CLEANUP_BUDGET: Duration = Duration::from_secs(15);
const ORDERLY_SESSION_POLL_INTERVAL: Duration = Duration::from_millis(10);

struct PendingNetwork {
    connection: u64,
    ticket: u64,
    request: Request,
}

struct OpenRequest {
    request: u64,
    caller: String,
    config: Open,
    deadline: u64,
    display_preflight_deadline: Option<u64>,
}

struct DisplayAdmission {
    request: u64,
    caller: String,
    config: Open,
    deadline: u64,
    request_sha256: String,
    verified: bool,
    verification_started_at: u64,
    display_preflight_deadline: Option<u64>,
    call: Call,
}

struct DisplayCleanup {
    owner: String,
    operation_id: String,
    call: Call,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryPhase {
    List,
    Stop,
    Wait,
    Complete,
    Blocked,
}

struct StartupRecovery {
    nonce: u64,
    operation_sequence: u64,
    deadline: u64,
    retry_status_at: u64,
    phase: RecoveryPhase,
    call: Option<Call>,
    stale_sessions: VecDeque<u64>,
    attempted_sessions: BTreeSet<u64>,
    current_session: Option<u64>,
    error: Option<&'static str>,
    blocked_reported: bool,
}

/// This runtime's own tickets. Startup reconciliation retires what an
/// earlier runtime left behind and must never stop one of these.
struct CurrentTickets {
    operations: BTreeSet<String>,
    sessions: BTreeSet<u64>,
    /// A ticket whose stream session has not been reported back yet.
    preparing: bool,
}

impl StartupRecovery {
    fn new(nonce: u64, timeout_ms: u64) -> Self {
        Self {
            nonce,
            operation_sequence: 0,
            deadline: timeout_ms,
            retry_status_at: 0,
            phase: RecoveryPhase::List,
            call: None,
            stale_sessions: VecDeque::new(),
            attempted_sessions: BTreeSet::new(),
            current_session: None,
            error: None,
            blocked_reported: false,
        }
    }

    fn ready(&self) -> bool {
        self.phase == RecoveryPhase::Complete
    }

    #[cfg(test)]
    fn completed(nonce: u64) -> Self {
        let mut recovery = Self::new(nonce, STARTUP_RECONCILIATION_TIMEOUT_MS);
        recovery.phase = RecoveryPhase::Complete;
        recovery
    }

    fn blocked(&self) -> bool {
        self.phase == RecoveryPhase::Blocked
    }

    fn blocked_code(&self) -> &'static str {
        self.error.unwrap_or("startup_reconciliation_unavailable")
    }

    /// Say once why reconciliation stopped. It no longer answers an open, so
    /// the log is where its reason is found.
    fn report_blocked(&mut self) {
        if self.blocked() && !self.blocked_reported {
            self.blocked_reported = true;
            eprintln!(
                "rtsp startup reconciliation blocked code={}; new tickets are still admitted",
                self.blocked_code()
            );
        }
    }

    fn operation_id(&mut self, step: &str, session: Option<u64>) -> String {
        self.operation_sequence = self
            .operation_sequence
            .checked_add(1)
            .expect("bounded startup reconciliation");
        match session {
            Some(session) => format!(
                "rtsp-recovery-{:016x}-{session:016x}-{step}-{}",
                self.nonce, self.operation_sequence
            ),
            None => format!(
                "rtsp-recovery-{:016x}-{step}-{}",
                self.nonce, self.operation_sequence
            ),
        }
    }
}

pub struct Controller {
    network: Network,
    settings: Settings,
    bootstrap_settings: Settings,
    network_base_port: Option<u16>,
    network_address_family: Option<crate::model::AddressFamily>,
    network_bind_address: Option<String>,
    listener_base_port: Option<u16>,
    active_network_base_port: Option<u16>,
    active_address_family: Option<crate::model::AddressFamily>,
    active_bind_address: Option<String>,
    network_error: Option<&'static str>,
    network_retry_at: u64,
    transport: Option<Transport>,
    local: Option<SocketAddr>,
    locals: Vec<SocketAddr>,
    records: BTreeMap<u64, Record>,
    pending_network: VecDeque<PendingNetwork>,
    /// The job of an open ticket or an incoming request. While it is busy
    /// no further request is read, so only open tickets may occupy it.
    job: Option<Job>,
    /// One cleanup job per ended ticket, each advancing on its own.
    retiring_jobs: BTreeMap<u64, Job>,
    /// The job slot's own job while an ended ticket's job occupies it.
    parked_job: Option<Job>,
    quiescence: Option<quiescence::Quiescence>,
    shutdown_deadline: Option<u64>,
    shutdown_settlement: Vec<Call>,
    display_admission: Option<DisplayAdmission>,
    display_cleanup: Vec<DisplayCleanup>,
    startup_recovery: StartupRecovery,
    /// Set once tickets are admitted; it does not close again.
    startup_admission_opened: bool,
    runtime_epoch: String,
    retired_operations: BTreeSet<(String, String)>,
    sequence: u64,
    status_after: u64,
}

impl Controller {
    #[cfg(test)]
    pub fn new(settings: Settings) -> Result<Self, &'static str> {
        Self::with_network(settings, Network::test_provider())
    }

    pub fn with_network(settings: Settings, network: Network) -> Result<Self, &'static str> {
        settings.validate()?;
        let mut epoch_bytes = [0; 16];
        getrandom::fill(&mut epoch_bytes).map_err(|_| "random_unavailable")?;
        let recovery_nonce = u64::from_ne_bytes(epoch_bytes[..8].try_into().expect("fixed slice"));
        let runtime_epoch = epoch_bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let network_base_port = settings.default_base_port();
        let (transport, locals, local) = if settings.enabled {
            let transport = Transport::bind_all_with_network(
                network.clone(),
                settings.resolved_binds()?,
                settings.allow_remote,
            )?;
            let locals = transport.local_addrs()?;
            let local = locals.first().copied().ok_or("tcp_address_failed")?;
            (Some(transport), locals, Some(local))
        } else {
            (None, Vec::new(), None)
        };
        #[cfg(test)]
        let startup_recovery = StartupRecovery::completed(recovery_nonce);
        #[cfg(not(test))]
        let startup_recovery =
            StartupRecovery::new(recovery_nonce, STARTUP_RECONCILIATION_TIMEOUT_MS);
        Ok(Self {
            network,
            bootstrap_settings: settings.clone(),
            active_address_family: None,
            active_bind_address: None,
            settings,
            network_base_port,
            network_address_family: None,
            network_bind_address: None,
            listener_base_port: network_base_port,
            active_network_base_port: None,
            network_error: None,
            network_retry_at: 0,
            transport,
            local,
            locals,
            records: BTreeMap::new(),
            pending_network: VecDeque::new(),
            job: None,
            retiring_jobs: BTreeMap::new(),
            parked_job: None,
            quiescence: None,
            shutdown_deadline: None,
            shutdown_settlement: Vec::new(),
            display_admission: None,
            display_cleanup: Vec::new(),
            startup_recovery,
            startup_admission_opened: false,
            runtime_epoch,
            retired_operations: BTreeSet::new(),
            sequence: 0,
            status_after: 0,
        })
    }

    pub fn tick(&mut self, runtime: &RuntimeClient, now: u64) -> ModuleResult<()> {
        if self.quiescence.is_some() {
            return self.tick_quiescing(runtime, now);
        }
        self.cap_announced_cleanup_deadline();
        self.poll_transport(now);
        self.advance_shutdown_settlement(runtime);
        self.cap_announced_cleanup_deadline();
        self.advance_startup_recovery(runtime, now);
        self.move_ended_job_to_its_own_lane();
        self.advance_job(runtime, now);
        self.move_ended_job_to_its_own_lane();
        self.advance_retiring_jobs(runtime, now);
        self.advance_display_admission(runtime, now);
        self.advance_display_cleanup(runtime);
        self.stamp_ended(now);
        self.keep_ticket_capacity();
        if self.job.is_none() {
            self.process_pending_network(now);
        }
        self.move_ended_job_to_its_own_lane();
        self.schedule_retiring(now);
        if self.job.is_none() {
            self.schedule_background(now);
        }
        if self.job.is_none() && self.display_admission.is_none() {
            self.poll_caller(runtime, now)?;
        }
        self.move_ended_job_to_its_own_lane();
        self.cap_announced_cleanup_deadline();
        self.reconcile_network_port(now);
        Ok(())
    }

    /// Revoke RTSP tickets and make bounded, owner-scoped stop attempts for every
    /// known stream-session before this module unloads.
    pub fn shutdown(&mut self, runtime: &RuntimeClient) {
        if self.quiescence.is_some() || self.shutdown_deadline.is_some() {
            return;
        }
        let cleanup_deadline = Instant::now() + ORDERLY_SESSION_CLEANUP_BUDGET;
        self.shutdown_settlement.retain_mut(|call| {
            !matches!(
                call.shutdown(runtime),
                Outcome::SettledFailure | Outcome::Result(_)
            )
        });
        if let Some(call) = self.startup_recovery.call.as_mut() {
            call.shutdown(runtime);
        }
        let jobs: Vec<Job> = self
            .job
            .take()
            .into_iter()
            .chain(std::mem::take(&mut self.retiring_jobs).into_values())
            .collect();
        for mut job in jobs {
            if matches!(
                job.phase,
                Phase::Observe | Phase::Status | Phase::Find | Phase::LifecycleLookup
            ) {
                job.call.cancel_read_only();
            }
            let outcome = job.call.shutdown(runtime);
            if let Some(record) = self.records.get_mut(&job.ticket) {
                apply_observed_shutdown_result(record, job.phase, &job.call);
                if matches!(outcome, Outcome::SettledFailure) {
                    apply_undispatched_shutdown_proof(record, job.phase, &job.call);
                } else {
                    record.uncertain_phase = Some(job.phase);
                    record.uncertain = Some(job.call);
                }
            }
        }
        if let Some(admission) = self.display_admission.as_mut() {
            admission.call.shutdown(runtime);
        }
        for cleanup in &mut self.display_cleanup {
            cleanup.call.shutdown(runtime);
        }
        for record in self.records.values_mut() {
            if let Some(mut call) = record.uncertain.take() {
                let phase = record.uncertain_phase.take().unwrap_or(Phase::Prepare);
                apply_observed_shutdown_result(record, phase, &call);
                if matches!(
                    phase,
                    Phase::Observe | Phase::Status | Phase::Find | Phase::LifecycleLookup
                ) {
                    call.cancel_read_only();
                }
                if matches!(call.shutdown(runtime), Outcome::SettledFailure) {
                    apply_undispatched_shutdown_proof(record, phase, &call);
                } else {
                    record.uncertain_phase = Some(phase);
                    record.uncertain = Some(call);
                }
            }
        }
        self.stop_owned_sessions_on_shutdown(runtime, cleanup_deadline);
    }

    fn stop_owned_sessions_on_shutdown(&mut self, runtime: &RuntimeClient, deadline: Instant) {
        let tickets: Vec<_> = self.records.keys().copied().collect();
        for ticket in tickets {
            let Some(mut record) = self.records.remove(&ticket) else {
                continue;
            };
            if !record.state.terminal() && record.state != State::Closing {
                record.close(Some("module_stopping"));
            }
            self.revoke_ticket(ticket);
            if !record.cleanup_confirmed {
                self.stop_owned_session_on_shutdown(runtime, &mut record, deadline);
            }
            self.records.insert(ticket, record);
        }
    }

    fn stop_owned_session_on_shutdown(
        &mut self,
        runtime: &RuntimeClient,
        record: &mut Record,
        deadline: Instant,
    ) {
        if record.uncertain.is_some() {
            let read_only = matches!(
                record.uncertain_phase,
                Some(Phase::Observe | Phase::Status | Phase::Find | Phase::LifecycleLookup)
            );
            let observed = record
                .uncertain
                .as_ref()
                .is_some_and(|call| call.observed_result().is_some());
            if (read_only || observed) && self.shutdown_settlement.len() < MAX_RECORDS {
                self.shutdown_settlement
                    .push(record.uncertain.take().expect("retained call"));
                record.uncertain_phase = None;
            } else {
                record.state = State::CleanupFailed;
                record.cleanup_error = Some("shutdown_settlement_unconfirmed");
                return;
            }
        }
        if record.session.is_none() && record.needs_lookup {
            let lookup = shutdown_request(
                runtime,
                &self.settings.stream_service,
                json!({"action":"lookup","operation_id":record.operation()}),
                format!("{}-module-shutdown-lookup", record.operation()),
                deadline,
            );
            match lookup {
                Ok(payload) if payload["ok"] == true => {
                    record.session = response_session(&payload);
                    if record.session.is_none() {
                        record.state = State::CleanupFailed;
                        record.cleanup_error = Some("shutdown_lookup_invalid");
                        return;
                    }
                }
                Ok(payload) if payload["code"] == "session_not_found" => {
                    record.state = State::CleanupFailed;
                    record.cleanup_error = Some("shutdown_lookup_not_found");
                    return;
                }
                Err(failure) => {
                    retain_shutdown_failure(record, Phase::Find, failure);
                    return;
                }
                Ok(_) => {
                    record.state = State::CleanupFailed;
                    record.cleanup_error = Some("shutdown_lookup_unconfirmed");
                    return;
                }
            }
        }

        if record.lifecycle_prepare_outstanding
            && !self.resolve_lifecycle_on_shutdown(runtime, record, deadline)
        {
            return;
        }

        let Some(_) = record.session else {
            record.cleanup_confirmed =
                record.lifecycle_target.is_none() && !record.lifecycle_prepare_outstanding;
            finish_record(record);
            return;
        };
        let stop = shutdown_request(
            runtime,
            &self.settings.stream_service,
            record.stop_payload(),
            format!("{}-module-shutdown-stop", record.operation()),
            deadline,
        );
        match stop {
            Ok(payload) if confirm_terminal(record, &payload) => {
                record.stream_state = stream_state(&payload).map(str::to_owned);
                record.cleanup_error = None;
                finish_record(record);
            }
            Ok(_) => {
                record.state = State::CleanupFailed;
                record.cleanup_error = Some("shutdown_stop_rejected");
            }
            Err(failure) => retain_shutdown_failure(record, Phase::Stop, failure),
        }
    }

    fn resolve_lifecycle_on_shutdown(
        &mut self,
        runtime: &RuntimeClient,
        record: &mut Record,
        deadline: Instant,
    ) -> bool {
        while Instant::now() < deadline {
            let operation_id = self.operation_id(record, "shutdown-lifecycle-lookup");
            let response = shutdown_request(
                runtime,
                self.settings.lifecycle_service.as_deref().unwrap_or(""),
                json!({"operation":"lookup", "operation_id":format!("{}-lifecycle-prepare", record.operation())}),
                operation_id,
                deadline,
            );
            match response {
                Ok(payload) => match preparation::apply_response(record, &payload) {
                    preparation::PreparationResolution::Admitted
                    | preparation::PreparationResolution::Rejected => return true,
                    preparation::PreparationResolution::Pending => {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if !remaining.is_zero() {
                            thread::sleep(ORDERLY_SESSION_POLL_INTERVAL.min(remaining));
                        }
                    }
                    preparation::PreparationResolution::Unconfirmed => {
                        record.state = State::CleanupFailed;
                        record.cleanup_error = Some("shutdown_lifecycle_unconfirmed");
                        return false;
                    }
                },
                Err(failure) => {
                    retain_shutdown_failure(record, Phase::LifecycleLookup, failure);
                    return false;
                }
            }
        }
        record.state = State::CleanupFailed;
        record.cleanup_error = Some("shutdown_lifecycle_deadline");
        false
    }

    fn advance_startup_recovery(&mut self, runtime: &RuntimeClient, now: u64) {
        self.advance_startup_recovery_step(runtime, now);
        self.startup_recovery.report_blocked();
        self.startup_admission_open(now);
    }

    /// Whether tickets are admitted yet. Reconciliation is waited for
    /// briefly and never decides admission beyond that.
    pub(super) fn startup_admission_open(&mut self, now: u64) -> bool {
        self.startup_admission_opened |= self.startup_recovery.ready()
            || self.startup_recovery.blocked()
            || now >= STARTUP_ADMISSION_GRACE_MS;
        self.startup_admission_opened
    }

    fn current_tickets(&self) -> CurrentTickets {
        CurrentTickets {
            operations: self.records.values().map(Record::operation).collect(),
            sessions: self
                .records
                .values()
                .filter_map(|record| record.session)
                .collect(),
            preparing: self
                .records
                .values()
                .any(|record| record.session.is_none() && !record.state.terminal()),
        }
    }

    fn advance_startup_recovery_step(&mut self, runtime: &RuntimeClient, now: u64) {
        let current = self.current_tickets();
        let recovery = &mut self.startup_recovery;
        if recovery.phase == RecoveryPhase::Complete {
            return;
        }
        if recovery.phase == RecoveryPhase::Blocked {
            if let Some(call) = recovery.call.as_mut() {
                match call.poll(runtime) {
                    Outcome::SettledFailure | Outcome::Result(_) => recovery.call = None,
                    Outcome::Waiting => {}
                    Outcome::Uncertain => {
                        // Retry only settlement of this retained broker identity.
                        // A transient outage must not permanently exhaust the
                        // local poll allowance and prevent explicit recovery.
                        call.retry_settlement();
                    }
                }
            }
            return;
        }
        if now >= recovery.deadline {
            recovery.error = Some("startup_reconciliation_timeout");
            recovery.phase = RecoveryPhase::Blocked;
            if let Some(call) = recovery.call.as_mut() {
                call.retry_settlement();
            }
            return;
        }

        if recovery.call.is_none() {
            match recovery.phase {
                RecoveryPhase::List => {
                    if now < recovery.retry_status_at {
                        return;
                    }
                    let operation = recovery.operation_id("list", None);
                    recovery.call = Some(Call::for_service(
                        &self.settings.stream_service,
                        json!({"action":"status"}),
                        operation,
                        None,
                    ));
                }
                RecoveryPhase::Stop => {
                    let Some(session) = recovery.stale_sessions.pop_front() else {
                        // The Stop replies prove these exact owners retired,
                        // but only a fresh global snapshot proves the durable
                        // startup journal finished reconciling.
                        recovery.phase = RecoveryPhase::List;
                        recovery.retry_status_at = now;
                        return;
                    };
                    recovery.current_session = Some(session);
                    let operation = recovery.operation_id("stop", Some(session));
                    recovery.call = Some(Call::for_service(
                        &self.settings.stream_service,
                        json!({"action":"stop","session":session}),
                        operation,
                        None,
                    ));
                }
                RecoveryPhase::Wait if now >= recovery.retry_status_at => {
                    let Some(session) = recovery.current_session else {
                        recovery.error = Some("startup_reconciliation_state_invalid");
                        recovery.phase = RecoveryPhase::Blocked;
                        return;
                    };
                    let operation = recovery.operation_id("status", Some(session));
                    recovery.call = Some(Call::for_service(
                        &self.settings.stream_service,
                        json!({"action":"status","session":session}),
                        operation,
                        None,
                    ));
                }
                RecoveryPhase::Wait => return,
                RecoveryPhase::Complete | RecoveryPhase::Blocked => return,
            }
        }

        let phase = recovery.phase;
        let outcome = recovery
            .call
            .as_mut()
            .expect("startup recovery call created")
            .poll(runtime);
        match outcome {
            Outcome::Waiting => {}
            Outcome::SettledFailure | Outcome::Uncertain => {
                recovery.error = Some("startup_reconciliation_unavailable");
                recovery.phase = RecoveryPhase::Blocked;
                if let Some(call) = recovery.call.as_mut() {
                    call.retry_settlement();
                }
            }
            Outcome::Result(payload) => {
                recovery.call = None;
                let result = match phase {
                    RecoveryPhase::List => accept_recovery_list(recovery, &payload, &current, now),
                    RecoveryPhase::Stop => {
                        let Some(session) = recovery.current_session else {
                            recovery.error = Some("startup_reconciliation_state_invalid");
                            recovery.phase = RecoveryPhase::Blocked;
                            return;
                        };
                        match recovery_stop_response(&payload, session) {
                            Ok(true) => finish_recovered_session(recovery),
                            Ok(false) => {
                                recovery.phase = RecoveryPhase::Wait;
                                recovery.retry_status_at = now.saturating_add(STATUS_INTERVAL_MS);
                                Ok(())
                            }
                            Err(code) => Err(code),
                        }
                    }
                    RecoveryPhase::Wait => {
                        let Some(session) = recovery.current_session else {
                            recovery.error = Some("startup_reconciliation_state_invalid");
                            recovery.phase = RecoveryPhase::Blocked;
                            return;
                        };
                        match recovery_status_response(&payload, session) {
                            Ok(true) => finish_recovered_session(recovery),
                            Ok(false) => {
                                recovery.retry_status_at = now.saturating_add(STATUS_INTERVAL_MS);
                                Ok(())
                            }
                            Err(code) => Err(code),
                        }
                    }
                    RecoveryPhase::Complete | RecoveryPhase::Blocked => Ok(()),
                };
                if let Err(code) = result {
                    recovery.error = Some(code);
                    recovery.phase = RecoveryPhase::Blocked;
                }
            }
        }
    }

    fn reply_status(&mut self, connection: u64, cseq: u32, status: u16) -> bool {
        serialize_response(status, cseq, &[], &[])
            .ok()
            .is_some_and(|reply| self.reply_bytes(connection, &reply))
    }

    fn reply_bytes(&mut self, connection: u64, reply: &[u8]) -> bool {
        self.transport
            .as_mut()
            .is_some_and(|transport| transport.reply(connection, reply).is_ok())
    }

    fn revoke_ticket(&mut self, ticket: u64) {
        if let Some(transport) = self.transport.as_mut() {
            let _ = transport.remove_ticket(ticket);
        }
        self.pending_network
            .retain(|pending| pending.ticket != ticket);
    }

    pub(super) fn configure_network_policy(
        &mut self,
        port: u16,
        address_family: Option<crate::model::AddressFamily>,
        bind_address: Option<String>,
        now: u64,
    ) -> Result<Value, &'static str> {
        crate::model::validate_base_port(port)?;
        let candidate =
            self.candidate_network_settings(port, address_family, bind_address.clone())?;
        candidate.validate()?;
        self.network_base_port = Some(port);
        self.network_address_family = address_family;
        self.network_bind_address = bind_address;
        self.network_error = None;
        let applied = self.try_apply_network_port();
        if let Err(error) = applied {
            self.network_error = Some(error);
            self.network_retry_at = now.saturating_add(1_000);
        } else if applied == Ok(true) {
            self.active_network_base_port = Some(port);
            self.network_retry_at = 0;
        }
        Ok(self.network_status(applied.is_ok_and(|applied| applied)))
    }

    #[cfg(test)]
    pub(super) fn configure_network_port(
        &mut self,
        port: u16,
        now: u64,
    ) -> Result<Value, &'static str> {
        self.configure_network_policy(port, None, None, now)
    }

    pub(super) fn media_binds_for_open(
        &self,
        local_address: IpAddr,
    ) -> Option<crate::model::MediaBinds> {
        let binds = self
            .active_network_base_port
            .or(self.listener_base_port)
            .and_then(|port| self.settings.media_binds_on(port, local_address).ok())?;
        // Reserve predictable ports for the signed concurrent-client bound so
        // administrators can permit a bounded UDP range through the firewall.
        // An open ticket holds its slot. A slot whose ticket ended without
        // confirming cleanup is used only when no slot is free, and then the
        // one that ended longest ago: its ports are the likeliest to have
        // been released, and a ticket that never confirms cannot keep them.
        let mut releasing: Option<(u64, crate::model::MediaBinds)> = None;
        for slot in 0..self.settings.max_live_tickets {
            let offset = u16::try_from(slot).ok()?.checked_mul(3)?;
            let mut candidate = binds;
            candidate
                .video
                .set_port(binds.video.port().checked_add(offset)?);
            candidate
                .control
                .set_port(binds.control.port().checked_add(offset)?);
            candidate
                .audio
                .set_port(binds.audio.port().checked_add(offset)?);
            let mut held = false;
            let mut last_ended = None;
            for record in self
                .records
                .values()
                .filter(|record| record.media_binds == Some(candidate))
            {
                match record.admission() {
                    Admission::Open => held = true,
                    Admission::Ended if record.retirement() == Retirement::Confirmed => {}
                    Admission::Ended => {
                        last_ended = last_ended.max(Some(record.ended_at.unwrap_or(u64::MAX)));
                    }
                }
            }
            if held {
                continue;
            }
            let Some(ended) = last_ended else {
                return Some(candidate);
            };
            if releasing.is_none_or(|(earliest, _)| ended < earliest) {
                releasing = Some((ended, candidate));
            }
        }
        releasing.map(|(_, candidate)| candidate)
    }

    pub(super) fn advertised_local_for_peer(
        &self,
        peer: IpAddr,
    ) -> Result<SocketAddr, &'static str> {
        let listener = self
            .locals
            .iter()
            .copied()
            .find(|address| address.ip().is_ipv4() == peer.is_ipv4())
            .ok_or("peer_family_mismatch")?;
        if !listener.ip().is_unspecified() {
            return Ok(listener);
        }

        // UDP connect performs route selection without sending a datagram. The
        // resulting local endpoint is advertised and reused for this stream's
        // per-session UDP listeners instead of returning an unusable wildcard.
        let route = self
            .network
            .bind_udp(SocketAddr::new(listener.ip(), 0))
            .map_err(|_| "route_selection_failed")?;
        route
            .connect(SocketAddr::new(peer, 9))
            .map_err(|_| "route_selection_failed")?;
        let source = route.local_addr().map_err(|_| "route_selection_failed")?;
        if source.ip().is_unspecified() {
            return Err("route_selection_failed");
        }
        Ok(SocketAddr::new(source.ip(), listener.port()))
    }

    fn reconcile_network_port(&mut self, now: u64) {
        if self.network_base_port.is_none() || now < self.network_retry_at {
            return;
        }
        match self.try_apply_network_port() {
            Ok(true) => {
                self.active_network_base_port = self.network_base_port;
                self.network_error = None;
                self.network_retry_at = 0;
            }
            Ok(false) => {}
            Err(error) => {
                self.network_error = Some(error);
                self.network_retry_at = now.saturating_add(1_000);
            }
        }
    }

    fn try_apply_network_port(&mut self) -> Result<bool, &'static str> {
        let Some(base_port) = self.network_base_port else {
            return Ok(false);
        };
        let candidate = self.candidate_network_settings(
            base_port,
            self.network_address_family,
            self.network_bind_address.clone(),
        )?;
        candidate.validate()?;
        let binds = candidate.rtsp_binds_for_base(base_port)?;
        if self.active_network_base_port == Some(base_port)
            && self.active_address_family == self.network_address_family
            && self.active_bind_address == self.network_bind_address
            && self.locals == binds
        {
            return Ok(true);
        }
        if !candidate.enabled
            || self.records.values().any(|record| !record.state.terminal())
            || self
                .transport
                .as_ref()
                .is_some_and(|transport| transport.connection_count() != 0)
        {
            return Ok(false);
        }

        candidate.probe_media_binds_with_network(&self.network, base_port)?;
        if self.locals == binds {
            self.listener_base_port = Some(base_port);
            self.active_network_base_port = Some(base_port);
            self.active_address_family = self.network_address_family;
            self.active_bind_address = self.network_bind_address.clone();
            self.settings = candidate;
            self.network_error = None;
            return Ok(true);
        }
        let transport = self.transport.as_mut().ok_or("tcp_listener_unavailable")?;
        transport.rebind_all(binds, candidate.allow_remote)?;
        let locals = transport.local_addrs()?;
        let local = locals.first().copied().ok_or("tcp_address_failed")?;
        self.local = Some(local);
        self.locals = locals;
        self.settings = candidate;
        self.listener_base_port = Some(base_port);
        self.active_network_base_port = Some(base_port);
        self.active_address_family = self.network_address_family;
        self.active_bind_address = self.network_bind_address.clone();
        self.network_error = None;
        Ok(true)
    }

    fn candidate_network_settings(
        &self,
        base_port: u16,
        address_family: Option<crate::model::AddressFamily>,
        bind_address: Option<String>,
    ) -> Result<Settings, &'static str> {
        crate::model::validate_base_port(base_port)?;
        let mut candidate = self.bootstrap_settings.clone();
        candidate.set_base_port(base_port)?;
        candidate.address_family = address_family.or(self.bootstrap_settings.address_family);
        candidate.bind_address =
            bind_address.or_else(|| self.bootstrap_settings.bind_address.clone());
        Ok(candidate)
    }

    fn network_status(&self, applied: bool) -> Value {
        serde_json::json!({
            "ok": true,
            "desired_port": self.network_base_port,
            "desired_address_family": self.network_address_family,
            "desired_bind_address": self.network_bind_address,
            "rtsp_port": self.local.map(|address| address.port()),
            "active_base_port": self.active_network_base_port,
            "active_address_family": self.active_address_family,
            "active_bind_address": self.active_bind_address,
            "rtsp_addresses": self.locals.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "active": applied,
            "pending": !applied,
            "udp_binds_preflighted": applied,
            "error": self.network_error,
        })
    }
}

fn accept_recovery_list(
    recovery: &mut StartupRecovery,
    payload: &Value,
    current: &CurrentTickets,
    now: u64,
) -> Result<(), &'static str> {
    let reconciled = payload["startup_reconciled"]
        .as_bool()
        .ok_or("startup_reconciliation_response_invalid")?;
    // Validate the complete owner-scoped page before choosing any mutations.
    let Some(mut stale_sessions) = recovery_list(payload, current)? else {
        // A session could not be told apart from a ticket that is being
        // prepared right now. Look again once that ticket knows its session.
        recovery.retry_status_at = now.saturating_add(STATUS_INTERVAL_MS);
        return Ok(());
    };
    if stale_sessions
        .iter()
        .any(|session| recovery.attempted_sessions.contains(session))
    {
        // A successful Stop followed by a live observation is not permission to
        // replay cleanup or report retirement. An explicit reconciliation pass
        // may retry after the conflicting state has been investigated.
        return Err("startup_reconciliation_cleanup_unconfirmed");
    }
    if !reconciled {
        // Stream-session still drives ordinary interrupted workflows itself.
        // Only its failed journal owners need this bounded owner Stop; waiting
        // for the global barrier first would prevent those owners ever retrying.
        stale_sessions.retain(|session| {
            payload["sessions"]
                .as_array()
                .expect("validated recovery page")
                .iter()
                .any(|entry| entry["session"] == *session && entry["state"] == "cleanup_failed")
        });
    }
    if stale_sessions.is_empty() {
        if reconciled {
            recovery.phase = RecoveryPhase::Complete;
        } else {
            recovery.retry_status_at = now.saturating_add(STATUS_INTERVAL_MS);
        }
        return Ok(());
    }
    if recovery.attempted_sessions.len() + stale_sessions.len() > MAX_STARTUP_RECOVERY_SESSIONS {
        return Err("startup_reconciliation_capacity_exhausted");
    }
    recovery
        .attempted_sessions
        .extend(stale_sessions.iter().copied());
    recovery.stale_sessions = stale_sessions;
    recovery.phase = RecoveryPhase::Stop;
    Ok(())
}

/// The sessions an earlier runtime left behind. `None` means one of them
/// could not be told apart from a ticket this runtime is preparing.
fn recovery_list(
    payload: &Value,
    current: &CurrentTickets,
) -> Result<Option<VecDeque<u64>>, &'static str> {
    let sessions = payload["sessions"]
        .as_array()
        .ok_or("startup_reconciliation_response_invalid")?;
    let total = payload["total"]
        .as_u64()
        .ok_or("startup_reconciliation_response_invalid")?;
    let truncated = payload["truncated"]
        .as_bool()
        .ok_or("startup_reconciliation_response_invalid")?;
    if payload["ok"] != true
        || sessions.len() > MAX_STARTUP_RECOVERY_SESSIONS
        || total < sessions.len() as u64
        || (truncated && sessions.len() != MAX_STARTUP_RECOVERY_SESSIONS)
        || (!truncated && total != sessions.len() as u64)
    {
        return Err("startup_reconciliation_response_invalid");
    }

    // Stream-session status is broker-scoped to this RTSP module principal and
    // places all nonterminal records before retained terminal history. Its live
    // record limit is at most 32, so this page includes every orphan. It also
    // includes the sessions of tickets admitted since this runtime started;
    // those are recognized by their operation or session and left alone.
    let mut stale = VecDeque::new();
    let mut seen = BTreeSet::new();
    let mut ambiguous = false;
    for session in sessions {
        let id = session["session"]
            .as_u64()
            .filter(|id| *id != 0)
            .ok_or("startup_reconciliation_response_invalid")?;
        let state = session["state"]
            .as_str()
            .ok_or("startup_reconciliation_response_invalid")?;
        if !known_stream_state(state) || !seen.insert(id) {
            return Err("startup_reconciliation_response_invalid");
        }
        let cleanup_clear = session["cleanup_error"].is_null();
        let terminal = matches!(state, "stopped" | "failed" | "cancelled") && cleanup_clear;
        if terminal {
            continue;
        }
        let operation = session["operation_id"].as_str();
        if current.sessions.contains(&id)
            || operation.is_some_and(|operation| current.operations.contains(operation))
        {
            continue;
        }
        // A provider that does not report the operation leaves a session
        // unidentified while one of this runtime's tickets is still preparing.
        ambiguous |= operation.is_none() && current.preparing;
        stale.push_back(id);
    }
    Ok((!ambiguous).then_some(stale))
}

fn recovery_stop_response(payload: &Value, expected_session: u64) -> Result<bool, &'static str> {
    if payload["ok"] == false && payload["code"] == "session_not_found" {
        return if payload["startup_reconciled"] == true {
            Ok(true)
        } else {
            Err("startup_reconciliation_unconfirmed")
        };
    }
    recovery_session_state(payload, expected_session)
}

fn recovery_status_response(payload: &Value, expected_session: u64) -> Result<bool, &'static str> {
    if payload["ok"] == false && payload["code"] == "session_not_found" {
        return if payload["startup_reconciled"] == true {
            Ok(true)
        } else {
            Err("startup_reconciliation_unconfirmed")
        };
    }
    recovery_session_state(payload, expected_session)
}

fn recovery_session_state(payload: &Value, expected_session: u64) -> Result<bool, &'static str> {
    if payload["ok"] != true || payload["session"].as_u64() != Some(expected_session) {
        return Err("startup_reconciliation_response_invalid");
    }
    let state = payload["state"]
        .as_str()
        .ok_or("startup_reconciliation_response_invalid")?;
    if !known_stream_state(state) {
        return Err("startup_reconciliation_response_invalid");
    }
    if matches!(state, "stopped" | "failed" | "cancelled") {
        return if payload["cleanup_error"].is_null() {
            Ok(true)
        } else {
            eprintln!(
                "rtsp startup recovery blocked state={state} cleanup_error={}",
                payload["cleanup_error"].as_str().unwrap_or("invalid")
            );
            Err("startup_reconciliation_cleanup_failed")
        };
    }
    if state == "cleanup_failed" {
        eprintln!(
            "rtsp startup recovery blocked state={state} cleanup_error={}",
            payload["cleanup_error"].as_str().unwrap_or("unknown")
        );
        return Err("startup_reconciliation_cleanup_failed");
    }
    Ok(false)
}

fn known_stream_state(state: &str) -> bool {
    matches!(
        state,
        "preparing"
            | "prepared"
            | "armed"
            | "starting"
            | "active"
            | "stopping"
            | "stopped"
            | "failed"
            | "cancelled"
            | "cleanup_failed"
    )
}

fn finish_recovered_session(recovery: &mut StartupRecovery) -> Result<(), &'static str> {
    recovery.current_session = None;
    recovery.phase = RecoveryPhase::Stop;
    Ok(())
}

fn response_session(payload: &Value) -> Option<u64> {
    payload["session"].as_u64().filter(|session| *session != 0)
}

fn local_port(payload: &Value, field: &str) -> Result<u16, &'static str> {
    let port = payload[field]
        .as_str()
        .ok_or("invalid_prepare_response")?
        .parse::<SocketAddr>()
        .map_err(|_| "invalid_prepare_response")?
        .port();
    (port != 0)
        .then_some(port)
        .ok_or("invalid_prepare_response")
}

fn authenticated_disconnect_reason(record: &Record, payload: &Value) -> Option<&'static str> {
    if !clean_terminal(record, payload) || payload["state"] != "failed" {
        return None;
    }
    if record.lifecycle_target.is_none() {
        let proof = &payload["authenticated_disconnect"];
        if proof["version"] != 1
            || proof["session"].as_u64() != record.session
            || proof["operation_id"] != record.operation()
            || proof["control_authenticated"] != true
            || proof["reason"] != payload["authenticated_disconnect_reason"]
        {
            return None;
        }
    }
    match payload["authenticated_disconnect_reason"].as_str()? {
        "peer_disconnected" => Some("peer_disconnected"),
        "authenticated_peer_idle" => Some("authenticated_peer_idle"),
        "peer_timeout" => Some("peer_timeout"),
        _ => None,
    }
}

struct ShutdownFailure {
    code: &'static str,
    call: Box<Call>,
}

fn apply_observed_shutdown_result(record: &mut Record, phase: Phase, call: &Call) {
    if matches!(phase, Phase::LifecyclePrepare | Phase::LifecycleLookup)
        && let Some(payload) = call.observed_result()
    {
        preparation::apply_response(record, payload);
    }
}

fn apply_undispatched_shutdown_proof(record: &mut Record, phase: Phase, call: &Call) {
    if !call.proven_not_dispatched() {
        return;
    }
    match phase {
        Phase::LifecyclePrepare => record.lifecycle_prepare_outstanding = false,
        Phase::Prepare => record.needs_lookup = false,
        _ => {}
    }
}

fn retain_shutdown_failure(record: &mut Record, phase: Phase, failure: ShutdownFailure) {
    record.state = State::CleanupFailed;
    record.cleanup_error = Some(failure.code);
    record.uncertain_phase = Some(phase);
    record.uncertain = Some(*failure.call);
}

fn shutdown_request(
    runtime: &RuntimeClient,
    service: &str,
    payload: Value,
    operation_id: String,
    deadline: Instant,
) -> Result<Value, ShutdownFailure> {
    let mut call = Call::for_service(service, payload, operation_id, None);
    let mut code = "shutdown_request_unconfirmed";
    while Instant::now() < deadline {
        match call.poll(runtime) {
            Outcome::Result(payload) => return Ok(payload),
            Outcome::SettledFailure => {
                code = "shutdown_request_unavailable";
                break;
            }
            Outcome::Uncertain => break,
            Outcome::Waiting => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !remaining.is_zero() {
                    thread::sleep(ORDERLY_SESSION_POLL_INTERVAL.min(remaining));
                }
            }
        }
    }
    // Retain exact call identity even if cancellation/release cannot be settled.
    // Canceling an unsubmitted call cannot admit work after the shared deadline.
    call.shutdown(runtime);
    Err(ShutdownFailure {
        code,
        call: Box::new(call),
    })
}

fn stream_state(payload: &Value) -> Option<&'static str> {
    match payload["state"].as_str()? {
        "preparing" => Some("preparing"),
        "prepared" => Some("prepared"),
        "armed" => Some("armed"),
        "starting" => Some("starting"),
        "active" => Some("active"),
        "stopping" => Some("stopping"),
        "stopped" => Some("stopped"),
        "failed" => Some("failed"),
        "cancelled" => Some("cancelled"),
        "cleanup_failed" => Some("cleanup_failed"),
        _ => None,
    }
}

fn missing(payload: &Value) -> bool {
    matches!(
        payload["code"].as_str(),
        Some("session_not_found" | "not_found")
    )
}

fn finish_record(record: &mut Record) {
    if record.lifecycle_prepare_outstanding
        || (record.lifecycle_target.is_some() && !record.cleanup_confirmed)
    {
        record.cleanup_confirmed = false;
        record.state = State::CleanupFailed;
        record.cleanup_error = Some("lifecycle_retirement_unconfirmed");
        return;
    }
    record.display_applied = false;
    record.cleanup_retry_count = 0;
    record.cleanup_retry_at = None;
    record.state = if record.failure.is_some() {
        State::Failed
    } else {
        State::Closed
    };
    record.cleanup_error = None;
}

fn complete_caller(record: &Record, caller: Option<u64>, runtime: &RuntimeClient) {
    if let Some(request) = caller.filter(|request| rpc::active(runtime, *request)) {
        rpc::complete(runtime, request, record.response());
    }
}

fn error(code: &str) -> Value {
    json!({"ok":false,"code":code})
}
