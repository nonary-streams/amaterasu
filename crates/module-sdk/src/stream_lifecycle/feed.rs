//! The launch owner's subscription to reported stream issues.
use super::{STREAM_REPORTS, StreamLifecycleStore, StreamReport};
use crate::LogEmitter;
use crate::control_error::ControlResult;
use crate::events::{Backlog, Delivery, Subscription};
use crate::{Command, ModuleResult, Response};

/// Events a subscription may hold before the owner must read them.
const FEED_CAPACITY: usize = 64;
/// Polls to wait before trying a refused subscription again.
const RESUBSCRIBE_AFTER_POLLS: u32 = 512;

/// Moves reported issues and bindings from the [`STREAM_REPORTS`] topic into
/// a store.
///
/// Every read is a non-blocking broker command, and a poll reads at most the
/// limit it is given, so the owner's tick stays bounded. Missed events mark
/// the open streams as having an audit gap.
#[derive(Debug, Default)]
pub struct StreamIssueFeed {
    subscription: Option<Subscription<StreamReport>>,
    retry_in: u32,
    malformed: u64,
    unavailable: Option<String>,
}

impl StreamIssueFeed {
    pub fn new() -> Self {
        Self::default()
    }

    /// Events from a publisher that did not match [`StreamReport`] or failed
    /// its bounds. They are logged and counted, never committed.
    pub fn malformed(&self) -> u64 {
        self.malformed
    }

    pub fn subscribed(&self) -> bool {
        self.subscription.is_some()
    }

    /// Why the subscription is not open, if it is not.
    pub fn unavailable(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// Commit up to `limit` waiting issues into `store`. Returns how many
    /// deliveries were read.
    pub fn poll_with(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        store: &mut StreamLifecycleStore,
        logger: Option<&LogEmitter>,
        limit: usize,
    ) -> usize {
        let Some(mut subscription) = self.take_subscription(send, logger) else {
            return 0;
        };
        let mut read = 0;
        while read < limit {
            match subscription.next_with(send) {
                Ok(None) => break,
                Ok(Some(delivery)) => {
                    read += 1;
                    self.receive(delivery, store, logger);
                }
                Err(error) => {
                    store.note_gap();
                    self.lose_subscription(error.to_string(), logger);
                    return read;
                }
            }
        }
        self.subscription = Some(subscription);
        read
    }

    /// The open subscription, opening it first when the back-off allows.
    fn take_subscription(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        logger: Option<&LogEmitter>,
    ) -> Option<Subscription<StreamReport>> {
        if let Some(subscription) = self.subscription.take() {
            return Some(subscription);
        }
        if self.retry_in > 0 {
            self.retry_in -= 1;
            return None;
        }
        let opened: ControlResult<_> =
            Subscription::open_with(send, &STREAM_REPORTS, Backlog::Complete(FEED_CAPACITY));
        match opened {
            Ok(subscription) => {
                self.unavailable = None;
                Some(subscription)
            }
            Err(error) => {
                self.lose_subscription(error.to_string(), logger);
                None
            }
        }
    }

    fn lose_subscription(&mut self, reason: String, logger: Option<&LogEmitter>) {
        self.subscription = None;
        self.retry_in = RESUBSCRIBE_AFTER_POLLS;
        if self.unavailable.as_deref() != Some(reason.as_str()) {
            if let Some(logger) = logger {
                let _ = logger.emit(
                    crate::LogLevel::Warning,
                    "stream.lifecycle.feed",
                    &format!("Stream issue updates are unavailable: {reason}. Will retry."),
                );
            }
            self.unavailable = Some(reason);
        }
    }

    fn receive(
        &mut self,
        delivery: Delivery<StreamReport>,
        store: &mut StreamLifecycleStore,
        logger: Option<&LogEmitter>,
    ) {
        match delivery {
            Delivery::Event(event) if event.payload.valid() => match event.payload {
                StreamReport::Issue(issue) => {
                    store.commit(&event.publisher, issue);
                }
                StreamReport::Binding(binding) => store.bind(&binding.stream, &binding.alias),
            },
            Delivery::Event(event) => {
                self.reject(&event.publisher, "invalid_stream_issue", logger);
            }
            Delivery::Malformed { publisher, .. } => {
                self.reject(&publisher, "malformed_stream_issue", logger);
            }
            Delivery::Gap => store.note_gap(),
        }
    }

    fn reject(&mut self, publisher: &str, code: &str, logger: Option<&LogEmitter>) {
        self.malformed = self.malformed.saturating_add(1);
        if let Some(logger) = logger {
            let _ = logger.emit(
                crate::LogLevel::Warning,
                "stream.lifecycle.feed",
                &format!(
                    "Ignored a stream issue from {publisher}: {}.",
                    code.replace('_', " ")
                ),
            );
        }
    }
}
