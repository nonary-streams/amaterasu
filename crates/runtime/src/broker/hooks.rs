use amaterasu_contracts::{Grants, HookOutcome, HookSpec, Proposal, Response, valid_name};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use super::errors::{capacity_exhausted, not_found, payload_fits, permission_denied};
use super::requests::{RequestOptions, RequestRouter};

const MAX_INVOCATIONS: usize = 64;
// Operation identities remain fenced for the lifetime of the broker, even after
// results expire or are released. Exhaustion fails closed instead of replaying.
const MAX_OPERATION_IDENTITIES: usize = 512;
const RESULT_RETENTION: Duration = Duration::from_secs(300);

struct ActiveParticipant {
    name: String,
    request_id: u64,
}
/// How an owner runs one invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HookRunMode {
    /// Attempt and report every participant instead of stopping at the first
    /// absent or failed one.
    pub cleanup: bool,
    /// Give each participant the previous participant's result as its input.
    pub chain: bool,
    /// Give the operation identifier up when the invocation is released or
    /// expires, instead of fencing it for the broker's lifetime.
    pub transient: bool,
}

struct OperationIdentity {
    invocation: u64,
    hook: String,
    input: Value,
    mode: HookRunMode,
}
struct HookInvocation {
    owner: String,
    hook_name: String,
    remaining_participants: VecDeque<String>,
    input: Value,
    deadline: Instant,
    active_participant: Option<ActiveParticipant>,
    proposals: Vec<Proposal>,
    outcomes: Vec<HookOutcome>,
    cleanup: bool,
    chain: bool,
    retained: bool,
    terminal: Option<(Response, Instant)>,
}

impl HookInvocation {
    fn advance(
        &mut self,
        invocation_id: u64,
        grants: &Grants,
        now: Instant,
        requests: &mut RequestRouter,
        registered: &BTreeSet<(String, String)>,
    ) -> Response {
        if let Some(participant) = self.active_participant.take() {
            let mut result = requests.result(&self.owner, participant.request_id);
            if matches!(result, Response::Pending) {
                self.active_participant = Some(participant);
                if now < self.deadline {
                    return Response::Pending;
                }
            } else {
                requests.release(&self.owner, participant.request_id);
                if self.chain {
                    result = self.adopt_chained_value(result);
                }
                if self.cleanup {
                    if matches!(&result, Response::Error { code, .. } if code == "deadline_exceeded")
                    {
                        result =
                            Response::error("deadline_exceeded", "participant outcome is unknown");
                    }
                    self.outcomes.push(HookOutcome {
                        participant: participant.name,
                        result: Box::new(result),
                    });
                } else if let Response::Result { payload } = result {
                    self.proposals.push(Proposal {
                        participant: participant.name,
                        payload,
                    });
                } else {
                    return attributed(result, &participant.name);
                }
            }
        }
        if now >= self.deadline
            && (self.active_participant.is_some() || !self.remaining_participants.is_empty())
        {
            if self.cleanup {
                if let Some(participant) = self.active_participant.as_ref() {
                    self.outcomes.push(HookOutcome {
                        participant: participant.name.clone(),
                        result: Box::new(Response::error(
                            "deadline_exceeded",
                            "participant outcome is unknown",
                        )),
                    });
                }
                self.cancel_active(now, requests);
                self.outcomes
                    .extend(
                        self.remaining_participants
                            .drain(..)
                            .map(|participant| HookOutcome {
                                participant,
                                result: Box::new(Response::error(
                                    "deadline_exceeded",
                                    "participant was not attempted",
                                )),
                            }),
                    );
                return Response::HookCleanupResult {
                    outcomes: std::mem::take(&mut self.outcomes),
                };
            }
            self.cancel_active(now, requests);
            return Response::error("deadline_exceeded", "hook deadline elapsed");
        }
        while let Some(participant) = self.remaining_participants.pop_front() {
            let response = if !registered.contains(&(self.hook_name.clone(), participant.clone())) {
                Response::error("unavailable", "hook participant is absent")
            } else {
                let options = RequestOptions {
                    service: format!("hooks/{}", self.hook_name),
                    payload: self.input.clone(),
                    timeout_ms: 0,
                    operation_id: format!("hook.{invocation_id}.{participant}"),
                    parent: None,
                    response_schema: None,
                };
                requests.submit_hook(
                    &self.owner,
                    grants,
                    participant.clone(),
                    options,
                    self.deadline,
                )
            };
            if let Response::Accepted { id } = response {
                self.active_participant = Some(ActiveParticipant {
                    name: participant,
                    request_id: id,
                });
                return Response::Pending;
            }
            if !self.cleanup {
                return attributed(response, &participant);
            }
            self.outcomes.push(HookOutcome {
                participant,
                result: Box::new(response),
            });
        }
        if self.cleanup {
            Response::HookCleanupResult {
                outcomes: std::mem::take(&mut self.outcomes),
            }
        } else {
            Response::HookResult {
                proposals: std::mem::take(&mut self.proposals),
            }
        }
    }

    /// In a chained invocation a participant's result is the next
    /// participant's input. A value too large to deliver counts as that
    /// participant's failure, and the value it received stays in effect.
    fn adopt_chained_value(&mut self, result: Response) -> Response {
        match result {
            Response::Result { payload } if payload_fits(&payload) => {
                self.input = payload.clone();
                Response::Result { payload }
            }
            Response::Result { .. } => {
                Response::error("invalid_request", "chained hook value is too large")
            }
            failure => failure,
        }
    }

    fn cancel_active(&mut self, now: Instant, requests: &mut RequestRouter) {
        if let Some(participant) = self.active_participant.take() {
            requests.cancel(&self.owner, participant.request_id, now);
            requests.release(&self.owner, participant.request_id);
        }
    }
}

/// A required hook ends at the first participant that fails. Name it in the
/// message, so the owner can say who stopped the operation.
fn attributed(response: Response, participant: &str) -> Response {
    match response {
        Response::Error { code, message } => {
            Response::error(&code, format!("{participant}: {message}"))
        }
        other => other,
    }
}

pub(super) struct HookRegistry {
    specifications: BTreeMap<String, HookSpec>,
    registered_participants: BTreeSet<(String, String)>,
    invocations: BTreeMap<u64, HookInvocation>,
    operations: BTreeMap<(String, String), OperationIdentity>,
    next_invocation_id: u64,
}

impl HookRegistry {
    pub fn new(specifications: BTreeMap<String, HookSpec>) -> Self {
        Self {
            specifications,
            registered_participants: BTreeSet::new(),
            invocations: BTreeMap::new(),
            operations: BTreeMap::new(),
            next_invocation_id: 0,
        }
    }
    pub fn register(&mut self, caller: &str, grants: &Grants, hook_name: String) -> Response {
        if !grants.hook.contains(&hook_name)
            || !self.specifications.get(&hook_name).is_some_and(|hook| {
                hook.participants
                    .iter()
                    .any(|participant| participant == caller)
            })
        {
            return permission_denied();
        }
        self.registered_participants
            .insert((hook_name, caller.to_owned()));
        Response::Ok
    }
    pub fn begin(
        &mut self,
        caller: &str,
        hook_name: String,
        input: Value,
        now: Instant,
    ) -> Response {
        let mode = HookRunMode {
            cleanup: false,
            chain: false,
            transient: false,
        };
        self.begin_inner(caller, hook_name, input, mode, false, now)
    }
    pub fn begin_operation(
        &mut self,
        caller: &str,
        hook: String,
        input: Value,
        operation_id: String,
        mode: HookRunMode,
        now: Instant,
    ) -> Response {
        if !valid_name(&operation_id) {
            return Response::error("invalid_request", "invalid operation identifier");
        }
        let key = (caller.to_owned(), operation_id);
        if let Some(previous) = self.operations.get(&key) {
            if previous.hook != hook || previous.input != input || previous.mode != mode {
                return Response::error(
                    "conflict",
                    "operation identifier was already used for a different hook",
                );
            }
            return self.operation_response(previous.invocation);
        }
        if self.operations.len() >= MAX_OPERATION_IDENTITIES {
            return capacity_exhausted();
        }
        let response = self.begin_inner(caller, hook.clone(), input.clone(), mode, true, now);
        if let Response::Accepted { id } = response {
            self.operations.insert(
                key,
                OperationIdentity {
                    invocation: id,
                    hook,
                    input,
                    mode,
                },
            );
        }
        response
    }
    fn begin_inner(
        &mut self,
        caller: &str,
        hook_name: String,
        input: Value,
        mode: HookRunMode,
        retained: bool,
        now: Instant,
    ) -> Response {
        let HookRunMode { cleanup, chain, .. } = mode;
        let Some(specification) = self.specifications.get(&hook_name) else {
            return not_found();
        };
        if specification.owner != caller {
            return permission_denied();
        }
        if !payload_fits(&input) {
            return Response::error("invalid_request", "hook input is too large");
        }
        if self.invocations.len() >= MAX_INVOCATIONS {
            return capacity_exhausted();
        }
        if !cleanup
            && !specification.participants.iter().all(|participant| {
                self.registered_participants
                    .contains(&(hook_name.clone(), participant.clone()))
            })
        {
            return Response::error("unavailable", "a required hook participant is absent");
        }
        let Some(invocation_id) = self.next_invocation_id.checked_add(1) else {
            return capacity_exhausted();
        };
        self.next_invocation_id = invocation_id;
        self.invocations.insert(
            invocation_id,
            HookInvocation {
                owner: caller.to_owned(),
                hook_name,
                remaining_participants: specification.participants.clone().into(),
                input,
                deadline: now + Duration::from_millis(specification.timeout_ms),
                active_participant: None,
                proposals: Vec::new(),
                outcomes: Vec::new(),
                cleanup,
                chain,
                retained,
                terminal: None,
            },
        );
        Response::Accepted { id: invocation_id }
    }
    fn operation_response(&self, invocation: u64) -> Response {
        if self.invocations.contains_key(&invocation) {
            Response::Accepted { id: invocation }
        } else {
            Response::error(
                "operation_expired",
                "hook operation result is no longer retained",
            )
        }
    }
    pub fn lookup(&self, caller: &str, operation_id: &str) -> Response {
        match self
            .operations
            .get(&(caller.to_owned(), operation_id.to_owned()))
        {
            Some(identity) => self.operation_response(identity.invocation),
            None => not_found(),
        }
    }
    pub fn advance(
        &mut self,
        caller: &str,
        grants: &Grants,
        invocation_id: u64,
        now: Instant,
        requests: &mut RequestRouter,
    ) -> Response {
        let Some(invocation) = self.invocations.get_mut(&invocation_id) else {
            return not_found();
        };
        if invocation.owner != caller {
            return permission_denied();
        }
        if let Some((response, _)) = &invocation.terminal {
            return response.clone();
        }
        let response = invocation.advance(
            invocation_id,
            grants,
            now,
            requests,
            &self.registered_participants,
        );
        if !matches!(response, Response::Pending) {
            if invocation.retained {
                invocation.terminal = Some((response.clone(), now));
            } else {
                self.invocations.remove(&invocation_id);
            }
        }
        response
    }
    pub fn cancel(
        &mut self,
        caller: &str,
        invocation_id: u64,
        now: Instant,
        requests: &mut RequestRouter,
    ) -> Response {
        let Some(invocation) = self.invocations.get_mut(&invocation_id) else {
            return not_found();
        };
        if invocation.owner != caller {
            return permission_denied();
        }
        if invocation.terminal.is_none() {
            invocation.cancel_active(now, requests);
            invocation.terminal =
                Some((Response::error("cancelled", "hook cancelled by owner"), now));
        }
        Response::Ok
    }
    /// Discards a finished invocation. A transient operation gives its
    /// identifier up with it, which lets an extension point run more often
    /// than the fenced identities would allow.
    pub fn release(&mut self, caller: &str, invocation_id: u64) -> Response {
        let Some(invocation) = self.invocations.get(&invocation_id) else {
            return not_found();
        };
        if invocation.owner != caller {
            return permission_denied();
        }
        if invocation.terminal.is_none() {
            return Response::error("request_pending", "unfinished hooks cannot be released");
        }
        self.invocations.remove(&invocation_id);
        self.forget_transient_identities();
        Response::Ok
    }
    pub fn expire(&mut self, now: Instant) {
        let held = self.invocations.len();
        self.invocations
            .retain(|_, invocation| match &invocation.terminal {
                Some((_, completed)) => {
                    now.saturating_duration_since(*completed) < RESULT_RETENTION
                }
                None => now.saturating_duration_since(invocation.deadline) < RESULT_RETENTION,
            });
        // This runs on every dispatch, so scan identities only after a change.
        if self.invocations.len() != held {
            self.forget_transient_identities();
        }
    }

    /// A transient identifier protects a retry only while its invocation is
    /// held. Once that is released or has expired, the identifier is free, so
    /// an owner that never collected a run does not fence one for good.
    fn forget_transient_identities(&mut self) {
        let invocations = &self.invocations;
        self.operations.retain(|_, identity| {
            !identity.mode.transient || invocations.contains_key(&identity.invocation)
        });
    }
}
