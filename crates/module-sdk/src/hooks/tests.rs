use super::*;
use amaterasu_contracts::{HookOutcome as WireOutcome, Proposal};
use serde::Deserialize;
use serde_json::json;
use std::collections::VecDeque;

const HOOK: &str = "example.launch.environment/1";

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

    fn poll(&mut self, run: &mut HookRun) -> Poll<HookOutcome> {
        let (replies, sent) = (&mut self.replies, &mut self.sent);
        run.poll_with(&mut |command| {
            sent.push(serde_json::to_value(&command).unwrap());
            replies
                .pop_front()
                .expect("the run sent an unscripted command")
        })
    }

    fn operations(&self) -> Vec<&str> {
        self.sent
            .iter()
            .map(|command| command["op"].as_str().unwrap())
            .collect()
    }
}

fn run() -> HookRun {
    HookRun::new(HOOK, "launch.1", &json!({"path": ["base"]})).unwrap()
}

fn accepted() -> ModuleResult<Response> {
    Ok(Response::Accepted { id: 6 })
}

fn refused(code: &str) -> ModuleResult<Response> {
    Ok(Response::error(code, "scripted"))
}

fn lost() -> ModuleResult<Response> {
    Err("reply lost".into())
}

fn proposals(answers: &[(&str, Value)]) -> ModuleResult<Response> {
    Ok(Response::HookResult {
        proposals: answers
            .iter()
            .map(|(participant, payload)| Proposal {
                participant: (*participant).into(),
                payload: payload.clone(),
            })
            .collect(),
    })
}

fn completed(outcome: Poll<HookOutcome>) -> HookReport {
    match outcome {
        Poll::Ready(HookOutcome::Completed(report)) => report,
        other => panic!("expected a completed run, got {other:?}"),
    }
}

#[test]
fn a_required_run_reports_every_answer_and_declares_its_identifier_transient() {
    let mut run = run();
    let mut script = Script::new([
        accepted(),
        Ok(Response::Pending),
        proposals(&[
            ("first", json!({"add": "one"})),
            ("second", json!({"add": "two"})),
        ]),
        Ok(Response::Ok),
    ]);
    assert!(script.poll(&mut run).is_pending());
    assert!(script.poll(&mut run).is_pending());
    let report = completed(script.poll(&mut run));
    assert!(run.is_settled());
    assert_eq!(
        report.answers().collect::<Vec<_>>(),
        [
            ("first", &json!({"add": "one"})),
            ("second", &json!({"add": "two"}))
        ]
    );
    assert_eq!(report.failures().count(), 0);
    assert_eq!(
        script.sent[0],
        json!({
            "op": "run_hook_operation",
            "hook": HOOK,
            "payload": {"path": ["base"]},
            "operation_id": "launch.1",
            "cleanup": false,
            // The identifier is given up with the run's record.
            "transient": true,
        })
    );
    assert_eq!(script.sent[3], json!({"op":"release_hook","invocation":6}));
}

#[test]
fn the_run_options_reach_the_broker() {
    let mut run = run().best_effort().chained().fenced();
    let mut script = Script::new([
        accepted(),
        Ok(Response::HookCleanupResult { outcomes: vec![] }),
        Ok(Response::Ok),
    ]);
    assert!(script.poll(&mut run).is_pending());
    completed(script.poll(&mut run));
    assert_eq!(script.sent[0]["cleanup"], true);
    assert_eq!(script.sent[0]["chain"], true);
    // A fenced run keeps its identifier for the broker's lifetime.
    assert!(script.sent[0].get("transient").is_none());
    assert_eq!(script.sent[2], json!({"op":"release_hook","invocation":6}));
}

#[test]
fn a_best_effort_run_keeps_each_participants_outcome_apart() {
    let mut run = run().best_effort().chained();
    let outcome = |participant: &str, result: Response| WireOutcome {
        participant: participant.into(),
        result: Box::new(result),
    };
    let mut script = Script::new([
        accepted(),
        Ok(Response::HookCleanupResult {
            outcomes: vec![
                outcome(
                    "first",
                    Response::error("unavailable", "hook participant is absent"),
                ),
                outcome(
                    "second",
                    Response::Result {
                        payload: json!({"path": ["base", "two"]}),
                    },
                ),
                outcome(
                    "third",
                    Response::error("refused.not_interested", "no change"),
                ),
            ],
        }),
        Ok(Response::Ok),
    ]);
    assert!(script.poll(&mut run).is_pending());
    let report = completed(script.poll(&mut run));
    let failures: Vec<_> = report.failures().collect();
    assert_eq!(failures[0].0, "first");
    assert_eq!(failures[0].1.code, ErrorCode::Unavailable);
    assert!(!failures[0].1.is_refusal());
    assert_eq!(failures[1].0, "third");
    assert!(failures[1].1.is_refusal());
    // The chain ends with the last answer that was given.
    assert_eq!(report.value(), &json!({"path": ["base", "two"]}));
}

#[test]
fn a_chain_nobody_answered_ends_with_the_owners_input() {
    let mut run = run().best_effort().chained();
    let mut script = Script::new([
        accepted(),
        Ok(Response::HookCleanupResult { outcomes: vec![] }),
        Ok(Response::Ok),
    ]);
    assert!(script.poll(&mut run).is_pending());
    assert_eq!(
        completed(script.poll(&mut run)).value(),
        &json!({"path": ["base"]})
    );
}

#[test]
fn answers_decode_individually() {
    #[derive(Debug, PartialEq, Deserialize)]
    struct Addition {
        add: String,
    }
    let mut run = run();
    let mut script = Script::new([
        accepted(),
        proposals(&[
            ("first", json!({"add": "one"})),
            ("second", json!({"add": 2})),
        ]),
        Ok(Response::Ok),
    ]);
    assert!(script.poll(&mut run).is_pending());
    let report = completed(script.poll(&mut run));
    let decoded = report.decode::<Addition>();
    assert_eq!(decoded[0], ("first", Ok(Addition { add: "one".into() })));
    assert_eq!(decoded[1].0, "second");
    assert!(decoded[1].1.is_err());
}

#[test]
fn a_run_the_broker_does_not_start_asked_nobody() {
    let mut run = run();
    let mut script = Script::new([refused("unavailable")]);
    assert!(matches!(
        script.poll(&mut run),
        Poll::Ready(HookOutcome::Refused(refusal)) if refusal.code == ErrorCode::Unavailable
    ));
    assert_eq!(script.operations(), ["run_hook_operation"]);
}

#[test]
fn a_participants_refusal_stops_a_required_run() {
    let mut run = run();
    let mut script = Script::new([
        accepted(),
        Ok(Response::error(
            "refused.stream_active",
            "first: a stream is running",
        )),
        Ok(Response::Ok),
    ]);
    assert!(script.poll(&mut run).is_pending());
    let Poll::Ready(HookOutcome::Stopped(stop)) = script.poll(&mut run) else {
        panic!("a refusal stops a required run");
    };
    assert!(stop.is_refusal());
    assert_eq!(stop.code, ErrorCode::Other("stream_active".into()));
    assert_eq!(stop.message, "first: a stream is running");
    assert_eq!(
        script.operations(),
        ["run_hook_operation", "hook_result", "release_hook"]
    );
}

#[test]
fn a_lost_start_is_repeated_with_the_identical_submission() {
    let mut run = run();
    let mut script = Script::new([lost(), accepted(), proposals(&[]), Ok(Response::Ok)]);
    assert!(script.poll(&mut run).is_pending());
    assert!(script.poll(&mut run).is_pending());
    completed(script.poll(&mut run));
    assert_eq!(script.sent[0], script.sent[1]);
}

#[test]
fn a_run_whose_result_expired_is_unconfirmed_not_refused() {
    let mut run = run();
    let mut script = Script::new([lost(), refused("operation_expired")]);
    assert!(script.poll(&mut run).is_pending());
    assert!(matches!(
        script.poll(&mut run),
        Poll::Ready(HookOutcome::Unconfirmed(_))
    ));
}

#[test]
fn cancelling_before_the_start_sends_nothing() {
    let mut run = run();
    run.cancel();
    let mut script = Script::new([]);
    assert!(matches!(
        script.poll(&mut run),
        Poll::Ready(HookOutcome::Refused(refusal)) if refusal.code == ErrorCode::Cancelled
    ));
}

#[test]
fn cancelling_a_running_hook_ends_it_and_releases_its_record() {
    let mut run = run();
    let mut script = Script::new([
        accepted(),
        Ok(Response::Pending),
        Ok(Response::Ok),
        refused("cancelled"),
        Ok(Response::Ok),
    ]);
    assert!(script.poll(&mut run).is_pending());
    assert!(script.poll(&mut run).is_pending());
    run.cancel();
    assert!(matches!(
        script.poll(&mut run),
        Poll::Ready(HookOutcome::Stopped(stop)) if stop.code == ErrorCode::Cancelled
    ));
    assert_eq!(
        script.operations(),
        [
            "run_hook_operation",
            "hook_result",
            "cancel_hook",
            "hook_result",
            "release_hook"
        ]
    );
}

#[test]
fn a_cancelled_run_with_a_lost_start_only_looks_for_it() {
    let mut run = run();
    let mut script = Script::new([lost(), refused("not_found")]);
    assert!(script.poll(&mut run).is_pending());
    run.cancel();
    assert!(matches!(
        script.poll(&mut run),
        Poll::Ready(HookOutcome::Refused(refusal)) if refusal.code == ErrorCode::Cancelled
    ));
    assert_eq!(script.operations(), ["run_hook_operation", "lookup_hook"]);
}

#[test]
fn the_outcome_is_returned_even_when_its_record_cannot_be_released() {
    let mut run = run();
    let mut script = Script::new([accepted(), proposals(&[]), lost(), lost(), lost()]);
    assert!(script.poll(&mut run).is_pending());
    assert!(script.poll(&mut run).is_pending());
    assert!(script.poll(&mut run).is_pending());
    completed(script.poll(&mut run));
}

#[test]
fn repeated_lost_exchanges_end_as_unconfirmed() {
    let mut run = run();
    let mut script = Script::new([accepted(), lost(), lost(), lost()]);
    assert!(script.poll(&mut run).is_pending());
    assert!(script.poll(&mut run).is_pending());
    assert!(script.poll(&mut run).is_pending());
    assert!(matches!(
        script.poll(&mut run),
        Poll::Ready(HookOutcome::Unconfirmed(ControlError::Transport(_)))
    ));
}

#[test]
fn a_participant_recognizes_a_hook_request_by_its_hook() {
    let request = |service: &str| IncomingRequest {
        id: 1,
        caller: "owner".into(),
        initiator: "owner".into(),
        service: service.to_owned(),
        payload: json!({}),
        remaining_ms: 100,
    };
    assert_eq!(
        request("hooks/example.launch.environment/1").hook(),
        Some(HOOK)
    );
    assert_eq!(request("example.echo/1").hook(), None);
}
