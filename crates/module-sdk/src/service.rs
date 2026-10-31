//! Typed helpers for the broker's installed, versioned API methods.
//!
//! A contract describes a method already approved by trusted policy; implementing
//! this trait does not grant permission or register a provider. Calls remain
//! asynchronous: admission is not execution, and cancellation does not undo work.
use crate::{ApiMethod, Command, Response, RuntimeClient, requests::IncomingRequest};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{fmt, marker::PhantomData};

/// A method's shared wire contract. Both provider and consumer implement or
/// import the same definition, including its exact approved schema and version.
pub trait ServiceMethod {
    type Request: Serialize + DeserializeOwned;
    type Response: Serialize + DeserializeOwned;

    fn method() -> ApiMethod;
}

/// Transport and broker failures are separate from a method's domain response.
#[derive(Debug, PartialEq, Eq)]
pub enum ServiceError {
    Transport(String),
    Broker { code: String, message: String },
    UnexpectedResponse(String),
    InvalidPayload(String),
    WrongMethod,
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "transport: {error}"),
            Self::Broker { code, message } => write!(formatter, "{code}: {message}"),
            Self::UnexpectedResponse(response) => {
                write!(formatter, "unexpected runtime response: {response}")
            }
            Self::InvalidPayload(error) => write!(formatter, "invalid payload: {error}"),
            Self::WrongMethod => {
                formatter.write_str("request targets a different service method or version")
            }
        }
    }
}
impl std::error::Error for ServiceError {}

fn response_error(response: Response) -> ServiceError {
    match response {
        Response::Error { code, message } => ServiceError::Broker { code, message },
        other => ServiceError::UnexpectedResponse(format!("{other:?}")),
    }
}

/// Admission options. Retrying an uncertain admission requires the same ID and
/// payload; a new operation requires a new ID. Parent propagates initiator authority.
pub struct CallOptions<'a> {
    pub timeout_ms: u64,
    pub operation_id: &'a str,
    pub parent: Option<u64>,
}

/// A broker request token bound to its response type. Polling is nonblocking;
/// retry records are retained until explicitly released.
pub struct ServiceCall<'a, M: ServiceMethod> {
    runtime: &'a RuntimeClient,
    id: u64,
    method: PhantomData<fn() -> M>,
}

impl<M: ServiceMethod> ServiceCall<'_, M> {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn poll(&self) -> Result<Option<M::Response>, ServiceError> {
        match self
            .runtime
            .send(Command::TakeResult { request: self.id })
            .map_err(ServiceError::Transport)?
        {
            Response::Pending => Ok(None),
            Response::Result { payload } => serde_json::from_value(payload)
                .map(Some)
                .map_err(|error| ServiceError::InvalidPayload(error.to_string())),
            other => Err(response_error(other)),
        }
    }

    /// Request cancellation; work already performed is not rolled back.
    pub fn cancel(&self) -> Result<(), ServiceError> {
        success(
            self.runtime
                .send(Command::Cancel { request: self.id })
                .map_err(ServiceError::Transport)?,
        )
    }

    /// Release the retry record once the broker permits it. On failure the
    /// token is returned so the caller can retry or cancel it.
    pub fn release(self) -> Result<(), (Self, ServiceError)> {
        let result = self
            .runtime
            .send(Command::ReleaseResult { request: self.id })
            .map_err(ServiceError::Transport)
            .and_then(success);
        result.map_err(|error| (self, error))
    }
}

fn success(response: Response) -> Result<(), ServiceError> {
    match response {
        Response::Ok => Ok(()),
        other => Err(response_error(other)),
    }
}

/// Register the exact method selected by trusted policy for this provider.
pub fn register<M: ServiceMethod>(runtime: &RuntimeClient) -> Result<(), ServiceError> {
    success(
        runtime
            .send(Command::RegisterApi {
                method: M::method(),
            })
            .map_err(ServiceError::Transport)?,
    )
}

/// Submit a typed call using the broker's version and authorization checks.
pub fn call<'a, M: ServiceMethod>(
    runtime: &'a RuntimeClient,
    options: CallOptions<'_>,
    request: &M::Request,
) -> Result<ServiceCall<'a, M>, ServiceError> {
    let method = M::method();
    let payload = serde_json::to_value(request)
        .map_err(|error| ServiceError::InvalidPayload(error.to_string()))?;
    match runtime
        .send(Command::CallApi {
            service: method.service,
            method: method.method,
            interface_major: method.interface_major,
            interface_minor: method.interface_minor,
            payload,
            timeout_ms: options.timeout_ms,
            operation_id: options.operation_id.to_owned(),
            parent: options.parent,
        })
        .map_err(ServiceError::Transport)?
    {
        Response::Accepted { id } => Ok(ServiceCall {
            runtime,
            id,
            method: PhantomData,
        }),
        other => Err(response_error(other)),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    method: String,
    interface_major: u32,
    interface_minor: u32,
    arguments: Value,
}

/// A request decoded for one exact method and its authenticated context.
pub struct IncomingCall<M: ServiceMethod> {
    pub request: IncomingRequest<M::Request>,
    method: PhantomData<fn() -> M>,
}

/// Decode a broker-delivered method without substituting caller identities.
/// The original request remains available to route another method or respond
/// to malformed input according to the installed response schema.
pub fn decode<M: ServiceMethod>(
    request: &IncomingRequest<Value>,
) -> Result<IncomingCall<M>, ServiceError> {
    let expected = M::method();
    let envelope: Envelope = serde_json::from_value(request.payload.clone())
        .map_err(|error| ServiceError::InvalidPayload(error.to_string()))?;
    if request.service != expected.service
        || envelope.method != expected.method
        || envelope.interface_major != expected.interface_major
        || envelope.interface_minor != expected.interface_minor
    {
        return Err(ServiceError::WrongMethod);
    }
    let payload = serde_json::from_value(envelope.arguments)
        .map_err(|error| ServiceError::InvalidPayload(error.to_string()))?;
    Ok(IncomingCall {
        request: IncomingRequest {
            id: request.id,
            caller: request.caller.clone(),
            initiator: request.initiator.clone(),
            service: request.service.clone(),
            payload,
            remaining_ms: request.remaining_ms,
        },
        method: PhantomData,
    })
}

/// Complete a decoded request with the corresponding method response.
pub fn complete<M: ServiceMethod>(
    runtime: &RuntimeClient,
    request: &IncomingCall<M>,
    response: &M::Response,
) -> Result<(), ServiceError> {
    let payload = serde_json::to_value(response)
        .map_err(|error| ServiceError::InvalidPayload(error.to_string()))?;
    success(
        runtime
            .send(Command::Complete {
                request: request.request.id,
                payload,
            })
            .map_err(ServiceError::Transport)?,
    )
}
