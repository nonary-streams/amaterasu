use crate::backend::{Backend, CaptureSession, TouchSurface};
use crate::model::{
    CaptureError, CaptureResult, Frame, FrameDelivery, MAX_STREAMS, MAX_WAIT_MS, Operation,
};
use amaterasu_media::CaptureGeometry;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod shutdown_tests;

#[cfg(test)]
#[path = "controller_async_tests.rs"]
mod controller_async_tests;

struct Stream {
    capture_format: crate::format::CaptureFormat,
    owner: String,
    output: String,
    session: Box<dyn CaptureSession>,
    geometry: Option<CaptureGeometry>,
    touch_surface: Option<TouchSurface>,
    operation_id: Option<String>,
    constant_rate: bool,
    opened_at: Instant,
    last_frame: Option<Frame>,
    next_sequence: u64,
    last_timestamp_us: u64,
    close_receipt: Option<CloseReceipt>,
    unpublished: bool,
}

#[derive(Clone)]
pub(crate) struct CloseReceipt(Arc<AtomicBool>);

impl CloseReceipt {
    pub(crate) fn retired(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl Stream {
    fn stamp(&mut self, frame: &mut Frame) -> CaptureResult<()> {
        let sequence = self.next_sequence.checked_add(1).ok_or_else(|| {
            CaptureError::new("sequence_exhausted", "capture frame sequence exhausted")
        })?;
        let next_timestamp_us = self.last_timestamp_us.checked_add(1).ok_or_else(|| {
            CaptureError::new("timestamp_exhausted", "capture frame timestamp exhausted")
        })?;
        let elapsed_us = self.opened_at.elapsed().as_micros().min(u64::MAX as u128) as u64;
        let timestamp_us = elapsed_us.max(next_timestamp_us);
        self.next_sequence = sequence;
        self.last_timestamp_us = timestamp_us;
        // Keep capture_pacing_timestamp_us anchored to WGC acquisition time;
        // this timestamp and sequence belong to the controller's output clock.
        frame.sequence = sequence;
        frame.timestamp_us = timestamp_us;
        Ok(())
    }
}

pub struct Controller<B: Backend> {
    backend: Arc<B>,
    enabled: bool,
    streams: BTreeMap<u64, Stream>,
    opening: BTreeMap<u64, Opening>,
    startup_retirement_uncertain: bool,
    opened_stream: Option<u64>,
}

pub(crate) enum OpenStart {
    Ready(Value),
    Pending(u64),
}

struct Opening {
    owner: String,
    output: String,
    operation_id: Option<String>,
    constant_rate: bool,
    capture_format: crate::format::CaptureFormat,
    cancelled: bool,
    worker: JoinHandle<CaptureResult<PreparedCapture>>,
}

struct PreparedCapture {
    session: Box<dyn CaptureSession>,
    geometry: Option<CaptureGeometry>,
    touch_surface: Option<TouchSurface>,
    failure: Option<CaptureError>,
}

impl<B: Backend> Controller<B> {
    pub(crate) fn backend(&self) -> Arc<B> {
        Arc::clone(&self.backend)
    }
    pub(crate) fn source_preflight(
        &self,
        caller: &str,
        request: &amaterasu_module_sdk::capture::SourcePreflightRequest,
    ) -> Result<
        amaterasu_module_sdk::capture::SourcePreflightResponse,
        amaterasu_module_sdk::abi::Status,
    > {
        crate::preflight::query(
            self.backend.as_ref(),
            self.enabled && !self.startup_retirement_uncertain,
            self.streams.len() + self.opening.len() < MAX_STREAMS,
            caller,
            request,
        )
    }
    pub fn new(backend: B, enabled: bool) -> Self {
        Self {
            backend: Arc::new(backend),
            enabled,
            streams: BTreeMap::new(),
            opening: BTreeMap::new(),
            startup_retirement_uncertain: false,
            opened_stream: None,
        }
    }

    pub fn shutdown(&mut self) {
        self.stop_admission();
        for opening in self.opening.values_mut() {
            opening.cancelled = true;
        }
        self.streams.clear();
    }

    pub fn stop_admission(&mut self) {
        self.enabled = false;
    }

    pub fn quiesce(&mut self) -> CaptureResult<bool> {
        self.stop_admission();
        let openings: Vec<_> = self.opening.keys().copied().collect();
        for id in openings {
            self.cancel_open(id);
            let _ = self.poll_open(id, false);
        }
        for stream in self.streams.values_mut() {
            mark_closing(stream);
        }
        if let Some((_, failure)) = self.poll_closing().into_iter().next() {
            return Err(failure);
        }
        if self.startup_retirement_uncertain {
            return Err(CaptureError::new(
                "cleanup_failed",
                "capture startup retirement cannot be proven",
            ));
        }
        Ok(self.opening.is_empty() && self.streams.is_empty())
    }

    pub(crate) fn begin_open(
        &mut self,
        caller: &str,
        output: String,
        operation_id: Option<String>,
        constant_rate: bool,
        source_format: Option<amaterasu_module_sdk::capture::SourceFormat>,
    ) -> CaptureResult<OpenStart> {
        let capture_format = crate::format::CaptureFormat::requested(source_format.as_ref())?;
        if !self.enabled {
            return Err(CaptureError::new(
                "capture_disabled",
                "capture requires enabled startup policy",
            ));
        }
        if self.startup_retirement_uncertain {
            return Err(CaptureError::new(
                "cleanup_failed",
                "capture startup retirement cannot be proven",
            ));
        }
        if let Some(operation_id) = &operation_id {
            validate_operation_id(operation_id)?;
            if let Some((id, stream)) = self.find_operation(caller, operation_id) {
                if stream.output != output
                    || stream.constant_rate != constant_rate
                    || stream.capture_format != capture_format
                {
                    return Err(CaptureError::new(
                        "operation_conflict",
                        "operation ID already selected a different output or delivery mode",
                    ));
                }
                if stream.close_receipt.is_some() {
                    return Err(CaptureError::new(
                        "cleanup_pending",
                        "the operation's capture stream is still retiring",
                    ));
                }
                return Ok(OpenStart::Ready(open_reply(
                    id,
                    &stream.output,
                    stream.geometry,
                    stream.touch_surface,
                    stream.constant_rate,
                )));
            }
            if let Some(existing) = self
                .opening
                .values()
                .find(|job| job.owner == caller && job.operation_id.as_ref() == Some(operation_id))
            {
                if existing.output != output
                    || existing.constant_rate != constant_rate
                    || existing.capture_format != capture_format
                {
                    return Err(CaptureError::new(
                        "operation_conflict",
                        "operation ID already selected a different output or delivery mode",
                    ));
                }
                return Err(CaptureError::new(
                    "cleanup_pending",
                    "the operation's capture stream is still opening",
                ));
            }
        }
        if self.streams.len() + self.opening.len() >= MAX_STREAMS {
            return Err(CaptureError::new(
                "resource_exhausted",
                "at most four capture streams may be open",
            ));
        }
        let id = self.next_stream_id()?;
        let backend = Arc::clone(&self.backend);
        let worker_output = output.clone();
        let worker = std::thread::Builder::new()
            .name("wgc-open".into())
            .spawn(move || prepare_capture(backend.as_ref(), &worker_output, capture_format))
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
                constant_rate,
                capture_format,
                cancelled: false,
                worker,
            },
        );
        Ok(OpenStart::Pending(id))
    }

    pub(crate) fn operation_pending(&self, caller: &str, operation_id: &str) -> bool {
        self.opening
            .values()
            .any(|job| job.owner == caller && job.operation_id.as_deref() == Some(operation_id))
            || self
                .find_operation(caller, operation_id)
                .is_some_and(|(_, stream)| stream.unpublished && stream.close_receipt.is_some())
    }

    pub(crate) fn cancel_open(&mut self, id: u64) {
        if let Some(job) = self.opening.get_mut(&id) {
            job.cancelled = true;
        }
    }

    pub(crate) fn poll_open(
        &mut self,
        id: u64,
        commit_allowed: bool,
    ) -> Option<(Value, Option<u64>)> {
        let Some(job) = self.opening.get(&id) else {
            return Some((
                CaptureError::new("cancelled", "capture startup no longer exists").payload(),
                None,
            ));
        };
        match crate::worker::thread_finished(&job.worker) {
            Ok(false) => return None,
            Ok(true) => (),
            Err(error) => {
                self.startup_retirement_uncertain = true;
                return Some((error.payload(), None));
            }
        }
        let job = self.opening.remove(&id).expect("finished opening");
        let prepared = match job.worker.join() {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => {
                if error.code == "retirement_uncertain" {
                    self.startup_retirement_uncertain = true;
                }
                return Some((error.payload(), None));
            }
            Err(_) => {
                self.startup_retirement_uncertain = true;
                return Some((
                    CaptureError::new("capture_unavailable", "capture startup worker panicked")
                        .payload(),
                    None,
                ));
            }
        };
        let reply = open_reply(
            id,
            &job.output,
            prepared.geometry,
            prepared.touch_surface,
            job.constant_rate,
        );
        let cancelled = job.cancelled
            || !commit_allowed
            || !self.enabled
            || self.startup_retirement_uncertain
            || prepared.failure.is_some();
        let failure = prepared.failure;
        let mut stream = Stream {
            capture_format: job.capture_format,
            owner: job.owner,
            output: job.output,
            session: prepared.session,
            geometry: prepared.geometry,
            touch_surface: prepared.touch_surface,
            operation_id: job.operation_id,
            constant_rate: job.constant_rate,
            opened_at: Instant::now(),
            last_frame: None,
            next_sequence: 0,
            last_timestamp_us: 0,
            close_receipt: None,
            unpublished: cancelled,
        };
        if cancelled {
            mark_closing(&mut stream);
        }
        self.streams.insert(id, stream);
        if let Some(error) = failure {
            Some((error.payload(), None))
        } else if cancelled {
            Some((
                CaptureError::new("cancelled", "capture startup was cancelled").payload(),
                None,
            ))
        } else {
            Some((reply, Some(id)))
        }
    }

    pub(crate) fn begin_close(&mut self, caller: &str, id: u64) -> CaptureResult<CloseReceipt> {
        self.authorize_owner(caller, id)?;
        let stream = self.streams.get_mut(&id).expect("authorized stream");
        Ok(mark_closing(stream))
    }

    pub(crate) fn abandon_open(&mut self, caller: &str, id: u64) -> CaptureResult<CloseReceipt> {
        self.authorize_owner(caller, id)?;
        let stream = self.streams.get_mut(&id).expect("authorized stream");
        stream.unpublished = true;
        Ok(mark_closing(stream))
    }

    pub(crate) fn poll_closing(&mut self) -> Vec<(u64, CaptureError)> {
        let mut failures = Vec::new();
        let mut retired = Vec::new();
        // Poll every owned source so one pending or failed worker does not
        // prevent independent streams from retiring on the same host tick.
        for (&id, stream) in &mut self.streams {
            let Some(receipt) = &stream.close_receipt else {
                continue;
            };
            match stream.session.quiesce() {
                Ok(true) => retired.push((id, receipt.clone())),
                Ok(false) => (),
                Err(error) => failures.push((id, error)),
            }
        }
        for (id, receipt) in retired {
            let stream = self.streams.remove(&id).expect("retiring stream");
            // A joined worker alone is insufficient: destroy its complete
            // session before any broker waiter can report successful Close.
            drop(stream);
            receipt.0.store(true, Ordering::Release);
        }
        failures
    }

    #[cfg(test)]
    pub fn take_opened_stream(&mut self) -> Option<u64> {
        self.opened_stream.take()
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
                constant_rate,
                source_format,
            } => {
                let format = match crate::format::CaptureFormat::requested(source_format.as_ref()) {
                    Ok(format) => format,
                    Err(error) => return error.payload(),
                };
                self.open(caller, &output, operation_id, constant_rate, format, guard)
            }
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
        constant_rate: bool,
        capture_format: crate::format::CaptureFormat,
        guard: &mut impl FnMut() -> CaptureResult<()>,
    ) -> CaptureResult<Value> {
        if !self.enabled {
            return Err(CaptureError::new(
                "capture_disabled",
                "capture requires enabled startup policy",
            ));
        }
        if self.startup_retirement_uncertain {
            return Err(CaptureError::new(
                "cleanup_failed",
                "capture startup retirement cannot be proven",
            ));
        }
        if let Some(operation_id) = &operation_id {
            validate_operation_id(operation_id)?;
            if let Some(existing) = self
                .opening
                .values()
                .find(|job| job.owner == caller && job.operation_id.as_ref() == Some(operation_id))
            {
                if existing.output != output
                    || existing.constant_rate != constant_rate
                    || existing.capture_format != capture_format
                {
                    return Err(CaptureError::new(
                        "operation_conflict",
                        "operation ID already selected a different output or delivery mode",
                    ));
                }
                return Err(CaptureError::new(
                    "cleanup_pending",
                    "the operation's capture stream is still opening",
                ));
            }
            if let Some((stream_id, existing)) = self.find_operation(caller, operation_id) {
                if existing.close_receipt.is_some() {
                    return Err(CaptureError::new(
                        "cleanup_pending",
                        "the operation's capture stream is still retiring",
                    ));
                }
                if existing.output != output
                    || existing.constant_rate != constant_rate
                    || existing.capture_format != capture_format
                {
                    return Err(CaptureError::new(
                        "operation_conflict",
                        "operation ID already selected a different output or delivery mode",
                    ));
                }
                return Ok(open_reply(
                    stream_id,
                    &existing.output,
                    existing.geometry,
                    existing.touch_surface,
                    existing.constant_rate,
                ));
            }
        }
        if self.streams.len() + self.opening.len() >= MAX_STREAMS {
            return Err(CaptureError::new(
                "resource_exhausted",
                "at most four capture streams may be open",
            ));
        }
        let id = self.next_stream_id()?;
        guard()?;
        let PreparedCapture {
            session,
            geometry,
            touch_surface,
            failure,
        } = prepare_capture(self.backend.as_ref(), output, capture_format)?;
        if let Some(error) = failure {
            return Err(error);
        }
        // Opening may block in a driver. Recheck before retaining resources.
        guard()?;
        self.streams.insert(
            id,
            Stream {
                capture_format,
                owner: caller.into(),
                output: output.into(),
                session,
                geometry,
                touch_surface,
                operation_id,
                constant_rate,
                opened_at: Instant::now(),
                last_frame: None,
                next_sequence: 0,
                last_timestamp_us: 0,
                close_receipt: None,
                unpublished: false,
            },
        );
        self.opened_stream = Some(id);
        Ok(open_reply(
            id,
            output,
            geometry,
            touch_surface,
            constant_rate,
        ))
    }

    fn status(&self, caller: &str) -> Value {
        let streams: Vec<_> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.owner == caller)
            .map(|(id, stream)| {
                let mut reply = stream_reply(
                    *id,
                    &stream.output,
                    stream.geometry,
                    stream.touch_surface,
                    stream.constant_rate,
                );
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
        let id = u64::from_le_bytes(random);
        if self.streams.contains_key(&id) || self.opening.contains_key(&id) {
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
        if self.startup_retirement_uncertain {
            return Err(CaptureError::new(
                "cleanup_failed",
                "capture startup retirement cannot be proven",
            ));
        }
        if self.operation_pending(caller, operation_id) {
            return Err(CaptureError::new(
                "cleanup_pending",
                "the operation's capture stream is still opening or retiring",
            ));
        }
        self.find_operation(caller, operation_id)
            .map(|(id, stream)| {
                open_reply(
                    id,
                    &stream.output,
                    stream.geometry,
                    stream.touch_surface,
                    stream.constant_rate,
                )
            })
            .ok_or_else(|| {
                CaptureError::new("not_found", "no capture stream owned by this operation")
            })
    }

    pub(crate) fn source_info(
        &mut self,
        caller: &str,
        id: u64,
    ) -> CaptureResult<(crate::format::CaptureFormat, crate::format::SourceInfo)> {
        self.authorize(caller, id)?;
        let result = (|| {
            let stream = self.streams.get_mut(&id).expect("authorized stream");
            stream.session.validate_source()?;
            let info = stream
                .session
                .source_info()?
                .or_else(|| {
                    if stream.capture_format.color_proof {
                        return None;
                    }
                    stream.geometry.map(|geometry| crate::format::SourceInfo {
                        width: geometry.output.width,
                        height: geometry.output.height,
                        encoding: stream.capture_format.encoding(),
                        desktop_hdr: false,
                    })
                })
                .ok_or_else(|| {
                    CaptureError::new(
                        "source_pending",
                        "capture source information is not yet available",
                    )
                })?;
            if info.encoding != stream.capture_format.encoding()
                || stream.capture_format.color_proof
                    && stream.capture_format.hdr != info.desktop_hdr
            {
                return Err(CaptureError::new(
                    "unsupported_format",
                    "the selected native source cannot preserve the requested format",
                ));
            }
            Ok((stream.capture_format, info))
        })();
        if result
            .as_ref()
            .is_err_and(|error: &CaptureError| matches!(error.code, "access_lost" | "device_lost"))
        {
            let _ = self.close(caller, id);
        }
        result
    }

    pub fn close(&mut self, caller: &str, id: u64) -> CaptureResult<()> {
        let receipt = self.begin_close(caller, id)?;
        if let Some((_, failure)) = self
            .poll_closing()
            .into_iter()
            .find(|(stream, _)| *stream == id)
        {
            return Err(failure);
        }
        if receipt.retired() {
            Ok(())
        } else {
            Err(CaptureError::new(
                "cleanup_pending",
                "capture worker is still retiring",
            ))
        }
    }

    fn authorize(&self, caller: &str, id: u64) -> CaptureResult<()> {
        self.authorize_owner(caller, id)?;
        if self
            .streams
            .get(&id)
            .expect("authorized stream")
            .close_receipt
            .is_some()
        {
            return Err(CaptureError::new(
                "access_lost",
                "capture stream is retiring",
            ));
        }
        Ok(())
    }

    fn authorize_owner(&self, caller: &str, id: u64) -> CaptureResult<()> {
        match self.streams.get(&id) {
            Some(stream) if stream.owner == caller => Ok(()),
            _ => Err(CaptureError::new(
                "not_found",
                "no capture stream owned by this caller",
            )),
        }
    }

    #[cfg(test)]
    pub fn read(&mut self, caller: &str, id: u64, wait_ms: u32) -> CaptureResult<Option<Frame>> {
        self.read_with_freshness(caller, id, wait_ms)
            .map(|delivery| delivery.map(|delivery| delivery.frame))
    }

    pub fn read_with_freshness(
        &mut self,
        caller: &str,
        id: u64,
        wait_ms: u32,
    ) -> CaptureResult<Option<FrameDelivery>> {
        self.authorize(caller, id)?;
        if wait_ms > MAX_WAIT_MS {
            self.streams
                .get_mut(&id)
                .expect("authorized stream")
                .last_frame = None;
            return Err(CaptureError::new(
                "invalid_request",
                "frame wait must be at most 16 ms",
            ));
        }
        let mut result = {
            let stream = self.streams.get_mut(&id).expect("authorized stream");
            match stream.session.read(wait_ms) {
                Ok(Some(mut frame)) if stream.constant_rate => match stream.stamp(&mut frame) {
                    Ok(()) => {
                        stream.last_frame = Some(frame.clone());
                        Ok(Some(FrameDelivery {
                            frame,
                            is_fresh: true,
                        }))
                    }
                    Err(error) => {
                        stream.last_frame = None;
                        Err(error)
                    }
                },
                Ok(None) if stream.constant_rate => {
                    let Some(mut frame) = stream.last_frame.clone() else {
                        return Ok(None);
                    };
                    match stream.stamp(&mut frame) {
                        Ok(()) => {
                            stream.last_frame = Some(frame.clone());
                            Ok(Some(FrameDelivery {
                                frame,
                                is_fresh: false,
                            }))
                        }
                        Err(error) => {
                            stream.last_frame = None;
                            Err(error)
                        }
                    }
                }
                Ok(frame) => Ok(frame.map(|frame| FrameDelivery {
                    frame,
                    is_fresh: true,
                })),
                Err(error) => {
                    stream.last_frame = None;
                    Err(error)
                }
            }
        };
        if let Ok(Some(delivery)) = &result {
            let stream = self.streams.get_mut(&id).expect("authorized stream");
            let source = stream.session.validate_source();
            if delivery.frame.encoding != stream.capture_format.encoding() || source.is_err() {
                stream.last_frame = None;
                result = Err(source.err().unwrap_or_else(|| {
                    CaptureError::new("access_lost", "capture frame layout changed after open")
                }));
            }
        }
        if result.as_ref().is_ok_and(Option::is_some)
            && self
                .streams
                .get(&id)
                .is_some_and(|stream| stream.geometry.is_some())
        {
            // The frame pool checks pixel dimensions, but an output can move on
            // the desktop without changing its size. Verify the Open snapshot
            // before publishing either a fresh frame or a cached repeat.
            let stream = self.streams.get_mut(&id).expect("authorized stream");
            let geometry_result = match stream.session.geometry() {
                Ok(current) if current == stream.geometry => Ok(()),
                Ok(_)
                | Err(CaptureError {
                    code: "geometry_unavailable",
                    ..
                }) => Err(CaptureError::new(
                    "access_lost",
                    "capture desktop geometry changed; enumerate and reopen",
                )),
                Err(error) => Err(error),
            };
            if let Err(error) = geometry_result {
                stream.last_frame = None;
                result = Err(error);
            }
        }
        if result
            .as_ref()
            .is_err_and(|error| error.code == "access_lost" || error.code == "device_lost")
        {
            let _ = self.close(caller, id);
        }
        result
    }

    pub fn discard_cached_frame(&mut self, caller: &str, id: u64) -> CaptureResult<()> {
        self.authorize(caller, id)?;
        self.streams
            .get_mut(&id)
            .expect("authorized stream")
            .last_frame = None;
        Ok(())
    }

    pub fn geometry(&mut self, caller: &str, id: u64) -> CaptureResult<Option<CaptureGeometry>> {
        self.authorize(caller, id)?;
        let stream = self.streams.get_mut(&id).expect("authorized stream");
        let current = stream.session.geometry();
        if current.is_err() {
            stream.last_frame = None;
        }
        let geometry_lost = match &current {
            Ok(geometry) => *geometry != stream.geometry,
            Err(error) => {
                stream.geometry.is_some()
                    || error.code == "access_lost"
                    || error.code == "device_lost"
            }
        };
        if geometry_lost {
            stream.last_frame = None;
            let _ = self.close(caller, id);
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
        // A startup worker executes provider code and can own native resources.
        // Never detach it when the controller is destroyed, even if a driver is
        // stalled; normal quiescence polls first and keeps this barrier off ticks.
        for (_, job) in std::mem::take(&mut self.opening) {
            let _ = job.worker.join();
        }
    }
}

fn prepare_capture<B: Backend>(
    backend: &B,
    output: &str,
    capture_format: crate::format::CaptureFormat,
) -> CaptureResult<PreparedCapture> {
    let mut session = backend.open_with_format(output, capture_format)?;
    let result = (|| {
        let geometry = initial_geometry(&mut *session)?;
        if capture_format.color_proof {
            session.validate_source()?;
            let info = session.source_info()?.ok_or_else(|| {
                CaptureError::new(
                    "unsupported_format",
                    "native source format proof unavailable",
                )
            })?;
            if info.encoding != capture_format.encoding() || info.desktop_hdr != capture_format.hdr
            {
                return Err(CaptureError::new(
                    "unsupported_format",
                    "native source format does not match requested representation",
                ));
            }
            capture_format.validate_dimensions(info.width, info.height)?;
        }
        let touch_surface = initial_touch_surface(&mut *session, geometry)?;
        Ok((geometry, touch_surface))
    })();
    let (geometry, touch_surface, failure) = match result {
        Ok((geometry, touch_surface)) => (geometry, touch_surface, None),
        Err(error) => (None, None, Some(error)),
    };
    Ok(PreparedCapture {
        session,
        geometry,
        touch_surface,
        failure,
    })
}

fn mark_closing(stream: &mut Stream) -> CloseReceipt {
    stream.last_frame = None;
    stream
        .close_receipt
        .get_or_insert_with(|| CloseReceipt(Arc::new(AtomicBool::new(false))))
        .clone()
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
            // The provider can resolve topology independently for each value.
            // Recheck the complete rectangle because a moved output may retain
            // the same dimensions.
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
    mut outputs: Vec<crate::model::Output>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> CaptureResult<Value> {
    const MAX_REPLY_BYTES: usize = 12_000;
    const MAX_PAGE_OUTPUTS: usize = 256;

    outputs.sort_by(|a, b| a.id.cmp(&b.id).then(a.device_name.cmp(&b.device_name)));
    if outputs.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(CaptureError::new(
            "backend_error",
            "capture backend enumerated duplicate output IDs",
        ));
    }
    let enumeration_id = enumeration_id(&outputs)?;
    let requested_page = offset.is_some() || limit.is_some();
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(MAX_PAGE_OUTPUTS);
    if limit == 0
        || limit > MAX_PAGE_OUTPUTS
        || offset > outputs.len()
        || (offset == outputs.len() && !outputs.is_empty())
    {
        return Err(CaptureError::new(
            "invalid_request",
            "enumeration offset or limit is out of range",
        ));
    }

    if !requested_page && outputs.len() <= MAX_PAGE_OUTPUTS {
        let reply = json!({"ok":true,"outputs":outputs,"enumeration_id":enumeration_id});
        if serialized_size(&reply)? <= MAX_REPLY_BYTES {
            return Ok(reply);
        }
    }

    let mut page = Vec::new();
    for output in outputs.iter().skip(offset).take(limit) {
        page.push(output);
        let next_offset = offset + page.len();
        let reply = page_reply(
            &page,
            (next_offset < outputs.len()).then_some(next_offset),
            &enumeration_id,
        );
        if serialized_size(&reply)? > MAX_REPLY_BYTES {
            page.pop();
            break;
        }
    }
    if page.is_empty() && offset < outputs.len() {
        return Err(CaptureError::new(
            "resource_exhausted",
            "one output exceeds the control reply limit",
        ));
    }
    let next_offset = offset + page.len();
    Ok(page_reply(
        &page,
        (next_offset < outputs.len()).then_some(next_offset),
        &enumeration_id,
    ))
}

fn enumeration_id(outputs: &[crate::model::Output]) -> CaptureResult<String> {
    let encoded = serde_json::to_vec(outputs)
        .map_err(|error| CaptureError::new("backend_error", error.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn page_reply(
    outputs: &[&crate::model::Output],
    next_offset: Option<usize>,
    enumeration_id: &str,
) -> Value {
    let mut reply = json!({"ok":true,"outputs":outputs,"enumeration_id":enumeration_id});
    if let Some(offset) = next_offset {
        reply["next_offset"] = json!(offset);
    }
    reply
}

fn serialized_size(reply: &Value) -> CaptureResult<usize> {
    serde_json::to_vec(reply)
        .map(|encoded| encoded.len())
        .map_err(|error| CaptureError::new("backend_error", error.to_string()))
}

fn open_reply(
    stream_id: u64,
    output: &str,
    geometry: Option<CaptureGeometry>,
    touch_surface: Option<TouchSurface>,
    constant_rate: bool,
) -> Value {
    let mut reply = stream_reply(stream_id, output, geometry, touch_surface, constant_rate);
    reply["ok"] = json!(true);
    reply
}

fn stream_reply(
    stream_id: u64,
    output: &str,
    geometry: Option<CaptureGeometry>,
    touch_surface: Option<TouchSurface>,
    constant_rate: bool,
) -> Value {
    let mut reply = json!({
        "stream": stream_id,
        "output": output,
        "constant_rate": constant_rate,
    });
    if let Some(geometry) = geometry {
        reply["desktop_geometry"] = json!(geometry);
    }
    if let Some(touch_surface) = touch_surface {
        reply["touch_surface"] = json!(touch_surface);
    }
    reply
}
