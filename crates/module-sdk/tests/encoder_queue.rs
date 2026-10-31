use amaterasu_module_sdk::encode::{EncodedPacket, FormatId, OutputQueue, QueueError, SessionMode};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn packet(data: &[u8]) -> EncodedPacket {
    EncodedPacket {
        flags: 0,
        session_id: 1,
        access_unit_id: 1,
        submission_id: 0,
        source_id: 1,
        config_generation: 1,
        timestamp_us: 0,
        codec: FormatId::default(),
        framing: FormatId::default(),
        bytes: data.to_vec(),
    }
}

#[test]
fn rejected_delivery_and_full_queue_preserve_exact_packet() {
    let queue = OutputQueue::new(SessionMode::Source, 1, 8).unwrap();
    queue.push(packet(b"first")).unwrap();
    let first = queue.front().unwrap();
    let (error, second) = queue.push(packet(b"next")).unwrap_err();
    assert_eq!(error, QueueError::Full);
    // A rejected sink does not acknowledge the packet.
    assert!(Arc::ptr_eq(&first, &queue.front().unwrap()));
    assert_eq!(queue.front().unwrap().bytes, b"first");
    queue.acknowledge(&first).unwrap();
    queue.push(*second).unwrap();
    assert_eq!(queue.acknowledge(&first), Err(QueueError::NotFront));
    assert_eq!(queue.front().unwrap().bytes, b"next");
}

#[test]
fn drain_completes_accepted_output_then_eos() {
    let queue = OutputQueue::new(SessionMode::Submitted, 1, 8).unwrap();
    queue.drain().unwrap();
    queue.drain().unwrap();
    queue.push(packet(b"work")).unwrap();
    let mut eos = packet(b"");
    eos.flags = amaterasu_module_sdk::media_abi::ENCODER_CHUNK_END_OF_STREAM;
    let (error, eos) = queue.finish(eos).unwrap_err();
    assert_eq!(error, QueueError::Full);
    queue.acknowledge(&queue.front().unwrap()).unwrap();
    queue.finish(*eos).unwrap();
    assert!(queue.snapshot().ended);
    assert_eq!(
        queue.front().unwrap().flags,
        amaterasu_module_sdk::media_abi::ENCODER_CHUNK_END_OF_STREAM
    );
    assert_eq!(
        queue.push(packet(b"late")).unwrap_err().0,
        QueueError::Closed
    );
}

#[test]
fn notifications_before_wait_and_cancellation_wake_waiters() {
    let queue = OutputQueue::new(SessionMode::Source, 2, 8).unwrap();
    let old = queue.snapshot().generation;
    queue.push(packet(b"x")).unwrap();
    let started = Instant::now();
    assert!(
        queue
            .wait_for_change(old, Duration::from_secs(2))
            .output_ready
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    let current = queue.snapshot().generation;
    let waiting = queue.clone();
    let worker =
        std::thread::spawn(move || waiting.wait_for_change(current, Duration::from_secs(2)));
    queue.cancel();
    assert!(worker.join().unwrap().cancelled);
    assert!(queue.front().is_none());
}

#[test]
fn source_rejects_submission_identity() {
    let queue = OutputQueue::new(SessionMode::Source, 1, 8).unwrap();
    let mut input = packet(b"x");
    input.submission_id = 7;
    assert_eq!(queue.push(input).unwrap_err().0, QueueError::InvalidPacket);
}
