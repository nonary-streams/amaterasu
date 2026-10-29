use super::*;
use crate::abi::*;
use amaterasu_media::*;
use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Provider {
    api: CaptureApiV1_7,
    frames: CaptureFrames<usize>,
}

impl Provider {
    fn new() -> (CapturePublisher<usize>, Box<Self>) {
        let (publisher, frames) = capture_channel();
        let mut provider = Box::new(Self {
            api: CaptureApiV1_7 {
                base: CaptureApiV1_6 {
                    base: NativeInterface {
                        header: AbiHeader {
                            size: CAPTURE_API_V1_7_SIZE,
                            major: INTERFACE_MAJOR,
                            minor: CAPTURE_EVENTS_INTERFACE_MINOR,
                        },
                        context: std::ptr::null_mut(),
                        release: None,
                    },
                    read_frame: cpu,
                    geometry,
                    read_frame_with_freshness: fresh,
                    read_frame_with_capture_timestamp: timed,
                    read_dmabuf_frame: gpu,
                    latest_presentation: presentation,
                    read_dmabuf_frame_leased: leased,
                },
                subscribe_events: subscribe,
                unsubscribe_events: unsubscribe,
                resolve_swap: resolve,
            },
            frames,
        });
        provider.api.base.base.context = (&mut *provider as *mut Self).cast();
        (publisher, provider)
    }
}

unsafe extern "C" fn cpu(_: *mut c_void, _: u64, _: FrameSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn geometry(_: *mut c_void, _: u64, _: *mut CaptureGeometry) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn fresh(_: *mut c_void, _: u64, _: FreshFrameSinkV1_2) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn timed(_: *mut c_void, _: u64, _: FreshFrameSinkV1_3) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn gpu(_: *mut c_void, _: u64, _: DmaBufFrameSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn presentation(_: *mut c_void, _: u64, _: *mut PresentationStatus) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn leased(_: *mut c_void, _: u64, _: DmaBufLeaseSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn subscribe(
    context: *mut c_void,
    stream: u64,
    sink: CaptureEventSink,
) -> Status {
    if stream != 7 {
        return MEDIA_CLOSED;
    }
    // SAFETY: the test keeps the boxed provider and consumer alive through unsubscribe.
    unsafe { (&*context.cast::<Provider>()).frames.subscribe_events(sink) }
}
unsafe extern "C" fn unsubscribe(context: *mut c_void, stream: u64) {
    if stream == 7 {
        // SAFETY: the boxed provider remains live until after the subscription drops.
        unsafe { &*context.cast::<Provider>() }
            .frames
            .unsubscribe_events();
    }
}
unsafe extern "C" fn resolve(
    context: *mut c_void,
    stream: u64,
    id: u64,
    decision: u32,
    reason: BorrowedBytes,
) -> Status {
    if stream != 7 {
        return MEDIA_CLOSED;
    }
    // SAFETY: the test lends initialized context/reason bytes for this call.
    unsafe {
        (&*context.cast::<Provider>())
            .frames
            .resolve_native_swap(id, decision, reason)
    }
}

fn request() -> SwapRequest {
    SwapRequest {
        preferred_provider: Some("other_capture".into()),
        output: Some("other-output".into()),
        reason: "source_unavailable".into(),
    }
}

fn publish<T: Send + Sync>(publisher: &CapturePublisher<T>, frame: T) {
    assert!(publisher.publish(frame).is_ok());
}

#[test]
fn capture_thread_publishes_and_wakes_without_encoding_or_pixel_copy() {
    let (publisher, provider) = Provider::new();
    // SAFETY: the boxed provider outlives the subscription and obeys the event ABI.
    let subscription = unsafe { CaptureSubscription::from_api(&provider.api, 7) }.unwrap();
    let notifications = subscription.notifications();
    let worker = std::thread::spawn(move || publish(&publisher, 42));
    let notice = notifications.wait_timeout(Duration::from_secs(2));
    worker.join().unwrap();
    assert!(notice.frame_ready);
    assert_eq!(
        provider.frames.latest_frame().unwrap().as_deref(),
        Some(&42)
    );
    assert!(!notifications.take().frame_ready);
}

#[test]
fn registration_announces_existing_frame_and_outstanding_swap() {
    let (publisher, provider) = Provider::new();
    publish(&publisher, 1);
    let id = publisher.request_capture_swap(request()).unwrap();
    // SAFETY: fixture provider implements the full v1.7 lifetime contract.
    let subscription = unsafe { CaptureSubscription::from_api(&provider.api, 7) }.unwrap();
    let notice = subscription.notifications().take();
    assert!(notice.frame_ready);
    assert_eq!(
        notice.swap_request,
        Some(PendingSwap {
            id,
            request: request()
        })
    );
    subscription
        .resolve_swap(
            id,
            SwapDecision::Rejected {
                reason: "not_allowed".into(),
            },
        )
        .unwrap();
    assert_eq!(
        publisher.swap_decision(id).unwrap(),
        Some(SwapDecision::Rejected {
            reason: "not_allowed".into()
        })
    );
    publish(&publisher, 2);
    assert_eq!(provider.frames.latest_frame().unwrap().as_deref(), Some(&2));
    assert!(subscription.notifications().take().swap_request.is_none());
}

#[test]
fn latest_frame_replaces_pending_storage_but_keeps_acquired_pixels_alive() {
    struct Pixels(Arc<AtomicUsize>);
    impl Drop for Pixels {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let (publisher, frames) = capture_channel();
    publish(&publisher, Pixels(drops.clone()));
    let acquired = frames.latest_frame().unwrap().unwrap();
    for _ in 0..100 {
        publish(&publisher, Pixels(drops.clone()));
    }
    assert_eq!(drops.load(Ordering::SeqCst), 99);
    frames.close();
    assert_eq!(drops.load(Ordering::SeqCst), 100);
    drop(acquired);
    assert_eq!(drops.load(Ordering::SeqCst), 101);
    let rejected = publisher.publish(Pixels(drops.clone())).err().unwrap();
    assert_eq!(rejected.error, CaptureError::Closed);
    assert_eq!(drops.load(Ordering::SeqCst), 101);
    drop(rejected);
    assert_eq!(drops.load(Ordering::SeqCst), 102);
}

#[test]
fn swaps_are_bounded_exactly_identified_and_advisory() {
    let (publisher, provider) = Provider::new();
    // SAFETY: test keeps the complete provider alive through unsubscribe.
    let subscription = unsafe { CaptureSubscription::from_api(&provider.api, 7) }.unwrap();
    let first = publisher.request_capture_swap(request()).unwrap();
    assert_eq!(
        publisher.request_capture_swap(request()),
        Err(CaptureError::Busy)
    );
    assert_eq!(
        subscription.resolve_swap(first + 1, SwapDecision::Accepted),
        Err(CaptureError::InvalidRequest)
    );
    assert_eq!(publisher.swap_decision(first).unwrap(), None);
    subscription
        .resolve_swap(first, SwapDecision::Accepted)
        .unwrap();
    assert_eq!(
        publisher.swap_decision(first).unwrap(),
        Some(SwapDecision::Accepted)
    );
    assert_eq!(
        subscription.resolve_swap(first, SwapDecision::Accepted),
        Err(CaptureError::InvalidRequest)
    );
    publish(&publisher, 3); // Acceptance itself must not stop or swap capture.
    let second = publisher.request_capture_swap(request()).unwrap();
    assert!(second > first);
    assert_eq!(publisher.swap_decision(first).unwrap(), None);
    assert_eq!(
        subscription.resolve_swap(first, SwapDecision::Accepted),
        Err(CaptureError::InvalidRequest)
    );
}

#[test]
fn dropping_subscription_detaches_callbacks_and_wakes_waiters_closed() {
    let (publisher, provider) = Provider::new();
    // SAFETY: test provider remains live and its unsubscribe is a lifetime barrier.
    let subscription = unsafe { CaptureSubscription::from_api(&provider.api, 7) }.unwrap();
    let notices = subscription.notifications();
    let waiter = std::thread::spawn(move || notices.wait_timeout(Duration::from_secs(2)));
    drop(subscription);
    assert!(waiter.join().unwrap().closed);
    for frame in 0..100 {
        publish(&publisher, frame);
    }
    // SAFETY: independently owned new callback context is retained until drop.
    let new_subscription = unsafe { CaptureSubscription::from_api(&provider.api, 7) }.unwrap();
    assert!(new_subscription.notifications().take().frame_ready);
    provider.frames.close();
    let stopped = new_subscription
        .notifications()
        .wait_timeout(Duration::from_secs(2));
    assert!(stopped.closed);
    assert_eq!(stopped.stop_reason.as_deref(), Some("capture_closed"));
    assert_eq!(
        publisher.request_capture_swap(request()),
        Err(CaptureError::Closed)
    );
    drop(new_subscription); // unsubscribe remains valid after control close.
}

#[test]
fn malformed_swap_requests_and_unknown_capture_streams_are_rejected() {
    let (publisher, provider) = Provider::new();
    for invalid in [
        SwapRequest {
            reason: "".into(),
            ..request()
        },
        SwapRequest {
            reason: "x".repeat(65),
            ..request()
        },
        SwapRequest {
            preferred_provider: Some("service/1".into()),
            ..request()
        },
        SwapRequest {
            output: Some("bad\noutput".into()),
            ..request()
        },
        SwapRequest {
            output: Some("x".repeat(513)),
            ..request()
        },
    ] {
        assert_eq!(
            publisher.request_capture_swap(invalid),
            Err(CaptureError::InvalidRequest)
        );
    }
    // SAFETY: test provider handles unknown stream IDs without keeping the sink.
    assert!(matches!(
        unsafe { CaptureSubscription::from_api(&provider.api, 8) },
        Err(CaptureError::Closed)
    ));
}

#[test]
fn optional_capture_event_tail_checks_size_major_and_minor() {
    let (_, mut provider) = Provider::new();
    for (size, major, minor) in [(88, 1, 7), (112, 2, 7), (112, 1, 6)] {
        provider.api.base.base.header = AbiHeader { size, major, minor };
        // SAFETY: initialized table metadata is deliberately incompatible.
        assert!(matches!(
            unsafe { CaptureSubscription::from_api(&provider.api, 7) },
            Err(CaptureError::Unsupported)
        ));
    }
}

#[test]
fn unsubscribe_waits_for_an_admitted_callback_and_busy_publish_returns_the_frame() {
    use std::sync::{Mutex, mpsc};
    struct Gate {
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    unsafe extern "C" fn ready(context: *mut c_void) {
        // SAFETY: the test gate is retained until unsubscribe and worker join.
        let gate = unsafe { &*context.cast::<Gate>() };
        let _ = gate.entered.send(());
        let _ = gate.release.lock().unwrap().recv();
    }
    unsafe extern "C" fn ignore_swap(_: *mut c_void, _: *const CaptureSwapRequest) -> Status {
        STATUS_OK
    }
    unsafe extern "C" fn ignore_stop(_: *mut c_void, _: BorrowedBytes) {}
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let gate = Box::new(Gate {
        entered: entered_tx,
        release: Mutex::new(release_rx),
    });
    let (publisher, frames) = capture_channel();
    let frames = Arc::new(frames);
    let sink = CaptureEventSink {
        context: (&*gate as *const Gate).cast_mut().cast(),
        frame_ready: ready,
        request_swap: ignore_swap,
        stopped: ignore_stop,
    };
    // SAFETY: gate and callbacks remain valid until the explicit barrier below.
    assert_eq!(unsafe { frames.subscribe_events(sink) }, STATUS_OK);
    let producer = publisher.clone();
    let publishing = std::thread::spawn(move || publish(&producer, 1));
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let rejected = publisher.publish(2).unwrap_err();
    assert_eq!(rejected.error, CaptureError::Busy);
    assert_eq!(rejected.frame, 2);
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let source = frames.clone();
    let unsubscribing = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        source.unsubscribe_events();
        done_tx.send(()).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    release_tx.send(()).unwrap();
    done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    publishing.join().unwrap();
    unsubscribing.join().unwrap();
    drop(gate);
    publish(&publisher, 3); // No callback may use the freed gate.
}

#[test]
fn published_cpu_frame_uses_its_original_allocation_for_native_delivery() {
    use crate::media::OwnedCpuFrame;
    let (publisher, frames) = capture_channel();
    let frame = OwnedCpuFrame::new(2, 2, PIXEL_BGRA8, 8, vec![91; 16], 123, 5).unwrap();
    let original = frame.pixels().as_ptr();
    publish(&publisher, frame);
    unsafe extern "C" fn receive(context: *mut c_void, frame: *const CpuFrame) -> Status {
        // SAFETY: test supplies an output slot and provider borrows live pixels.
        let (seen, frame) = unsafe { (&mut *context.cast::<(*const u8, u64, u64)>(), &*frame) };
        *seen = (frame.data, frame.timestamp_us, frame.sequence);
        // SAFETY: this test's owned frame retains all initialized pixels.
        assert_eq!(unsafe { frame.bytes() }.unwrap(), &[91; 16]);
        STATUS_OK
    }
    let mut seen = (std::ptr::null::<u8>(), 0_u64, 0_u64);
    let sink = FrameSink {
        context: (&mut seen as *mut (*const u8, u64, u64)).cast(),
        write: receive,
    };
    // SAFETY: test callback/output slot remain live through synchronous delivery.
    assert_eq!(unsafe { frames.read_cpu_frame(sink) }, STATUS_OK);
    assert_eq!(seen, (original, 123, 5));
    assert!(frames.notifications().take().frame_ready);
    frames.close();
    // SAFETY: same live test sink; a closed reader must not invoke it.
    assert_eq!(unsafe { frames.read_cpu_frame(sink) }, MEDIA_CLOSED);
}

#[test]
fn native_request_busy_is_retried_and_malformed_requests_never_enter_the_receiver() {
    let (publisher, provider) = Provider::new();
    let notes = CaptureNotifications::new();
    // SAFETY: notes is retained until frames.unsubscribe_events below.
    let sink = unsafe { notes.native_sink() };
    let reason = BorrowedBytes::new(b"retry");
    let empty = BorrowedBytes::new(b"");
    let mut request = CaptureSwapRequest {
        id: 7,
        preferred_provider: empty,
        output: empty,
        reason,
    };
    // SAFETY: every descriptor borrows valid test-owned initialized storage.
    assert_eq!(
        unsafe { (sink.request_swap)(sink.context, &request) },
        STATUS_OK
    );
    // SAFETY: capture source and notes obey the subscription lifetime contract.
    assert_eq!(unsafe { provider.frames.subscribe_events(sink) }, STATUS_OK);
    let id = publisher
        .request_capture_swap(super::tests::request())
        .unwrap();
    assert_eq!(notes.take().swap_request.unwrap().id, 7); // New request met Busy.
    notes.clear_swap(7);
    publisher.retry_notifications().unwrap();
    assert_eq!(notes.take().swap_request.unwrap().id, id);
    notes.clear_swap(id);
    request.id = 0;
    // SAFETY: initialized test request with intentionally invalid identity.
    assert_eq!(
        unsafe { (sink.request_swap)(sink.context, &request) },
        STATUS_INVALID_ARGUMENT
    );
    request.id = 9;
    request.reason = BorrowedBytes {
        ptr: std::ptr::null(),
        len: 65,
    };
    // SAFETY: size is deliberately over limit and must be rejected before dereference.
    assert_eq!(
        unsafe { (sink.request_swap)(sink.context, &request) },
        STATUS_INVALID_ARGUMENT
    );
    assert!(notes.take().swap_request.is_none());
    provider.frames.unsubscribe_events();
}

#[test]
fn terminal_source_loss_wakes_native_and_local_consumers_without_becoming_a_swap() {
    let (publisher, provider) = Provider::new();
    let local = provider.frames.notifications();
    // SAFETY: provider allocation remains live through the subscriber barrier.
    let subscription = unsafe { CaptureSubscription::from_api(&provider.api, 7) }.unwrap();
    publisher.stop("source_lost").unwrap();
    for notice in [
        local.take(),
        subscription
            .notifications()
            .wait_timeout(Duration::from_secs(2)),
    ] {
        assert!(notice.closed);
        assert_eq!(notice.stop_reason.as_deref(), Some("source_lost"));
        assert!(notice.swap_request.is_none());
    }
    assert_eq!(
        publisher.publish(9).unwrap_err().error,
        CaptureError::Closed
    );
    assert_eq!(
        publisher.request_capture_swap(request()),
        Err(CaptureError::Closed)
    );
}
