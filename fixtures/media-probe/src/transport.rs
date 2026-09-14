//! Finite signed-library workflow; test endpoints are always local loopback.
use crate::opus::{Packet, encode_transport_packets};
use amaterasu_media::{
    CODEC_H264, CODEC_OPUS, EncodedAudioPacket, EncodedPacket, FLAG_KEYFRAME, MediaTransportApi,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient, abi};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const ENCODER: &str = "amaterasu.audio-encode/1";
const TRANSPORT: &str = "amaterasu.media-transport/1";

#[derive(Clone, Copy)]
enum Stage {
    EncoderOpen,
    TransportOpen,
    TransportStatus,
    TransportClose,
    EncoderClose,
}

pub struct TransportProbe {
    request: u64,
    pending: u64,
    stage: Stage,
    encoder: Option<u64>,
    transport: Option<u64>,
    settings: Value,
    audio: Vec<Packet>,
    result: Value,
    deadline: Instant,
    sequence: u32,
}

impl TransportProbe {
    pub fn start(runtime: &RuntimeClient, request: u64, settings: Value) -> ModuleResult<Self> {
        for field in ["video_port", "audio_port"] {
            if !settings[field]
                .as_u64()
                .is_some_and(|port| (1..=65535).contains(&port))
            {
                return Err("transport fixture requires valid local UDP ports".into());
            }
        }
        let pending = call(
            runtime,
            request,
            0,
            ENCODER,
            json!({
                "action": "open",
                "channels": 2,
                "packet_ms": 10,
                "bitrate": 128000,
                "operation_id": format!("udp-probe-encoder-{request}"),
            }),
        )?;
        Ok(Self {
            request,
            pending,
            stage: Stage::EncoderOpen,
            encoder: None,
            transport: None,
            settings,
            audio: Vec::new(),
            result: Value::Null,
            deadline: Instant::now() + Duration::from_secs(3),
            sequence: 0,
        })
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        let response = runtime.send_ephemeral(Command::TakeResult {
            request: self.pending,
        });
        let payload = match response {
            Ok(Response::Pending) => return Ok(false),
            Ok(Response::Result { payload }) if payload["ok"] == true => payload,
            _ => return self.cleanup_failure(runtime, "transport fixture control failed".into()),
        };
        let outcome = match self.stage {
            Stage::EncoderOpen => self.open_transport(runtime, payload),
            Stage::TransportOpen => self.send_media(runtime, payload),
            Stage::TransportStatus => self.check_drained(runtime, payload),
            Stage::TransportClose => {
                self.transport = None;
                self.close_encoder(runtime)
            }
            Stage::EncoderClose => {
                self.encoder = None;
                return self.complete(runtime);
            }
        };
        match outcome {
            Ok(()) => Ok(false),
            Err(error) => self.cleanup_failure(runtime, error),
        }
    }

    fn cleanup_failure(&mut self, runtime: &RuntimeClient, error: String) -> ModuleResult<bool> {
        self.result = json!({"ok": false, "error": error});
        // Known allocations are closed even when encoding, native binding, or a
        // later control operation fails. Failed close attempts remain explicit;
        // the Python harness then tears down its isolated host in finally.
        if matches!(self.stage, Stage::TransportClose) {
            self.result["transport_cleanup_failed"] = json!(true);
            self.transport = None;
        }
        if matches!(self.stage, Stage::EncoderClose) {
            self.result["encoder_cleanup_failed"] = json!(true);
            self.encoder = None;
        }
        if let Some(stream) = self.transport {
            if self
                .next(
                    runtime,
                    TRANSPORT,
                    json!({"action": "close", "stream": stream}),
                    Stage::TransportClose,
                )
                .is_ok()
            {
                return Ok(false);
            }
            self.result["transport_cleanup_failed"] = json!(true);
            self.transport = None;
        }
        if let Some(stream) = self.encoder {
            if self
                .next(
                    runtime,
                    ENCODER,
                    json!({"action": "close", "stream": stream}),
                    Stage::EncoderClose,
                )
                .is_ok()
            {
                return Ok(false);
            }
            self.result["encoder_cleanup_failed"] = json!(true);
            self.encoder = None;
        }
        self.complete(runtime)
    }

    fn complete(&self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        match runtime.send_ephemeral(Command::Complete {
            request: self.request,
            payload: self.result.clone(),
        })? {
            Response::Ok => Ok(true),
            Response::Error { code, .. }
                if matches!(
                    code.as_str(),
                    "cancelled"
                        | "deadline_exceeded"
                        | "not_found"
                        | "already_completed"
                        | "request_ended"
                ) =>
            {
                Ok(true)
            }
            _ => Err("transport fixture could not complete its result".into()),
        }
    }

    fn next(
        &mut self,
        runtime: &RuntimeClient,
        service: &str,
        payload: Value,
        stage: Stage,
    ) -> ModuleResult<()> {
        self.sequence += 1;
        self.pending = call(runtime, self.request, self.sequence, service, payload)?;
        self.stage = stage;
        Ok(())
    }

    fn open_transport(&mut self, runtime: &RuntimeClient, payload: Value) -> ModuleResult<()> {
        let encoder = payload["stream"]
            .as_u64()
            .ok_or("encoder open omitted stream")?;
        self.encoder = Some(encoder);
        active_request(runtime, self.request)?;
        self.audio = encode_transport_packets(runtime, encoder)?;
        let encrypted = self.settings["encrypted"] == true;
        let mut open = json!({
            "action": "open",
            "peer_ip": "127.0.0.1",
            "video_port": self.settings["video_port"],
            "audio_port": self.settings["audio_port"],
            "audio": {
                "packet_ms": 10,
                "channels": 2,
                "wire_payload_bytes": if encrypted { 176 } else { 160 },
                "ssrc": 0,
            },
            "operation_id": format!("udp-probe-transport-{}", self.request),
        });
        if encrypted {
            open["encryption"] = json!({
                "key_hex": self.settings["key_hex"],
                "key_id": self.settings["key_id"],
                "video": true,
                "audio": true,
                "permit_legacy_audio_iv_wrap": false,
            });
        }
        self.next(runtime, TRANSPORT, open, Stage::TransportOpen)
    }

    fn send_media(&mut self, runtime: &RuntimeClient, payload: Value) -> ModuleResult<()> {
        let stream = payload["stream"]
            .as_u64()
            .ok_or("transport open omitted stream")?;
        self.transport = Some(stream);
        active_request(runtime, self.request)?;
        self.result = match submit_native(runtime, stream, &self.audio) {
            Ok(mut result) => {
                result["video_local"] = payload["video_local"].clone();
                result["audio_local"] = payload["audio_local"].clone();
                result
            }
            Err(error) => json!({"ok": false, "error": error}),
        };
        self.next(
            runtime,
            TRANSPORT,
            json!({"action": "status", "stream": stream}),
            Stage::TransportStatus,
        )
    }

    fn check_drained(&mut self, runtime: &RuntimeClient, payload: Value) -> ModuleResult<()> {
        let status = &payload["streams"][0];
        if status["state"] == "drained"
            || status["state"] == "failed"
            || Instant::now() >= self.deadline
            || self.result["ok"] != true
        {
            if self.result["ok"] == true && status["state"] != "drained" {
                self.result =
                    json!({"ok": false, "error": "transport did not drain before deadline"});
            }
            self.result["transport_status"] = status.clone();
            self.next(
                runtime,
                TRANSPORT,
                json!({"action": "close", "stream": self.transport}),
                Stage::TransportClose,
            )
        } else {
            self.next(
                runtime,
                TRANSPORT,
                json!({"action": "status", "stream": self.transport}),
                Stage::TransportStatus,
            )
        }
    }

    fn close_encoder(&mut self, runtime: &RuntimeClient) -> ModuleResult<()> {
        self.next(
            runtime,
            ENCODER,
            json!({"action": "close", "stream": self.encoder}),
            Stage::EncoderClose,
        )
    }
}

fn call(
    runtime: &RuntimeClient,
    parent: u64,
    sequence: u32,
    service: &str,
    payload: Value,
) -> ModuleResult<u64> {
    let authority_parent = (payload["action"] == "open").then_some(parent);
    match runtime.send_ephemeral(Command::Request {
        service: service.into(),
        payload,
        timeout_ms: 1000,
        operation_id: format!("udp-probe-{parent}-{sequence}"),
        parent: authority_parent,
    })? {
        Response::Accepted { id } => Ok(id),
        _ => Err("transport fixture could not queue control request".into()),
    }
}

fn active_request(runtime: &RuntimeClient, request: u64) -> ModuleResult<()> {
    match runtime.send_ephemeral(Command::CheckRequest { request })? {
        Response::Ok => Ok(()),
        _ => Err("transport fixture parent request is no longer active".into()),
    }
}

fn submit_native(runtime: &RuntimeClient, stream: u64, audio: &[Packet]) -> ModuleResult<Value> {
    // SAFETY: the signed provider supplies the exact media-v1 table. Binding,
    // input buffers, and metadata remain live throughout these synchronous calls.
    let binding =
        unsafe { runtime.bind_native(TRANSPORT, 1, 0, size_of::<MediaTransportApi>() as u32) }?;
    let api = unsafe { binding.table::<MediaTransportApi>() }?;
    let mut video = vec![0x55; 1700];
    video[..5].copy_from_slice(&[0, 0, 0, 1, 0x65]);
    let frame = EncodedPacket {
        codec: CODEC_H264,
        flags: FLAG_KEYFRAME,
        data: video.as_ptr(),
        len: video.len(),
        timestamp_us: 1_000_000,
        sequence: 1,
    };
    // SAFETY: all borrowed packet allocations outlive their synchronous submits.
    require_ok(
        unsafe { (api.submit_video)(api.base.context, stream, &frame) },
        "video submit",
    )?;
    for packet in audio {
        let encoded = EncodedAudioPacket {
            codec: CODEC_OPUS,
            flags: packet.flags,
            sample_rate: 48000,
            channels: 2,
            frames: packet.frames,
            data: packet.bytes.as_ptr(),
            len: packet.bytes.len(),
            timestamp_us: packet.timestamp_us,
            sequence: packet.sequence,
        };
        require_ok(
            unsafe { (api.submit_audio)(api.base.context, stream, &encoded) },
            "audio submit",
        )?;
    }
    require_ok(
        unsafe { (api.drain)(api.base.context, stream) },
        "transport drain",
    )?;
    let video_after_drain = unsafe { (api.submit_video)(api.base.context, stream, &frame) };
    if video_after_drain != amaterasu_media::MEDIA_CLOSED {
        return Err("transport accepted video after drain".into());
    }
    Ok(json!({
        "ok": true,
        "expected_video_hex": hex(&video),
        "expected_audio_hex": audio.iter().map(|packet| hex(&packet.bytes)).collect::<Vec<_>>(),
        "video_after_drain": video_after_drain,
    }))
}

fn require_ok(status: abi::Status, operation: &str) -> ModuleResult<()> {
    if status == abi::STATUS_OK {
        Ok(())
    } else {
        Err(format!("{operation} failed with {status}"))
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    output
}
