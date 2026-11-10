//! Typed outcomes of control-plane operations.
//!
//! [`RuntimeClient::send`](crate::RuntimeClient::send) reports a failed
//! transport as a string and leaves a broker refusal inside the reply, so a
//! caller has to match on code strings to tell them apart. The helpers in
//! [`crate::requests`] return these types instead: a module decides from the
//! code, and `?` still works in a function that returns
//! [`ModuleResult`](crate::ModuleResult).
use crate::{ModuleResult, Response};
use std::fmt;

/// The code of an error reply.
///
/// The named variants are codes the runtime itself produces. A provider is free
/// to define codes for its own service; those arrive as [`ErrorCode::Other`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    /// The endpoint lacks the grant, or does not own the handle it named.
    PermissionDenied,
    /// The command, its payload or its limits were not acceptable.
    InvalidRequest,
    /// The handle, request or interface is unknown or no longer retained.
    NotFound,
    /// No provider is registered for the service, or a participant is absent.
    Unavailable,
    /// The operation identifier is already bound to different input.
    Conflict,
    /// Another delivery or receiver is still in flight.
    Busy,
    /// A bounded runtime table is full.
    ResourceExhausted,
    /// The request's deadline passed before a provider replied.
    DeadlineExceeded,
    /// The request's owner, or the end of its parent, cancelled it.
    Cancelled,
    /// The request already has a terminal result.
    RequestEnded,
    /// The request has no terminal result yet.
    RequestPending,
    /// The operation's retained result has expired.
    OperationExpired,
    /// A direct call was not served; nothing happened in the provider.
    DirectUnavailable,
    /// No approved method satisfies the requested interface version.
    IncompatibleApi,
    /// Any other code, including one a provider defines for its own service.
    Other(String),
}

impl ErrorCode {
    /// Classify a wire code. Unknown codes are preserved in [`ErrorCode::Other`].
    pub fn parse(code: &str) -> Self {
        match code {
            "permission_denied" => Self::PermissionDenied,
            "invalid_request" => Self::InvalidRequest,
            "not_found" => Self::NotFound,
            "unavailable" => Self::Unavailable,
            "conflict" => Self::Conflict,
            "busy" => Self::Busy,
            "resource_exhausted" => Self::ResourceExhausted,
            "deadline_exceeded" => Self::DeadlineExceeded,
            "cancelled" => Self::Cancelled,
            "request_ended" => Self::RequestEnded,
            "request_pending" => Self::RequestPending,
            "operation_expired" => Self::OperationExpired,
            crate::DIRECT_UNAVAILABLE => Self::DirectUnavailable,
            "incompatible_api" => Self::IncompatibleApi,
            other => Self::Other(other.to_owned()),
        }
    }

    /// The code as it appears on the wire.
    pub fn as_str(&self) -> &str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::InvalidRequest => "invalid_request",
            Self::NotFound => "not_found",
            Self::Unavailable => "unavailable",
            Self::Conflict => "conflict",
            Self::Busy => "busy",
            Self::ResourceExhausted => "resource_exhausted",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::Cancelled => "cancelled",
            Self::RequestEnded => "request_ended",
            Self::RequestPending => "request_pending",
            Self::OperationExpired => "operation_expired",
            Self::DirectUnavailable => crate::DIRECT_UNAVAILABLE,
            Self::IncompatibleApi => "incompatible_api",
            Self::Other(code) => code,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Who decided an error reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorOrigin {
    /// The broker itself: a missing grant, a full table, an ended request.
    Broker,
    /// The principal serving the request, a provider or a layer stacked on
    /// it, declined with [`refuse`](crate::RuntimeClient::refuse). It states
    /// that it did not perform the operation.
    Provider,
}

/// An error reply: the broker's own refusal, the terminal error it recorded
/// for a request, or a serving principal's refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerError {
    pub code: ErrorCode,
    pub message: String,
    pub origin: ErrorOrigin,
}

impl BrokerError {
    /// An error the broker itself decided.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            origin: ErrorOrigin::Broker,
        }
    }

    /// Classify an error reply as it appears on the wire. A serving
    /// principal's refusal arrives as `refused.<code>`; the prefix becomes the
    /// origin and `<code>` is classified like any other.
    pub fn from_wire(code: &str, message: impl Into<String>) -> Self {
        let (origin, code) = match code.strip_prefix(crate::REFUSED_PREFIX) {
            Some(code) => (ErrorOrigin::Provider, code),
            None => (ErrorOrigin::Broker, code),
        };
        Self {
            code: ErrorCode::parse(code),
            message: message.into(),
            origin,
        }
    }

    /// Whether the serving provider or a layer declined the request.
    pub fn is_refusal(&self) -> bool {
        self.origin == ErrorOrigin::Provider
    }
}

impl fmt::Display for BrokerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_refusal() {
            formatter.write_str(crate::REFUSED_PREFIX)?;
        }
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

/// Why a control operation did not produce the reply it defines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlError {
    /// The command or its reply was lost in transport. The broker may or may
    /// not have acted on it.
    Transport(String),
    /// The broker answered with an error.
    Broker(BrokerError),
    /// The broker answered with a reply this operation does not define.
    UnexpectedReply(String),
    /// A payload could not be encoded for sending or decoded after receipt.
    Payload(String),
}

impl ControlError {
    /// The code of the error reply, whoever decided it.
    pub fn code(&self) -> Option<&ErrorCode> {
        match self {
            Self::Broker(error) => Some(&error.code),
            _ => None,
        }
    }

    /// Whether the broker itself answered with exactly this code. A serving
    /// principal's refusal never matches, so a provider that refuses with
    /// `not_found` is not mistaken for a handle the broker no longer holds.
    pub fn is(&self, code: &ErrorCode) -> bool {
        matches!(self, Self::Broker(error) if !error.is_refusal() && error.code == *code)
    }

    /// The serving principal's refusal, when that is what this error is.
    pub fn refusal(&self) -> Option<&BrokerError> {
        match self {
            Self::Broker(error) if error.is_refusal() => Some(error),
            _ => None,
        }
    }

    pub(crate) fn unexpected(response: &Response) -> Self {
        Self::UnexpectedReply(format!("unexpected runtime response: {response:?}"))
    }

    pub(crate) fn payload(error: serde_json::Error) -> Self {
        Self::Payload(error.to_string())
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) | Self::UnexpectedReply(message) | Self::Payload(message) => {
                formatter.write_str(message)
            }
            Self::Broker(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ControlError {}

impl From<BrokerError> for ControlError {
    fn from(error: BrokerError) -> Self {
        Self::Broker(error)
    }
}

impl From<ControlError> for String {
    fn from(error: ControlError) -> Self {
        error.to_string()
    }
}

pub type ControlResult<T> = Result<T, ControlError>;

/// Separate a lost exchange and a broker refusal from an ordinary reply.
///
/// This is the classification behind the typed helpers. Call it directly when
/// a command is sent through an injected transport, as unit tests do.
pub fn checked(exchange: ModuleResult<Response>) -> ControlResult<Response> {
    match exchange {
        Err(message) => Err(ControlError::Transport(message)),
        Ok(Response::Error { code, message }) => {
            Err(ControlError::Broker(BrokerError::from_wire(&code, message)))
        }
        Ok(response) => Ok(response),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_code_round_trips_and_unknown_codes_are_preserved() {
        for code in [
            "permission_denied",
            "invalid_request",
            "not_found",
            "unavailable",
            "conflict",
            "busy",
            "resource_exhausted",
            "deadline_exceeded",
            "cancelled",
            "request_ended",
            "request_pending",
            "operation_expired",
            "direct_unavailable",
            "incompatible_api",
        ] {
            let parsed = ErrorCode::parse(code);
            assert!(!matches!(parsed, ErrorCode::Other(_)), "{code}");
            assert_eq!(parsed.as_str(), code);
        }
        let provider_code = ErrorCode::parse("session_not_found");
        assert_eq!(provider_code, ErrorCode::Other("session_not_found".into()));
        assert_eq!(provider_code.as_str(), "session_not_found");
    }

    #[test]
    fn classification_separates_transport_refusal_and_reply() {
        assert_eq!(
            checked(Err("runtime transport failed with status 2".into())).unwrap_err(),
            ControlError::Transport("runtime transport failed with status 2".into())
        );
        let refused = checked(Ok(Response::error("permission_denied", "not granted")));
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.is(&ErrorCode::PermissionDenied))
        );
        assert_eq!(
            String::from(refused.unwrap_err()),
            "permission_denied: not granted"
        );
        assert!(matches!(
            checked(Ok(Response::Pending)),
            Ok(Response::Pending)
        ));
    }

    #[test]
    fn a_serving_principals_refusal_is_told_apart_from_the_brokers_own_code() {
        let refusal =
            checked(Ok(Response::error("refused.not_found", "no such session"))).unwrap_err();
        // The code is classified like any other, so a caller can match on it.
        assert_eq!(refusal.code(), Some(&ErrorCode::NotFound));
        assert_eq!(
            refusal.refusal().map(|refusal| refusal.origin),
            Some(ErrorOrigin::Provider)
        );
        // It is not the broker saying a handle is unknown.
        assert!(!refusal.is(&ErrorCode::NotFound));
        assert_eq!(refusal.to_string(), "refused.not_found: no such session");

        let broker = checked(Ok(Response::error("not_found", "unknown handle"))).unwrap_err();
        assert!(broker.is(&ErrorCode::NotFound));
        assert!(broker.refusal().is_none());

        let custom = BrokerError::from_wire("refused.stream_active", "a stream is running");
        assert_eq!(custom.code, ErrorCode::Other("stream_active".into()));
        assert!(custom.is_refusal());
    }
}
