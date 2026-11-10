use super::*;
use crate::{Command, Response, abi::*};
use serde_json::{Value, json};
use std::{
    ffi::c_void,
    sync::{Mutex, atomic::AtomicUsize},
    time::Duration,
};

struct Fixture {
    gate: Mutex<Option<Receiver<()>>>,
    entered: SyncSender<()>,
    request: AtomicBool,
    cancelled: AtomicBool,
    fail: u8,
    ticks: AtomicUsize,
    effects: AtomicUsize,
    prepared: AtomicBool,
    prepared_budget_ms: AtomicU64,
    dropped: AtomicUsize,
    notifications: AtomicUsize,
    completed: Mutex<Vec<Value>>,
}

fn fixture(block: bool, fail: u8) -> (Arc<Fixture>, Receiver<()>, SyncSender<()>) {
    let (entered, arrival) = mpsc::sync_channel(1);
    let (release, gate) = mpsc::sync_channel(1);
    (
        Arc::new(Fixture {
            gate: Mutex::new(block.then_some(gate)),
            entered,
            fail,
            request: AtomicBool::new(true),
            cancelled: AtomicBool::new(false),
            ticks: AtomicUsize::new(0),
            effects: AtomicUsize::new(0),
            prepared: AtomicBool::new(false),
            prepared_budget_ms: AtomicU64::new(u64::MAX),
            dropped: AtomicUsize::new(0),
            notifications: AtomicUsize::new(0),
            completed: Mutex::new(Vec::new()),
        }),
        arrival,
        release,
    )
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> Status {
    // SAFETY: each test retains the Arc until the adapter joins its worker;
    // shared observations are atomic or protected by their own short mutex.
    let state = unsafe { &*context.cast::<Fixture>() };
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let response = match command {
        Command::Register { service } => {
            assert_eq!(service, "example.worker/1");
            Response::Ok
        }
        Command::Poll {} if state.request.swap(false, Ordering::AcqRel) => Response::Request {
            id: 1,
            caller: "consumer".into(),
            initiator: "consumer".into(),
            service: "example.worker/1".into(),
            payload: json!({"action":"read"}),
            remaining_ms: 1000,
        },
        Command::Poll {} => Response::Pending,
        Command::CheckRequest { .. } if state.cancelled.load(Ordering::Acquire) => {
            Response::error("request_ended", "cancelled")
        }
        Command::CheckRequest { .. } => Response::Ok,
        Command::Complete { payload, .. } => {
            state.completed.lock().unwrap().push(payload);
            Response::Ok
        }
        Command::NotifyWorkReady {} => {
            state.notifications.fetch_add(1, Ordering::AcqRel);
            Response::Ok
        }
        Command::Log { .. } => Response::Ok,
        _ => return STATUS_INTERNAL_ERROR,
    };
    let bytes = serde_json::to_vec(&response).unwrap();
    // SAFETY: the SDK copies the reply while this allocation remains live.
    unsafe { reply.send(&bytes) };
    STATUS_OK
}

fn runtime(state: &Arc<Fixture>) -> RuntimeClient {
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: Arc::as_ptr(state).cast_mut().cast(),
        invoke,
    };
    // SAFETY: callers retain state until the adapter and all callbacks retire.
    unsafe { RuntimeClient::from_endpoint(endpoint) }
}

struct Probe<const VALUE: u64> {
    runtime: RuntimeClient,
}
impl<const VALUE: u64> ManagedModule for Probe<VALUE> {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: "example.worker/1".into(),
        })?;
        Ok(Self { runtime })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        let state = self.state();
        state.ticks.fetch_add(1, Ordering::AcqRel);
        let gate = state.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            state.entered.send(()).unwrap();
            gate.recv().unwrap();
        }
        match state.fail {
            1 => return Err("injected work failure".into()),
            2 => panic!("injected worker panic"),
            _ => {}
        }
        if let Response::Request { id, .. } = self.runtime.send(Command::Poll {})?
            && matches!(
                self.runtime.send(Command::CheckRequest { request: id })?,
                Response::Ok
            )
        {
            state.effects.fetch_add(1, Ordering::AcqRel);
            self.runtime.require_success(Command::Complete {
                request: id,
                payload: json!({"value":VALUE}),
            })?;
        }
        Ok(())
    }
    fn prepare_shutdown(&mut self, remaining_ms: u64) -> ModuleResult<()> {
        self.state()
            .prepared_budget_ms
            .store(remaining_ms, Ordering::Release);
        self.state().prepared.store(true, Ordering::Release);
        Ok(())
    }
    fn quiesce(&mut self) -> ModuleResult<bool> {
        assert!(self.state().prepared.load(Ordering::Acquire));
        if self.state().fail == 3 && !self.state().cancelled.load(Ordering::Acquire) {
            return Err("injected retirement failure".into());
        }
        Ok(true)
    }
}
impl<const VALUE: u64> Probe<VALUE> {
    fn state(&self) -> &Fixture {
        // SAFETY: each fixture outlives its module and joined worker.
        unsafe { &*self.runtime.endpoint.context.cast::<Fixture>() }
    }
}
impl<const VALUE: u64> Drop for Probe<VALUE> {
    fn drop(&mut self) {
        self.state().dropped.fetch_add(1, Ordering::AcqRel);
    }
}

fn drain<M: ManagedModule + 'static>(worker: &mut ServiceWorker<M>) {
    worker.prepare_shutdown(1000).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if worker.quiesce().unwrap() {
            return;
        }
        assert!(Instant::now() < deadline, "worker failed to retire");
        thread::yield_now();
    }
}

#[test]
fn blocked_service_does_not_block_host_ticks_or_retire_before_its_work() {
    let (state, entered, release) = fixture(true, 0);
    let worker = ServiceWorker::<Probe<1>>::start(runtime(&state)).unwrap();
    let (returned, results) = mpsc::sync_channel(1);
    let (continue_host, started_work) = mpsc::sync_channel(1);
    let lifetime = state.clone();
    let host = thread::spawn(move || {
        let _lifetime = lifetime;
        let mut worker = worker;
        worker.tick().unwrap();
        started_work.recv().unwrap();
        for _ in 0..1000 {
            worker.tick().unwrap();
        }
        worker.prepare_shutdown(1000).unwrap();
        let ready = worker.quiesce().unwrap();
        assert!(returned.send((worker, ready)).is_ok());
    });
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    continue_host.send(()).unwrap();
    let result = results.recv_timeout(Duration::from_secs(1));
    // Always release the blocked service, including a failed nonblocking check.
    state.cancelled.store(true, Ordering::Release);
    release.send(()).unwrap();
    let (mut worker, ready) = result.expect("host callbacks waited for service work");
    host.join().unwrap();
    assert!(
        !ready,
        "quiescence cannot precede completion of accepted work"
    );
    drain(&mut worker);
    assert_eq!(
        state.effects.load(Ordering::Acquire),
        0,
        "cancellation must still be checked at the effect boundary"
    );
    assert_eq!(state.dropped.load(Ordering::Acquire), 1);
    assert_eq!(
        state.ticks.load(Ordering::Acquire),
        1,
        "queued wakes must coalesce and yield to shutdown"
    );
}

#[test]
fn errors_and_panics_are_reported_without_dropping_the_service_before_quiescence() {
    for fail in [1, 2] {
        let (state, _, _) = fixture(false, fail);
        let mut worker = ServiceWorker::<Probe<1>>::start(runtime(&state)).unwrap();
        worker.tick().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while worker.health().is_ok() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(state.dropped.load(Ordering::Acquire), 0);
        drain(&mut worker);
        assert_eq!(state.dropped.load(Ordering::Acquire), 1);
    }
}

fn unchanged_consumer<const VALUE: u64>() -> u64 {
    let (state, _, _) = fixture(false, 0);
    let mut worker = ServiceWorker::<Probe<VALUE>>::start(runtime(&state)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        worker.tick().unwrap();
        if !state.completed.lock().unwrap().is_empty() {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert_eq!(
        state.notifications.load(Ordering::Acquire),
        0,
        "idle passes must not create host wake feedback"
    );
    let value = state.completed.lock().unwrap()[0]["value"]
        .as_u64()
        .unwrap();
    drain(&mut worker);
    value
}

#[test]
fn unchanged_broker_consumer_can_replace_its_serialized_service() {
    assert_eq!(unchanged_consumer::<17>(), 17);
    assert_eq!(unchanged_consumer::<42>(), 42);
}

#[test]
fn quiescence_failure_remains_visible_until_the_service_actually_retires() {
    let (state, _, _) = fixture(false, 3);
    let mut worker = ServiceWorker::<Probe<1>>::start(runtime(&state)).unwrap();
    worker.prepare_shutdown(1000).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if worker.quiesce().is_err() {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert_eq!(state.dropped.load(Ordering::Acquire), 0);
    state.cancelled.store(true, Ordering::Release);
    loop {
        if matches!(worker.quiesce(), Ok(true)) {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert_eq!(state.dropped.load(Ordering::Acquire), 1);
}

#[test]
fn quiescence_without_an_announcement_does_not_invent_a_new_shutdown_budget() {
    let (state, _, _) = fixture(false, 0);
    let mut worker = ServiceWorker::<Probe<1>>::start(runtime(&state)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !worker.quiesce().unwrap() {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert_eq!(state.prepared_budget_ms.load(Ordering::Acquire), 0);
}
