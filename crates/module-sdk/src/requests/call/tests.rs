use super::*;
use serde::Deserialize;
use serde_json::json;
use std::collections::VecDeque;

/// A transport that answers each command with the next scripted reply and
/// fails the test when the call sends more than the script expects.
struct Script {
    replies: VecDeque<ModuleResult<Response>>,
    sent: Vec<Command>,
}

impl Script {
    fn new(replies: impl IntoIterator<Item = ModuleResult<Response>>) -> Self {
        Self {
            replies: replies.into_iter().collect(),
            sent: Vec::new(),
        }
    }

    fn poll_at(&mut self, call: &mut OutboundCall, now: Instant) -> Poll<CallOutcome> {
        let (replies, sent) = (&mut self.replies, &mut self.sent);
        call.poll_at(now, &mut |command| {
            sent.push(command);
            replies
                .pop_front()
                .expect("the call sent an unscripted command")
        })
    }

    fn poll(&mut self, call: &mut OutboundCall) -> Poll<CallOutcome> {
        self.poll_at(call, Instant::now())
    }

    fn sent_operations(&self) -> Vec<String> {
        self.sent
            .iter()
            .map(|command| {
                serde_json::to_value(command).unwrap()["op"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }
}

fn options() -> RequestOptions<'static> {
    RequestOptions {
        service: "example.store/1",
        timeout_ms: 5_000,
        operation_id: "example.write.1",
        parent: Some(3),
    }
}

fn write_call() -> OutboundCall {
    OutboundCall::request(options(), &json!({"action":"write","value":1})).unwrap()
}

fn accepted() -> ModuleResult<Response> {
    Ok(Response::Accepted { id: 7 })
}

fn refused(code: &str) -> ModuleResult<Response> {
    Ok(Response::error(code, "scripted"))
}

fn lost() -> ModuleResult<Response> {
    Err("reply lost".into())
}

fn reply(payload: Value) -> ModuleResult<Response> {
    Ok(Response::Result { payload })
}

#[test]
fn a_reply_is_returned_once_its_broker_record_is_released() {
    let mut call = write_call();
    let mut script = Script::new([
        accepted(),
        Ok(Response::Pending),
        reply(json!({"ok":true})),
        Ok(Response::Ok),
    ]);
    assert_eq!(call.request_id(), None);
    assert!(script.poll(&mut call).is_pending());
    assert_eq!(call.request_id(), Some(7));
    assert!(script.poll(&mut call).is_pending());
    assert_eq!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Completed(json!({"ok":true})))
    );
    assert!(call.is_settled());
    assert_eq!(
        script.sent_operations(),
        ["request", "take_result", "take_result", "release_result"]
    );
    assert_eq!(
        serde_json::to_value(&script.sent[0]).unwrap(),
        json!({"op":"request","service":"example.store/1","payload":{"action":"write","value":1},
            "timeout_ms":5000,"operation_id":"example.write.1","parent":3})
    );
}

#[test]
fn a_broker_refusal_proves_no_provider_saw_the_request() {
    for code in [
        "permission_denied",
        "unavailable",
        "resource_exhausted",
        "conflict",
    ] {
        let mut call = write_call();
        let mut script = Script::new([refused(code)]);
        let Poll::Ready(CallOutcome::Refused(refusal)) = script.poll(&mut call) else {
            panic!("{code} must refuse the call");
        };
        assert_eq!(refusal.code.as_str(), code);
        assert!(call.is_settled());
    }
}

#[test]
fn a_lost_admission_is_repeated_with_the_identical_submission() {
    let mut call = write_call();
    let mut script = Script::new([lost(), accepted(), reply(json!(1)), Ok(Response::Ok)]);
    assert!(script.poll(&mut call).is_pending());
    assert!(script.poll(&mut call).is_pending());
    assert_eq!(
        serde_json::to_value(&script.sent[0]).unwrap(),
        serde_json::to_value(&script.sent[1]).unwrap()
    );
    assert_eq!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Completed(json!(1)))
    );
}

#[test]
fn a_refusal_after_a_lost_admission_proves_it_never_arrived() {
    let mut call = write_call();
    let mut script = Script::new([lost(), refused("unavailable")]);
    assert!(script.poll(&mut call).is_pending());
    assert!(matches!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Refused(refusal)) if refusal.code == ErrorCode::Unavailable
    ));
}

#[test]
fn an_unknown_admission_is_never_repeated_after_the_retention_window() {
    let started = Instant::now();
    let mut call = write_call();
    let mut script = Script::new([lost()]);
    assert!(script.poll_at(&mut call, started).is_pending());
    // No reply is scripted: sending anything here would fail the test.
    assert!(matches!(
        script.poll_at(&mut call, started + ADMISSION_RETRY_WINDOW),
        Poll::Ready(CallOutcome::Unconfirmed(ControlError::Transport(_)))
    ));
    assert_eq!(script.sent_operations(), ["request"]);
}

#[test]
fn a_mutating_call_that_ends_without_a_reply_keeps_its_broker_record() {
    for code in ["deadline_exceeded", "cancelled"] {
        let mut call = write_call();
        let mut script = Script::new([accepted(), refused(code)]);
        assert!(script.poll(&mut call).is_pending());
        let Poll::Ready(CallOutcome::Unconfirmed(error)) = script.poll(&mut call) else {
            panic!("{code} leaves the provider's effect unknown");
        };
        assert_eq!(error.code().unwrap().as_str(), code);
        assert_eq!(script.sent_operations(), ["request", "take_result"]);
    }
}

#[test]
fn a_read_only_call_that_ends_without_a_reply_releases_its_broker_record() {
    let mut call = write_call().read_only();
    let mut script = Script::new([accepted(), refused("deadline_exceeded"), Ok(Response::Ok)]);
    assert!(script.poll(&mut call).is_pending());
    assert!(matches!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Unconfirmed(error)) if error.is(&ErrorCode::DeadlineExceeded)
    ));
    assert_eq!(
        script.sent_operations(),
        ["request", "take_result", "release_result"]
    );

    // An expired record has nothing left to release.
    let mut call = write_call().read_only();
    let mut script = Script::new([accepted(), refused("not_found")]);
    assert!(script.poll(&mut call).is_pending());
    assert!(script.poll(&mut call).is_ready());
    assert_eq!(script.sent_operations(), ["request", "take_result"]);
}

#[test]
fn a_known_reply_survives_a_failing_release() {
    let mut call = write_call();
    let mut script = Script::new([
        accepted(),
        reply(json!("kept")),
        lost(),
        refused("not_found"),
    ]);
    assert!(script.poll(&mut call).is_pending());
    assert!(script.poll(&mut call).is_pending());
    // The lost release did happen; the retry learns the record is gone.
    assert_eq!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Completed(json!("kept")))
    );

    let mut call = write_call();
    let mut script = Script::new([accepted(), reply(json!("kept")), lost(), lost(), lost()]);
    assert!(script.poll(&mut call).is_pending());
    assert!(script.poll(&mut call).is_pending());
    assert!(script.poll(&mut call).is_pending());
    assert_eq!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Completed(json!("kept")))
    );
}

#[test]
fn repeated_transport_failure_stops_with_the_effect_unknown() {
    let mut call = write_call();
    let mut script = Script::new([accepted(), lost(), lost(), lost()]);
    assert!(script.poll(&mut call).is_pending());
    assert!(script.poll(&mut call).is_pending());
    assert!(script.poll(&mut call).is_pending());
    assert!(matches!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Unconfirmed(ControlError::Transport(_)))
    ));

    // A successful exchange in between starts the count again.
    let mut call = write_call();
    let mut script = Script::new([
        accepted(),
        lost(),
        lost(),
        Ok(Response::Pending),
        lost(),
        lost(),
        Ok(Response::Pending),
    ]);
    for _ in 0..7 {
        assert!(script.poll(&mut call).is_pending());
    }
}

#[test]
fn cancelling_before_submission_sends_nothing() {
    let mut call = write_call();
    call.cancel();
    let mut script = Script::new([]);
    assert!(matches!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Refused(refusal)) if refusal.code == ErrorCode::Cancelled
    ));
    assert!(script.sent.is_empty());
}

#[test]
fn cancelling_an_unknown_admission_looks_it_up_without_admitting_work() {
    let mut call = write_call();
    let mut script = Script::new([lost(), refused("not_found")]);
    assert!(script.poll(&mut call).is_pending());
    call.cancel();
    assert!(matches!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Refused(refusal)) if refusal.code == ErrorCode::Cancelled
    ));
    assert_eq!(script.sent_operations(), ["request", "lookup_request"]);
    assert_eq!(
        serde_json::to_value(&script.sent[1]).unwrap(),
        json!({"op":"lookup_request","service":"example.store/1",
            "payload":{"action":"write","value":1},"operation_id":"example.write.1","parent":3})
    );

    // The lost submission had been admitted: cancel the request it created.
    let mut call = write_call();
    let mut script = Script::new([lost(), accepted(), Ok(Response::Ok), refused("cancelled")]);
    assert!(script.poll(&mut call).is_pending());
    call.cancel();
    assert!(script.poll(&mut call).is_pending());
    assert!(matches!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Unconfirmed(error)) if error.is(&ErrorCode::Cancelled)
    ));
    assert_eq!(
        script.sent_operations(),
        ["request", "lookup_request", "cancel", "take_result"]
    );
}

#[test]
fn a_reply_recorded_before_cancellation_is_still_returned() {
    let mut call = write_call();
    let mut script = Script::new([
        accepted(),
        Ok(Response::Pending),
        Ok(Response::Ok),
        reply(json!({"ok":true})),
        Ok(Response::Ok),
    ]);
    assert!(script.poll(&mut call).is_pending());
    assert!(script.poll(&mut call).is_pending());
    call.cancel();
    assert_eq!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Completed(json!({"ok":true})))
    );
    assert_eq!(
        script.sent_operations(),
        [
            "request",
            "take_result",
            "cancel",
            "take_result",
            "release_result"
        ]
    );
}

#[test]
fn a_lost_cancellation_is_sent_again_and_only_once_it_lands() {
    let mut call = write_call();
    let mut script = Script::new([
        accepted(),
        lost(),
        Ok(Response::Ok),
        Ok(Response::Pending),
        refused("cancelled"),
    ]);
    assert!(script.poll(&mut call).is_pending());
    call.cancel();
    assert!(script.poll(&mut call).is_pending());
    assert!(script.poll(&mut call).is_pending());
    assert!(script.poll(&mut call).is_ready());
    assert_eq!(
        script.sent_operations(),
        ["request", "cancel", "cancel", "take_result", "take_result"]
    );
}

#[test]
fn an_api_call_submits_and_looks_up_its_exact_method() {
    let mut call = OutboundCall::api(
        ApiRequestOptions {
            request: options(),
            method: "read",
            interface_major: 2,
            interface_minor: 1,
        },
        &json!({"key":"k"}),
    )
    .unwrap();
    let mut script = Script::new([lost(), refused("not_found")]);
    assert!(script.poll(&mut call).is_pending());
    call.cancel();
    assert!(script.poll(&mut call).is_ready());
    assert_eq!(
        serde_json::to_value(&script.sent[0]).unwrap(),
        json!({"op":"call_api","service":"example.store/1","method":"read","interface_major":2,
            "interface_minor":1,"payload":{"key":"k"},"timeout_ms":5000,
            "operation_id":"example.write.1","parent":3})
    );
    assert_eq!(
        serde_json::to_value(&script.sent[1]).unwrap(),
        json!({"op":"lookup_api_request","service":"example.store/1","method":"read",
            "interface_major":2,"interface_minor":1,"payload":{"key":"k"},
            "operation_id":"example.write.1","parent":3})
    );
}

#[test]
fn polling_a_settled_call_sends_nothing_and_reports_the_misuse() {
    let mut call = write_call();
    let mut script = Script::new([refused("unavailable")]);
    assert!(script.poll(&mut call).is_ready());
    assert!(matches!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Unconfirmed(ControlError::UnexpectedReply(_)))
    ));
    assert_eq!(script.sent_operations(), ["request"]);
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Stored {
    revision: u64,
}

#[test]
fn a_reply_of_the_wrong_shape_is_not_a_completed_call() {
    assert_eq!(
        CallOutcome::Completed(json!({"revision":4})).decode::<Stored>(),
        CallOutcome::Completed(Stored { revision: 4 })
    );
    assert!(matches!(
        CallOutcome::Completed(json!({"revision":"four"})).decode::<Stored>(),
        CallOutcome::Unconfirmed(ControlError::Payload(_))
    ));
    let refusal = BrokerError::new(ErrorCode::Unavailable, "no provider");
    assert_eq!(
        CallOutcome::Refused(refusal.clone()).decode::<Stored>(),
        CallOutcome::Refused(refusal)
    );
}

#[test]
fn generated_operation_identifiers_do_not_repeat() {
    let first = unique_operation_id("example.read");
    let second = unique_operation_id("example.read");
    assert_ne!(first, second);
    assert!(first.starts_with("example.read."));
    assert!(amaterasu_contracts::valid_name(&first));
}

#[test]
fn a_serving_principals_refusal_settles_as_refused_and_releases_the_record() {
    let mut call = write_call();
    let mut script = Script::new([accepted(), refused("refused.not_found"), Ok(Response::Ok)]);
    assert!(script.poll(&mut call).is_pending());
    let Poll::Ready(CallOutcome::Refused(refusal)) = script.poll(&mut call) else {
        panic!("a refusal means the operation was not performed");
    };
    // The provider's `not_found` is its own answer, not a lost broker record.
    assert_eq!(refusal.code, ErrorCode::NotFound);
    assert!(refusal.is_refusal());
    assert_eq!(
        script.sent_operations(),
        ["request", "take_result", "release_result"]
    );
}

#[test]
fn a_refusal_is_still_returned_when_its_record_cannot_be_released() {
    let mut call = write_call();
    let mut script = Script::new([accepted(), refused("refused.busy"), lost(), lost(), lost()]);
    assert!(script.poll(&mut call).is_pending());
    assert!(script.poll(&mut call).is_pending());
    assert!(script.poll(&mut call).is_pending());
    assert!(matches!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Refused(refusal)) if refusal.code == ErrorCode::Busy
    ));
}

#[test]
fn a_single_use_call_releases_a_record_no_repeat_will_need() {
    let mut call = write_call().single_use();
    let mut script = Script::new([accepted(), refused("cancelled"), Ok(Response::Ok)]);
    assert!(script.poll(&mut call).is_pending());
    // The provider may still have acted, so the outcome stays unconfirmed.
    assert!(matches!(
        script.poll(&mut call),
        Poll::Ready(CallOutcome::Unconfirmed(_))
    ));
    assert_eq!(
        script.sent_operations(),
        ["request", "take_result", "release_result"]
    );
}
