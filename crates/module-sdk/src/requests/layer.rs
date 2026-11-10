//! Stacking on another module's service.
//!
//! Trusted composition can list this module as a layer of a service. Every
//! call a consumer makes to that service then arrives here first, and this
//! module's own call to the service continues to the layer beneath it, or to
//! the provider when it is the innermost. Consumers and the provider keep
//! using the one service name and are not rebuilt.
//!
//! A layer decides, per request, to pass it on unchanged, pass on something
//! different, answer it itself, or [`refuse`](IncomingRequest::refuse) it.
//! [`Relay`] carries the requests it passes on and answers each consumer when
//! the inner call settles.
//!
//! A forwarded request keeps the consumer as its caller, so the provider
//! authorizes exactly who it would without the layer. That is why a layer is
//! approved like a replacement provider: it can act as any caller of the
//! service it wraps.
use super::{
    ApiRequestOptions, CallOutcome, IncomingRequest, OutboundCall, RequestOptions, refuse_with,
    reply_with,
};
use crate::control_error::{ControlError, ControlResult};
use crate::{Command, ModuleResult, Response, RuntimeClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::task::Poll;
use std::time::Instant;

/// The broker's longest request deadline.
const MAX_TIMEOUT_MS: u64 = 30_000;

impl RuntimeClient {
    /// Take this module's place in the stack of `service`. Trusted
    /// composition must list the module as a layer of exactly this service;
    /// that listing is the authority, and no `provide` grant is involved.
    ///
    /// Calls to the service are refused as unavailable until every listed
    /// layer and the provider have registered, so a layer is never skipped
    /// while it starts. Requests then arrive through
    /// [`next_request`](Self::next_request) like a provider's.
    pub fn wrap(&self, service: &str) -> ControlResult<()> {
        match self.send_checked(Command::Register {
            service: service.to_owned(),
        })? {
            Response::Ok => Ok(()),
            response => Err(ControlError::unexpected(&response)),
        }
    }
}

/// The envelope the broker delivers for a call to an approved API method.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiEnvelope {
    method: String,
    interface_major: u32,
    interface_minor: u32,
    arguments: Value,
}

impl IncomingRequest<Value> {
    /// The call that passes this request, unchanged, to the next hop beneath
    /// this layer. See [`forward_with`](Self::forward_with).
    pub fn forward(&self) -> ControlResult<OutboundCall> {
        self.forward_with(&self.payload)
    }

    /// The call that passes `payload` to the next hop in place of this
    /// request's own. It is made on behalf of this request: the next hop sees
    /// this request's caller, the call cannot outlive this request's deadline,
    /// and it is cancelled when this request ends.
    ///
    /// The operation identifier is derived from the request, so a layer can
    /// forward each request it receives once. Build an [`OutboundCall`] with
    /// this request as `parent` to make further calls for it.
    pub fn forward_with<P: Serialize>(&self, payload: &P) -> ControlResult<OutboundCall> {
        let operation_id = self.forward_operation_id();
        // A request is forwarded once, so no repeat needs the ended record.
        OutboundCall::request(self.forward_options(&operation_id), payload)
            .map(OutboundCall::single_use)
    }

    /// Like [`forward`](Self::forward), for a service whose methods are
    /// approved API contracts. The method and version are taken from the
    /// delivered envelope, and this layer must be an approved caller of them.
    pub fn forward_api(&self) -> ControlResult<OutboundCall> {
        let envelope = self.api_envelope()?;
        self.forward_api_envelope(&envelope, &envelope.arguments)
    }

    /// Like [`forward_with`](Self::forward_with) for an approved API method:
    /// `arguments` replace the delivered ones and are checked against the
    /// method's installed request schema.
    pub fn forward_api_with<P: Serialize>(&self, arguments: &P) -> ControlResult<OutboundCall> {
        self.forward_api_envelope(&self.api_envelope()?, arguments)
    }

    fn forward_api_envelope<P: Serialize>(
        &self,
        envelope: &ApiEnvelope,
        arguments: &P,
    ) -> ControlResult<OutboundCall> {
        let operation_id = self.forward_operation_id();
        OutboundCall::api(
            ApiRequestOptions {
                request: self.forward_options(&operation_id),
                method: &envelope.method,
                interface_major: envelope.interface_major,
                interface_minor: envelope.interface_minor,
            },
            arguments,
        )
        .map(OutboundCall::single_use)
    }

    fn api_envelope(&self) -> ControlResult<ApiEnvelope> {
        ApiEnvelope::deserialize(&self.payload).map_err(ControlError::payload)
    }

    fn forward_operation_id(&self) -> String {
        // Request identifiers are unique for the life of the runtime, and the
        // broker scopes operation identifiers to the calling module.
        format!("forward.{}", self.id)
    }

    fn forward_options<'a>(&'a self, operation_id: &'a str) -> RequestOptions<'a> {
        RequestOptions {
            service: &self.service,
            // The broker also caps the call at this request's own deadline.
            timeout_ms: self.remaining_ms.clamp(1, MAX_TIMEOUT_MS),
            operation_id,
            parent: Some(self.id),
        }
    }
}

/// What a layer tells the consumer once the call it forwarded has settled.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// Reply with this payload.
    Reply(Value),
    /// Decline: the operation was not performed.
    Refuse { code: String, message: String },
    /// Say nothing. The consumer's request ends at its own deadline, which
    /// tells it the outcome is unknown.
    Leave,
}

impl Answer {
    /// The transparent answer: pass a reply through, pass a refusal on with
    /// its code, and stay silent when the inner outcome is unknown. A layer
    /// must not turn an unknown outcome into a refusal, because a refusal
    /// tells the consumer nothing happened.
    pub fn relay(outcome: CallOutcome) -> Self {
        match outcome {
            CallOutcome::Completed(reply) => Self::Reply(reply),
            CallOutcome::Refused(refusal) => Self::Refuse {
                code: refusal.code.as_str().to_owned(),
                message: refusal.message,
            },
            CallOutcome::Unconfirmed(_) => Self::Leave,
        }
    }
}

struct Forwarded {
    request: IncomingRequest<Value>,
    call: OutboundCall,
}

/// The requests a layer has passed on and not yet answered.
///
/// ```ignore
/// fn tick(&mut self) -> ModuleResult<()> {
///     while let Some(request) = self.runtime.next_request()? {
///         self.relay.forward(request)?;
///     }
///     self.relay.poll(&self.runtime, |_request, reply| reply)?;
///     Ok(())
/// }
/// ```
///
/// The broker bounds how many requests exist at once, which bounds a relay
/// that forwards each request it receives.
#[derive(Default)]
pub struct Relay {
    in_flight: Vec<Forwarded>,
}

impl Relay {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many forwarded requests are waiting for the inner call to settle.
    pub fn len(&self) -> usize {
        self.in_flight.len()
    }

    pub fn is_empty(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// Pass `request` on unchanged.
    pub fn forward(&mut self, request: IncomingRequest<Value>) -> ControlResult<()> {
        let call = request.forward()?;
        self.track(request, call);
        Ok(())
    }

    /// Pass `payload` on in place of the request's own.
    pub fn forward_with<P: Serialize>(
        &mut self,
        request: IncomingRequest<Value>,
        payload: &P,
    ) -> ControlResult<()> {
        let call = request.forward_with(payload)?;
        self.track(request, call);
        Ok(())
    }

    /// Pass on a call to an approved API method unchanged.
    pub fn forward_api(&mut self, request: IncomingRequest<Value>) -> ControlResult<()> {
        let call = request.forward_api()?;
        self.track(request, call);
        Ok(())
    }

    /// Pass on a call to an approved API method with different arguments.
    pub fn forward_api_with<P: Serialize>(
        &mut self,
        request: IncomingRequest<Value>,
        arguments: &P,
    ) -> ControlResult<()> {
        let call = request.forward_api_with(arguments)?;
        self.track(request, call);
        Ok(())
    }

    /// Carry a call this layer built itself for `request`, for example one
    /// declared [`read_only`](OutboundCall::read_only).
    pub fn track(&mut self, request: IncomingRequest<Value>, call: OutboundCall) {
        self.in_flight.push(Forwarded { request, call });
    }

    /// Advance every forwarded call and answer the consumers whose calls
    /// settled. `rewrite` sees each reply before the consumer does; refusals
    /// and unknown outcomes are relayed as [`Answer::relay`] describes.
    /// Returns how many requests were answered or left.
    pub fn poll(
        &mut self,
        runtime: &RuntimeClient,
        mut rewrite: impl FnMut(&IncomingRequest<Value>, Value) -> Value,
    ) -> ControlResult<usize> {
        self.poll_at(
            Instant::now(),
            &mut |command| runtime.send(command),
            &mut |request, outcome| match outcome {
                CallOutcome::Completed(reply) => Answer::Reply(rewrite(request, reply)),
                other => Answer::relay(other),
            },
        )
    }

    /// Like [`poll`](Self::poll), with the layer choosing the answer for
    /// every outcome.
    pub fn poll_with(
        &mut self,
        runtime: &RuntimeClient,
        mut answer: impl FnMut(&IncomingRequest<Value>, CallOutcome) -> Answer,
    ) -> ControlResult<usize> {
        self.poll_at(
            Instant::now(),
            &mut |command| runtime.send(command),
            &mut answer,
        )
    }

    /// Advance through an injected transport, as of `now`.
    ///
    /// Every forwarded call is advanced even when answering one consumer
    /// fails; the first such failure is returned afterwards.
    pub fn poll_at(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        answer: &mut impl FnMut(&IncomingRequest<Value>, CallOutcome) -> Answer,
    ) -> ControlResult<usize> {
        let mut settled = 0;
        let mut first_failure = None;
        let mut still_waiting = Vec::with_capacity(self.in_flight.len());
        for mut forwarded in self.in_flight.drain(..) {
            let Poll::Ready(outcome) = forwarded.call.poll_at(now, send) else {
                still_waiting.push(forwarded);
                continue;
            };
            settled += 1;
            let delivered = deliver(
                send,
                &forwarded.request,
                answer(&forwarded.request, outcome),
            );
            if let Err(error) = delivered {
                first_failure.get_or_insert(error);
            }
        }
        self.in_flight = still_waiting;
        match first_failure {
            Some(error) => Err(error),
            None => Ok(settled),
        }
    }
}

fn deliver(
    send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    request: &IncomingRequest<Value>,
    answer: Answer,
) -> ControlResult<()> {
    match answer {
        Answer::Reply(reply) => reply_with(send, request.id, &reply).map(drop),
        Answer::Refuse { code, message } => {
            refuse_with(send, request.id, &code, &message).map(drop)
        }
        Answer::Leave => Ok(()),
    }
}

#[cfg(test)]
mod tests;
