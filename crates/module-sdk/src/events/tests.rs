use super::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Changed {
    revision: u64,
}

const CHANGED: Topic<Changed> = Topic::new("example.thing.changed/1");

struct Script {
    replies: VecDeque<ModuleResult<Response>>,
    sent: Vec<Value>,
}

impl Script {
    fn new(replies: impl IntoIterator<Item = ModuleResult<Response>>) -> Self {
        Self {
            replies: replies.into_iter().collect(),
            sent: Vec::new(),
        }
    }

    fn send(&mut self) -> impl FnMut(Command) -> ModuleResult<Response> + '_ {
        |command| {
            self.sent.push(serde_json::to_value(&command).unwrap());
            self.replies
                .pop_front()
                .expect("an unscripted command was sent")
        }
    }
}

fn event(sequence: u64, payload: Value) -> ModuleResult<Response> {
    Ok(Response::Event {
        publisher: "example.owner".into(),
        topic: CHANGED.name().into(),
        sequence,
        payload,
    })
}

#[test]
fn publishing_sends_the_topics_name_and_typed_payload() {
    let mut script = Script::new([Ok(Response::Ok)]);
    publish_with(&mut script.send(), &CHANGED, &Changed { revision: 7 }).unwrap();
    assert_eq!(
        script.sent,
        [json!({"op":"publish","topic":"example.thing.changed/1","payload":{"revision":7}})]
    );

    let mut refused = Script::new([Ok(Response::error("permission_denied", "not granted"))]);
    let error = publish_with(&mut refused.send(), &CHANGED, &Changed { revision: 7 }).unwrap_err();
    assert!(error.is(&ErrorCode::PermissionDenied));
}

#[test]
fn the_backlog_selects_the_queue_size_and_what_overflow_does() {
    for (backlog, capacity, coalesce) in [
        (Backlog::Latest(1), 1, true),
        (Backlog::Complete(32), 32, false),
    ] {
        let mut script = Script::new([Ok(Response::Accepted { id: 3 })]);
        Subscription::open_with(&mut script.send(), &CHANGED, backlog).unwrap();
        assert_eq!(
            script.sent,
            [json!({
                "op": "subscribe",
                "topic": "example.thing.changed/1",
                "capacity": capacity,
                "coalesce": coalesce,
            })]
        );
    }
}

#[test]
fn events_are_decoded_and_a_malformed_one_is_reported_without_failing() {
    let mut script = Script::new([
        Ok(Response::Accepted { id: 3 }),
        event(1, json!({"revision": 4})),
        event(2, json!({"revision": "four"})),
        Ok(Response::Pending),
    ]);
    let mut changes =
        Subscription::open_with(&mut script.send(), &CHANGED, Backlog::Complete(8)).unwrap();
    assert_eq!(
        changes.next_with(&mut script.send()).unwrap(),
        Some(Delivery::Event(Event {
            publisher: "example.owner".into(),
            sequence: 1,
            payload: Changed { revision: 4 },
        }))
    );
    assert!(matches!(
        changes.next_with(&mut script.send()).unwrap(),
        Some(Delivery::Malformed { sequence: 2, .. })
    ));
    assert_eq!(changes.next_with(&mut script.send()).unwrap(), None);
}

#[test]
fn an_overflowed_subscription_reports_a_gap_and_is_renewed() {
    let mut script = Script::new([
        Ok(Response::Accepted { id: 3 }),
        Ok(Response::ResyncRequired {
            topic: CHANGED.name().into(),
        }),
        Ok(Response::Ok),
        Ok(Response::Accepted { id: 4 }),
        event(9, json!({"revision": 12})),
    ]);
    let mut changes =
        Subscription::open_with(&mut script.send(), &CHANGED, Backlog::Complete(2)).unwrap();
    assert_eq!(
        changes.next_with(&mut script.send()).unwrap(),
        Some(Delivery::Gap)
    );
    assert!(matches!(
        changes.next_with(&mut script.send()).unwrap(),
        Some(Delivery::Event(Event { sequence: 9, .. }))
    ));
    let operations: Vec<_> = script
        .sent
        .iter()
        .map(|command| (command["op"].as_str().unwrap(), &command["subscription"]))
        .collect();
    assert_eq!(operations[1], ("next_event", &json!(3)));
    assert_eq!(operations[2], ("unsubscribe", &json!(3)));
    assert_eq!(operations[3].0, "subscribe");
    // Later reads use the renewed subscription.
    assert_eq!(operations[4], ("next_event", &json!(4)));
}

#[test]
fn a_failed_renewal_is_retried_on_the_next_read_and_the_gap_is_reported_once() {
    let mut script = Script::new([
        Ok(Response::Accepted { id: 3 }),
        Ok(Response::ResyncRequired {
            topic: CHANGED.name().into(),
        }),
        Ok(Response::Ok),
        Ok(Response::error(
            "resource_exhausted",
            "bounded runtime capacity reached",
        )),
        Ok(Response::Accepted { id: 5 }),
        Ok(Response::Pending),
    ]);
    let mut changes =
        Subscription::open_with(&mut script.send(), &CHANGED, Backlog::Complete(2)).unwrap();
    assert_eq!(
        changes.next_with(&mut script.send()).unwrap(),
        Some(Delivery::Gap)
    );
    // The retry subscribes again and delivers nothing by itself.
    assert_eq!(changes.next_with(&mut script.send()).unwrap(), None);
    assert_eq!(changes.next_with(&mut script.send()).unwrap(), None);
    assert_eq!(script.sent.last().unwrap()["subscription"], 5);
}

#[test]
fn closing_frees_the_queue_and_tolerates_one_that_is_already_gone() {
    for reply in [
        Ok(Response::Ok),
        Ok(Response::error("not_found", "unknown handle or interface")),
    ] {
        let mut script = Script::new([Ok(Response::Accepted { id: 3 }), reply]);
        let changes =
            Subscription::open_with(&mut script.send(), &CHANGED, Backlog::Latest(1)).unwrap();
        changes.close_with(&mut script.send()).unwrap();
        assert_eq!(script.sent[1], json!({"op":"unsubscribe","subscription":3}));
    }
}
