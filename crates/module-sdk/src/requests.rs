//! Typed JSON helpers for the ordinary, asynchronous broker request protocol.
//!
//! Payloads are serialized across the ABI; no Rust object crosses the module
//! boundary. The runtime derives caller and initiator identities itself.
use crate::{Command, ModuleResult, Response, RuntimeClient};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

/// An inbound request and the broker-authenticated identities associated with it.
#[derive(Debug)]
pub struct IncomingRequest<T> {
    pub id: u64,
    pub caller: String,
    pub initiator: String,
    pub service: String,
    pub payload: T,
    pub remaining_ms: u64,
}

impl IncomingRequest<Value> {
    /// Decode without losing the request identity if the payload is malformed.
    pub fn decode<T: DeserializeOwned>(&self) -> ModuleResult<T> {
        serde_json::from_value(self.payload.clone()).map_err(|error| error.to_string())
    }
}

/// Request admission parameters. Reuse the same operation ID and unchanged
/// payload when retrying a lost admission reply; choose a new ID for new work.
pub struct RequestOptions<'a> {
    pub service: &'a str,
    pub timeout_ms: u64,
    pub operation_id: &'a str,
    pub parent: Option<u64>,
}

/// An exact installed provider method and broker request options.
pub struct ApiRequestOptions<'a> {
    pub request: RequestOptions<'a>,
    pub method: &'a str,
    pub interface_major: u32,
    pub interface_minor: u32,
}

impl RuntimeClient {
    /// Read agreed preparation constraints for this consumer's admitted launch.
    /// The coordinator checks its explicit read grant and the launch owner and
    /// operation binding; returned plans do not grant new mutation authority.
    pub fn request_prepared_plans(
        &self,
        options: RequestOptions<'_>,
        target: crate::extension_api::LifecycleTarget,
        owner: &str,
        application_operation_id: &str,
    ) -> ModuleResult<u64> {
        self.request_serialized(
            options,
            &crate::extension_api::PreparedPlansRequest {
                operation: "read_prepared_plans".into(),
                target,
                owner: owner.into(),
                application_operation_id: application_operation_id.into(),
            },
        )
    }
    /// Queue a read of the generic context for a dispatched lifecycle
    /// operation while its transition remains pending. The caller needs a broker grant, and the
    /// coordinator still authenticates the exact provider request.
    pub fn request_provider_context(
        &self,
        options: RequestOptions<'_>,
        provider_request: &crate::extension_api::ProviderRequest,
    ) -> ModuleResult<u64> {
        self.request_serialized(
            options,
            &crate::extension_api::ProviderContextRequest {
                operation: "read_provider_context".into(),
                provider_request: provider_request.clone(),
            },
        )
    }

    /// Queue serialized work. Acceptance is not confirmation of its effect.
    pub fn request_serialized<T: Serialize>(
        &self,
        options: RequestOptions<'_>,
        payload: &T,
    ) -> ModuleResult<u64> {
        let payload = serde_json::to_value(payload).map_err(|error| error.to_string())?;
        match self.send(Command::Request {
            service: options.service.to_owned(),
            payload,
            timeout_ms: options.timeout_ms,
            operation_id: options.operation_id.to_owned(),
            parent: options.parent,
        })? {
            Response::Accepted { id } => Ok(id),
            response => Err(unexpected_response(response)),
        }
    }

    /// Queue a call to an exact approved provider method and interface version.
    pub fn call_api_serialized<T: Serialize>(
        &self,
        options: ApiRequestOptions<'_>,
        payload: &T,
    ) -> ModuleResult<u64> {
        let payload = serde_json::to_value(payload).map_err(|error| error.to_string())?;
        match self.send(Command::CallApi {
            service: options.request.service.to_owned(),
            method: options.method.to_owned(),
            interface_major: options.interface_major,
            interface_minor: options.interface_minor,
            payload,
            timeout_ms: options.request.timeout_ms,
            operation_id: options.request.operation_id.to_owned(),
            parent: options.request.parent,
        })? {
            Response::Accepted { id } => Ok(id),
            response => Err(unexpected_response(response)),
        }
    }

    /// Poll for one provider request. `None` means no request is ready.
    pub fn poll_serialized(&self) -> ModuleResult<Option<IncomingRequest<Value>>> {
        match self.send(Command::Poll {})? {
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
            response => Err(unexpected_response(response)),
        }
    }

    /// Complete an inbound request with a serialized response.
    pub fn complete_serialized<T: Serialize>(&self, request: u64, payload: &T) -> ModuleResult<()> {
        let payload = serde_json::to_value(payload).map_err(|error| error.to_string())?;
        self.require_success(Command::Complete { request, payload })
    }

    /// Read a result without releasing its broker retry record. `None` means
    /// the operation is pending. Releasing or cancelling it is explicit.
    pub fn take_serialized<T: DeserializeOwned>(&self, request: u64) -> ModuleResult<Option<T>> {
        match self.send(Command::TakeResult { request })? {
            Response::Pending => Ok(None),
            Response::Result { payload } => serde_json::from_value(payload)
                .map(Some)
                .map_err(|error| error.to_string()),
            response => Err(unexpected_response(response)),
        }
    }
}

fn unexpected_response(response: Response) -> String {
    match response {
        Response::Error { code, message } => format!("{code}: {message}"),
        unexpected => format!("unexpected runtime response: {unexpected:?}"),
    }
}
