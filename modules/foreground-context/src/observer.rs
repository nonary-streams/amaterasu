//! Runs foreground observation away from the host's cooperative tick.
//!
//! Observing the foreground window starts helper processes and waits for them,
//! which takes tens of milliseconds. The host ticks every module on one thread,
//! so doing that inline would freeze capture, encoding and packet delivery for
//! the whole wait.
use amaterasu_module_sdk::foreground_context::Snapshot;
use std::{
    sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel},
    thread::{self, JoinHandle},
};

/// Observations are serialized, and only one is ever accepted at a time.
const REQUEST_CAPACITY: usize = 1;

pub(crate) struct Observation {
    pub request: u64,
    pub snapshot: Option<Snapshot>,
}

pub(crate) struct Observer {
    requests: Option<SyncSender<u64>>,
    results: Receiver<Observation>,
    worker: Option<JoinHandle<()>>,
    in_flight: bool,
}

impl Observer {
    pub(crate) fn start(
        observe: impl Fn() -> Option<Snapshot> + Send + 'static,
    ) -> Result<Self, String> {
        let (requests, queued) = sync_channel::<u64>(REQUEST_CAPACITY);
        let (finished, results) = sync_channel::<Observation>(REQUEST_CAPACITY);
        let worker = thread::Builder::new()
            .name("foreground-observe".into())
            .spawn(move || {
                while let Ok(request) = queued.recv() {
                    let snapshot = observe();
                    if finished.send(Observation { request, snapshot }).is_err() {
                        return;
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            requests: Some(requests),
            results,
            worker: Some(worker),
            in_flight: false,
        })
    }

    pub(crate) fn busy(&self) -> bool {
        self.in_flight
    }

    /// Queues one observation. A refused request is reported to the caller as
    /// unavailable evidence rather than waiting here.
    pub(crate) fn submit(&mut self, request: u64) -> bool {
        let Some(requests) = &self.requests else {
            return false;
        };
        if self.in_flight {
            return false;
        }
        match requests.try_send(request) {
            Ok(()) => {
                self.in_flight = true;
                true
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }

    pub(crate) fn poll(&mut self) -> Option<Observation> {
        match self.results.try_recv() {
            Ok(observation) => {
                self.in_flight = false;
                Some(observation)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.in_flight = false;
                None
            }
        }
    }

    /// Stops accepting work. Returns true once the worker has exited and no
    /// result remains to deliver; helper processes are bounded by their own
    /// timeouts, so this settles without the host waiting on it.
    pub(crate) fn retire(&mut self) -> bool {
        self.requests.take();
        if self.in_flight {
            return false;
        }
        let Some(worker) = &self.worker else {
            return true;
        };
        if !worker.is_finished() {
            return false;
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        true
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        self.requests.take();
        while self.in_flight {
            if self.results.recv().is_err() {
                break;
            }
            self.in_flight = false;
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};

    fn wait_for(observer: &mut Observer) -> Observation {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(observation) = observer.poll() {
                return observation;
            }
            assert!(Instant::now() < deadline, "observation never finished");
            thread::yield_now();
        }
    }

    #[test]
    fn a_slow_observation_never_blocks_the_caller() {
        let release = Arc::new(AtomicBool::new(false));
        let gate = release.clone();
        let mut observer = Observer::start(move || {
            while !gate.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(1));
            }
            None
        })
        .unwrap();

        let started = Instant::now();
        assert!(observer.submit(7));
        assert!(observer.poll().is_none());
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(observer.busy());

        release.store(true, Ordering::SeqCst);
        let observation = wait_for(&mut observer);
        assert_eq!(observation.request, 7);
        assert!(observation.snapshot.is_none());
        assert!(!observer.busy());
    }

    #[test]
    fn only_one_observation_is_admitted_at_a_time() {
        let release = Arc::new(AtomicBool::new(false));
        let gate = release.clone();
        let mut observer = Observer::start(move || {
            while !gate.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(1));
            }
            None
        })
        .unwrap();

        assert!(observer.submit(1));
        assert!(!observer.submit(2));

        release.store(true, Ordering::SeqCst);
        assert_eq!(wait_for(&mut observer).request, 1);
        assert!(observer.submit(3));
        assert_eq!(wait_for(&mut observer).request, 3);
    }

    #[test]
    fn retirement_reports_progress_and_finishes_after_the_last_result() {
        let mut observer = Observer::start(|| None).unwrap();
        assert!(observer.submit(1));
        let deadline = Instant::now() + Duration::from_secs(5);
        while observer.busy() {
            let _ = observer.poll();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        while !observer.retire() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(!observer.submit(2));
    }
}
