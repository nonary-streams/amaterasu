//! Bounded, owned encoder output for provider implementations.
//!
//! The queue does not schedule an encoder or relax its thread-affinity rules.
//! A consumer reads the front packet, calls its sink, and acknowledges only on
//! success. Rejection leaves the same immutable packet at the front for retry.
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub mod cpu_pool_writer;
pub mod pool;
mod provider;
pub use provider::*;

/// Submitted sessions receive frames through the separately negotiated resource
/// consumer. Source sessions produce output without frame submissions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionMode {
    Submitted,
    Source,
}

/// Identifies a negotiated codec, framing, or schema; interpretation belongs to
/// the provider and consumer that negotiated it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FormatId {
    pub namespace_id: u64,
    pub value: u64,
    pub schema: u64,
}

/// Output bytes and metadata remain immutable for the lifetime of this object.
#[derive(Debug, Eq, PartialEq)]
pub struct EncodedPacket {
    pub flags: u32,
    pub session_id: u64,
    pub access_unit_id: u64,
    pub submission_id: u64,
    pub source_id: u64,
    pub config_generation: u64,
    pub timestamp_us: u64,
    pub codec: FormatId,
    pub framing: FormatId,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueError {
    Full,
    TooLarge,
    Closed,
    InvalidPacket,
    NotFront,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Active,
    Draining,
    Ended,
    Cancelled,
}

struct State {
    phase: Phase,
    packets: VecDeque<Arc<EncodedPacket>>,
    bytes: usize,
    generation: u64,
    sink: Option<EventRegistration>,
}

struct EventRegistration {
    session: u64,
    sink: amaterasu_media::EncoderEventSink,
}

// SAFETY: installation requires a context valid and thread-safe until the
// synchronous unsubscribe barrier. The state mutex serializes all callbacks.
unsafe impl Send for EventRegistration {}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    max_packets: usize,
    max_bytes: usize,
    mode: SessionMode,
}

/// Thread-safe queue. Limits bound queued storage, while consumers retaining
/// packet handles after acknowledgement are responsible for that extra memory.
#[derive(Clone)]
pub struct OutputQueue(Arc<Shared>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueSnapshot {
    pub generation: u64,
    pub output_ready: bool,
    pub draining: bool,
    pub ended: bool,
    pub cancelled: bool,
}

impl OutputQueue {
    pub fn new(
        mode: SessionMode,
        max_packets: usize,
        max_bytes: usize,
    ) -> Result<Self, QueueError> {
        if max_packets == 0 || max_bytes == 0 {
            return Err(QueueError::InvalidPacket);
        }
        Ok(Self(Arc::new(Shared {
            state: Mutex::new(State {
                phase: Phase::Active,
                packets: VecDeque::new(),
                bytes: 0,
                generation: 0,
                sink: None,
            }),
            changed: Condvar::new(),
            max_packets,
            max_bytes,
            mode,
        })))
    }

    pub fn mode(&self) -> SessionMode {
        self.0.mode
    }

    /// On failure returns the original packet so the producer can retry it.
    /// During drain, only output from already accepted work may be pushed.
    pub fn push(&self, packet: EncodedPacket) -> Result<(), (QueueError, Box<EncodedPacket>)> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let error = if state.phase == Phase::Cancelled || state.phase == Phase::Ended {
            Some(QueueError::Closed)
        } else if packet.flags & amaterasu_media::ENCODER_CHUNK_END_OF_STREAM != 0
            || (self.0.mode == SessionMode::Source && packet.submission_id != 0)
        {
            Some(QueueError::InvalidPacket)
        } else if packet.bytes.len() > self.0.max_bytes
            || packet.bytes.len() > amaterasu_media::ENCODER_PROVIDER_MAX_OUTPUT_BYTES
        {
            Some(QueueError::TooLarge)
        } else if state.packets.len() == self.0.max_packets
            || packet.bytes.len() > self.0.max_bytes - state.bytes
        {
            Some(QueueError::Full)
        } else {
            None
        };
        if let Some(error) = error {
            return Err((error, Box::new(packet)));
        }
        state.bytes += packet.bytes.len();
        state.packets.push_back(Arc::new(packet));
        self.signal(&mut state);
        Ok(())
    }

    /// Marks drain in progress. The provider must stop input admission itself
    /// and finish all accepted work before EOS.
    pub fn drain(&self) -> Result<(), QueueError> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        match state.phase {
            Phase::Active => {
                state.phase = Phase::Draining;
                self.signal(&mut state);
                Ok(())
            }
            Phase::Draining | Phase::Ended => Ok(()),
            Phase::Cancelled => Err(QueueError::Closed),
        }
    }

    /// Enqueues EOS after accepted work completes; a full queue may be retried.
    pub fn finish(&self, eos: EncodedPacket) -> Result<(), (QueueError, Box<EncodedPacket>)> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let error = if state.phase != Phase::Draining {
            Some(QueueError::Closed)
        } else if eos.flags != amaterasu_media::ENCODER_CHUNK_END_OF_STREAM
            || !eos.bytes.is_empty()
            || eos.submission_id != 0
        {
            Some(QueueError::InvalidPacket)
        } else if state.packets.len() == self.0.max_packets {
            Some(QueueError::Full)
        } else {
            None
        };
        if let Some(error) = error {
            return Err((error, Box::new(eos)));
        }
        state.packets.push_back(Arc::new(eos));
        state.phase = Phase::Ended;
        self.signal(&mut state);
        Ok(())
    }

    /// Returns a stable front handle. A rejected sink simply retains the queue
    /// entry; acknowledge only after successful delivery. Serialize receives per
    /// session as required by the provider ABI. The provider must split output
    /// into chunks that fit the receiver's negotiated `max_bytes` before enqueueing.
    pub fn front(&self) -> Option<Arc<EncodedPacket>> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .packets
            .front()
            .cloned()
    }

    pub fn acknowledge(&self, packet: &Arc<EncodedPacket>) -> Result<(), QueueError> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state
            .packets
            .front()
            .is_some_and(|front| Arc::ptr_eq(front, packet))
        {
            return Err(QueueError::NotFront);
        }
        let removed = state.packets.pop_front().expect("front checked");
        state.bytes -= removed.bytes.len();
        self.signal(&mut state);
        Ok(())
    }

    /// Cancels queued output and wakes waiters. It does not cancel input resource
    /// leases or join provider work; those barriers belong to the provider.
    pub fn cancel(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.phase = Phase::Cancelled;
        state.packets.clear();
        state.bytes = 0;
        self.signal(&mut state);
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        snapshot(&state)
    }

    /// Waits only if the observed generation still matches, so notifications
    /// preceding the wait cannot be lost. Wakeups can be coalesced or spurious.
    pub fn wait_for_change(&self, observed: u64, timeout: Duration) -> QueueSnapshot {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, _) = self
            .0
            .changed
            .wait_timeout_while(state, timeout, |state| {
                state.generation == observed && state.phase != Phase::Cancelled
            })
            .unwrap_or_else(|e| e.into_inner());
        snapshot(&state)
    }

    /// Installs a native notification sink, announcing existing output or stop.
    ///
    /// # Safety
    /// The context must remain live and thread-safe until unsubscribe returns.
    /// The callback must not reenter this queue or unsubscribe, and must not unwind.
    pub unsafe fn subscribe_native(
        &self,
        session: u64,
        sink: amaterasu_media::EncoderEventSink,
    ) -> crate::abi::Status {
        use crate::abi::{STATUS_INVALID_ARGUMENT, STATUS_OK};
        use amaterasu_media::{ENCODER_EVENT_OUTPUT_READY, ENCODER_EVENT_STOPPED, MEDIA_BUSY};
        if session == 0 {
            return STATUS_INVALID_ARGUMENT;
        }
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.sink.is_some() {
            return MEDIA_BUSY;
        }
        state.sink = Some(EventRegistration { session, sink });
        let event =
            if matches!(state.phase, Phase::Cancelled | Phase::Ended) && state.packets.is_empty() {
                Some(ENCODER_EVENT_STOPPED)
            } else if !state.packets.is_empty() {
                Some(ENCODER_EVENT_OUTPUT_READY)
            } else {
                None
            };
        if let Some(event) = event {
            // SAFETY: caller guarantees sink lifetime and callback contract.
            unsafe { (sink.notify)(sink.context, session, event) };
        }
        STATUS_OK
    }

    /// Prevents new notifications and waits for any admitted callback to return.
    /// Must not be called from the notification callback.
    pub fn unsubscribe_native(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.sink = None;
    }

    /// Delivers at most one immutable packet. Sink rejection retains it verbatim.
    /// The caller must serialize receive and destruction for this session.
    ///
    /// # Safety
    /// The sink must be valid for the synchronous call, must not reenter this
    /// queue, and must not unwind; its borrowed descriptor expires on return.
    pub unsafe fn receive_native(
        &self,
        session: u64,
        max_bytes: usize,
        sink: amaterasu_media::EncoderOutputSink,
    ) -> crate::abi::Status {
        use crate::abi::{BorrowedBytes, STATUS_INVALID_ARGUMENT, STATUS_OK};
        use amaterasu_media::{
            ENCODER_PROVIDER_MAX_OUTPUT_BYTES, EncoderFormatId, EncoderOutputChunk, MEDIA_BUSY,
            MEDIA_CLOSED, MEDIA_PENDING,
        };
        if session == 0 || max_bytes == 0 || max_bytes > ENCODER_PROVIDER_MAX_OUTPUT_BYTES {
            return STATUS_INVALID_ARGUMENT;
        }
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(packet) = state.packets.front() else {
            return if matches!(state.phase, Phase::Cancelled | Phase::Ended) {
                MEDIA_CLOSED
            } else {
                MEDIA_PENDING
            };
        };
        if packet.session_id != session {
            return STATUS_INVALID_ARGUMENT;
        }
        if packet.bytes.len() > max_bytes {
            return MEDIA_BUSY;
        }
        let format = |id: FormatId| EncoderFormatId {
            namespace_id: id.namespace_id,
            value: id.value,
            schema: id.schema,
        };
        let chunk = EncoderOutputChunk {
            size: std::mem::size_of::<EncoderOutputChunk>() as u32,
            flags: packet.flags,
            session_id: packet.session_id,
            access_unit_id: packet.access_unit_id,
            submission_id: packet.submission_id,
            source_id: packet.source_id,
            config_generation: packet.config_generation,
            timestamp_us: packet.timestamp_us,
            codec: format(packet.codec),
            framing: format(packet.framing),
            bytes: BorrowedBytes::new(&packet.bytes),
        };
        // SAFETY: the packet and descriptor are retained through the synchronous call.
        let status = unsafe { (sink.write)(sink.context, &chunk) };
        if status == STATUS_OK {
            let packet = state.packets.pop_front().expect("front checked");
            state.bytes -= packet.bytes.len();
            self.signal(&mut state);
        }
        status
    }

    fn signal(&self, state: &mut State) {
        state.generation = state.generation.wrapping_add(1);
        self.0.changed.notify_all();
        if let Some(registration) = &state.sink {
            let event = if matches!(state.phase, Phase::Cancelled | Phase::Ended)
                && state.packets.is_empty()
            {
                amaterasu_media::ENCODER_EVENT_STOPPED
            } else if !state.packets.is_empty() {
                amaterasu_media::ENCODER_EVENT_OUTPUT_READY
            } else {
                amaterasu_media::ENCODER_EVENT_STATE_CHANGED
            };
            // SAFETY: subscribe_native requires context validity through unsubscribe;
            // holding the mutex is the callback lifetime barrier.
            unsafe {
                (registration.sink.notify)(registration.sink.context, registration.session, event)
            };
        }
    }
}

fn snapshot(state: &State) -> QueueSnapshot {
    QueueSnapshot {
        generation: state.generation,
        output_ready: !state.packets.is_empty(),
        draining: matches!(state.phase, Phase::Draining | Phase::Ended),
        ended: state.phase == Phase::Ended,
        cancelled: state.phase == Phase::Cancelled,
    }
}
