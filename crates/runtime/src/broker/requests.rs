use amaterasu_contracts::{ApiSchema, Grants, Response, valid_name};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use super::errors::{capacity_exhausted, not_found, payload_fits, permission_denied};

const MAX_RETAINED_REQUESTS: usize = 512;
const RESULT_RETENTION: Duration = Duration::from_secs(300);

pub(super) struct RequestOptions {
    pub service: String,
    pub payload: Value,
    pub timeout_ms: u64,
    pub operation_id: String,
    pub parent: Option<u64>,
    pub response_schema: Option<ApiSchema>,
}

struct CallAuthority {
    initiator: String,
    allowed_services: BTreeSet<String>,
    deadline: Instant,
}

struct RequestRecord {
    caller: String,
    provider: String,
    options: RequestOptions,
    authority: CallAuthority,
    completed_at: Option<Instant>,
    result: Option<Response>,
    delivered: bool,
    separate_retry: bool,
}

impl RequestRecord {
    fn uncertain_state_mutation(&self) -> bool {
        self.delivered
            && self.options.service == "amaterasu.state/1"
            && self.options.payload.get("action").and_then(Value::as_str) != Some("get")
    }
    fn finish(&mut self, result: Response, now: Instant) {
        self.result = Some(result);
        self.completed_at = Some(now);
    }

    fn matches_retry(&self, options: &RequestOptions) -> bool {
        self.options.service == options.service
            && self.options.payload == options.payload
            && self.options.parent == options.parent
    }
}

#[derive(Default)]
pub(super) struct RequestRouter {
    providers: BTreeMap<String, String>,
    requests: BTreeMap<u64, RequestRecord>,
    provider_queues: BTreeMap<String, VecDeque<u64>>,
    next_request_id: u64,
    state_revisions: BTreeMap<String, u64>,
    uncertain_state_writers: BTreeSet<String>,
}

impl RequestRouter {
    pub fn retained_state_value_contains(
        &self,
        caller: &str,
        provider: &str,
        key: &str,
        revision: u64,
        expected: &Value,
    ) -> bool {
        self.requests.keys().any(|id| {
            let Some((request, result)) = self.selected_state_read(caller, provider, *id) else {
                return false;
            };
            request.get("key").and_then(Value::as_str) == Some(key)
                && result.get("revision").and_then(Value::as_u64) == Some(revision)
                && result.get("ok").and_then(Value::as_bool) == Some(true)
                && result.get("found").and_then(Value::as_bool) == Some(true)
                && result.pointer("/value/version").and_then(Value::as_u64) == Some(1)
                && result
                    .pointer("/value/receipts")
                    .and_then(Value::as_array)
                    .is_some_and(|items| items.len() <= 32 && items.contains(expected))
        })
    }
    /// Read only a delivered terminal State Get owned by the exact authority
    /// and supplied by the trusted selected State provider. Retention and
    /// authenticated request metadata prevent callers substituting JSON.
    pub fn selected_state_read(
        &self,
        authority: &str,
        provider: &str,
        request_id: u64,
    ) -> Option<(&Value, &Value)> {
        let request = self.requests.get(&request_id)?;
        if request.caller != authority
            || request.authority.initiator != authority
            || request.provider != provider
            || request.options.service != "amaterasu.state/1"
            || request.options.parent.is_some()
            || !request.delivered
            || request.options.payload.get("action")?.as_str()? != "get"
        {
            return None;
        }
        match request.result.as_ref()? {
            Response::Result { payload } => {
                let revision = payload.get("revision")?.as_u64()?;
                if self.uncertain_state_writers.contains(authority)
                    || self.state_revisions.get(authority) != Some(&revision)
                    || self.requests.values().any(|other| {
                        other.caller == authority
                            && other.options.service == "amaterasu.state/1"
                            && other.result.is_none()
                            && other.options.payload.get("action").and_then(Value::as_str)
                                != Some("get")
                    })
                {
                    return None;
                }
                Some((&request.options.payload, payload))
            }
            _ => None,
        }
    }

    /// The initiator is inherited from a broker-owned request, never supplied
    /// as a module-controlled identity field in the command payload.
    pub fn initiator_for_parent(&self, caller: &str, parent_id: u64) -> Result<&str, Response> {
        let parent = self
            .requests
            .get(&parent_id)
            .ok_or_else(permission_denied)?;
        if parent.provider != caller {
            return Err(permission_denied());
        }
        Ok(&parent.authority.initiator)
    }

    pub fn discovery_authority(
        &self,
        caller: &str,
        parent_id: u64,
    ) -> Result<(String, BTreeSet<String>), Response> {
        let parent = self
            .requests
            .get(&parent_id)
            .ok_or_else(permission_denied)?;
        if parent.provider != caller || !parent.delivered || parent.result.is_some() {
            return Err(permission_denied());
        }
        Ok((
            parent.authority.initiator.clone(),
            parent.authority.allowed_services.clone(),
        ))
    }

    pub fn register_service(&mut self, caller: &str, grants: &Grants, service: String) -> Response {
        if !grants.provide.contains(&service) {
            return permission_denied();
        }

        if let Some(provider) = self.providers.get(&service) {
            if provider == caller {
                return Response::Ok;
            }
            return Response::error(
                "conflict",
                "a provider is already selected for this service",
            );
        }

        self.providers.insert(service, caller.to_owned());
        Response::Ok
    }

    pub fn submit(
        &mut self,
        caller: &str,
        grants: &Grants,
        options: RequestOptions,
        now: Instant,
    ) -> Response {
        self.submit_inner(caller, grants, options, now, false)
    }

    /// The protected-effect ledger owns retry identity and retains it beyond
    /// ordinary broker result eviction. Never merge these calls by operation ID.
    pub fn submit_guarded(
        &mut self,
        caller: &str,
        grants: &Grants,
        options: RequestOptions,
        now: Instant,
    ) -> Response {
        self.submit_inner(caller, grants, options, now, true)
    }

    fn submit_inner(
        &mut self,
        caller: &str,
        grants: &Grants,
        options: RequestOptions,
        now: Instant,
        separate_retry: bool,
    ) -> Response {
        if !grants.call.contains(&options.service) {
            return permission_denied();
        }
        if let Err(error) = validate_options(&options) {
            return error;
        }

        // A retry observes an already authorized request, even after its parent ends.
        if !separate_retry && let Some(previous_response) = self.find_retry(caller, &options) {
            return previous_response;
        }
        let authority = match self.resolve_authority(caller, grants, &options, now) {
            Ok(authority) => authority,
            Err(error) => return error,
        };

        let Some(provider) = self.providers.get(&options.service).cloned() else {
            return Response::error("unavailable", "service is not registered");
        };
        self.enqueue(caller, provider, options, authority, separate_retry)
    }

    fn resolve_authority(
        &self,
        caller: &str,
        grants: &Grants,
        options: &RequestOptions,
        now: Instant,
    ) -> Result<CallAuthority, Response> {
        let requested_deadline = now + Duration::from_millis(options.timeout_ms);
        let Some(parent_id) = options.parent else {
            return Ok(CallAuthority {
                initiator: caller.to_owned(),
                allowed_services: grants.call.clone(),
                deadline: requested_deadline,
            });
        };

        let parent = self
            .requests
            .get(&parent_id)
            .ok_or_else(permission_denied)?;
        if parent.provider != caller
            || !parent.delivered
            || parent.result.is_some()
            || !parent.authority.allowed_services.contains(&options.service)
        {
            return Err(permission_denied());
        }

        let allowed_services = grants
            .call
            .intersection(&parent.authority.allowed_services)
            .cloned()
            .collect();

        Ok(CallAuthority {
            initiator: parent.authority.initiator.clone(),
            allowed_services,
            deadline: requested_deadline.min(parent.authority.deadline),
        })
    }

    fn find_retry(&self, caller: &str, options: &RequestOptions) -> Option<Response> {
        let previous = self.requests.iter().find(|(_, request)| {
            !request.separate_retry
                && request.caller == caller
                && request.options.operation_id == options.operation_id
        });
        let (request_id, request) = previous?;

        if request.matches_retry(options) {
            Some(Response::Accepted { id: *request_id })
        } else {
            Some(Response::error(
                "conflict",
                "operation identifier was already used for a different request",
            ))
        }
    }

    fn enqueue(
        &mut self,
        caller: &str,
        provider: String,
        options: RequestOptions,
        authority: CallAuthority,
        separate_retry: bool,
    ) -> Response {
        if self.requests.len() >= MAX_RETAINED_REQUESTS {
            return capacity_exhausted();
        }

        let Some(request_id) = self.next_request_id.checked_add(1) else {
            return capacity_exhausted();
        };
        self.next_request_id = request_id;
        self.provider_queues
            .entry(provider.clone())
            .or_default()
            .push_back(request_id);
        self.requests.insert(
            request_id,
            RequestRecord {
                caller: caller.to_owned(),
                provider,
                options,
                authority,
                completed_at: None,
                result: None,
                delivered: false,
                separate_retry,
            },
        );

        Response::Accepted { id: request_id }
    }

    pub fn poll(&mut self, provider: &str, now: Instant) -> Response {
        let queue = self.provider_queues.entry(provider.to_owned()).or_default();

        while let Some(request_id) = queue.pop_front() {
            let Some(request) = self.requests.get_mut(&request_id) else {
                continue;
            };
            if request.result.is_some() {
                continue;
            }

            request.delivered = true;
            return Response::Request {
                id: request_id,
                caller: request.caller.clone(),
                initiator: request.authority.initiator.clone(),
                service: request.options.service.clone(),
                payload: request.options.payload.clone(),
                remaining_ms: request
                    .authority
                    .deadline
                    .saturating_duration_since(now)
                    .as_millis() as u64,
            };
        }

        Response::Pending
    }

    pub fn complete(
        &mut self,
        provider: &str,
        request_id: u64,
        payload: Value,
        now: Instant,
    ) -> Response {
        let Some(request) = self.requests.get_mut(&request_id) else {
            return not_found();
        };
        if request.provider != provider || !request.delivered {
            return permission_denied();
        }
        if request.result.is_some() {
            return Response::error(
                "request_ended",
                "completion arrived after the request ended",
            );
        }
        if !payload_fits(&payload) {
            return Response::error("invalid_request", "result payload is too large");
        }
        if request
            .options
            .response_schema
            .as_ref()
            .is_some_and(|schema| !super::api::payload_matches(schema, &payload))
        {
            return Response::error(
                "invalid_request",
                "API result does not match its installed schema",
            );
        }

        let state_revision = (request.options.service == "amaterasu.state/1"
            && matches!(
                request
                    .options
                    .payload
                    .get("action")
                    .and_then(Value::as_str),
                Some("get" | "set" | "delete")
            )
            && payload.get("ok").and_then(Value::as_bool) == Some(true))
        .then(|| payload.get("revision").and_then(Value::as_u64))
        .flatten()
        .map(|revision| (request.caller.clone(), revision));
        if request.uncertain_state_mutation() && state_revision.is_none() {
            self.uncertain_state_writers.insert(request.caller.clone());
        }
        request.finish(Response::Result { payload }, now);
        if let Some((caller, revision)) = state_revision {
            let observed = self.state_revisions.entry(caller).or_default();
            *observed = (*observed).max(revision);
        }
        Response::Ok
    }

    pub fn check_active(&self, provider: &str, request_id: u64) -> Response {
        let Some(request) = self.requests.get(&request_id) else {
            return not_found();
        };
        if request.provider != provider || !request.delivered {
            return permission_denied();
        }
        if request.result.is_some() {
            return Response::error("request_ended", "request is no longer active");
        }
        Response::Ok
    }

    /// A protected retry may report an existing admission only while the
    /// ordinary request record still exists; it must never enqueue a second call.
    pub fn retained(&self, caller: &str, request_id: u64) -> bool {
        self.requests
            .get(&request_id)
            .is_some_and(|request| request.caller == caller)
    }

    pub fn result(&self, caller: &str, request_id: u64) -> Response {
        let Some(request) = self.requests.get(&request_id) else {
            return not_found();
        };
        if request.caller != caller {
            return permission_denied();
        }
        request.result.clone().unwrap_or(Response::Pending)
    }

    pub fn cancel(&mut self, caller: &str, request_id: u64, now: Instant) -> Response {
        let Some(request) = self.requests.get_mut(&request_id) else {
            return not_found();
        };
        if request.caller != caller {
            return permission_denied();
        }
        if request.result.is_none() {
            if request.uncertain_state_mutation() {
                self.uncertain_state_writers.insert(request.caller.clone());
            }
            request.finish(
                Response::error("cancelled", "cancelled by the request owner"),
                now,
            );
        }
        Response::Ok
    }

    /// Explicitly relinquishes both the completed payload and retry record.
    /// Active calls must be completed or cancelled before their owner releases them.
    pub fn release(&mut self, caller: &str, request_id: u64) -> Response {
        let Some(request) = self.requests.get(&request_id) else {
            return not_found();
        };
        if request.caller != caller {
            return permission_denied();
        }
        if request.result.is_none() {
            return Response::error("request_pending", "unfinished requests cannot be released");
        }
        let provider = request.provider.clone();
        self.requests.remove(&request_id);
        if let Some(queue) = self.provider_queues.get_mut(&provider) {
            queue.retain(|queued| *queued != request_id);
        }
        Response::Ok
    }

    pub fn expire(&mut self, now: Instant) {
        self.finish_expired_requests(now);
        self.cancel_orphaned_delegations(now);
        self.remove_expired_results(now);
    }

    fn finish_expired_requests(&mut self, now: Instant) {
        for request in self.requests.values_mut() {
            if request.result.is_none() && now >= request.authority.deadline {
                if request.uncertain_state_mutation() {
                    self.uncertain_state_writers.insert(request.caller.clone());
                }
                request.finish(
                    Response::error("deadline_exceeded", "request deadline elapsed"),
                    now,
                );
            }
        }
    }

    fn cancel_orphaned_delegations(&mut self, now: Instant) {
        // A parent that ended can no longer sponsor unfinished delegated work.
        // Repeat because cancellation may make grandchildren into orphans too.
        loop {
            let finished: BTreeSet<_> = self
                .requests
                .iter()
                .filter(|(_, request)| request.result.is_some())
                .map(|(request_id, _)| *request_id)
                .collect();

            let mut cancelled_any = false;
            for request in self.requests.values_mut() {
                let parent_ended = request
                    .options
                    .parent
                    .is_some_and(|id| finished.contains(&id));
                if request.result.is_none() && parent_ended {
                    if request.uncertain_state_mutation() {
                        self.uncertain_state_writers.insert(request.caller.clone());
                    }
                    request.finish(Response::error("cancelled", "parent request ended"), now);
                    cancelled_any = true;
                }
            }

            if !cancelled_any {
                break;
            }
        }
    }

    fn remove_expired_results(&mut self, now: Instant) {
        self.requests.retain(|_, request| {
            request
                .completed_at
                .is_none_or(|completed| now.saturating_duration_since(completed) < RESULT_RETENTION)
        });

        let retained_ids: BTreeSet<_> = self.requests.keys().copied().collect();
        for queue in self.provider_queues.values_mut() {
            queue.retain(|id| retained_ids.contains(id));
        }
    }

    pub fn submit_hook(
        &mut self,
        owner: &str,
        grants: &Grants,
        participant: String,
        options: RequestOptions,
        deadline: Instant,
    ) -> Response {
        let authority = CallAuthority {
            initiator: owner.to_owned(),
            allowed_services: grants.call.clone(),
            deadline,
        };
        self.enqueue(owner, participant, options, authority, true)
    }
}

fn validate_options(options: &RequestOptions) -> Result<(), Response> {
    if !(1..=30_000).contains(&options.timeout_ms) {
        return Err(Response::error(
            "invalid_request",
            "deadline must be 1 through 30000 milliseconds",
        ));
    }
    if !valid_name(&options.operation_id) {
        return Err(Response::error(
            "invalid_request",
            "invalid operation identifier",
        ));
    }
    if !payload_fits(&options.payload) {
        return Err(Response::error(
            "invalid_request",
            "request payload is too large",
        ));
    }
    Ok(())
}
