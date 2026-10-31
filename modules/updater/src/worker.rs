//! Release checks run outside the module tick and retire before module unload.
use crate::release::ReleaseSet;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

pub(super) struct ReleaseWorker {
    result: Receiver<Result<ReleaseSet, String>>,
    thread: Option<JoinHandle<()>>,
}

impl ReleaseWorker {
    pub fn spawn(
        check: impl FnOnce() -> Result<ReleaseSet, String> + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, result) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("amaterasu-updater-check".into())
            .spawn(move || {
                let _ = sender.send(check());
            })?;
        Ok(Self {
            result,
            thread: Some(thread),
        })
    }

    /// Joining only a finished worker keeps module ticks nonblocking. A panic
    /// becomes a failed check rather than leaving check_in_progress set forever.
    pub fn poll(&mut self) -> Option<Result<ReleaseSet, String>> {
        if !self.thread.as_ref()?.is_finished() {
            return None;
        }
        let thread = self.thread.take().expect("finished worker exists");
        if thread.join().is_err() {
            return Some(Err("release worker stopped".into()));
        }
        Some(
            self.result
                .try_recv()
                .unwrap_or_else(|_| Err("release worker returned no result".into())),
        )
    }
}

impl Drop for ReleaseWorker {
    fn drop(&mut self) {
        // The bounded HTTP check owns provider bindings and executes code from
        // this module. Joining is mandatory before either DLL can be unloaded.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
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

    #[test]
    fn result_is_returned_after_worker_retirement() {
        let retired = Arc::new(AtomicBool::new(false));
        let worker_retired = retired.clone();
        let mut worker = ReleaseWorker::spawn(move || {
            worker_retired.store(true, Ordering::Release);
            Ok(ReleaseSet {
                stable: None,
                prerelease: None,
            })
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = worker.poll() {
                assert_eq!(
                    result.unwrap(),
                    ReleaseSet {
                        stable: None,
                        prerelease: None
                    }
                );
                assert!(retired.load(Ordering::Acquire));
                assert!(worker.thread.is_none());
                break;
            }
            assert!(Instant::now() < deadline, "worker did not finish");
            std::thread::yield_now();
        }
    }

    #[test]
    fn drop_joins_an_active_check_before_returning() {
        let (started, entered) = mpsc::sync_channel(1);
        let (release, continue_check) = mpsc::sync_channel(1);
        let worker = ReleaseWorker::spawn(move || {
            started.send(()).unwrap();
            continue_check.recv().unwrap();
            Err("finished".into())
        })
        .unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let (dropped, completed) = mpsc::sync_channel(1);
        let closer = std::thread::spawn(move || {
            drop(worker);
            dropped.send(()).unwrap();
        });
        assert!(completed.recv_timeout(Duration::from_millis(20)).is_err());
        release.send(()).unwrap();
        completed.recv_timeout(Duration::from_secs(2)).unwrap();
        closer.join().unwrap();
    }

    #[test]
    fn a_panicked_check_returns_a_failure_instead_of_staying_pending() {
        let mut worker = ReleaseWorker::spawn(|| panic!("injected release failure")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = worker.poll() {
                assert!(result.is_err());
                assert!(worker.thread.is_none());
                break;
            }
            assert!(Instant::now() < deadline, "panicked worker stayed pending");
            std::thread::yield_now();
        }
    }
}
