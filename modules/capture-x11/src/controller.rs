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
    output: String,
    operation_id: Option<String>,
    session: Box<dyn CaptureSession>,
    opened_at: Instant,
    sequence: u64,
    last_timestamp_us: u64,
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
        let stream = self.streams.get_mut(&id).expect("authorized X11 stream");
        let mut frame = stream.session.read()?;
        stream.sequence = stream.sequence.checked_add(1).ok_or_else(|| {
            CaptureError::new("sequence_exhausted", "X11 capture frame sequence exhausted")
        })?;
        let elapsed_us = stream.opened_at.elapsed().as_micros().min(u64::MAX as u128) as u64;
        let next_timestamp_us = stream.last_timestamp_us.checked_add(1).ok_or_else(|| {
            CaptureError::new("timestamp_exhausted", "X11 capture timestamp exhausted")
        })?;
        stream.last_timestamp_us = elapsed_us.max(next_timestamp_us);
        frame.sequence = stream.sequence;
        frame.timestamp_us = stream.last_timestamp_us;
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
            .expect("authorized X11 stream")
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
        Ok(enumeration_reply(&outputs[offset..end], outputs.len(), end))
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
                "X11 capture requires enabled startup policy",
            ));
        }
        if let Some(operation_id) = operation_id.as_ref() {
            validate_operation_id(operation_id)?;
            if let Some((stream, existing)) = self.find_operation(caller, operation_id) {
                if existing.output != output {
                    return Err(CaptureError::new(
                        "operation_conflict",
                        "operation ID already selected a different X11 output",
                    ));
                }
                return Ok(open_reply(
                    stream,
                    &existing.output,
                    existing.session.geometry().capture,
                ));
            }
        }
        if self.streams.len() >= MAX_STREAMS {
            return Err(CaptureError::new(
                "resource_exhausted",
                "at most four X11 streams may be open",
            ));
        }
        let (outputs, _) = self.backend.enumerate()?;
        if !outputs
            .iter()
            .any(|candidate| candidate.id == output && candidate.attached)
        {
            return Err(CaptureError::new(
                "output_unavailable",
                "selected X11 output is not active",
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
                output: output.clone(),
                operation_id,
                session,
                opened_at: Instant::now(),
                sequence: 0,
                last_timestamp_us: 0,
            },
        );
        self.opened_stream = Some(id);
        Ok(open_reply(id, &output, geometry))
    }

    fn status(&self, caller: &str) -> Value {
        let streams: Vec<_> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.owner == caller)
            .map(|(id, stream)| {
                stream_reply(*id, &stream.output, stream.session.geometry().capture)
            })
            .collect();
        let availability = self.backend.availability();
        json!({
            "ok": true,
            "enabled": self.enabled,
            "native_supported": availability.native_supported,
            "available": availability.available,
            "reason": availability.reason,
            "streams": streams,
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
            "X11 stream ID collision",
        ))
    }

    fn lookup(&self, caller: &str, operation_id: &str) -> CaptureResult<Value> {
        validate_operation_id(operation_id)?;
        self.find_operation(caller, operation_id)
            .map(|(id, stream)| open_reply(id, &stream.output, stream.session.geometry().capture))
            .ok_or_else(|| CaptureError::new("not_found", "X11 stream was not found"))
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
                "X11 stream was not found for this caller",
            ))
        }
    }
}

fn enumeration_reply(outputs: &[Output], total: usize, end: usize) -> Value {
    json!({
        "ok": true,
        "outputs": outputs,
        "total": total,
        "next_offset": (end < total).then_some(end),
    })
}

fn stream_reply(id: u64, output: &str, geometry: amaterasu_media::CaptureGeometry) -> Value {
    json!({
        "stream": id,
        "output": output,
        "desktop_geometry": geometry,
    })
}

fn open_reply(id: u64, output: &str, geometry: amaterasu_media::CaptureGeometry) -> Value {
    let mut reply = stream_reply(id, output, geometry);
    reply["ok"] = json!(true);
    reply
}
