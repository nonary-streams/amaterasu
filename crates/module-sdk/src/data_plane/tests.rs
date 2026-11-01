use super::{abi::*, *};
use crate::abi::{STATUS_INVALID_ARGUMENT, STATUS_OK};
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::Duration;

fn limits(bytes: u64, chunks: u32) -> Limits {
    Limits {
        max_chunk_bytes: bytes,
        max_queued_bytes: bytes,
        max_record_bytes: u64::MAX,
        max_queued_chunks: chunks,
        max_segments: 8,
        max_metadata_bytes: 1024,
        reserved: 0,
    }
}
fn config(direction: u32, bytes: u64, chunks: u32) -> Description {
    let schema = Schema {
        namespace: 987,
        value: 2,
        version: 1,
    };
    description(direction, schema, schema, limits(bytes, chunks)).unwrap()
}
fn channel(bytes: u64, chunks: u32) -> Channel {
    Channel::new(config(SOURCE, bytes, chunks))
}
fn consume(channel: &Channel) -> i32 {
    // SAFETY: callback neither stores nor dereferences borrowed pointers.
    unsafe { channel.receive(|_| STATUS_OK) }
}

#[test]
fn negotiated_limits_are_independent_of_json_and_intersect_without_raising_policy() {
    let provider = limits(128 * 1024 * 1024, 16);
    let consumer = Limits {
        max_chunk_bytes: 32 * 1024 * 1024,
        max_queued_bytes: 64 * 1024 * 1024,
        max_queued_chunks: 4,
        max_segments: 2,
        max_metadata_bytes: 0,
        ..provider
    };
    assert_eq!(negotiate_limits(provider, consumer).unwrap(), consumer);
    assert!(
        validate_limits(Limits {
            reserved: 1,
            ..provider
        })
        .is_err()
    );
    assert!(
        validate_limits(Limits {
            max_chunk_bytes: u64::MAX,
            ..provider
        })
        .is_err()
    );
    assert!(
        validate_limits(Limits {
            max_segments: MAX_SEGMENTS + 1,
            ..provider
        })
        .is_err()
    );
}

#[test]
fn artwork_and_large_unknown_format_records_move_without_json_or_payload_copy() {
    let channel = channel(40 * 1024 * 1024, 2);
    for (record, length) in [(1, 985_696), (2, 32 * 1024 * 1024 + 17)] {
        let bytes = vec![record as u8; length];
        let original = bytes.as_ptr();
        channel.push(OwnedChunk::with_bytes(record, bytes)).unwrap();
        // SAFETY: inspect only during synchronous delivery, retain no pointers.
        assert_eq!(
            unsafe {
                channel.receive(|chunk| {
                    let segment = &*chunk.segments;
                    assert_eq!(segment.data, original, "owned path must not copy payload");
                    assert_eq!(segment.len, length);
                    assert!(
                        std::slice::from_raw_parts(segment.data, segment.len)
                            .iter()
                            .all(|byte| *byte == record as u8)
                    );
                    STATUS_OK
                })
            },
            STATUS_OK
        );
        assert_eq!(channel.snapshot().queued_bytes, 0);
    }
}

#[test]
fn backpressure_and_sink_rejection_preserve_order_and_return_producer_ownership() {
    let channel = channel(12, 1);
    channel
        .push(OwnedChunk::with_bytes(1, vec![1; 12]))
        .unwrap();
    let error = channel
        .push(OwnedChunk::with_bytes(2, vec![2; 12]))
        .unwrap_err();
    assert_eq!(error.0, BUSY);
    // SAFETY: no pointer escapes callback.
    assert_eq!(unsafe { channel.receive(|_| BUSY) }, BUSY);
    assert_eq!(channel.snapshot().queued_bytes, 12);
    assert_eq!(consume(&channel), STATUS_OK);
    channel.push(error.1).unwrap();
    assert_eq!(consume(&channel), STATUS_OK);
    assert_eq!(
        channel
            .push(OwnedChunk::with_bytes(2, vec![1]))
            .unwrap_err()
            .0,
        STATUS_INVALID_ARGUMENT
    );
}

#[test]
fn fragmented_record_can_exceed_queue_budget_and_rejects_gaps_and_truncation() {
    let channel = channel(4, 1);
    let fragment = |offset| OwnedChunk {
        record_id: 9,
        offset,
        total_bytes: 12,
        segments: vec![vec![7; 4].into_boxed_slice()].into_boxed_slice(),
        metadata: Box::default(),
    };
    channel.push(fragment(0)).unwrap();
    assert_eq!(channel.finish(), Err(STATUS_INVALID_ARGUMENT));
    assert_eq!(consume(&channel), STATUS_OK);
    assert_eq!(
        channel.push(fragment(8)).unwrap_err().0,
        STATUS_INVALID_ARGUMENT
    );
    channel.push(fragment(4)).unwrap();
    assert_eq!(consume(&channel), STATUS_OK);
    channel.push(fragment(8)).unwrap();
    channel.finish().unwrap();
    assert_eq!(channel.snapshot().phase, DRAINING);
    assert_eq!(consume(&channel), STATUS_OK);
    assert_eq!(channel.snapshot().phase, ENDED);
    assert_eq!(consume(&channel), CLOSED);
}

#[test]
fn checked_offsets_metadata_and_segments_cannot_escape_limits() {
    let channel = channel(32, 4);
    let invalid = OwnedChunk {
        record_id: 1,
        offset: u64::MAX - 2,
        total_bytes: u64::MAX,
        segments: vec![vec![0; 8].into_boxed_slice()].into_boxed_slice(),
        metadata: Box::default(),
    };
    assert_eq!(
        channel.push(invalid).unwrap_err().0,
        STATUS_INVALID_ARGUMENT
    );
    let excessive = OwnedChunk {
        record_id: 1,
        offset: 0,
        total_bytes: 8,
        segments: vec![vec![0; 8].into_boxed_slice()].into_boxed_slice(),
        metadata: vec![0; 33].into_boxed_slice(),
    };
    assert_eq!(channel.push(excessive).unwrap_err().0, TOO_LARGE);
    assert_eq!(channel.snapshot().queued_chunks, 0);
}

#[test]
fn waits_observe_changes_before_wait_and_wake_on_cancel_without_ticks() {
    let channel = channel(100, 2);
    let previous = channel.snapshot().generation;
    channel.push(OwnedChunk::with_bytes(1, vec![1])).unwrap();
    assert_ne!(channel.wait(previous, 1000).unwrap().generation, previous);
    let generation = channel.snapshot().generation;
    assert_eq!(channel.wait(generation, 1).unwrap_err(), TIMED_OUT);
    let worker = channel.clone();
    let waiter = std::thread::spawn(move || worker.wait(generation, 30_000).unwrap());
    channel.cancel();
    assert_eq!(waiter.join().unwrap().phase, CANCELLED);
    assert_eq!(
        channel
            .wait(channel.snapshot().generation, 30_000)
            .unwrap()
            .phase,
        CANCELLED
    );
}

#[test]
fn cancellation_waits_for_in_flight_sink_and_counts_its_capacity() {
    let channel = channel(4, 1);
    channel.push(OwnedChunk::with_bytes(1, vec![1; 4])).unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let reader = {
        let channel = channel.clone();
        let entered = entered.clone();
        let release = release.clone();
        std::thread::spawn(move || unsafe {
            channel.receive(|_| {
                entered.wait();
                release.wait();
                STATUS_OK
            })
        })
    };
    entered.wait();
    assert_eq!(channel.snapshot().queued_bytes, 4);
    assert_eq!(channel.snapshot().queued_chunks, 1);
    assert_eq!(
        channel
            .push(OwnedChunk::with_bytes(2, vec![2; 4]))
            .unwrap_err()
            .0,
        BUSY
    );
    let (finished, completion) = mpsc::channel();
    let closer = {
        let channel = channel.clone();
        std::thread::spawn(move || {
            channel.cancel();
            finished.send(()).unwrap();
        })
    };
    assert!(completion.recv_timeout(Duration::from_millis(30)).is_err());
    release.wait();
    reader.join().unwrap();
    closer.join().unwrap();
    completion.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(channel.snapshot().queued_bytes, 0);
    assert_eq!(channel.snapshot().phase, CANCELLED);
}

struct LeaseOwner {
    bytes: Box<[u8]>,
    retains: AtomicUsize,
    releases: AtomicUsize,
}
unsafe extern "C" fn retain(context: *mut std::ffi::c_void) {
    // SAFETY: test keeps the owner alive through cancellation.
    unsafe { &*context.cast::<LeaseOwner>() }
        .retains
        .fetch_add(1, Ordering::SeqCst);
}
unsafe extern "C" fn release(context: *mut std::ffi::c_void) {
    // SAFETY: test keeps the owner alive through cancellation.
    unsafe { &*context.cast::<LeaseOwner>() }
        .releases
        .fetch_add(1, Ordering::SeqCst);
}
#[test]
fn leased_scatter_gather_is_zero_copy_busy_does_not_retain_and_cancel_releases_once() {
    let channel = channel(1024, 1);
    let mut owner = Box::new(LeaseOwner {
        bytes: vec![42; 1024].into_boxed_slice(),
        retains: AtomicUsize::new(0),
        releases: AtomicUsize::new(0),
    });
    let segments = [
        Segment {
            data: owner.bytes.as_ptr(),
            len: 512,
        },
        Segment {
            data: owner.bytes[512..].as_ptr(),
            len: 512,
        },
    ];
    let lease = BufferLease {
        context: (&mut *owner as *mut LeaseOwner).cast(),
        retained_bytes: 1024,
        retain,
        release,
    };
    let mut chunk = Chunk {
        size: size_of::<Chunk>() as u32,
        reserved: 0,
        record_id: 1,
        offset: 0,
        total_bytes: 1024,
        segments: segments.as_ptr(),
        segment_count: 2,
        metadata: std::ptr::null(),
        metadata_len: 0,
        lease: &lease,
    };
    // SAFETY: owner, bytes and callbacks live throughout channel cancellation.
    assert_eq!(unsafe { channel.submit(&chunk) }, STATUS_OK);
    chunk.record_id = 2;
    assert_eq!(unsafe { channel.submit(&chunk) }, BUSY);
    assert_eq!(owner.retains.load(Ordering::SeqCst), 1);
    assert_eq!(
        unsafe {
            channel.receive(|chunk| {
                assert!(chunk.lease.is_null());
                assert_eq!((*chunk.segments).data, owner.bytes.as_ptr());
                assert_eq!((*chunk.segments.add(1)).data, owner.bytes[512..].as_ptr());
                BUSY
            })
        },
        BUSY
    );
    assert_eq!(owner.releases.load(Ordering::SeqCst), 0);
    channel.cancel();
    channel.cancel();
    assert_eq!(owner.releases.load(Ordering::SeqCst), 1);
    assert_eq!(unsafe { channel.submit(&chunk) }, CLOSED);
    assert_eq!(owner.retains.load(Ordering::SeqCst), 1);
}

#[test]
fn registry_enforces_aggregate_budget_principal_binding_and_restart_identity() {
    let hub = Hub::new(2, 100).unwrap();
    let (id, _) = hub.create("alice", config(SOURCE, 100, 2)).unwrap();
    assert_eq!(hub.create("alice", config(SOURCE, 1, 1)).err(), Some(BUSY));
    let binding = hub.new_binding().unwrap();
    assert_eq!(hub.channel("bob", binding, id).err(), Some(DENIED));
    assert!(hub.channel("alice", binding, id).is_ok());
    assert_eq!(hub.channel("alice", binding + 1, id).err(), Some(DENIED));
    let foreign = ChannelId {
        incarnation: id.incarnation ^ 1,
        ..id
    };
    assert_eq!(hub.channel("alice", binding, foreign).err(), Some(CLOSED));
    hub.release_binding(binding);
    assert_eq!(hub.channel("alice", binding, id).err(), Some(CLOSED));
    let (next, _) = hub.create("alice", config(SOURCE, 100, 2)).unwrap();
    assert_ne!(id, next);
}

#[test]
fn duplex_pair_rolls_back_and_retires_both_directions() {
    let hub = Hub::new(2, 8).unwrap();
    assert!(
        DuplexPair::create(&hub, "consumer", config(SOURCE, 4, 1), config(SINK, 8, 1)).is_err()
    );
    let pair =
        DuplexPair::create(&hub, "consumer", config(SOURCE, 4, 1), config(SINK, 4, 1)).unwrap();
    let input = pair.input().clone();
    let output = pair.output().clone();
    input.push(OwnedChunk::with_bytes(1, vec![1; 4])).unwrap();
    assert_eq!(
        input
            .push(OwnedChunk::with_bytes(2, vec![2]))
            .unwrap_err()
            .0,
        BUSY
    );
    output.push(OwnedChunk::with_bytes(1, vec![3])).unwrap();
    input.finish().unwrap();
    assert_eq!(consume(&input), STATUS_OK);
    assert_eq!(consume(&input), CLOSED);
    assert_eq!(consume(&output), STATUS_OK);
    output.push(OwnedChunk::with_bytes(2, vec![4])).unwrap();
    drop(pair);
    assert_eq!(consume(&input), CLOSED);
    assert_eq!(consume(&output), CLOSED);
    assert!(DuplexPair::create(&hub, "consumer", config(SOURCE, 4, 1), config(SINK, 4, 1)).is_ok());
}

#[test]
fn duplex_pair_rejects_wrong_directions() {
    let hub = Hub::new(2, 8).unwrap();
    assert!(
        DuplexPair::create(&hub, "consumer", config(SINK, 4, 1), config(SOURCE, 4, 1)).is_err()
    );
}
