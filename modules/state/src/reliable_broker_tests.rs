//! Real-broker regression coverage for the exported background State provider.
use super::*;
use amaterasu_contracts::{Grants, HOST_STATE_CONTEXT_SERVICE, RuntimeConfig, RuntimeHostContext};
use amaterasu_module_sdk::{abi::*, service_worker::ServiceWorker};
use amaterasu_runtime::Broker;
use serde_json::json;
use std::{
    ffi::c_void,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

struct Host {
    broker: Mutex<Broker>,
    lose_completion: AtomicBool,
    completions: AtomicUsize,
    block_check: AtomicBool,
    entered: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, sink: ReplySink) -> Status {
    // SAFETY: the fixture retains this immutable endpoint through worker retirement.
    let host = unsafe { &*context.cast::<Host>() };
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    if matches!(command, Command::CheckRequest { .. })
        && host.block_check.swap(false, Ordering::AcqRel)
    {
        host.entered.send(()).unwrap();
        // The broker remains available while the service is deliberately stalled.
        host.release.lock().unwrap().recv().unwrap();
    }
    let completing = matches!(command, Command::Complete { .. });
    if completing {
        host.completions.fetch_add(1, Ordering::AcqRel);
    }
    let response = host.broker.lock().unwrap().dispatch("state", command);
    if completing
        && matches!(response, Response::Ok)
        && host.lose_completion.swap(false, Ordering::AcqRel)
    {
        return STATUS_INTERNAL_ERROR;
    }
    // SAFETY: the SDK copies the response synchronously.
    unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

type Worker = ServiceWorker<StateModule>;
struct ReleaseOnDrop(mpsc::SyncSender<()>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

fn setup(
    block: bool,
    lose: bool,
) -> (Arc<Host>, PathBuf, mpsc::Receiver<()>, mpsc::SyncSender<()>) {
    let root = std::env::temp_dir().join(format!(
        "amaterasu-state-worker-{}",
        amaterasu_storage::random_identifier().unwrap()
    ));
    let config = RuntimeConfig {
        principals: [
            (
                "state".into(),
                Grants {
                    provide: [SERVICE.into()].into(),
                    call: [HOST_STATE_CONTEXT_SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "consumer".into(),
                Grants {
                    call: [SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        host_context: RuntimeHostContext {
            schema: 1,
            state_root: Some(root.to_string_lossy().into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let (entered, arrival) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let host = Arc::new(Host {
        broker: Mutex::new(Broker::new(config).unwrap()),
        lose_completion: AtomicBool::new(lose),
        completions: AtomicUsize::new(0),
        block_check: AtomicBool::new(block),
        entered,
        release: Mutex::new(released),
    });
    (host, root, arrival, release)
}

fn start(host: &Arc<Host>) -> Worker {
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: Arc::as_ptr(host).cast_mut().cast(),
        invoke,
    };
    // SAFETY: tests declare the host before the worker so it outlives the joined worker.
    let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
    Worker::start(runtime).unwrap()
}

fn request(host: &Host) -> u64 {
    match host.broker.lock().unwrap().dispatch(
        "consumer",
        Command::Request {
            service: SERVICE.into(),
            payload: json!({"action":"set","key":"once","value":17,"expected_revision":0}),
            timeout_ms: 10_000,
            operation_id: "once".into(),
            parent: None,
        },
    ) {
        Response::Accepted { id } => id,
        other => panic!("{other:?}"),
    }
}

fn drain(worker: &mut Worker) {
    worker.prepare_shutdown(2000).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !worker.quiesce().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn background_state_retries_lost_completion_without_repeating_the_durable_mutation() {
    let (host, root, _, release) = setup(false, true);
    let mut worker = start(&host);
    let _release = ReleaseOnDrop(release);
    let id = request(&host);
    let deadline = Instant::now() + Duration::from_secs(3);
    let result = loop {
        worker.tick().unwrap();
        let result = host
            .broker
            .lock()
            .unwrap()
            .dispatch("consumer", Command::TakeResult { request: id });
        if let Response::Result { payload } = result {
            break payload;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    };
    assert_eq!(result["revision"], 1);
    drain(&mut worker);
    assert_eq!(host.completions.load(Ordering::Acquire), 2);
    // Quiescence must release the actual lifetime lock before claiming ready.
    let store = StateStore::open(root.clone()).unwrap();
    assert_eq!(
        store.handle("consumer", json!({"action":"get","key":"once"}))["value"],
        17
    );
    assert_eq!(
        store.handle("consumer", json!({"action":"snapshot"}))["revision"],
        1
    );
    drop(store);
    drop(worker);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancellation_while_state_worker_is_stalled_still_prevents_the_write() {
    let (host, root, entered, release) = setup(true, false);
    let mut worker = start(&host);
    let release = ReleaseOnDrop(release);
    let id = request(&host);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        worker.tick().unwrap();
        if entered.try_recv().is_ok() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(matches!(
        host.broker
            .lock()
            .unwrap()
            .dispatch("consumer", Command::Cancel { request: id }),
        Response::Ok
    ));
    drop(release);
    drain(&mut worker);
    let store = StateStore::open(root.clone()).unwrap();
    assert_eq!(
        store.handle("consumer", json!({"action":"get","key":"once"}))["found"],
        false
    );
    assert_eq!(host.completions.load(Ordering::Acquire), 0);
    drop(store);
    drop(worker);
    std::fs::remove_dir_all(root).unwrap();
}
