//! Optional input-extension service. This is independent of `amaterasu.input/1`.
//!
//! Trusted composition supplies each route and the caller/initiator grants. Route
//! identifiers in a request are selectors, never authority. Providers must retire
//! routes on session teardown and must not reuse generations for a replacement.
//! The service carries bounded schema-specific bytes; a schema's implementation,
//! client negotiation, device access, and effects remain the provider's job.

use crate::requests::IncomingRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SERVICE: &str = "amaterasu.input-extension/1";
pub const MAX_ROUTES: usize = 32;
pub const MAX_CAPABILITIES: usize = 32;
pub const MAX_PAYLOAD_BYTES: usize = 2048;
pub const MAX_REQUEST_BYTES: usize = 16_384;

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
}

/// An exact, namespaced schema identity. Both ends must understand its semantics.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub schema: String,
    pub version: u16,
    pub direction: Direction,
}

impl Capability {
    fn valid(&self) -> bool {
        valid_name(&self.schema) && self.schema.contains('.') && self.version != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    ToDevice,
    ToClient,
}

/// All IDs are opaque and scoped to the trusted lifecycle that installed them.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub host_epoch: String,
    pub session: String,
    pub source: String,
    pub device: String,
    pub generation: u64,
}

impl Route {
    fn valid(&self) -> bool {
        valid_name(&self.host_epoch)
            && valid_name(&self.session)
            && valid_name(&self.source)
            && valid_name(&self.device)
            && self.generation != 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Principal {
    pub caller: String,
    pub initiator: String,
}

impl Principal {
    fn valid(&self) -> bool {
        valid_name(&self.caller) && valid_name(&self.initiator)
    }

    fn matches(&self, request: &IncomingRequest<Value>) -> bool {
        self.caller == request.caller && self.initiator == request.initiator
    }
}

/// Installed by trusted composition, not constructed from the request body.
/// `required` capabilities must appear in both offers; the only permitted
/// messages are in the intersection. Empty grants deny a direction.
pub struct RoutePolicy {
    pub route: Route,
    pub provider_offer: BTreeSet<Capability>,
    pub adapter_offer: BTreeSet<Capability>,
    pub required: BTreeSet<Capability>,
    pub to_device: Vec<Principal>,
    pub to_client: Vec<Principal>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub route: Route,
    pub capability: Capability,
    /// Strictly increasing per route and direction; first accepted value is 1.
    pub sequence: u64,
    pub payload: Vec<u8>,
}

impl Message {
    /// Submit through the broker using its authenticated caller and optional parent.
    /// Reconcile an uncertain result with the same operation ID and payload.
    pub fn submit(
        &self,
        runtime: &crate::RuntimeClient,
        timeout_ms: u64,
        operation_id: &str,
        parent: Option<u64>,
    ) -> crate::ModuleResult<u64> {
        if !self.route.valid()
            || !self.capability.valid()
            || self.payload.len() > MAX_PAYLOAD_BYTES
            || self.sequence == 0
        {
            return Err("invalid input extension message".into());
        }
        runtime.request_serialized(
            crate::requests::RequestOptions {
                service: SERVICE,
                timeout_ms,
                operation_id,
                parent,
            },
            self,
        )
    }
}

/// A successful submit means local admission only. BUSY must consume nothing.
pub enum SubmitError {
    Busy,
    Rejected,
}

pub trait Endpoint {
    /// Actual provider implementation support; declarations alone are insufficient.
    fn supports(&self, capability: &Capability) -> bool;
    fn submit(&mut self, message: &Message) -> Result<(), SubmitError>;
}

struct ActiveRoute {
    capabilities: BTreeSet<Capability>,
    to_device: Vec<Principal>,
    to_client: Vec<Principal>,
    device_sequence: u64,
    client_sequence: u64,
}

/// Adapter for an independently built provider. It validates routing before the
/// endpoint sees bytes. The owner must serialize dispatch and retirement.
pub struct ExtensionAdapter<E> {
    endpoint: E,
    routes: BTreeMap<Route, ActiveRoute>,
    generations: BTreeMap<(String, String, String, String), u64>,
}

impl<E: Endpoint> ExtensionAdapter<E> {
    pub fn new(endpoint: E, policies: Vec<RoutePolicy>) -> Result<Self, &'static str> {
        let mut adapter = Self {
            endpoint,
            routes: BTreeMap::new(),
            generations: BTreeMap::new(),
        };
        for policy in policies {
            adapter.install(policy)?;
        }
        Ok(adapter)
    }

    /// Install a route from trusted lifecycle evidence. A replacement must be
    /// retired first and use a higher generation. Retired identity tombstones
    /// remain bounded and are retained for the adapter's entire lifetime.
    pub fn install(&mut self, policy: RoutePolicy) -> Result<(), &'static str> {
        let sets = [
            &policy.provider_offer,
            &policy.adapter_offer,
            &policy.required,
        ];
        if !policy.route.valid()
            || sets
                .iter()
                .any(|set| set.len() > MAX_CAPABILITIES || set.iter().any(|cap| !cap.valid()))
            || policy.to_device.len() > MAX_CAPABILITIES
            || policy.to_client.len() > MAX_CAPABILITIES
            || policy
                .to_device
                .iter()
                .chain(&policy.to_client)
                .any(|p| !p.valid())
        {
            return Err("invalid_policy");
        }
        if policy
            .provider_offer
            .iter()
            .any(|cap| !self.endpoint.supports(cap))
        {
            return Err("provider_capability_unavailable");
        }
        let capabilities: BTreeSet<_> = policy
            .provider_offer
            .intersection(&policy.adapter_offer)
            .cloned()
            .collect();
        if !policy.required.is_subset(&capabilities) {
            return Err("required_capability_unavailable");
        }
        let key = (
            policy.route.host_epoch.clone(),
            policy.route.session.clone(),
            policy.route.source.clone(),
            policy.route.device.clone(),
        );
        if self.routes.keys().any(|route| {
            route.host_epoch == key.0
                && route.session == key.1
                && route.source == key.2
                && route.device == key.3
        }) {
            return Err("route_active");
        }
        if self
            .generations
            .get(&key)
            .is_some_and(|last| policy.route.generation <= *last)
        {
            return Err("stale_generation");
        }
        if self.routes.len() >= MAX_ROUTES
            || (!self.generations.contains_key(&key) && self.generations.len() >= MAX_ROUTES)
        {
            return Err("too_many_routes");
        }
        let active = ActiveRoute {
            capabilities,
            to_device: policy.to_device,
            to_client: policy.to_client,
            device_sequence: 0,
            client_sequence: 0,
        };
        self.generations.insert(key, policy.route.generation);
        self.routes.insert(policy.route, active);
        Ok(())
    }

    /// Fence future submissions before retiring endpoint-owned device resources.
    /// This does not cancel work already admitted by the endpoint.
    pub fn retire(&mut self, route: &Route) -> bool {
        self.routes.remove(route).is_some()
    }

    pub fn endpoint(&self) -> &E {
        &self.endpoint
    }

    pub fn dispatch(&mut self, request: &IncomingRequest<Value>) -> Value {
        match self.submit(request) {
            Ok(()) => json!({"ok": true, "accepted": true}),
            Err(code) => json!({"ok": false, "error": {"code": code}}),
        }
    }

    fn submit(&mut self, request: &IncomingRequest<Value>) -> Result<(), &'static str> {
        if request.service != SERVICE || request.remaining_ms == 0 {
            return Err("invalid_request");
        }
        if !serde_json::to_vec(&request.payload).is_ok_and(|b| b.len() <= MAX_REQUEST_BYTES) {
            return Err("invalid_request");
        }
        let message: Message = request.decode().map_err(|_| "invalid_request")?;
        if !message.route.valid()
            || !message.capability.valid()
            || message.payload.len() > MAX_PAYLOAD_BYTES
        {
            return Err("invalid_request");
        }
        let route = self.routes.get_mut(&message.route).ok_or("unauthorized")?;
        let (principals, sequence) = match message.capability.direction {
            Direction::ToDevice => (&route.to_device, &mut route.device_sequence),
            Direction::ToClient => (&route.to_client, &mut route.client_sequence),
        };
        if !principals
            .iter()
            .any(|principal| principal.matches(request))
        {
            return Err("unauthorized");
        }
        if !route.capabilities.contains(&message.capability) {
            return Err("unsupported_capability");
        }
        if sequence.checked_add(1) != Some(message.sequence) {
            return Err("invalid_sequence");
        }
        match self.endpoint.submit(&message) {
            Ok(()) => {
                *sequence = message.sequence;
                Ok(())
            }
            Err(SubmitError::Busy) => Err("busy"),
            Err(SubmitError::Rejected) => Err("rejected"),
        }
    }
}
