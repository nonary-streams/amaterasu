use crate::backend::{Backend, CaptureSession, TouchSurface};
use crate::model::{CaptureError, CaptureResult, Frame, MAX_STREAMS, MAX_WAIT_MS, Operation};
use amaterasu_media::CaptureGeometry;
use serde_json::{Value, json};
use std::collections::BTreeMap;

struct Stream {
    owner: String,
    output: String,
    session: Box<dyn CaptureSession>,
    geometry: Option<CaptureGeometry>,
    touch_surface: Option<TouchSurface>,
    operation_id: Option<String>,
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
            } => self.open(caller, &output, operation_id, guard),
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
                if existing.output != output {
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
                open_reply(id, &stream.output, stream.geometry, stream.touch_surface)
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
        if result
            .as_ref()
            .is_err_and(|error| error.code == "access_lost" || error.code == "device_lost")
        {
            self.streams.remove(&id);
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
            self.streams.remove(&id);
            return Err(CaptureError::new(
                "access_lost",
                "capture desktop geometry changed or became unavailable; enumerate and reopen",
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
