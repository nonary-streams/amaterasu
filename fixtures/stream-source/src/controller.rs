use amaterasu_media::*;
use amaterasu_module_sdk::abi::{STATUS_OK, Status};
use amaterasu_module_sdk::{EncoderSelection, validate_software_options};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Instant;

pub type Result<T> = std::result::Result<T, &'static str>;
pub type Reply = (Value, Option<(Kind, u64)>);
const FRAMES: u64 = 8;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Video,
    Audio,
}

impl Kind {
    pub fn service(self) -> &'static str {
        match self {
            Self::Video => "amaterasu.video/1",
            Self::Audio => "amaterasu.audio/1",
        }
    }
}

struct Stream {
    owner: String,
    kind: Kind,
    delivered: u64,
    draining: bool,
    eos: bool,
    unsupported_audio: bool,
    geometry_failed: bool,
    started: Instant,
    sustain_ms: u32,
}

impl Stream {
    fn video_state(&self) -> &'static str {
        if self.geometry_failed {
            "failed"
        } else if self.draining {
            "draining"
        } else {
            "running"
        }
    }
}

struct Operation {
    stream: u64,
    payload: Value,
}

#[derive(Default)]
pub struct Controller {
    streams: BTreeMap<u64, Stream>,
    operations: BTreeMap<(String, Kind, String), Operation>,
    next: u64,
    video_opens: u64,
    audio_opens: u64,
    video_closes: u64,
    audio_closes: u64,
    video_drains: u64,
    video_eos: u64,
    last_video_policy: Option<Value>,
    force_idr: u64,
    video_fragments: u64,
    audio_packets: u64,
    desktop_geometry: bool,
    sustain_ms: u32,
}

impl Controller {
    #[cfg(test)]
    pub fn new(desktop_geometry: bool) -> Self {
        Self::with_duration(desktop_geometry, 0)
    }

    pub fn with_duration(desktop_geometry: bool, sustain_ms: u32) -> Self {
        Self {
            desktop_geometry,
            sustain_ms,
            ..Self::default()
        }
    }

    pub fn handle(&mut self, owner: &str, service: &str, payload: Value) -> Result<Reply> {
        let action = payload["action"].as_str().ok_or("invalid_request")?;
        if service == crate::METRICS {
            return match action {
                "status" => Ok((self.metrics(), None)),
                "invalidate_geometry" if self.desktop_geometry => {
                    for stream in self.streams.values_mut() {
                        if stream.kind == Kind::Video {
                            stream.geometry_failed = true;
                        }
                    }
                    Ok((json!({"ok": true}), None))
                }
                _ => Err("unsupported_action"),
            };
        }
        let kind = match service {
            "amaterasu.video/1" => Kind::Video,
            "amaterasu.audio/1" => Kind::Audio,
            _ => return Err("invalid_service"),
        };
        if action == "open" {
            return self.open(owner, kind, payload);
        }
        let response = match action {
            "lookup" => {
                let operation = operation_id(&payload)?.ok_or("invalid_operation_id")?;
                let id = self
                    .operations
                    .get(&(owner.into(), kind, operation.into()))
                    .ok_or("stream_not_found")?
                    .stream;
                self.metadata(owner, kind, id)?
            }
            "status" => {
                if let Some(id) = payload["stream"].as_u64() {
                    self.metadata(owner, kind, id)?
                } else {
                    let streams: Vec<_> = self
                        .streams
                        .iter()
                        .filter(|(_, stream)| stream.owner == owner && stream.kind == kind)
                        .map(|(&id, _)| {
                            self.metadata(owner, kind, id)
                                .expect("selected owned stream")
                        })
                        .collect();
                    json!({"ok": true, "enabled": true, "streams": streams})
                }
            }
            "close" => self.close(owner, kind, stream_id(&payload)?)?,
            "drain" if kind == Kind::Video => {
                let stream = self.owned_mut(owner, kind, stream_id(&payload)?)?;
                if !stream.draining {
                    stream.draining = true;
                    self.video_drains += 1;
                }
                json!({"ok": true})
            }
            "force_idr" if kind == Kind::Video => {
                self.owned_mut(owner, kind, stream_id(&payload)?)?;
                self.force_idr += 1;
                json!({"ok": true})
            }
            "enumerate" if kind == Kind::Audio => json!({
                "ok": true, "enabled": true,
                "endpoints": [{"id": "synthetic-render", "name": "Synthetic PCM fixture", "default_multimedia": true}]
            }),
            _ => return Err("unsupported_action"),
        };
        Ok((response, None))
    }

    fn open(&mut self, owner: &str, kind: Kind, payload: Value) -> Result<Reply> {
        validate_open(kind, &payload)?;
        let policy = (kind == Kind::Video)
            .then(|| video_policy_snapshot(&payload))
            .transpose()?;
        let operation = operation_id(&payload)?.map(str::to_owned);
        if let Some(operation) = &operation
            && let Some(record) = self
                .operations
                .get(&(owner.into(), kind, operation.clone()))
        {
            if record.payload != payload {
                return Err("operation_conflict");
            }
            return Ok((self.metadata(owner, kind, record.stream)?, None));
        }
        if self.streams.len() >= 16 || self.next >= 128 {
            return Err("fixture_stream_limit");
        }
        self.next += 1;
        let id = self.next;
        self.streams.insert(
            id,
            Stream {
                owner: owner.into(),
                kind,
                delivered: 0,
                draining: false,
                eos: false,
                unsupported_audio: payload["endpoint"] == "synthetic-unsupported-format",
                geometry_failed: false,
                started: Instant::now(),
                sustain_ms: self.sustain_ms,
            },
        );
        match kind {
            Kind::Video => {
                self.video_opens += 1;
                self.last_video_policy = policy;
            }
            Kind::Audio => self.audio_opens += 1,
        }
        if let Some(operation) = operation {
            self.operations.insert(
                (owner.into(), kind, operation),
                Operation {
                    stream: id,
                    payload,
                },
            );
        }
        Ok((self.metadata(owner, kind, id)?, Some((kind, id))))
    }

    fn owned_mut(&mut self, owner: &str, kind: Kind, id: u64) -> Result<&mut Stream> {
        self.streams
            .get_mut(&id)
            .filter(|stream| stream.owner == owner && stream.kind == kind)
            .ok_or("stream_not_found")
    }

    fn metadata(&self, owner: &str, kind: Kind, id: u64) -> Result<Value> {
        let stream = self
            .streams
            .get(&id)
            .filter(|stream| stream.owner == owner && stream.kind == kind)
            .ok_or("stream_not_found")?;
        let mut response = match kind {
            Kind::Video => json!({
                "ok": true, "stream": id,
                "state": stream.video_state(),
                "packet_boundaries": true, "supports_packet_boundaries": true,
                "fixture": true, "decodable_video": stream.sustain_ms != 0
            }),
            Kind::Audio => json!({
                "ok": true, "stream": id,
                "endpoint": if stream.unsupported_audio { "synthetic-unsupported-format" } else { "synthetic-render" },
                "format": {"sample_rate": if stream.unsupported_audio { 44100 } else { 48000 }, "channels": 2, "channel_mask": 3, "format": "i16"},
                "fixture": true
            }),
        };
        if kind == Kind::Video && self.desktop_geometry {
            response["desktop_geometry"] = json!({
                "output": {"left": -1920, "top": 0, "width": 1920, "height": 1080},
                "virtual_desktop": {"left": -1920, "top": -200, "width": 3840, "height": 1440},
            });
        }
        Ok(response)
    }

    pub fn close(&mut self, owner: &str, kind: Kind, id: u64) -> Result<Value> {
        self.owned_mut(owner, kind, id)?;
        self.streams.remove(&id);
        match kind {
            Kind::Video => self.video_closes += 1,
            Kind::Audio => self.audio_closes += 1,
        }
        Ok(json!({"ok": true}))
    }

    fn metrics(&self) -> Value {
        json!({
            "ok": true, "fixture": true,
            "video": {
                "opened": self.video_opens, "closed": self.video_closes,
                "active": self.streams.values().filter(|stream| stream.kind == Kind::Video).count(),
                "frames": self.video_fragments / 2, "fragments": self.video_fragments,
                "force_idr": self.force_idr,
                "drains": self.video_drains, "eos": self.video_eos,
                "last_open_policy": self.last_video_policy
            },
            "audio": {
                "opened": self.audio_opens, "closed": self.audio_closes,
                "active": self.streams.values().filter(|stream| stream.kind == Kind::Audio).count(),
                "frames": self.audio_packets
            }
        })
    }

    pub fn shutdown(&mut self) {
        self.streams.clear();
    }

    /// # Safety
    /// The sink and context remain live through the synchronous callback.
    pub unsafe fn video(&mut self, owner: &str, id: u64, sink: PacketSink) -> Status {
        let Ok(stream) = self.owned_mut(owner, Kind::Video, id) else {
            return MEDIA_CLOSED;
        };
        if stream.geometry_failed {
            return MEDIA_BACKEND_ERROR;
        }
        if stream.eos {
            return MEDIA_PENDING;
        }
        let sustained = stream.sustain_ms != 0;
        let frame_limit = if sustained {
            u64::from(stream.sustain_ms) * 30 / 1000
        } else {
            FRAMES
        };
        if stream.delivered >= frame_limit * 2 || (sustained && stream.draining) {
            if !stream.draining {
                return MEDIA_PENDING;
            }
            let packet = EncodedPacket {
                codec: CODEC_H264,
                flags: FLAG_END_OF_STREAM,
                data: std::ptr::null(),
                len: 0,
                timestamp_us: 0,
                sequence: 0,
            };
            let status = unsafe { sink.send(&packet) };
            if status == STATUS_OK {
                stream.eos = true;
                self.video_eos += 1;
            }
            return status;
        }
        let sequence = stream.delivered / 2;
        if sustained && stream.started.elapsed().as_micros() < u128::from(sequence) * 1_000_000 / 30
        {
            return MEDIA_PENDING;
        }
        let last = stream.delivered % 2 == 1;
        let complete = if sustained {
            include_bytes!("sustained-idr.h264").to_vec()
        } else {
            video_bytes(sequence)
        };
        let midpoint = complete.len() / 2;
        let bytes = if last {
            &complete[midpoint..]
        } else {
            &complete[..midpoint]
        };
        let packet = EncodedPacket {
            codec: CODEC_H264,
            flags: if last {
                FLAG_END_OF_ACCESS_UNIT
            } else {
                FLAG_KEYFRAME
            },
            data: bytes.as_ptr(),
            len: bytes.len(),
            timestamp_us: if sustained {
                sequence * 1_000_000 / 30
            } else {
                sequence * 10_000
            },
            sequence,
        };
        // SAFETY: local packet and initialized bytes remain live until sink returns.
        let status = unsafe { sink.send(&packet) };
        if status == STATUS_OK {
            stream.delivered += 1;
            self.video_fragments += 1;
        }
        status
    }

    /// # Safety
    /// The sink/context remain live and may only borrow samples during the call.
    pub unsafe fn audio(&mut self, owner: &str, id: u64, sink: AudioSink) -> Status {
        let Ok(stream) = self.owned_mut(owner, Kind::Audio, id) else {
            return MEDIA_CLOSED;
        };
        let sustained = stream.sustain_ms != 0;
        let frame_limit = if sustained {
            u64::from(stream.sustain_ms) / 10
        } else {
            FRAMES
        };
        if stream.delivered >= frame_limit {
            return MEDIA_PENDING;
        }
        let sequence = stream.delivered;
        if sustained && stream.started.elapsed().as_micros() < u128::from(sequence) * 10_000 {
            return MEDIA_PENDING;
        }
        let frames = if stream.unsupported_audio { 441 } else { 480 };
        let mut bytes = Vec::with_capacity(frames * 4);
        for frame in 0..frames as i16 {
            let sample =
                ((i64::from(frame) + (sequence % 100) as i64 * 13) % 100 - 50) as i16 * 100;
            bytes.extend_from_slice(&sample.to_le_bytes());
            bytes.extend_from_slice(&(-sample).to_le_bytes());
        }
        let packet = AudioFrame {
            sample_rate: if stream.unsupported_audio {
                44100
            } else {
                48000
            },
            channels: 2,
            format: PCM_I16,
            frames: frames as u32,
            flags: 0,
            data: bytes.as_ptr(),
            len: bytes.len(),
            timestamp_us: sequence * 10_000,
            sequence,
        };
        // SAFETY: all PCM bytes are initialized and live until the callback returns.
        let status = unsafe { sink.send(&packet) };
        if status == STATUS_OK {
            stream.delivered += 1;
            self.audio_packets += 1;
        }
        status
    }
}

/// Only bounded policy fields are observable; unrelated request data never enters metrics.
fn video_policy_snapshot(payload: &Value) -> Result<Value> {
    let encoder = match payload.get("encoder") {
        Some(value) => {
            let selection: EncoderSelection =
                serde_json::from_value(value.clone()).map_err(|_| "invalid_encoder_selection")?;
            selection.validate()?;
            value.clone()
        }
        None => Value::Null,
    };
    let tuning = |key| match payload["configuration"].get(key) {
        Some(Value::String(value)) => Ok(Some(value.as_str())),
        None | Some(Value::Null) => Ok(None),
        _ => Err("invalid_software_encoder_options"),
    };
    let preset = tuning("sw_preset")?;
    let tune = tuning("sw_tune")?;
    validate_software_options(preset, tune)?;
    Ok(json!({"encoder": encoder, "sw_preset": preset, "sw_tune": tune}))
}

fn video_bytes(sequence: u64) -> Vec<u8> {
    let mut bytes = vec![0, 0, 0, 1, 0x65];
    bytes.extend((5..1700).map(|index| ((index * 17 + sequence * 13) % 251 + 1) as u8));
    bytes
}

fn operation_id(payload: &Value) -> Result<Option<&str>> {
    if payload["operation_id"].is_null() {
        return Ok(None);
    }
    payload["operation_id"]
        .as_str()
        .filter(|value| {
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        })
        .map(Some)
        .ok_or("invalid_operation_id")
}
fn stream_id(payload: &Value) -> Result<u64> {
    payload["stream"].as_u64().ok_or("invalid_stream")
}
fn validate_open(kind: Kind, payload: &Value) -> Result<()> {
    match kind {
        Kind::Video
            if payload["output"] == "synthetic-display" && payload["packet_boundaries"] == true =>
        {
            Ok(())
        }
        Kind::Audio
            if matches!(
                payload["endpoint"].as_str(),
                Some("default" | "synthetic-render" | "synthetic-unsupported-format")
            ) =>
        {
            Ok(())
        }
        _ => Err("unsupported_fixture_configuration"),
    }
}

#[cfg(test)]
mod tests;
