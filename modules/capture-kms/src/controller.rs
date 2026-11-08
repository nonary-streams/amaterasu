use crate::{
    backend::{Backend, CaptureSession, NativeBackend},
    model::{
        CaptureError, CaptureResult, Frame, MAX_OUTPUTS, MAX_STREAMS, Operation, Output,
        validate_operation_id,
    },
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

struct Stream {
    owner: String,
    output: Output,
    operation_id: Option<String>,
    session: Box<dyn CaptureSession>,
    opened_at: Instant,
    sequence: u64,
    last_timestamp_us: u64,
    frame_readback_verified: bool,
}

pub struct Controller<B: Backend = NativeBackend> {
    backend: B,
    enabled: bool,
    streams: BTreeMap<u64, Stream>,
    opened_stream: Option<u64>,
}

impl<B: Backend> Controller<B> {
    pub fn new(backend: B, enabled: bool) -> Self {
        Self {
            backend,
            enabled,
            streams: BTreeMap::new(),
            opened_stream: None,
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
            Operation::Enumerate { offset, limit } => self.enumerate(offset, limit),
            Operation::Open {
                output,
                operation_id,
            } => self.open(caller, output, operation_id, guard),
            Operation::Lookup { operation_id } => self.lookup(caller, &operation_id),
            Operation::Close { stream } => self.close(caller, stream).map(|()| json!({"ok":true})),
        };
        result.unwrap_or_else(CaptureError::payload)
    }

    pub fn take_opened_stream(&mut self) -> Option<u64> {
        self.opened_stream.take()
    }

    #[cfg(test)]
    fn with_backend<C: Backend>(self, backend: C) -> Controller<C> {
        Controller {
            backend,
            enabled: self.enabled,
            streams: self.streams,
            opened_stream: self.opened_stream,
        }
    }

    pub fn shutdown(&mut self) {
        self.enabled = false;
        self.streams.clear();
    }

    pub fn close(&mut self, caller: &str, id: u64) -> CaptureResult<()> {
        self.authorize(caller, id)?;
        self.streams.remove(&id);
        Ok(())
    }

    pub fn read(&mut self, caller: &str, id: u64) -> CaptureResult<Frame> {
        self.authorize(caller, id)?;
        let stream = self.streams.get_mut(&id).expect("authorized DRM stream");
        let mut frame = stream.session.read()?;
        stream.sequence = stream.sequence.checked_add(1).ok_or_else(|| {
            CaptureError::new("sequence_exhausted", "DRM capture sequence exhausted")
        })?;
        let elapsed_us = stream.opened_at.elapsed().as_micros().min(u64::MAX as u128) as u64;
        stream.last_timestamp_us =
            elapsed_us.max(stream.last_timestamp_us.checked_add(1).ok_or_else(|| {
                CaptureError::new("timestamp_exhausted", "DRM capture timestamp exhausted")
            })?);
        frame.sequence = stream.sequence;
        frame.timestamp_us = stream.last_timestamp_us;
        stream.frame_readback_verified = true;
        Ok(frame)
    }

    pub fn geometry(
        &self,
        caller: &str,
        id: u64,
    ) -> CaptureResult<amaterasu_media::CaptureGeometry> {
        self.authorize(caller, id)?;
        Ok(self
            .streams
            .get(&id)
            .expect("authorized DRM stream")
            .session
            .geometry()
            .capture)
    }

    fn enumerate(&self, offset: Option<usize>, limit: Option<usize>) -> CaptureResult<Value> {
        let (outputs, _) = self.backend.enumerate()?;
        let offset = offset.unwrap_or(0);
        let limit = limit.unwrap_or(MAX_OUTPUTS).min(MAX_OUTPUTS);
        if offset > outputs.len() {
            return Err(CaptureError::new(
                "invalid_request",
                "output offset is out of range",
            ));
        }
        let end = offset.saturating_add(limit).min(outputs.len());
        Ok(json!({
            "ok":true,
            "outputs":outputs[offset..end],
            "total":outputs.len(),
            "next_offset":(end < outputs.len()).then_some(end),
            "presentation_events":false,
            "managed_virtual_output":false,
            "display_composition":"active_primary_plane_only",
            "cursor_and_overlay_planes_captured":false,
            "crtc_color_pipeline_captured":false,
        }))
    }

    fn open(
        &mut self,
        caller: &str,
        output: String,
        operation_id: Option<String>,
        guard: &mut impl FnMut() -> CaptureResult<()>,
    ) -> CaptureResult<Value> {
        if !self.enabled {
            return Err(CaptureError::new(
                "capture_disabled",
                "DRM capture requires enabled startup policy",
            ));
        }
        if let Some(operation_id) = operation_id.as_ref() {
            validate_operation_id(operation_id)?;
            if let Some((stream, existing)) = self.find_operation(caller, operation_id) {
                if existing.output.id != output {
                    return Err(CaptureError::new(
                        "operation_conflict",
                        "operation ID already selected a different DRM output",
                    ));
                }
                return Ok(open_reply(stream, existing));
            }
        }
        if self.streams.len() >= MAX_STREAMS {
            return Err(CaptureError::new(
                "resource_exhausted",
                "at most four DRM streams may be open",
            ));
        }
        let (outputs, _) = self.backend.enumerate()?;
        let selected = outputs
            .into_iter()
            .find(|candidate| candidate.id == output && candidate.attached)
            .ok_or_else(|| {
                CaptureError::new("output_unavailable", "selected DRM output is not active")
            })?;
        if !selected.framebuffer_export_supported {
            return Err(CaptureError::new(
                "unsupported_pixel_format",
                selected
                    .readback_reason
                    .unwrap_or("DRM framebuffer is not exportable"),
            ));
        }
        let id = self.next_stream_id()?;
        guard()?;
        let session = self.backend.open(&output)?;
        let geometry = session.geometry().capture;
        guard()?;
        self.streams.insert(
            id,
            Stream {
                owner: caller.into(),
                output: selected,
                operation_id,
                session,
                opened_at: Instant::now(),
                sequence: 0,
                last_timestamp_us: 0,
                frame_readback_verified: false,
            },
        );
        self.opened_stream = Some(id);
        let stream = self.streams.get(&id).expect("new DRM stream inserted");
        Ok(open_reply_with_geometry(id, stream, geometry))
    }

    fn status(&self, caller: &str) -> Value {
        let streams: Vec<_> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.owner == caller)
            .map(|(id, stream)| stream_reply(*id, stream))
            .collect();
        let availability = self.backend.availability();
        json!({
            "ok":true,
            "enabled":self.enabled,
            "native_supported":availability.native_supported,
            "available":availability.available,
            "framebuffer_export_supported":availability.framebuffer_export_supported,
            "presentation_events":availability.presentation_events,
            "managed_virtual_output":availability.managed_virtual_output,
            "display_composition":"active_primary_plane_only",
            "cursor_and_overlay_planes_captured":false,
            "crtc_color_pipeline_captured":false,
            "timestamp_source":"monotonic_acquisition",
            "pixel_path":"gbm_egl_gles_cpu_bgra",
            "reason":availability.reason,
            "streams":streams,
        })
    }

    fn next_stream_id(&self) -> CaptureResult<u64> {
        for _ in 0..4 {
            let mut random = [0; 8];
            getrandom::fill(&mut random)
                .map_err(|error| CaptureError::new("entropy_error", error.to_string()))?;
            let id = u64::from_le_bytes(random);
            if id != 0 && !self.streams.contains_key(&id) {
                return Ok(id);
            }
        }
        Err(CaptureError::new(
            "entropy_error",
            "DRM stream ID collision",
        ))
    }

    fn lookup(&self, caller: &str, operation: &str) -> CaptureResult<Value> {
        validate_operation_id(operation)?;
        self.find_operation(caller, operation)
            .map(|(id, stream)| open_reply(id, stream))
            .ok_or_else(|| CaptureError::new("not_found", "DRM stream was not found"))
    }

    fn find_operation(&self, caller: &str, operation: &str) -> Option<(u64, &Stream)> {
        self.streams
            .iter()
            .find(|(_, stream)| {
                stream.owner == caller && stream.operation_id.as_deref() == Some(operation)
            })
            .map(|(id, stream)| (*id, stream))
    }

    fn authorize(&self, caller: &str, id: u64) -> CaptureResult<()> {
        if self
            .streams
            .get(&id)
            .is_some_and(|stream| stream.owner == caller)
        {
            Ok(())
        } else {
            Err(CaptureError::new(
                "not_found",
                "DRM stream was not found for this caller",
            ))
        }
    }
}

fn stream_reply(id: u64, stream: &Stream) -> Value {
    json!({
        "stream":id,
        "output":stream.output.id,
        "frame_readback_verified":stream.frame_readback_verified,
    })
}

fn open_reply(id: u64, stream: &Stream) -> Value {
    let mut reply = stream_reply(id, stream);
    reply["ok"] = json!(true);
    reply
}

fn open_reply_with_geometry(
    id: u64,
    stream: &Stream,
    geometry: amaterasu_media::CaptureGeometry,
) -> Value {
    let mut reply = open_reply(id, stream);
    reply["desktop_geometry"] = json!(geometry);
    reply
}

// Experimental scheduler; it is not registered by the production module.
#[cfg(all(test, feature = "isolated-helper", target_os = "linux", amaterasu_kms))]
mod isolated {
    use super::*;
    use crate::{
        backend::{
            IsolatedOpenTask, IsolatedSession, convert_isolated_info, convert_isolated_outputs,
        },
        helper_process::Registry,
        metadata_task::{Metadata, MetadataRequest, MetadataTask},
        open_task::TaskPoll,
    };

    const MAX_PENDING: usize = 8;

    pub(crate) struct IsolatedCompletion {
        pub id: u64,
        pub caller: String,
        pub payload: Value,
        pub opened_stream: Option<u64>,
    }

    struct IsolatedStream {
        owner: String,
        operation: Option<String>,
        session: IsolatedSession,
    }
    enum Work {
        Metadata(MetadataTask),
        Open(IsolatedOpenTask),
        Ready(CaptureResult<Outcome>),
        Close(u64),
    }
    enum Outcome {
        Metadata(Metadata),
        Open(IsolatedSession),
    }
    enum Kind {
        Status,
        Enumerate {
            offset: Option<usize>,
            limit: Option<usize>,
        },
        Open {
            output: String,
            operation: Option<String>,
        },
        Close,
    }
    struct Pending {
        caller: String,
        kind: Kind,
        work: Work,
        cancelled: bool,
    }

    pub(crate) struct IsolatedController {
        registry: Registry,
        enabled: bool,
        streams: BTreeMap<u64, IsolatedStream>,
        retiring: BTreeMap<u64, IsolatedStream>,
        pending: BTreeMap<u64, Pending>,
    }

    impl IsolatedController {
        pub fn new(enabled: bool) -> Self {
            Self {
                registry: Registry::default(),
                enabled,
                streams: BTreeMap::new(),
                retiring: BTreeMap::new(),
                pending: BTreeMap::new(),
            }
        }

        pub fn submit(
            &mut self,
            id: u64,
            caller: String,
            operation: Operation,
        ) -> CaptureResult<Option<Value>> {
            if self.pending.contains_key(&id) || self.pending.len() >= MAX_PENDING {
                return Err(CaptureError::new(
                    "resource_exhausted",
                    "KMS request capacity exhausted",
                ));
            }
            let (kind, work) = match operation {
                Operation::Status => (
                    Kind::Status,
                    Work::Metadata(
                        MetadataTask::spawn(self.registry.clone(), MetadataRequest::Probe)
                            .map_err(start_error)?,
                    ),
                ),
                Operation::Enumerate { offset, limit } => (
                    Kind::Enumerate { offset, limit },
                    Work::Metadata(
                        MetadataTask::spawn(self.registry.clone(), MetadataRequest::Enumerate)
                            .map_err(start_error)?,
                    ),
                ),
                Operation::Open {
                    output,
                    operation_id,
                } => {
                    if !self.enabled {
                        return Err(CaptureError::new(
                            "capture_disabled",
                            "DRM capture requires enabled startup policy",
                        ));
                    }
                    crate::model::validate_output_id(&output)?;
                    if let Some(operation) = &operation_id {
                        validate_operation_id(operation)?;
                        if let Some((stream_id, stream)) =
                            self.streams.iter().find(|(_, stream)| {
                                stream.owner == caller
                                    && stream.operation.as_ref() == Some(operation)
                            })
                        {
                            if stream.session.output().id != output {
                                return Err(CaptureError::new(
                                    "operation_conflict",
                                    "operation ID already selected a different DRM output",
                                ));
                            }
                            return Ok(Some(open_payload(*stream_id, stream, false)));
                        }
                        if self.pending.values().any(|pending| pending.caller == caller && matches!(&pending.kind, Kind::Open { operation: Some(existing), .. } if existing == operation)) {
                            return Err(CaptureError::new("operation_pending", "DRM open operation is still pending"));
                        }
                    }
                    let inflight = self
                        .pending
                        .values()
                        .filter(|pending| matches!(pending.kind, Kind::Open { .. }))
                        .count();
                    if self.streams.len() + self.retiring.len() + inflight >= MAX_STREAMS {
                        return Err(CaptureError::new(
                            "resource_exhausted",
                            "at most four DRM streams may be open",
                        ));
                    }
                    let task = IsolatedOpenTask::spawn(self.registry.clone(), output.clone())
                        .map_err(start_error)?;
                    (
                        Kind::Open {
                            output,
                            operation: operation_id,
                        },
                        Work::Open(task),
                    )
                }
                Operation::Lookup { operation_id } => {
                    validate_operation_id(&operation_id)?;
                    let (stream_id, stream) = self
                        .streams
                        .iter()
                        .find(|(_, stream)| {
                            stream.owner == caller
                                && stream.operation.as_ref() == Some(&operation_id)
                        })
                        .ok_or_else(|| {
                            CaptureError::new("not_found", "DRM stream was not found")
                        })?;
                    return Ok(Some(open_payload(*stream_id, stream, false)));
                }
                Operation::Close { stream } => {
                    let entry = self
                        .streams
                        .get(&stream)
                        .filter(|entry| entry.owner == caller)
                        .ok_or_else(|| {
                            CaptureError::new(
                                "not_found",
                                "DRM stream was not found for this caller",
                            )
                        })?;
                    let _ = entry;
                    let entry = self.streams.remove(&stream).expect("authorized stream");
                    self.retiring.insert(stream, entry);
                    (Kind::Close, Work::Close(stream))
                }
            };
            self.pending.insert(
                id,
                Pending {
                    caller,
                    kind,
                    work,
                    cancelled: false,
                },
            );
            Ok(None)
        }

        pub fn cancel(&mut self, id: u64) {
            if let Some(pending) = self.pending.get_mut(&id) {
                pending.cancelled = true;
                match &pending.work {
                    Work::Metadata(task) => {
                        task.cancel();
                    }
                    Work::Open(task) => {
                        task.cancel();
                    }
                    _ => {}
                }
            }
        }

        pub fn rollback(&mut self, caller: &str, id: u64) {
            if self
                .streams
                .get(&id)
                .is_some_and(|stream| stream.owner == caller)
            {
                let stream = self.streams.remove(&id).expect("owned stream");
                self.retiring.insert(id, stream);
            }
        }

        pub fn poll(
            &mut self,
            active: &mut impl FnMut(u64) -> CaptureResult<bool>,
        ) -> CaptureResult<Vec<IsolatedCompletion>> {
            self.registry.poll().map_err(cleanup_error)?;
            for (id, pending) in &mut self.pending {
                if !pending.cancelled && !active(*id)? {
                    pending.cancelled = true;
                }
                if pending.cancelled {
                    match &pending.work {
                        Work::Metadata(task) => {
                            task.cancel();
                        }
                        Work::Open(task) => {
                            task.cancel();
                        }
                        _ => {}
                    }
                }
                let outcome = match &mut pending.work {
                    Work::Metadata(task) => match task.poll() {
                        TaskPoll::Pending => None,
                        TaskPoll::Failed(error) => return Err(cleanup_error(error)),
                        TaskPoll::Retired(result) => Some(
                            result
                                .map_err(cleanup_error)
                                .and_then(|result| result.map(Outcome::Metadata)),
                        ),
                    },
                    Work::Open(task) => match task.poll() {
                        TaskPoll::Pending => None,
                        TaskPoll::Failed(error) => return Err(cleanup_error(error)),
                        TaskPoll::Retired(result) => Some(
                            result
                                .map_err(cleanup_error)
                                .and_then(|result| result.map(Outcome::Open)),
                        ),
                    },
                    _ => None,
                };
                if let Some(outcome) = outcome {
                    pending.work = Work::Ready(outcome);
                }
            }
            let mut retired = Vec::new();
            for (id, stream) in &mut self.retiring {
                if stream.session.retire()? {
                    retired.push(*id);
                }
            }
            for id in retired {
                self.retiring.remove(&id);
            }
            let ready: Vec<u64> = self
                .pending
                .iter()
                .filter_map(|(id, pending)| match &pending.work {
                    Work::Ready(_) => Some(*id),
                    Work::Close(stream) if !self.retiring.contains_key(stream) => Some(*id),
                    _ => None,
                })
                .collect();
            let mut completions = Vec::new();
            for id in ready {
                // Check before moving a completed allocation into the stream table.
                let is_active = if self.pending[&id].cancelled {
                    false
                } else {
                    active(id)?
                };
                let pending = self.pending.remove(&id).expect("ready request");
                if !is_active {
                    continue;
                }
                let (payload, opened_stream) = match (pending.kind, pending.work) {
                    (Kind::Close, Work::Close(_)) => (json!({"ok":true}), None),
                    (kind, Work::Ready(result)) => match result {
                        Err(error) => (error.payload(), None),
                        Ok(Outcome::Metadata(metadata)) => (
                            self.metadata_payload(&pending.caller, kind, metadata)
                                .unwrap_or_else(CaptureError::payload),
                            None,
                        ),
                        Ok(Outcome::Open(session)) => {
                            let Kind::Open { output, operation } = kind else {
                                return Err(CaptureError::new(
                                    "capture_unavailable",
                                    "mismatched KMS operation",
                                ));
                            };
                            if session.output().id != output {
                                return Err(CaptureError::new(
                                    "capture_unavailable",
                                    "mismatched KMS output",
                                ));
                            }
                            let stream_id = self.next_stream_id()?;
                            let stream = IsolatedStream {
                                owner: pending.caller.clone(),
                                operation,
                                session,
                            };
                            let payload = open_payload(stream_id, &stream, true);
                            self.streams.insert(stream_id, stream);
                            (payload, Some(stream_id))
                        }
                    },
                    _ => {
                        return Err(CaptureError::new(
                            "capture_unavailable",
                            "invalid KMS request state",
                        ));
                    }
                };
                completions.push(IsolatedCompletion {
                    id,
                    caller: pending.caller,
                    payload,
                    opened_stream,
                });
            }
            Ok(completions)
        }

        pub fn shutdown(&mut self) -> CaptureResult<bool> {
            self.enabled = false;
            for pending in self.pending.values_mut() {
                pending.cancelled = true;
                match &pending.work {
                    Work::Metadata(task) => {
                        task.cancel();
                    }
                    Work::Open(task) => {
                        task.cancel();
                    }
                    _ => {}
                }
            }
            self.retiring.append(&mut self.streams);
            self.poll(&mut |_| Ok(false))?;
            Ok(self.pending.is_empty()
                && self.retiring.is_empty()
                && self.registry.poll().map_err(cleanup_error)?)
        }

        fn next_stream_id(&self) -> CaptureResult<u64> {
            for _ in 0..4 {
                let mut random = [0; 8];
                getrandom::fill(&mut random)
                    .map_err(|error| CaptureError::new("entropy_error", error.to_string()))?;
                let id = u64::from_le_bytes(random);
                if id != 0 && !self.streams.contains_key(&id) && !self.retiring.contains_key(&id) {
                    return Ok(id);
                }
            }
            Err(CaptureError::new(
                "entropy_error",
                "DRM stream ID collision",
            ))
        }

        fn metadata_payload(
            &self,
            caller: &str,
            kind: Kind,
            metadata: Metadata,
        ) -> CaptureResult<Value> {
            match (kind, metadata) {
                (Kind::Status, Metadata::Probe(info)) => {
                    let availability = convert_isolated_info(info);
                    let streams: Vec<_> = self.streams.iter().filter(|(_, stream)| stream.owner == caller).map(|(id, stream)| json!({"stream":id,"output":stream.session.output().id,"frame_readback_verified":false})).collect();
                    Ok(
                        json!({"ok":true,"enabled":self.enabled,"native_supported":availability.native_supported,"available":availability.available,"framebuffer_export_supported":availability.framebuffer_export_supported,"presentation_events":false,"managed_virtual_output":false,"display_composition":"active_primary_plane_only","cursor_and_overlay_planes_captured":false,"crtc_color_pipeline_captured":false,"timestamp_source":"monotonic_acquisition","pixel_path":"gbm_egl_gles_cpu_bgra","reason":availability.reason,"streams":streams}),
                    )
                }
                (Kind::Enumerate { offset, limit }, Metadata::Enumerate(records)) => {
                    let (outputs, _) = convert_isolated_outputs(&records)?;
                    let offset = offset.unwrap_or(0);
                    let limit = limit.unwrap_or(MAX_OUTPUTS).min(MAX_OUTPUTS);
                    if offset > outputs.len() {
                        return Err(CaptureError::new(
                            "invalid_request",
                            "output offset is out of range",
                        ));
                    }
                    let end = offset.saturating_add(limit).min(outputs.len());
                    Ok(
                        json!({"ok":true,"outputs":outputs[offset..end],"total":outputs.len(),"next_offset":(end < outputs.len()).then_some(end),"presentation_events":false,"managed_virtual_output":false,"display_composition":"active_primary_plane_only","cursor_and_overlay_planes_captured":false,"crtc_color_pipeline_captured":false}),
                    )
                }
                _ => Err(CaptureError::new(
                    "capture_unavailable",
                    "mismatched KMS metadata",
                )),
            }
        }
    }

    fn open_payload(id: u64, stream: &IsolatedStream, geometry: bool) -> Value {
        let mut value = json!({"ok":true,"stream":id,"output":stream.session.output().id,"frame_readback_verified":false});
        if geometry {
            value["desktop_geometry"] = json!(stream.session.geometry().capture);
        }
        value
    }
    fn start_error(error: std::io::Error) -> CaptureError {
        CaptureError::new("capture_unavailable", error.to_string())
    }
    fn cleanup_error(error: std::io::Error) -> CaptureError {
        CaptureError::new("cleanup_failed", error.to_string())
    }
}

#[cfg(all(test, feature = "isolated-helper", target_os = "linux", amaterasu_kms))]
pub(crate) use isolated::{IsolatedCompletion, IsolatedController};

#[cfg(test)]
mod tests {
    use super::Controller;
    use crate::{
        backend::{Backend, CaptureSession},
        model::{
            CaptureError, CaptureResult, DesktopGeometry, NativeAvailability, Operation, Output,
        },
    };
    use amaterasu_media::{CaptureGeometry, DesktopRect};
    use serde_json::Value;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct MetadataBackend {
        opened: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
    }

    impl Backend for MetadataBackend {
        fn availability(&self) -> NativeAvailability {
            NativeAvailability {
                native_supported: true,
                available: true,
                framebuffer_export_supported: true,
                presentation_events: false,
                managed_virtual_output: false,
                reason: None,
            }
        }

        fn enumerate(&self) -> CaptureResult<(Vec<Output>, DesktopRect)> {
            Ok((vec![output()], DesktopRect::new(0, 0, 2, 2).unwrap()))
        }

        fn open(&self, output_id: &str) -> CaptureResult<Box<dyn CaptureSession>> {
            if output_id != "kms:card0/conn=1/crtc=2/plane=3" {
                return Err(CaptureError::new(
                    "output_unavailable",
                    "missing test output",
                ));
            }
            self.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(NoReadbackSession {
                dropped: self.dropped.clone(),
            }))
        }
    }

    struct NoReadbackSession {
        dropped: Arc<AtomicUsize>,
    }

    impl CaptureSession for NoReadbackSession {
        fn read(&mut self) -> CaptureResult<crate::model::Frame> {
            Err(CaptureError::new(
                "capture_unavailable",
                "no DRM device is connected in this unit-test backend",
            ))
        }

        fn geometry(&self) -> DesktopGeometry {
            DesktopGeometry {
                capture: CaptureGeometry::new(
                    DesktopRect::new(0, 0, 2, 2).unwrap(),
                    DesktopRect::new(0, 0, 2, 2).unwrap(),
                )
                .unwrap(),
            }
        }
    }

    impl Drop for NoReadbackSession {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn output() -> Output {
        Output {
            id: "kms:card0/conn=1/crtc=2/plane=3".into(),
            adapter: "/dev/dri/card0".into(),
            adapter_vendor_id: None,
            adapter_device_id: None,
            adapter_description: Some("DRM/KMS primary plane".into()),
            device_name: "DP-1 (/dev/dri/card0)".into(),
            attached: true,
            framebuffer_export_supported: true,
            readback_reason: None,
            left: 0,
            top: 0,
            width: 2,
            height: 2,
            rotation: 1,
        }
    }

    fn controller(
        enabled: bool,
    ) -> (
        Controller<MetadataBackend>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        let opened = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        (
            Controller::new(
                MetadataBackend {
                    opened: opened.clone(),
                    dropped: dropped.clone(),
                },
                enabled,
            ),
            opened,
            dropped,
        )
    }

    fn request(controller: &mut Controller<MetadataBackend>, operation: Operation) -> Value {
        controller.handle("console.one", operation, &mut || Ok(()))
    }

    #[test]
    fn module_quiescence_releases_capture_before_reporting_ready() {
        use amaterasu_module_sdk::{ManagedModule, RuntimeClient, abi::*};
        use std::{ffi::c_void, sync::Mutex};

        unsafe extern "C" fn unused_host(
            _context: *mut c_void,
            _request: BorrowedBytes,
            _reply: ReplySink,
        ) -> Status {
            STATUS_INTERNAL_ERROR
        }

        let (mut controller, _, dropped) = controller(true);
        let reply = request(
            &mut controller,
            Operation::Open {
                output: output().id,
                operation_id: Some("shutdown".into()),
            },
        );
        let stream = reply["stream"].as_u64().unwrap();
        let controller = Arc::new(Mutex::new(
            controller.with_backend(crate::backend::NativeBackend::new()),
        ));
        let factory = crate::native::Factory::new(controller.clone());
        // SAFETY: the stateless endpoint stays live through this fixture.
        // Shutdown must not dispatch requests or touch a real DRM device.
        let runtime = unsafe {
            RuntimeClient::from_endpoint(HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: std::ptr::null_mut(),
                invoke: unused_host,
            })
        };
        let mut module = crate::CaptureModule {
            runtime,
            controller,
            _factory: factory,
            shutting_down: false,
        };

        module.prepare_shutdown(20_000).unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        assert!(
            module
                .controller
                .lock()
                .unwrap()
                .geometry("console.one", stream)
                .is_ok()
        );
        let shared = module.controller.clone();
        let held = shared.lock().unwrap();
        assert_eq!(module.quiesce(), Ok(false));
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(held);
        assert_eq!(module.quiesce(), Ok(true));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(module.quiesce(), Ok(true));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn tick_does_not_dequeue_work_while_native_read_holds_controller() {
        use amaterasu_module_sdk::{Command, ManagedModule, Response, RuntimeClient, abi::*};
        use std::{ffi::c_void, sync::Mutex, time::Duration};

        unsafe extern "C" fn host(
            context: *mut c_void,
            request: BorrowedBytes,
            reply: ReplySink,
        ) -> Status {
            // SAFETY: this test retains the counter and request for the call.
            let polls = unsafe { &*context.cast::<AtomicUsize>() };
            let command: Command =
                serde_json::from_slice(unsafe { request.read().unwrap() }).unwrap();
            assert!(matches!(command, Command::Poll {}));
            polls.fetch_add(1, Ordering::SeqCst);
            let response = serde_json::to_vec(&Response::Pending).unwrap();
            // SAFETY: the synchronous reply sink copies this byte slice.
            unsafe { reply.send(&response) };
            STATUS_OK
        }

        let polls = Arc::new(AtomicUsize::new(0));
        let controller = Arc::new(Mutex::new(Controller::new(
            crate::backend::NativeBackend::new(),
            true,
        )));
        let factory = crate::native::Factory::new(controller.clone());
        // SAFETY: polls remains alive until after the module is dropped.
        let runtime = unsafe {
            RuntimeClient::from_endpoint(HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: Arc::as_ptr(&polls).cast_mut().cast(),
                invoke: host,
            })
        };
        let mut module = crate::CaptureModule {
            runtime,
            controller: controller.clone(),
            _factory: factory,
            shutting_down: false,
        };
        let (locked, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _guard = controller.lock().unwrap();
            locked.send(()).unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(2));
        });
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        module.tick().unwrap();
        let calls_while_locked = polls.load(Ordering::SeqCst);
        release.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(calls_while_locked, 0);
        module.tick().unwrap();
        assert_eq!(polls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stream_leases_are_caller_scoped_and_release_native_session() {
        let (mut controller, opened, dropped) = controller(true);
        let reply = request(
            &mut controller,
            Operation::Open {
                output: output().id,
                operation_id: Some("launch.one".into()),
            },
        );
        assert_eq!(reply["ok"], true);
        let stream = reply["stream"].as_u64().unwrap();
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        assert_eq!(
            controller.read("console.two", stream).unwrap_err().code,
            "not_found"
        );
        assert_eq!(
            controller.read("console.one", stream).unwrap_err().code,
            "capture_unavailable"
        );
        controller.close("console.one", stream).unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn same_operation_is_idempotent_without_reopening_the_device() {
        let (mut controller, opened, _) = controller(true);
        let first = request(
            &mut controller,
            Operation::Open {
                output: output().id,
                operation_id: Some("launch.one".into()),
            },
        );
        let second = request(
            &mut controller,
            Operation::Open {
                output: output().id,
                operation_id: Some("launch.one".into()),
            },
        );
        assert_eq!(first["stream"], second["stream"]);
        assert_eq!(opened.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn disabled_capture_does_not_open_a_device() {
        let (mut controller, opened, _) = controller(false);
        let reply = request(
            &mut controller,
            Operation::Open {
                output: output().id,
                operation_id: None,
            },
        );
        assert_eq!(reply["error"]["code"], "capture_disabled");
        assert_eq!(opened.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn denied_frame_access_never_returns_pixel_success() {
        let (mut controller, _, _) = controller(true);
        let reply = request(
            &mut controller,
            Operation::Open {
                output: output().id,
                operation_id: None,
            },
        );
        let stream = reply["stream"].as_u64().unwrap();
        assert_eq!(
            controller.read("console.one", stream).unwrap_err().code,
            "capture_unavailable"
        );
        let status = request(&mut controller, Operation::Status);
        assert_eq!(status["streams"][0]["frame_readback_verified"], false);
        assert_eq!(status["display_composition"], "active_primary_plane_only");
        assert_eq!(status["cursor_and_overlay_planes_captured"], false);
        assert_eq!(status["crtc_color_pipeline_captured"], false);
    }
}
