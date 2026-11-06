//! Recovery is executed by the consumer that originally admitted the resource.
//! A selected policy returns exact, bounded retirement references. It cannot
//! select a service, manufacture an arbitrary request, or declare native cleanup
//! successful. The policy independently verifies the owner's status afterward.
use amaterasu_module_sdk::requests::RequestOptions;
use amaterasu_module_sdk::retirement::{self, Contract, OperationRef, State, Status};
use amaterasu_module_sdk::session_presentation::{
    self as session, RecoveryBinding, RecoveryWorkItem, Request,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::Value;
use std::collections::{BTreeSet, VecDeque};
use std::time::{Duration, Instant};

#[cfg(test)]
const TRANSPORT_SERVICE: &str = "amaterasu.rtsp/1";
#[cfg(test)]
const APPLICATION_SERVICE: &str = "amaterasu.launch/1";
const LIST_BUDGET: Duration = Duration::from_secs(5);
const SCAN_INTERVAL: Duration = Duration::from_secs(1);
const RETRY_INTERVAL: Duration = Duration::from_millis(100);
const MAX_SCAN_PAGES: usize = 128;
const MAX_SCAN_ITEMS: usize = MAX_SCAN_PAGES * session::MAX_RECOVERY_ITEMS;

/// Keeps at most one admitted call and one bounded page of retirement intents.
/// `send` must use the same broker endpoint as normal launch and RTSP ownership.
pub(super) struct RecoveryPump {
    operation_prefix: String,
    next_operation: u64,
    phase: Phase,
    call: Option<BrokerCall>,
    current: Option<Attempt>,
    queued: VecDeque<RecoveryWorkItem>,
    cursor: Option<String>,
    seen_cursors: BTreeSet<String>,
    seen_items: BTreeSet<String>,
    scan_pages: usize,
    scan_had_work: bool,
    next_poll: Option<Instant>,
    observed_empty: bool,
}

#[derive(Clone, Copy)]
enum Phase {
    List,
    Retire,
    Status,
    ReconcileOwner,
    ReconcilePolicy,
}

struct Attempt {
    item: RecoveryWorkItem,
    deadline: Instant,
}

struct BrokerCall {
    command: Command,
    deadline: Instant,
    request: Option<u64>,
    attempted: bool,
    cancelling: bool,
    terminal: Option<Result<Value, ()>>,
}

enum CallResult {
    Waiting,
    Complete(Result<Value, ()>),
}

impl RecoveryPump {
    pub(super) fn new() -> ModuleResult<Self> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "recovery_operation_identity_unavailable")?;
        Ok(Self {
            operation_prefix: nonce.iter().map(|byte| format!("{byte:02x}")).collect(),
            next_operation: 0,
            phase: Phase::List,
            call: None,
            current: None,
            queued: VecDeque::new(),
            cursor: None,
            seen_cursors: BTreeSet::new(),
            seen_items: BTreeSet::new(),
            scan_pages: 0,
            scan_had_work: false,
            next_poll: None,
            observed_empty: false,
        })
    }

    /// `may_retire` must reject a binding held by any current live, paused,
    /// pending or uncertain controller record. It is checked before mutations,
    /// including recovery of an uncertain broker admission.
    pub(super) fn poll(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        mut may_retire: impl FnMut(&RecoveryBinding) -> bool,
    ) {
        if self.next_poll.is_some_and(|next| now < next) {
            return;
        }
        self.next_poll = None;
        if let Some(attempt) = &self.current {
            if matches!(self.phase, Phase::Retire | Phase::ReconcileOwner)
                && !may_retire(&attempt.item.binding)
            {
                if self.call.as_ref().is_some_and(|call| call.attempted) {
                    // Its admission may already exist. Keep the exact command
                    // and obligation until ownership permits safe settlement.
                    self.next_poll = Some(now + RETRY_INTERVAL);
                    return;
                }
                self.call = None;
                self.current = None;
                self.schedule_next(now);
                return;
            }
            if now >= attempt.deadline && self.call.is_none() {
                self.retry_scan(now);
                return;
            }
        }
        if self.call.is_none() {
            self.observed_empty = false;
            let Ok(call) = self.prepare_call(now) else {
                self.retry_scan(now);
                return;
            };
            self.call = Some(call);
        }
        let call = self.call.as_mut().expect("prepared recovery call");
        if now >= call.deadline {
            call.cancelling = true;
        }
        let result = match call.poll(send) {
            CallResult::Waiting => return,
            CallResult::Complete(result) => result,
        };
        let expired = self
            .call
            .as_ref()
            .expect("completed recovery call")
            .cancelling;
        self.call = None;
        if expired {
            self.retry_scan(now);
            return;
        }
        let Ok(payload) = result else {
            self.retry_scan(now);
            return;
        };
        match self.phase {
            Phase::List => self.accept_page(payload, now),
            Phase::Retire => {
                // An accepted stop is not retirement. Read the exact owner's
                // proof even if its stop response already looks terminal.
                self.phase = Phase::Status;
                self.next_poll = Some(now + RETRY_INTERVAL);
            }
            Phase::Status => self.accept_status(payload, now),
            Phase::ReconcileOwner => {
                self.phase = Phase::Status;
                self.next_poll = Some(now + RETRY_INTERVAL);
            }
            Phase::ReconcilePolicy => match session::decode_reply(payload) {
                Ok(session::Reply::Acknowledged {}) => {
                    self.current = None;
                    self.schedule_next(now);
                }
                Ok(session::Reply::Pending { retry_after_ms }) => {
                    self.next_poll = Some(now + Duration::from_millis(retry_after_ms));
                }
                _ => self.retry_scan(now),
            },
        }
    }

    /// Only an authoritative empty scan, with every broker result retired,
    /// establishes that this consumer has no outstanding recovery work.
    pub(super) fn is_idle(&self) -> bool {
        self.observed_empty
            && self.call.is_none()
            && self.current.is_none()
            && self.queued.is_empty()
    }

    fn prepare_call(&mut self, now: Instant) -> ModuleResult<BrokerCall> {
        self.next_operation = self
            .next_operation
            .checked_add(1)
            .ok_or("recovery_operation_identifiers_exhausted")?;
        let operation_id = format!(
            "session-recovery-{}-{}",
            self.operation_prefix, self.next_operation
        );
        let deadline = self
            .current
            .as_ref()
            .map_or(now + LIST_BUDGET, |attempt| attempt.deadline);
        let timeout_ms = deadline
            .saturating_duration_since(now)
            .as_millis()
            .clamp(1, session::MAX_RECOVERY_BUDGET_MS as u128) as u64;
        let command = match self.phase {
            Phase::List => session::command(
                RequestOptions {
                    service: session::SERVICE,
                    timeout_ms,
                    operation_id: &operation_id,
                    parent: None,
                },
                &Request::RecoveryWork {
                    cursor: self.cursor.clone(),
                    limit: session::MAX_RECOVERY_ITEMS,
                },
            )?,
            Phase::ReconcilePolicy => session::command(
                RequestOptions {
                    service: session::SERVICE,
                    timeout_ms,
                    operation_id: &operation_id,
                    parent: None,
                },
                &Request::ReconcileRecovery {
                    work_id: self.item()?.id.clone(),
                },
            )?,
            Phase::Retire | Phase::Status | Phase::ReconcileOwner => {
                let binding = &self.item()?.binding;
                let (contract, reference) = owner_reference(binding);
                let options = RequestOptions {
                    service: contract.service(),
                    timeout_ms,
                    operation_id: &operation_id,
                    parent: None,
                };
                match self.phase {
                    Phase::Retire => retirement::retire_command(options, contract, &reference)?,
                    Phase::Status => retirement::status_command(options, contract, &reference)?,
                    Phase::ReconcileOwner if contract == Contract::Transport => {
                        retirement::reconcile_transport_command(options)?
                    }
                    _ => return Err("unsupported_recovery_owner_action".into()),
                }
            }
        };
        Ok(BrokerCall {
            command,
            deadline,
            request: None,
            attempted: false,
            cancelling: false,
            terminal: None,
        })
    }

    fn item(&self) -> ModuleResult<&RecoveryWorkItem> {
        self.current
            .as_ref()
            .map(|attempt| &attempt.item)
            .ok_or_else(|| "missing_recovery_intent".into())
    }

    fn accept_page(&mut self, payload: Value, now: Instant) {
        let Ok(session::Reply::RecoveryWork { items, next_cursor }) =
            session::decode_reply(payload)
        else {
            self.retry_scan(now);
            return;
        };
        self.scan_pages += 1;
        if self.scan_pages > MAX_SCAN_PAGES
            || self.seen_items.len() + items.len() > MAX_SCAN_ITEMS
            || next_cursor
                .as_ref()
                .is_some_and(|cursor| self.seen_cursors.contains(cursor))
            || items.iter().any(|item| self.seen_items.contains(&item.id))
        {
            self.retry_scan(now);
            return;
        }
        for item in &items {
            self.seen_items.insert(item.id.clone());
        }
        if let Some(cursor) = &next_cursor {
            self.seen_cursors.insert(cursor.clone());
        }
        self.scan_had_work |= !items.is_empty();
        self.cursor = next_cursor;
        self.queued = items.into();
        self.schedule_next(now);
    }

    fn accept_status(&mut self, payload: Value, now: Instant) {
        let Some(attempt) = &self.current else {
            self.retry_scan(now);
            return;
        };
        let status = decode_retirement_status(&attempt.item.binding, &payload);
        if status.as_ref().is_some_and(|status| status.retired) {
            self.phase = Phase::ReconcilePolicy;
        } else if matches!(attempt.item.binding, RecoveryBinding::Transport { .. })
            && status
                .as_ref()
                .is_some_and(|status| status.state == State::RecoveryBlocked)
        {
            self.phase = Phase::ReconcileOwner;
        } else {
            self.next_poll = Some(now + RETRY_INTERVAL);
        }
    }

    fn schedule_next(&mut self, now: Instant) {
        if let Some(item) = self.queued.pop_front() {
            self.current = Some(Attempt {
                deadline: now + Duration::from_millis(item.budget_ms),
                item,
            });
            self.phase = Phase::Retire;
            return;
        }
        if self.cursor.is_some() {
            self.phase = Phase::List;
            return;
        }
        self.observed_empty = !self.scan_had_work;
        self.reset_scan();
        self.next_poll = Some(now + SCAN_INTERVAL);
    }

    fn retry_scan(&mut self, now: Instant) {
        self.observed_empty = false;
        self.current = None;
        self.queued.clear();
        self.reset_scan();
        self.next_poll = Some(now + RETRY_INTERVAL);
    }

    fn reset_scan(&mut self) {
        self.phase = Phase::List;
        self.cursor = None;
        self.scan_pages = 0;
        self.scan_had_work = false;
        self.seen_cursors.clear();
        self.seen_items.clear();
    }
}

impl BrokerCall {
    fn poll(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) -> CallResult {
        if self.terminal.is_some() {
            return self.release(send);
        }
        let Some(request) = self.request else {
            if self.cancelling && !self.attempted {
                return CallResult::Complete(Err(()));
            }
            self.attempted = true;
            return match send(self.command.clone()) {
                Ok(Response::Accepted { id }) => {
                    self.request = Some(id);
                    CallResult::Waiting
                }
                Ok(Response::Error { .. }) => CallResult::Complete(Err(())),
                _ => CallResult::Waiting,
            };
        };
        if self.cancelling {
            let _ = send(Command::Cancel { request });
        }
        match send(Command::TakeResult { request }) {
            Ok(Response::Pending) => CallResult::Waiting,
            Ok(Response::Result { payload }) => {
                self.terminal = Some(if self.cancelling {
                    Err(())
                } else {
                    Ok(payload)
                });
                self.release(send)
            }
            Ok(Response::Error { code, .. }) if code == "not_found" => {
                CallResult::Complete(Err(()))
            }
            Ok(Response::Error { code, .. })
                if matches!(code.as_str(), "cancelled" | "deadline_exceeded") =>
            {
                self.terminal = Some(Err(()));
                self.release(send)
            }
            _ => {
                self.cancelling = true;
                CallResult::Waiting
            }
        }
    }

    fn release(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) -> CallResult {
        let request = self
            .request
            .expect("terminal recovery call has an admission");
        match send(Command::ReleaseResult { request }) {
            Ok(Response::Ok) => {
                CallResult::Complete(self.terminal.take().expect("terminal result"))
            }
            Ok(Response::Error { code, .. }) if code == "not_found" => {
                CallResult::Complete(self.terminal.take().expect("terminal result"))
            }
            _ => CallResult::Waiting,
        }
    }
}

fn owner_reference(binding: &RecoveryBinding) -> (Contract, OperationRef) {
    match binding {
        RecoveryBinding::Transport {
            operation_id,
            epoch,
        } => (
            Contract::Transport,
            OperationRef {
                operation_id: operation_id.clone(),
                epoch: epoch.clone(),
            },
        ),
        RecoveryBinding::Application {
            operation_id,
            epoch,
            ..
        } => (
            Contract::Application,
            OperationRef {
                operation_id: operation_id.clone(),
                epoch: epoch.clone(),
            },
        ),
    }
}

fn decode_retirement_status(binding: &RecoveryBinding, payload: &Value) -> Option<Status> {
    let (contract, reference) = owner_reference(binding);
    let status = retirement::parse_status(contract, &reference, payload).ok()?;
    if let RecoveryBinding::Application {
        application_generation,
        ..
    } = binding
        && status
            .application_generation
            .is_some_and(|generation| generation != *application_generation)
    {
        return None;
    }
    Some(status)
}

#[cfg(test)]
#[path = "recovery/tests.rs"]
mod tests;
