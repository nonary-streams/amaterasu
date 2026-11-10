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

/// An error reply: the broker's own refusal, or the terminal error it recorded
/// for a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerError {
    pub code: ErrorCode,
    pub message: String,
}

impl BrokerError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for BrokerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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
    /// The broker's code, when the broker answered.
    pub fn code(&self) -> Option<&ErrorCode> {
        match self {
            Self::Broker(error) => Some(&error.code),
            _ => None,
        }
    }

    /// Whether the broker answered with exactly this code.
    pub fn is(&self, code: &ErrorCode) -> bool {
        self.code() == Some(code)
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
        Ok(Response::Error { code, message }) => Err(ControlError::Broker(BrokerError::new(
            ErrorCode::parse(&code),
            message,
        ))),
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
}
