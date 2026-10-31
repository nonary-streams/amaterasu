use amaterasu_abi::{STATUS_OK, Status};
use amaterasu_media::*;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};

const CAPACITY: usize = 2;
const MAX_SESSIONS: usize = 32;
const MAX_OPERATIONS: usize = 128;

#[derive(Default)]
pub struct State {
    next: u64,
    sessions: HashMap<u64, Session>,
    operations: HashMap<(String, String), Operation>,
    pub(crate) sources: HashMap<u64, crate::source::SourceSession>,
    pub(crate) consumers: HashMap<u64, crate::consumer::Selection>,
}
struct Session {
    owner: String,
    source_id: Option<u64>,
    configuration: Value,
    resource_clock: Option<ResourceClock>,
    last_resource_sequence: u64,
    queue: VecDeque<Packet>,
    draining: bool,
    eos_sent: bool,
}
struct Operation {
    id: u64,
    encoder: Value,
    config: Value,
    boundaries: bool,
    source_id: Option<u64>,
    resource_clock: Option<ResourceClock>,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ResourceClock {
    pub epoch: [u8; 16],
    pub origin: u64,
    pub value: Value,
}
struct Packet {
    bytes: Vec<u8>,
    timestamp: u64,
    sequence: u64,
    flags: u32,
}

fn identity() -> Value {
    json!({"clsid":"example.fake-h264","hardware_url":null,"friendly_name":"Example fake H.264"})
}
fn valid_config(config: &Value) -> bool {
    let number = |key: &str| config.get(key).and_then(Value::as_u64).unwrap_or(0);
    config.is_object()
        && (16..=3840).contains(&number("width"))
        && (16..=2160).contains(&number("height"))
        && number("width") % 2 == 0
        && number("height") % 2 == 0
        && (1..=120).contains(&number("fps"))
        && (100_000..=100_000_000).contains(&number("bitrate"))
        && config.get("codec").is_none_or(|v| v == "h264")
        && config
            .get("video_stream_format")
            .is_none_or(|v| v == &json!({"codec":"h264","bit_depth":8,"dynamic_range":"sdr"}))
}
fn valid_operation(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_graphic())
}
fn parse_clock(value: &Value) -> Option<ResourceClock> {
    let object = value.as_object()?;
    if object.len() != 3 || value.get("domain")?.as_str()? != "stream_relative" {
        return None;
    }
    let text = value.get("epoch_id")?.as_str()?;
    if text.len() != 32 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut epoch = [0; 16];
    for (i, byte) in epoch.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    if epoch == [0; 16] {
        return None;
    }
    let origin = value.get("host_origin_us")?.as_u64().filter(|n| *n != 0)?;
    Some(ResourceClock {
        epoch,
        origin,
        value: value.clone(),
    })
}
fn reply(id: u64, config: &Value, boundaries: bool, clock: Option<&ResourceClock>) -> Value {
    let mut result = json!({"ok":true,"session_id":id,"configuration":config,"codec":"h264","max_reference_frames":config.get("max_reference_frames").and_then(Value::as_u64).unwrap_or(0),"packet_boundaries":boundaries,"supports_packet_boundaries":true});
    if let Some(clock) = clock {
        result["resource_output_clock"] = clock.value.clone();
        result["resource_sequence_policy"] = json!("strict_source_echo");
        result["resource_callback_contract"] = json!("cross_service_safe");
    }
    result
}
impl State {
    pub fn control(&mut self, owner: &str, request: Value) -> (Value, Option<u64>) {
        match self.handle(owner, request) {
            Ok(v) => v,
            Err(code) => (json!({"ok":false,"code":code}), None),
        }
    }
    fn handle(
        &mut self,
        owner: &str,
        request: Value,
    ) -> Result<(Value, Option<u64>), &'static str> {
        let action = request
            .get("action")
            .and_then(Value::as_str)
            .ok_or("invalid_request")?;
        match action {
            "enumerate" => Ok((
                json!({"ok":true,"enabled":true,"encoders":[{"identity":identity(),"name":"Example fake H.264","codec":"h264","hardware":false,"vendor_id":null,"inputs":["cpu"],"max_width":3840,"max_height":2160}]}),
                None,
            )),
            "probe" | "open" => {
                let encoder = request.get("encoder").ok_or("invalid_request")?;
                let config = request.get("configuration").ok_or("invalid_request")?;
                if *encoder != identity() {
                    return Err("encoder_not_found");
                }
                if !valid_config(config) {
                    return Err("invalid_configuration");
                }
                if action == "probe" {
                    return Ok((
                        json!({"ok":true,"encoder":encoder,"configuration":config,"codec":"h264","usable":true,"settled":true,"code":null,"reason":null}),
                        None,
                    ));
                }
                if request
                    .get("encoder_options")
                    .is_some_and(|value| value.as_object().is_none_or(|object| !object.is_empty()))
                {
                    return Err("encoder_options_unsupported");
                }
                let source_id = match (request.get("mode"), request.get("source_id")) {
                    (None, None) => None,
                    (Some(mode), Some(source)) if mode == "source" => Some(
                        source
                            .as_u64()
                            .filter(|id| *id != 0)
                            .ok_or("invalid_source")?,
                    ),
                    _ => return Err("invalid_source"),
                };
                let resource_clock = request
                    .get("resource_output_clock")
                    .map(|value| parse_clock(value).ok_or("invalid_resource_clock"))
                    .transpose()?;
                if source_id.is_some() && resource_clock.is_some() {
                    return Err("invalid_resource_clock");
                }
                match (
                    resource_clock.is_some(),
                    request.get("resource_callback_contract"),
                ) {
                    (true, Some(value)) if value == "cross_service_safe" => {}
                    (false, None) => {}
                    _ => return Err("resource_callback_contract_unsupported"),
                }
                let boundaries = request
                    .get("packet_boundaries")
                    .map_or(Ok(false), |v| v.as_bool().ok_or("invalid_request"))?;
                let operation = request
                    .get("operation_id")
                    .filter(|v| !v.is_null())
                    .map(|v| v.as_str().ok_or("invalid_operation_id"))
                    .transpose()?;
                if let Some(operation) = operation {
                    if !valid_operation(operation) {
                        return Err("invalid_operation_id");
                    }
                    if let Some(record) = self
                        .operations
                        .get(&(owner.to_owned(), operation.to_owned()))
                    {
                        if record.encoder != *encoder
                            || record.config != *config
                            || record.boundaries != boundaries
                            || record.source_id != source_id
                            || record.resource_clock != resource_clock
                        {
                            return Err("operation_conflict");
                        }
                        if !self.sessions.contains_key(&record.id) {
                            return Err("session_closed");
                        }
                        return Ok((
                            reply(
                                record.id,
                                config,
                                boundaries,
                                record.resource_clock.as_ref(),
                            ),
                            None,
                        ));
                    }
                    if self.operations.len() >= MAX_OPERATIONS {
                        return Err("operation_limit");
                    }
                }
                if self.sessions.len() >= MAX_SESSIONS {
                    return Err("session_limit");
                }
                self.next = self.next.checked_add(1).ok_or("session_limit")?;
                let id = self.next;
                self.sessions.insert(
                    id,
                    Session {
                        owner: owner.into(),
                        source_id,
                        configuration: config.clone(),
                        resource_clock: resource_clock.clone(),
                        last_resource_sequence: 0,
                        queue: VecDeque::new(),
                        draining: false,
                        eos_sent: false,
                    },
                );
                if let Some(operation) = operation {
                    self.operations.insert(
                        (owner.into(), operation.into()),
                        Operation {
                            id,
                            encoder: encoder.clone(),
                            config: config.clone(),
                            boundaries,
                            source_id,
                            resource_clock: resource_clock.clone(),
                        },
                    );
                }
                Ok((
                    reply(id, config, boundaries, resource_clock.as_ref()),
                    Some(id),
                ))
            }
            "lookup" => {
                let operation = request
                    .get("operation_id")
                    .and_then(Value::as_str)
                    .filter(|s| valid_operation(s))
                    .ok_or("invalid_operation_id")?;
                let record = self
                    .operations
                    .get(&(owner.into(), operation.into()))
                    .ok_or("session_not_found")?;
                if !self.sessions.contains_key(&record.id) {
                    return Err("session_not_found");
                }
                Ok((
                    reply(
                        record.id,
                        &record.config,
                        record.boundaries,
                        record.resource_clock.as_ref(),
                    ),
                    None,
                ))
            }
            "close" => {
                let id = request
                    .get("session_id")
                    .and_then(Value::as_u64)
                    .ok_or("invalid_request")?;
                Ok((self.close(owner, id)?, None))
            }
            _ => Err("invalid_request"),
        }
    }
    pub fn close(&mut self, owner: &str, id: u64) -> Result<Value, &'static str> {
        self.owned(owner, id).ok_or("session_not_found")?;
        self.sources.remove(&id);
        self.consumers.remove(&id);
        self.sessions.remove(&id);
        Ok(json!({"ok":true,"session_id":id,"state":"closed"}))
    }
    pub(crate) fn authorized(&self, owner: &str, id: u64) -> bool {
        self.sessions
            .get(&id)
            .is_some_and(|session| session.owner == owner)
    }
    pub(crate) fn authorized_source(&self, owner: &str, id: u64, source: u64) -> bool {
        self.sessions
            .get(&id)
            .is_some_and(|session| session.owner == owner && session.source_id == Some(source))
    }
    pub(crate) fn source_configuration_matches(
        &self,
        id: u64,
        config: &amaterasu_media::EncoderCommonConfigV1,
    ) -> bool {
        let Some(session) = self.sessions.get(&id) else {
            return false;
        };
        let number = |key: &str| session.configuration.get(key).and_then(Value::as_u64);
        number("width") == Some(u64::from(config.coded_width))
            && number("height") == Some(u64::from(config.coded_height))
            && number("fps").is_some_and(|fps| {
                fps * u64::from(config.fps_denominator) == u64::from(config.fps_numerator)
            })
            && number("bitrate") == Some(config.bitrate_bps)
            && config.crop_x == 0
            && config.crop_y == 0
            && config.visible_width == config.coded_width
            && config.visible_height == config.coded_height
    }
    pub(crate) fn submitted_dimensions(&self, owner: &str, id: u64) -> Option<(u32, u32)> {
        let session = self.sessions.get(&id)?;
        if session.owner != owner || session.source_id.is_some() {
            return None;
        }
        Some((
            u32::try_from(session.configuration.get("width")?.as_u64()?).ok()?,
            u32::try_from(session.configuration.get("height")?.as_u64()?).ok()?,
        ))
    }
    pub(crate) fn submitted_clock(&self, owner: &str, id: u64) -> Option<ResourceClock> {
        let session = self.sessions.get(&id)?;
        if session.owner != owner || session.source_id.is_some() {
            return None;
        }
        session.resource_clock.clone()
    }
    pub(crate) fn last_resource_sequence(&self, owner: &str, id: u64) -> Option<u64> {
        self.sessions
            .get(&id)
            .filter(|session| session.owner == owner)
            .map(|session| session.last_resource_sequence)
    }
    pub(crate) fn record_resource_sequence(&mut self, owner: &str, id: u64, sequence: u64) {
        if let Some(session) = self.owned(owner, id) {
            session.last_resource_sequence = sequence;
        }
    }
    fn owned(&mut self, owner: &str, id: u64) -> Option<&mut Session> {
        self.sessions.get_mut(&id).filter(|s| s.owner == owner)
    }
    pub fn submit(&mut self, owner: &str, id: u64, frame: &CpuFrame, force: u32) -> Status {
        self.submit_frame(owner, id, frame, force, false)
    }
    pub(crate) fn submit_resource(
        &mut self,
        owner: &str,
        id: u64,
        frame: &CpuFrame,
        force: u32,
    ) -> Status {
        self.submit_frame(owner, id, frame, force, true)
    }
    fn submit_frame(
        &mut self,
        owner: &str,
        id: u64,
        frame: &CpuFrame,
        force: u32,
        resource: bool,
    ) -> Status {
        let Some(session) = self.owned(owner, id) else {
            return MEDIA_CLOSED;
        };
        if session.draining || session.source_id.is_some() {
            return MEDIA_CLOSED;
        }
        if resource != session.resource_clock.is_some() {
            return MEDIA_UNSUPPORTED;
        }
        if session.queue.len() >= CAPACITY {
            return MEDIA_BUSY;
        }
        if let Err(status) = frame.validate() {
            return status;
        }
        // Test payload deliberately does not read or retain caller-owned pixel memory.
        let mut bytes = b"FAKE-ENCODER\0".to_vec();
        bytes.extend_from_slice(&frame.sequence.to_le_bytes());
        session.queue.push_back(Packet {
            bytes,
            timestamp: frame.timestamp_us,
            sequence: frame.sequence,
            flags: FLAG_END_OF_ACCESS_UNIT | if force != 0 { FLAG_KEYFRAME } else { 0 },
        });
        STATUS_OK
    }
    pub fn drain(&mut self, owner: &str, id: u64) -> Status {
        let Some(session) = self.owned(owner, id) else {
            return MEDIA_CLOSED;
        };
        session.draining = true;
        STATUS_OK
    }
    /// The sink is valid for this call and must not retain the packet borrow.
    pub unsafe fn receive(&mut self, owner: &str, id: u64, sink: PacketSink) -> Status {
        let Some(session) = self.owned(owner, id) else {
            return MEDIA_CLOSED;
        };
        if let Some(packet) = session.queue.front() {
            let output = EncodedPacket {
                codec: CODEC_H264,
                flags: packet.flags,
                data: packet.bytes.as_ptr(),
                len: packet.bytes.len(),
                timestamp_us: packet.timestamp,
                sequence: packet.sequence,
            };
            // SAFETY: queue owns the bytes throughout the synchronous callback.
            let status = unsafe { sink.send(&output) };
            if status == STATUS_OK {
                session.queue.pop_front();
            }
            return status;
        }
        if session.draining && !session.eos_sent {
            let eos = EncodedPacket {
                codec: CODEC_H264,
                flags: FLAG_END_OF_STREAM,
                data: std::ptr::null(),
                len: 0,
                timestamp_us: 0,
                sequence: 0,
            };
            // SAFETY: EOS and sink remain live for the synchronous callback.
            let status = unsafe { sink.send(&eos) };
            if status == STATUS_OK {
                session.eos_sent = true;
            }
            return status;
        }
        if session.eos_sent {
            MEDIA_CLOSED
        } else {
            MEDIA_PENDING
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_abi::STATUS_INVALID_ARGUMENT;
    use std::ffi::c_void;
    fn open(state: &mut State, owner: &str, operation: &str) -> u64 {
        let (reply, _) = state.control(owner, json!({"action":"open","encoder":identity(),"configuration":{"width":16,"height":16,"fps":60,"bitrate":1000000},"operation_id":operation,"packet_boundaries":true}));
        reply["session_id"].as_u64().unwrap()
    }
    #[derive(Default)]
    struct Sink {
        reject: bool,
        packets: Vec<(Vec<u8>, u32, u64)>,
    }
    unsafe extern "C" fn write(context: *mut c_void, packet: *const EncodedPacket) -> Status {
        // SAFETY: tests provide valid sink and packet pointers for this callback.
        let sink = unsafe { &mut *context.cast::<Sink>() };
        // SAFETY: packet points to valid storage during the callback.
        let packet = unsafe { &*packet };
        // SAFETY: packet bytes remain live for the callback.
        let bytes = unsafe { packet.bytes().unwrap() }.to_vec();
        sink.packets.push((bytes, packet.flags, packet.sequence));
        if sink.reject { MEDIA_BUSY } else { STATUS_OK }
    }
    fn receive(state: &mut State, owner: &str, id: u64, sink: &mut Sink) -> Status {
        // SAFETY: sink and callback stay live through receive.
        unsafe {
            state.receive(
                owner,
                id,
                PacketSink {
                    context: (sink as *mut Sink).cast(),
                    write,
                },
            )
        }
    }
    #[test]
    fn bounded_queue_retries_rejected_packet_and_drains_once() {
        let mut state = State::default();
        let id = open(&mut state, "alice", "op");
        let pixels = vec![0; 16 * 16 * 4];
        let mut frame = CpuFrame {
            width: 16,
            height: 16,
            format: PIXEL_BGRA8,
            stride: 64,
            data: pixels.as_ptr(),
            len: pixels.len(),
            timestamp_us: 7,
            sequence: 1,
        };
        assert_eq!(state.submit("alice", id, &frame, 1), STATUS_OK);
        frame.sequence = 2;
        assert_eq!(state.submit("alice", id, &frame, 0), STATUS_OK);
        frame.sequence = 3;
        assert_eq!(state.submit("alice", id, &frame, 0), MEDIA_BUSY);
        let mut sink = Sink {
            reject: true,
            ..Default::default()
        };
        assert_eq!(receive(&mut state, "alice", id, &mut sink), MEDIA_BUSY);
        sink.reject = false;
        assert_eq!(receive(&mut state, "alice", id, &mut sink), STATUS_OK);
        assert_eq!(sink.packets[0], sink.packets[1]);
        assert_eq!(state.submit("alice", id, &frame, 0), STATUS_OK);
        assert_eq!(state.drain("alice", id), STATUS_OK);
        assert_eq!(state.drain("alice", id), STATUS_OK);
        assert_eq!(state.submit("alice", id, &frame, 0), MEDIA_CLOSED);
        assert_eq!(receive(&mut state, "alice", id, &mut sink), STATUS_OK);
        assert_eq!(receive(&mut state, "alice", id, &mut sink), STATUS_OK);
        sink.reject = true;
        assert_eq!(receive(&mut state, "alice", id, &mut sink), MEDIA_BUSY);
        sink.reject = false;
        assert_eq!(receive(&mut state, "alice", id, &mut sink), STATUS_OK);
        assert_eq!(sink.packets[4], sink.packets[5]);
        assert_eq!(sink.packets[5].1, FLAG_END_OF_STREAM);
        assert_eq!(receive(&mut state, "alice", id, &mut sink), MEDIA_CLOSED);
    }
    #[test]
    fn generic_open_requires_and_replays_callback_contract() {
        let mut state = State::default();
        let mut request = json!({"action":"open", "encoder":identity(),
            "configuration":{"width":16,"height":16,"fps":60,"bitrate":1_000_000},
            "operation_id":"generic", "resource_output_clock":{"domain":"stream_relative",
                "epoch_id":"01010101010101010101010101010101","host_origin_us":100}});
        assert_eq!(
            state.control("alice", request.clone()).0["code"],
            "resource_callback_contract_unsupported"
        );
        request["resource_callback_contract"] = json!("unsupported");
        assert_eq!(
            state.control("alice", request.clone()).0["code"],
            "resource_callback_contract_unsupported"
        );
        request["resource_callback_contract"] = json!("cross_service_safe");
        let (opened, created) = state.control("alice", request.clone());
        assert!(created.is_some());
        assert_eq!(opened["resource_callback_contract"], "cross_service_safe");
        assert_eq!(opened["resource_sequence_policy"], "strict_source_echo");
        assert_eq!(
            opened["resource_output_clock"],
            request["resource_output_clock"]
        );
        let (retried, created) = state.control("alice", request);
        assert!(created.is_none());
        assert_eq!(retried, opened);
        assert_eq!(
            state
                .control("alice", json!({"action":"lookup","operation_id":"generic"}))
                .0,
            opened
        );
    }
    #[test]
    fn operation_retry_and_authenticated_ownership() {
        let mut state = State::default();
        let id = open(&mut state, "alice", "same");
        assert_eq!(open(&mut state, "alice", "same"), id);
        let other = open(&mut state, "bob", "same");
        assert_ne!(id, other);
        assert_eq!(
            state
                .control("bob", json!({"action":"close","session_id":id}))
                .0["code"],
            "session_not_found"
        );
        assert_eq!(state.drain("bob", id), MEDIA_CLOSED);
        assert_eq!(
            state
                .control("alice", json!({"action":"lookup","operation_id":"same"}))
                .0["session_id"],
            id
        );
        assert!(state.close("alice", id).is_ok());
        assert!(state.close("alice", id).is_err());
        assert_eq!(
            state
                .control("alice", json!({"action":"lookup","operation_id":"same"}))
                .0["code"],
            "session_not_found"
        );
        assert_eq!(state.drain("alice", id), MEDIA_CLOSED);
    }
    #[test]
    fn invalid_frames_and_conflicting_operation_are_rejected() {
        let mut state = State::default();
        let id = open(&mut state, "alice", "op");
        let frame = CpuFrame {
            width: 0,
            height: 0,
            format: PIXEL_BGRA8,
            stride: 0,
            data: std::ptr::null(),
            len: 0,
            timestamp_us: 0,
            sequence: 0,
        };
        assert_eq!(
            state.submit("alice", id, &frame, 0),
            STATUS_INVALID_ARGUMENT
        );
        let (reply,_) = state.control("alice",json!({"action":"open","encoder":identity(),"configuration":{"width":32,"height":16,"fps":60,"bitrate":1000000},"operation_id":"op","packet_boundaries":true}));
        assert_eq!(reply["code"], "operation_conflict");
    }
}
