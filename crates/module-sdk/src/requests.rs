//! Typed JSON helpers for the ordinary, asynchronous broker request protocol.
//!
//! Payloads are serialized across the ABI; no Rust object crosses the module
//! boundary. The runtime derives caller and initiator identities itself.
mod call;
mod layer;
mod provider;

pub use call::{CallOutcome, OutboundCall, unique_operation_id};
pub use layer::{Answer, Relay};
pub use provider::{
    MalformedRequest, ReplyDelivery, next_request_with, refuse_with, reply_with,
    request_active_with,
};

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

/// Opt-in receiver for broker deliveries that must survive a lost poll reply.
/// Serialize calls through each receiver. Every instance has a unique broker
/// identity; a replacement cannot claim a live receiver's unaccepted offer.
/// An offer is retained locally until its acceptance is confirmed, so an
/// acceptance reply lost in transport cannot dispatch the handler twice.
pub struct ReliableRequestReceiver {
    receiver: String,
    offered: Option<IncomingRequest<Value>>,
    offer_started: Option<std::time::Instant>,
    poll_uncertain: bool,
    undispatched: Option<u64>,
}

impl ReliableRequestReceiver {
    /// Create a receiver with an unpredictable identity. Entropy failure is
    /// returned rather than reusing an identity from another provider instance.
    pub fn new() -> ModuleResult<Self> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)
            .map_err(|error| format!("receiver identity unavailable: {error}"))?;
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut receiver = String::with_capacity(32);
        for byte in bytes {
            receiver.push(HEX[(byte >> 4) as usize] as char);
            receiver.push(HEX[(byte & 15) as usize] as char);
        }
        Ok(Self {
            receiver,
            offered: None,
            offer_started: None,
            poll_uncertain: false,
            undispatched: None,
        })
    }
    /// Select reliable delivery. A broker may reject a mode transition while
    /// previous deliveries are still active; do not fall back to legacy poll.
    pub fn select(&mut self, runtime: &RuntimeClient) -> ModuleResult<()> {
        runtime.require_success(Command::SelectRequestDeliveryMode { reliable: true })
    }

    /// Permit a later legacy provider to select its delivery mode. Call only
    /// after all requests handed to handlers have actually retired; broker
    /// request completion alone does not prove native effects have stopped.
    pub fn deselect(&mut self, runtime: &RuntimeClient) -> ModuleResult<()> {
        if !self.is_idle() {
            return Err("reliable receiver still owns an offer".into());
        }
        runtime.require_success(Command::SelectRequestDeliveryMode { reliable: false })
    }

    /// Poll or retry acceptance. An error preserves any unaccepted offer for
    /// the next call. A returned request must be handed to its handler once.
    /// Check broker liveness immediately before effects; cancellation can
    /// still race that check and requires provider-specific reconciliation.
    pub fn poll(
        &mut self,
        runtime: &RuntimeClient,
    ) -> ModuleResult<Option<IncomingRequest<Value>>> {
        self.poll_with(&mut |command| runtime.send(command))
    }

    /// Resolve a lost poll reply without admitting new work. Returned requests
    /// still require handling and completion before the provider can retire.
    pub fn drain(
        &mut self,
        runtime: &RuntimeClient,
    ) -> ModuleResult<Option<IncomingRequest<Value>>> {
        self.drain_with(&mut |command| runtime.send(command))
    }

    /// This excludes work already handed to handlers, which the provider must
    /// track separately through completion.
    pub fn is_idle(&self) -> bool {
        self.offered.is_none() && !self.poll_uncertain && self.undispatched.is_none()
    }

    /// Confirm that a previously returned handler and all of its effects have
    /// actually retired. A false result means the broker request is still active;
    /// retry without releasing the provider or its resources.
    pub fn retire(&mut self, runtime: &RuntimeClient, request: u64) -> ModuleResult<bool> {
        self.retire_with(request, &mut |command| runtime.send(command))
    }

    fn retire_with(
        &self,
        request: u64,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<bool> {
        match send(Command::RetireRequestDelivery {
            request,
            receiver: self.receiver.clone(),
        })? {
            Response::Ok => Ok(true),
            Response::Error { code, .. } if code == "not_found" => Ok(true),
            Response::Error { code, .. } if code == "request_pending" => Ok(false),
            response => Err(format!(
                "unexpected delivery retirement response: {response:?}"
            )),
        }
    }

    fn retire_undispatched(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        if let Some(request) = self.undispatched
            && self.retire_with(request, send)?
        {
            self.undispatched = None;
        }
        Ok(())
    }

    fn drain_with(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<Option<IncomingRequest<Value>>> {
        if self.undispatched.is_some() {
            self.retire_undispatched(send)?;
            return Ok(None);
        }
        if self.offered.is_none() && self.poll_uncertain {
            let started = std::time::Instant::now();
            match send(Command::InspectRequestOffer {
                receiver: self.receiver.clone(),
            })? {
                Response::Pending => self.poll_uncertain = false,
                Response::Request {
                    id,
                    caller,
                    initiator,
                    service,
                    payload,
                    remaining_ms,
                } => {
                    self.offered = Some(IncomingRequest {
                        id,
                        caller,
                        initiator,
                        service,
                        payload,
                        remaining_ms,
                    });
                    self.offer_started = Some(started);
                    self.poll_uncertain = false;
                }
                response => {
                    return Err(format!(
                        "unexpected reliable offer inspection response: {response:?}"
                    ));
                }
            }
        }
        self.accept_offered(send)
    }

    fn poll_with(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<Option<IncomingRequest<Value>>> {
        if self.undispatched.is_some() {
            self.retire_undispatched(send)?;
            return Ok(None);
        }
        if self.offered.is_none() {
            // Charge time spent in the poll call as well as acceptance retries
            // against the broker's remaining deadline.
            let offer_started = std::time::Instant::now();
            self.poll_uncertain = true;
            match send(Command::PollV2 {
                receiver: self.receiver.clone(),
            })? {
                Response::Pending => {
                    self.poll_uncertain = false;
                    return Ok(None);
                }
                Response::Request {
                    id,
                    caller,
                    initiator,
                    service,
                    payload,
                    remaining_ms,
                } => {
                    self.offered = Some(IncomingRequest {
                        id,
                        caller,
                        initiator,
                        service,
                        payload,
                        remaining_ms,
                    });
                    self.offer_started = Some(offer_started);
                    self.poll_uncertain = false;
                }
                response => return Err(format!("unexpected reliable poll response: {response:?}")),
            }
        }
        self.accept_offered(send)
    }

    fn accept_offered(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<Option<IncomingRequest<Value>>> {
        let Some(request) = self.offered.as_ref().map(|offer| offer.id) else {
            return Ok(None);
        };
        match send(Command::AcceptRequest {
            request,
            receiver: self.receiver.clone(),
        })? {
            Response::Ok => {
                let mut request = self.offered.take().expect("offer retained");
                let elapsed = self
                    .offer_started
                    .take()
                    .expect("offer timestamp")
                    .elapsed()
                    .as_millis();
                request.remaining_ms = request
                    .remaining_ms
                    .saturating_sub(elapsed.min(u128::from(u64::MAX)) as u64);
                if request.remaining_ms == 0 {
                    // No handler ran. Wait for the broker deadline before
                    // retiring the delivery, without fabricating a result.
                    self.undispatched = Some(request.id);
                    return Ok(None);
                }
                Ok(Some(request))
            }
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                self.offered = None;
                self.offer_started = None;
                // An earlier acceptance reply may have been lost before the
                // request ended. Retire a possible accepted delivery even
                // though this receiver never dispatched it to a handler.
                self.undispatched = Some(request);
                Ok(None)
            }
            response => Err(format!(
                "unexpected reliable acceptance response: {response:?}"
            )),
        }
    }
}

#[cfg(test)]
mod reliable_delivery_tests {
    use super::*;

    fn offer() -> Response {
        Response::Request {
            id: 17,
            caller: "caller".into(),
            initiator: "initiator".into(),
            service: "service/1".into(),
            payload: serde_json::json!({"action":"write"}),
            remaining_ms: 500,
        }
    }

    #[test]
    fn lost_poll_and_acceptance_replies_do_not_dispatch_twice() {
        let mut receiver = ReliableRequestReceiver::new().unwrap();
        assert!(
            receiver
                .poll_with(&mut |_| Err("lost poll".into()))
                .is_err()
        );
        let mut calls = 0;
        assert!(
            receiver
                .poll_with(&mut |command| {
                    calls += 1;
                    match command {
                        Command::PollV2 { .. } => Ok(offer()),
                        Command::AcceptRequest { request: 17, .. } => Err("lost acceptance".into()),
                        _ => panic!("unexpected command"),
                    }
                })
                .is_err()
        );
        assert_eq!(calls, 2);
        let request = receiver
            .poll_with(&mut |command| {
                assert!(matches!(
                    command,
                    Command::AcceptRequest { request: 17, .. }
                ));
                Ok(Response::Ok)
            })
            .unwrap()
            .unwrap();
        assert_eq!(request.id, 17);
        assert!(
            receiver
                .poll_with(&mut |command| {
                    assert!(matches!(command, Command::PollV2 { .. }));
                    Ok(Response::Pending)
                })
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn expired_offer_is_not_dispatched() {
        let mut receiver = ReliableRequestReceiver::new().unwrap();
        assert!(
            receiver
                .poll_with(&mut |command| match command {
                    Command::PollV2 { .. } => Ok(offer()),
                    Command::AcceptRequest { request: 17, .. } =>
                        Ok(Response::error("request_ended", "expired")),
                    _ => panic!("unexpected command"),
                })
                .unwrap()
                .is_none()
        );
        assert!(!receiver.is_idle());
        assert!(
            receiver
                .drain_with(&mut |command| {
                    assert!(matches!(
                        command,
                        Command::RetireRequestDelivery { request: 17, .. }
                    ));
                    Ok(Response::error("not_found", "not accepted"))
                })
                .unwrap()
                .is_none()
        );
        assert!(receiver.is_idle());
    }

    #[test]
    fn lost_acceptance_then_expiry_still_retires_delivery() {
        let mut receiver = ReliableRequestReceiver::new().unwrap();
        assert!(
            receiver
                .poll_with(&mut |command| match command {
                    Command::PollV2 { .. } => Ok(offer()),
                    Command::AcceptRequest { .. } => Err("accept reply lost".into()),
                    _ => panic!("unexpected command"),
                })
                .is_err()
        );
        assert!(
            receiver
                .drain_with(&mut |command| {
                    assert!(matches!(
                        command,
                        Command::AcceptRequest { request: 17, .. }
                    ));
                    Ok(Response::error("request_ended", "expired"))
                })
                .unwrap()
                .is_none()
        );
        assert!(!receiver.is_idle());
        assert!(
            receiver
                .drain_with(&mut |command| {
                    assert!(matches!(
                        command,
                        Command::RetireRequestDelivery { request: 17, .. }
                    ));
                    Ok(Response::Ok)
                })
                .unwrap()
                .is_none()
        );
        assert!(receiver.is_idle());
    }

    #[test]
    fn delayed_acceptance_waits_for_deadline_without_dispatching() {
        let mut receiver = ReliableRequestReceiver::new().unwrap();
        assert!(
            receiver
                .poll_with(&mut |command| match command {
                    Command::PollV2 { .. } => Ok(offer()),
                    Command::AcceptRequest { .. } => Err("lost reply".into()),
                    _ => panic!("unexpected command"),
                })
                .is_err()
        );
        receiver.offer_started =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        assert!(
            receiver
                .poll_with(&mut |_| Ok(Response::Ok))
                .unwrap()
                .is_none()
        );
        assert!(!receiver.is_idle());
        assert!(
            receiver
                .drain_with(&mut |command| {
                    assert!(matches!(
                        command,
                        Command::RetireRequestDelivery { request: 17, .. }
                    ));
                    Ok(Response::error("request_pending", "not yet expired"))
                })
                .unwrap()
                .is_none()
        );
        assert!(!receiver.is_idle());
        assert!(
            receiver
                .drain_with(&mut |command| {
                    assert!(matches!(
                        command,
                        Command::RetireRequestDelivery { request: 17, .. }
                    ));
                    Err("retirement reply lost".into())
                })
                .is_err()
        );
        assert!(!receiver.is_idle());
        assert!(
            receiver
                .drain_with(&mut |command| {
                    assert!(matches!(
                        command,
                        Command::RetireRequestDelivery { request: 17, .. }
                    ));
                    Ok(Response::error("not_found", "already retired"))
                })
                .unwrap()
                .is_none()
        );
        assert!(receiver.is_idle());
    }

    #[test]
    fn submillisecond_deadline_rounding_never_dispatches() {
        let mut receiver = ReliableRequestReceiver::new().unwrap();
        let result = receiver
            .poll_with(&mut |command| match command {
                Command::PollV2 { .. } => match offer() {
                    Response::Request {
                        id,
                        caller,
                        initiator,
                        service,
                        payload,
                        ..
                    } => Ok(Response::Request {
                        id,
                        caller,
                        initiator,
                        service,
                        payload,
                        remaining_ms: 0,
                    }),
                    _ => unreachable!(),
                },
                Command::AcceptRequest { .. } => Ok(Response::Ok),
                _ => panic!("unexpected command"),
            })
            .unwrap();
        assert!(result.is_none());
        assert!(!receiver.is_idle());
    }

    #[test]
    fn drain_resolves_lost_poll_without_offering_new_work() {
        let mut receiver = ReliableRequestReceiver::new().unwrap();
        assert!(
            receiver
                .poll_with(&mut |_| Err("lost poll".into()))
                .is_err()
        );
        assert!(!receiver.is_idle());
        assert!(
            receiver
                .drain_with(&mut |command| {
                    assert!(matches!(command, Command::InspectRequestOffer { .. }));
                    Ok(Response::Pending)
                })
                .unwrap()
                .is_none()
        );
        assert!(receiver.is_idle());

        assert!(
            receiver
                .poll_with(&mut |_| Err("lost poll".into()))
                .is_err()
        );
        let request = receiver
            .drain_with(&mut |command| match command {
                Command::InspectRequestOffer { .. } => Ok(offer()),
                Command::AcceptRequest { request: 17, .. } => Ok(Response::Ok),
                _ => panic!("drain admitted new work"),
            })
            .unwrap()
            .unwrap();
        assert_eq!(request.id, 17);
        assert!(receiver.is_idle());
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

/// Build a processing-intent request, including the output range when known.
pub fn processing_intent_payload(
    target: &crate::extension_api::LifecycleTarget,
    revision: &str,
    settings: &Value,
    output_hdr: Option<bool>,
) -> ModuleResult<Value> {
    if !crate::processing_provider::valid_intent_revision(revision) || !settings.is_object() {
        return Err("invalid processing intent".into());
    }
    let mut payload = serde_json::json!({"target":target,"revision":revision,"settings":settings});
    if let Some(output_hdr) = output_hdr {
        payload["output_hdr"] = Value::Bool(output_hdr);
    }
    Ok(payload)
}

/// Build a frozen-settings read for a consumer that dispatches requests from
/// its own workflow instead of calling `request_frozen_provider_settings`.
pub fn frozen_provider_settings_request(
    target: crate::extension_api::LifecycleTarget,
    provider: &str,
) -> crate::extension_api::FrozenProviderSettingsRequest {
    crate::extension_api::FrozenProviderSettingsRequest {
        operation: "read_frozen_provider_settings".into(),
        target,
        provider: provider.into(),
    }
}

#[cfg(test)]
mod processing_intent_tests {
    use super::*;

    #[test]
    fn output_range_is_included_when_known() {
        let target = serde_json::from_value(serde_json::json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1})).unwrap();
        let settings = serde_json::json!({"enabled":false});
        for hdr in [false, true] {
            let current =
                processing_intent_payload(&target, "revision", &settings, Some(hdr)).unwrap();
            assert_eq!(current["output_hdr"], hdr);
            assert_eq!(current["settings"], settings);
        }
        assert!(processing_intent_payload(&target, "", &settings, Some(true)).is_err());
        assert_eq!(
            processing_intent_payload(&target, "revision", &settings, None)
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            3
        );
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
        self.request_serialized(options, &frozen_provider_settings_request(target, provider))
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
    /// [`next_request`](Self::next_request) is the same operation with a
    /// typed error.
    pub fn poll_serialized(&self) -> ModuleResult<Option<IncomingRequest<Value>>> {
        Ok(self.next_request()?)
    }

    /// Complete an inbound request with a serialized response. A request that
    /// already ended is an error here; use [`reply`](Self::reply) to treat
    /// that as the ordinary outcome it is.
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
