//! Test-only consumer for the custom NV12 source and the real software video path.
use amaterasu_media::{
    EncodedPacket, FLAG_END_OF_ACCESS_UNIT, MEDIA_PENDING, PacketSink, VideoApi,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient, abi};
use serde_json::{Value, json};
use std::ffi::c_void;

const VIDEO: &str = "amaterasu.video/1";

pub struct CustomVideoProbe {
    request: u64,
    pending: u64,
    stream: Option<u64>,
    stage: Stage,
    packets: Packets,
    video_status: Value,
}

#[derive(Clone, Copy)]
enum Stage {
    Opening,
    Collecting,
    Status,
    Closing,
}

#[derive(Default)]
struct Packets {
    bytes: Vec<u8>,
    access_units: u32,
}

impl CustomVideoProbe {
    pub fn start(runtime: &RuntimeClient, request: u64) -> ModuleResult<Self> {
        let pending = send(
            runtime,
            request,
            json!({
                "action":"open", "operation_id":format!("custom-video-{request}"),
                "capture_provider":"custom", "output":"nv12-fixture-display", "encoder":"software",
                "packet_boundaries":true,
                "configuration":{"width":64,"height":48,"fps":30,"bitrate":1000000,
                    "sw_preset":"superfast","sw_tune":"zerolatency"}
            }),
            "open",
        )?;
        Ok(Self {
            request,
            pending,
            stream: None,
            stage: Stage::Opening,
            packets: Packets::default(),
            video_status: Value::Null,
        })
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        if matches!(self.stage, Stage::Collecting) {
            // SAFETY: the binding is retained for this call, and the sink borrows
            // the live packet buffer only until the synchronous callback returns.
            let binding = unsafe { amaterasu_module_sdk::video::bind(runtime, VIDEO) }?;
            let api = unsafe { binding.table::<VideoApi>() }?;
            let sink = PacketSink {
                context: (&mut self.packets as *mut Packets).cast(),
                write: collect,
            };
            let status = unsafe { (api.receive)(api.base.context, self.stream.unwrap(), sink) };
            if !matches!(status, abi::STATUS_OK | MEDIA_PENDING) {
                return Err(format!("custom video receive failed: {status}"));
            }
            if self.packets.access_units >= 12 {
                self.pending = send(
                    runtime,
                    self.request,
                    json!({"action":"status", "stream":self.stream}),
                    "status",
                )?;
                self.stage = Stage::Status;
            }
            return Ok(false);
        }
        let payload = match runtime.send_ephemeral(Command::TakeResult {
            request: self.pending,
        })? {
            Response::Pending => return Ok(false),
            Response::Result { payload } if payload["ok"] == true => payload,
            response => return Err(format!("custom video control failed: {response:?}")),
        };
        match self.stage {
            Stage::Opening => {
                self.stream = payload["stream"].as_u64();
                if self.stream.is_none() {
                    return Err("missing custom video stream".into());
                }
                self.stage = Stage::Collecting;
            }
            Stage::Status => {
                self.video_status = payload;
                self.pending = send(
                    runtime,
                    self.request,
                    json!({"action":"close","stream":self.stream}),
                    "close",
                )?;
                self.stage = Stage::Closing;
            }
            Stage::Closing => {
                let hex: String = self
                    .packets
                    .bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                runtime.require_success(Command::Complete { request:self.request, payload:json!({"ok":true,"video":self.video_status,"access_units":self.packets.access_units,"h264_hex":hex}) })?;
                return Ok(true);
            }
            Stage::Collecting => unreachable!(),
        }
        Ok(false)
    }
}

fn send(runtime: &RuntimeClient, parent: u64, payload: Value, action: &str) -> ModuleResult<u64> {
    match runtime.send_ephemeral(Command::Request {
        service: VIDEO.into(),
        payload,
        timeout_ms: 5000,
        operation_id: format!("custom-video-{action}-{parent}"),
        parent: Some(parent),
    })? {
        Response::Accepted { id } => Ok(id),
        response => Err(format!("custom video request rejected: {response:?}")),
    }
}

unsafe extern "C" fn collect(context: *mut c_void, packet: *const EncodedPacket) -> abi::Status {
    if context.is_null() || packet.is_null() {
        return abi::STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: video passes this live sink context and a packet borrowed through the callback.
    let output = unsafe { &mut *context.cast::<Packets>() };
    let packet = unsafe { &*packet };
    let Ok(bytes) = (unsafe { packet.bytes() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    if output.bytes.len().saturating_add(bytes.len()) > 1_000_000 {
        return abi::STATUS_INVALID_ARGUMENT;
    }
    output.bytes.extend_from_slice(bytes);
    if packet.flags & FLAG_END_OF_ACCESS_UNIT != 0 {
        output.access_units += 1;
    }
    abi::STATUS_OK
}
