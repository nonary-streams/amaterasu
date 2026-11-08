//! Bounded, read-only process snapshots off the managed-module tick.
use super::{ProcessSnapshot, scan_processes};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

pub(super) struct ProcessScanner {
    requests: Option<SyncSender<()>>,
    results: Receiver<Option<ProcessSnapshot>>,
    pending: bool,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ProcessScanner {
    #[cfg(test)]
    pub(super) fn held_for_test() -> (Self, Receiver<()>, mpsc::Sender<()>) {
        let (started, observed) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let mut scanner = Self::with_reader(move || {
            let _ = started.send(());
            let _ = blocked.recv();
            Some(ProcessSnapshot::default())
        })
        .unwrap();
        assert!(scanner.request());
        (scanner, observed, release)
    }
    pub(super) fn start(install_root: PathBuf) -> Option<Self> {
        Self::with_reader(move || scan_processes(&install_root))
    }

    fn with_reader(
        mut read: impl FnMut() -> Option<ProcessSnapshot> + Send + 'static,
    ) -> Option<Self> {
        let (requests, incoming) = mpsc::sync_channel(1);
        let (outgoing, results) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("amaterasu-steam-scan".into())
            .spawn(move || {
                while incoming.recv().is_ok() {
                    if outgoing.try_send(read()).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Self {
            requests: Some(requests),
            results,
            pending: false,
            worker: Some(worker),
        })
    }

    pub(super) fn request(&mut self) -> bool {
        if self.requests.is_none() {
            return false;
        }
        if self.pending {
            return true;
        }
        self.pending = self
            .requests
            .as_ref()
            .is_some_and(|requests| requests.try_send(()).is_ok());
        self.pending
    }

    /// Stop admitting scans and report retirement without waiting on a live read.
    pub(super) fn poll_shutdown(&mut self) -> bool {
        self.requests.take();
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return false;
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        true
    }

    pub(super) fn poll(&mut self) -> Option<Option<ProcessSnapshot>> {
        match self.results.try_recv() {
            Ok(result) => {
                self.pending = false;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.pending = false;
                Some(None)
            }
        }
    }
}

impl Drop for ProcessScanner {
    fn drop(&mut self) {
        self.requests.take();
        // Native module code must stay loaded until its scan has returned.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn shutdown_waits_nonblocking_for_pending_reader() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Retired(Arc<AtomicBool>);
        impl Drop for Retired {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let retired = Arc::new(AtomicBool::new(false));
        let guard = Retired(retired.clone());
        let (started, observed) = mpsc::sync_channel(1);
        let (release, blocked) = mpsc::sync_channel(1);
        let mut scanner = ProcessScanner::with_reader(move || {
            let _ = &guard;
            started.send(()).unwrap();
            blocked.recv().unwrap();
            Some(ProcessSnapshot::default())
        })
        .unwrap();
        scanner.request();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let retired_early = scanner.poll_shutdown();
        let admitted = scanner.request();
        release.send(()).unwrap();
        assert!(!retired_early);
        assert!(!admitted);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !scanner.poll_shutdown() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(retired.load(Ordering::SeqCst));
    }

    #[test]
    fn a_slow_scan_does_not_block_polling_or_queue_duplicate_work() {
        let (started, observed) = mpsc::sync_channel(1);
        let (release, blocked) = mpsc::sync_channel(1);
        let mut scanner = ProcessScanner::with_reader(move || {
            started.send(()).unwrap();
            blocked.recv().unwrap();
            Some(ProcessSnapshot::default())
        })
        .unwrap();
        assert!(scanner.request());
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(scanner.poll().is_none());
        assert!(scanner.request());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(snapshot) = scanner.poll() {
                assert!(snapshot.unwrap().processes.is_empty());
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(observed.try_recv().is_err());
    }
}
