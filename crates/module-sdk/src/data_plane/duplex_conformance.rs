//! In-process conformance model. This does not exercise a real HTTP/TLS owner
//! or separately loaded native providers.
use super::{abi::*, *};
use crate::abi::STATUS_OK;
use std::sync::{Arc, Barrier};

fn desc(direction: u32) -> Description {
    let schema = Schema {
        namespace: 91,
        value: 1,
        version: 1,
    };
    description(
        direction,
        schema,
        schema,
        Limits {
            max_chunk_bytes: 4,
            max_queued_bytes: 4,
            max_record_bytes: 4,
            max_queued_chunks: 1,
            max_segments: 1,
            max_metadata_bytes: 0,
            reserved: 0,
        },
    )
    .unwrap()
}

// The owner consumes a complete transfer record synchronously; no ABI borrow escapes.
fn read(channel: &Channel) -> Result<Vec<u8>, i32> {
    let mut bytes = Vec::new();
    // SAFETY: descriptor and segment are read only within the callback.
    let status = unsafe {
        channel.receive(|chunk| {
            for segment in std::slice::from_raw_parts(chunk.segments, chunk.segment_count as usize)
            {
                bytes.extend_from_slice(std::slice::from_raw_parts(segment.data, segment.len));
            }
            STATUS_OK
        })
    };
    if status == STATUS_OK {
        Ok(bytes)
    } else {
        Err(status)
    }
}

trait Provider {
    fn process(&mut self, input: &Channel, output: &Channel) -> i32;
}

// Two distinct in-process implementations of the same neutral interface.
struct CopyProvider {
    next: u64,
}
impl Provider for CopyProvider {
    fn process(&mut self, input: &Channel, output: &Channel) -> i32 {
        // SAFETY: data is copied before the synchronous callback returns.
        unsafe {
            input.receive(|chunk| {
                let segment = &*chunk.segments;
                let data = std::slice::from_raw_parts(segment.data, segment.len).to_vec();
                match output.push(OwnedChunk::with_bytes(self.next, data)) {
                    Ok(()) => {
                        self.next += 1;
                        STATUS_OK
                    }
                    Err((status, _)) => status,
                }
            })
        }
    }
}
struct ReverseProvider {
    next: u64,
}
impl Provider for ReverseProvider {
    fn process(&mut self, input: &Channel, output: &Channel) -> i32 {
        // SAFETY: data is copied before the synchronous callback returns.
        unsafe {
            input.receive(|chunk| {
                let segment = &*chunk.segments;
                let data = std::slice::from_raw_parts(segment.data, segment.len)
                    .iter()
                    .rev()
                    .copied()
                    .collect();
                match output.push(OwnedChunk::with_bytes(self.next, data)) {
                    Ok(()) => {
                        self.next += 1;
                        STATUS_OK
                    }
                    Err((status, _)) => status,
                }
            })
        }
    }
}

fn exercise_owner(provider: &mut dyn Provider, expected: [&[u8]; 2]) {
    let hub = Hub::new(2, 8).unwrap();
    let pair = DuplexPair::create(&hub, "consumer", desc(SOURCE), desc(SINK)).unwrap();
    // Post-request plaintext was already read by the connection owner. It is
    // admitted before the next network read, and a full queue pauses that read.
    pair.input()
        .push(OwnedChunk::with_bytes(1, b"pre".to_vec()))
        .unwrap();
    let later = pair
        .input()
        .push(OwnedChunk::with_bytes(2, b"next".to_vec()))
        .unwrap_err();
    assert_eq!(later.0, BUSY);
    assert_eq!(provider.process(pair.input(), pair.output()), STATUS_OK);
    pair.input().push(later.1).unwrap();
    // Output backpressure rejects input delivery without consuming its record.
    assert_eq!(provider.process(pair.input(), pair.output()), BUSY);
    assert_eq!(read(pair.output()).unwrap(), expected[0]);
    assert_eq!(provider.process(pair.input(), pair.output()), STATUS_OK);
    pair.input().finish().unwrap();
    assert_eq!(read(pair.input()), Err(CLOSED));
    assert_eq!(read(pair.output()).unwrap(), expected[1]);
    pair.output().finish().unwrap();
    assert_eq!(read(pair.output()), Err(CLOSED));
}

#[test]
fn duplex_neutral_providers_preserve_buffered_input_and_backpressure() {
    exercise_owner(&mut CopyProvider { next: 1 }, [b"pre", b"next"]);
    exercise_owner(&mut ReverseProvider { next: 1 }, [b"erp", b"txen"]);
}

#[test]
fn duplex_retirement_signals_both_before_waiting_for_callbacks() {
    let hub = Hub::new(2, 8).unwrap();
    let pair = DuplexPair::create(&hub, "consumer", desc(SOURCE), desc(SINK)).unwrap();
    let input = pair.input().clone();
    let output = pair.output().clone();
    input
        .push(OwnedChunk::with_bytes(1, b"x".to_vec()))
        .unwrap();
    let generation = output.snapshot().generation;
    let entered = Arc::new(Barrier::new(2));
    let worker = {
        let entered = entered.clone();
        std::thread::spawn(move || {
            // SAFETY: no descriptor pointers escape the callback.
            unsafe {
                input.receive(|_| {
                    entered.wait();
                    assert_eq!(output.wait(generation, 10_000).unwrap().phase, CANCELLED);
                    STATUS_OK
                })
            }
        })
    };
    entered.wait();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let retire = std::thread::spawn(move || {
        drop(pair);
        done_tx.send(()).unwrap();
    });
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("both cancellations must wake the callback");
    assert_eq!(worker.join().unwrap(), STATUS_OK);
    retire.join().unwrap();
}

#[test]
fn duplex_output_stages_a_whole_record_before_partial_writes() {
    let hub = Hub::new(2, 8).unwrap();
    let pair = DuplexPair::create(&hub, "consumer", desc(SOURCE), desc(SINK)).unwrap();
    pair.output()
        .push(OwnedChunk::with_bytes(1, b"abcd".to_vec()))
        .unwrap();
    // Acceptance copies the whole bounded record once; partial writes advance
    // staging progress and never cause transfer to redeliver the same record.
    let staged = read(pair.output()).unwrap();
    let mut wire = Vec::new();
    let mut offset = 0;
    while offset < staged.len() {
        let end = (offset + 2).min(staged.len());
        wire.extend_from_slice(&staged[offset..end]);
        offset = end;
    }
    assert_eq!(wire, b"abcd");
    assert_eq!(read(pair.output()), Err(EMPTY));
}

// The real owner must serialize this gate with socket writes and authority
// invalidation. This deliberately models only single-threaded owner behavior.
struct FakeConnection {
    pair: Option<DuplexPair>,
    open: bool,
    sent: Vec<u8>,
}
impl FakeConnection {
    fn flush(&mut self) {
        if self.open
            && let Some(pair) = &self.pair
            && let Ok(bytes) = read(pair.output())
        {
            self.sent.extend(bytes);
        }
    }
    fn disconnect_or_revoke(&mut self) {
        self.open = false;
        drop(self.pair.take());
    }
}

#[test]
fn duplex_owner_disconnect_or_revoke_discards_pending_output() {
    for _reason in ["disconnect", "revocation"] {
        let hub = Hub::new(2, 8).unwrap();
        let pair = DuplexPair::create(&hub, "consumer", desc(SOURCE), desc(SINK)).unwrap();
        let input = pair.input().clone();
        let output = pair.output().clone();
        output
            .push(OwnedChunk::with_bytes(1, b"late".to_vec()))
            .unwrap();
        let mut connection = FakeConnection {
            pair: Some(pair),
            open: true,
            sent: Vec::new(),
        };
        connection.disconnect_or_revoke();
        connection.flush();
        assert!(connection.sent.is_empty());
        assert_eq!(read(&input), Err(CLOSED));
        assert_eq!(read(&output), Err(CLOSED));
    }
}
