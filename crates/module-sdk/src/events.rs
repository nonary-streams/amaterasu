//! Typed access to the runtime's versioned event topics.
//!
//! Topics and payload schemas are contracts shared by publishers and subscribers.
//! Publisher identities and topic grants are established by the runtime endpoint,
//! never by a field supplied to this adapter. Delivery is bounded and is not a
//! durable log; publication acceptance does not imply subscriber delivery.
use crate::{Command, ModuleResult, Response, RuntimeClient};
use serde::{Serialize, de::DeserializeOwned};
use std::{cell::Cell, marker::PhantomData};

/// A versioned topic and its wire payload. Incompatible payload changes require
/// a new topic identity and matching publish/subscribe grants.
pub trait EventTopic {
    const TOPIC: &'static str;
    type Payload: Serialize + DeserializeOwned;
}

/// The identity is attested by the runtime, and sequence numbers are scoped to
/// a publisher and topic, not globally ordered across publishers or restarts.
#[derive(Debug, PartialEq)]
pub struct Event<T> {
    pub publisher: String,
    pub sequence: u64,
    pub payload: T,
}

/// An overflow or uncertain delivery invalidates the subscription until it is replaced.
#[derive(Debug, PartialEq)]
pub enum EventPoll<T> {
    Pending,
    Event(Event<T>),
    ResyncRequired,
}

/// Owns one bounded, non-coalescing broker subscription. Dropping it attempts
/// to unsubscribe once, but cannot report a failure or guarantee cleanup. Call
/// `close` and retain this handle for retry when cleanup must be confirmed.
pub struct Subscription<'a, T: EventTopic> {
    runtime: &'a RuntimeClient,
    id: Option<u64>,
    capacity: usize,
    needs_resynchronization: Cell<bool>,
    topic: PhantomData<T>,
}

/// A subscription whose retirement failed. Retain it and retry `close`.
pub struct CleanupFailure<'a, T: EventTopic> {
    pub subscription: Subscription<'a, T>,
    pub error: String,
}

/// The replacement is live even if retiring the old subscription failed.
pub struct RecoverySuccess<'a, T: EventTopic, S> {
    pub subscription: Subscription<'a, T>,
    pub snapshot: S,
    pub cleanup: Option<CleanupFailure<'a, T>>,
}

/// Recovery failed; the original subscription remains owned by the caller.
/// If retiring the new subscription also failed, `cleanup` retains it for retry.
pub struct RecoveryFailure<'a, T: EventTopic> {
    pub subscription: Subscription<'a, T>,
    pub error: String,
    pub cleanup: Option<CleanupFailure<'a, T>>,
}

impl RuntimeClient {
    /// Publishes on the topic using this client's authenticated endpoint.
    pub fn publish_event<T: EventTopic>(&self, payload: &T::Payload) -> ModuleResult<()> {
        let payload = serde_json::to_value(payload).map_err(|error| error.to_string())?;
        self.require_success(Command::Publish {
            topic: T::TOPIC.to_owned(),
            payload,
        })
    }

    /// Subscribe with a queue of 1 through 64 events. Overflow is reported
    /// explicitly rather than silently dropping or coalescing an event.
    pub fn subscribe_events<T: EventTopic>(
        &self,
        capacity: usize,
    ) -> ModuleResult<Subscription<'_, T>> {
        match self.send(Command::Subscribe {
            topic: T::TOPIC.to_owned(),
            capacity,
            coalesce: false,
        })? {
            Response::Accepted { id } => Ok(Subscription {
                runtime: self,
                id: Some(id),
                capacity,
                needs_resynchronization: Cell::new(false),
                topic: PhantomData,
            }),
            response => Err(unexpected_response(response)),
        }
    }
}

impl<'a, T: EventTopic> Subscription<'a, T> {
    /// A malformed payload, an unexpected response, or a transport failure may
    /// have consumed an event. Return the error once, then require recovery on
    /// every subsequent poll instead of silently skipping a possible gap.
    pub fn next(&self) -> ModuleResult<EventPoll<T::Payload>> {
        let id = self.id.ok_or("event subscription is closed")?;
        if self.needs_resynchronization.get() {
            return Ok(EventPoll::ResyncRequired);
        }
        let result = match self.runtime.send(Command::NextEvent { subscription: id }) {
            Ok(Response::Pending) => Ok(EventPoll::Pending),
            Ok(Response::ResyncRequired { topic }) if topic == T::TOPIC => {
                self.needs_resynchronization.set(true);
                Ok(EventPoll::ResyncRequired)
            }
            Ok(Response::Event {
                publisher,
                topic,
                sequence,
                payload,
            }) if topic == T::TOPIC => serde_json::from_value(payload)
                .map(|payload| {
                    EventPoll::Event(Event {
                        publisher,
                        sequence,
                        payload,
                    })
                })
                .map_err(|error| error.to_string()),
            Ok(response) => Err(unexpected_response(response)),
            Err(error) => Err(error),
        };
        if result.is_err() {
            self.needs_resynchronization.set(true);
        }
        result
    }

    /// Start a replacement before reading a fresh snapshot. Events may overlap
    /// the snapshot: reconcile them using a revision or ordering guarantee from
    /// the service. This is not an atomic snapshot, cursor, or replay. Overflow
    /// while taking the snapshot still requires another resynchronization.
    /// Any subscription that could not be retired is returned for cleanup retry.
    pub fn resubscribe_with_snapshot<S>(
        mut self,
        snapshot: impl FnOnce() -> ModuleResult<S>,
    ) -> Result<RecoverySuccess<'a, T, S>, Box<RecoveryFailure<'a, T>>> {
        let mut replacement = match self.runtime.subscribe_events::<T>(self.capacity) {
            Ok(replacement) => replacement,
            Err(error) => {
                return Err(Box::new(RecoveryFailure {
                    subscription: self,
                    error,
                    cleanup: None,
                }));
            }
        };
        match snapshot() {
            Ok(snapshot) => {
                let cleanup = self.close().err().map(|error| CleanupFailure {
                    subscription: self,
                    error,
                });
                Ok(RecoverySuccess {
                    subscription: replacement,
                    snapshot,
                    cleanup,
                })
            }
            Err(error) => {
                let cleanup = replacement.close().err().map(|error| CleanupFailure {
                    subscription: replacement,
                    error,
                });
                Err(Box::new(RecoveryFailure {
                    subscription: self,
                    error,
                    cleanup,
                }))
            }
        }
    }

    /// Confirm retirement or retain this handle to retry. If a previous reply
    /// was lost after retirement, the broker's `not_found` confirms absence.
    pub fn close(&mut self) -> ModuleResult<()> {
        let Some(subscription) = self.id else {
            return Ok(());
        };
        match self.runtime.send(Command::Unsubscribe { subscription })? {
            Response::Ok => {}
            Response::Error { code, .. } if code == "not_found" => {}
            response => return Err(unexpected_response(response)),
        }
        self.id = None;
        Ok(())
    }
}

impl<T: EventTopic> Drop for Subscription<'_, T> {
    fn drop(&mut self) {
        if let Some(subscription) = self.id.take() {
            let _ = self
                .runtime
                .require_success(Command::Unsubscribe { subscription });
        }
    }
}

fn unexpected_response(response: Response) -> String {
    match response {
        Response::Error { code, message } => format!("{code}: {message}"),
        unexpected => format!("unexpected runtime response: {unexpected:?}"),
    }
}
