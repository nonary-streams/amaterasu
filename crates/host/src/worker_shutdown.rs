//! EOF on the supervisor-owned pipe requests orderly worker shutdown.

use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

/// Receives only a closed shutdown pipe. Data and reader failures are errors.
///
/// The reader thread deliberately detaches: its blocking read must never hold
/// up worker shutdown when another worker error already requires exit. The
/// process owns this receiver and reclaims the reader when it exits.
pub(crate) struct WorkerShutdown {
    receiver: Receiver<io::Result<()>>,
    requested: bool,
}

impl WorkerShutdown {
    pub(crate) fn from_reader(mut reader: impl Read + Send + 'static) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("worker-shutdown".to_owned())
            .spawn(move || {
                let result = read_shutdown_request(&mut reader);
                let _ = sender.send(result);
            })?;
        Ok(Self {
            receiver,
            requested: false,
        })
    }

    /// Poll without waiting for the pipe reader. Once observed, EOF stays set.
    pub(crate) fn is_requested(&mut self) -> io::Result<bool> {
        if self.requested {
            return Ok(true);
        }
        match self.receiver.try_recv() {
            Ok(result) => {
                result?;
                self.requested = true;
                Ok(true)
            }
            Err(TryRecvError::Empty) => Ok(false),
            Err(TryRecvError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "supervisor shutdown reader stopped without observing EOF",
            )),
        }
    }
}

fn read_shutdown_request(reader: &mut impl Read) -> io::Result<()> {
    let mut byte = [0_u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "supervisor shutdown pipe carried unexpected data",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Always retire the owned supervisor, preserving either or both failures.
pub(crate) fn finish_with_cleanup<T>(
    result: Result<T, String>,
    cleanup: impl FnOnce() -> Result<(), String>,
) -> Result<T, String> {
    match (result, cleanup()) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => {
            Err(format!("{error}; shutdown cleanup failed: {cleanup_error}"))
        }
    }
}

/// Read readiness only after the process wait observes the worker running.
pub(crate) fn readiness_after_process_wait(
    wait_result: u32,
    read_ready: impl FnOnce() -> Result<bool, String>,
) -> Result<bool, String> {
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;
    match wait_result {
        WAIT_TIMEOUT => read_ready(),
        WAIT_OBJECT_0 => Err("interactive worker exited before readiness".into()),
        _ => Err("interactive worker readiness process status is unavailable".into()),
    }
}

/// Charge forced containment to the existing service shutdown allowance.
pub(crate) fn remaining_containment(
    shutdown_deadline: Option<Instant>,
    now: Instant,
    allowance: Duration,
) -> Duration {
    match shutdown_deadline {
        None => allowance,
        Some(deadline) => {
            deadline
                .checked_add(allowance)
                .map_or(Duration::ZERO, |service_deadline| {
                    service_deadline
                        .saturating_duration_since(now)
                        .min(allowance)
                })
        }
    }
}

/// A failed startup permits replacement only after complete worker retirement.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StartupFailure {
    Retryable(String),
    CleanupFailed(String),
}

impl From<String> for StartupFailure {
    fn from(message: String) -> Self {
        Self::Retryable(message)
    }
}

impl From<&str> for StartupFailure {
    fn from(message: &str) -> Self {
        Self::Retryable(message.to_owned())
    }
}

impl StartupFailure {
    pub(crate) fn after_cleanup(primary: String, cleanup: Result<(), String>) -> Self {
        match cleanup {
            Ok(()) => Self::Retryable(primary),
            Err(cleanup_error) => Self::CleanupFailed(format!(
                "{primary}; startup cleanup failed: {cleanup_error}"
            )),
        }
    }

    pub(crate) fn into_retryable(self, stopping: bool) -> Result<String, String> {
        match (self, stopping) {
            (Self::Retryable(message), false) => Ok(message),
            (Self::Retryable(message) | Self::CleanupFailed(message), _) => Err(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc::Sender;
    use std::time::{Duration, Instant};

    #[test]
    fn readiness_requires_a_running_process_before_reading_ready() {
        assert_eq!(readiness_after_process_wait(258, || Ok(true)), Ok(true));
        assert_eq!(readiness_after_process_wait(258, || Ok(false)), Ok(false));
        assert_eq!(
            readiness_after_process_wait(258, || Err("pipe failed".to_owned())),
            Err("pipe failed".to_owned())
        );
        for (result, message) in [
            (0, "interactive worker exited before readiness"),
            (
                u32::MAX,
                "interactive worker readiness process status is unavailable",
            ),
            (
                1,
                "interactive worker readiness process status is unavailable",
            ),
        ] {
            let mut read = false;
            let observed = readiness_after_process_wait(result, || {
                read = true;
                Ok(true)
            });
            assert_eq!(observed, Err(message.to_owned()));
            assert!(!read, "a READY record must not override an uncertain wait");
        }
    }

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    fn await_request(shutdown: &mut WorkerShutdown) -> io::Result<bool> {
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            match shutdown.is_requested() {
                Ok(false) => {
                    assert!(Instant::now() < deadline, "shutdown reader did not finish");
                    thread::yield_now();
                }
                result => return result,
            }
        }
    }

    #[test]
    fn eof_requests_shutdown_and_remains_requested() {
        let mut shutdown = WorkerShutdown::from_reader(Cursor::new(Vec::<u8>::new())).unwrap();
        assert!(await_request(&mut shutdown).unwrap());
        assert!(shutdown.is_requested().unwrap());
        assert!(shutdown.is_requested().unwrap());
    }

    struct PendingReader {
        started: Sender<()>,
        release: Receiver<()>,
        finished: Sender<()>,
    }

    impl Read for PendingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            self.started.send(()).unwrap();
            self.release.recv().unwrap();
            self.finished.send(()).unwrap();
            Ok(0)
        }
    }

    #[test]
    fn pending_reader_does_not_block_polling_or_receiver_drop() {
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let (finished_sender, finished_receiver) = mpsc::channel();
        let mut shutdown = WorkerShutdown::from_reader(PendingReader {
            started: started_sender,
            release: release_receiver,
            finished: finished_sender,
        })
        .unwrap();
        started_receiver.recv_timeout(TEST_TIMEOUT).unwrap();
        assert!(!shutdown.is_requested().unwrap());
        assert_eq!(finished_receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
        drop(shutdown);
        release_sender.send(()).unwrap();
        finished_receiver.recv_timeout(TEST_TIMEOUT).unwrap();
    }

    #[test]
    fn any_input_byte_is_a_protocol_error() {
        for byte in [0, b'\n', b'1', 255] {
            let mut shutdown = WorkerShutdown::from_reader(Cursor::new([byte])).unwrap();
            assert_eq!(
                await_request(&mut shutdown).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected shutdown read failure",
            ))
        }
    }

    #[test]
    fn read_failure_is_not_a_shutdown_request() {
        let mut shutdown = WorkerShutdown::from_reader(FailingReader).unwrap();
        let error = await_request(&mut shutdown).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "injected shutdown read failure");
    }

    struct InterruptedReader {
        interruptions: usize,
    }

    impl Read for InterruptedReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            if self.interruptions > 0 {
                self.interruptions -= 1;
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(0)
            }
        }
    }

    #[test]
    fn interrupted_reads_retry_until_eof() {
        let mut shutdown =
            WorkerShutdown::from_reader(InterruptedReader { interruptions: 3 }).unwrap();
        assert!(await_request(&mut shutdown).unwrap());
    }

    struct PanickingReader;

    impl Read for PanickingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            panic!("injected shutdown reader panic");
        }
    }

    #[test]
    fn panicking_reader_is_not_mistaken_for_eof() {
        let mut shutdown = WorkerShutdown::from_reader(PanickingReader).unwrap();
        assert_eq!(
            await_request(&mut shutdown).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn disconnected_channel_without_eof_is_an_error() {
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let mut shutdown = WorkerShutdown {
            receiver,
            requested: false,
        };
        assert_eq!(
            shutdown.is_requested().unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn queued_eof_is_observed_after_reader_has_disconnected() {
        let (sender, receiver) = mpsc::channel();
        sender.send(Ok(())).unwrap();
        drop(sender);
        let mut shutdown = WorkerShutdown {
            receiver,
            requested: false,
        };
        assert!(shutdown.is_requested().unwrap());
        assert!(shutdown.is_requested().unwrap());
    }

    #[test]
    fn finalizer_runs_after_an_error_and_preserves_the_primary_error() {
        let mut cleanup_called = false;
        let result = finish_with_cleanup::<()>(Err("supervision failed".to_owned()), || {
            cleanup_called = true;
            Ok(())
        });
        assert!(cleanup_called);
        assert_eq!(result.unwrap_err(), "supervision failed");
    }

    #[test]
    fn finalizer_preserves_both_primary_and_cleanup_failures() {
        let error = finish_with_cleanup::<()>(Err("supervision failed".to_owned()), || {
            Err("worker failed to exit".to_owned())
        })
        .unwrap_err();
        assert!(error.contains("supervision failed"));
        assert!(error.contains("worker failed to exit"));
    }

    #[test]
    fn cleanup_failure_prevents_successful_supervision_result() {
        assert_eq!(
            finish_with_cleanup(Ok(17), || Err("worker still running".to_owned())),
            Err("worker still running".to_owned())
        );
    }

    #[test]
    fn successful_cleanup_preserves_the_supervision_value() {
        assert_eq!(finish_with_cleanup(Ok(17), || Ok(())), Ok(17));
    }

    #[test]
    fn failed_startup_retirement_never_permits_replacement() {
        for stopping in [false, true] {
            let failure = StartupFailure::after_cleanup(
                "worker readiness failed".to_owned(),
                Err("worker did not retire".to_owned()),
            );
            let error = failure.into_retryable(stopping).unwrap_err();
            assert!(error.contains("worker readiness failed"));
            assert!(error.contains("worker did not retire"));
        }
    }

    #[test]
    fn service_stop_cannot_swallow_a_startup_failure() {
        let failure = StartupFailure::after_cleanup("startup cancelled".to_owned(), Ok(()));
        assert_eq!(
            failure.into_retryable(true),
            Err("startup cancelled".to_owned())
        );
    }

    #[test]
    fn successful_startup_cleanup_permits_retry_and_preserves_the_diagnostic() {
        let failure = StartupFailure::after_cleanup("worker readiness failed".to_owned(), Ok(()));
        assert_eq!(
            failure.into_retryable(false),
            Ok("worker readiness failed".to_owned())
        );
    }

    #[test]
    fn early_drain_failure_gets_at_most_the_containment_allowance() {
        let now = Instant::now();
        let allowance = Duration::from_secs(3);
        assert_eq!(
            remaining_containment(Some(now + Duration::from_secs(5)), now, allowance),
            allowance
        );
    }

    #[test]
    fn late_shutdown_consumes_the_existing_containment_time() {
        let now = Instant::now();
        assert_eq!(
            remaining_containment(
                Some(now - Duration::from_secs(2)),
                now,
                Duration::from_secs(3),
            ),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn expired_service_shutdown_gets_no_additional_containment_time() {
        let now = Instant::now();
        assert_eq!(
            remaining_containment(
                Some(now - Duration::from_secs(4)),
                now,
                Duration::from_secs(3),
            ),
            Duration::ZERO
        );
    }

    #[test]
    fn dropping_without_explicit_shutdown_gets_the_normal_containment_allowance() {
        let allowance = Duration::from_secs(3);
        assert_eq!(
            remaining_containment(None, Instant::now(), allowance),
            allowance
        );
    }

    #[test]
    fn ordinary_startup_errors_convert_to_retryable_failures() {
        assert_eq!(
            StartupFailure::from("token lookup failed").into_retryable(false),
            Ok("token lookup failed".to_owned())
        );
        assert_eq!(
            StartupFailure::from("spawn failed".to_owned()).into_retryable(false),
            Ok("spawn failed".to_owned())
        );
    }
}
