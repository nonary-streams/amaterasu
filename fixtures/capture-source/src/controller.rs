use crate::model::{
    FPS, FRAME_COUNT, FixtureResult, HEIGHT, MAX_OPENS, MAX_STREAMS, OUTPUT, Operation, WIDTH,
    frame_bytes, geometry, validate_operation_id,
};
use amaterasu_media::{MEDIA_BUSY, MEDIA_CLOSED, MEDIA_PENDING};
use amaterasu_module_sdk::abi::{STATUS_OK, Status};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

#[derive(Default)]
struct CallerMetrics {
    opened: u64,
    closed: u64,
    frames: Arc<AtomicU64>,
}

#[derive(Default)]
struct DeliveryState {
    accepted: AtomicU64,
    reading: AtomicBool,
    closed: AtomicBool,
}

struct Stream {
    owner: String,
    operation_id: Option<String>,
    delivery: Arc<DeliveryState>,
    resource_selected: bool,
}

/// One exclusively reserved frame. Dropping a rejected delivery permits an
/// identical retry; acceptance advances the sequence exactly once.
pub struct FrameDelivery {
    pub sequence: u64,
    pub bytes: Vec<u8>,
    delivery: Arc<DeliveryState>,
    global_frames: Arc<AtomicU64>,
    caller_frames: Arc<AtomicU64>,
}

impl FrameDelivery {
    pub fn is_closed(&self) -> bool {
        self.delivery.closed.load(Ordering::SeqCst)
    }

    pub fn finish(self, status: Status) -> Status {
        if status == STATUS_OK {
            self.delivery.accepted.fetch_add(1, Ordering::SeqCst);
            self.global_frames.fetch_add(1, Ordering::SeqCst);
            self.caller_frames.fetch_add(1, Ordering::SeqCst);
        }
        status
    }
}

impl Drop for FrameDelivery {
    fn drop(&mut self) {
        self.delivery.reading.store(false, Ordering::SeqCst);
    }
}

#[derive(Default)]
pub struct Controller {
    streams: BTreeMap<u64, Stream>,
    closed: BTreeMap<u64, String>,
    callers: BTreeMap<String, CallerMetrics>,
    global_frames: Arc<AtomicU64>,
    opened: u64,
    closed_count: u64,
}

impl Controller {
    pub fn handle(
        &mut self,
        caller: &str,
        operation: Operation,
    ) -> FixtureResult<(Value, Option<u64>)> {
        let response = match operation {
            Operation::Status => self.status(caller),
            Operation::Enumerate { offset, limit } => enumerate(offset, limit)?,
            Operation::Open {
                output,
                operation_id,
            } => return self.open(caller, &output, operation_id),
            Operation::Lookup { operation_id } => self.lookup(caller, &operation_id)?,
            Operation::Close { stream } => {
                self.close(caller, stream)?;
                json!({"ok": true})
            }
        };
        Ok((response, None))
    }

    fn open(
        &mut self,
        caller: &str,
        output: &str,
        operation_id: Option<String>,
    ) -> FixtureResult<(Value, Option<u64>)> {
        if let Some(operation) = &operation_id {
            validate_operation_id(operation)?;
            if let Some(id) = self.find_operation(caller, operation) {
                if output != OUTPUT {
                    return Err("operation_conflict");
                }
                return Ok((stream_reply(id), None));
            }
        }
        if output != OUTPUT {
            return Err("output_not_found");
        }
        if self.streams.len() >= MAX_STREAMS || self.opened >= MAX_OPENS {
            return Err("resource_exhausted");
        }
        self.opened += 1;
        let id = self.opened;
        self.streams.insert(
            id,
            Stream {
                owner: caller.into(),
                operation_id,
                delivery: Arc::new(DeliveryState::default()),
                resource_selected: false,
            },
        );
        self.callers.entry(caller.into()).or_default().opened += 1;
        Ok((stream_reply(id), Some(id)))
    }

    fn find_operation(&self, caller: &str, operation: &str) -> Option<u64> {
        self.streams.iter().find_map(|(&id, stream)| {
            (stream.owner == caller && stream.operation_id.as_deref() == Some(operation))
                .then_some(id)
        })
    }

    fn lookup(&self, caller: &str, operation: &str) -> FixtureResult<Value> {
        validate_operation_id(operation)?;
        self.find_operation(caller, operation)
            .map(stream_reply)
            .ok_or("not_found")
    }

    fn owned(&self, caller: &str, id: u64) -> FixtureResult<&Stream> {
        self.streams
            .get(&id)
            .filter(|stream| stream.owner == caller)
            .ok_or("not_found")
    }

    pub fn authorize(&self, caller: &str, id: u64) -> FixtureResult<()> {
        self.owned(caller, id).map(|_| ())
    }

    pub fn select_resource(
        &mut self,
        caller: &str,
        id: u64,
        generation: u64,
        offer: u64,
    ) -> Status {
        let Some(stream) = self
            .streams
            .get_mut(&id)
            .filter(|stream| stream.owner == caller)
        else {
            return MEDIA_CLOSED;
        };
        if generation != 1 || offer != 1 {
            return amaterasu_media::MEDIA_UNSUPPORTED;
        }
        stream.resource_selected = true;
        STATUS_OK
    }

    pub fn resource_selected(&self, caller: &str, id: u64) -> Result<bool, Status> {
        self.owned(caller, id)
            .map(|stream| stream.resource_selected)
            .map_err(|_| MEDIA_CLOSED)
    }

    pub fn close(&mut self, caller: &str, id: u64) -> FixtureResult<()> {
        if self.closed.get(&id).is_some_and(|owner| owner == caller) {
            return Ok(());
        }
        self.authorize(caller, id)?;
        let stream = self.streams.remove(&id).expect("authorized stream");
        stream.delivery.closed.store(true, Ordering::SeqCst);
        self.closed.insert(id, stream.owner);
        self.closed_count += 1;
        self.callers
            .get_mut(caller)
            .expect("stream owner metrics")
            .closed += 1;
        Ok(())
    }

    pub fn rollback_open(&mut self, caller: &str, created: Option<u64>) {
        if let Some(id) = created {
            let _ = self.close(caller, id);
        }
    }

    pub fn prepare_frame(&self, caller: &str, id: u64) -> Result<FrameDelivery, Status> {
        let stream = self.owned(caller, id).map_err(|_| MEDIA_CLOSED)?;
        if stream
            .delivery
            .reading
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(MEDIA_BUSY);
        }
        let sequence = stream.delivery.accepted.load(Ordering::SeqCst);
        let delivery = FrameDelivery {
            sequence,
            bytes: if sequence < FRAME_COUNT {
                frame_bytes(sequence)
            } else {
                Vec::new()
            },
            delivery: stream.delivery.clone(),
            global_frames: self.global_frames.clone(),
            caller_frames: self.callers[caller].frames.clone(),
        };
        if sequence >= FRAME_COUNT {
            return Err(MEDIA_PENDING);
        }
        Ok(delivery)
    }

    fn status(&self, caller: &str) -> Value {
        let streams: Vec<_> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.owner == caller)
            .map(|(&id, _)| stream_reply(id))
            .collect();
        json!({"ok": true, "enabled": true, "native_supported": true,
            "fixture": true, "test_only": true, "native_capture_qualified": false,
            "streams": streams})
    }

    pub fn metrics(&self) -> Value {
        let callers: BTreeMap<_, _> = self
            .callers
            .iter()
            .map(|(owner, counters)| {
                (
                    owner,
                    json!({"opened": counters.opened, "closed": counters.closed,
                        "frames": counters.frames.load(Ordering::SeqCst),
                        "active": self.streams.values().filter(|stream| &stream.owner == owner).count()}),
                )
            })
            .collect();
        json!({"ok": true, "fixture": true, "test_only": true,
            "source": "test_only_nv12", "native_capture_qualified": false,
            "opened": self.opened, "closed": self.closed_count,
            "frames": self.global_frames.load(Ordering::SeqCst), "active": self.streams.len(),
            "width": WIDTH, "height": HEIGHT, "fps": FPS, "frames_per_stream": FRAME_COUNT,
            "callers": callers})
    }

    pub fn shutdown(&mut self) {
        let owned: Vec<_> = self
            .streams
            .iter()
            .map(|(&id, stream)| (id, stream.owner.clone()))
            .collect();
        for (id, owner) in owned {
            let _ = self.close(&owner, id);
        }
    }
}

fn stream_reply(id: u64) -> Value {
    json!({"ok": true, "stream": id, "output": OUTPUT,
        "desktop_geometry": geometry(), "fixture": true, "test_only": true})
}

fn enumerate(offset: Option<usize>, limit: Option<usize>) -> FixtureResult<Value> {
    if offset.unwrap_or(0) != 0 || !(1..=256).contains(&limit.unwrap_or(256)) {
        return Err("invalid_request");
    }
    Ok(
        json!({"ok": true, "enumeration_id": "nv12-fixture-display-64x48-v1",
        "outputs": [{"id": OUTPUT, "adapter": "test-only-nv12-fixture",
            "adapter_vendor_id": 0, "adapter_device_id": 0,
            "adapter_description": "Trusted test-only NV12 fixture",
            "device_name": OUTPUT, "attached": true, "left": 0, "top": 0,
            "width": WIDTH, "height": HEIGHT, "rotation": 1}],
        "fixture": true, "test_only": true}),
    )
}

#[cfg(test)]
mod tests;
