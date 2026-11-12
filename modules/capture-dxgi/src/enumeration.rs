//! An owned native topology observation that never runs on the broker tick.
use crate::backend::Backend;
use crate::controller::enumeration_reply;
use crate::model::{CaptureError, CaptureResult};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::Value;
use std::thread::{self, JoinHandle};

#[cfg(target_os = "linux")]
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

pub(crate) struct Enumeration {
    thread: Option<JoinHandle<CaptureResult<Value>>>,
    #[cfg(target_os = "linux")]
    tid: Arc<AtomicU32>,
}

pub(crate) const MAX_PENDING_REQUESTS: usize = 32;
const MAX_RUNNING_ENUMERATIONS: usize = 4;

pub(crate) struct PendingEnumeration {
    request: Option<u64>,
    offset: Option<usize>,
    limit: Option<usize>,
    job: Option<Enumeration>,
    reply: Option<Value>,
}

impl PendingEnumeration {
    pub(crate) fn new(request: u64, offset: Option<usize>, limit: Option<usize>) -> Self {
        Self {
            request: Some(request),
            offset,
            limit,
            job: None,
            reply: None,
        }
    }
}

/// Drive all observations even when another request or worker has failed.
/// Canceled observations remain owned until the native worker has exited.
pub(crate) fn poll_requests<B: Backend + 'static>(
    runtime: &RuntimeClient,
    shutting_down: bool,
    pending: &mut Vec<PendingEnumeration>,
    make_backend: impl Fn() -> B,
) -> ModuleResult<()> {
    let mut failure = None;
    let mut running = pending.iter().filter(|item| item.job.is_some()).count();
    for mut item in std::mem::take(pending) {
        let mut activity_known = true;
        let active = if let Some(request) = item.request {
            match runtime.send(Command::CheckRequest { request }) {
                Ok(Response::Ok) => true,
                Ok(Response::Error { code, .. })
                    if code == "request_ended" || code == "not_found" =>
                {
                    item.request = None;
                    false
                }
                result => {
                    failure.get_or_insert_with(|| {
                        format!("capture enumeration activity check failed: {result:?}")
                    });
                    activity_known = false;
                    false
                }
            }
        } else {
            false
        };

        if let Some(job) = &mut item.job {
            match job.poll() {
                Ok(Some(reply)) => {
                    item.job = None;
                    running -= 1;
                    if item.request.is_some() && !shutting_down {
                        item.reply = Some(reply);
                    }
                }
                Ok(None) => (),
                Err(error) if job.thread.is_some() => {
                    failure.get_or_insert_with(|| {
                        format!(
                            "capture enumeration retirement failed: {}: {}",
                            error.code, error.message
                        )
                    });
                }
                Err(error) => {
                    item.job = None;
                    running -= 1;
                    if item.request.is_some() && !shutting_down {
                        item.reply = Some(error.payload());
                    }
                }
            }
        }

        if shutting_down && active && item.reply.is_none() {
            item.reply =
                Some(CaptureError::new("cancelled", "capture provider is shutting down").payload());
        }
        if active
            && !shutting_down
            && item.job.is_none()
            && item.reply.is_none()
            && running < MAX_RUNNING_ENUMERATIONS
        {
            let backend = make_backend();
            if !backend.supported() {
                item.reply = Some(
                    CaptureError::new("unsupported_platform", "DXGI capture requires Windows")
                        .payload(),
                );
            } else {
                match Enumeration::start(backend, item.offset, item.limit) {
                    Ok(job) => {
                        item.job = Some(job);
                        running += 1;
                    }
                    Err(error) => item.reply = Some(error.payload()),
                }
            }
        }
        if activity_known && let (Some(request), Some(reply)) = (item.request, item.reply.as_ref())
        {
            match runtime.send(Command::Complete {
                request,
                payload: reply.clone(),
            }) {
                Ok(Response::Ok) => item.request = None,
                Ok(Response::Error { code, .. })
                    if code == "request_ended" || code == "not_found" =>
                {
                    item.request = None
                }
                result => {
                    failure.get_or_insert_with(|| {
                        format!("capture enumeration completion failed: {result:?}")
                    });
                }
            }
        }
        if item.request.is_some() || item.job.is_some() {
            pending.push(item);
        }
    }
    failure.map_or(Ok(()), Err)
}

impl Enumeration {
    pub(crate) fn start<B: Backend + 'static>(
        backend: B,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> CaptureResult<Self> {
        #[cfg(target_os = "linux")]
        let tid = Arc::new(AtomicU32::new(0));
        #[cfg(target_os = "linux")]
        let thread_tid = tid.clone();
        let thread = thread::Builder::new()
            .name("dxgi-enumerate".into())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                {
                    let link = std::fs::read_link("/proc/thread-self").map_err(|error| {
                        CaptureError::new(
                            "enumeration_unavailable",
                            format!("cannot identify enumeration worker: {error}"),
                        )
                    })?;
                    let id = link
                        .file_name()
                        .and_then(|name| name.to_str())
                        .and_then(|name| name.parse::<u32>().ok())
                        .filter(|id| *id != 0)
                        .ok_or_else(|| {
                            CaptureError::new(
                                "enumeration_unavailable",
                                "invalid enumeration worker identity",
                            )
                        })?;
                    thread_tid.store(id, Ordering::Release);
                }
                backend
                    .enumerate()
                    .and_then(|outputs| enumeration_reply(outputs, offset, limit))
            })
            .map_err(|error| {
                CaptureError::new(
                    "enumeration_unavailable",
                    format!("cannot start enumeration worker: {error}"),
                )
            })?;
        Ok(Self {
            thread: Some(thread),
            #[cfg(target_os = "linux")]
            tid,
        })
    }

    /// A returned value proves the worker has exited, including native TLS
    /// destructors. An observation error leaves the thread owned for retry.
    pub(crate) fn poll(&mut self) -> CaptureResult<Option<Value>> {
        let Some(thread) = self.thread.as_ref() else {
            return Err(CaptureError::new(
                "enumeration_unavailable",
                "enumeration was already collected",
            ));
        };
        if !self.exited(thread)? {
            return Ok(None);
        }
        let thread = self.thread.take().expect("observed enumeration worker");
        match thread.join() {
            Ok(result) => result.map(Some),
            Err(_) => Err(CaptureError::new(
                "enumeration_unavailable",
                "enumeration worker panicked",
            )),
        }
    }

    #[cfg(windows)]
    fn exited(&self, thread: &JoinHandle<CaptureResult<Value>>) -> CaptureResult<bool> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::WaitForSingleObject;
        // SAFETY: the owned JoinHandle retains a valid native handle until join.
        match unsafe { WaitForSingleObject(HANDLE(thread.as_raw_handle()), 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(CaptureError::new(
                "enumeration_unavailable",
                "cannot observe enumeration worker termination",
            )),
        }
    }

    #[cfg(target_os = "linux")]
    fn exited(&self, thread: &JoinHandle<CaptureResult<Value>>) -> CaptureResult<bool> {
        if !thread.is_finished() {
            return Ok(false);
        }
        let tid = self.tid.load(Ordering::Acquire);
        // If identity lookup failed before any native work, joining is safe only
        // after the native task is gone; absence of identity cannot prove that.
        if tid == 0 {
            return Err(CaptureError::new(
                "enumeration_unavailable",
                "enumeration worker identity unavailable",
            ));
        }
        let own = std::fs::read_link("/proc/thread-self").map_err(|error| {
            CaptureError::new(
                "enumeration_unavailable",
                format!("cannot observe current task: {error}"),
            )
        })?;
        if !std::path::Path::new("/proc").join(own).is_dir() {
            return Err(CaptureError::new(
                "enumeration_unavailable",
                "current task is not observable",
            ));
        }
        std::path::Path::new(&format!("/proc/self/task/{tid}"))
            .try_exists()
            .map(|exists| !exists)
            .map_err(|error| {
                CaptureError::new(
                    "enumeration_unavailable",
                    format!("cannot observe enumeration worker: {error}"),
                )
            })
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    fn exited(&self, thread: &JoinHandle<CaptureResult<Value>>) -> CaptureResult<bool> {
        // DXGI is unsupported on this platform; this supports portable fixtures.
        Ok(thread.is_finished())
    }
}

impl Drop for Enumeration {
    fn drop(&mut self) {
        // Final destruction is the provider-code lifetime barrier. Quiescence
        // keeps polling and never calls this while a worker remains unretired.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::CaptureSession;
    use crate::model::Output;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    struct HeldBackend {
        entered: mpsc::Sender<()>,
        release: std::sync::Mutex<mpsc::Receiver<()>>,
    }

    impl Backend for HeldBackend {
        fn supported(&self) -> bool {
            true
        }
        fn enumerate(&self) -> CaptureResult<Vec<Output>> {
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            Ok(Vec::new())
        }
        fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
            unreachable!()
        }
    }

    fn poll_until_finished(job: &mut Enumeration) -> Value {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(reply) = job.poll().unwrap() {
                return reply;
            }
            assert!(Instant::now() < deadline, "enumeration worker did not exit");
            thread::yield_now();
        }
    }

    #[test]
    fn held_native_enumeration_remains_owned_and_poll_is_nonblocking() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut job = Enumeration::start(
            HeldBackend {
                entered: entered_tx,
                release: std::sync::Mutex::new(release_rx),
            },
            None,
            None,
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(job.poll().unwrap().is_none());
        release_tx.send(()).unwrap();
        assert_eq!(
            poll_until_finished(&mut job)["outputs"],
            serde_json::json!([])
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn completed_worker_is_not_joined_before_thread_local_destructor_finishes() {
        use std::cell::RefCell;
        struct HeldDestructor(mpsc::Sender<()>, mpsc::Receiver<()>);
        impl Drop for HeldDestructor {
            fn drop(&mut self) {
                self.0.send(()).unwrap();
                self.1.recv().unwrap();
            }
        }
        thread_local! { static HELD: RefCell<Option<HeldDestructor>> = const { RefCell::new(None) }; }
        struct TlsBackend(std::sync::Mutex<Option<HeldDestructor>>);
        impl Backend for TlsBackend {
            fn supported(&self) -> bool {
                true
            }
            fn enumerate(&self) -> CaptureResult<Vec<Output>> {
                HELD.with(|held| *held.borrow_mut() = self.0.lock().unwrap().take());
                Ok(Vec::new())
            }
            fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
                unreachable!()
            }
        }
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut job = Enumeration::start(
            TlsBackend(std::sync::Mutex::new(Some(HeldDestructor(
                entered_tx, release_rx,
            )))),
            None,
            None,
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(job.poll().unwrap().is_none());
        release_tx.send(()).unwrap();
        assert_eq!(
            poll_until_finished(&mut job)["outputs"],
            serde_json::json!([])
        );
    }
}
