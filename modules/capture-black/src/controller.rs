use crate::model::{
    MAX_BUFFER_BYTES, MAX_LEDGER, MAX_STREAMS, Operation, ResourceCaptureClock, SourceMode,
    validate_operation_id,
};
use amaterasu_media::{
    MEDIA_BACKEND_ERROR, MEDIA_BUSY, MEDIA_CLOSED, MEDIA_PENDING, MEDIA_UNSUPPORTED, PIXEL_NV12,
    PIXEL_P010,
};
use amaterasu_module_sdk::abi::{STATUS_OK, Status};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, TryLockError},
};

const RETIRED_OPERATION_US: u64 = 30 * 60 * 1_000_000;

#[derive(Clone, Copy)]
pub struct FrameIdentity {
    pub sequence: u64,
    pub timestamp_us: u64,
}

struct Pacing {
    origin_us: u64,
    accepted_sequence: u64,
    pending: Option<FrameIdentity>,
    reading: bool,
    closed: bool,
}

pub struct Delivery {
    pub mode: SourceMode,
    pub pixels: Arc<[u8]>,
    pub identity: FrameIdentity,
    pub clock_epoch: Option<[u8; 16]>,
    pacing: Arc<Mutex<Pacing>>,
}

impl Delivery {
    pub fn finish(self, status: Status) -> Status {
        if status == STATUS_OK {
            let mut pacing = self
                .pacing
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            pacing.accepted_sequence = self.identity.sequence;
            pacing.pending = None;
        }
        status
    }
}

impl Drop for Delivery {
    fn drop(&mut self) {
        self.pacing
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .reading = false;
    }
}

struct Stream {
    owner: String,
    operation_id: Option<String>,
    mode: SourceMode,
    pixel_mode: SourceMode,
    pixels: Arc<[u8]>,
    resource_clock: Option<ResourceCaptureClock>,
    selected: bool,
    abandoned: bool,
    pacing: Arc<Mutex<Pacing>>,
}

struct Retired {
    owner: String,
    operation_id: Option<String>,
    retired_at_us: u64,
}

pub struct Controller {
    enabled: bool,
    next_id: u64,
    bytes: usize,
    streams: BTreeMap<u64, Stream>,
    retired: BTreeMap<u64, Retired>,
}

impl Controller {
    pub fn preflight(
        &self,
        request: &amaterasu_module_sdk::capture::SourcePreflightRequest,
    ) -> amaterasu_module_sdk::capture::SourcePreflightResponse {
        use amaterasu_module_sdk::capture::{
            SourceClass, SourceModeCapabilities, SourcePreflightResponse,
        };
        if request.source_class != SourceClass::Synthetic {
            return SourcePreflightResponse::unsupported(request, "source_class_unsupported");
        }
        if !self.enabled {
            return SourcePreflightResponse::unavailable(request, "disabled");
        }
        if !request.mode.refresh_millihz.is_multiple_of(1_000) {
            return SourcePreflightResponse::unsupported(request, "refresh_unsupported");
        }
        let mode = SourceMode {
            width: request.mode.width,
            height: request.mode.height,
            fps: request.mode.refresh_millihz / 1_000,
            format: if request.mode.hdr {
                PIXEL_P010
            } else {
                PIXEL_NV12
            },
        };
        let mode = match SourceMode::parse(&mode.output()) {
            Ok(mode) => mode,
            Err(_) => return SourcePreflightResponse::unsupported(request, "mode_unsupported"),
        };
        if let Some(route) = &request.route {
            match SourceMode::parse(route) {
                Ok(route)
                    if route.width == mode.width
                        && route.height == mode.height
                        && route.fps == mode.fps => {}
                _ => return SourcePreflightResponse::unsupported(request, "route_unsupported"),
            }
        }
        if self.streams.len() >= MAX_STREAMS
            || self.streams.len() + self.retired.len() >= MAX_LEDGER
            || self
                .bytes
                .checked_add(mode.buffer_length().expect("validated mode"))
                .is_none_or(|bytes| bytes > MAX_BUFFER_BYTES)
        {
            return SourcePreflightResponse::unavailable(request, "source_capacity");
        }
        SourcePreflightResponse::ready(
            request,
            vec![SourceModeCapabilities {
                source_class: SourceClass::Synthetic,
                min_width: 2,
                max_width: 8192,
                min_height: 2,
                max_height: 8192,
                min_refresh_millihz: 1_000,
                max_refresh_millihz: 240_000,
                sdr: true,
                hdr: true,
            }],
        )
    }

    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            next_id: 0,
            bytes: 0,
            streams: BTreeMap::new(),
            retired: BTreeMap::new(),
        }
    }

    pub fn handle(
        &mut self,
        caller: &str,
        operation: Operation,
        now_us: u64,
        authorize: &mut impl FnMut() -> Result<(), &'static str>,
    ) -> Result<(Value, Option<u64>), &'static str> {
        if caller.is_empty() || caller.len() > 128 || now_us == 0 {
            return Err("invalid_caller_or_clock");
        }
        self.maintain(now_us);
        authorize()?;
        let reply = match operation {
            Operation::Status => {
                let streams: Vec<_> = self
                    .streams
                    .iter()
                    .filter(|(_, stream)| stream.owner == caller)
                    .map(|(&id, stream)| stream_reply(id, stream))
                    .collect();
                json!({"ok":true,"enabled":self.enabled,"native_supported":true,"streams":streams})
            }
            Operation::Enumerate { offset, limit } => {
                if offset.unwrap_or(0) != 0 || !(1..=256).contains(&limit.unwrap_or(256)) {
                    return Err("invalid_request");
                }
                json!({"ok":true,"enumeration_id":"black-source-v1","outputs":[]})
            }
            Operation::Open {
                output,
                operation_id,
                resource_capture_clock,
                resource_callback_contract,
            } => {
                return self.open(
                    caller,
                    &output,
                    operation_id,
                    resource_capture_clock,
                    resource_callback_contract,
                    now_us,
                    authorize,
                );
            }
            Operation::Lookup { operation_id } => self.lookup(caller, &operation_id)?,
            Operation::Close { stream } => {
                self.close(caller, stream, now_us)?;
                json!({"ok":true})
            }
        };
        Ok((reply, None))
    }

    #[allow(clippy::too_many_arguments)]
    fn open(
        &mut self,
        caller: &str,
        output: &str,
        operation_id: Option<String>,
        resource_clock: Option<ResourceCaptureClock>,
        callback_contract: Option<String>,
        now_us: u64,
        authorize: &mut impl FnMut() -> Result<(), &'static str>,
    ) -> Result<(Value, Option<u64>), &'static str> {
        if !self.enabled {
            return Err("disabled");
        }
        match (resource_clock.is_some(), callback_contract.as_deref()) {
            (false, None) | (true, Some("cross_service_safe")) => (),
            _ => return Err("invalid_resource_callback_contract"),
        }
        if let Some(clock) = &resource_clock {
            clock.epoch()?;
        }
        let mode = SourceMode::parse(output)?;
        if let Some(operation_id) = &operation_id {
            validate_operation_id(operation_id)?;
            if let Some((&id, stream)) = self.streams.iter().find(|(_, stream)| {
                stream.owner == caller && stream.operation_id.as_ref() == Some(operation_id)
            }) {
                return if stream.mode == mode && stream.resource_clock == resource_clock {
                    Ok((stream_reply(id, stream), None))
                } else {
                    Err("operation_conflict")
                };
            }
            if self.retired.values().any(|retired| {
                retired.owner == caller && retired.operation_id.as_ref() == Some(operation_id)
            }) {
                return Err("operation_closed");
            }
        }
        let length = mode.buffer_length()?;
        let total = self.bytes.checked_add(length).ok_or("resource_exhausted")?;
        if self.streams.len() >= MAX_STREAMS
            || self.streams.len() + self.retired.len() >= MAX_LEDGER
            || total > MAX_BUFFER_BYTES
        {
            return Err("resource_exhausted");
        }
        let pixels: Arc<[u8]> = mode.pixels()?.into();
        authorize()?;
        let id = self.next_id.checked_add(1).ok_or("resource_exhausted")?;
        let stream = Stream {
            owner: caller.into(),
            operation_id,
            mode,
            pixel_mode: mode,
            pixels,
            resource_clock,
            selected: false,
            abandoned: false,
            pacing: Arc::new(Mutex::new(Pacing {
                origin_us: now_us,
                accepted_sequence: 0,
                pending: None,
                reading: false,
                closed: false,
            })),
        };
        let reply = stream_reply(id, &stream);
        self.streams.insert(id, stream);
        self.next_id = id;
        self.bytes = total;
        Ok((reply, Some(id)))
    }

    fn lookup(&self, caller: &str, operation_id: &str) -> Result<Value, &'static str> {
        validate_operation_id(operation_id)?;
        self.streams
            .iter()
            .find(|(_, stream)| {
                stream.owner == caller && stream.operation_id.as_deref() == Some(operation_id)
            })
            .map(|(&id, stream)| stream_reply(id, stream))
            .ok_or("not_found")
    }

    fn stream(&self, caller: &str, id: u64) -> Result<&Stream, Status> {
        self.streams
            .get(&id)
            .filter(|stream| stream.owner == caller)
            .ok_or(MEDIA_CLOSED)
    }

    pub fn mode(&self, caller: &str, id: u64) -> Result<SourceMode, Status> {
        self.stream(caller, id).map(|stream| stream.mode)
    }

    pub fn select(&mut self, caller: &str, id: u64, generation: u64, offer: u64) -> Status {
        let Some(stream) = self
            .streams
            .get_mut(&id)
            .filter(|stream| stream.owner == caller)
        else {
            return MEDIA_CLOSED;
        };
        let format = match offer {
            1 => PIXEL_NV12,
            2 => PIXEL_P010,
            _ => return MEDIA_UNSUPPORTED,
        };
        if generation != id {
            return MEDIA_UNSUPPORTED;
        }
        let mode = SourceMode {
            format,
            ..stream.mode
        };
        let Ok(length) = mode.buffer_length() else {
            return MEDIA_UNSUPPORTED;
        };
        let mut pacing = stream
            .pacing
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if pacing.closed {
            return MEDIA_CLOSED;
        }
        if pacing.reading {
            return MEDIA_BUSY;
        }
        if stream.pixel_mode != mode {
            // Both the previous and candidate allocation must fit while a
            // selection transaction is prepared; no live frame is overwritten.
            if self
                .bytes
                .checked_add(length)
                .is_none_or(|bytes| bytes > MAX_BUFFER_BYTES)
            {
                return MEDIA_BUSY;
            }
            let pixels: Arc<[u8]> = match mode.pixels() {
                Ok(pixels) => pixels.into(),
                Err(_) => return MEDIA_BACKEND_ERROR,
            };
            self.bytes = self.bytes - stream.pixels.len() + pixels.len();
            stream.pixels = pixels;
            stream.pixel_mode = mode;
            pacing.pending = None;
        }
        stream.selected = true;
        STATUS_OK
    }

    pub fn prepare(
        &self,
        caller: &str,
        id: u64,
        now_us: u64,
        resource: bool,
    ) -> Result<Delivery, Status> {
        let stream = self.stream(caller, id)?;
        if resource && !stream.selected {
            return Err(MEDIA_UNSUPPORTED);
        }
        let mut pacing = stream.pacing.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => MEDIA_BUSY,
            TryLockError::Poisoned(_) => MEDIA_CLOSED,
        })?;
        if pacing.closed {
            return Err(MEDIA_CLOSED);
        }
        if pacing.reading {
            return Err(MEDIA_BUSY);
        }
        let identity = if let Some(identity) = pacing.pending {
            identity
        } else {
            let due_us = pacing.origin_us.saturating_add(
                pacing.accepted_sequence.saturating_mul(1_000_000) / u64::from(stream.mode.fps),
            );
            if now_us < due_us {
                return Err(MEDIA_PENDING);
            }
            let elapsed_frames = now_us
                .saturating_sub(pacing.origin_us)
                .saturating_mul(u64::from(stream.mode.fps))
                / 1_000_000;
            let sequence = elapsed_frames
                .saturating_add(1)
                .max(pacing.accepted_sequence.saturating_add(1));
            if sequence == u64::MAX {
                return Err(MEDIA_CLOSED);
            }
            let timestamp_us = pacing.origin_us.saturating_add(
                (sequence - 1).saturating_mul(1_000_000) / u64::from(stream.mode.fps),
            );
            let identity = FrameIdentity {
                sequence,
                timestamp_us,
            };
            pacing.pending = Some(identity);
            identity
        };
        pacing.reading = true;
        drop(pacing);
        Ok(Delivery {
            mode: stream.pixel_mode,
            pixels: stream.pixels.clone(),
            identity,
            clock_epoch: stream
                .resource_clock
                .as_ref()
                .map(|clock| clock.epoch().expect("validated resource clock")),
            pacing: stream.pacing.clone(),
        })
    }

    pub fn close(&mut self, caller: &str, id: u64, now_us: u64) -> Result<(), &'static str> {
        if self
            .retired
            .get(&id)
            .is_some_and(|retired| retired.owner == caller)
        {
            return Ok(());
        }
        let stream = self
            .streams
            .get(&id)
            .filter(|stream| stream.owner == caller)
            .ok_or("not_found")?;
        let mut pacing = stream
            .pacing
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        pacing.closed = true;
        if pacing.reading {
            return Err("busy");
        }
        drop(pacing);
        let stream = self.streams.remove(&id).expect("authorized stream");
        self.bytes -= stream.pixels.len();
        self.retired.insert(
            id,
            Retired {
                owner: stream.owner,
                operation_id: stream.operation_id,
                retired_at_us: now_us,
            },
        );
        Ok(())
    }

    pub fn rollback_open(&mut self, caller: &str, created: Option<u64>, now_us: u64) {
        if let Some(id) = created {
            if let Some(stream) = self
                .streams
                .get_mut(&id)
                .filter(|stream| stream.owner == caller)
            {
                stream.abandoned = true;
            }
            let _ = self.close(caller, id, now_us);
        }
    }

    pub fn maintain(&mut self, now_us: u64) {
        self.retired.retain(|_, retired| {
            now_us.saturating_sub(retired.retired_at_us) <= RETIRED_OPERATION_US
        });
        let abandoned: Vec<_> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.abandoned)
            .map(|(&id, stream)| (id, stream.owner.clone()))
            .collect();
        for (id, owner) in abandoned {
            let _ = self.close(&owner, id, now_us);
        }
    }

    pub fn shutdown(&mut self) {
        for stream in self.streams.values() {
            stream
                .pacing
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .closed = true;
        }
        self.streams.clear();
        self.bytes = 0;
    }
}

fn stream_reply(id: u64, stream: &Stream) -> Value {
    let mut reply = json!({"ok":true,"stream":id,"output":stream.mode.output(),"desktop_geometry":stream.mode.geometry()});
    if let Some(clock) = &stream.resource_clock {
        reply["resource_capture_clock"] = json!(clock);
        reply["resource_callback_contract"] = json!("cross_service_safe");
    }
    reply
}

#[cfg(test)]
mod tests;
