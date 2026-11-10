//! The provider side of the request protocol: registration, delivery and reply.
//!
//! A request can end while its provider is still working: the caller cancels
//! it, its deadline passes, or its parent ends. That is ordinary, so a reply to
//! an ended request is reported as [`ReplyDelivery::RequestEnded`] rather than
//! as a failure of the provider's tick.
use super::IncomingRequest;
use crate::control_error::{ControlError, ControlResult, ErrorCode, checked};
use crate::{ApiMethod, Command, ModuleResult, Response, RuntimeClient};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

/// What became of a reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyDelivery {
    /// The broker recorded the reply for the caller to collect.
    Recorded,
    /// The request had already ended, so the reply was discarded. Work the
    /// provider committed stands; the caller recovers it with an idempotent
    /// retry.
    RequestEnded,
}

/// A delivered request whose payload is not an operation of the service.
/// The request comes back so the provider can still reply to it.
#[derive(Debug)]
pub struct MalformedRequest {
    pub request: IncomingRequest<Value>,
    pub reason: String,
}

/// Take the next request delivered to this provider through `send`.
pub fn next_request_with(
    send: &mut impl FnMut(Command) -> ModuleResult<Response>,
) -> ControlResult<Option<IncomingRequest<Value>>> {
    match checked(send(Command::Poll {}))? {
        Response::Pending => Ok(None),
        Response::Request {
            id,
            caller,
            initiator,
            service,
            payload,
            remaining_ms,
        } => Ok(Some(IncomingRequest {
            id,
            caller,
            initiator,
            service,
            payload,
            remaining_ms,
        })),
        response => Err(ControlError::unexpected(&response)),
    }
}

/// Reply to a delivered request through `send`.
pub fn reply_with<T: Serialize>(
    send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    request: u64,
    payload: &T,
) -> ControlResult<ReplyDelivery> {
    let payload = serde_json::to_value(payload).map_err(ControlError::payload)?;
    match checked(send(Command::Complete { request, payload })) {
        Ok(Response::Ok) => Ok(ReplyDelivery::Recorded),
        Ok(response) => Err(ControlError::unexpected(&response)),
        Err(error) if request_has_ended(&error) => Ok(ReplyDelivery::RequestEnded),
        Err(error) => Err(error),
    }
}

/// Ask through `send` whether a delivered request can still take a reply.
pub fn request_active_with(
    send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    request: u64,
) -> ControlResult<bool> {
    match checked(send(Command::CheckRequest { request })) {
        Ok(Response::Ok) => Ok(true),
        Ok(response) => Err(ControlError::unexpected(&response)),
        Err(error) if request_has_ended(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// An ended request stays known as `request_ended` while the broker retains
/// it, and becomes `not_found` once its record expires or is released.
fn request_has_ended(error: &ControlError) -> bool {
    error.is(&ErrorCode::RequestEnded) || error.is(&ErrorCode::NotFound)
}

impl RuntimeClient {
    /// Become the provider of a broker service this module holds a `provide`
    /// grant for. Registering the same service again is accepted.
    pub fn provide(&self, service: &str) -> ControlResult<()> {
        match self.send_checked(Command::Register {
            service: service.to_owned(),
        })? {
            Response::Ok => Ok(()),
            response => Err(ControlError::unexpected(&response)),
        }
    }

    /// Become the provider of one approved method of a versioned API. The
    /// installed policy must name this module as the method's provider with
    /// exactly this schema.
    pub fn provide_api(&self, method: ApiMethod) -> ControlResult<()> {
        match self.send_checked(Command::RegisterApi { method })? {
            Response::Ok => Ok(()),
            response => Err(ControlError::unexpected(&response)),
        }
    }

    /// Take the next request delivered to this provider, across every service
    /// it registered. `None` means nothing is waiting.
    ///
    /// A provider that selected reliable delivery uses
    /// [`ReliableRequestReceiver`](super::ReliableRequestReceiver) instead.
    pub fn next_request(&self) -> ControlResult<Option<IncomingRequest<Value>>> {
        next_request_with(&mut |command| self.send(command))
    }

    /// Reply to a delivered request. A request that already ended reports
    /// [`ReplyDelivery::RequestEnded`]; that is not an error.
    pub fn reply<T: Serialize>(&self, request: u64, payload: &T) -> ControlResult<ReplyDelivery> {
        reply_with(&mut |command| self.send(command), request, payload)
    }

    /// Whether a delivered request can still take a reply. Check immediately
    /// before a side effect that should not happen for an abandoned request.
    /// This is an observation, not a lock: the request can still end before
    /// the effect or the reply.
    pub fn request_active(&self, request: u64) -> ControlResult<bool> {
        request_active_with(&mut |command| self.send(command), request)
    }
}

impl<T> IncomingRequest<T> {
    /// Reply to this request. See [`RuntimeClient::reply`].
    pub fn reply<R: Serialize>(
        &self,
        runtime: &RuntimeClient,
        payload: &R,
    ) -> ControlResult<ReplyDelivery> {
        runtime.reply(self.id, payload)
    }

    /// Whether this request can still take a reply. See
    /// [`RuntimeClient::request_active`].
    pub fn is_active(&self, runtime: &RuntimeClient) -> ControlResult<bool> {
        runtime.request_active(self.id)
    }
}

impl IncomingRequest<Value> {
    /// Decode the payload as the service's operation type, keeping the
    /// broker-attested identities beside it. A payload that does not decode
    /// returns the untouched request, so the provider can reply to it in its
    /// own service's terms.
    pub fn parse<T: DeserializeOwned>(self) -> Result<IncomingRequest<T>, Box<MalformedRequest>> {
        // Probe by reference first: deserializing by value would consume the
        // payload the provider still needs when it turns out to be malformed.
        match T::deserialize(&self.payload) {
            Ok(payload) => Ok(IncomingRequest {
                id: self.id,
                caller: self.caller,
                initiator: self.initiator,
                service: self.service,
                payload,
                remaining_ms: self.remaining_ms,
            }),
            Err(error) => Err(Box::new(MalformedRequest {
                reason: error.to_string(),
                request: self,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    fn delivery(payload: Value) -> Response {
        Response::Request {
            id: 4,
            caller: "caller".into(),
            initiator: "owner".into(),
            service: "example/1".into(),
            payload,
            remaining_ms: 250,
        }
    }

    #[test]
    fn delivery_keeps_broker_identities_and_distinguishes_an_empty_queue() {
        let request = next_request_with(&mut |command| {
            assert!(matches!(command, Command::Poll {}));
            Ok(delivery(json!({"action":"read"})))
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            (
                request.id,
                request.caller.as_str(),
                request.initiator.as_str()
            ),
            (4, "caller", "owner")
        );
        assert_eq!(
            (request.service.as_str(), request.remaining_ms),
            ("example/1", 250)
        );
        assert!(
            next_request_with(&mut |_| Ok(Response::Pending))
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            next_request_with(&mut |_| Ok(Response::Ok)),
            Err(ControlError::UnexpectedReply(_))
        ));
    }

    #[test]
    fn a_reply_to_an_ended_request_is_not_a_failure() {
        let reply =
            |response: Response| reply_with(&mut |_| Ok(response.clone()), 4, &json!({"ok":true}));
        assert_eq!(reply(Response::Ok), Ok(ReplyDelivery::Recorded));
        for code in ["request_ended", "not_found"] {
            assert_eq!(
                reply(Response::error(code, "ended")),
                Ok(ReplyDelivery::RequestEnded)
            );
        }
        // A reply the broker rejects for its content is the provider's bug.
        assert!(
            reply(Response::error(
                "invalid_request",
                "result payload is too large"
            ))
            .is_err_and(|error| error.is(&ErrorCode::InvalidRequest))
        );
        assert!(matches!(
            reply_with(&mut |_| Err("lost".into()), 4, &json!({})),
            Err(ControlError::Transport(_))
        ));
    }

    #[test]
    fn activity_check_reports_an_ended_request_without_failing() {
        let active = |response: Response| request_active_with(&mut |_| Ok(response.clone()), 4);
        assert_eq!(active(Response::Ok), Ok(true));
        assert_eq!(active(Response::error("request_ended", "ended")), Ok(false));
        assert_eq!(active(Response::error("not_found", "expired")), Ok(false));
        assert!(active(Response::error("permission_denied", "not yours")).is_err());
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
    enum Operation {
        Read { key: String },
    }

    #[test]
    fn parsing_keeps_identity_and_returns_a_malformed_request_intact() {
        let request = |payload| IncomingRequest {
            id: 9,
            caller: "caller".into(),
            initiator: "owner".into(),
            service: "example/1".into(),
            payload,
            remaining_ms: 100,
        };
        let parsed = request(json!({"action":"read","key":"k"}))
            .parse::<Operation>()
            .unwrap();
        assert_eq!(parsed.payload, Operation::Read { key: "k".into() });
        assert_eq!((parsed.id, parsed.caller.as_str()), (9, "caller"));

        let malformed = request(json!({"action":"read","key":"k","extra":1}))
            .parse::<Operation>()
            .unwrap_err();
        assert_eq!(malformed.request.id, 9);
        assert_eq!(malformed.request.payload["extra"], 1);
        assert!(!malformed.reason.is_empty());
    }
}
