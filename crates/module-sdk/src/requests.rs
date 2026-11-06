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

/// Validated full settings for a new revision, scoped to the requested provider.
pub struct LiveProviderSettings {
    pub revision: String,
    pub settings: Value,
}

/// Build the selected processing-intent contract. Version 1.0 keeps its exact
/// legacy shape; 1.1 adds the requested output range so a provider can reserve
/// a compatible processing path even when its effect is initially disabled.
pub fn processing_intent_payload(
    interface_minor: u32,
    target: &crate::extension_api::LifecycleTarget,
    revision: &str,
    settings: &Value,
    output_hdr: Option<bool>,
) -> ModuleResult<Value> {
    if interface_minor > 1
        || revision.is_empty()
        || revision.len() > 128
        || revision.chars().any(char::is_control)
        || !settings.is_object()
    {
        return Err("invalid processing intent".into());
    }
    let mut payload = serde_json::json!({"target":target,"revision":revision,"settings":settings});
    if interface_minor == 1
        && let Some(output_hdr) = output_hdr
    {
        payload["output_hdr"] = Value::Bool(output_hdr);
    }
    Ok(payload)
}

#[cfg(test)]
mod processing_intent_tests {
    use super::*;

    #[test]
    fn output_range_is_only_sent_with_the_selected_version_one_one_contract() {
        let target = serde_json::from_value(serde_json::json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1})).unwrap();
        let settings = serde_json::json!({"enabled":false});
        let legacy =
            processing_intent_payload(0, &target, "revision", &settings, Some(true)).unwrap();
        assert_eq!(legacy.as_object().unwrap().len(), 3);
        for hdr in [false, true] {
            let current =
                processing_intent_payload(1, &target, "revision", &settings, Some(hdr)).unwrap();
            assert_eq!(current["output_hdr"], hdr);
            assert_eq!(current["settings"], settings);
        }
        assert!(processing_intent_payload(2, &target, "revision", &settings, Some(true)).is_err());
    }
}

/// Build a retryable live-settings admission for an exact stream and provider.
/// Reuse the returned command unchanged if the admission response is lost.
pub fn live_provider_settings_command(
    options: RequestOptions<'_>,
    target: crate::extension_api::LifecycleTarget,
    provider: &str,
) -> ModuleResult<Command> {
    if !matches!(target, crate::extension_api::LifecycleTarget::Stream { .. })
        || !serde_json::to_vec(&target).is_ok_and(|bytes| bytes.len() <= 1024)
        || provider.is_empty()
        || provider.len() > 128
    {
        return Err("invalid live settings target or provider".into());
    }
    let payload = serde_json::to_value(crate::extension_api::LiveProviderSettingsRequest {
        operation: "read_live_provider_settings".into(),
        target,
        provider: provider.into(),
    })
    .map_err(|error| error.to_string())?;
    Ok(Command::Request {
        service: options.service.to_owned(),
        payload,
        timeout_ms: options.timeout_ms,
        operation_id: options.operation_id.to_owned(),
        parent: options.parent,
    })
}

/// Validate a live-settings response against the trusted admitted stream and
/// provider. `None` means the read is pending or the revision is unchanged.
/// A returned update is an observation, not proof the consumer applied it.
pub fn validate_live_provider_settings(
    payload: &Value,
    target: &crate::extension_api::LifecycleTarget,
    provider: &str,
    current_revision: &str,
) -> ModuleResult<Option<LiveProviderSettings>> {
    if !matches!(target, crate::extension_api::LifecycleTarget::Stream { .. })
        || !serde_json::to_vec(target).is_ok_and(|bytes| bytes.len() <= 1024)
        || provider.is_empty()
        || provider.len() > 128
        || current_revision.is_empty()
        || current_revision.len() > 128
    {
        return Err("invalid live settings reference".into());
    }
    if payload.get("status").and_then(Value::as_str) == Some("pending") {
        return Ok(None);
    }
    let settings = payload
        .get("settings")
        .ok_or("invalid live settings response")?;
    if !settings.is_object()
        || !serde_json::to_vec(settings).is_ok_and(|bytes| bytes.len() <= 16 * 1024)
    {
        return Err("invalid live settings response".into());
    }
    // Ignore extra top-level fields while validating the published response type.
    let fields = ["status", "target", "provider", "revision", "settings"]
        .into_iter()
        .filter_map(|name| {
            payload
                .get(name)
                .map(|value| (name.to_owned(), value.clone()))
        })
        .collect();
    let response: crate::extension_api::LiveProviderSettingsResponse =
        serde_json::from_value(Value::Object(fields))
            .map_err(|_| "invalid live settings response")?;
    if response.status != "succeeded"
        || response.target != *target
        || response.provider != provider
        || response.revision.is_empty()
        || response.revision.len() > 128
    {
        return Err("invalid live settings response".into());
    }
    if response.revision == current_revision {
        return Ok(None);
    }
    Ok(Some(LiveProviderSettings {
        revision: response.revision,
        settings: response.settings,
    }))
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
    /// Request the frozen settings of an explicitly granted provider for an
    /// exact ready session or stream. The broker and lifecycle coordinator
    /// authenticate the consumer and its installed provider-specific grant.
    pub fn request_frozen_provider_settings(
        &self,
        options: RequestOptions<'_>,
        target: crate::extension_api::LifecycleTarget,
        provider: &str,
    ) -> ModuleResult<u64> {
        self.request_serialized(
            options,
            &crate::extension_api::FrozenProviderSettingsRequest {
                operation: "read_frozen_provider_settings".into(),
                target,
                provider: provider.into(),
            },
        )
    }

    /// Request a provider's live-adjustable settings for an exact stream.
    /// Pending responses can be retried with a new request; successful reads
    /// do not establish that the consumer applied the returned settings.
    pub fn request_live_provider_settings(
        &self,
        options: RequestOptions<'_>,
        target: crate::extension_api::LifecycleTarget,
        provider: &str,
    ) -> ModuleResult<u64> {
        match self.send(live_provider_settings_command(options, target, provider)?)? {
            Response::Accepted { id } => Ok(id),
            response => Err(unexpected_response(response)),
        }
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

#[cfg(test)]
mod live_settings_tests {
    use super::*;
    use crate::extension_api::LifecycleTarget;
    use serde_json::json;

    fn target() -> LifecycleTarget {
        LifecycleTarget::Stream {
            host_epoch: "epoch".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        }
    }

    #[test]
    fn command_preserves_retry_identity_and_exact_scope() {
        let command = live_provider_settings_command(
            RequestOptions {
                service: "lifecycle",
                timeout_ms: 5000,
                operation_id: "op",
                parent: Some(4),
            },
            target(),
            "effect",
        )
        .unwrap();
        match command {
            Command::Request {
                service,
                timeout_ms,
                operation_id,
                parent,
                payload,
            } => {
                assert_eq!(
                    (service.as_str(), timeout_ms, operation_id.as_str(), parent),
                    ("lifecycle", 5000, "op", Some(4))
                );
                assert_eq!(
                    payload,
                    json!({"operation":"read_live_provider_settings", "target":target(), "provider":"effect"})
                );
            }
            _ => panic!("expected request"),
        }
    }

    #[test]
    fn validates_scope_revision_size_and_pending() {
        let target = target();
        let valid = json!({"status":"succeeded", "target":target, "provider":"effect", "revision":"new", "settings":{"gain":2}});
        let validate =
            |payload: &Value| validate_live_provider_settings(payload, &target, "effect", "old");
        assert!(validate(&json!({"status":"pending"})).unwrap().is_none());
        let update = validate(&valid).unwrap().unwrap();
        assert_eq!(update.revision, "new");
        assert_eq!(update.settings, json!({"gain":2}));
        let mut changed = valid.clone();
        changed["future_field"] = json!(true);
        assert!(validate(&changed).unwrap().is_some());
        changed["revision"] = json!("old");
        assert!(validate(&changed).unwrap().is_none());
        for (field, value) in [
            ("provider", json!("other")),
            ("target", json!({})),
            ("revision", json!("")),
            ("revision", json!("x".repeat(129))),
            ("settings", json!([])),
            ("settings", json!({"value":"x".repeat(16384)})),
            ("status", json!("rejected")),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(validate(&invalid).is_err(), "accepted invalid {field}");
        }
        let mut oversized = target.clone();
        if let LifecycleTarget::Stream { stream_id, .. } = &mut oversized {
            *stream_id = "x".repeat(1024);
        }
        assert!(validate_live_provider_settings(&valid, &oversized, "effect", "old").is_err());
    }
}
