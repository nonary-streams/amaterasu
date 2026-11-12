use super::*;
use crate::backend::CaptureSession;
use crate::model::{CaptureResult, Frame};
use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status};
use serde_json::json;
use std::collections::BTreeSet;
use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

struct GatedSession {
    entered: Sender<()>,
    release: Receiver<()>,
}

impl CaptureSession for GatedSession {
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        Ok(None)
    }
}

impl Drop for GatedSession {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        // A failed assertion must not leave a retirement worker stuck in the test.
        let _ = self.release.recv_timeout(Duration::from_secs(3));
    }
}

struct BrokerState {
    broker: amaterasu_runtime::Broker,
    blocked_completions: BTreeSet<u64>,
}

unsafe extern "C" fn invoke_broker(
    context: *mut c_void,
    input: BorrowedBytes,
    sink: ReplySink,
) -> Status {
    // SAFETY: the boxed state outlives the module and its runtime callbacks.
    let state = unsafe { &*context.cast::<Mutex<BrokerState>>() };
    // SAFETY: the runtime supplies valid input for the duration of this callback.
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
    // SAFETY: the reply sink is valid during this callback.
    unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

struct Harness {
    module: CaptureModule,
    state: Box<Mutex<BrokerState>>,
    entered_a: Receiver<()>,
    release_a: Sender<()>,
    entered_b: Receiver<()>,
    release_b: Sender<()>,
}

impl Harness {
    fn new() -> Self {
        let config = serde_json::from_value(json!({
            "principals": {
                "video": {"call": [SERVICE]},
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
        let (entered_a_tx, entered_a) = mpsc::channel();
        let (release_a, release_a_rx) = mpsc::channel();
        let mut module =
            crate::controller::shutdown_tests::module_with_session(Box::new(GatedSession {
                entered: entered_a_tx,
                release: release_a_rx,
            }));
        let (entered_b_tx, entered_b) = mpsc::channel();
        let (release_b, release_b_rx) = mpsc::channel();
        crate::controller::shutdown_tests::add_session(
            &mut module,
            2,
            "video",
            Box::new(GatedSession {
                entered: entered_b_tx,
                release: release_b_rx,
            }),
        );
        let mut state = Box::new(Mutex::new(BrokerState {
            broker,
            blocked_completions: BTreeSet::new(),
        }));
        // SAFETY: the stable boxed state is dropped after the module.
        module.runtime = unsafe {
            RuntimeClient::from_endpoint(HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: std::ptr::from_mut(&mut *state).cast(),
                invoke: invoke_broker,
            })
        };
        Self {
            module,
            state,
            entered_a,
            release_a,
            entered_b,
            release_b,
        }
    }

    fn request(&self, payload: Value, operation_id: &str) -> u64 {
        let response = self.state.lock().unwrap().broker.dispatch(
            "video",
            Command::Request {
                service: SERVICE.into(),
                payload,
                timeout_ms: 30_000,
                operation_id: operation_id.into(),
                parent: None,
            },
        );
        match response {
            Response::Accepted { id } => id,
            other => panic!("request rejected: {other:?}"),
        }
    }

    fn result(&self, id: u64) -> Response {
        self.state
            .lock()
            .unwrap()
            .broker
            .dispatch("video", Command::TakeResult { request: id })
    }

    fn wait_result(&mut self, id: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            self.module.tick().unwrap();
            match self.result(id) {
                Response::Result { payload } => return payload,
                Response::Pending => (),
                other => panic!("unexpected result: {other:?}"),
            }
            assert!(Instant::now() < deadline, "request {id} did not complete");
            std::thread::yield_now();
        }
    }
}

#[test]
fn held_open_replay_fences_only_its_own_stream_for_the_same_caller() {
    let mut h = Harness::new();
    let open_a = h.request(
        json!({"action":"open", "output":"fixture-monitor", "operation_id":"owned-open"}),
        "replay-a",
    );
    h.state.lock().unwrap().blocked_completions.insert(open_a);
    let close_b = h.request(json!({"action":"close", "stream":2}), "close-b");
    let close_a = h.request(json!({"action":"close", "stream":1}), "close-a");
    for _ in 0..4 {
        h.module.tick().unwrap();
    }
    h.entered_b
        .recv_timeout(Duration::from_millis(500))
        .expect("independent stream B must retire while A's reply is held");
    assert!(
        h.entered_a.try_recv().is_err(),
        "Close A overtook its Open reply"
    );
    assert!(matches!(h.result(close_a), Response::Pending));
    h.release_b.send(()).unwrap();
    assert_eq!(h.wait_result(close_b), json!({"ok":true}));
    h.state.lock().unwrap().blocked_completions.remove(&open_a);
    assert_eq!(h.wait_result(open_a)["stream"], 1);
    let deadline = Instant::now() + Duration::from_secs(1);
    while h.entered_a.try_recv().is_err() {
        h.module.tick().unwrap();
        assert!(Instant::now() < deadline, "Close A never began");
        std::thread::yield_now();
    }
    assert!(matches!(h.result(close_a), Response::Pending));
    h.release_a.send(()).unwrap();
    assert_eq!(h.wait_result(close_a), json!({"ok":true}));
}

#[test]
fn held_close_fences_only_its_own_stream_open_replay_for_the_same_caller() {
    let mut h = Harness::new();
    let close_a = h.request(json!({"action":"close", "stream":1}), "close-a");
    let open_b = h.request(
        json!({"action":"open", "output":"fixture-2", "operation_id":"open-2"}),
        "replay-b",
    );
    let open_a = h.request(
        json!({"action":"open", "output":"fixture-monitor", "operation_id":"owned-open"}),
        "replay-a",
    );
    h.module.tick().unwrap();
    h.entered_a.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        h.wait_result(open_b)["stream"],
        2,
        "independent replay B must complete while A is retiring"
    );
    assert!(
        matches!(h.result(open_a), Response::Pending),
        "replay A must remain fenced by its own Close"
    );
    assert!(matches!(h.result(close_a), Response::Pending));
    assert!(matches!(
        h.state
            .lock()
            .unwrap()
            .broker
            .dispatch("video", Command::Cancel { request: open_a }),
        Response::Ok
    ));
    h.release_a.send(()).unwrap();
    assert_eq!(h.wait_result(close_a), json!({"ok":true}));
    h.release_b.send(()).unwrap();
}
