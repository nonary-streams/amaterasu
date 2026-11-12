use crate::backend::{Backend, CaptureSession, TouchSurface};
use crate::model::{
    CaptureError, CaptureResult, Frame, MAX_ENUMERATE_LIMIT, MAX_STREAMS, MAX_WAIT_MS, Operation,
    Output,
};
use amaterasu_media::CaptureGeometry;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

#[cfg(test)]
#[path = "shutdown_tests.rs"]
pub(crate) mod shutdown_tests;

struct Stream {
    owner: String,
    output: String,
    session: Box<dyn CaptureSession>,
    geometry: Option<CaptureGeometry>,
    touch_surface: Option<TouchSurface>,
    operation_id: Option<String>,
    format: crate::format::CaptureFormat,
}

#[derive(Clone)]
pub(crate) struct CloseReceipt(Arc<AtomicBool>);

impl CloseReceipt {
    pub(crate) fn retired(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct Retirement {
    owner: String,
    operation_id: Option<String>,
    stream: Arc<Mutex<Option<Stream>>>,
    worker: Option<JoinHandle<()>>,
    receipt: CloseReceipt,
    failure: Option<String>,
    #[cfg(target_os = "linux")]
    tid: Arc<std::sync::atomic::AtomicU32>,
}

struct PreparedCapture {
    session: Box<dyn CaptureSession>,
    geometry: Option<CaptureGeometry>,
    touch_surface: Option<TouchSurface>,
    failure: Option<CaptureError>,
}

struct Opening {
    owner: String,
    output: String,
    operation_id: Option<String>,
    format: crate::format::CaptureFormat,
    cancelled: bool,
    worker: JoinHandle<CaptureResult<PreparedCapture>>,
    #[cfg(target_os = "linux")]
    tid: Arc<std::sync::atomic::AtomicU32>,
}

pub(crate) enum OpenStart {
    Ready(Value),
    Pending(u64),
}

pub struct Controller<B: Backend> {
    backend: Arc<B>,
    enabled: bool,
    streams: BTreeMap<u64, Stream>,
    retiring: BTreeMap<u64, Retirement>,
    opening: BTreeMap<u64, Opening>,
    close_id_reservations: BTreeSet<u64>,
    startup_retirement_uncertain: bool,
    #[cfg(test)]
    test_open_observation_failures: usize,
    opened_stream: Option<u64>,
}

impl<B: Backend> Controller<B> {
    pub(crate) fn preflight_state(&self) -> (bool, bool) {
        (
            self.enabled && !self.startup_retirement_uncertain,
            self.streams.len() + self.retiring.len() + self.opening.len() < MAX_STREAMS,
        )
    }

    pub fn new(backend: B, enabled: bool) -> Self {
        Self {
            backend: Arc::new(backend),
            enabled,
            streams: BTreeMap::new(),
            retiring: BTreeMap::new(),
            opening: BTreeMap::new(),
            close_id_reservations: BTreeSet::new(),
            startup_retirement_uncertain: false,
            #[cfg(test)]
            test_open_observation_failures: 0,
            opened_stream: None,
        }
    }

    pub fn shutdown(&mut self) {
        self.stop_admission();
        for (_, job) in std::mem::take(&mut self.opening) {
            let _ = job.worker.join();
        }
        let owners: Vec<_> = self
            .streams
            .iter()
            .map(|(&id, s)| (id, s.owner.clone()))
            .collect();
        for (id, owner) in owners {
            let _ = self.begin_close(&owner, id);
        }
        // Final destruction is the mandatory provider-code lifetime barrier.
        // Unlike quiesce, it may wait for a stuck driver rather than detach it.
        for (_, mut retirement) in std::mem::take(&mut self.retiring) {
            if let Some(worker) = retirement.worker.take() {
                let _ = worker.join();
            }
        }
    }

    pub(crate) fn quiesce(&mut self) -> CaptureResult<bool> {
        self.stop_admission();
        let openings: Vec<_> = self.opening.keys().copied().collect();
        let mut startup_error = None;
        for id in openings {
            self.cancel_open(id);
            if let Err(error) = self.poll_open(id, false) {
                startup_error.get_or_insert(error);
            }
        }
        let owners: Vec<_> = self
            .streams
            .iter()
            .map(|(&id, s)| (id, s.owner.clone()))
            .collect();
        for (id, owner) in owners {
            self.begin_close(&owner, id)?;
        }
        if let Some((_, error)) = self.poll_closing().into_iter().next() {
            return Err(error);
        }
        if let Some(error) = startup_error {
            return Err(error);
        }
        if self.startup_retirement_uncertain {
            return Err(CaptureError::new(
                "retirement_failed",
                "capture startup retirement cannot be proven",
            ));
        }
        Ok(self.streams.is_empty() && self.retiring.is_empty() && self.opening.is_empty())
    }

    pub fn stop_admission(&mut self) {
        self.enabled = false;
    }

    pub(crate) fn set_close_id_reservations(&mut self, ids: impl IntoIterator<Item = u64>) {
        self.close_id_reservations = ids.into_iter().collect();
    }

    #[cfg(test)]
    pub fn take_opened_stream(&mut self) -> Option<u64> {
        self.opened_stream.take()
    }

    pub(crate) fn begin_open(
        &mut self,
        caller: &str,
        output: String,
        operation_id: Option<String>,
        source_format: Option<amaterasu_module_sdk::capture::SourceFormat>,
    ) -> CaptureResult<OpenStart> {
        let format = crate::format::CaptureFormat::requested(source_format.as_ref())?;
        if !self.enabled {
            return Err(CaptureError::new(
                "capture_disabled",
                "capture requires enabled startup policy",
            ));
        }
        if self.startup_retirement_uncertain {
            return Err(CaptureError::new(
                "retirement_failed",
                "capture startup retirement cannot be proven",
            ));
        }
        if let Some(operation_id) = &operation_id {
            validate_operation_id(operation_id)?;
            if let Some((id, stream)) = self.find_operation(caller, operation_id) {
                if stream.output != output || stream.format != format {
                    return Err(CaptureError::new(
                        "operation_conflict",
                        "operation ID already selected a different output",
                    ));
                }
                return Ok(OpenStart::Ready(open_reply(
                    id,
                    &stream.output,
                    stream.geometry,
                    stream.touch_surface,
                )));
            }
            if let Some(job) = self
                .opening
                .values()
                .find(|job| job.owner == caller && job.operation_id.as_ref() == Some(operation_id))
            {
                if job.output != output || job.format != format {
                    return Err(CaptureError::new(
                        "operation_conflict",
                        "operation ID already selected a different output",
                    ));
                }
                return Err(CaptureError::new(
                    "cleanup_pending",
                    "capture operation is still opening",
                ));
            }
            if self.retiring.values().any(|entry| {
                entry.owner == caller && entry.operation_id.as_ref() == Some(operation_id)
            }) {
                return Err(CaptureError::new(
                    "cleanup_pending",
                    "capture operation is still retiring",
                ));
            }
        }
        if !self.backend.supported() {
            return Err(CaptureError::new(
                "unsupported_platform",
                "DXGI capture requires Windows",
            ));
        }
        if self.streams.len() + self.retiring.len() + self.opening.len() >= MAX_STREAMS {
            return Err(CaptureError::new(
                "resource_exhausted",
                "at most four capture streams may be open",
            ));
        }
        let id = self.next_stream_id()?;
        let backend = self.backend.clone();
        let selected_output = output.clone();
        #[cfg(target_os = "linux")]
        let tid = Arc::new(std::sync::atomic::AtomicU32::new(0));
        #[cfg(target_os = "linux")]
        let worker_tid = tid.clone();
        let worker = std::thread::Builder::new()
            .name("dxgi-open".into())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                {
                    let id = std::fs::read_link("/proc/thread-self")
                        .ok()
                        .and_then(|path| path.file_name()?.to_str()?.parse::<u32>().ok())
                        .expect("observe startup thread identity");
                    worker_tid.store(id, Ordering::Release);
                }
                prepare_capture(backend.as_ref(), &selected_output, format)
            })
            .map_err(|error| {
                CaptureError::new(
                    "capture_unavailable",
                    format!("unable to start capture worker: {error}"),
                )
            })?;
        self.opening.insert(
            id,
            Opening {
                owner: caller.into(),
                output,
                operation_id,
                format,
                cancelled: false,
                worker,
                #[cfg(target_os = "linux")]
                tid,
            },
        );
        Ok(OpenStart::Pending(id))
    }

    pub(crate) fn operation_pending(&self, caller: &str, operation_id: &str) -> bool {
        self.opening
            .values()
            .any(|job| job.owner == caller && job.operation_id.as_deref() == Some(operation_id))
            || self
                .retiring
                .values()
                .any(|job| job.owner == caller && job.operation_id.as_deref() == Some(operation_id))
    }

    /// Snapshot the exact operation identity for an owned Close target. The
    /// outer option distinguishes a missing target from a stream opened
    /// without an idempotency key.
    pub(crate) fn close_target(&self, caller: &str, id: u64) -> Option<Option<String>> {
        if let Some(stream) = self
            .streams
            .get(&id)
            .filter(|stream| stream.owner == caller)
        {
            return Some(stream.operation_id.clone());
        }
        if let Some(job) = self.opening.get(&id).filter(|job| job.owner == caller) {
            return Some(job.operation_id.clone());
        }
        self.retiring
            .get(&id)
            .filter(|job| job.owner == caller)
            .map(|job| job.operation_id.clone())
    }

    pub(crate) fn cancel_open(&mut self, id: u64) {
        if let Some(job) = self.opening.get_mut(&id) {
            job.cancelled = true;
        }
    }

    pub(crate) fn abandon_open(&mut self, caller: &str, id: u64) -> CaptureResult<CloseReceipt> {
        self.begin_close(caller, id)
    }

    pub(crate) fn poll_open(
        &mut self,
        id: u64,
        commit_allowed: bool,
    ) -> CaptureResult<Option<(Value, Option<u64>)>> {
        #[cfg(test)]
        if self.test_open_observation_failures > 0 {
            self.test_open_observation_failures -= 1;
            return Err(CaptureError::new(
                "retirement_failed",
                "injected startup observation failure",
            ));
        }
        let Some(job) = self.opening.get(&id) else {
            return Ok(Some((
                CaptureError::new("cancelled", "capture startup no longer exists").payload(),
                None,
            )));
        };
        match worker_finished(&job.worker, opening_tid(job)) {
            Ok(false) => return Ok(None),
            // A failed native wait does not prove termination. Keep this
            // exact job and its waiter so a later observation can recover.
            Err(error) => return Err(error),
            Ok(true) => (),
        }
        let job = self.opening.remove(&id).expect("finished startup");
        let prepared = match job.worker.join() {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => return Ok(Some((error.payload(), None))),
            Err(_) => {
                self.startup_retirement_uncertain = true;
                return Ok(Some((
                    CaptureError::new("retirement_failed", "capture startup worker panicked")
                        .payload(),
                    None,
                )));
            }
        };
        let reply = open_reply(id, &job.output, prepared.geometry, prepared.touch_surface);
        let cancelled =
            job.cancelled || !commit_allowed || !self.enabled || prepared.failure.is_some();
        let failure = prepared.failure;
        self.streams.insert(
            id,
            Stream {
                owner: job.owner.clone(),
                output: job.output,
                session: prepared.session,
                geometry: prepared.geometry,
                touch_surface: prepared.touch_surface,
                operation_id: job.operation_id,
                format: job.format,
            },
        );
        if cancelled {
            let result = self.begin_close(&job.owner, id);
            if let Err(error) = result {
                return Ok(Some((error.payload(), None)));
            }
            Ok(Some((
                failure
                    .unwrap_or_else(|| {
                        CaptureError::new("cancelled", "capture startup was cancelled")
                    })
                    .payload(),
                None,
            )))
        } else {
            Ok(Some((reply, Some(id))))
        }
    }

    pub fn handle(
        &mut self,
        caller: &str,
        operation: Operation,
        guard: &mut impl FnMut() -> CaptureResult<()>,
    ) -> Value {
        self.opened_stream = None;
        let result = match operation {
            Operation::Status => Ok(self.status(caller)),
            Operation::Enumerate { offset, limit } => self
                .backend
                .enumerate()
                .and_then(|outputs| enumeration_reply(outputs, offset, limit)),
            Operation::Open {
                output,
                operation_id,
                source_format,
            } => self.open(caller, &output, operation_id, source_format.as_ref(), guard),
            Operation::Lookup { operation_id } => self.lookup(caller, &operation_id),
            Operation::Close { stream } => self.close(caller, stream).map(|()| json!({"ok":true})),
        };
        result.unwrap_or_else(CaptureError::payload)
    }

    fn open(
        &mut self,
        caller: &str,
        output: &str,
        operation_id: Option<String>,
        source_format: Option<&amaterasu_module_sdk::capture::SourceFormat>,
        guard: &mut impl FnMut() -> CaptureResult<()>,
    ) -> CaptureResult<Value> {
        let format = crate::format::CaptureFormat::requested(source_format)?;
        if !self.enabled {
            return Err(CaptureError::new(
                "capture_disabled",
                "capture requires enabled startup policy",
            ));
        }
        if let Some(operation_id) = &operation_id {
            validate_operation_id(operation_id)?;
            if self.retiring.values().any(|entry| {
                entry.owner == caller && entry.operation_id.as_ref() == Some(operation_id)
            }) {
                return Err(CaptureError::new(
                    "cleanup_pending",
                    "the operation's capture stream is still retiring",
                ));
            }
            if let Some((stream_id, existing)) = self.find_operation(caller, operation_id) {
                if existing.output != output || existing.format != format {
                    return Err(CaptureError::new(
                        "operation_conflict",
                        "operation ID already selected a different output",
                    ));
                }
                return Ok(open_reply(
                    stream_id,
                    &existing.output,
                    existing.geometry,
                    existing.touch_surface,
                ));
            }
        }
        if self.streams.len() + self.retiring.len() + self.opening.len() >= MAX_STREAMS {
            return Err(CaptureError::new(
                "resource_exhausted",
                "at most four capture streams may be open",
            ));
        }
        self.backend.verify_source_format(output, format)?;
        let id = self.next_stream_id()?;
        guard()?;
        let mut session = self.backend.open_with_format(output, format)?;
        let geometry = initial_geometry(&mut *session)?;
        let touch_surface = initial_touch_surface(&mut *session, geometry)?;
        // Opening may block in a driver. Recheck before retaining resources.
        guard()?;
        self.streams.insert(
            id,
            Stream {
                owner: caller.into(),
                output: output.into(),
                session,
                geometry,
                touch_surface,
                operation_id,
                format,
            },
        );
        self.opened_stream = Some(id);
        Ok(open_reply(id, output, geometry, touch_surface))
    }

    fn status(&self, caller: &str) -> Value {
        let streams: Vec<_> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.owner == caller)
            .map(|(id, stream)| {
                let mut reply =
                    stream_reply(*id, &stream.output, stream.geometry, stream.touch_surface);
                reply["device_adapter"] = stream.session.device_adapter();
                reply
            })
            .collect();
        json!({
            "ok": true,
            "enabled": self.enabled,
            "native_supported": self.backend.supported(),
            "streams": streams,
        })
    }

    fn next_stream_id(&self) -> CaptureResult<u64> {
        let mut random = [0; 8];
        getrandom::fill(&mut random)
            .map_err(|error| CaptureError::new("entropy_error", error.to_string()))?;
        self.validate_stream_id(u64::from_le_bytes(random))
    }

    pub(crate) fn validate_stream_id(&self, id: u64) -> CaptureResult<u64> {
        if self.streams.contains_key(&id)
            || self.retiring.contains_key(&id)
            || self.opening.contains_key(&id)
            || self.close_id_reservations.contains(&id)
        {
            return Err(CaptureError::new(
                "entropy_error",
                "capture stream ID collision; retry open",
            ));
        }
        Ok(id)
    }

    fn find_operation(&self, caller: &str, operation_id: &str) -> Option<(u64, &Stream)> {
        self.streams
            .iter()
            .find(|(_, stream)| {
                stream.owner == caller && stream.operation_id.as_deref() == Some(operation_id)
            })
            .map(|(id, stream)| (*id, stream))
    }

    fn lookup(&self, caller: &str, operation_id: &str) -> CaptureResult<Value> {
        validate_operation_id(operation_id)?;
        self.find_operation(caller, operation_id)
            .map(|(id, stream)| {
                open_reply(id, &stream.output, stream.geometry, stream.touch_surface)
            })
            .ok_or_else(|| {
                CaptureError::new("not_found", "no capture stream owned by this operation")
            })
    }

    pub fn close(&mut self, caller: &str, id: u64) -> CaptureResult<()> {
        let receipt = self.begin_close(caller, id)?;
        if let Some((_, error)) = self
            .poll_closing()
            .into_iter()
            .find(|(stream, _)| *stream == id)
        {
            return Err(error);
        }
        if receipt.retired() {
            Ok(())
        } else {
            Err(CaptureError::new(
                "cleanup_pending",
                "capture session is still retiring",
            ))
        }
    }

    pub(crate) fn begin_close(&mut self, caller: &str, id: u64) -> CaptureResult<CloseReceipt> {
        if let Some(retirement) = self.retiring.get(&id) {
            return if retirement.owner == caller {
                Ok(retirement.receipt.clone())
            } else {
                Err(CaptureError::new(
                    "not_found",
                    "no capture stream owned by this caller",
                ))
            };
        }
        self.authorize(caller, id)?;
        let stream = self.streams.remove(&id).expect("authorized stream");
        let receipt = CloseReceipt(Arc::new(AtomicBool::new(false)));
        self.retiring.insert(
            id,
            Retirement {
                owner: stream.owner.clone(),
                operation_id: stream.operation_id.clone(),
                stream: Arc::new(Mutex::new(Some(stream))),
                worker: None,
                receipt: receipt.clone(),
                failure: None,
                #[cfg(target_os = "linux")]
                tid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            },
        );
        // Ownership has already transferred to the retained retirement. Even
        // if thread creation fails, return its exact receipt; later polling
        // retries startup and reports any failure without losing admission.
        let _ = self.start_retirement(id);
        Ok(receipt)
    }

    fn start_retirement(&mut self, id: u64) -> CaptureResult<()> {
        let retirement = self.retiring.get_mut(&id).expect("retirement exists");
        if retirement.worker.is_some() || retirement.failure.is_some() {
            return Ok(());
        }
        let stream = retirement.stream.clone();
        #[cfg(target_os = "linux")]
        let tid = retirement.tid.clone();
        let worker = std::thread::Builder::new()
            .name("dxgi-retire".into())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                {
                    let id = std::fs::read_link("/proc/thread-self")
                        .ok()
                        .and_then(|path| path.file_name()?.to_str()?.parse::<u32>().ok())
                        .expect("observe retirement thread identity");
                    tid.store(id, Ordering::Release);
                }
                let owned = stream.lock().expect("retirement ownership poisoned").take();
                drop(owned);
            })
            .map_err(|error| CaptureError::new("retirement_unavailable", error.to_string()))?;
        retirement.worker = Some(worker);
        Ok(())
    }

    pub(crate) fn poll_closing(&mut self) -> Vec<(u64, CaptureError)> {
        let ids: Vec<_> = self.retiring.keys().copied().collect();
        let mut errors = Vec::new();
        for id in ids {
            if let Err(error) = self.start_retirement(id) {
                errors.push((id, error));
                continue;
            }
            let entry = self.retiring.get_mut(&id).expect("retirement exists");
            let finished = match entry
                .worker
                .as_ref()
                .map(|worker| worker_finished(worker, retirement_tid(entry)))
                .transpose()
            {
                Ok(finished) => finished.unwrap_or(false),
                Err(error) => {
                    errors.push((id, error));
                    continue;
                }
            };
            if finished {
                if entry.worker.take().expect("finished worker").join().is_ok() {
                    entry.receipt.0.store(true, Ordering::Release);
                    self.retiring.remove(&id);
                    continue;
                }
                entry.failure = Some("capture retirement worker panicked".into());
            }
            if let Some(message) = &entry.failure {
                errors.push((id, CaptureError::new("retirement_failed", message.clone())));
            }
        }
        errors
    }

    fn authorize(&self, caller: &str, id: u64) -> CaptureResult<()> {
        match self.streams.get(&id) {
            Some(stream) if stream.owner == caller => Ok(()),
            _ => Err(CaptureError::new(
                "not_found",
                "no capture stream owned by this caller",
            )),
        }
    }

    pub fn read(&mut self, caller: &str, id: u64, wait_ms: u32) -> CaptureResult<Option<Frame>> {
        self.authorize(caller, id)?;
        if wait_ms > MAX_WAIT_MS {
            return Err(CaptureError::new(
                "invalid_request",
                "frame wait must be at most 16 ms",
            ));
        }
        let result = self
            .streams
            .get_mut(&id)
            .expect("authorized stream")
            .session
            .read(wait_ms);
        if result.as_ref().is_err_and(|error| {
            matches!(
                error.code,
                "access_lost"
                    | "device_lost"
                    | "source_color_unavailable"
                    | "source_range_mismatch"
                    | "unsupported_format"
            )
        }) {
            let _ = self.begin_close(caller, id);
        }
        let frame = result?;
        if frame.is_some()
            && self
                .streams
                .get(&id)
                .is_some_and(|stream| stream.geometry.is_some())
        {
            // A successful read can follow a same-size output move. Verify the
            // published coordinates before exposing any pixels to the caller.
            self.geometry(caller, id)?;
        }
        Ok(frame)
    }

    pub fn source_info(
        &mut self,
        caller: &str,
        id: u64,
    ) -> CaptureResult<Option<crate::format::SourceInfo>> {
        self.authorize(caller, id)?;
        let stream = self.streams.get_mut(&id).expect("authorized stream");
        let result = stream.session.source_info().and_then(|info| {
            if let Some(info) = info
                && (info.format != stream.format
                    || (info.format.ten_bit
                        && info.encoding
                            == amaterasu_module_sdk::capture::high_depth::RgbEncoding::Bgra8Srgb))
            {
                return Err(CaptureError::new(
                    "access_lost",
                    "native source no longer matches the admitted source format",
                ));
            }
            Ok(info)
        });
        if result.as_ref().is_err_and(|e| {
            matches!(
                e.code,
                "access_lost"
                    | "device_lost"
                    | "source_color_unavailable"
                    | "source_range_mismatch"
                    | "unsupported_format"
            )
        }) {
            let _ = self.begin_close(caller, id);
        }
        result
    }
    pub fn capture_format(
        &self,
        caller: &str,
        id: u64,
    ) -> CaptureResult<crate::format::CaptureFormat> {
        self.authorize(caller, id)?;
        Ok(self.streams.get(&id).expect("authorized stream").format)
    }

    pub fn geometry(&mut self, caller: &str, id: u64) -> CaptureResult<Option<CaptureGeometry>> {
        self.authorize(caller, id)?;
        let stream = self.streams.get_mut(&id).expect("authorized stream");
        let current = stream.session.geometry();
        let geometry_lost = match &current {
            Ok(geometry) => *geometry != stream.geometry,
            Err(error) => {
                stream.geometry.is_some()
                    || error.code == "access_lost"
                    || error.code == "device_lost"
            }
        };
        if geometry_lost {
            let _ = self.begin_close(caller, id);
            return Err(CaptureError::new(
                "access_lost",
                "capture desktop geometry changed or became unavailable; enumerate and reopen",
            ));
        }
        current
    }
}

impl<B: Backend> Drop for Controller<B> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(windows)]
fn worker_finished<T>(
    worker: &JoinHandle<T>,
    _: Option<&std::sync::atomic::AtomicU32>,
) -> CaptureResult<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::WaitForSingleObject;

    // SAFETY: the JoinHandle owns this live thread handle for the duration of
    // the zero-time wait. A signaled thread has completed its TLS destructors,
    // so subsequently joining it cannot wait on provider thread execution.
    match unsafe { WaitForSingleObject(HANDLE(worker.as_raw_handle()), 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(CaptureError::new(
            "retirement_failed",
            "could not observe capture retirement thread",
        )),
    }
}

#[cfg(target_os = "linux")]
fn worker_finished<T>(
    worker: &JoinHandle<T>,
    tid: Option<&std::sync::atomic::AtomicU32>,
) -> CaptureResult<bool> {
    if !worker.is_finished() {
        return Ok(false);
    }
    let tid = tid.expect("Linux thread identity").load(Ordering::Acquire);
    if tid == 0 {
        // Initialization can fail before the thread publishes its identity.
        // The owned stream remains in its slot and retirement must fail closed.
        return Err(CaptureError::new(
            "retirement_failed",
            "capture worker did not publish thread identity",
        ));
    }
    // Verify that procfs and this process's task directory remain observable
    // before interpreting an absent worker TID as termination. A reused TID
    // only delays retirement until the unrelated thread also exits.
    let own_task = std::fs::read_link("/proc/thread-self").map_err(|error| {
        CaptureError::new(
            "retirement_failed",
            format!("cannot observe current task: {error}"),
        )
    })?;
    if !std::path::Path::new("/proc").join(own_task).is_dir() {
        return Err(CaptureError::new(
            "retirement_failed",
            "current task is not observable",
        ));
    }
    std::path::Path::new(&format!("/proc/self/task/{tid}"))
        .try_exists()
        .map(|exists| !exists)
        .map_err(|error| {
            CaptureError::new(
                "retirement_failed",
                format!("could not observe capture retirement thread: {error}"),
            )
        })
}

#[cfg(not(any(windows, target_os = "linux")))]
fn worker_finished<T>(
    worker: &JoinHandle<T>,
    _: Option<&std::sync::atomic::AtomicU32>,
) -> CaptureResult<bool> {
    // Native DXGI is unsupported here; this supports portable fixtures only.
    Ok(worker.is_finished())
}

fn retirement_tid(entry: &Retirement) -> Option<&std::sync::atomic::AtomicU32> {
    #[cfg(target_os = "linux")]
    {
        Some(&entry.tid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = entry;
        None
    }
}

fn opening_tid(entry: &Opening) -> Option<&std::sync::atomic::AtomicU32> {
    #[cfg(target_os = "linux")]
    {
        Some(&entry.tid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = entry;
        None
    }
}

fn prepare_capture<B: Backend>(
    backend: &B,
    output: &str,
    format: crate::format::CaptureFormat,
) -> CaptureResult<PreparedCapture> {
    backend.verify_source_format(output, format)?;
    let mut session = backend.open_with_format(output, format)?;
    let result = initial_geometry(&mut *session).and_then(|geometry| {
        initial_touch_surface(&mut *session, geometry).map(|surface| (geometry, surface))
    });
    let (geometry, touch_surface, failure) = match result {
        Ok((geometry, surface)) => (geometry, surface, None),
        Err(error) => (None, None, Some(error)),
    };
    Ok(PreparedCapture {
        session,
        geometry,
        touch_surface,
        failure,
    })
}

fn initial_geometry(session: &mut dyn CaptureSession) -> CaptureResult<Option<CaptureGeometry>> {
    match session.geometry() {
        Err(error) if error.code == "geometry_unavailable" => Ok(None),
        Ok(Some(geometry)) if geometry.validate().is_err() => Err(CaptureError::new(
            "geometry_unavailable",
            "capture backend returned invalid desktop geometry",
        )),
        result => result,
    }
}

fn initial_touch_surface(
    session: &mut dyn CaptureSession,
    geometry: Option<CaptureGeometry>,
) -> CaptureResult<Option<TouchSurface>> {
    let Some(geometry) = geometry else {
        return Ok(None);
    };
    match session.touch_surface() {
        Err(error) if error.code == "geometry_unavailable" => Ok(None),
        Err(error) => Err(error),
        Ok(Some(surface)) if !surface.valid() => Err(CaptureError::new(
            "geometry_unavailable",
            "capture backend returned invalid touch surface",
        )),
        Ok(Some(surface)) => {
            if surface.width != geometry.output.width || surface.height != geometry.output.height {
                return Ok(None);
            }
            // Providers may query topology independently for each value. A
            // moved output can retain its dimensions, so verify the complete
            // rectangle after obtaining the touch surface.
            if initial_geometry(session)? == Some(geometry) {
                Ok(Some(surface))
            } else {
                Ok(None)
            }
        }
        Ok(None) => Ok(None),
    }
}

fn validate_operation_id(operation_id: &str) -> CaptureResult<()> {
    if operation_id.is_empty() || operation_id.len() > 128 {
        return Err(CaptureError::new(
            "invalid_request",
            "operation ID must contain 1..128 UTF-8 bytes",
        ));
    }
    Ok(())
}

pub(crate) fn enumeration_reply(
    mut outputs: Vec<Output>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> CaptureResult<Value> {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(MAX_ENUMERATE_LIMIT);
    if limit == 0 || limit > MAX_ENUMERATE_LIMIT {
        return Err(CaptureError::new(
            "invalid_request",
            "enumeration limit must be within 1..=256",
        ));
    }
    outputs.sort_by(|left, right| left.id.cmp(&right.id));
    if outputs.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(CaptureError::new(
            "backend_error",
            "capture backend returned duplicate output identities",
        ));
    }
    let full_list = serde_json::to_vec(&outputs)
        .map_err(|error| CaptureError::new("backend_error", error.to_string()))?;
    let enumeration_id = format!("{:x}", Sha256::digest(&full_list));
    if offset > outputs.len() || (offset == outputs.len() && offset != 0) {
        return Err(CaptureError::new(
            "invalid_request",
            "enumeration offset is outside the current output list",
        ));
    }

    let total_outputs = outputs.len();
    let mut page = Vec::new();
    for output in outputs.into_iter().skip(offset).take(limit) {
        page.push(output);
        let next_offset = offset + page.len();
        // Reserve the worst-case cursor overhead so adding it cannot overflow
        // the control reply after the page has been selected.
        let encoded =
            serde_json::to_vec(&json!({"ok":true,"outputs":page,"next_offset":next_offset,"enumeration_id":enumeration_id}))
                .map_err(|error| CaptureError::new("backend_error", error.to_string()))?;
        if encoded.len() > 12_000 {
            page.pop();
            if page.is_empty() {
                return Err(CaptureError::new(
                    "resource_exhausted",
                    "one output exceeds the control reply limit",
                ));
            }
            break;
        }
    }
    let next_offset = offset + page.len();
    let reply = if next_offset < total_outputs {
        json!({"ok":true,"outputs":page,"next_offset":next_offset,"enumeration_id":enumeration_id})
    } else {
        json!({"ok":true,"outputs":page,"enumeration_id":enumeration_id})
    };
    Ok(reply)
}

fn open_reply(
    stream_id: u64,
    output: &str,
    geometry: Option<CaptureGeometry>,
    touch_surface: Option<TouchSurface>,
) -> Value {
    let mut reply = stream_reply(stream_id, output, geometry, touch_surface);
    reply["ok"] = json!(true);
    reply
}

fn stream_reply(
    stream_id: u64,
    output: &str,
    geometry: Option<CaptureGeometry>,
    touch_surface: Option<TouchSurface>,
) -> Value {
    let mut reply = json!({ "stream": stream_id, "output": output });
    if let Some(geometry) = geometry {
        reply["desktop_geometry"] = json!(geometry);
    }
    if let Some(touch_surface) = touch_surface {
        reply["touch_surface"] = json!(touch_surface);
    }
    reply
}
