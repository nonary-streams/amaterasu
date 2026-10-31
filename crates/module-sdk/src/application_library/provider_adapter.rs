//! Broker-facing dispatch for a selected application-library provider.
//!
//! Register the service with `Command::Register`, then pass broker `Poll` requests
//! to this adapter. The configured service, caller and initiator allowlist are
//! fixed at startup; no identity or service selector is taken from the payload.
//! Grants route trusted native code and are not a sandbox. An adapter cannot
//! prove effects, persistence, lifecycle retirement, or host-side selection.
use super::provider::{
    ArtworkChunk, ArtworkReference, DiscoveryPage, LifecycleRequest, LifecycleState,
};
use crate::requests::IncomingRequest;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const MAX_RESPONSE_BYTES: usize = 12_000;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Discover {
        cursor: Option<String>,
        limit: usize,
    },
    Artwork {
        reference: ArtworkReference,
        offset: u64,
    },
    Launch {
        operation_id: String,
        application_id: String,
    },
    Lookup {
        operation_id: String,
    },
    Stop {
        operation_id: String,
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
    fn lifecycle(
        &mut self,
        identity: &Identity<'_>,
        request: LifecycleRequest,
    ) -> Result<LifecycleState, &'static str>;
}

/// Fixed provider policy. An empty initiator set denies all requests.
pub struct ProviderAdapter<P> {
    provider: P,
    service: String,
    caller: String,
    initiators: BTreeSet<String>,
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
    pub fn new(
        provider: P,
        service: String,
        caller: String,
        initiators: BTreeSet<String>,
    ) -> Result<Self, &'static str> {
        let Some((namespace, version)) = service.rsplit_once('/') else {
            return Err("invalid service");
        };
        if !valid_identifier(namespace)
            || version != "1"
            || !valid_identifier(&caller)
            || initiators.len() > 16
            || initiators.iter().any(|id| !valid_identifier(id))
        {
            return Err("invalid provider policy");
        }
        Ok(Self {
            provider,
            service,
            caller,
            initiators,
        })
    }

    /// Dispatch exactly one broker-delivered request. Returns a JSON response
    /// suitable for `RuntimeClient::complete_serialized`.
    pub fn dispatch(&mut self, incoming: &IncomingRequest<Value>) -> Value {
        if incoming.service != self.service
            || incoming.caller != self.caller
            || !self.initiators.contains(&incoming.initiator)
        {
            return failure("unauthorized");
        }
        if incoming.remaining_ms == 0 {
            return failure("deadline_expired");
        }
        let Ok(request) = incoming.decode::<Request>() else {
            return failure("invalid_request");
        };
        let identity = Identity {
            caller: &incoming.caller,
            initiator: &incoming.initiator,
        };
        let result = match request {
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
            Request::Launch {
                operation_id,
                application_id,
            } => self.lifecycle(
                &identity,
                LifecycleRequest::Launch {
                    operation_id,
                    application_id,
                },
            ),
            Request::Lookup { operation_id } => {
                self.lifecycle(&identity, LifecycleRequest::Lookup { operation_id })
            }
            Request::Stop { operation_id } => {
                self.lifecycle(&identity, LifecycleRequest::Stop { operation_id })
            }
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
        let state = self.provider.lifecycle(identity, request)?;
        if let LifecycleState::Failed { message } = &state
            && (message.len() > 512 || message.chars().any(char::is_control))
        {
            return Err("invalid_provider_response");
        }
        serde_json::to_value(state).map_err(|_| "invalid_provider_response")
    }
}

fn valid_cursor(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn failure(code: &str) -> Value {
    json!({"ok":false,"error":{"code":code}})
}
