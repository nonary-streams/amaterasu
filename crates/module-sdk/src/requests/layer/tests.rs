use super::*;
use serde_json::json;
use std::collections::VecDeque;

fn received(id: u64, payload: Value, remaining_ms: u64) -> IncomingRequest<Value> {
    IncomingRequest {
        id,
        caller: "consumer".into(),
        initiator: "consumer".into(),
        service: "example.echo/1".into(),
        payload,
        remaining_ms,
    }
}

/// Answers each command with the next scripted reply and records what was sent.
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

    fn poll(
        &mut self,
        relay: &mut Relay,
        answer: &mut impl FnMut(&IncomingRequest<Value>, CallOutcome) -> Answer,
    ) -> ControlResult<usize> {
        let (replies, sent) = (&mut self.replies, &mut self.sent);
        relay.poll_at(
            Instant::now(),
            &mut |command| {
                sent.push(serde_json::to_value(&command).unwrap());
                replies
                    .pop_front()
                    .expect("the relay sent an unscripted command")
            },
            answer,
        )
    }

    fn operations(&self) -> Vec<&str> {
        self.sent
            .iter()
            .map(|command| command["op"].as_str().unwrap())
            .collect()
    }
}

fn first_command(mut call: OutboundCall) -> Value {
    let mut sent = None;
    let _ = call.poll_at(Instant::now(), &mut |command| {
        sent = Some(serde_json::to_value(&command).unwrap());
        Ok(Response::Accepted { id: 1 })
    });
    sent.unwrap()
}

#[test]
fn a_forward_is_a_delegated_call_to_the_same_service() {
    let request = received(41, json!({"text":"hi"}), 750);
    assert_eq!(
        first_command(request.forward().unwrap()),
        json!({
            "op": "request",
            "service": "example.echo/1",
            "payload": {"text":"hi"},
            "timeout_ms": 750,
            "operation_id": "forward.41",
            "parent": 41,
        })
    );
    let rewritten = first_command(request.forward_with(&json!({"text":"HI"})).unwrap());
    assert_eq!(rewritten["payload"], json!({"text":"HI"}));
    assert_eq!(rewritten["parent"], 41);
}

#[test]
fn a_forward_never_asks_for_a_deadline_the_broker_rejects() {
    let expired = first_command(received(1, json!({}), 0).forward().unwrap());
    assert_eq!(expired["timeout_ms"], 1);
    let long = first_command(received(2, json!({}), 90_000).forward().unwrap());
    assert_eq!(long["timeout_ms"], 30_000);
}

#[test]
fn an_api_forward_repeats_the_delivered_method_and_version() {
    let envelope = json!({
        "method": "render",
        "interface_major": 1,
        "interface_minor": 2,
        "arguments": {"size": 900},
    });
    let request = received(8, envelope, 500);
    assert_eq!(
        first_command(request.forward_api().unwrap()),
        json!({
            "op": "call_api",
            "service": "example.echo/1",
            "method": "render",
            "interface_major": 1,
            "interface_minor": 2,
            "payload": {"size": 900},
            "timeout_ms": 500,
            "operation_id": "forward.8",
            "parent": 8,
        })
    );
    let limited = first_command(request.forward_api_with(&json!({"size": 64})).unwrap());
    assert_eq!(limited["payload"], json!({"size": 64}));

    let plain = received(9, json!({"text":"hi"}), 500);
    assert!(matches!(plain.forward_api(), Err(ControlError::Payload(_))));
}

#[test]
fn a_reply_is_rewritten_and_recorded_for_the_consumer() {
    let mut relay = Relay::new();
    relay
        .forward(received(5, json!({"text":"hi"}), 500))
        .unwrap();
    let mut script = Script::new([
        Ok(Response::Accepted { id: 9 }),
        Ok(Response::Result {
            payload: json!({"text":"hi"}),
        }),
        Ok(Response::Ok),
        Ok(Response::Ok),
    ]);
    let mut shout = |request: &IncomingRequest<Value>, outcome: CallOutcome| {
        assert_eq!(request.id, 5);
        match outcome {
            CallOutcome::Completed(reply) => {
                Answer::Reply(json!({"text": reply["text"], "seen": true}))
            }
            other => Answer::relay(other),
        }
    };
    assert_eq!(script.poll(&mut relay, &mut shout).unwrap(), 0);
    assert_eq!(relay.len(), 1);
    assert_eq!(script.poll(&mut relay, &mut shout).unwrap(), 1);
    assert!(relay.is_empty());
    assert_eq!(
        script.operations(),
        ["request", "take_result", "release_result", "complete"]
    );
    assert_eq!(
        script.sent[3],
        json!({"op":"complete","request":5,"payload":{"text":"hi","seen":true}})
    );
}

#[test]
fn an_inner_refusal_reaches_the_consumer_with_its_code() {
    let mut relay = Relay::new();
    relay.forward(received(5, json!({}), 500)).unwrap();
    let mut script = Script::new([
        Ok(Response::error("unavailable", "service is not registered")),
        Ok(Response::Ok),
    ]);
    assert_eq!(
        script
            .poll(&mut relay, &mut |_, outcome| Answer::relay(outcome))
            .unwrap(),
        1
    );
    assert_eq!(
        script.sent[1],
        json!({
            "op": "refuse",
            "request": 5,
            "code": "unavailable",
            "message": "service is not registered",
        })
    );
}

#[test]
fn an_unknown_inner_outcome_is_never_reported_as_a_refusal() {
    let mut relay = Relay::new();
    relay.forward(received(5, json!({}), 500)).unwrap();
    let mut script = Script::new([
        Ok(Response::Accepted { id: 9 }),
        Ok(Response::error(
            "deadline_exceeded",
            "request deadline elapsed",
        )),
        Ok(Response::Ok),
    ]);
    let mut relayed = |_: &IncomingRequest<Value>, outcome: CallOutcome| Answer::relay(outcome);
    assert_eq!(script.poll(&mut relay, &mut relayed).unwrap(), 0);
    assert_eq!(script.poll(&mut relay, &mut relayed).unwrap(), 1);
    // Nothing is said to the consumer: its own deadline reports the unknown.
    // The forward is never repeated, so its ended record is released.
    assert_eq!(
        script.operations(),
        ["request", "take_result", "release_result"]
    );
    assert!(relay.is_empty());
}

#[test]
fn one_failed_answer_does_not_strand_the_other_forwards() {
    let mut relay = Relay::new();
    relay.forward(received(1, json!({}), 500)).unwrap();
    relay.forward(received(2, json!({}), 500)).unwrap();
    let refused = || Ok(Response::error("busy", "scripted"));
    let mut script = Script::new([
        refused(),
        Err("reply lost".into()),
        refused(),
        Ok(Response::Ok),
    ]);
    let result = script.poll(&mut relay, &mut |_, outcome| Answer::relay(outcome));
    assert!(matches!(result, Err(ControlError::Transport(_))));
    // Both forwards settled and both consumers were addressed.
    assert!(relay.is_empty());
    assert_eq!(
        script.operations(),
        ["request", "refuse", "request", "refuse"]
    );
}

#[test]
fn the_transparent_answer_keeps_the_three_outcomes_apart() {
    assert_eq!(
        Answer::relay(CallOutcome::Completed(json!(1))),
        Answer::Reply(json!(1))
    );
    let refusal = crate::BrokerError::from_wire("refused.stream_active", "a stream is running");
    assert_eq!(
        Answer::relay(CallOutcome::Refused(refusal)),
        Answer::Refuse {
            code: "stream_active".into(),
            message: "a stream is running".into(),
        }
    );
    assert_eq!(
        Answer::relay(CallOutcome::Unconfirmed(ControlError::Transport(
            "lost".into()
        ))),
        Answer::Leave
    );
}
