//! Tracked asynchronous helper startup. Cancellation interrupts socket I/O,
//! but the worker remains owned until it actually exits and is joined.
use std::{
    ffi::c_void,
    io,
    net::Shutdown,
    os::unix::net::UnixStream,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicU8, Ordering},
    },
    thread,
};

#[derive(Default)]
pub(crate) struct Cancellation {
    state: AtomicU8,
    socket: Mutex<Option<UnixStream>>,
    receipt: Mutex<Option<Arc<crate::helper_process::Receipt>>>,
}

impl Cancellation {
    pub fn cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == 1
    }

    /// The caller retains its original socket; shutdown through this clone
    /// interrupts all pending I/O on that socket. Registration and cancellation
    /// synchronize so a cancellation cannot miss a newly spawned helper.
    pub fn register(&self, socket: &UnixStream) -> io::Result<()> {
        let socket = socket.try_clone()?;
        let mut slot = self
            .socket
            .lock()
            .map_err(|_| io::Error::other("open cancellation poisoned"))?;
        if self.state.load(Ordering::Acquire) != 0 {
            let _ = socket.shutdown(Shutdown::Both);
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        *slot = Some(socket);
        Ok(())
    }

    /// Publish the pre-spawn reservation before creating the process so
    /// cancellation also interrupts an in-flight exec handshake.
    pub fn register_receipt(&self, receipt: Arc<crate::helper_process::Receipt>) -> io::Result<()> {
        let mut slot = self
            .receipt
            .lock()
            .map_err(|_| io::Error::other("open cancellation poisoned"))?;
        if self.state.load(Ordering::Acquire) != 0 {
            receipt.stop();
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        *slot = Some(receipt);
        Ok(())
    }

    pub fn cancel(&self) -> bool {
        self.cancel_with(|socket| socket.shutdown(Shutdown::Both))
    }

    fn cancel_with(&self, shutdown: impl FnOnce(&UnixStream) -> io::Result<()>) -> bool {
        if self
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            == Err(2)
        {
            return true;
        }
        let receipt_cancelled = match self.receipt.try_lock() {
            Ok(receipt) => {
                if let Some(receipt) = receipt.as_ref() {
                    receipt.stop();
                }
                true
            }
            Err(TryLockError::Poisoned(error)) => {
                if let Some(receipt) = error.into_inner().as_ref() {
                    receipt.stop();
                }
                true
            }
            Err(TryLockError::WouldBlock) => false,
        };
        let mut slot = match self.socket.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::WouldBlock) => return false,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        if let Some(socket) = slot.as_ref()
            && let Err(error) = shutdown(socket)
            && !matches!(
                error.kind(),
                io::ErrorKind::NotConnected | io::ErrorKind::BrokenPipe
            )
        {
            // Retain the handle so a later cooperative poll can retry.
            return false;
        }
        slot.take();
        receipt_cancelled
    }

    fn cancel_for_drop(&self) {
        if self
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            == Err(2)
        {
            return;
        }
        // This is the final library-unload barrier, not a cooperative poll.
        // Wait for an in-progress registration so we cannot miss its socket.
        let receipt = self
            .receipt
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(receipt) = receipt.as_ref() {
            receipt.stop();
        }
        drop(receipt);
        let mut slot = self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(socket) = slot.take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    /// Once the worker is joined, atomically transfer its resources to the
    /// completed result. A concurrent cancellation wins or loses the CAS;
    /// it cannot shut down a session after it has been adopted.
    fn disarm(&self) -> Disarm {
        match self
            .state
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) | Err(2) => {}
            Err(_) => return Disarm::Cancelled,
        }
        let mut receipt = match self.receipt.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::WouldBlock) => return Disarm::Pending,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        let mut socket = match self.socket.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::WouldBlock) => return Disarm::Pending,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        receipt.take();
        socket.take();
        Disarm::Ready
    }
}

enum Disarm {
    Ready,
    Pending,
    Cancelled,
}

pub(crate) struct OpenTask<T> {
    cancellation: Arc<Cancellation>,
    worker: Option<libc::pthread_t>,
    result: Arc<Mutex<Option<std::thread::Result<T>>>>,
    delivered: bool,
}

pub(crate) enum TaskPoll<T> {
    Pending,
    /// Thread and TLS destructors have exited and the pthread was joined.
    Retired(io::Result<T>),
    /// The join failed; this task still owns the pthread and must be retained.
    Failed(io::Error),
}

struct Start<T> {
    work: Box<dyn FnOnce(Arc<Cancellation>) -> T + Send>,
    cancellation: Arc<Cancellation>,
    result: Arc<Mutex<Option<std::thread::Result<T>>>>,
}

extern "C" fn run_worker<T: Send + 'static>(context: *mut c_void) -> *mut c_void {
    // SAFETY: pthread_create transfers exactly one boxed Start to this worker.
    let start = unsafe { Box::from_raw(context.cast::<Start<T>>()) };
    let Start {
        work,
        cancellation,
        result: slot,
    } = *start;
    let result = catch_unwind(AssertUnwindSafe(|| work(cancellation)));
    *slot.lock().unwrap_or_else(|error| error.into_inner()) = Some(result);
    std::ptr::null_mut()
}

impl<T: Send + 'static> OpenTask<T> {
    pub fn spawn(work: impl FnOnce(Arc<Cancellation>) -> T + Send + 'static) -> io::Result<Self> {
        let cancellation = Arc::new(Cancellation::default());
        let result = Arc::new(Mutex::new(None));
        let start = Box::new(Start {
            work: Box::new(work),
            cancellation: cancellation.clone(),
            result: result.clone(),
        });
        let context = Box::into_raw(start);
        let mut worker = std::mem::MaybeUninit::<libc::pthread_t>::uninit();
        // SAFETY: context is a live uniquely owned Start. On success the new
        // thread takes ownership; on failure it remains ours to reclaim.
        let status = unsafe {
            libc::pthread_create(
                worker.as_mut_ptr(),
                std::ptr::null(),
                run_worker::<T>,
                context.cast(),
            )
        };
        if status != 0 {
            // SAFETY: a failed pthread_create did not consume the boxed Start.
            unsafe { drop(Box::from_raw(context)) };
            return Err(io::Error::from_raw_os_error(status));
        }
        // SAFETY: successful pthread_create initialized this thread identifier.
        let worker = unsafe { worker.assume_init() };
        Ok(Self {
            cancellation,
            worker: Some(worker),
            result,
            delivered: false,
        })
    }

    pub fn cancel(&self) -> bool {
        self.cancellation.cancel()
    }

    pub fn poll(&mut self) -> TaskPoll<T> {
        self.poll_with(|worker| {
            // SAFETY: this task uniquely owns the joinable pthread. tryjoin returns
            // EBUSY until all thread cleanup, including TLS destructors, has ended.
            unsafe { libc::pthread_tryjoin_np(worker, std::ptr::null_mut()) }
        })
    }

    fn poll_with(&mut self, try_join: impl FnOnce(libc::pthread_t) -> i32) -> TaskPoll<T> {
        if self.delivered {
            return TaskPoll::Failed(io::Error::other("KMS open result already delivered"));
        }
        if let Some(worker) = self.worker {
            let status = try_join(worker);
            if status == libc::EBUSY {
                return TaskPoll::Pending;
            }
            if status != 0 {
                return TaskPoll::Failed(io::Error::from_raw_os_error(status));
            }
            self.worker = None;
        }
        match self.cancellation.disarm() {
            Disarm::Pending => return TaskPoll::Pending,
            Disarm::Cancelled => {
                if !self.cancellation.cancel() {
                    return TaskPoll::Pending;
                }
                self.delivered = true;
                self.result
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                return TaskPoll::Retired(Err(io::Error::from(io::ErrorKind::Interrupted)));
            }
            Disarm::Ready => {}
        }
        self.delivered = true;
        let result = self
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        TaskPoll::Retired(match result {
            Some(Ok(value)) => Ok(value),
            Some(Err(_)) => Err(io::Error::other("KMS open worker panicked")),
            None => Err(io::Error::other("KMS open worker produced no result")),
        })
    }
}

impl<T> Drop for OpenTask<T> {
    fn drop(&mut self) {
        self.cancellation.cancel_for_drop();
        if let Some(worker) = self.worker.take() {
            // SAFETY: final safety barrier for the uniquely owned pthread.
            let _ = unsafe { libc::pthread_join(worker, std::ptr::null_mut()) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        io::Read,
        sync::mpsc,
        time::{Duration, Instant},
    };

    struct ExitGuard {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl Drop for ExitGuard {
        fn drop(&mut self) {
            let _ = self.entered.send(());
            let _ = self.release.recv_timeout(Duration::from_secs(2));
        }
    }

    thread_local! {
        static EXIT_GUARD: RefCell<Option<ExitGuard>> = const { RefCell::new(None) };
    }

    #[test]
    fn cancellation_interrupts_startup_and_waits_for_worker_exit() {
        let (socket, peer) = UnixStream::pair().unwrap();
        let (ready, registered) = mpsc::channel();
        let mut task = OpenTask::spawn(move |cancel| {
            cancel.register(&socket).unwrap();
            ready.send(()).unwrap();
            let mut socket = socket;
            let mut byte = [0];
            socket.read(&mut byte)
        })
        .unwrap();
        registered.recv_timeout(Duration::from_secs(1)).unwrap();
        let cancelled = task.cancel();
        if !cancelled {
            // Do not strand the worker if this test environment rejects
            // shutdown(2); its peer closure still lets the fixture retire.
            drop(peer);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            match task.poll() {
                TaskPoll::Retired(result) => break result,
                TaskPoll::Failed(error) => panic!("worker join failed: {error}"),
                TaskPoll::Pending => {}
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        };
        assert!(cancelled, "socket shutdown was rejected");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn cancellation_before_registration_cannot_miss_the_socket() {
        let cancellation = Cancellation::default();
        assert!(cancellation.cancel());
        let (socket, _peer) = UnixStream::pair().unwrap();
        assert_eq!(
            cancellation.register(&socket).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        let receipt = crate::helper_process::test_receipt();
        assert_eq!(
            cancellation
                .register_receipt(receipt.clone())
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(receipt.is_stopping());
    }

    #[test]
    fn cancellation_does_not_block_on_registration() {
        let cancellation = Cancellation::default();
        let held = cancellation.socket.lock().unwrap();
        assert!(!cancellation.cancel());
        assert!(cancellation.cancelled());
        drop(held);
        assert!(cancellation.cancel());
    }

    #[test]
    fn cancellation_accepts_disconnected_socket_but_retries_other_failures() {
        let disconnected = Cancellation::default();
        let (socket, _peer) = UnixStream::pair().unwrap();
        disconnected.register(&socket).unwrap();
        assert!(disconnected.cancel_with(|_| Err(io::ErrorKind::NotConnected.into())));

        let retry = Cancellation::default();
        retry.register(&socket).unwrap();
        assert!(!retry.cancel_with(|_| Err(io::ErrorKind::PermissionDenied.into())));
        assert!(retry.socket.lock().unwrap().is_some());
        assert!(retry.cancel_with(|_| Ok(())));
    }

    #[test]
    fn joined_result_is_adopted_without_stopping_its_session() {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let receipt = crate::helper_process::test_receipt();
        let worker_receipt = receipt.clone();
        let mut task = OpenTask::spawn(move |cancellation| {
            cancellation.register_receipt(worker_receipt).unwrap();
            cancellation.register(&socket).unwrap();
            socket
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let adopted = loop {
            match task.poll() {
                TaskPoll::Retired(result) => break result.unwrap(),
                TaskPoll::Failed(error) => panic!("worker join failed: {error}"),
                TaskPoll::Pending => {}
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        };
        assert!(task.cancel());
        drop(task);
        assert!(!receipt.is_stopping());
        drop(adopted);
    }

    #[test]
    fn poll_waits_for_tls_destructors_without_joining_early() {
        let (entered, in_destructor) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let mut task = OpenTask::spawn(move |_| {
            EXIT_GUARD.with(|guard| {
                *guard.borrow_mut() = Some(ExitGuard {
                    entered,
                    release: released,
                })
            });
            42
        })
        .unwrap();
        in_destructor.recv_timeout(Duration::from_secs(1)).unwrap();
        let still_pending = matches!(task.poll(), TaskPoll::Pending);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let value = loop {
            match task.poll() {
                TaskPoll::Retired(value) => break value.unwrap(),
                TaskPoll::Failed(error) => panic!("worker join failed: {error}"),
                TaskPoll::Pending => {}
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        };
        assert!(still_pending);
        assert_eq!(value, 42);
    }

    #[test]
    fn failed_try_join_retains_the_exact_worker() {
        let (release, wait) = mpsc::channel::<()>();
        let mut task = OpenTask::spawn(move |_| wait.recv().unwrap()).unwrap();
        assert!(matches!(
            task.poll_with(|_| libc::EINVAL),
            TaskPoll::Failed(_)
        ));
        assert!(task.worker.is_some());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match task.poll() {
                TaskPoll::Retired(value) => {
                    value.unwrap();
                    break;
                }
                TaskPoll::Failed(error) => panic!("worker join failed: {error}"),
                TaskPoll::Pending => {}
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }
}
