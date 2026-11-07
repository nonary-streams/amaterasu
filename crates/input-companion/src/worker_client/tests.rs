use super::*;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

#[derive(Default)]
struct Transcript {
    writes: Vec<Value>,
    reads: VecDeque<InputResult<Option<Vec<u8>>>>,
    fail_next_write: bool,
}

struct SyntheticTransport(Rc<RefCell<Transcript>>);

impl WorkerTransport for SyntheticTransport {
    fn write_frame(&mut self, frame: &[u8], _: Instant) -> InputResult<()> {
        let mut transcript = self.0.borrow_mut();
        transcript
            .writes
            .push(serde_json::from_slice(frame).unwrap());
        if std::mem::take(&mut transcript.fail_next_write) {
            Err(InputError::Device)
        } else {
            Ok(())
        }
    }

    fn read_frame(&mut self, _: Instant) -> InputResult<Option<Vec<u8>>> {
        self.0.borrow_mut().reads.pop_front().unwrap_or(Ok(None))
    }
}

fn reply(frame: Value) -> InputResult<Option<Vec<u8>>> {
    Ok(Some(serde_json::to_vec(&frame).unwrap()))
}

fn connected() -> (WorkerClient<SyntheticTransport>, Rc<RefCell<Transcript>>) {
    let transcript = Rc::new(RefCell::new(Transcript::default()));
    // The installed Windows v1 companion does not include gamepad in its grant.
    transcript.borrow_mut().reads.push_back(reply(json!({
        "kind": "granted", "generation": 1, "token": "0123456789abcdef0123456789abcdef"
    })));
    let client =
        WorkerClient::connect(SyntheticTransport(Rc::clone(&transcript)), REPLY_TIMEOUT).unwrap();
    (client, transcript)
}

fn relative() -> InputEvent {
    InputEvent::Relative { dx: 1, dy: -1 }
}

fn wire_kinds(transcript: &Transcript) -> Vec<&str> {
    transcript
        .writes
        .iter()
        .map(|frame| frame["kind"].as_str().unwrap())
        .collect()
}

#[test]
fn delayed_event_ok_cannot_acknowledge_rejected_close() {
    let (mut client, transcript) = connected();
    transcript.borrow_mut().reads.extend([
        Ok(None),
        reply(json!({"kind": "ok"})),
        reply(json!({"kind": "denied"})),
    ]);
    assert_eq!(client.send(relative()), Err(InputError::Device));
    assert_eq!(client.close(), Err(InputError::Device));
    assert_eq!(client.send(relative()), Err(InputError::Closed));
    assert_eq!(
        wire_kinds(&transcript.borrow()),
        ["worker_start", "worker_event", "worker_close"]
    );
    assert!(!client.closed);
}

#[test]
fn timed_out_close_waits_for_its_original_acknowledgement() {
    let (mut client, transcript) = connected();
    transcript
        .borrow_mut()
        .reads
        .extend([Ok(None), Ok(None), reply(json!({"kind": "ok"}))]);
    assert_eq!(client.close(), Err(InputError::Device));
    assert_eq!(client.close(), Err(InputError::Device));
    assert_eq!(client.close(), Ok(()));
    assert_eq!(client.close(), Ok(()));
    assert_eq!(
        wire_kinds(&transcript.borrow()),
        ["worker_start", "worker_close"]
    );
}

#[test]
fn close_does_not_write_until_outstanding_event_settles() {
    let (mut client, transcript) = connected();
    transcript.borrow_mut().reads.extend([Ok(None), Ok(None)]);
    assert_eq!(client.send(relative()), Err(InputError::Device));
    assert_eq!(client.close(), Err(InputError::Device));
    assert_eq!(
        wire_kinds(&transcript.borrow()),
        ["worker_start", "worker_event"]
    );
    transcript.borrow_mut().reads.extend([
        reply(json!({"kind": "denied"})),
        reply(json!({"kind": "ok"})),
    ]);
    assert_eq!(client.close(), Ok(()));
    assert_eq!(
        wire_kinds(&transcript.borrow()),
        ["worker_start", "worker_event", "worker_close"]
    );
}

#[test]
fn subsequent_event_cannot_consume_an_earlier_event_reply() {
    let (mut client, transcript) = connected();
    transcript.borrow_mut().reads.extend([
        Ok(None),
        reply(json!({"kind": "ok"})),
        reply(json!({"kind": "denied"})),
    ]);
    assert_eq!(client.send(relative()), Err(InputError::Device));
    assert_eq!(client.send(relative()), Err(InputError::PermissionDenied));
    let frames = &transcript.borrow().writes;
    assert_eq!(frames[1]["packet"]["sequence"], 1);
    assert_eq!(frames[2]["packet"]["sequence"], 2);
}

#[test]
fn failed_write_cannot_be_recovered_by_an_unrelated_ok() {
    let (mut client, transcript) = connected();
    transcript.borrow_mut().fail_next_write = true;
    assert_eq!(client.send(relative()), Err(InputError::Device));
    transcript
        .borrow_mut()
        .reads
        .push_back(reply(json!({"kind": "ok"})));
    assert_eq!(client.close(), Err(InputError::Device));
    assert_eq!(client.close(), Err(InputError::Device));
    drop(client);
    assert_eq!(
        wire_kinds(&transcript.borrow()),
        ["worker_start", "worker_event"]
    );
}

#[test]
fn malformed_reply_poisons_the_stream_instead_of_certifying_close() {
    let (mut client, transcript) = connected();
    transcript
        .borrow_mut()
        .reads
        .extend([Ok(Some(b"not-json".to_vec())), reply(json!({"kind": "ok"}))]);
    assert_eq!(client.send(relative()), Err(InputError::InvalidEvent));
    assert_eq!(client.close(), Err(InputError::Device));
    assert!(!client.closed);
    assert_eq!(
        wire_kinds(&transcript.borrow()),
        ["worker_start", "worker_event"]
    );
}

#[test]
fn transport_failure_poisons_pending_close_and_preserves_failed_cleanup() {
    let (mut client, transcript) = connected();
    transcript
        .borrow_mut()
        .reads
        .extend([Err(InputError::Closed), reply(json!({"kind": "ok"}))]);
    assert_eq!(client.close(), Err(InputError::Closed));
    assert_eq!(client.close(), Err(InputError::Device));
    assert!(!client.closed);
    assert_eq!(
        wire_kinds(&transcript.borrow()),
        ["worker_start", "worker_close"]
    );
}

#[test]
fn unrelated_grant_cannot_complete_an_outstanding_event_or_close() {
    let (mut client, transcript) = connected();
    transcript.borrow_mut().reads.push_back(reply(json!({
        "kind": "granted", "generation": 1, "token": "0123456789abcdef0123456789abcdef"
    })));
    assert_eq!(client.send(relative()), Err(InputError::InvalidEvent));
    assert_eq!(client.close(), Err(InputError::Device));
    assert!(!client.closed);
}

#[cfg(target_os = "linux")]
#[test]
fn delayed_feedback_reply_is_consumed_before_close_is_sent() {
    let (mut client, transcript) = connected();
    transcript.borrow_mut().reads.extend([
        Ok(None),
        reply(json!({"kind": "feedback", "event": null})),
        reply(json!({"kind": "denied"})),
    ]);
    assert_eq!(client.poll_gamepad_feedback(), Err(InputError::Device));
    assert_eq!(client.close(), Err(InputError::Device));
    assert_eq!(
        wire_kinds(&transcript.borrow()),
        ["worker_start", "worker_feedback_poll", "worker_close"]
    );
    assert!(!client.closed);
}

#[test]
fn expired_shutdown_deadline_never_writes_or_consumes_a_reply() {
    let (mut client, transcript) = connected();
    transcript
        .borrow_mut()
        .reads
        .push_back(reply(json!({"kind": "ok"})));
    client.restrict_deadline(Instant::now());
    assert_eq!(client.close(), Err(InputError::Device));
    assert_eq!(client.close(), Err(InputError::Device));
    assert!(!client.closed);
    assert_eq!(transcript.borrow().reads.len(), 1);
    assert_eq!(wire_kinds(&transcript.borrow()), ["worker_start"]);
    drop(client);
    assert_eq!(wire_kinds(&transcript.borrow()), ["worker_start"]);
}

#[test]
fn expired_deadline_preserves_the_original_pending_close() {
    let (mut client, transcript) = connected();
    assert_eq!(client.close(), Err(InputError::Device));
    client.restrict_deadline(Instant::now());
    transcript
        .borrow_mut()
        .reads
        .push_back(reply(json!({"kind": "ok"})));
    assert_eq!(client.close(), Err(InputError::Device));
    assert_eq!(client.pending, Some(Operation::Close));
    assert!(!client.closed);
    assert_eq!(transcript.borrow().reads.len(), 1);
    assert_eq!(
        wire_kinds(&transcript.borrow()),
        ["worker_start", "worker_close"]
    );
}

#[test]
fn later_shutdown_announcement_does_not_renew_an_expired_deadline() {
    let (mut client, transcript) = connected();
    client.restrict_deadline(Instant::now());
    client.restrict_deadline(Instant::now() + Duration::from_secs(60));
    assert_eq!(client.close(), Err(InputError::Device));
    assert_eq!(wire_kinds(&transcript.borrow()), ["worker_start"]);
}

#[test]
fn expired_initialization_never_requests_a_native_lease() {
    let transcript = Rc::new(RefCell::new(Transcript::default()));
    let client = WorkerClient::connect_with_deadline(
        SyntheticTransport(Rc::clone(&transcript)),
        REPLY_TIMEOUT,
        Some(Instant::now()),
    );
    assert!(matches!(client, Err(InputError::Device)));
    assert!(transcript.borrow().writes.is_empty());
}
