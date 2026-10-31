//! Typed access to the runtime's versioned event topics.
//!
//! Topics and payload schemas are contracts shared by publishers and subscribers.
//! Publisher identities and topic grants are established by the runtime endpoint,
//! never by a field supplied to this adapter. Delivery is bounded and is not a
//! durable log; publication acceptance does not imply subscriber delivery.
use crate::{Command, ModuleResult, Response, RuntimeClient};
use serde::{Serialize, de::DeserializeOwned};
use std::marker::PhantomData;

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

/// An overflow invalidates the subscription until it is replaced.
#[derive(Debug, PartialEq)]
pub enum EventPoll<T> {
    Pending,
    Event(Event<T>),
    ResyncRequired,
}

/// Owns one bounded, non-coalescing broker subscription. Dropping it attempts
/// to unsubscribe; use `close` when the caller needs to observe cleanup failure.
pub struct Subscription<'a, T: EventTopic> {
    runtime: &'a RuntimeClient,
    id: Option<u64>,
    capacity: usize,
    topic: PhantomData<T>,
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
                topic: PhantomData,
            }),
            response => Err(unexpected_response(response)),
        }
    }
}

impl<'a, T: EventTopic> Subscription<'a, T> {
    pub fn next(&self) -> ModuleResult<EventPoll<T::Payload>> {
        let id = self.id.ok_or("event subscription is closed")?;
        match self.runtime.send(Command::NextEvent { subscription: id })? {
            Response::Pending => Ok(EventPoll::Pending),
            Response::ResyncRequired { topic } if topic == T::TOPIC => {
                Ok(EventPoll::ResyncRequired)
            }
            Response::Event {
                publisher,
                topic,
                sequence,
                payload,
            } if topic == T::TOPIC => {
                let payload = serde_json::from_value(payload).map_err(|error| error.to_string())?;
                Ok(EventPoll::Event(Event {
                    publisher,
                    sequence,
                    payload,
                }))
            }
            response => Err(unexpected_response(response)),
        }
    }

    /// Start a replacement subscription before reading a fresh snapshot. Events
    /// may overlap the snapshot: callers must reconcile them using a revision or
    /// another ordering guarantee provided by their specific service. This is
    /// not an atomic snapshot, cursor, or replay. If the new queue overflows
    /// while taking the snapshot, it too reports `ResyncRequired` on polling.
    /// The old subscription remains usable if creating the replacement or
    /// reading the snapshot fails; on success it is closed best-effort.
    pub fn resubscribe_with_snapshot<S>(
        self,
        snapshot: impl FnOnce() -> ModuleResult<S>,
    ) -> Result<(Subscription<'a, T>, S), (Self, String)> {
        let replacement = match self.runtime.subscribe_events::<T>(self.capacity) {
            Ok(replacement) => replacement,
            Err(error) => return Err((self, error)),
        };
        match snapshot() {
            Ok(snapshot) => Ok((replacement, snapshot)),
            Err(error) => Err((self, error)),
        }
    }

    /// Close explicitly, returning any transport or authorization error. If the
    /// request fails, dropping this value retries once on a best-effort basis.
    pub fn close(mut self) -> ModuleResult<()> {
        let id = self.id.ok_or("event subscription is closed")?;
        self.runtime
            .require_success(Command::Unsubscribe { subscription: id })?;
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
