use crate::backend::{Backend, CaptureSession};
use crate::model::{
    CaptureError, CaptureResult, Frame, FrameDelivery, MAX_STREAMS, MAX_WAIT_MS, Operation,
};
use amaterasu_media::CaptureGeometry;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Instant;

struct Stream {
    owner: String,
    output: String,
    session: Box<dyn CaptureSession>,
    geometry: Option<CaptureGeometry>,
    operation_id: Option<String>,
    constant_rate: bool,
    opened_at: Instant,
    last_frame: Option<Frame>,
    next_sequence: u64,
    last_timestamp_us: u64,
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
        frame.sequence = sequence;
        frame.timestamp_us = timestamp_us;
        Ok(())
    }
}

pub struct Controller<B: Backend> {
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

    pub fn shutdown(&mut self) {
        self.enabled = false;
        self.streams.clear();
    }

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
            Operation::Enumerate => self.backend.enumerate().and_then(enumeration_reply),
            Operation::Open {
                output,
                operation_id,
                constant_rate,
            } => self.open(caller, &output, operation_id, constant_rate, guard),
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
        guard: &mut impl FnMut() -> CaptureResult<()>,
    ) -> CaptureResult<Value> {
        if !self.enabled {
            return Err(CaptureError::new(
                "capture_disabled",
                "capture requires enabled startup policy",
            ));
        }
        if let Some(operation_id) = &operation_id {
            validate_operation_id(operation_id)?;
            if let Some((stream_id, existing)) = self.find_operation(caller, operation_id) {
                if existing.output != output || existing.constant_rate != constant_rate {
                    return Err(CaptureError::new(
                        "operation_conflict",
                        "operation ID already selected a different output or delivery mode",
                    ));
                }
                return Ok(open_reply(
                    stream_id,
                    &existing.output,
                    existing.geometry,
                    existing.constant_rate,
                ));
            }
        }
        if self.streams.len() >= MAX_STREAMS {
            return Err(CaptureError::new(
                "resource_exhausted",
                "at most four capture streams may be open",
            ));
        }
        let id = self.next_stream_id()?;
        guard()?;
        let mut session = self.backend.open(output)?;
        let geometry = initial_geometry(&mut *session)?;
        // Opening may block in a driver. Recheck before retaining resources.
        guard()?;
        self.streams.insert(
            id,
            Stream {
                owner: caller.into(),
                output: output.into(),
                session,
                geometry,
                operation_id,
                constant_rate,
                opened_at: Instant::now(),
                last_frame: None,
                next_sequence: 0,
                last_timestamp_us: 0,
            },
        );
        self.opened_stream = Some(id);
        Ok(open_reply(id, output, geometry, constant_rate))
    }

    fn status(&self, caller: &str) -> Value {
        let streams: Vec<_> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.owner == caller)
            .map(|(id, stream)| {
                let mut reply =
                    stream_reply(*id, &stream.output, stream.geometry, stream.constant_rate);
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
        if self.streams.contains_key(&id) {
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
                open_reply(id, &stream.output, stream.geometry, stream.constant_rate)
            })
            .ok_or_else(|| {
                CaptureError::new("not_found", "no capture stream owned by this operation")
            })
    }

    pub fn close(&mut self, caller: &str, id: u64) -> CaptureResult<()> {
        self.authorize(caller, id)?;
        self.streams.remove(&id);
        Ok(())
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
        let result = {
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
        if result
            .as_ref()
            .is_err_and(|error| error.code == "access_lost" || error.code == "device_lost")
        {
            self.streams.remove(&id);
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
        let changed = current
            .as_ref()
            .is_ok_and(|current| *current != stream.geometry);
        if changed
            || current
                .as_ref()
                .is_err_and(|error| error.code == "access_lost" || error.code == "device_lost")
        {
            stream.last_frame = None;
            self.streams.remove(&id);
            return Err(CaptureError::new(
                "access_lost",
                "capture desktop geometry changed; enumerate and reopen",
            ));
        }
        current
    }
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

fn validate_operation_id(operation_id: &str) -> CaptureResult<()> {
    if operation_id.is_empty() || operation_id.len() > 128 {
        return Err(CaptureError::new(
            "invalid_request",
            "operation ID must contain 1..128 UTF-8 bytes",
        ));
    }
    Ok(())
}

fn enumeration_reply(outputs: Vec<crate::model::Output>) -> CaptureResult<Value> {
    let reply = json!({"ok":true,"outputs":outputs});
    let encoded = serde_json::to_vec(&reply)
        .map_err(|error| CaptureError::new("backend_error", error.to_string()))?;
    if encoded.len() > 12_000 {
        return Err(CaptureError::new(
            "resource_exhausted",
            "output metadata exceeds the control reply limit",
        ));
    }
    Ok(reply)
}

fn open_reply(
    stream_id: u64,
    output: &str,
    geometry: Option<CaptureGeometry>,
    constant_rate: bool,
) -> Value {
    let mut reply = stream_reply(stream_id, output, geometry, constant_rate);
    reply["ok"] = json!(true);
    reply
}

fn stream_reply(
    stream_id: u64,
    output: &str,
    geometry: Option<CaptureGeometry>,
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
    reply
}
