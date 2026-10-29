use super::{CaptureError, PendingSwap, SwapDecision, SwapRequest};
use crate::{NativeBinding, abi::*};
use amaterasu_media::*;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

#[derive(Default)]
struct NoticeState {
    frame_ready: bool,
    swap_request: Option<PendingSwap>,
    closed: bool,
    stop_reason: Option<String>,
}

#[derive(Default)]
struct Signals {
    state: Mutex<NoticeState>,
    ready: Condvar,
}

/// Lightweight, thread-safe notification reader. Waiting does not acquire a
/// frame or call a provider. A frame-ready flag is coalesced and consumed by
/// take/wait; a swap remains visible until explicitly resolved.
#[derive(Clone)]
pub struct CaptureNotifications {
    signals: Arc<Signals>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CaptureNotice {
    pub frame_ready: bool,
    pub swap_request: Option<PendingSwap>,
    pub closed: bool,
    pub stop_reason: Option<String>,
}

impl CaptureNotifications {
    pub fn new() -> Self {
        Self {
            signals: Arc::new(Signals::default()),
        }
    }

    /// # Safety
    /// Retain this reader until the provider has synchronously unsubscribed.
    /// The source must honor CaptureEventSink's lifetime and threading rules.
    pub unsafe fn native_sink(&self) -> CaptureEventSink {
        CaptureEventSink {
            context: Arc::as_ptr(&self.signals).cast_mut().cast(),
            frame_ready,
            request_swap,
            stopped,
        }
    }

    pub(super) fn notify_frame(&self) {
        // SAFETY: our own stable callback context is retained by this reader.
        unsafe { frame_ready(Arc::as_ptr(&self.signals).cast_mut().cast()) };
    }

    pub(super) fn notify_swap(&self, pending: &PendingSwap) {
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
        // SAFETY: all values are our owned storage, borrowed synchronously.
        unsafe { request_swap(Arc::as_ptr(&self.signals).cast_mut().cast(), &request) };
    }

    pub(super) fn notify_stopped(&self, reason: &str) {
        // SAFETY: callback/context and borrowed reason are owned by this reader.
        unsafe {
            stopped(
                Arc::as_ptr(&self.signals).cast_mut().cast(),
                BorrowedBytes::new(reason.as_bytes()),
            )
        };
    }

    pub fn take(&self) -> CaptureNotice {
        let mut state = self
            .signals
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        take_notice(&mut state)
    }

    /// Predicate and wait use the same lock, including notifications arriving
    /// before this call, so no edge-triggered wake can be lost.
    pub fn wait_timeout(&self, timeout: Duration) -> CaptureNotice {
        let state = self
            .signals
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (mut state, _) = self
            .signals
            .ready
            .wait_timeout_while(state, timeout, |state| {
                !state.frame_ready && state.swap_request.is_none() && !state.closed
            })
            .unwrap_or_else(|error| error.into_inner());
        take_notice(&mut state)
    }

    /// Called only after the source has settled the exact outstanding request.
    pub fn clear_swap(&self, id: u64) {
        let mut state = self
            .signals
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state
            .swap_request
            .as_ref()
            .is_some_and(|request| request.id == id)
        {
            state.swap_request = None;
        }
    }

    /// Marks a detached notification reader closed and wakes its waiters.
    /// The source must be unsubscribed before retiring its last reader.
    pub fn close(&self) {
        let mut state = self
            .signals
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        state.frame_ready = false;
        state.swap_request = None;
        self.signals.ready.notify_all();
    }
}

impl Default for CaptureNotifications {
    fn default() -> Self {
        Self::new()
    }
}

fn take_notice(state: &mut NoticeState) -> CaptureNotice {
    CaptureNotice {
        frame_ready: std::mem::take(&mut state.frame_ready),
        swap_request: state.swap_request.clone(),
        closed: state.closed,
        stop_reason: state.stop_reason.clone(),
    }
}

/// Owns the consumer callback storage, pinned through unsubscribe. Native frame
/// acquisition and swap decisions stay on the binding's owning module thread;
/// only the notification reader can be sent to another thread.
pub struct CaptureSubscription<'provider> {
    api: &'provider CaptureApiV1_7,
    stream: u64,
    signals: Arc<Signals>,
    // Borrowing NativeBinding prevents releasing/unloading a provider first.
    _binding: std::marker::PhantomData<&'provider NativeBinding>,
}

impl<'provider> CaptureSubscription<'provider> {
    /// # Safety
    /// The binding must be a trusted capture provider honoring v1.7, including
    /// ownership, threading, and the unsubscribe lifetime barrier. The stream
    /// must have been opened by this binding's authenticated principal.
    pub unsafe fn subscribe(
        binding: &'provider NativeBinding,
        stream: u64,
    ) -> Result<Self, CaptureError> {
        // SAFETY: caller identifies a capture service; read only its legacy prefix.
        let prefix = unsafe { binding.table::<CaptureApiPrefix>() }
            .map_err(|_| CaptureError::Unsupported)?;
        if !capture_events_available(prefix.base.header) {
            return Err(CaptureError::Unsupported);
        }
        // SAFETY: advertised size/version cover the entire additive table.
        let api =
            unsafe { binding.table::<CaptureApiV1_7>() }.map_err(|_| CaptureError::Unsupported)?;
        // SAFETY: caller's provider and binding remain alive for this borrow.
        unsafe { Self::from_api(api, stream) }
    }

    /// # Safety
    /// The table, context, callbacks and source module must remain live for
    /// 'provider. It must implement the complete capture v1.7 contract. This
    /// low-level entry point is for adapters already owning a provider binding.
    pub unsafe fn from_api(
        api: &'provider CaptureApiV1_7,
        stream: u64,
    ) -> Result<Self, CaptureError> {
        if !capture_events_available(api.base.base.header) {
            return Err(CaptureError::Unsupported);
        }
        let signals = Arc::new(Signals::default());
        let sink = CaptureEventSink {
            context: Arc::as_ptr(&signals).cast_mut().cast(),
            frame_ready,
            request_swap,
            stopped,
        };
        // SAFETY: the allocation is stable and retained until unsubscribe. Our
        // callbacks only copy bounded metadata and wake the notification reader.
        status(unsafe { (api.subscribe_events)(api.base.base.context, stream, sink) })?;
        Ok(Self {
            api,
            stream,
            signals,
            _binding: std::marker::PhantomData,
        })
    }

    pub fn notifications(&self) -> CaptureNotifications {
        CaptureNotifications {
            signals: self.signals.clone(),
        }
    }

    pub fn resolve_swap(&self, id: u64, decision: SwapDecision) -> Result<(), CaptureError> {
        {
            let state = self
                .signals
                .state
                .lock()
                .map_err(|_| CaptureError::Closed)?;
            if state.closed {
                return Err(CaptureError::Closed);
            }
            if state
                .swap_request
                .as_ref()
                .is_none_or(|request| request.id != id)
            {
                return Err(CaptureError::InvalidRequest);
            }
        }
        let (result, reason) = match &decision {
            SwapDecision::Accepted => (CAPTURE_SWAP_ACCEPTED, ""),
            SwapDecision::Rejected { reason } if super::valid_code(reason) => {
                (CAPTURE_SWAP_REJECTED, reason.as_str())
            }
            _ => return Err(CaptureError::InvalidRequest),
        };
        // SAFETY: table/stream and reason bytes remain live through this call.
        // No receiver lock is held while calling the provider.
        status(unsafe {
            (self.api.resolve_swap)(
                self.api.base.base.context,
                self.stream,
                id,
                result,
                BorrowedBytes::new(reason.as_bytes()),
            )
        })?;
        let mut state = self
            .signals
            .state
            .lock()
            .map_err(|_| CaptureError::Closed)?;
        if state
            .swap_request
            .as_ref()
            .is_some_and(|request| request.id == id)
        {
            state.swap_request = None;
        }
        Ok(())
    }
}

impl Drop for CaptureSubscription<'_> {
    fn drop(&mut self) {
        // SAFETY: this lifetime barrier ends callbacks while their storage and
        // the provider binding remain alive. A closed stream must also permit it.
        unsafe { (self.api.unsubscribe_events)(self.api.base.base.context, self.stream) };
        let mut state = self
            .signals
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        state.frame_ready = false;
        state.swap_request = None;
        self.signals.ready.notify_all();
    }
}

unsafe extern "C" fn frame_ready(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() {
            return;
        }
        // SAFETY: subscriber owns this allocation until unsubscribe completes.
        let signals = unsafe { &*context.cast::<Signals>() };
        if let Ok(mut state) = signals.state.lock()
            && !state.closed
        {
            state.frame_ready = true;
            signals.ready.notify_one();
        }
    }));
}

unsafe extern "C" fn stopped(context: *mut c_void, reason: BorrowedBytes) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() || reason.len > 64 {
            return;
        }
        // SAFETY: provider retains initialized reason/context through callback.
        let (signals, bytes) = unsafe { (&*context.cast::<Signals>(), reason.read()) };
        let Ok(bytes) = bytes else {
            return;
        };
        let Ok(reason) = std::str::from_utf8(bytes) else {
            return;
        };
        if !super::valid_code(reason) {
            return;
        }
        if let Ok(mut state) = signals.state.lock() {
            state.closed = true;
            state.stop_reason = Some(reason.into());
            state.frame_ready = false;
            state.swap_request = None;
            signals.ready.notify_all();
        }
    }));
}

unsafe extern "C" fn request_swap(
    context: *mut c_void,
    request: *const CaptureSwapRequest,
) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null()
            || request.is_null()
            || !(request as usize).is_multiple_of(align_of::<CaptureSwapRequest>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: source borrows the initialized request and callback context.
        let (signals, request) = unsafe { (&*context.cast::<Signals>(), &*request) };
        if request.id == 0 {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: source keeps the bounded UTF-8 byte allocations live until return.
        let parsed = unsafe { parse_request(request) };
        let Ok(parsed) = parsed else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Ok(mut state) = signals.state.lock() else {
            return MEDIA_CLOSED;
        };
        if state.closed {
            return MEDIA_CLOSED;
        }
        if let Some(pending) = &state.swap_request {
            return if pending == &parsed {
                STATUS_OK
            } else {
                MEDIA_BUSY
            };
        }
        state.swap_request = Some(parsed);
        signals.ready.notify_one();
        STATUS_OK
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}

unsafe fn parse_request(request: &CaptureSwapRequest) -> Result<PendingSwap, CaptureError> {
    unsafe fn text(bytes: BorrowedBytes, limit: usize) -> Result<String, CaptureError> {
        if bytes.len > limit {
            return Err(CaptureError::InvalidRequest);
        }
        // SAFETY: caller supplies valid borrowed allocations; size is checked first.
        let bytes = unsafe { bytes.read() }.map_err(|_| CaptureError::InvalidRequest)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| CaptureError::InvalidRequest)
    }
    // SAFETY: the provider supplies all three borrowed strings for this callback.
    let (provider, output, reason) = unsafe {
        (
            text(request.preferred_provider, 64)?,
            text(request.output, 512)?,
            text(request.reason, 64)?,
        )
    };
    let request_value = SwapRequest {
        preferred_provider: (!provider.is_empty()).then_some(provider),
        output: (!output.is_empty()).then_some(output),
        reason,
    };
    request_value.validate()?;
    Ok(PendingSwap {
        id: request.id,
        request: request_value,
    })
}

fn status(status: Status) -> Result<(), CaptureError> {
    match status {
        STATUS_OK => Ok(()),
        MEDIA_BUSY => Err(CaptureError::Busy),
        MEDIA_CLOSED => Err(CaptureError::Closed),
        MEDIA_UNSUPPORTED => Err(CaptureError::Unsupported),
        STATUS_INVALID_ARGUMENT => Err(CaptureError::InvalidRequest),
        _ => Err(CaptureError::ProviderFailed),
    }
}
