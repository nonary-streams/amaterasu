//! Join every service-owned thread even if another thread fails.
use crate::{InputError, InputResult};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread::JoinHandle,
};

/// A panic may interrupt native device cleanup. Fence the service before
/// allowing another session and wake the worker listener for prompt shutdown.
pub(crate) fn run_guarded_worker(
    work: impl FnOnce(),
    stop: &AtomicBool,
    cleanup_failed: &AtomicBool,
    wake: impl FnOnce(),
) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_err() {
        cleanup_failed.store(true, Ordering::Release);
        stop.store(true, Ordering::Release);
        wake();
    }
}

pub(crate) fn join_service_threads(
    workers: Vec<JoinHandle<()>>,
    host_listener: JoinHandle<InputResult<()>>,
) -> InputResult<()> {
    let mut worker_failed = false;
    for worker in workers {
        worker_failed |= worker.join().is_err();
    }
    // The host listener owns service resources too, including on worker failure.
    let host_result = host_listener.join().map_err(|_| InputError::Device)?;
    if worker_failed {
        Err(InputError::Device)
    } else {
        host_result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn failed_worker_does_not_detach_other_workers_or_listener() {
        let (worker_release, worker_wait) = mpsc::channel();
        let (host_release, host_wait) = mpsc::channel();
        let (finished, completion) = mpsc::channel();
        let failed = std::thread::spawn(|| panic!("injected worker failure"));
        let worker = std::thread::spawn(move || worker_wait.recv().unwrap());
        let host = std::thread::spawn(move || {
            host_wait.recv().unwrap();
            Ok(())
        });
        let joiner = std::thread::spawn(move || {
            finished
                .send(join_service_threads(vec![failed, worker], host))
                .unwrap();
        });
        assert!(completion.recv_timeout(Duration::from_millis(30)).is_err());
        worker_release.send(()).unwrap();
        assert!(completion.recv_timeout(Duration::from_millis(30)).is_err());
        host_release.send(()).unwrap();
        assert_eq!(
            completion.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(InputError::Device)
        );
        joiner.join().unwrap();
    }

    #[test]
    fn worker_panic_fences_service_and_wakes_listener() {
        let stop = AtomicBool::new(false);
        let cleanup_failed = AtomicBool::new(false);
        let mut woke = false;
        run_guarded_worker(
            || panic!("injected worker failure"),
            &stop,
            &cleanup_failed,
            || woke = true,
        );
        assert!(stop.load(Ordering::Acquire));
        assert!(cleanup_failed.load(Ordering::Acquire));
        assert!(woke);
    }

    #[test]
    fn listener_failure_is_returned_after_workers_join() {
        let worker = std::thread::spawn(|| {});
        let host = std::thread::spawn(|| Err(InputError::Closed));
        assert_eq!(
            join_service_threads(vec![worker], host),
            Err(InputError::Closed)
        );
    }
}
