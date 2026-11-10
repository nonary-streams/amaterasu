mod timing;
use amaterasu_contracts::{ApiSchema, Grants, REFUSED_PREFIX, Response, valid_name};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};
use timing::RequestTiming;

use super::errors::{capacity_exhausted, not_found, payload_fits, permission_denied, result_fits};
use super::layers::{NextHop, ServiceLayers};

const MAX_RETAINED_REQUESTS: usize = 512;
const RESULT_RETENTION: Duration = Duration::from_secs(300);
const MAX_REFUSAL_CODE_BYTES: usize = 64;
const MAX_REFUSAL_MESSAGE_BYTES: usize = 512;

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

/// Who serves an admitted request and whose identity it is shown.
struct Delivery {
    provider: String,
    /// The original caller a layer forwards for. `None` shows the requester.
    on_behalf_of: Option<String>,
    internal_hook: bool,
}

struct RequestRecord {
    /// Owns the result: only this principal may read, cancel or release it.
    caller: String,
    on_behalf_of: Option<String>,
    provider: String,
    options: RequestOptions,
    authority: CallAuthority,
    enqueued_at: Instant,
    accepted_at: Option<Instant>,
    result_observed: bool,
    completed_at: Option<Instant>,
    result: Option<Response>,
    delivered: bool,
    accepted_receiver: Option<String>,
    internal_hook: bool,
}

impl RequestRecord {
    /// The identity the serving principal authorizes. A layer's forwarded
    /// call keeps the caller of the request it forwards.
    fn presented_caller(&self) -> &str {
        self.on_behalf_of.as_deref().unwrap_or(&self.caller)
    }

    fn offer_response(&self, id: u64, now: Instant) -> Response {
        Response::Request {
            id,
            caller: self.presented_caller().to_owned(),
            initiator: self.authority.initiator.clone(),
            service: self.options.service.clone(),
            payload: self.options.payload.clone(),
            remaining_ms: self
                .authority
                .deadline
                .saturating_duration_since(now)
                .as_millis() as u64,
        }
    }
    fn finish(&mut self, result: Response, now: Instant) {
        self.result = Some(result);
        self.completed_at = Some(now);
    }

    fn matches_retry(&self, options: &RequestOptions) -> bool {
        self.options.service == options.service
            && self.options.payload == options.payload
            && self.options.parent == options.parent
            && self.options.response_schema == options.response_schema
    }
}

#[derive(Default)]
pub(super) struct RequestRouter {
    providers: BTreeMap<String, String>,
    layers: ServiceLayers,
    requests: BTreeMap<u64, RequestRecord>,
    provider_queues: BTreeMap<String, VecDeque<u64>>,
    delivery_modes: BTreeMap<String, bool>,
    offers: BTreeMap<String, (u64, String)>,
    unretired: BTreeMap<u64, (String, String)>,
    legacy_tainted: BTreeSet<String>,
    next_request_id: u64,
    last_capacity_report: Option<Instant>,
    timing: RequestTiming,
}

impl RequestRouter {
    pub fn with_layers(layers: BTreeMap<String, Vec<String>>) -> Self {
        Self {
            layers: ServiceLayers::new(layers),
            ..Self::default()
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

    /// Recover only the authority of this caller's retained operation when its
    /// parent has already been released. Submission still checks the complete
    /// request against the retained operation before returning its identifier.
    pub fn initiator_for_retry(&self, caller: &str, operation: &str, parent: u64) -> Option<&str> {
        self.requests.values().find_map(|request| {
            (!request.internal_hook
                && request.caller == caller
                && request.options.operation_id == operation
                && request.options.parent == Some(parent))
            .then_some(request.authority.initiator.as_str())
        })
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

    /// The principal that serves `caller`'s call to `service`: the next layer
    /// stacked on the service, or its provider. The provider must have
    /// registered even when a layer answers first, so a call is refused at
    /// admission instead of failing halfway down the stack.
    pub fn route(&self, caller: &str, service: &str) -> Result<String, Response> {
        let next = self.layers.next_hop(service, caller)?;
        let provider = self
            .providers
            .get(service)
            .ok_or_else(|| Response::error("unavailable", "service is not registered"))?;
        Ok(match next {
            NextHop::Layer(layer) => layer.to_owned(),
            NextHop::Provider => provider.clone(),
        })
    }

    pub fn is_layer(&self, service: &str, principal: &str) -> bool {
        self.layers.is_layer(service, principal)
    }

    /// Registers `caller` as a layer of `service` when trusted composition
    /// lists it as one. The listing is the authority; no `provide` grant applies.
    pub fn register_layer(&mut self, caller: &str, service: &str) -> bool {
        self.layers.register(service, caller)
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
        if !grants.call.contains(&options.service) {
            return permission_denied();
        }
        if let Err(error) = validate_options(&options) {
            return error;
        }

        // A retry observes an already authorized request, even after its parent ends.
        if let Some(previous_response) = self.find_retry(caller, &options) {
            return previous_response;
        }
        let authority = match self.resolve_authority(caller, grants, &options, now) {
            Ok(authority) => authority,
            Err(error) => return error,
        };

        let provider = match self.route(caller, &options.service) {
            Ok(provider) => provider,
            Err(error) => return error,
        };
        let delivery = Delivery {
            provider,
            on_behalf_of: self.forwarded_caller(caller, &options),
            internal_hook: false,
        };
        self.enqueue(caller, delivery, options, authority, now)
    }

    /// A layer that forwards a request it received for the wrapped service
    /// keeps that request's caller, so the next hop authorizes the consumer
    /// and not the layer. The identity comes from the broker's own record of
    /// the parent; a call the layer makes on its own behalf shows the layer.
    fn forwarded_caller(&self, caller: &str, options: &RequestOptions) -> Option<String> {
        let parent = self.requests.get(&options.parent?)?;
        self.forwards(caller, options, parent)
            .then(|| parent.presented_caller().to_owned())
    }

    /// Whether `caller` is a layer passing on `parent`, a request it received
    /// for the service it now calls.
    fn forwards(&self, caller: &str, options: &RequestOptions, parent: &RequestRecord) -> bool {
        self.layers.is_layer(&options.service, caller)
            && parent.provider == caller
            && parent.options.service == options.service
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

        // A layer passing a request on is transparent: the next hop may
        // delegate exactly what it could for the consumer's own request. Any
        // other delegated call is narrowed by the delegating caller's grants.
        let allowed_services = if self.forwards(caller, options, parent) {
            parent.authority.allowed_services.clone()
        } else {
            grants
                .call
                .intersection(&parent.authority.allowed_services)
                .cloned()
                .collect()
        };

        Ok(CallAuthority {
            initiator: parent.authority.initiator.clone(),
            allowed_services,
            deadline: requested_deadline.min(parent.authority.deadline),
        })
    }

    fn find_retry(&self, caller: &str, options: &RequestOptions) -> Option<Response> {
        let previous = self.requests.iter().find(|(_, request)| {
            !request.internal_hook
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

    /// Inspect a request without admitting a replacement operation.
    pub fn lookup(&self, caller: &str, grants: &Grants, options: &RequestOptions) -> Response {
        if !grants.call.contains(&options.service) {
            return permission_denied();
        }
        if !valid_name(&options.operation_id) || !payload_fits(&options.payload) {
            return Response::error("invalid_request", "invalid lookup operation or payload");
        }
        let Some((id, request)) = self.requests.iter().find(|(_, request)| {
            !request.internal_hook
                && request.caller == caller
                && request.options.operation_id == options.operation_id
        }) else {
            return not_found();
        };
        if request.matches_retry(options) {
            Response::Accepted { id: *id }
        } else {
            Response::error(
                "conflict",
                "operation identifier was already used for a different request",
            )
        }
    }

    fn enqueue(
        &mut self,
        caller: &str,
        delivery: Delivery,
        options: RequestOptions,
        authority: CallAuthority,
        now: Instant,
    ) -> Response {
        if self.requests.len() >= MAX_RETAINED_REQUESTS {
            let now = Instant::now();
            if self
                .last_capacity_report
                .is_none_or(|previous| now.duration_since(previous) >= Duration::from_secs(5))
            {
                let mut counts = BTreeMap::new();
                for record in self.requests.values() {
                    *counts
                        .entry((
                            &record.caller,
                            &record.options.service,
                            record.result.is_some(),
                        ))
                        .or_insert(0_u32) += 1;
                }
                eprintln!(
                    "runtime request capacity reached caller={caller} retained_by_caller_service_completed={counts:?}"
                );
                self.last_capacity_report = Some(now);
            }
            return capacity_exhausted();
        }

        let Some(request_id) = self.next_request_id.checked_add(1) else {
            return capacity_exhausted();
        };
        self.next_request_id = request_id;
        let Delivery {
            provider,
            on_behalf_of,
            internal_hook,
        } = delivery;
        self.provider_queues
            .entry(provider.clone())
            .or_default()
            .push_back(request_id);
        self.requests.insert(
            request_id,
            RequestRecord {
                caller: caller.to_owned(),
                on_behalf_of,
                provider,
                options,
                authority,
                enqueued_at: now,
                accepted_at: None,
                result_observed: false,
                completed_at: None,
                result: None,
                delivered: false,
                accepted_receiver: None,
                internal_hook,
            },
        );

        let record = &self.requests[&request_id];
        self.timing.started.get_or_insert(now);
        self.timing.service(record);
        Response::Accepted { id: request_id }
    }

    pub fn select_delivery_mode(&mut self, provider: &str, reliable: bool) -> Response {
        if self.delivery_modes.get(provider) == Some(&reliable) {
            return Response::Ok;
        }
        if self.offers.contains_key(provider)
            || self.unretired.values().any(|(owner, _)| owner == provider)
            || self.legacy_tainted.contains(provider)
            || self.requests.values().any(|request| {
                request.provider == provider && request.delivered && request.result.is_none()
            })
        {
            return Response::error("busy", "request delivery is still in flight");
        }
        self.delivery_modes.insert(provider.to_owned(), reliable);
        Response::Ok
    }

    pub fn poll(&mut self, provider: &str, now: Instant, receiver: Option<&str>) -> Response {
        let reliable = receiver.is_some();
        if receiver.is_some_and(|receiver| !valid_name(receiver)) {
            return Response::error("invalid_request", "invalid receiver identifier");
        }
        if let Some(receiver) = receiver
            && self
                .unretired
                .values()
                .any(|(owner, previous)| owner == provider && previous != receiver)
        {
            return Response::error("busy", "another receiver has unretired deliveries");
        }
        match self.delivery_modes.get(provider) {
            Some(selected) if *selected != reliable => {
                return Response::error("delivery_mode_mismatch", "request delivery mode differs");
            }
            None => {
                self.delivery_modes.insert(provider.to_owned(), reliable);
            }
            _ => {}
        }
        let queue = self.provider_queues.entry(provider.to_owned()).or_default();

        while let Some(request_id) = queue.front().copied() {
            let Some(request) = self.requests.get_mut(&request_id) else {
                queue.pop_front();
                continue;
            };
            if request.result.is_some() {
                queue.pop_front();
                self.offers.remove(provider);
                continue;
            }
            if let Some(receiver) = receiver {
                if self
                    .offers
                    .get(provider)
                    .is_some_and(|(id, owner)| *id != request_id || owner != receiver)
                {
                    return Response::error("busy", "another receiver owns the offer");
                }
                self.offers
                    .insert(provider.to_owned(), (request_id, receiver.to_owned()));
            } else {
                queue.pop_front();
                request.delivered = true;
                request.accepted_at = Some(now);
                self.timing.accepted(request, now);
            }
            return request.offer_response(request_id, now);
        }

        Response::Pending
    }

    pub fn inspect_offer(&self, provider: &str, receiver: &str, now: Instant) -> Response {
        if !valid_name(receiver) {
            return Response::error("invalid_request", "invalid receiver identifier");
        }
        match self.delivery_modes.get(provider) {
            None => return Response::Pending,
            Some(false) => {
                return Response::error(
                    "delivery_mode_mismatch",
                    "reliable delivery is not selected",
                );
            }
            Some(true) => {}
        }
        match self.offers.get(provider) {
            Some((id, owner)) if owner == receiver => self
                .requests
                .get(id)
                .filter(|request| request.result.is_none() && !request.delivered)
                .map_or(Response::Pending, |request| {
                    request.offer_response(*id, now)
                }),
            _ => Response::Pending,
        }
    }

    pub fn accept_at(&mut self, provider: &str, id: u64, receiver: &str, now: Instant) -> Response {
        if !valid_name(receiver) {
            return Response::error("invalid_request", "invalid receiver identifier");
        }
        if self.delivery_modes.get(provider) != Some(&true) {
            return Response::error(
                "delivery_mode_mismatch",
                "reliable delivery is not selected",
            );
        }
        let Some(request) = self.requests.get_mut(&id) else {
            return not_found();
        };
        if request.provider != provider {
            return permission_denied();
        }
        if request.result.is_some() {
            return Response::error("request_ended", "request is no longer active");
        }
        if request.delivered {
            return if request.accepted_receiver.as_deref() == Some(receiver) {
                Response::Ok
            } else {
                permission_denied()
            };
        }
        if !self
            .offers
            .get(provider)
            .is_some_and(|(offered, owner)| *offered == id && owner == receiver)
        {
            return permission_denied();
        }
        if self.unretired.len() >= MAX_RETAINED_REQUESTS {
            return capacity_exhausted();
        }
        request.delivered = true;
        request.accepted_receiver = Some(receiver.to_owned());
        request.accepted_at = Some(now);
        self.timing.accepted(request, now);
        self.unretired
            .insert(id, (provider.to_owned(), receiver.to_owned()));
        self.offers.remove(provider);
        if let Some(queue) = self.provider_queues.get_mut(provider)
            && queue.front() == Some(&id)
        {
            queue.pop_front();
        }
        Response::Ok
    }

    /// A receiver may retire only after its handler has stopped and the caller
    /// result is terminal. Entries survive ordinary result eviction.
    pub fn retire_delivery(&mut self, provider: &str, id: u64, receiver: &str) -> Response {
        let Some((owner, recorded_receiver)) = self.unretired.get(&id) else {
            return not_found();
        };
        if owner != provider || recorded_receiver != receiver {
            return permission_denied();
        }
        if self
            .requests
            .get(&id)
            .is_some_and(|request| request.result.is_none())
        {
            return Response::error("request_pending", "request has not ended");
        }
        self.unretired.remove(&id);
        Response::Ok
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
        if !result_fits(&payload) {
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

        request.finish(Response::Result { payload }, now);
        self.timing.finished(request, now);
        Response::Ok
    }

    /// Ends a delivered request with the provider's statement that it did not
    /// perform the operation. The requester reads `refused.<code>`, which no
    /// broker-issued code can be mistaken for.
    pub fn refuse(
        &mut self,
        provider: &str,
        request_id: u64,
        code: &str,
        message: String,
        now: Instant,
    ) -> Response {
        let Some(request) = self.requests.get_mut(&request_id) else {
            return not_found();
        };
        if request.provider != provider || !request.delivered {
            return permission_denied();
        }
        if request.result.is_some() {
            return Response::error("request_ended", "refusal arrived after the request ended");
        }
        if !valid_refusal(code, &message) {
            return Response::error("invalid_request", "invalid refusal code or message");
        }

        request.finish(
            Response::error(&format!("{REFUSED_PREFIX}{code}"), message),
            now,
        );
        self.timing.finished(request, now);
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

    pub fn result(&mut self, caller: &str, request_id: u64) -> Response {
        self.result_at(caller, request_id, Instant::now())
    }

    pub fn result_at(&mut self, caller: &str, request_id: u64, now: Instant) -> Response {
        let Some(request) = self.requests.get_mut(&request_id) else {
            return not_found();
        };
        if request.caller != caller {
            return permission_denied();
        }
        if request.result.is_some() && !request.result_observed {
            self.timing.observed(request, now);
            request.result_observed = true;
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
            if request.delivered && request.accepted_receiver.is_none() {
                self.legacy_tainted.insert(request.provider.clone());
            }
            request.finish(
                Response::error("cancelled", "cancelled by the request owner"),
                now,
            );
            self.timing.finished(request, now);
        }
        // Hook cancellation releases its participant immediately. End delegated
        // work while the parent's terminal record still identifies its children.
        self.cancel_orphaned_delegations(now);
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
        if self
            .offers
            .get(&provider)
            .is_some_and(|(offered, _)| *offered == request_id)
        {
            self.offers.remove(&provider);
        }
        Response::Ok
    }

    pub fn expire(&mut self, now: Instant) {
        self.finish_expired_requests(now);
        self.cancel_orphaned_delegations(now);
        self.remove_expired_results(now);
        self.offers.retain(|_, (id, _)| {
            self.requests
                .get(id)
                .is_some_and(|request| request.result.is_none())
        });
    }

    fn finish_expired_requests(&mut self, now: Instant) {
        for request in self.requests.values_mut() {
            if request.result.is_none() && now >= request.authority.deadline {
                if request.delivered && request.accepted_receiver.is_none() {
                    self.legacy_tainted.insert(request.provider.clone());
                }
                request.finish(
                    Response::error("deadline_exceeded", "request deadline elapsed"),
                    now,
                );
                self.timing.finished(request, now);
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
                    if request.delivered && request.accepted_receiver.is_none() {
                        self.legacy_tainted.insert(request.provider.clone());
                    }
                    request.finish(Response::error("cancelled", "parent request ended"), now);
                    self.timing.finished(request, now);
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
        self.offers.retain(|_, (id, _)| retained_ids.contains(id));
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
        let delivery = Delivery {
            provider: participant,
            on_behalf_of: None,
            internal_hook: true,
        };
        self.enqueue(owner, delivery, options, authority, Instant::now())
    }
}

/// A refusal carries a short machine-readable code and one bounded line of
/// text. `refused.` is the broker's own prefix and cannot be nested.
fn valid_refusal(code: &str, message: &str) -> bool {
    valid_name(code)
        && code.len() <= MAX_REFUSAL_CODE_BYTES
        && !code.starts_with(REFUSED_PREFIX)
        && message.len() <= MAX_REFUSAL_MESSAGE_BYTES
        && !message.chars().any(char::is_control)
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

#[cfg(test)]
mod retry_contract_tests {
    use super::*;
    use amaterasu_contracts::ApiFieldKind;

    #[test]
    fn retry_rejects_different_response_contract_and_ordinary_collision() {
        let mut router = RequestRouter::default();
        let owner_grants = Grants {
            call: ["example/1".into()].into(),
            ..Default::default()
        };
        let provider_grants = Grants {
            provide: ["example/1".into()].into(),
            ..Default::default()
        };
        assert!(matches!(
            router.register_service("provider", &provider_grants, "example/1".into()),
            Response::Ok
        ));
        let schema = ApiSchema {
            required: [("result".into(), ApiFieldKind::String)].into(),
            optional: BTreeMap::new(),
            allow_extra: false,
        };
        let options = |response_schema| RequestOptions {
            service: "example/1".into(),
            payload: serde_json::json!({"method":"write"}),
            timeout_ms: 1000,
            operation_id: "stable".into(),
            parent: None,
            response_schema,
        };
        let now = Instant::now();
        let first = router.submit("owner", &owner_grants, options(Some(schema.clone())), now);
        assert!(matches!(first, Response::Accepted { .. }));
        assert!(matches!(
            router.submit("owner", &owner_grants, options(Some(schema.clone())), now),
            Response::Accepted { .. }
        ));
        for changed in [
            None,
            Some(ApiSchema {
                allow_extra: true,
                ..schema
            }),
        ] {
            assert!(
                matches!(router.submit("owner", &owner_grants, options(changed), now), Response::Error { code, .. } if code == "conflict")
            );
        }
    }
}
