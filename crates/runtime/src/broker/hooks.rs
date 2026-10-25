use amaterasu_contracts::{Grants, HookSpec, Proposal, Response};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use super::errors::{capacity_exhausted, not_found, payload_fits, permission_denied};
use super::requests::{RequestOptions, RequestRouter};

const MAX_INVOCATIONS: usize = 64;
const EXPIRED_INVOCATION_RETENTION: Duration = Duration::from_secs(300);

struct ActiveParticipant {
    name: String,
    request_id: u64,
}

struct HookInvocation {
    owner: String,
    hook_name: String,
    remaining_participants: VecDeque<String>,
    input: Value,
    deadline: Instant,
    active_participant: Option<ActiveParticipant>,
    proposals: Vec<Proposal>,
}

impl HookInvocation {
    fn collect_active_result(&mut self, requests: &mut RequestRouter) -> Result<bool, Response> {
        let Some(participant) = &self.active_participant else {
            return Ok(true);
        };

        match requests.result(&self.owner, participant.request_id) {
            Response::Pending => Ok(false),
            Response::Result { payload } => {
                requests.release(&self.owner, participant.request_id);
                self.proposals.push(Proposal {
                    participant: participant.name.clone(),
                    payload,
                });
                self.active_participant = None;
                Ok(true)
            }
            failure => {
                requests.release(&self.owner, participant.request_id);
                Err(failure)
            }
        }
    }

    fn start_next_participant(
        &mut self,
        invocation_id: u64,
        grants: &Grants,
        requests: &mut RequestRouter,
    ) -> Response {
        let Some(participant) = self.remaining_participants.pop_front() else {
            return Response::HookResult {
                proposals: std::mem::take(&mut self.proposals),
            };
        };

        let options = RequestOptions {
            service: format!("hooks/{}", self.hook_name),
            payload: self.input.clone(),
            timeout_ms: 0, // Hook requests use the invocation's absolute deadline.
            operation_id: format!("hook.{invocation_id}.{participant}"),
            parent: None,
            response_schema: None,
        };
        let response = requests.submit_hook(
            &self.owner,
            grants,
            participant.clone(),
            options,
            self.deadline,
        );

        if let Response::Accepted { id: request_id } = response {
            self.active_participant = Some(ActiveParticipant {
                name: participant,
                request_id,
            });
            Response::Pending
        } else {
            response
        }
    }
}

pub(super) struct HookRegistry {
    specifications: BTreeMap<String, HookSpec>,
    registered_participants: BTreeSet<(String, String)>,
    invocations: BTreeMap<u64, HookInvocation>,
    next_invocation_id: u64,
}

impl HookRegistry {
    pub fn new(specifications: BTreeMap<String, HookSpec>) -> Self {
        Self {
            specifications,
            registered_participants: BTreeSet::new(),
            invocations: BTreeMap::new(),
            next_invocation_id: 0,
        }
    }

    pub fn register(&mut self, caller: &str, grants: &Grants, hook_name: String) -> Response {
        if !grants.hook.contains(&hook_name) {
            return permission_denied();
        }

        let listed = self.specifications.get(&hook_name).is_some_and(|hook| {
            hook.participants
                .iter()
                .any(|participant| participant == caller)
        });
        if !listed {
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
        if !self.all_participants_registered(&hook_name, specification) {
            return Response::error("unavailable", "a required hook participant is absent");
        }

        self.next_invocation_id += 1;
        let invocation_id = self.next_invocation_id;
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
            },
        );
        Response::Accepted { id: invocation_id }
    }

    fn all_participants_registered(&self, hook_name: &str, specification: &HookSpec) -> bool {
        specification.participants.iter().all(|participant| {
            self.registered_participants
                .contains(&(hook_name.to_owned(), participant.clone()))
        })
    }

    pub fn advance(
        &mut self,
        caller: &str,
        grants: &Grants,
        invocation_id: u64,
        now: Instant,
        requests: &mut RequestRouter,
    ) -> Response {
        let Some(invocation) = self.invocations.get(&invocation_id) else {
            return not_found();
        };
        if invocation.owner != caller {
            return permission_denied();
        }

        let mut invocation = self
            .invocations
            .remove(&invocation_id)
            .expect("checked above");
        let response = advance_invocation(&mut invocation, invocation_id, grants, now, requests);
        if matches!(response, Response::Pending) {
            self.invocations.insert(invocation_id, invocation);
        }
        response
    }

    pub fn expire(&mut self, now: Instant) {
        self.invocations.retain(|_, invocation| {
            now.saturating_duration_since(invocation.deadline) < EXPIRED_INVOCATION_RETENTION
        });
    }
}

fn advance_invocation(
    invocation: &mut HookInvocation,
    invocation_id: u64,
    grants: &Grants,
    now: Instant,
    requests: &mut RequestRouter,
) -> Response {
    if now >= invocation.deadline {
        if let Some(participant) = invocation.active_participant.take() {
            requests.cancel(&invocation.owner, participant.request_id, now);
            requests.release(&invocation.owner, participant.request_id);
        }
        return Response::error("deadline_exceeded", "hook failed closed");
    }

    match invocation.collect_active_result(requests) {
        Ok(false) => Response::Pending,
        Ok(true) => invocation.start_next_participant(invocation_id, grants, requests),
        Err(failure) => failure,
    }
}
