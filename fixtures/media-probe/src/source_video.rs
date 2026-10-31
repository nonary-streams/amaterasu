//! Test-only native consumer for a signed source-mode encoder route.
use amaterasu_media::{
    EncodedPacket, FLAG_END_OF_STREAM, MEDIA_BUSY, MEDIA_PENDING, PacketSink, VideoApi,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient, abi};
use serde_json::{Value, json};
use std::ffi::c_void;

const VIDEO: &str = "amaterasu.video/1";

pub struct SourceVideoProbe {
    request: u64,
    pending: u64,
    stream: u64,
    stage: Stage,
    packets: Vec<Value>,
    generic: bool,
    reject_eos: bool,
    rejected_eos: bool,
}

#[derive(Clone, Copy)]
enum Stage {
    Opening,
    Collecting,
    Draining,
    Eos,
    WaitingClose,
    ForeignClose,
    CaptureStatus,
}

impl SourceVideoProbe {
    pub fn foreign_close(runtime: &RuntimeClient, request: u64, stream: u64) -> ModuleResult<Self> {
        let pending = send(
            runtime,
            request,
            json!({"action":"close","stream":stream}),
            "foreign-close",
        )?;
        Ok(Self {
            request,
            pending,
            stream,
            stage: Stage::ForeignClose,
            packets: Vec::new(),
            generic: false,
            reject_eos: false,
            rejected_eos: false,
        })
    }

    pub fn start(runtime: &RuntimeClient, request: u64) -> ModuleResult<Self> {
        let pending = send(
            runtime,
            request,
            json!({
                "action":"open", "operation_id":format!("source-video-{request}"),
                "source_id":42, "encoder_provider":"fixture", "packet_boundaries":true,
                "encoder":{"clsid":"example.fake-h264","hardware_url":null,"friendly_name":"Example fake H.264"},
                "configuration":{"width":64,"height":48,"fps":30,"bitrate":1000000}
            }),
            "open",
        )?;
        Ok(Self {
            request,
            pending,
            stream: 0,
            stage: Stage::Opening,
            packets: Vec::new(),
            generic: false,
            reject_eos: false,
            rejected_eos: false,
        })
    }

    pub fn start_generic(runtime: &RuntimeClient, request: u64) -> ModuleResult<Self> {
        let pending = send(
            runtime,
            request,
            json!({
                "action":"open", "operation_id":format!("source-video-{request}"),
                "capture_provider":"custom", "output":"nv12-fixture-display", "encoder_provider":"generic", "packet_boundaries":true,
                "encoder":{"clsid":"example.fake-h264","hardware_url":null,"friendly_name":"Example fake H.264"},
                "configuration":{"width":64,"height":48,"fps":30,"bitrate":1000000}
            }),
            "open",
        )?;
        Ok(Self {
            request,
            pending,
            stream: 0,
            stage: Stage::Opening,
            packets: Vec::new(),
            generic: true,
            reject_eos: true,
            rejected_eos: false,
        })
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        if matches!(self.stage, Stage::Collecting | Stage::Eos) {
            let status = receive(runtime, self.stream, &mut self.packets, self.reject_eos)?;
            if status == MEDIA_BUSY && self.generic && self.reject_eos {
                self.reject_eos = false;
                self.rejected_eos = true;
                self.pending = send_service(
                    runtime,
                    self.request,
                    "test.capture-source/1",
                    json!({"action":"status"}),
                    "capture-status",
                )?;
                self.stage = Stage::CaptureStatus;
                return Ok(false);
            }
            if !matches!(status, abi::STATUS_OK | MEDIA_PENDING) {
                return Err(format!("source video receive failed: {status}"));
            }
            if matches!(self.stage, Stage::Collecting)
                && self.packets.len() >= if self.generic { 12 } else { 1 }
            {
                self.pending = send(
                    runtime,
                    self.request,
                    json!({"action":"drain","stream":self.stream}),
                    "drain",
                )?;
                self.stage = Stage::Draining;
            } else if matches!(self.stage, Stage::Eos)
                && self
                    .packets
                    .last()
                    .is_some_and(|p| p["flags"] == FLAG_END_OF_STREAM)
            {
                self.pending = send(
                    runtime,
                    self.request,
                    json!({"action":"status","stream":self.stream}),
                    "status",
                )?;
                self.stage = Stage::WaitingClose;
            }
            return Ok(false);
        }
        let response = runtime.send_ephemeral(Command::TakeResult {
            request: self.pending,
        })?;
        if matches!(self.stage, Stage::ForeignClose) {
            return match response {
                Response::Pending => Ok(false),
                Response::Result { payload } => {
                    runtime.require_success(Command::Complete {
                        request: self.request,
                        payload,
                    })?;
                    Ok(true)
                }
                response => Err(format!("foreign close request failed: {response:?}")),
            };
        }
        let payload = match response {
            Response::Pending => return Ok(false),
            Response::Result { payload } if payload["ok"] == true => payload,
            response => return Err(format!("source video control failed: {response:?}")),
        };
        match self.stage {
            Stage::Opening => {
                self.stream = payload["stream"]
                    .as_u64()
                    .ok_or("missing source video stream")?;
                self.stage = Stage::Collecting;
            }
            Stage::Draining => self.stage = Stage::Eos,
            Stage::CaptureStatus => {
                if payload["active"] != 1 {
                    return Err(format!("capture released before EOS acceptance: {payload}"));
                }
                self.stage = Stage::Eos;
            }
            Stage::WaitingClose => {
                if payload["streams"].as_array().is_some_and(Vec::is_empty) {
                    runtime.require_success(Command::Complete {
                        request: self.request,
                        payload: json!({"ok":true,"stream":self.stream,"packets":self.packets,"rejected_eos":self.rejected_eos}),
                    })?;
                    return Ok(true);
                }
                self.pending = send(
                    runtime,
                    self.request,
                    json!({"action":"status","stream":self.stream}),
                    "status",
                )?;
            }
            _ => unreachable!(),
        }
        Ok(false)
    }
}

fn send(runtime: &RuntimeClient, parent: u64, payload: Value, action: &str) -> ModuleResult<u64> {
    send_service(runtime, parent, VIDEO, payload, action)
}

fn send_service(
    runtime: &RuntimeClient,
    parent: u64,
    service: &str,
    payload: Value,
    action: &str,
) -> ModuleResult<u64> {
    match runtime.send_ephemeral(Command::Request {
        service: service.into(),
        payload,
        timeout_ms: 5000,
        operation_id: format!("source-video-{action}-{parent}"),
        parent: Some(parent),
    })? {
        Response::Accepted { id } => Ok(id),
        response => Err(format!("source video request rejected: {response:?}")),
    }
}

pub fn foreign_receive(runtime: &RuntimeClient, stream: u64) -> ModuleResult<abi::Status> {
    receive(runtime, stream, &mut Vec::new(), false)
}

fn receive(
    runtime: &RuntimeClient,
    stream: u64,
    packets: &mut Vec<Value>,
    reject_eos: bool,
) -> ModuleResult<abi::Status> {
    // SAFETY: binding owns the interface for this synchronous call; callback borrows packets only during receive.
    let binding = unsafe { runtime.bind_native(VIDEO, 1, 0, size_of::<VideoApi>() as u32) }?;
    let api = unsafe { binding.table::<VideoApi>() }?;
    let mut state = Sink {
        packets,
        reject_eos,
    };
    let sink = PacketSink {
        context: (&mut state as *mut Sink).cast(),
        write: collect,
    };
    Ok(unsafe { (api.receive)(api.base.context, stream, sink) })
}

struct Sink<'a> {
    packets: &'a mut Vec<Value>,
    reject_eos: bool,
}

unsafe extern "C" fn collect(context: *mut c_void, packet: *const EncodedPacket) -> abi::Status {
    if context.is_null() || packet.is_null() {
        return abi::STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: video lends a valid packet and the live vector for this synchronous callback.
    let (sink, packet) = unsafe { (&mut *context.cast::<Sink>(), &*packet) };
    if sink.reject_eos && packet.flags == FLAG_END_OF_STREAM {
        return MEDIA_BUSY;
    }
    let packets = &mut sink.packets;
    let Ok(bytes) = (unsafe { packet.bytes() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    if packets.len() >= 32 || bytes.len() > 1024 {
        return abi::STATUS_INVALID_ARGUMENT;
    }
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    packets.push(
        json!({"codec":packet.codec,"flags":packet.flags,"sequence":packet.sequence,
        "timestamp_us":packet.timestamp_us,"bytes_hex":hex}),
    );
    abi::STATUS_OK
}
