//! Broker-facing dispatch for a selected application-library provider.
//!
//! Register the service with `Command::Register`, then pass broker `Poll` requests
//! to this adapter. The configured service, caller and initiator allowlist are
//! fixed at startup; no identity or service selector is taken from the payload.
//! Grants route trusted native code and are not a sandbox. An adapter cannot
//! prove effects, persistence, lifecycle retirement, or host-side selection.
use super::provider::{ArtworkChunk, ArtworkReference, DiscoveryPage};
use super::provider_lifecycle::{self, Action, Observation, Request as LifecycleRequest};
use crate::requests::IncomingRequest;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_RESPONSE_BYTES: usize = 12_000;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Describe {},
    Discover {
        cursor: Option<String>,
        limit: usize,
    },
    Artwork {
        reference: ArtworkReference,
        offset: u64,
    },
    Identity {},
    LimiterTargetCapabilitiesV1 {},
    AttestLimiterTargetV1 {
        request: super::limiter_target::Request,
    },
    RetireLimiterTargetV1 {
        request: super::limiter_target::RetirementRequest,
    },
    Launch {
        operation_id: String,
        application_id: String,
        provider_incarnation: String,
    },
    Lookup {
        operation_id: String,
        application_id: String,
        provider_incarnation: String,
    },
    RecoverStop {
        operation_id: String,
        application_id: String,
        provider_incarnation: String,
    },
    Stop {
        operation_id: String,
        application_id: String,
        provider_incarnation: String,
        ticket: String,
    },
}

/// Broker-attested request identity. Provider implementations must not use a
/// payload field as a substitute for these identities.
pub struct Identity<'a> {
    pub caller: &'a str,
    pub initiator: &'a str,
}

/// Provider domain implementation. The implementation owns durable idempotency,
/// effect reconciliation, lifecycle retirement, and actual artwork access.
/// An uncertain effect must return Unknown, not Stopped or Failed. Never reuse
/// an operation ID for changed launch arguments. Before acknowledging safe
/// retirement the implementation must prove its provider-owned resources stopped.
/// An error or lost reply after dispatch can follow a side effect and must be
/// reconciled by stable operation ID; it does not prove that no effect occurred.
pub trait Provider {
    /// Actual implemented capabilities, independently restricted by caller policy.
    fn capabilities(&self) -> super::provider::Capabilities {
        super::provider::Capabilities {
            discovery: true,
            artwork: true,
            lifecycle: true,
        }
    }
    fn discover(
        &mut self,
        identity: &Identity<'_>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DiscoveryPage, &'static str>;
    fn artwork(
        &mut self,
        identity: &Identity<'_>,
        reference: &ArtworkReference,
        offset: u64,
    ) -> Result<ArtworkChunk, &'static str>;
    /// Must be unique to this provider incarnation and stable while dispatching.
    fn incarnation(&self) -> &str;
    /// Explicitly advertise the optional versioned executor-target contract.
    fn supports_limiter_target_v1(&self) -> bool {
        false
    }
    /// Advertise only when retirement fences delayed reservations for this incarnation.
    fn supports_limiter_retirement_v1(&self) -> bool {
        false
    }
    /// Bind the reserved target to this operation and keep it unchanged through
    /// the running ticket's retirement. Reject if that guarantee is unavailable.
    fn attest_limiter_target(
        &mut self,
        _identity: &Identity<'_>,
        _request: &super::limiter_target::Request,
    ) -> Result<super::limiter_target::Attestation, &'static str> {
        Err("unsupported")
    }
    /// Idempotently fence this operation, including any delayed reservation.
    fn retire_limiter_target(
        &mut self,
        _identity: &Identity<'_>,
        _request: &super::limiter_target::RetirementRequest,
    ) -> Result<super::limiter_target::Retirement, &'static str> {
        Err("unsupported")
    }
    fn lifecycle(
        &mut self,
        identity: &Identity<'_>,
        request: LifecycleRequest,
    ) -> Result<Observation, &'static str>;
}

/// Fixed provider policy. An empty initiator set denies all requests.
pub struct ProviderAdapter<P> {
    provider: P,
    service: String,
    callers: BTreeMap<String, CallerPolicy>,
}

/// Startup authorization for a single immediate caller. Grants are independent;
/// a discovery caller should not automatically gain lifecycle authority.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CallerPolicy {
    pub initiators: BTreeSet<String>,
    pub discovery: bool,
    pub artwork: bool,
    pub lifecycle: bool,
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl<P: Provider> ProviderAdapter<P> {
    /// Validates a fixed, startup-provided service and identity policy. Service
    /// routing authorization still belongs to the host's registration grants.
    /// This convenience constructor grants all adapter capabilities to the
    /// caller. Prefer `with_policies` when callers have different roles.
    pub fn new(
        provider: P,
        service: String,
        caller: String,
        initiators: BTreeSet<String>,
    ) -> Result<Self, &'static str> {
        Self::with_policies(
            provider,
            service,
            BTreeMap::from([(
                caller,
                CallerPolicy {
                    initiators,
                    discovery: true,
                    artwork: true,
                    lifecycle: true,
                },
            )]),
        )
    }

    /// Bind each broker-attested immediate caller to its permitted initiators and
    /// capabilities. Empty maps and empty sets deny access. Policy is fixed at startup.
    pub fn with_policies(
        provider: P,
        service: String,
        callers: BTreeMap<String, CallerPolicy>,
    ) -> Result<Self, &'static str> {
        let Some((namespace, version)) = service.rsplit_once('/') else {
            return Err("invalid service");
        };
        if !valid_identifier(namespace)
            || version != "1"
            || callers.len() > 16
            || callers.iter().any(|(caller, policy)| {
                !valid_identifier(caller)
                    || policy.initiators.len() > 16
                    || policy.initiators.iter().any(|id| !valid_identifier(id))
            })
        {
            return Err("invalid provider policy");
        }
        Ok(Self {
            provider,
            service,
            callers,
        })
    }

    /// Dispatch exactly one broker-delivered request. Returns a JSON response
    /// suitable for `RuntimeClient::complete_serialized`.
    pub fn dispatch(&mut self, incoming: &IncomingRequest<Value>) -> Value {
        if incoming.service != self.service
            || !self
                .callers
                .get(&incoming.caller)
                .is_some_and(|policy| policy.initiators.contains(&incoming.initiator))
        {
            return failure("unauthorized");
        }
        if incoming.remaining_ms == 0 {
            return failure("deadline_expired");
        }
        let Ok(request) = incoming.decode::<Request>() else {
            return failure("invalid_request");
        };
        let policy = &self.callers[&incoming.caller];
        let capabilities = self.provider.capabilities();
        let authorized = match &request {
            Request::Describe {} => true,
            Request::Discover { .. } => policy.discovery && capabilities.discovery,
            Request::Artwork { .. } => policy.artwork && capabilities.artwork,
            Request::Identity { .. }
            | Request::LimiterTargetCapabilitiesV1 { .. }
            | Request::AttestLimiterTargetV1 { .. }
            | Request::RetireLimiterTargetV1 { .. }
            | Request::Launch { .. }
            | Request::Lookup { .. }
            | Request::RecoverStop { .. }
            | Request::Stop { .. } => policy.lifecycle && capabilities.lifecycle,
        };
        if !authorized {
            return failure("unauthorized");
        }
        let identity = Identity {
            caller: &incoming.caller,
            initiator: &incoming.initiator,
        };
        let result = match request {
            Request::Describe {} => {
                let capabilities = self.provider.capabilities();
                Ok(json!({"capabilities":super::provider::Capabilities {
                    discovery: capabilities.discovery && policy.discovery,
                    artwork: capabilities.artwork && policy.artwork,
                    lifecycle: capabilities.lifecycle && policy.lifecycle,
                }}))
            }
            Request::Discover { cursor, limit } => {
                if !(1..=super::provider::MAX_PAGE_ENTRIES).contains(&limit)
                    || cursor
                        .as_deref()
                        .is_some_and(|cursor| !valid_cursor(cursor))
                {
                    return failure("invalid_request");
                }
                self.provider
                    .discover(&identity, cursor.as_deref(), limit)
                    .and_then(|page| {
                        page.validate()?;
                        if page.entries.len() > limit {
                            return Err("invalid_provider_response");
                        }
                        serde_json::to_value(page).map_err(|_| "invalid_provider_response")
                    })
            }
            Request::Artwork { reference, offset } => {
                if !valid_cursor(&reference.id)
                    || !valid_cursor(&reference.revision)
                    || offset >= super::provider::MAX_ARTWORK_BYTES
                {
                    return failure("invalid_request");
                }
                self.provider
                    .artwork(&identity, &reference, offset)
                    .and_then(|chunk| {
                        chunk.validate()?;
                        if chunk.reference != reference || chunk.offset != offset {
                            return Err("invalid_provider_response");
                        }
                        Ok(json!({
                            "reference": chunk.reference,
                            "mime": chunk.mime,
                            "offset": chunk.offset,
                            "total_bytes": chunk.total_bytes,
                            "data_base64": STANDARD.encode(&chunk.bytes),
                        }))
                    })
            }
            Request::Identity {} => {
                if !provider_lifecycle::valid_incarnation(self.provider.incarnation()) {
                    return failure("invalid_provider_response");
                }
                Ok(json!({"provider_incarnation":self.provider.incarnation()}))
            }
            Request::LimiterTargetCapabilitiesV1 {} => Ok(
                json!({"version":1,"supported":self.provider.supports_limiter_target_v1(),
                    "retirement_supported":self.provider.supports_limiter_retirement_v1()}),
            ),
            Request::AttestLimiterTargetV1 { request } => request.validate().and_then(|()| {
                if !self.provider.supports_limiter_target_v1() {
                    return Err("unsupported");
                }
                if self.provider.incarnation() != request.provider_incarnation {
                    return Err("stale_provider_incarnation");
                }
                let assertion = self.provider.attest_limiter_target(&identity, &request)?;
                assertion.validate_for(&request)?;
                Ok(json!({"attestation":assertion}))
            }),
            Request::RetireLimiterTargetV1 { request } => request.validate().and_then(|()| {
                if !self.provider.supports_limiter_target_v1()
                    || !self.provider.supports_limiter_retirement_v1()
                    || identity.caller != identity.initiator
                {
                    return Err("unauthorized");
                }
                if self.provider.incarnation() != request.provider_incarnation {
                    return Err("stale_provider_incarnation");
                }
                let proof = self.provider.retire_limiter_target(&identity, &request)?;
                proof.validate_for(&request)?;
                Ok(json!({"retirement":proof}))
            }),
            Request::Launch {
                operation_id,
                application_id,
                provider_incarnation,
            } => self.lifecycle(
                &identity,
                LifecycleRequest {
                    action: Action::Launch,
                    operation_id,
                    application_id,
                    provider_incarnation,
                    ticket: None,
                },
            ),
            Request::Lookup {
                operation_id,
                application_id,
                provider_incarnation,
            } => self.lifecycle(
                &identity,
                LifecycleRequest {
                    action: Action::Lookup,
                    operation_id,
                    application_id,
                    provider_incarnation,
                    ticket: None,
                },
            ),
            Request::RecoverStop {
                operation_id,
                application_id,
                provider_incarnation,
            } => self.lifecycle(
                &identity,
                LifecycleRequest {
                    action: Action::RecoverStop,
                    operation_id,
                    application_id,
                    provider_incarnation,
                    ticket: None,
                },
            ),
            Request::Stop {
                operation_id,
                application_id,
                provider_incarnation,
                ticket,
            } => self.lifecycle(
                &identity,
                LifecycleRequest {
                    action: Action::Stop,
                    operation_id,
                    application_id,
                    provider_incarnation,
                    ticket: Some(ticket),
                },
            ),
        };
        match result {
            Ok(Value::Object(mut value)) => {
                value.insert("ok".into(), true.into());
                let value = Value::Object(value);
                if serde_json::to_vec(&value).is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES) {
                    value
                } else {
                    failure("invalid_provider_response")
                }
            }
            _ => failure("provider_failure"),
        }
    }

    fn lifecycle(
        &mut self,
        identity: &Identity<'_>,
        request: LifecycleRequest,
    ) -> Result<Value, &'static str> {
        request.validate()?;
        if self.provider.incarnation() != request.provider_incarnation {
            return Err("stale_provider_incarnation");
        }
        let observation = self.provider.lifecycle(identity, request.clone())?;
        observation.validate_for(&request)?;
        serde_json::to_value(observation).map_err(|_| "invalid_provider_response")
    }
}

fn valid_cursor(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn failure(code: &str) -> Value {
    json!({"ok":false,"error":{"code":code}})
}
