use super::*;
use crate::backend::CaptureSession;
use crate::model::{CaptureResult, Frame};
use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status};
use serde_json::json;
use std::collections::{BTreeSet, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Broker {
    requests: VecDeque<Response>,
    active: BTreeSet<u64>,
    fail_activity_once: BTreeSet<u64>,
    fail_completion_once: BTreeSet<u64>,
    fail_completions: bool,
    completion_attempts: Vec<(u64, Value)>,
    completed: Vec<(u64, Value, usize)>,
    drops: Arc<AtomicUsize>,
    warnings: usize,
}

impl Broker {
    fn request(&mut self, id: u64, payload: Value) {
        self.active.insert(id);
        self.requests.push_back(Response::Request {
            id,
            caller: "video".into(),
            initiator: "video".into(),
            service: SERVICE.into(),
            payload,
            remaining_ms: 30_000,
        });
    }
}

unsafe extern "C" fn broker_runtime(
    context: *mut c_void,
    input: BorrowedBytes,
    sink: ReplySink,
) -> Status {
    // SAFETY: Harness keeps the boxed broker alive until after module destruction.
    let broker = unsafe { &*context.cast::<Mutex<Broker>>() };
    // SAFETY: the runtime provides valid input bytes for this synchronous call.
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let mut broker = broker.lock().unwrap();
    let response = match command {
        Command::Poll {} => broker.requests.pop_front().unwrap_or(Response::Pending),
        Command::CheckRequest { request } if broker.fail_activity_once.remove(&request) => {
            Response::Error {
                code: "temporary_failure".into(),
                message: "retry".into(),
            }
        }
        Command::CheckRequest { request } if broker.active.contains(&request) => Response::Ok,
        Command::CheckRequest { .. } => Response::Error {
            code: "request_ended".into(),
            message: "request cancelled".into(),
        },
        Command::Complete { request, payload }
            if broker.fail_completions || broker.fail_completion_once.remove(&request) =>
        {
            broker.completion_attempts.push((request, payload));
            Response::Error {
                code: "temporary_failure".into(),
                message: "retry".into(),
            }
        }
        Command::Complete { request, payload } if broker.active.remove(&request) => {
            broker.completion_attempts.push((request, payload.clone()));
            let drops = broker.drops.load(Ordering::SeqCst);
            broker.completed.push((request, payload, drops));
            Response::Ok
        }
        Command::Complete { .. } => Response::Error {
            code: "request_ended".into(),
            message: "request cancelled".into(),
        },
        Command::Log { .. } => {
            broker.warnings += 1;
            Response::Ok
        }
        _ => panic!("unexpected capture broker command"),
    };
    // SAFETY: the supplied sink is valid for the duration of this call.
    unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

#[test]
fn transient_close_activity_error_is_logged_and_retried_by_host_tick() {
    let mut harness = Harness::new();
    {
        let mut broker = harness.broker.lock().unwrap();
        broker.request(40, json!({"action":"close", "stream":1}));
        broker.fail_activity_once.insert(40);
    }
    harness.tick_promptly();
    assert_eq!(harness.broker.lock().unwrap().warnings, 1);
    assert!(harness.broker.lock().unwrap().completed.is_empty());
    harness.tick_promptly();
    harness
        .destructor_entered
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(harness.broker.lock().unwrap().completed.is_empty());
    harness.release_destructor.send(()).unwrap();
    harness.wait_for_completion(40);
    let broker = harness.broker.lock().unwrap();
    assert_eq!(broker.completed[0].1, json!({"ok":true}));
    assert_eq!(broker.completed[0].2, 1);
}

#[test]
fn malformed_and_capacity_replies_survive_uncertain_completion() {
    let mut harness = Harness::new();
    let controller = harness.module.controller.clone();
    let (entered, locked) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = controller.lock().unwrap();
        entered.send(()).unwrap();
        let _ = gate.recv_timeout(Duration::from_secs(5));
    });
    locked.recv_timeout(Duration::from_secs(1)).unwrap();
    {
        let mut broker = harness.broker.lock().unwrap();
        for id in 100..(100 + MAX_PENDING_OPERATIONS as u64) {
            broker.active.insert(id);
            harness.module.pending_operations.push(PendingOperation {
                request: id,
                caller: "video".into(),
                operation: Some(Operation::Status),
                reply: None,
            });
        }
        broker.request(50, json!({"action":"unrecognized"}));
        broker.request(51, json!({"action":"status"}));
        broker.fail_completion_once.extend([50, 51]);
    }
    harness.tick_promptly();
    {
        let broker = harness.broker.lock().unwrap();
        assert!(broker.completed.is_empty());
        assert_eq!(broker.completion_attempts.len(), 2);
    }
    harness.tick_promptly();
    {
        let broker = harness.broker.lock().unwrap();
        for (id, expected_code) in [(50, "invalid_request"), (51, "resource_exhausted")] {
            let attempts: Vec<_> = broker
                .completion_attempts
                .iter()
                .filter(|(request, _)| *request == id)
                .collect();
            assert_eq!(attempts.len(), 2);
            assert_eq!(attempts[0].1, attempts[1].1);
            assert_eq!(attempts[1].1["error"]["code"], expected_code);
            assert!(
                broker
                    .completed
                    .iter()
                    .any(|(request, _, _)| *request == id)
            );
        }
    }
    release.send(()).unwrap();
    holder.join().unwrap();
    harness.release_destructor.send(()).unwrap();
}

#[test]
fn cancelled_completed_status_waiters_release_capacity_after_completion_failures() {
    let mut harness = Harness::new();
    {
        let mut broker = harness.broker.lock().unwrap();
        broker.fail_completions = true;
        for id in 100..100 + MAX_PENDING_OPERATIONS as u64 {
            broker.request(id, json!({"action":"status"}));
        }
    }
    for _ in 0..4 {
        harness.tick_promptly();
    }
    assert_eq!(
        harness.module.pending_operations.len(),
        MAX_PENDING_OPERATIONS
    );
    assert!(
        harness
            .module
            .pending_operations
            .iter()
            .all(|pending| pending.reply.is_some())
    );
    {
        let mut broker = harness.broker.lock().unwrap();
        for id in 100..100 + MAX_PENDING_OPERATIONS as u64 {
            broker.active.remove(&id);
        }
    }
    harness.tick_promptly();
    assert!(harness.module.pending_operations.is_empty());
    {
        let mut broker = harness.broker.lock().unwrap();
        broker.fail_completions = false;
        broker.request(200, json!({"action":"close", "stream":1}));
    }
    harness.tick_promptly();
    harness
        .destructor_entered
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(harness.broker.lock().unwrap().completed.is_empty());
    harness.release_destructor.send(()).unwrap();
    harness.wait_for_completion(200);
    let broker = harness.broker.lock().unwrap();
    assert_eq!(broker.completed[0].1, json!({"ok":true}));
    assert_eq!(broker.completed[0].2, 1);
}

#[test]
fn pending_close_reserves_an_absent_id_until_its_last_waiter_retires() {
    let mut harness = Harness::new();
    let candidate = 42;
    let waiter = |request| PendingClose {
        request: Some(request),
        caller: "video".into(),
        delivery_order: request,
        stream: candidate,
        target_operation: None,
        receipt: None,
        reply: None,
        admitted: true,
    };
    harness.module.pending_closes.extend([waiter(1), waiter(2)]);
    harness.module.poll_open_requests().unwrap();
    assert_eq!(
        harness
            .module
            .controller
            .lock()
            .unwrap()
            .validate_stream_id(candidate)
            .unwrap_err()
            .code,
        "entropy_error"
    );
    harness.module.pending_closes.remove(0);
    harness.module.poll_open_requests().unwrap();
    assert_eq!(
        harness
            .module
            .controller
            .lock()
            .unwrap()
            .validate_stream_id(candidate)
            .unwrap_err()
            .code,
        "entropy_error"
    );
    harness.module.pending_closes.clear();
    harness.module.poll_open_requests().unwrap();
    assert_eq!(
        harness
            .module
            .controller
            .lock()
            .unwrap()
            .validate_stream_id(candidate)
            .unwrap(),
        candidate
    );
    harness.release_destructor.send(()).unwrap();
}

struct ActualBroker {
    broker: amaterasu_runtime::Broker,
    blocked_completions: BTreeSet<u64>,
}

unsafe extern "C" fn actual_broker_runtime(
    context: *mut c_void,
    input: BorrowedBytes,
    sink: ReplySink,
) -> Status {
    // SAFETY: the test keeps the boxed broker alive through every module call.
    let state = unsafe { &*context.cast::<Mutex<ActualBroker>>() };
    // SAFETY: input is valid for this synchronous host callback.
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let mut state = state.lock().unwrap();
    let response = match &command {
        Command::Complete { request, .. } if state.blocked_completions.contains(request) => {
            Response::Error {
                code: "temporary_failure".into(),
                message: "completion unavailable".into(),
            }
        }
        _ => state.broker.dispatch("dxgi", command),
    };
    // SAFETY: sink belongs to this in-flight host call.
    unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

#[test]
fn actual_broker_terminal_checks_release_saturated_replies_and_unblock_close() {
    let config = serde_json::from_value(json!({
        "principals": {
            "video": {"call": [SERVICE]},
            "dxgi": {"provide": [SERVICE]}
        },
        "hooks": {},
        "settings": {}
    }))
    .unwrap();
    let mut broker = amaterasu_runtime::Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch(
            "dxgi",
            Command::Register {
                service: SERVICE.into()
            }
        ),
        Response::Ok
    ));
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module =
        crate::controller::shutdown_tests::module_with_session(Box::new(HeldSession {
            entered,
            release: release_rx,
            drops: drops.clone(),
        }));
    let mut state = Box::new(Mutex::new(ActualBroker {
        broker,
        blocked_completions: BTreeSet::new(),
    }));
    // SAFETY: state has stable allocation and outlives the module and callbacks.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::from_mut(&mut *state).cast(),
            invoke: actual_broker_runtime,
        })
    };
    let request = |payload, operation_id, timeout_ms| Command::Request {
        service: SERVICE.into(),
        payload,
        timeout_ms,
        operation_id,
        parent: None,
    };
    let mut ids = Vec::new();
    {
        let mut state = state.lock().unwrap();
        for index in 0..MAX_PENDING_REPLIES {
            let response = state.broker.dispatch(
                "video",
                request(json!({"action":"bad"}), format!("invalid-{index}"), 30_000),
            );
            let Response::Accepted { id } = response else {
                panic!("request rejected: {response:?}")
            };
            state.blocked_completions.insert(id);
            ids.push(id);
        }
    }
    for _ in 0..4 {
        module.tick().unwrap();
    }
    assert_eq!(module.pending_replies.len(), MAX_PENDING_REPLIES);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert!(entered_rx.try_recv().is_err());
    let close_id = {
        let mut state = state.lock().unwrap();
        let response = state.broker.dispatch(
            "video",
            request(
                json!({"action":"close", "stream":1}),
                "close".into(),
                30_000,
            ),
        );
        let Response::Accepted { id: close_id } = response else {
            panic!("close rejected: {response:?}")
        };
        close_id
    };
    module.tick().unwrap();
    assert!(entered_rx.try_recv().is_err());
    assert_eq!(module.pending_replies.len(), MAX_PENDING_REPLIES);
    assert!(matches!(
        state
            .lock()
            .unwrap()
            .broker
            .dispatch("video", Command::TakeResult { request: close_id }),
        Response::Pending
    ));
    {
        let mut state = state.lock().unwrap();
        for id in ids {
            assert!(matches!(
                state
                    .broker
                    .dispatch("video", Command::Cancel { request: id }),
                Response::Ok
            ));
        }
    }
    module.tick().unwrap();
    assert!(module.pending_replies.is_empty());
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        state
            .lock()
            .unwrap()
            .broker
            .dispatch("video", Command::TakeResult { request: close_id }),
        Response::Pending
    ));
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        module.tick().unwrap();
        match state
            .lock()
            .unwrap()
            .broker
            .dispatch("video", Command::TakeResult { request: close_id })
        {
            Response::Result { payload } => {
                assert_eq!(payload, json!({"ok":true}));
                break;
            }
            Response::Pending => (),
            other => panic!("unexpected close result: {other:?}"),
        }
        assert!(Instant::now() < deadline, "close did not retire");
        std::thread::yield_now();
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    // Module must drop before the runtime endpoint's backing state.
    drop(module);
}

#[test]
fn actual_broker_open_replay_precedes_later_close_even_with_uncertain_completion() {
    let config = serde_json::from_value(json!({
        "principals": {
            "video": {"call": [SERVICE]},
            "other": {"call": [SERVICE]},
            "dxgi": {"provide": [SERVICE]}
        },
        "hooks": {}, "settings": {}
    }))
    .unwrap();
    let mut broker = amaterasu_runtime::Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch(
            "dxgi",
            Command::Register {
                service: SERVICE.into()
            }
        ),
        Response::Ok
    ));
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut module =
        crate::controller::shutdown_tests::module_with_session(Box::new(HeldSession {
            entered,
            release: release_rx,
            drops: drops.clone(),
        }));
    let (other_entered, other_entered_rx) = mpsc::channel();
    let (other_release, other_release_rx) = mpsc::channel();
    let other_drops = Arc::new(AtomicUsize::new(0));
    crate::controller::shutdown_tests::add_session(
        &mut module,
        2,
        "other",
        Box::new(HeldSession {
            entered: other_entered,
            release: other_release_rx,
            drops: other_drops.clone(),
        }),
    );
    let mut state = Box::new(Mutex::new(ActualBroker {
        broker,
        blocked_completions: BTreeSet::new(),
    }));
    // SAFETY: state retains a stable broker allocation until after module destruction.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::from_mut(&mut *state).cast(),
            invoke: actual_broker_runtime,
        })
    };
    let request = |payload, operation_id: &str| Command::Request {
        service: SERVICE.into(),
        payload,
        timeout_ms: 30_000,
        operation_id: operation_id.into(),
        parent: None,
    };
    let (open_id, close_id, other_id) = {
        let mut state = state.lock().unwrap();
        let open = state.broker.dispatch(
            "video",
            request(
                json!({"action":"open", "output":"fixture-monitor", "operation_id":"owned-open"}),
                "open-replay",
            ),
        );
        let Response::Accepted { id: open_id } = open else {
            panic!("open rejected: {open:?}")
        };
        state.blocked_completions.insert(open_id);
        let close = state.broker.dispatch(
            "video",
            request(json!({"action":"close", "stream":1}), "close"),
        );
        let Response::Accepted { id: close_id } = close else {
            panic!("close rejected: {close:?}")
        };
        let other = state.broker.dispatch(
            "other",
            request(json!({"action":"close", "stream":2}), "other-close"),
        );
        let Response::Accepted { id: other_id } = other else {
            panic!("independent Close rejected: {other:?}")
        };
        (open_id, close_id, other_id)
    };
    for _ in 0..3 {
        module.tick().unwrap();
    }
    assert!(
        entered_rx.try_recv().is_err(),
        "Close overtook the earlier Open replay"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    other_entered_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    other_release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        module.tick().unwrap();
        match state
            .lock()
            .unwrap()
            .broker
            .dispatch("other", Command::TakeResult { request: other_id })
        {
            Response::Result { payload } => {
                assert_eq!(payload, json!({"ok":true}));
                break;
            }
            Response::Pending => (),
            other => panic!("unexpected independent Close result: {other:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "independent Close did not retire"
        );
        std::thread::yield_now();
    }
    assert_eq!(other_drops.load(Ordering::SeqCst), 1);
    assert!(
        entered_rx.try_recv().is_err(),
        "same-owner Close was not fenced"
    );
    assert!(matches!(
        state
            .lock()
            .unwrap()
            .broker
            .dispatch("video", Command::TakeResult { request: close_id }),
        Response::Pending
    ));
    state.lock().unwrap().blocked_completions.remove(&open_id);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        module.tick().unwrap();
        if entered_rx.try_recv().is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Close did not start after Open settled"
        );
        std::thread::yield_now();
    }
    match state
        .lock()
        .unwrap()
        .broker
        .dispatch("video", Command::TakeResult { request: open_id })
    {
        Response::Result { payload } => assert_eq!(payload["stream"], 1),
        other => panic!("Open replay did not settle first: {other:?}"),
    }
    assert!(matches!(
        state
            .lock()
            .unwrap()
            .broker
            .dispatch("video", Command::TakeResult { request: close_id }),
        Response::Pending
    ));
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        module.tick().unwrap();
        match state
            .lock()
            .unwrap()
            .broker
            .dispatch("video", Command::TakeResult { request: close_id })
        {
            Response::Result { payload } => {
                assert_eq!(payload, json!({"ok":true}));
                break;
            }
            Response::Pending => (),
            other => panic!("unexpected Close result: {other:?}"),
        }
        assert!(Instant::now() < deadline, "Close did not retire");
        std::thread::yield_now();
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    drop(module);
}

#[test]
fn cancelled_completed_close_waiters_release_slots_for_another_session() {
    let config = serde_json::from_value(json!({
        "principals": {
            "video": {"call": [SERVICE]},
            "other": {"call": [SERVICE]},
            "dxgi": {"provide": [SERVICE]}
        },
        "hooks": {}, "settings": {}
    }))
    .unwrap();
    let mut broker = amaterasu_runtime::Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch(
            "dxgi",
            Command::Register {
                service: SERVICE.into()
            }
        ),
        Response::Ok
    ));
    let (first_entered, first_entered_rx) = mpsc::channel();
    let (first_release, first_release_rx) = mpsc::channel();
    let first_drops = Arc::new(AtomicUsize::new(0));
    let mut module =
        crate::controller::shutdown_tests::module_with_session(Box::new(HeldSession {
            entered: first_entered,
            release: first_release_rx,
            drops: first_drops.clone(),
        }));
    let (second_entered, second_entered_rx) = mpsc::channel();
    let (second_release, second_release_rx) = mpsc::channel();
    let second_drops = Arc::new(AtomicUsize::new(0));
    crate::controller::shutdown_tests::add_session(
        &mut module,
        2,
        "other",
        Box::new(HeldSession {
            entered: second_entered,
            release: second_release_rx,
            drops: second_drops.clone(),
        }),
    );
    let mut state = Box::new(Mutex::new(ActualBroker {
        broker,
        blocked_completions: BTreeSet::new(),
    }));
    // SAFETY: state outlives the module and all of its broker calls.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::from_mut(&mut *state).cast(),
            invoke: actual_broker_runtime,
        })
    };
    let mut ids = Vec::new();
    {
        let mut state = state.lock().unwrap();
        for index in 0..MAX_PENDING_CLOSES {
            let response = state.broker.dispatch(
                "video",
                Command::Request {
                    service: SERVICE.into(),
                    payload: json!({"action":"close", "stream":1}),
                    timeout_ms: 30_000,
                    operation_id: format!("close-{index}"),
                    parent: None,
                },
            );
            let Response::Accepted { id } = response else {
                panic!("Close rejected: {response:?}")
            };
            state.blocked_completions.insert(id);
            ids.push(id);
        }
    }
    for _ in 0..4 {
        module.tick().unwrap();
    }
    first_entered_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    first_release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while module.pending_closes.len() != MAX_PENDING_CLOSES
        || module
            .pending_closes
            .iter()
            .any(|close| close.reply.is_none())
    {
        module.tick().unwrap();
        assert!(
            Instant::now() < deadline,
            "completed Close waiters did not accumulate"
        );
        std::thread::yield_now();
    }
    assert_eq!(first_drops.load(Ordering::SeqCst), 1);
    assert_eq!(second_drops.load(Ordering::SeqCst), 0);
    {
        let mut state = state.lock().unwrap();
        for id in ids {
            assert!(matches!(
                state
                    .broker
                    .dispatch("video", Command::Cancel { request: id }),
                Response::Ok
            ));
        }
    }
    module.tick().unwrap();
    assert!(module.pending_closes.is_empty());
    let other_id = {
        let mut state = state.lock().unwrap();
        let response = state.broker.dispatch(
            "other",
            Command::Request {
                service: SERVICE.into(),
                payload: json!({"action":"close", "stream":2}),
                timeout_ms: 30_000,
                operation_id: "other-close".into(),
                parent: None,
            },
        );
        let Response::Accepted { id } = response else {
            panic!("independent Close rejected: {response:?}")
        };
        id
    };
    module.tick().unwrap();
    second_entered_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        state
            .lock()
            .unwrap()
            .broker
            .dispatch("other", Command::TakeResult { request: other_id }),
        Response::Pending
    ));
    second_release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        module.tick().unwrap();
        match state
            .lock()
            .unwrap()
            .broker
            .dispatch("other", Command::TakeResult { request: other_id })
        {
            Response::Result { payload } => {
                assert_eq!(payload, json!({"ok":true}));
                break;
            }
            Response::Pending => (),
            other => panic!("unexpected independent Close result: {other:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "independent Close did not retire"
        );
        std::thread::yield_now();
    }
    assert_eq!(second_drops.load(Ordering::SeqCst), 1);
    drop(module);
}

struct HeldSession {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    drops: Arc<AtomicUsize>,
}

impl CaptureSession for HeldSession {
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        Ok(None)
    }
}

impl Drop for HeldSession {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        let _ = self.release.recv_timeout(Duration::from_secs(5));
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

struct Harness {
    module: CaptureModule,
    broker: Box<Mutex<Broker>>,
    destructor_entered: mpsc::Receiver<()>,
    release_destructor: mpsc::Sender<()>,
}

impl Harness {
    fn new() -> Self {
        let (entered, destructor_entered) = mpsc::channel();
        let (release_destructor, release) = mpsc::channel();
        let drops = Arc::new(AtomicUsize::new(0));
        let mut module =
            crate::controller::shutdown_tests::module_with_session(Box::new(HeldSession {
                entered,
                release,
                drops: drops.clone(),
            }));
        let mut broker = Box::new(Mutex::new(Broker {
            drops,
            ..Broker::default()
        }));
        // SAFETY: the broker's stable allocation outlives module and its runtime calls.
        module.runtime = unsafe {
            RuntimeClient::from_endpoint(HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: std::ptr::from_mut(&mut *broker).cast(),
                invoke: broker_runtime,
            })
        };
        Self {
            module,
            broker,
            destructor_entered,
            release_destructor,
        }
    }

    fn tick_promptly(&mut self) {
        let start = Instant::now();
        self.module.tick().unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "broker tick blocked"
        );
    }

    fn wait_for_completion(&mut self, request: u64) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self
            .broker
            .lock()
            .unwrap()
            .completed
            .iter()
            .any(|(id, _, _)| *id == request)
        {
            self.tick_promptly();
            assert!(Instant::now() < deadline, "request did not complete");
            std::thread::yield_now();
        }
    }
}

#[test]
fn close_waits_for_held_destructor_without_blocking_broker_tick() {
    let mut harness = Harness::new();
    harness
        .broker
        .lock()
        .unwrap()
        .request(10, json!({"action":"close", "stream":1}));
    harness.tick_promptly();
    harness
        .destructor_entered
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    for _ in 0..3 {
        harness.tick_promptly();
        assert!(harness.broker.lock().unwrap().completed.is_empty());
    }
    harness.release_destructor.send(()).unwrap();
    harness.wait_for_completion(10);
    let broker = harness.broker.lock().unwrap();
    assert_eq!(broker.completed.len(), 1);
    assert_eq!(broker.completed[0].1, json!({"ok":true}));
    assert_eq!(
        broker.completed[0].2, 1,
        "Close success must follow destruction"
    );
}

#[test]
fn status_ahead_of_close_does_not_block_tick_when_controller_is_held() {
    let mut harness = Harness::new();
    let controller = harness.module.controller.clone();
    let (entered, locked) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = controller.lock().unwrap();
        entered.send(()).unwrap();
        let _ = gate.recv_timeout(Duration::from_secs(5));
    });
    locked.recv_timeout(Duration::from_secs(1)).unwrap();
    {
        let mut broker = harness.broker.lock().unwrap();
        broker.request(20, json!({"action":"status"}));
        broker.request(21, json!({"action":"close", "stream":1}));
    }
    for _ in 0..3 {
        harness.tick_promptly();
        assert!(harness.broker.lock().unwrap().completed.is_empty());
    }
    release.send(()).unwrap();
    holder.join().unwrap();
    harness.tick_promptly();
    harness
        .destructor_entered
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    harness.wait_for_completion(20);
    assert!(
        !harness
            .broker
            .lock()
            .unwrap()
            .completed
            .iter()
            .any(|(id, _, _)| *id == 21)
    );
    harness.release_destructor.send(()).unwrap();
    harness.wait_for_completion(21);
    let broker = harness.broker.lock().unwrap();
    let close = broker
        .completed
        .iter()
        .find(|(id, _, _)| *id == 21)
        .unwrap();
    assert_eq!(close.1, json!({"ok":true}));
    assert_eq!(close.2, 1);
}

#[test]
fn cancelled_admitted_close_still_retires_its_exact_session() {
    let mut harness = Harness::new();
    let controller = harness.module.controller.clone();
    let (entered, locked) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = controller.lock().unwrap();
        entered.send(()).unwrap();
        let _ = gate.recv_timeout(Duration::from_secs(5));
    });
    locked.recv_timeout(Duration::from_secs(1)).unwrap();
    harness
        .broker
        .lock()
        .unwrap()
        .request(30, json!({"action":"close", "stream":1}));
    harness.tick_promptly();
    assert!(harness.module.pending_closes[0].admitted);
    harness.broker.lock().unwrap().active.remove(&30);
    harness.tick_promptly();
    assert_eq!(harness.module.pending_closes[0].request, None);
    release.send(()).unwrap();
    holder.join().unwrap();
    harness.tick_promptly();
    harness
        .destructor_entered
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(!harness.module.quiesce().unwrap());
    assert!(harness.broker.lock().unwrap().completed.is_empty());
    harness.release_destructor.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !harness.module.quiesce().unwrap() {
        assert!(Instant::now() < deadline, "cancelled close did not retire");
        std::thread::yield_now();
    }
    let broker = harness.broker.lock().unwrap();
    assert_eq!(broker.drops.load(Ordering::SeqCst), 1);
    assert!(broker.completed.is_empty());
}
