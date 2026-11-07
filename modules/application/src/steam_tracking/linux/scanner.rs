//! Bounded, read-only process snapshots off the managed-module tick.
use super::{Process, scan_processes};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

pub(super) struct ProcessScanner {
    requests: SyncSender<()>,
    results: Receiver<Option<Vec<Process>>>,
    pending: bool,
}

impl ProcessScanner {
    pub(super) fn start(install_root: PathBuf) -> Option<Self> {
        Self::with_reader(move || scan_processes(&install_root))
    }

    fn with_reader(
        mut read: impl FnMut() -> Option<Vec<Process>> + Send + 'static,
    ) -> Option<Self> {
        let (requests, incoming) = mpsc::sync_channel(1);
        let (outgoing, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("amaterasu-steam-scan".into())
            .spawn(move || {
                while incoming.recv().is_ok() {
                    if outgoing.try_send(read()).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        // Dropping the scanner closes both channels. A pending read is allowed
        // to finish; it cannot signal processes and its worker then retires.
        Some(Self {
            requests,
            results,
            pending: false,
        })
    }

    pub(super) fn request(&mut self) -> bool {
        if self.pending {
            return true;
        }
        self.pending = self.requests.try_send(()).is_ok();
        self.pending
    }

    pub(super) fn poll(&mut self) -> Option<Option<Vec<Process>>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn dropping_the_scanner_retires_a_pending_reader() {
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
            Some(Vec::new())
        })
        .unwrap();
        scanner.request();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(scanner);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !retired.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_slow_scan_does_not_block_polling_or_queue_duplicate_work() {
        let (started, observed) = mpsc::sync_channel(1);
        let (release, blocked) = mpsc::sync_channel(1);
        let mut scanner = ProcessScanner::with_reader(move || {
            started.send(()).unwrap();
            blocked.recv().unwrap();
            Some(Vec::new())
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
                assert!(snapshot.unwrap().is_empty());
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(observed.try_recv().is_err());
    }
}
