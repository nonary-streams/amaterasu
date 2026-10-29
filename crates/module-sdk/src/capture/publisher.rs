use super::{
    CaptureError, CaptureNotifications, PendingSwap, SwapDecision, SwapRequest, valid_code,
};
use crate::abi::{BorrowedBytes, STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use amaterasu_media::{
    CAPTURE_SWAP_ACCEPTED, CAPTURE_SWAP_REJECTED, CaptureEventSink, CaptureSwapRequest, MEDIA_BUSY,
    MEDIA_CLOSED,
};
use std::sync::{Arc, Mutex, TryLockError};

struct State<T> {
    frame: Option<Arc<T>>,
    closed: bool,
    sink: Option<CaptureEventSink>,
    next_swap: u64,
    pending_swap: Option<PendingSwap>,
    swap_announced: bool,
    decision: Option<(u64, SwapDecision)>,
    notifications: CaptureNotifications,
}

// SAFETY: a sink may only be installed through unsafe subscribe_events, whose
// contract requires concurrent, bounded notification callbacks and a context
// kept live until unsubscribe/close. The mutex serializes notifications and is
// their lifetime barrier. No GPU descriptor or provider binding crosses here.
unsafe impl<T: Send + Sync> Send for State<T> {}

/// Producer handle, normally moved to a capture worker. `T` owns the captured
/// pixels/resource; publishing moves that ownership without copying pixels.
/// Use an owned CPU frame or your own immutable GPU-resource owner. Existing
/// borrowed DMA-BUF descriptors/leases are deliberately not made Send/Sync.
pub struct CapturePublisher<T> {
    state: Arc<Mutex<State<T>>>,
}

/// Module-thread reader and native subscription adapter for one capture session.
/// Native provider callbacks must still authorize their bound caller/stream ID
/// before accessing this object. Dropping it closes all producer handles.
pub struct CaptureFrames<T> {
    state: Arc<Mutex<State<T>>>,
}

/// A rejected publication returns its original resource to the producer.
pub struct PublishError<T> {
    pub error: CaptureError,
    pub frame: T,
}

impl<T> std::fmt::Debug for PublishError<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublishError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl<T> std::fmt::Display for PublishError<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl<T> std::error::Error for PublishError<T> {}

pub fn capture_channel<T: Send + Sync>() -> (CapturePublisher<T>, CaptureFrames<T>) {
    let state = Arc::new(Mutex::new(State {
        frame: None,
        closed: false,
        sink: None,
        next_swap: 0,
        pending_swap: None,
        swap_announced: false,
        decision: None,
        notifications: CaptureNotifications::new(),
    }));
    (
        CapturePublisher {
            state: state.clone(),
        },
        CaptureFrames { state },
    )
}

impl<T> Clone for CapturePublisher<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

impl<T: Send + Sync> CapturePublisher<T> {
    /// Reports terminal source loss and stops publishing. Unlike a swap request,
    /// this tells the consumer that capture cannot continue. It retires only the
    /// mailbox's own frame reference, not frames already acquired by consumers.
    pub fn stop(&self, reason: &str) -> Result<(), CaptureError> {
        if !valid_code(reason) {
            return Err(CaptureError::InvalidRequest);
        }
        let mut state = self.state.try_lock().map_err(|_| CaptureError::Busy)?;
        if state.closed {
            return Err(CaptureError::Closed);
        }
        state.closed = true;
        state.pending_swap = None;
        state.notifications.notify_stopped(reason);
        if let Some(sink) = state.sink.take() {
            // SAFETY: the registration contract retains the consumer context;
            // this guard prevents concurrent unsubscribe through callback exit.
            unsafe { (sink.stopped)(sink.context, BorrowedBytes::new(reason.as_bytes())) };
        }
        let previous = state.frame.take();
        drop(state);
        drop(previous);
        Ok(())
    }

    /// Replaces the latest frame, then sends a lightweight ready notification.
    /// A contended mailbox returns Busy without accepting the frame. Capture
    /// must publish frames in source order. Displaced resource destructors run
    /// outside the mailbox lock, on the publishing thread. Resource destructors
    /// must not wait for GPU work if the producer requires a nonblocking publish.
    pub fn publish(&self, frame: T) -> Result<(), PublishError<T>> {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(error) => {
                return Err(PublishError {
                    error: match error {
                        TryLockError::WouldBlock => CaptureError::Busy,
                        TryLockError::Poisoned(_) => CaptureError::Closed,
                    },
                    frame,
                });
            }
        };
        if state.closed {
            return Err(PublishError {
                error: CaptureError::Closed,
                frame,
            });
        }
        let previous = state.frame.replace(Arc::new(frame));
        state.notifications.notify_frame();
        if let Some(sink) = state.sink {
            // SAFETY: subscribe establishes the sink lifetime/threading contract;
            // holding this guard prevents unsubscribe from retiring its context.
            unsafe { (sink.frame_ready)(sink.context) };
        }
        announce_swap(&mut state);
        drop(state);
        drop(previous);
        Ok(())
    }

    /// Advisory only: does not stop capture or select/open another provider.
    /// One request may be outstanding; the coordinator decides whether to act.
    pub fn request_capture_swap(&self, request: SwapRequest) -> Result<u64, CaptureError> {
        request.validate()?;
        let mut state = self.state.try_lock().map_err(|_| CaptureError::Busy)?;
        if state.closed {
            return Err(CaptureError::Closed);
        }
        if state.pending_swap.is_some() {
            return Err(CaptureError::Busy);
        }
        let id = state.next_swap.checked_add(1).ok_or(CaptureError::Closed)?;
        state.next_swap = id;
        state.pending_swap = Some(PendingSwap { id, request });
        state
            .notifications
            .notify_swap(state.pending_swap.as_ref().expect("new request"));
        state.swap_announced = false;
        state.decision = None;
        announce_swap(&mut state);
        Ok(id)
    }

    /// Retries a notification rejected with Busy without capturing another frame.
    pub fn retry_notifications(&self) -> Result<(), CaptureError> {
        let mut state = self.state.try_lock().map_err(|_| CaptureError::Busy)?;
        if state.closed {
            return Err(CaptureError::Closed);
        }
        announce_swap(&mut state);
        Ok(())
    }

    /// Reads the decision for the most recently settled request. A new request
    /// retires this record; closed capture reports Closed even after acceptance.
    pub fn swap_decision(&self, id: u64) -> Result<Option<SwapDecision>, CaptureError> {
        let state = self.state.lock().map_err(|_| CaptureError::Closed)?;
        if state.closed {
            return Err(CaptureError::Closed);
        }
        Ok(state
            .decision
            .as_ref()
            .filter(|(request, _)| *request == id)
            .map(|(_, result)| result.clone()))
    }
}

impl<T> CaptureFrames<T> {
    /// Local consumer path: wait for frame-ready/swap notifications without
    /// native callback wiring. Plugins use the v1.7 adapters across their ABI.
    pub fn notifications(&self) -> CaptureNotifications {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .notifications
            .clone()
    }

    /// Snapshot the latest owned frame; no lock remains held during native
    /// delivery/encoding. The mailbox retains one latest frame for repeats;
    /// consumers must bound any additional in-flight references themselves.
    pub fn latest_frame(&self) -> Result<Option<Arc<T>>, CaptureError> {
        let state = self.state.lock().map_err(|_| CaptureError::Closed)?;
        if state.closed {
            return Err(CaptureError::Closed);
        }
        Ok(state.frame.clone())
    }

    /// Installs one native consumer. An already available frame/request is
    /// announced during registration, so startup cannot lose a ready event.
    ///
    /// # Safety
    /// Sink callbacks/context must obey CaptureEventSink's thread-safe,
    /// non-reentrant notification contract and remain live until unsubscribe
    /// or close returns. They cannot call this channel (including close).
    pub unsafe fn subscribe_events(&self, sink: CaptureEventSink) -> Status {
        if sink.context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let Ok(mut state) = self.state.lock() else {
            return MEDIA_CLOSED;
        };
        if state.closed {
            return MEDIA_CLOSED;
        }
        if state.sink.is_some() {
            return MEDIA_BUSY;
        }
        state.sink = Some(sink);
        state.swap_announced = false;
        if state.frame.is_some() {
            // SAFETY: caller establishes the callback contract for this subscription.
            unsafe { (sink.frame_ready)(sink.context) };
        }
        announce_swap(&mut state);
        STATUS_OK
    }

    /// Synchronous lifetime barrier, also valid after close. The guard waits for
    /// an admitted notification and prevents new callbacks before clearing it.
    pub fn unsubscribe_events(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.sink = None;
        state.swap_announced = false;
    }

    pub fn resolve_swap(&self, id: u64, decision: SwapDecision) -> Result<(), CaptureError> {
        if matches!(&decision, SwapDecision::Rejected { reason } if !valid_code(reason)) {
            return Err(CaptureError::InvalidRequest);
        }
        let mut state = self.state.lock().map_err(|_| CaptureError::Closed)?;
        if state.closed {
            return Err(CaptureError::Closed);
        }
        if state
            .pending_swap
            .as_ref()
            .is_none_or(|request| request.id != id)
        {
            return Err(CaptureError::InvalidRequest);
        }
        state.pending_swap = None;
        state.notifications.clear_swap(id);
        state.decision = Some((id, decision));
        Ok(())
    }

    /// Adapter for CaptureApiV1_7.resolve_swap after authenticating stream ownership.
    ///
    /// # Safety
    /// Reason bytes must remain readable for this call, as required by the ABI.
    pub unsafe fn resolve_native_swap(
        &self,
        id: u64,
        decision: u32,
        reason: BorrowedBytes,
    ) -> Status {
        if reason.len > 64 {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller guarantees the bounded reason allocation for this call.
        let Ok(bytes) = (unsafe { reason.read() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let decision = match decision {
            CAPTURE_SWAP_ACCEPTED if bytes.is_empty() => SwapDecision::Accepted,
            CAPTURE_SWAP_REJECTED => match std::str::from_utf8(bytes) {
                Ok(reason) if valid_code(reason) => SwapDecision::Rejected {
                    reason: reason.into(),
                },
                _ => return STATUS_INVALID_ARGUMENT,
            },
            _ => return STATUS_INVALID_ARGUMENT,
        };
        match self.resolve_swap(id, decision) {
            Ok(()) => STATUS_OK,
            Err(CaptureError::Closed) => MEDIA_CLOSED,
            Err(_) => STATUS_INVALID_ARGUMENT,
        }
    }

    /// Stops publication/notifications. Existing acquired frames remain owned
    /// by their readers; this does not assert that GPU reads have completed.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        if let Some(sink) = state.sink.take() {
            // SAFETY: the subscription context remains live through this final
            // notification; the guard is the unsubscribe/close lifetime barrier.
            unsafe { (sink.stopped)(sink.context, BorrowedBytes::new(b"capture_closed")) };
        }
        state.pending_swap = None;
        state.notifications.close();
        let frame = state.frame.take();
        drop(state);
        drop(frame);
    }
}

impl<T> Drop for CaptureFrames<T> {
    fn drop(&mut self) {
        self.close();
    }
}

impl CaptureFrames<crate::media::OwnedCpuFrame> {
    /// Native CPU-frame adapter. Authorize the caller/stream before invoking it.
    /// It borrows the published allocation without an intermediate pixel copy.
    ///
    /// # Safety
    /// Sink/context must obey FrameSink's synchronous callback contract and
    /// remain live until this call returns.
    pub unsafe fn read_cpu_frame(&self, sink: amaterasu_media::FrameSink) -> Status {
        let frame = match self.latest_frame() {
            Ok(Some(frame)) => frame,
            Ok(None) => return amaterasu_media::MEDIA_PENDING,
            Err(_) => return MEDIA_CLOSED,
        };
        // SAFETY: the acquired owner keeps immutable pixels alive through send.
        unsafe { sink.send(frame.borrow().descriptor()) }
    }
}

fn announce_swap<T>(state: &mut State<T>) {
    let (Some(sink), Some(pending)) = (state.sink, &state.pending_swap) else {
        return;
    };
    if state.swap_announced {
        return;
    }
    let request = CaptureSwapRequest {
        id: pending.id,
        preferred_provider: BorrowedBytes::new(
            pending
                .request
                .preferred_provider
                .as_deref()
                .unwrap_or("")
                .as_bytes(),
        ),
        output: BorrowedBytes::new(pending.request.output.as_deref().unwrap_or("").as_bytes()),
        reason: BorrowedBytes::new(pending.request.reason.as_bytes()),
    };
    // SAFETY: the registration contract keeps this consumer callback live. All
    // strings and the request are borrowed only until the bounded call returns.
    state.swap_announced = unsafe { (sink.request_swap)(sink.context, &request) } == STATUS_OK;
}
