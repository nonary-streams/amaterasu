//! Typed events: a module announces that something changed, and any module
//! granted the topic follows along.
//!
//! An event is a notification, not a delivery guarantee. Each subscription
//! has a bounded queue, and a subscriber that falls behind either loses the
//! oldest events or is told there is a gap, as its [`Backlog`] says. Publish
//! a revision or an identifier and let subscribers read the state from its
//! owner; do not publish the state itself and expect every copy to arrive.
//!
//! A [`Topic`] ties a topic name to the payload its publisher sends, so the
//! publisher and its subscribers share one definition:
//!
//! ```ignore
//! pub const SESSION_CHANGED: Topic<SessionChanged> = Topic::new("amaterasu.session.changed/1");
//!
//! runtime.publish(&SESSION_CHANGED, &SessionChanged { revision })?;
//!
//! let mut changes = Subscription::open(&runtime, &SESSION_CHANGED, Backlog::Latest(1))?;
//! // each tick:
//! while let Some(delivery) = changes.next(&runtime)? { ... }
//! ```
use crate::control_error::{ControlError, ControlResult, ErrorCode, checked};
use crate::{Command, ModuleResult, Response, RuntimeClient};
use serde::{Serialize, de::DeserializeOwned};
use std::fmt;
use std::marker::PhantomData;

/// A topic name and the payload type published on it.
pub struct Topic<T> {
    name: &'static str,
    payload: PhantomData<fn() -> T>,
}

impl<T> Topic<T> {
    /// `name` includes the interface major, as in `example.thing.changed/1`.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            payload: PhantomData,
        }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }
}

impl<T> Clone for Topic<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Topic<T> {}

impl<T> fmt::Debug for Topic<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Topic").field(&self.name).finish()
    }
}

/// Publish `payload` on `topic` through `send`.
pub fn publish_with<T: Serialize>(
    send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    topic: &Topic<T>,
    payload: &T,
) -> ControlResult<()> {
    let payload = serde_json::to_value(payload).map_err(ControlError::payload)?;
    let publication = Command::Publish {
        topic: topic.name.to_owned(),
        payload,
    };
    match checked(send(publication))? {
        Response::Ok => Ok(()),
        response => Err(ControlError::unexpected(&response)),
    }
}

impl RuntimeClient {
    /// Publish `payload` to every current subscriber of `topic`. This module
    /// needs the topic's `publish` grant. The runtime stamps the event with
    /// this module's identity and a sequence number; nobody is waited for.
    pub fn publish<T: Serialize>(&self, topic: &Topic<T>, payload: &T) -> ControlResult<()> {
        publish_with(&mut |command| self.send(command), topic, payload)
    }
}

/// What a subscription keeps when events arrive faster than they are read.
/// The runtime allows a queue of 1 to 64 events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backlog {
    /// Keep the newest events and drop the oldest silently. For a subscriber
    /// that only needs to know the latest state changed.
    Latest(usize),
    /// Keep every event up to this many. When more arrive the subscriber is
    /// told there is a [`Delivery::Gap`] instead of being handed a partial
    /// history.
    Complete(usize),
}

impl Backlog {
    fn capacity(self) -> usize {
        match self {
            Self::Latest(capacity) | Self::Complete(capacity) => capacity,
        }
    }

    fn coalesce(self) -> bool {
        matches!(self, Self::Latest(_))
    }
}

/// One published event.
#[derive(Clone, Debug, PartialEq)]
pub struct Event<T> {
    /// The publishing module, as authenticated by the runtime.
    pub publisher: String,
    /// Counts this publisher's events on this topic, starting at 1.
    pub sequence: u64,
    pub payload: T,
}

/// What a subscription hands over next.
#[derive(Clone, Debug, PartialEq)]
pub enum Delivery<T> {
    Event(Event<T>),
    /// Events were missed and the subscription has been renewed. Read the
    /// current state from its owner before relying on later events.
    Gap,
    /// An event whose payload is not this topic's type. It is skipped.
    Malformed {
        publisher: String,
        sequence: u64,
        reason: String,
    },
}

/// This module's place in a topic's audience.
#[derive(Debug)]
pub struct Subscription<T> {
    topic: &'static str,
    backlog: Backlog,
    /// `None` while a renewal after a gap has not succeeded yet.
    handle: Option<u64>,
    payload: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> Subscription<T> {
    /// Start following `topic`. This module needs the topic's `subscribe`
    /// grant. Events published before this call are not delivered.
    pub fn open(
        runtime: &RuntimeClient,
        topic: &Topic<T>,
        backlog: Backlog,
    ) -> ControlResult<Self> {
        Self::open_with(&mut |command| runtime.send(command), topic, backlog)
    }

    /// Start following `topic` through `send`.
    pub fn open_with(
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        topic: &Topic<T>,
        backlog: Backlog,
    ) -> ControlResult<Self> {
        let mut subscription = Self {
            topic: topic.name,
            backlog,
            handle: None,
            payload: PhantomData,
        };
        subscription.subscribe(send)?;
        Ok(subscription)
    }

    pub fn topic(&self) -> &'static str {
        self.topic
    }

    /// Take the next delivery. `None` means nothing is waiting.
    pub fn next(&mut self, runtime: &RuntimeClient) -> ControlResult<Option<Delivery<T>>> {
        self.next_with(&mut |command| runtime.send(command))
    }

    /// Take the next delivery through `send`.
    pub fn next_with(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ControlResult<Option<Delivery<T>>> {
        let Some(subscription) = self.handle else {
            // An earlier renewal failed. The gap was already reported.
            self.subscribe(send)?;
            return Ok(None);
        };
        match checked(send(Command::NextEvent { subscription }))? {
            Response::Pending => Ok(None),
            Response::Event {
                publisher,
                sequence,
                payload,
                ..
            } => Ok(Some(match T::deserialize(&payload) {
                Ok(payload) => Delivery::Event(Event {
                    publisher,
                    sequence,
                    payload,
                }),
                Err(error) => Delivery::Malformed {
                    publisher,
                    sequence,
                    reason: error.to_string(),
                },
            })),
            Response::ResyncRequired { .. } => {
                self.renew(send);
                Ok(Some(Delivery::Gap))
            }
            response => Err(ControlError::unexpected(&response)),
        }
    }

    /// Stop following the topic and free its queue.
    pub fn close(self, runtime: &RuntimeClient) -> ControlResult<()> {
        self.close_with(&mut |command| runtime.send(command))
    }

    /// Stop following the topic through `send`.
    pub fn close_with(
        self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ControlResult<()> {
        match self.handle {
            Some(subscription) => unsubscribe(send, subscription),
            None => Ok(()),
        }
    }

    fn subscribe(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ControlResult<()> {
        let subscription = Command::Subscribe {
            topic: self.topic.to_owned(),
            capacity: self.backlog.capacity(),
            coalesce: self.backlog.coalesce(),
        };
        match checked(send(subscription))? {
            Response::Accepted { id } => {
                self.handle = Some(id);
                Ok(())
            }
            response => Err(ControlError::unexpected(&response)),
        }
    }

    /// A subscription that overflowed delivers nothing more. Replace it. The
    /// caller reports the gap either way; a failed renewal is tried again on
    /// the next read.
    fn renew(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) {
        if let Some(overflowed) = self.handle.take() {
            let _ = unsubscribe(send, overflowed);
        }
        let _ = self.subscribe(send);
    }
}

fn unsubscribe(
    send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    subscription: u64,
) -> ControlResult<()> {
    match checked(send(Command::Unsubscribe { subscription })) {
        Ok(Response::Ok) => Ok(()),
        Ok(response) => Err(ControlError::unexpected(&response)),
        // Already gone.
        Err(error) if error.is(&ErrorCode::NotFound) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
