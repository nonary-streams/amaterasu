//! A single owned observation worker. Native discovery must not run on a caller's tick.
use amaterasu_media::{MEDIA_BACKEND_ERROR, MEDIA_BUSY};
use amaterasu_module_sdk::{
    abi::Status,
    capture::{SourcePreflightRequest, SourcePreflightResponse},
};
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

// The remote-session consumer retries after 250 ms. Observations are single-use
// and cannot be replayed indefinitely after a display topology change.
const RESULT_LIFETIME: Duration = Duration::from_secs(2);
const QUEUED_REQUEST_LIFETIME: Duration = Duration::from_secs(2);
const MAX_OBSERVATIONS: usize = 16;

#[derive(Clone, PartialEq, Eq)]
struct Key {
    caller: String,
    request: SourcePreflightRequest,
}

enum Observation {
    Queued(Key, Instant),
    Running(Key),
    Completed(Key, Instant, Result<SourcePreflightResponse, Status>),
}

#[derive(Default)]
struct State {
    stopping: bool,
    observations: Vec<Observation>,
}

type Shared = Arc<(Mutex<State>, Condvar)>;
type Observe =
    dyn FnMut(&str, &SourcePreflightRequest) -> Result<SourcePreflightResponse, Status> + Send;

pub(super) struct Worker {
    state: Shared,
    thread: Mutex<Option<JoinHandle<()>>>,
    join_failed: AtomicBool,
    #[cfg(target_os = "linux")]
    tid: Arc<std::sync::atomic::AtomicU32>,
}

impl Worker {
    pub(super) fn start(mut observe: Box<Observe>) -> Result<Self, String> {
        let state: Shared = Arc::default();
        let worker_state = state.clone();
        #[cfg(target_os = "linux")]
        let tid = Arc::new(std::sync::atomic::AtomicU32::new(0));
        #[cfg(target_os = "linux")]
        let worker_tid = tid.clone();
        let thread = thread::Builder::new()
            .name("dxgi-preflight".into())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                {
                    let id = std::fs::read_link("/proc/thread-self")
                        .ok()
                        .and_then(|path| path.file_name()?.to_str()?.parse::<u32>().ok())
                        .expect("observe preflight worker identity");
                    worker_tid.store(id, Ordering::Release);
                }
                run(&worker_state, &mut *observe)
            })
            .map_err(|error| format!("could not start DXGI preflight worker: {error}"))?;
        Ok(Self {
            state,
            thread: Mutex::new(Some(thread)),
            join_failed: AtomicBool::new(false),
            #[cfg(target_os = "linux")]
            tid,
        })
    }

    pub(super) fn query(
        &self,
        caller: &str,
        request: &SourcePreflightRequest,
    ) -> Result<SourcePreflightResponse, Status> {
        {
            let thread = self.thread.lock().map_err(|_| MEDIA_BACKEND_ERROR)?;
            if thread.as_ref().is_none_or(|thread| thread.is_finished()) {
                return Err(MEDIA_BACKEND_ERROR);
            }
        }
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().map_err(|_| MEDIA_BACKEND_ERROR)?;
        if state.stopping {
            return Err(MEDIA_BUSY);
        }
        let key = Key {
            caller: caller.into(),
            request: request.clone(),
        };
        remove_expired(&mut state.observations);
        if let Some(index) = state
            .observations
            .iter()
            .position(|observation| match observation {
                Observation::Queued(previous, _)
                | Observation::Running(previous)
                | Observation::Completed(previous, _, _) => previous == &key,
            })
        {
            if matches!(state.observations[index], Observation::Completed(..)) {
                let Observation::Completed(_, _, result) = state.observations.remove(index) else {
                    unreachable!()
                };
                return result;
            }
            if let Observation::Queued(_, last_retry) = &mut state.observations[index] {
                *last_retry = Instant::now();
            }
            return Err(MEDIA_BUSY);
        }
        if state.observations.len() >= MAX_OBSERVATIONS {
            return Err(MEDIA_BUSY);
        }
        state
            .observations
            .push(Observation::Queued(key, Instant::now()));
        wake.notify_one();
        Err(MEDIA_BUSY)
    }

    pub(super) fn stop(&self) -> Result<(), Status> {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().map_err(|_| MEDIA_BACKEND_ERROR)?;
        state.stopping = true;
        state
            .observations
            .retain(|observation| matches!(observation, Observation::Running(_)));
        wake.notify_one();
        Ok(())
    }

    pub(super) fn quiesce(&self) -> Result<bool, Status> {
        self.stop()?;
        if self.join_failed.load(Ordering::Acquire) {
            return Err(MEDIA_BACKEND_ERROR);
        }
        let mut thread = self.thread.lock().map_err(|_| MEDIA_BACKEND_ERROR)?;
        if let Some(thread) = thread.as_ref()
            && !self.thread_exited(thread)?
        {
            return Ok(false);
        }
        if let Some(thread) = thread.take()
            && thread.join().is_err()
        {
            self.join_failed.store(true, Ordering::Release);
            return Err(MEDIA_BACKEND_ERROR);
        }
        Ok(true)
    }

    #[cfg(target_os = "linux")]
    fn thread_exited(&self, thread: &JoinHandle<()>) -> Result<bool, Status> {
        if !thread.is_finished() {
            return Ok(false);
        }
        let tid = self.tid.load(Ordering::Acquire);
        if tid == 0 {
            return Err(MEDIA_BACKEND_ERROR);
        }
        let current = std::fs::read_link("/proc/thread-self").map_err(|_| MEDIA_BACKEND_ERROR)?;
        if !std::path::Path::new("/proc")
            .join(current)
            .try_exists()
            .map_err(|_| MEDIA_BACKEND_ERROR)?
        {
            return Err(MEDIA_BACKEND_ERROR);
        }
        std::path::Path::new(&format!("/proc/self/task/{tid}"))
            .try_exists()
            .map(|exists| !exists)
            .map_err(|_| MEDIA_BACKEND_ERROR)
    }

    #[cfg(not(target_os = "linux"))]
    fn thread_exited(&self, thread: &JoinHandle<()>) -> Result<bool, Status> {
        thread_exited(thread)
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn thread_exited(thread: &JoinHandle<()>) -> Result<bool, Status> {
    Ok(thread.is_finished())
}

#[cfg(windows)]
fn thread_exited(thread: &JoinHandle<()>) -> Result<bool, Status> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::WaitForSingleObject,
    };
    // SAFETY: JoinHandle owns a valid thread handle throughout this zero-time
    // wait. A signaled native thread has completed its TLS destructors too.
    match unsafe { WaitForSingleObject(HANDLE(thread.as_raw_handle()), 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(MEDIA_BACKEND_ERROR),
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stop();
        let thread = self
            .thread
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(thread) = thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(shared: &Shared, observe: &mut Observe) {
    let (lock, wake) = &**shared;
    loop {
        let key = {
            let mut state = match lock.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            remove_expired(&mut state.observations);
            while !state.stopping
                && !state
                    .observations
                    .iter()
                    .any(|item| matches!(item, Observation::Queued(..)))
            {
                state = match wake.wait(state) {
                    Ok(state) => state,
                    Err(_) => return,
                };
                remove_expired(&mut state.observations);
            }
            if state.stopping {
                return;
            }
            let Some(index) = state
                .observations
                .iter()
                .position(|item| matches!(item, Observation::Queued(..)))
            else {
                continue;
            };
            let Observation::Queued(key, _) = &state.observations[index] else {
                unreachable!()
            };
            let key = key.clone();
            state.observations[index] = Observation::Running(key.clone());
            key
        };
        let result = observe(&key.caller, &key.request);
        let mut state = match lock.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        if state.stopping {
            state.observations.clear();
            return;
        }
        if let Some(index) = state
            .observations
            .iter()
            .position(|item| matches!(item, Observation::Running(existing) if existing == &key))
        {
            state.observations[index] = Observation::Completed(key, Instant::now(), result);
        } else {
            return;
        }
    }
}

fn remove_expired(observations: &mut Vec<Observation>) {
    observations.retain(|observation| match observation {
        Observation::Queued(_, last_retry) => last_retry.elapsed() <= QUEUED_REQUEST_LIFETIME,
        Observation::Completed(_, completed, _) => completed.elapsed() <= RESULT_LIFETIME,
        Observation::Running(_) => true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::capture::{SourceClass, SourceMode};
    use std::sync::mpsc;

    fn request() -> SourcePreflightRequest {
        SourcePreflightRequest {
            source_class: SourceClass::ManagedOutput,
            route: None,
            mode: SourceMode {
                width: 1920,
                height: 1080,
                refresh_millihz: 60_000,
                hdr: false,
                bit_depth: None,
            },
        }
    }

    fn until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !condition() {
            assert!(Instant::now() < deadline, "worker did not make progress");
            std::thread::yield_now();
        }
    }

    #[test]
    fn held_observation_does_not_block_queries_or_claim_shutdown() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = Worker::start(Box::new(move |_, request| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(super::super::ready(request))
        }))
        .unwrap();
        assert_eq!(worker.query("alice", &request()).unwrap_err(), MEDIA_BUSY);
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(worker.query("alice", &request()).unwrap_err(), MEDIA_BUSY);
        assert_eq!(worker.query("bob", &request()).unwrap_err(), MEDIA_BUSY);
        assert!(!worker.quiesce().unwrap());
        assert_eq!(worker.query("alice", &request()).unwrap_err(), MEDIA_BUSY);
        release_tx.send(()).unwrap();
        until(|| worker.quiesce().unwrap());
    }

    #[test]
    fn result_is_scoped_to_exact_caller_and_request_and_consumed_once() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let worker = Worker::start(Box::new(move |caller, request| {
            entered_tx.send(caller.to_owned()).unwrap();
            Ok(super::super::ready(request))
        }))
        .unwrap();
        assert_eq!(worker.query("alice", &request()).unwrap_err(), MEDIA_BUSY);
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            "alice"
        );
        let mut result = None;
        until(|| match worker.query("alice", &request()) {
            Ok(value) => {
                result = Some(value);
                true
            }
            Err(MEDIA_BUSY) => false,
            Err(error) => panic!("unexpected worker error: {error:?}"),
        });
        assert!(result.unwrap().is_ready());
        assert_eq!(worker.query("bob", &request()).unwrap_err(), MEDIA_BUSY);
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            "bob"
        );
        let mut different = request();
        different.mode.width = 1280;
        until(|| {
            let state = worker.state.0.lock().unwrap();
            state
                .observations
                .iter()
                .any(|entry| matches!(entry, Observation::Completed(..)))
        });
        assert_eq!(worker.query("bob", &different).unwrap_err(), MEDIA_BUSY);
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            "bob"
        );
        worker.stop().unwrap();
        until(|| worker.quiesce().unwrap());
    }

    #[test]
    fn worker_panic_remains_a_shutdown_error() {
        let worker = Worker::start(Box::new(|_, _| panic!("failed observation"))).unwrap();
        assert_eq!(worker.query("alice", &request()).unwrap_err(), MEDIA_BUSY);
        until(|| {
            worker
                .thread
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_finished()
        });
        assert_eq!(
            worker.query("alice", &request()).unwrap_err(),
            MEDIA_BACKEND_ERROR
        );
        until(|| match worker.quiesce() {
            Err(MEDIA_BACKEND_ERROR) => true,
            Ok(false) => false,
            other => panic!("panic must not report successful retirement: {other:?}"),
        });
        assert_eq!(worker.quiesce().unwrap_err(), MEDIA_BACKEND_ERROR);
    }

    #[test]
    fn independent_clients_can_retrieve_their_completed_observations() {
        let worker =
            Worker::start(Box::new(|_, request| Ok(super::super::ready(request)))).unwrap();
        assert_eq!(worker.query("alice", &request()).unwrap_err(), MEDIA_BUSY);
        assert_eq!(worker.query("bob", &request()).unwrap_err(), MEDIA_BUSY);
        until(|| {
            worker
                .state
                .0
                .lock()
                .unwrap()
                .observations
                .iter()
                .filter(|item| matches!(item, Observation::Completed(..)))
                .count()
                == 2
        });
        assert!(worker.query("alice", &request()).unwrap().is_ready());
        assert!(worker.query("bob", &request()).unwrap().is_ready());
        worker.stop().unwrap();
        until(|| worker.quiesce().unwrap());
    }

    #[test]
    fn abandoned_queued_observation_is_not_started_after_a_stalled_one() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = Worker::start(Box::new(move |caller, request| {
            started_tx.send(caller.to_owned()).unwrap();
            if caller == "alice" {
                release_rx.recv().unwrap();
            }
            Ok(super::super::ready(request))
        }))
        .unwrap();
        assert_eq!(worker.query("alice", &request()).unwrap_err(), MEDIA_BUSY);
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            "alice"
        );
        assert_eq!(worker.query("bob", &request()).unwrap_err(), MEDIA_BUSY);
        {
            let mut state = worker.state.0.lock().unwrap();
            let queued = state
                .observations
                .iter_mut()
                .find_map(|item| match item {
                    Observation::Queued(key, last_retry) if key.caller == "bob" => Some(last_retry),
                    _ => None,
                })
                .unwrap();
            *queued = Instant::now() - QUEUED_REQUEST_LIFETIME - Duration::from_secs(1);
        }
        release_tx.send(()).unwrap();
        until(|| worker.query("alice", &request()).is_ok());
        assert_eq!(worker.query("carol", &request()).unwrap_err(), MEDIA_BUSY);
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            "carol"
        );
        assert!(started_rx.try_recv().is_err());
        worker.stop().unwrap();
        until(|| worker.quiesce().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn quiescence_waits_for_native_thread_exit_after_tls_destructors() {
        struct Gate {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Drop for Gate {
            fn drop(&mut self) {
                self.entered.send(()).unwrap();
                self.release.recv().unwrap();
            }
        }
        thread_local! {
            static GATE: std::cell::RefCell<Option<Gate>> = const { std::cell::RefCell::new(None) };
        }
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut gate = Some(Gate {
            entered: entered_tx,
            release: release_rx,
        });
        let worker = Worker::start(Box::new(move |_, request| {
            GATE.with(|slot| *slot.borrow_mut() = gate.take());
            Ok(super::super::ready(request))
        }))
        .unwrap();
        assert_eq!(worker.query("alice", &request()).unwrap_err(), MEDIA_BUSY);
        until(|| worker.query("alice", &request()).is_ok());
        worker.stop().unwrap();
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(!worker.quiesce().unwrap());
        release_tx.send(()).unwrap();
        until(|| worker.quiesce().unwrap());
    }
}
