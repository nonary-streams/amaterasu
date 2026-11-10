//! One outbound request, driven from admission to a settled outcome.
//!
//! A queued request takes several exchanges: submit it, collect its result on
//! a later tick, then release the broker's record. Any of those replies can be
//! lost, and the request can end without a reply. [`OutboundCall`] owns that
//! sequence so a module states what it wants and reads one of three outcomes.
use super::{ApiRequestOptions, RequestOptions};
use crate::control_error::{BrokerError, ControlError, ControlResult, ErrorCode, checked};
use crate::{Command, ModuleResult, Response, RuntimeClient};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

/// The broker retains a finished request, and with it the operation identity,
/// for this long. Inside the window an identical submission returns the
/// original request instead of admitting a second one.
const ADMISSION_RETRY_WINDOW: Duration = Duration::from_secs(300);

/// Consecutive lost exchanges after which a call stops retrying.
const MAX_CONSECUTIVE_FAILURES: u8 = 3;

/// How a call ended.
#[derive(Clone, Debug, PartialEq)]
pub enum CallOutcome<T = Value> {
    /// The provider replied. The broker's record of the call has been released.
    Completed(T),
    /// The broker never admitted the request, so no provider saw it. Starting
    /// a new call is safe.
    Refused(BrokerError),
    /// The request was, or may have been, admitted and ended without a usable
    /// reply. The provider may have acted on it. For a mutating call the
    /// broker's record is left in place, so repeating the same operation
    /// identifier and payload cannot run the operation twice while it lasts.
    Unconfirmed(ControlError),
}

impl CallOutcome<Value> {
    /// Decode a completed reply. A reply of the wrong shape means the provider
    /// acted but said something unusable, which is [`CallOutcome::Unconfirmed`].
    pub fn decode<T: DeserializeOwned>(self) -> CallOutcome<T> {
        match self {
            Self::Completed(reply) => match serde_json::from_value(reply) {
                Ok(reply) => CallOutcome::Completed(reply),
                Err(error) => CallOutcome::Unconfirmed(ControlError::payload(error)),
            },
            Self::Refused(refusal) => CallOutcome::Refused(refusal),
            Self::Unconfirmed(error) => CallOutcome::Unconfirmed(error),
        }
    }
}

/// The exact request the broker is asked to admit. It never changes, because
/// the broker matches a retry against the complete original submission.
struct Submission {
    service: String,
    api: Option<ApiSelection>,
    payload: Value,
    timeout_ms: u64,
    operation_id: String,
    parent: Option<u64>,
}

struct ApiSelection {
    method: String,
    interface_major: u32,
    interface_minor: u32,
}

impl Submission {
    fn admit(&self) -> Command {
        match &self.api {
            None => Command::Request {
                service: self.service.clone(),
                payload: self.payload.clone(),
                timeout_ms: self.timeout_ms,
                operation_id: self.operation_id.clone(),
                parent: self.parent,
            },
            Some(api) => Command::CallApi {
                service: self.service.clone(),
                method: api.method.clone(),
                interface_major: api.interface_major,
                interface_minor: api.interface_minor,
                payload: self.payload.clone(),
                timeout_ms: self.timeout_ms,
                operation_id: self.operation_id.clone(),
                parent: self.parent,
            },
        }
    }

    /// Finds the request this submission created, without admitting one.
    fn look_up(&self) -> Command {
        match &self.api {
            None => Command::LookupRequest {
                service: self.service.clone(),
                payload: self.payload.clone(),
                operation_id: self.operation_id.clone(),
                parent: self.parent,
            },
            Some(api) => Command::LookupApiRequest {
                service: self.service.clone(),
                method: api.method.clone(),
                interface_major: api.interface_major,
                interface_minor: api.interface_minor,
                payload: self.payload.clone(),
                operation_id: self.operation_id.clone(),
                parent: self.parent,
            },
        }
    }
}

enum Stage {
    /// Nothing has been sent.
    Unsent,
    /// A submission was sent and its reply was lost.
    AdmissionUnknown,
    /// The broker admitted the request under this identifier.
    Admitted { request: u64, cancel_sent: bool },
    /// The provider's reply is held until its broker record is released.
    Releasing { request: u64, reply: Value },
    /// A read-only request ended without a reply; its record is being released.
    Discarding { request: u64, ending: ControlError },
    /// The outcome has been returned.
    Settled,
}

/// A request to another module, polled once per tick until it settles.
///
/// ```ignore
/// let mut call = OutboundCall::request(options, &json!({"action": "status"}))?.read_only();
/// // each tick:
/// if let Poll::Ready(outcome) = call.poll(&runtime) {
///     match outcome.decode::<Status>() {
///         CallOutcome::Completed(status) => ...,
///         CallOutcome::Refused(refusal) => ...,
///         CallOutcome::Unconfirmed(error) => ...,
///     }
/// }
/// ```
///
/// A call is mutating unless it is declared [`read_only`](Self::read_only).
/// Dropping a call that has not settled leaves its request with the broker
/// until the request's deadline and retention pass; [`cancel`](Self::cancel)
/// and keep polling to end it sooner.
pub struct OutboundCall {
    submission: Submission,
    read_only: bool,
    cancel_requested: bool,
    stage: Stage,
    first_sent: Option<Instant>,
    consecutive_failures: u8,
}

impl OutboundCall {
    /// A request to a service's ordinary interface.
    pub fn request<T: Serialize>(options: RequestOptions<'_>, payload: &T) -> ControlResult<Self> {
        Self::new(options, None, payload)
    }

    /// A request to one approved method of a versioned API.
    pub fn api<T: Serialize>(options: ApiRequestOptions<'_>, payload: &T) -> ControlResult<Self> {
        let api = ApiSelection {
            method: options.method.to_owned(),
            interface_major: options.interface_major,
            interface_minor: options.interface_minor,
        };
        Self::new(options.request, Some(api), payload)
    }

    fn new<T: Serialize>(
        options: RequestOptions<'_>,
        api: Option<ApiSelection>,
        payload: &T,
    ) -> ControlResult<Self> {
        Ok(Self {
            submission: Submission {
                service: options.service.to_owned(),
                api,
                payload: serde_json::to_value(payload).map_err(ControlError::payload)?,
                timeout_ms: options.timeout_ms,
                operation_id: options.operation_id.to_owned(),
                parent: options.parent,
            },
            read_only: false,
            cancel_requested: false,
            stage: Stage::Unsent,
            first_sent: None,
            consecutive_failures: 0,
        })
    }

    /// Declare that the provider changes no state for this call. A read-only
    /// call that ends without a reply has its broker record released at once,
    /// because nothing needs protecting from a repeat.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Ask for the request to end. Keep polling: a reply the provider already
    /// recorded is still returned, and the outcome says what is known.
    pub fn cancel(&mut self) {
        self.cancel_requested = true;
    }

    /// The broker's identifier, once the request is known to be admitted.
    pub fn request_id(&self) -> Option<u64> {
        match self.stage {
            Stage::Admitted { request, .. }
            | Stage::Releasing { request, .. }
            | Stage::Discarding { request, .. } => Some(request),
            Stage::Unsent | Stage::AdmissionUnknown | Stage::Settled => None,
        }
    }

    /// Whether a submission was ever sent. A call that was never sent can be
    /// dropped without leaving anything with the broker.
    pub fn was_submitted(&self) -> bool {
        self.first_sent.is_some()
    }

    pub fn operation_id(&self) -> &str {
        &self.submission.operation_id
    }

    /// Whether the outcome has already been returned.
    pub fn is_settled(&self) -> bool {
        matches!(self.stage, Stage::Settled)
    }

    /// Advance the call. Returns the outcome exactly once.
    pub fn poll(&mut self, runtime: &RuntimeClient) -> Poll<CallOutcome> {
        self.poll_at(Instant::now(), &mut |command| runtime.send(command))
    }

    /// Advance the call through an injected transport, as of `now`.
    pub fn poll_at(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<CallOutcome> {
        match std::mem::replace(&mut self.stage, Stage::Settled) {
            Stage::Unsent => self.submit(now, send),
            Stage::AdmissionUnknown => self.resolve_admission(now, send),
            Stage::Admitted {
                request,
                cancel_sent,
            } => self.collect(request, cancel_sent, send),
            Stage::Releasing { request, reply } => self.release_reply(request, reply, send),
            Stage::Discarding { request, ending } => self.discard(request, ending, send),
            Stage::Settled => Poll::Ready(CallOutcome::Unconfirmed(ControlError::UnexpectedReply(
                "the call was polled after it settled".into(),
            ))),
        }
    }

    fn submit(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<CallOutcome> {
        if self.cancel_requested {
            return Poll::Ready(CallOutcome::Refused(BrokerError::new(
                ErrorCode::Cancelled,
                "cancelled before submission",
            )));
        }
        self.first_sent = Some(now);
        match checked(send(self.submission.admit())) {
            Ok(Response::Accepted { id }) => self.admitted(id),
            Err(ControlError::Broker(refusal)) => Poll::Ready(CallOutcome::Refused(refusal)),
            Ok(response) => {
                self.exchange_lost(ControlError::unexpected(&response), Stage::AdmissionUnknown)
            }
            Err(error) => self.exchange_lost(error, Stage::AdmissionUnknown),
        }
    }

    /// The first submission may or may not have reached the broker. Repeating
    /// it is safe inside the retention window: an admitted request is found by
    /// its operation identity and returned, never admitted twice. A cancelled
    /// call only looks the request up, so it cannot start work it no longer
    /// wants.
    fn resolve_admission(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<CallOutcome> {
        if self.retry_window_closed(now) {
            return Poll::Ready(CallOutcome::Unconfirmed(ControlError::Transport(
                "request admission was never confirmed".into(),
            )));
        }
        let command = if self.cancel_requested {
            self.submission.look_up()
        } else {
            self.submission.admit()
        };
        match checked(send(command)) {
            Ok(Response::Accepted { id }) => self.admitted(id),
            Err(ControlError::Broker(refusal)) => {
                // An admitted request is retained for the whole window, so a
                // refusal inside it proves the first submission never arrived.
                // Read the clock again: the exchange itself may have stalled.
                if self.retry_window_closed(Instant::now().max(now)) {
                    return Poll::Ready(CallOutcome::Unconfirmed(ControlError::Broker(refusal)));
                }
                if self.cancel_requested && refusal.code == ErrorCode::NotFound {
                    return Poll::Ready(CallOutcome::Refused(BrokerError::new(
                        ErrorCode::Cancelled,
                        "cancelled before admission",
                    )));
                }
                Poll::Ready(CallOutcome::Refused(refusal))
            }
            Ok(response) => {
                self.exchange_lost(ControlError::unexpected(&response), Stage::AdmissionUnknown)
            }
            Err(error) => self.exchange_lost(error, Stage::AdmissionUnknown),
        }
    }

    fn collect(
        &mut self,
        request: u64,
        mut cancel_sent: bool,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<CallOutcome> {
        if self.cancel_requested && !cancel_sent {
            if let Err(error) = cancel_request(request, send) {
                let unsent = Stage::Admitted {
                    request,
                    cancel_sent,
                };
                return self.exchange_lost(error, unsent);
            }
            cancel_sent = true;
        }
        let waiting = Stage::Admitted {
            request,
            cancel_sent,
        };
        match checked(send(Command::TakeResult { request })) {
            Ok(Response::Pending) => {
                self.consecutive_failures = 0;
                self.stage = waiting;
                Poll::Pending
            }
            Ok(Response::Result { payload }) => self.release_reply(request, payload, send),
            Err(ControlError::Broker(ending)) => self.ended_without_reply(request, ending, send),
            Ok(response) => self.exchange_lost(ControlError::unexpected(&response), waiting),
            Err(error) => self.exchange_lost(error, waiting),
        }
    }

    fn ended_without_reply(
        &mut self,
        request: u64,
        ending: BrokerError,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<CallOutcome> {
        let record_is_gone = ending.code == ErrorCode::NotFound;
        let ending = ControlError::Broker(ending);
        if self.read_only && !record_is_gone {
            return self.discard(request, ending, send);
        }
        // A mutating request keeps its record: until it expires, the same
        // operation identity resolves to this ended request, not a new one.
        Poll::Ready(CallOutcome::Unconfirmed(ending))
    }

    fn release_reply(
        &mut self,
        request: u64,
        reply: Value,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<CallOutcome> {
        if self.record_released(request, send) {
            return Poll::Ready(CallOutcome::Completed(reply));
        }
        self.consecutive_failures += 1;
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            // The reply is known. Its record expires on its own; failing to
            // release it must not hide what the provider said.
            return Poll::Ready(CallOutcome::Completed(reply));
        }
        self.stage = Stage::Releasing { request, reply };
        Poll::Pending
    }

    fn discard(
        &mut self,
        request: u64,
        ending: ControlError,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<CallOutcome> {
        if self.record_released(request, send) {
            return Poll::Ready(CallOutcome::Unconfirmed(ending));
        }
        self.consecutive_failures += 1;
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            return Poll::Ready(CallOutcome::Unconfirmed(ending));
        }
        self.stage = Stage::Discarding { request, ending };
        Poll::Pending
    }

    /// A lost release reply may still have released the record, which the next
    /// attempt then reports as not found.
    fn record_released(
        &self,
        request: u64,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> bool {
        match checked(send(Command::ReleaseResult { request })) {
            Ok(Response::Ok) => true,
            Err(error) => error.is(&ErrorCode::NotFound),
            Ok(_) => false,
        }
    }

    fn admitted(&mut self, request: u64) -> Poll<CallOutcome> {
        self.consecutive_failures = 0;
        self.stage = Stage::Admitted {
            request,
            cancel_sent: false,
        };
        Poll::Pending
    }

    /// An exchange failed in transport. Retry it from `resume` on the next
    /// poll, or stop once failures repeat: what the broker did is unknown.
    fn exchange_lost(&mut self, error: ControlError, resume: Stage) -> Poll<CallOutcome> {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            return Poll::Ready(CallOutcome::Unconfirmed(error));
        }
        self.stage = resume;
        Poll::Pending
    }

    fn retry_window_closed(&self, now: Instant) -> bool {
        self.first_sent
            .is_some_and(|sent| now.saturating_duration_since(sent) >= ADMISSION_RETRY_WINDOW)
    }
}

/// Ask the broker to end `request`. A request it no longer holds needs no
/// cancelling; collecting the result reports what became of it.
fn cancel_request(
    request: u64,
    send: &mut impl FnMut(Command) -> ModuleResult<Response>,
) -> ControlResult<()> {
    match checked(send(Command::Cancel { request })) {
        Ok(Response::Ok) => Ok(()),
        Err(error) if error.is(&ErrorCode::NotFound) => Ok(()),
        Ok(response) => Err(ControlError::unexpected(&response)),
        Err(error) => Err(error),
    }
}

/// An operation identifier no other call from this module has used: `prefix`
/// followed by a counter that lasts for the life of the process.
///
/// The broker scopes identifiers to the calling module, so a prefix only has
/// to be unique within it. Use lowercase letters, digits, `.`, `_`, `-` and
/// `/`, and keep the whole identifier within 128 bytes. Choose a fixed
/// identifier instead when a retry of the same logical operation must find the
/// earlier attempt.
pub fn unique_operation_id(prefix: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("{prefix}.{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests;
