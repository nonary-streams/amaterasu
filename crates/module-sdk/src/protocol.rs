//! Bounded control-extension admission over the existing approved API broker.
//!
//! This helper is inert until a trusted coordinator wires authenticated ingress,
//! authority, and an obligation store. It does not authenticate a client, provide
//! transport, negotiate capabilities, or retire effects. Native plugins are trusted.
use crate::{
    Command, Response,
    extension_api::{LifecycleTarget, MethodRef},
};
use amaterasu_contracts::ApiMethodApproval;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Maximum binary payload. Base64 and the bounded envelope fit the broker limit.
pub const MAX_MESSAGE_BYTES: usize = 2048;
/// Encoded broker response bound before the ingress owner forwards it.
pub const MAX_RESPONSE_BYTES: usize = 4096;
pub const MAX_ROUTES: usize = 32;
/// Control types already interpreted by the current GameStream profile, including
/// its host feedback types. A router cannot take ownership of these routes.
const RESERVED_CONTROL_TYPES: &[u16] = &[
    0x0001, 0x0109, 0x010b, 0x010e, 0x0200, 0x0201, 0x0204, 0x0206, 0x0301, 0x0302, 0x0305, 0x0307,
    0x0350, 0x5500, 0x5501, 0x5502, 0x5503, 0x5601,
];

/// Trusted startup policy, not a remote request or an authorization grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub message_type: u16,
    pub capability: String,
    pub approval: ApiMethodApproval,
    pub method: MethodRef,
    /// Provider-owned cleanup method; the coordinator must retain its obligation.
    pub cleanup: MethodRef,
    pub cleanup_approval: ApiMethodApproval,
    pub max_payload_bytes: usize,
}

/// References are selectors, not bearer credentials. Only the ingress owner may
/// supply this context, after authenticated transport and session binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionContext {
    pub target: LifecycleTarget,
    pub peer_ref: String,
}

/// The trusted authority checks current authenticated identity, stream generation,
/// negotiated capability and purpose-specific permission together. Its guard must
/// fence revocation through broker admission. Providers must revalidate before an
/// effect; this guard does not authorize work after the admission call returns.
/// The default denies. The production ingress must not enable this helper until it
/// can implement these semantics, including crash and revocation reconciliation.
pub trait SessionAuthority {
    type AdmissionGuard;
    fn admit(
        &self,
        _context: &SessionContext,
        _capability: &str,
    ) -> Result<Self::AdmissionGuard, &'static str> {
        Err("unauthorized")
    }
}

/// A conservative cleanup obligation is reserved *before* a provider call.
/// The host must persist and reconcile it, including an uncertain broker outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingEffect {
    /// Authenticated broker caller principal; together with operation_id this
    /// identifies the broker operation and the authority used for reconciliation.
    pub caller: String,
    pub operation_id: String,
    pub context: SessionContext,
    pub message_type: u16,
    pub capability: String,
    pub provider: String,
    pub method: MethodRef,
    pub cleanup: MethodRef,
    /// Digest only; the obligation does not retain the message body.
    pub payload_sha256: String,
    pub timeout_ms: u64,
    /// Optional broker parent; authority and deadline are intersected by the runtime.
    pub parent: Option<u64>,
}

/// Result of an atomic, durable reservation. Existing and uncertain operations
/// require reconciliation; neither authorizes a new provider call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reservation {
    New,
    Existing,
    Uncertain,
}

/// The host must atomically reserve the (caller, operation_id) pair, preserving
/// the complete effect until confirmed cleanup. An identical record is Existing;
/// conflicting reuse, capacity exhaustion and unavailable durable state fail
/// closed. If the store cannot determine whether an admission occurred, return
/// Uncertain. A crash after New also requires reconciliation, even if no broker
/// call was made. Retain the reservation or a tombstone until the host prevents
/// all further use of that operation identity. If bounded storage is full, reject
/// new work; do not evict identities that may be retried. Reconciliation must
/// use current host authority; a retained parent ID is not authority after restart.
/// Do not treat broker retry retention as durable deduplication.
pub trait ObligationStore {
    fn reserve(&mut self, effect: &PendingEffect) -> Result<Reservation, &'static str>;
}

/// A trusted composition binding the actual authenticated broker endpoint and
/// its principal. Never construct from client input: `send` must always use the
/// endpoint belonging to `caller`, including after a runtime restart. The host
/// must retain or re-establish that authority for reconciliation.
pub struct BrokerEndpoint<'a, F> {
    caller: &'a str,
    send: F,
}

impl<'a, F> BrokerEndpoint<'a, F> {
    pub fn new(caller: &'a str, send: F) -> Self {
        Self { caller, send }
    }
}

/// Authenticated ingress values supplied by the trusted owning coordinator.
pub struct DispatchRequest<'a> {
    pub context: &'a SessionContext,
    pub message_type: u16,
    pub payload: &'a [u8],
    pub operation_id: &'a str,
    pub timeout_ms: u64,
    pub parent: Option<u64>,
}

/// Client-to-host router only. Response delivery and session retirement remain
/// the ingress coordinator's responsibility. No default provider is selected.
pub struct ProtocolRouter {
    routes: Vec<Route>,
}

impl ProtocolRouter {
    pub fn new(routes: Vec<Route>) -> Result<Self, &'static str> {
        if routes.len() > MAX_ROUTES {
            return Err("too_many_routes");
        }
        let mut types = BTreeSet::new();
        for route in &routes {
            let method = &route.approval.method;
            if RESERVED_CONTROL_TYPES.contains(&route.message_type)
                || !types.insert(route.message_type)
                || !valid_name(&route.capability)
                || !valid_name(&route.approval.provider)
                || !valid_name(&method.service)
                || !valid_name(&method.method)
                || method.interface_major == 0
                || route.method.service != method.service
                || route.method.method != method.method
                || route.method.interface_major != method.interface_major
                || route.method.interface_minor != method.interface_minor
                || route.method.schema_revision == 0
                || route.cleanup_approval.provider != route.approval.provider
                || route.cleanup_approval.method.service != route.cleanup.service
                || route.cleanup_approval.method.method != route.cleanup.method
                || route.cleanup_approval.method.interface_major != route.cleanup.interface_major
                || route.cleanup_approval.method.interface_minor != route.cleanup.interface_minor
                || !valid_name(&route.cleanup.service)
                || !valid_name(&route.cleanup.method)
                || (route.cleanup.service == route.method.service
                    && route.cleanup.method == route.method.method
                    && route.cleanup.interface_major == route.method.interface_major)
                || route.cleanup.interface_major == 0
                || route.cleanup.schema_revision == 0
                || route.max_payload_bytes == 0
                || route.max_payload_bytes > MAX_MESSAGE_BYTES
            {
                return Err("invalid_route");
            }
        }
        Ok(Self { routes })
    }

    /// Route an authenticated inner control message using the existing broker.
    /// The endpoint must be the trusted caller endpoint installed in the approval.
    /// Even a transport error after reservation leaves the effect in `store`.
    pub fn dispatch<A: SessionAuthority, S: ObligationStore>(
        &self,
        authority: &A,
        store: &mut S,
        request: DispatchRequest<'_>,
        endpoint: BrokerEndpoint<'_, impl FnMut(Command) -> Result<Response, String>>,
    ) -> Result<Response, String> {
        let BrokerEndpoint { caller, mut send } = endpoint;
        let DispatchRequest {
            context,
            message_type,
            payload,
            operation_id,
            timeout_ms,
            parent,
        } = request;
        let route = self
            .routes
            .iter()
            .find(|route| route.message_type == message_type)
            .ok_or("unregistered_route")?;
        if !amaterasu_contracts::valid_name(caller)
            || !route.approval.callers.contains(caller)
            || !route.cleanup_approval.callers.contains(caller)
            || !valid_context(context)
            || !amaterasu_contracts::valid_name(operation_id)
            || !(1..=30_000).contains(&timeout_ms)
            || payload.len() > route.max_payload_bytes
        {
            return Err("invalid_request".into());
        }
        // The guard remains live through reservation and broker admission. Its
        // implementation, not this helper, enforces atomic revocation fencing.
        let _guard = authority
            .admit(context, &route.capability)
            .map_err(str::to_owned)?;
        let method = &route.approval.method;
        // Both action and cleanup must resolve to the exact selected provider.
        for expected in [&route.approval, &route.cleanup_approval] {
            let approved = &expected.method;
            let response = send(Command::QueryApiApproval {
                service: approved.service.clone(),
                method: approved.method.clone(),
                interface_major: approved.interface_major,
                min_minor: approved.interface_minor,
            })?;
            let Response::Result { payload } = response else {
                return Err("approval_unavailable".into());
            };
            if serde_json::from_value::<ApiMethodApproval>(payload)
                .ok()
                .as_ref()
                != Some(expected)
            {
                return Err("approval_mismatch".into());
            }
        }
        let effect = PendingEffect {
            caller: caller.into(),
            operation_id: operation_id.into(),
            context: context.clone(),
            message_type,
            capability: route.capability.clone(),
            provider: route.approval.provider.clone(),
            method: route.method.clone(),
            cleanup: route.cleanup.clone(),
            payload_sha256: format!("{:x}", Sha256::digest(payload)),
            timeout_ms,
            parent,
        };
        match store.reserve(&effect).map_err(str::to_owned)? {
            Reservation::New => {}
            Reservation::Existing | Reservation::Uncertain => {
                return Err("operation_requires_reconciliation".into());
            }
        }
        send(Command::CallApi {
            service: method.service.clone(),
            method: method.method.clone(),
            interface_major: method.interface_major,
            interface_minor: method.interface_minor,
            payload: serde_json::json!({"context": context, "capability":route.capability,
                "message_type":message_type, "payload_base64":STANDARD.encode(payload)}),
            timeout_ms,
            operation_id: operation_id.into(),
            parent,
        })
    }
}

/// Validate a completed broker response before a coordinator forwards it. The
/// broker enforces the installed response schema; this adds a tighter byte bound.
/// The owning coordinator must separately revalidate the session and permission
/// and match the result to the original request before any client delivery.
pub fn validate_response(payload: &serde_json::Value) -> Result<(), &'static str> {
    if serde_json::to_vec(payload).is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES) {
        Ok(())
    } else {
        Err("response_too_large")
    }
}

fn valid_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
fn valid_context(context: &SessionContext) -> bool {
    matches!(&context.target, LifecycleTarget::Stream { host_epoch, session_id, stream_id, generation }
        if valid_name(host_epoch) && valid_name(session_id) && valid_name(stream_id) && *generation > 0)
        && valid_name(&context.peer_ref)
}
