use super::{
    abi::*,
    storage::{BorrowedChunk, OwnedChunk, Shape, StoredChunk},
};
use amaterasu_abi::{STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

struct Entry {
    chunk: StoredChunk,
    charge: u64,
}
struct State {
    queue: VecDeque<Entry>,
    bytes: u64,
    generation: u64,
    phase: u32,
    receiving: bool,
    retiring: usize,
    last_record: u64,
    record_total: u64,
    next_offset: u64,
}
struct Shared {
    description: Description,
    state: Mutex<State>,
    changed: Condvar,
}

/// A bounded, ordered channel. Clone is a shared handle, not extra capacity.
/// Owned producer buffers move into the queue; native input can instead retain
/// a producer lease. Queue bytes include in-flight callbacks and pinned capacity.
#[derive(Clone)]
pub struct Channel(Arc<Shared>);
impl Channel {
    pub(super) fn new(description: Description) -> Self {
        Self(Arc::new(Shared {
            description,
            state: Mutex::new(State {
                queue: VecDeque::new(),
                bytes: 0,
                generation: 1,
                phase: ACTIVE,
                receiving: false,
                retiring: 0,
                last_record: 0,
                record_total: 0,
                next_offset: 0,
            }),
            changed: Condvar::new(),
        }))
    }
    pub fn description(&self) -> Description {
        self.0.description
    }
    fn signal(&self, state: &mut State) {
        // Only equality is used by wait; no signed ordering or wall clock.
        state.generation = state.generation.wrapping_add(1);
        self.0.changed.notify_all();
    }
    fn admit(&self, state: &State, shape: Shape) -> Result<(), Status> {
        let limits = self.0.description.limits;
        if state.phase != ACTIVE {
            return Err(CLOSED);
        }
        if shape.charge > limits.max_queued_bytes {
            return Err(TOO_LARGE);
        }
        if state.next_offset != 0 {
            if shape.record_id != state.last_record
                || shape.offset != state.next_offset
                || shape.total != state.record_total
            {
                return Err(STATUS_INVALID_ARGUMENT);
            }
        } else if shape.record_id <= state.last_record || shape.offset != 0 {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if state.queue.len() + usize::from(state.receiving) >= limits.max_queued_chunks as usize
            || shape.charge > limits.max_queued_bytes - state.bytes
        {
            return Err(BUSY);
        }
        Ok(())
    }
    fn enqueue(&self, state: &mut State, shape: Shape, chunk: StoredChunk) {
        state.last_record = shape.record_id;
        state.record_total = shape.total;
        let end = shape.offset + shape.payload;
        state.next_offset = if end == shape.total { 0 } else { end };
        state.bytes += shape.charge;
        state.queue.push_back(Entry {
            chunk,
            charge: shape.charge,
        });
        self.signal(state);
    }
    /// Returns ownership unchanged on rejection. No payload copy on success.
    pub fn push(&self, chunk: OwnedChunk) -> Result<(), (Status, OwnedChunk)> {
        let shape = match chunk.validate(self.0.description.limits) {
            Ok(shape) => shape,
            Err(status) => return Err((status, chunk)),
        };
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(status) = self.admit(&state, shape) {
            return Err((status, chunk));
        }
        self.enqueue(&mut state, shape, StoredChunk::Owned(chunk));
        Ok(())
    }
    /// # Safety
    /// Follow Chunk's complete pointer and optional lease lifetime contracts.
    pub unsafe fn submit(&self, chunk: &Chunk) -> Status {
        // SAFETY: native caller guarantees readable immutable descriptor/storage.
        let borrowed = match unsafe { BorrowedChunk::new(chunk, self.0.description.limits) } {
            Ok(value) => value,
            Err(status) => return status,
        };
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(status) = self.admit(&state, borrowed.shape) {
            return status;
        }
        let shape = borrowed.shape;
        let chunk = borrowed.retain_or_copy();
        self.enqueue(&mut state, shape, chunk);
        STATUS_OK
    }
    /// The descriptor is a synchronous borrow; pointers cannot escape callback.
    /// A rejected sink preserves the same front chunk. Panic is contained and
    /// treated as rejection. Do not synchronously cancel this channel in callback.
    ///
    /// # Safety
    /// Callback must not retain descriptor pointers or let work using them
    /// outlive the call. Cross-service forwarding must finish before returning.
    pub unsafe fn receive(&self, callback: impl FnOnce(&Chunk) -> Status) -> Status {
        let entry = {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.phase == CANCELLED {
                return CLOSED;
            }
            if state.receiving {
                return BUSY;
            }
            let Some(entry) = state.queue.pop_front() else {
                return if state.phase == ENDED { CLOSED } else { EMPTY };
            };
            state.receiving = true;
            entry
        };
        // No queue lock held across the consumer callback.
        let status = catch_unwind(AssertUnwindSafe(|| entry.chunk.with_descriptor(callback)))
            .unwrap_or(STATUS_INTERNAL_ERROR);
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if status != STATUS_OK && state.phase != CANCELLED {
            state.queue.push_front(entry);
        } else {
            let charge = entry.charge;
            drop(state);
            // Lease release outside locks, still covered by receiving barrier.
            drop(entry);
            state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            state.bytes -= charge;
        }
        state.receiving = false;
        if state.phase == DRAINING && state.queue.is_empty() {
            state.phase = ENDED;
        }
        self.signal(&mut state);
        status
    }
    pub fn snapshot(&self) -> Snapshot {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        snapshot(&state)
    }
    /// Generation-based blocking wait for dedicated workers. A mutation between
    /// snapshot and wait is observed immediately. Cancellation wakes all waiters.
    pub fn wait(&self, generation: u64, timeout_ms: u32) -> Result<Snapshot, Status> {
        if timeout_ms > MAX_WAIT_MS {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if timeout_ms == 0 {
            return Ok(snapshot(&state));
        }
        let (state, timeout) = self
            .0
            .changed
            .wait_timeout_while(
                state,
                Duration::from_millis(u64::from(timeout_ms)),
                |state| state.generation == generation && !matches!(state.phase, CANCELLED | ENDED),
            )
            .unwrap_or_else(|e| e.into_inner());
        if timeout.timed_out()
            && state.generation == generation
            && !matches!(state.phase, CANCELLED | ENDED)
        {
            Err(TIMED_OUT)
        } else {
            Ok(snapshot(&state))
        }
    }
    pub fn finish(&self) -> Result<(), Status> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase == CANCELLED {
            return Err(CLOSED);
        }
        if state.next_offset != 0 {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        state.phase = if state.queue.is_empty() && !state.receiving {
            ENDED
        } else {
            DRAINING
        };
        self.signal(&mut state);
        Ok(())
    }
    // Signal cancellation and wake waiters without waiting for callbacks or
    // releasing payloads. Pair owners signal both directions before barriers.
    pub(super) fn begin_cancel(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.phase = CANCELLED;
        self.signal(&mut state);
    }
    /// Synchronous barrier for pending/in-flight payloads and lease destructors.
    /// Producer threads owned by a plugin must additionally be joined by it.
    pub fn cancel(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.phase = CANCELLED;
        let pending = std::mem::take(&mut state.queue);
        let bytes: u64 = pending.iter().map(|entry| entry.charge).sum();
        state.retiring += 1;
        self.signal(&mut state);
        drop(state);
        drop(pending);
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.bytes -= bytes;
        state.retiring -= 1;
        self.signal(&mut state);
        drop(
            self.0
                .changed
                .wait_while(state, |state| state.receiving || state.retiring != 0)
                .unwrap_or_else(|e| e.into_inner()),
        );
    }
}
fn snapshot(state: &State) -> Snapshot {
    Snapshot {
        generation: state.generation,
        queued_bytes: state.bytes,
        queued_chunks: (state.queue.len() + usize::from(state.receiving)) as u32,
        phase: state.phase,
    }
}
