use amaterasu_contracts::{Grants, Response};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};

use super::errors::{capacity_exhausted, not_found, payload_fits, permission_denied};

const MAX_SUBSCRIPTIONS: usize = 128;
const MAX_QUEUE_CAPACITY: usize = 64;

struct Subscription {
    owner: String,
    topic: String,
    capacity: usize,
    coalesce: bool,
    needs_resynchronization: bool,
    events: VecDeque<Response>,
}

impl Subscription {
    fn enqueue(&mut self, event: &Response) {
        if self.needs_resynchronization {
            return;
        }

        if self.events.len() >= self.capacity {
            if self.coalesce {
                self.events.pop_front();
            } else {
                self.events.clear();
                self.needs_resynchronization = true;
                return;
            }
        }
        self.events.push_back(event.clone());
    }

    fn next(&mut self) -> Response {
        if self.needs_resynchronization {
            return Response::ResyncRequired {
                topic: self.topic.clone(),
            };
        }
        self.events.pop_front().unwrap_or(Response::Pending)
    }
}

#[derive(Default)]
pub(super) struct EventBus {
    subscriptions: BTreeMap<u64, Subscription>,
    publisher_sequences: BTreeMap<(String, String), u64>,
    next_subscription_id: u64,
}

impl EventBus {
    pub fn subscribe(
        &mut self,
        caller: &str,
        grants: &Grants,
        topic: String,
        capacity: usize,
        coalesce: bool,
    ) -> Response {
        if !grants.subscribe.contains(&topic) {
            return permission_denied();
        }
        if !(1..=MAX_QUEUE_CAPACITY).contains(&capacity) {
            return Response::error(
                "invalid_request",
                "subscription capacity must be 1 through 64",
            );
        }
        if self.subscriptions.len() >= MAX_SUBSCRIPTIONS {
            return capacity_exhausted();
        }

        self.next_subscription_id += 1;
        let subscription_id = self.next_subscription_id;
        self.subscriptions.insert(
            subscription_id,
            Subscription {
                owner: caller.to_owned(),
                topic,
                capacity,
                coalesce,
                needs_resynchronization: false,
                events: VecDeque::new(),
            },
        );
        Response::Accepted {
            id: subscription_id,
        }
    }

    pub fn publish(
        &mut self,
        caller: &str,
        grants: &Grants,
        topic: String,
        payload: Value,
    ) -> Response {
        if !grants.publish.contains(&topic) {
            return permission_denied();
        }
        if !payload_fits(&payload) {
            return Response::error("invalid_request", "event payload is too large");
        }

        let sequence = self
            .publisher_sequences
            .entry((caller.to_owned(), topic.clone()))
            .or_default();
        *sequence += 1;
        let event = Response::Event {
            publisher: caller.to_owned(),
            topic: topic.clone(),
            sequence: *sequence,
            payload,
        };

        for subscription in self.subscriptions.values_mut() {
            if subscription.topic == topic {
                subscription.enqueue(&event);
            }
        }
        Response::Ok
    }

    pub fn next_event(&mut self, caller: &str, subscription_id: u64) -> Response {
        let Some(subscription) = self.subscriptions.get_mut(&subscription_id) else {
            return not_found();
        };
        if subscription.owner != caller {
            return permission_denied();
        }
        subscription.next()
    }

    pub fn unsubscribe(&mut self, caller: &str, subscription_id: u64) -> Response {
        let Some(subscription) = self.subscriptions.get(&subscription_id) else {
            return not_found();
        };
        if subscription.owner != caller {
            return permission_denied();
        }
        self.subscriptions.remove(&subscription_id);
        Response::Ok
    }
}
