use crate::{
    model::*,
    platform,
    worker::{Backend, NativeBackend, Opened, Source},
};
use amaterasu_media::{
    AUDIO_SILENT, AUDIO_TIMESTAMP_ESTIMATED, AudioFrame, AudioSink, MEDIA_BACKEND_ERROR,
    MEDIA_CLOSED, MEDIA_PENDING, PCM_F32, PCM_I16,
};
use amaterasu_module_sdk::abi::{STATUS_OK, Status};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};

struct Stream {
    owner: String,
    requested_endpoint: String,
    endpoint: String,
    virtual_sink: Option<String>,
    channels: Option<u32>,
    host_audio_enabled: bool,
    capture_sink_without_routing: bool,
    format: Format,
    source: Box<dyn Source>,
    continuous_audio: Option<ContinuousAudio>,
    continuous: Option<ContinuousStream>,
    pending: Option<Packet>,
    failure: Option<&'static str>,
}

struct OperationRecord {
    stream: u64,
    endpoint: String,
    virtual_sink: Option<String>,
    channels: Option<u32>,
    host_audio_enabled: bool,
    capture_sink_without_routing: bool,
    continuous_audio: Option<ContinuousAudio>,
}

#[derive(Clone, Copy)]
struct OpenPolicy {
    host_audio_enabled: bool,
    capture_sink_without_routing: bool,
}

struct OpenRequest {
    endpoint: String,
    virtual_sink: Option<String>,
    channels: Option<u32>,
    continuous_audio: Option<ContinuousAudio>,
    policy: OpenPolicy,
    operation: Option<String>,
}

struct StreamOpen {
    requested_endpoint: String,
    virtual_sink: Option<String>,
    channels: Option<u32>,
    continuous_audio: Option<ContinuousAudio>,
    continuous: Option<ContinuousStream>,
    policy: OpenPolicy,
    opened: Opened,
}

struct ContinuousStream {
    packetizer: ContinuousPacketizer,
    ready: VecDeque<Packet>,
    terminal_failure: Option<&'static str>,
    packet_duration: Duration,
    deadline: Instant,
    started_at: Instant,
}

struct ContinuousPacketizer {
    format: Format,
    frames_per_packet: u32,
    bytes_per_packet: usize,
    packet_duration_us: u64,
    block_align: usize,
    pending_bytes: Vec<u8>,
    pending_timestamp_us: Option<u64>,
    pending_flags: u32,
    next_timestamp_us: Option<u64>,
    next_sequence: u64,
}

impl ContinuousStream {
    fn new(format: Format, continuous_audio: ContinuousAudio) -> AudioResult<Self> {
        continuous_audio.validate()?;
        let frames_per_packet = format
            .sample_rate
            .checked_mul(continuous_audio.packet_ms)
            .ok_or("unsupported_packet_size")?
            / 1000;
        let packetizer = ContinuousPacketizer::new(format, frames_per_packet)?;
        let packet_duration = Duration::from_millis(u64::from(continuous_audio.packet_ms));
        let started_at = Instant::now();
        Ok(Self {
            packetizer,
            ready: VecDeque::new(),
            terminal_failure: None,
            packet_duration,
            deadline: started_at + packet_duration,
            started_at,
        })
    }

    fn read(&mut self, source: &mut dyn Source) -> AudioResult<Option<Packet>> {
        if let Some(packet) = self.ready.pop_front() {
            self.deadline = Instant::now() + self.packet_duration;
            return Ok(Some(packet));
        }

        if let Some(error) = self.terminal_failure {
            return Err(error);
        }
        match source.read() {
            Ok(Some(captured)) => self.ready.extend(self.packetizer.push(captured)?),
            Ok(None) => {}
            Err(error) => {
                self.terminal_failure = Some(error);
                if let Some(packet) = self.packetizer.flush_partial()? {
                    self.deadline = Instant::now() + self.packet_duration;
                    return Ok(Some(packet));
                }
                return Err(error);
            }
        }

        if let Some(packet) = self.ready.pop_front() {
            self.deadline = Instant::now() + self.packet_duration;
            return Ok(Some(packet));
        }

        if Instant::now() < self.deadline {
            return Ok(None);
        }

        let elapsed_us = self
            .started_at
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        let packet = self.packetizer.idle(elapsed_us)?;
        self.deadline = Instant::now() + self.packet_duration;
        Ok(Some(packet))
    }
}

impl ContinuousPacketizer {
    fn new(format: Format, frames_per_packet: u32) -> AudioResult<Self> {
        let block_align = format.block_align();
        let bytes_per_packet = format.packet_bytes(frames_per_packet)?;
        let packet_duration_us = u64::from(frames_per_packet)
            .checked_mul(1_000_000)
            .ok_or("unsupported_packet_size")?
            / u64::from(format.sample_rate);
        Ok(Self {
            format,
            frames_per_packet,
            bytes_per_packet,
            packet_duration_us,
            block_align,
            pending_bytes: Vec::with_capacity(bytes_per_packet),
            pending_timestamp_us: None,
            pending_flags: 0,
            next_timestamp_us: None,
            next_sequence: 0,
        })
    }

    fn push(&mut self, packet: Packet) -> AudioResult<Vec<Packet>> {
        let expected_bytes = self.format.packet_bytes(packet.frames)?;
        if packet.bytes.len() != expected_bytes {
            return Err("invalid_audio_buffer");
        }

        // Device recovery starts a new capture timeline. Finish the old
        // partial frame before accepting samples from the recovered device.
        let mut ready = Vec::new();
        if packet.flags & amaterasu_media::AUDIO_DISCONTINUITY != 0
            && let Some(partial) = self.flush_partial()?
        {
            ready.push(partial);
        }
        let mut input_flags = packet.flags & !AUDIO_SILENT;
        let mut offset = 0;
        while offset < packet.bytes.len() {
            if self.pending_bytes.is_empty() {
                let input_frame_offset = offset / self.block_align;
                let input_offset_us = (input_frame_offset as u64).saturating_mul(1_000_000)
                    / u64::from(self.format.sample_rate);
                let input_timestamp = packet.timestamp_us.saturating_add(input_offset_us);
                self.pending_timestamp_us = Some(
                    self.next_timestamp_us
                        .map_or(input_timestamp, |next| input_timestamp.max(next)),
                );
            }
            self.pending_flags |= input_flags;
            input_flags = 0;

            let remaining = self.bytes_per_packet - self.pending_bytes.len();
            let copied = remaining.min(packet.bytes.len() - offset);
            self.pending_bytes
                .extend_from_slice(&packet.bytes[offset..offset + copied]);
            offset += copied;

            if self.pending_bytes.len() == self.bytes_per_packet {
                let timestamp_us = self
                    .pending_timestamp_us
                    .take()
                    .ok_or("audio_timeline_failed")?;
                let bytes = std::mem::replace(
                    &mut self.pending_bytes,
                    Vec::with_capacity(self.bytes_per_packet),
                );
                let flags = std::mem::take(&mut self.pending_flags);
                ready.push(self.packet(timestamp_us, flags, bytes)?);
            }
        }
        Ok(ready)
    }

    fn flush_partial(&mut self) -> AudioResult<Option<Packet>> {
        if self.pending_bytes.is_empty() {
            Ok(None)
        } else {
            self.idle(0).map(Some)
        }
    }

    fn idle(&mut self, elapsed_us: u64) -> AudioResult<Packet> {
        let synthetic_silence = self.pending_bytes.is_empty();
        let timestamp_us = if synthetic_silence {
            self.next_timestamp_us
                .map_or(elapsed_us, |next| elapsed_us.max(next))
        } else {
            self.pending_timestamp_us
                .take()
                .ok_or("audio_timeline_failed")?
        };
        let mut flags = std::mem::take(&mut self.pending_flags);
        if synthetic_silence {
            flags |= AUDIO_TIMESTAMP_ESTIMATED;
        }
        let mut bytes = std::mem::replace(
            &mut self.pending_bytes,
            Vec::with_capacity(self.bytes_per_packet),
        );
        bytes.resize(self.bytes_per_packet, 0);
        self.packet(timestamp_us, flags, bytes)
    }

    fn packet(&mut self, timestamp_us: u64, mut flags: u32, bytes: Vec<u8>) -> AudioResult<Packet> {
        if bytes.len() != self.bytes_per_packet {
            return Err("invalid_audio_buffer");
        }
        flags &= !AUDIO_SILENT;
        if self.is_silent(&bytes) {
            flags |= AUDIO_SILENT;
        }
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or("sequence_exhausted")?;
        self.next_timestamp_us = Some(timestamp_us.saturating_add(self.packet_duration_us));
        Ok(Packet {
            frames: self.frames_per_packet,
            flags,
            bytes,
            timestamp_us,
            sequence,
        })
    }

    fn is_silent(&self, bytes: &[u8]) -> bool {
        match self.format.code() {
            PCM_F32 => {
                let (samples, remainder) = bytes.as_chunks::<4>();
                remainder.is_empty()
                    && samples
                        .iter()
                        .all(|sample| f32::from_le_bytes(*sample) == 0.0)
            }
            PCM_I16 => {
                let (samples, remainder) = bytes.as_chunks::<2>();
                remainder.is_empty()
                    && samples
                        .iter()
                        .all(|sample| i16::from_le_bytes(*sample) == 0)
            }
            _ => false,
        }
    }
}

pub struct Controller {
    backend: Box<dyn Backend>,
    enabled: bool,
    next_stream: u64,
    streams: BTreeMap<u64, Stream>,
    operations: BTreeMap<(String, String), OperationRecord>,
}

pub struct ControlReply {
    pub payload: Value,
    pub created: Option<u64>,
}

impl Controller {
    pub fn new(enabled: bool) -> Self {
        Self {
            backend: Box::new(NativeBackend),
            enabled,
            next_stream: 0,
            streams: BTreeMap::new(),
            operations: BTreeMap::new(),
        }
    }

    pub fn handle(&mut self, owner: &str, operation: Operation) -> AudioResult<ControlReply> {
        let previous = self.next_stream;
        let payload = match operation {
            Operation::Enumerate => {
                let endpoints = if self.enabled {
                    self.backend.enumerate()?
                } else {
                    Vec::new()
                };
                let reply = json!({"ok":true,"enabled":self.enabled,"endpoints":endpoints});
                if serde_json::to_vec(&reply)
                    .map_err(|_| "endpoint_metadata_failed")?
                    .len()
                    > 12_000
                {
                    return Err("endpoint_metadata_limit");
                }
                reply
            }
            Operation::Open {
                endpoint,
                virtual_sink,
                channels,
                host_audio_enabled,
                operation_id,
                capture_sink_without_routing,
            } => self.open(
                owner,
                OpenRequest {
                    endpoint,
                    virtual_sink,
                    channels,
                    continuous_audio: None,
                    policy: OpenPolicy {
                        host_audio_enabled,
                        capture_sink_without_routing,
                    },
                    operation: operation_id,
                },
            )?,
            Operation::OpenContinuous {
                endpoint,
                virtual_sink,
                channels,
                host_audio_enabled,
                operation_id,
                capture_sink_without_routing,
                continuous_audio,
            } => self.open(
                owner,
                OpenRequest {
                    endpoint,
                    virtual_sink: virtual_sink.clone(),
                    channels,
                    continuous_audio: Some(continuous_audio),
                    policy: OpenPolicy {
                        host_audio_enabled,
                        capture_sink_without_routing: if virtual_sink.is_some() {
                            false
                        } else {
                            capture_sink_without_routing
                        },
                    },
                    operation: operation_id,
                },
            )?,
            Operation::Lookup { operation_id } => self.lookup(owner, &operation_id)?,
            Operation::Close { stream } => self.close(owner, stream)?,
            Operation::Status { stream } => self.status(owner, stream),
        };
        Ok(ControlReply {
            payload,
            created: (self.next_stream != previous).then_some(self.next_stream),
        })
    }

    fn open(&mut self, owner: &str, request: OpenRequest) -> AudioResult<Value> {
        let OpenRequest {
            endpoint,
            virtual_sink,
            mut channels,
            continuous_audio,
            policy,
            operation,
        } = request;
        if !self.enabled {
            return Err("disabled");
        }
        if let Some(continuous_audio) = continuous_audio {
            continuous_audio.validate()?;
        }
        if !endpoint.is_empty() {
            validate_endpoint(&endpoint)?;
        }
        if let Some(virtual_sink) = &virtual_sink {
            validate_virtual_sink(virtual_sink)?;
            let effective_channels = channels.unwrap_or(2);
            crate::virtual_sink::channel_layout(effective_channels)?;
            channels = Some(effective_channels);
            if policy.capture_sink_without_routing {
                return Err("invalid_open_policy");
            }
        } else if channels.is_some() {
            return Err("invalid_channel_layout");
        }
        if let Some(operation) = &operation {
            validate_operation(operation)?;
            if let Some(record) = self.operations.get(&(owner.into(), operation.clone())) {
                if record.endpoint != endpoint
                    || record.virtual_sink != virtual_sink
                    || record.channels != channels
                    || record.host_audio_enabled != policy.host_audio_enabled
                    || record.capture_sink_without_routing != policy.capture_sink_without_routing
                    || record.continuous_audio != continuous_audio
                {
                    return Err("operation_conflict");
                }
                let stream = self.streams.get(&record.stream).ok_or("stream_closed")?;
                return Ok(open_reply(record.stream, stream));
            }
            if self.operations.len() >= 128 {
                return Err("operation_limit");
            }
        }
        if self.streams.len() >= 4
            || self
                .streams
                .values()
                .filter(|stream| stream.owner == owner)
                .count()
                >= 2
        {
            return Err("stream_limit");
        }
        let opened = self.backend.open(
            &endpoint,
            virtual_sink.as_deref(),
            channels,
            policy.host_audio_enabled,
            policy.capture_sink_without_routing,
        )?;
        let continuous = continuous_audio
            .map(|configuration| ContinuousStream::new(opened.format, configuration))
            .transpose()?;
        let id = self.insert(
            owner,
            StreamOpen {
                requested_endpoint: endpoint.clone(),
                virtual_sink: virtual_sink.clone(),
                channels,
                continuous_audio,
                continuous,
                policy,
                opened,
            },
        )?;
        if let Some(operation) = operation {
            self.operations.insert(
                (owner.into(), operation),
                OperationRecord {
                    stream: id,
                    endpoint,
                    virtual_sink,
                    channels,
                    host_audio_enabled: policy.host_audio_enabled,
                    capture_sink_without_routing: policy.capture_sink_without_routing,
                    continuous_audio,
                },
            );
        }
        Ok(open_reply(id, &self.streams[&id]))
    }

    fn insert(&mut self, owner: &str, open: StreamOpen) -> AudioResult<u64> {
        let StreamOpen {
            requested_endpoint,
            virtual_sink,
            channels,
            continuous_audio,
            continuous,
            policy,
            opened,
        } = open;
        let id = self.next_stream.checked_add(1).ok_or("stream_limit")?;
        self.streams.insert(
            id,
            Stream {
                owner: owner.into(),
                requested_endpoint,
                endpoint: opened.endpoint,
                virtual_sink,
                channels,
                host_audio_enabled: policy.host_audio_enabled,
                capture_sink_without_routing: policy.capture_sink_without_routing,
                format: opened.format,
                source: opened.source,
                continuous_audio,
                continuous,
                pending: None,
                failure: None,
            },
        );
        self.next_stream = id;
        Ok(id)
    }

    fn lookup(&self, owner: &str, operation: &str) -> AudioResult<Value> {
        validate_operation(operation)?;
        let record = self
            .operations
            .get(&(owner.into(), operation.into()))
            .ok_or("stream_not_found")?;
        let stream = self.streams.get(&record.stream).ok_or("stream_not_found")?;
        Ok(open_reply(record.stream, stream))
    }

    pub fn close(&mut self, owner: &str, id: u64) -> AudioResult<Value> {
        self.owned(owner, id)?;
        self.streams.remove(&id);
        Ok(json!({"ok":true}))
    }

    /// Undo a newly created open whose reply was never delivered. An explicit
    /// close keeps its operation record so retries still report stream_closed.
    pub fn rollback_unpublished(&mut self, owner: &str, id: u64) {
        if self.owned(owner, id).is_err() {
            return;
        }
        self.operations
            .retain(|(record_owner, _), record| record_owner != owner || record.stream != id);
        self.streams.remove(&id);
    }

    fn owned(&mut self, owner: &str, id: u64) -> AudioResult<&mut Stream> {
        self.streams
            .get_mut(&id)
            .filter(|stream| stream.owner == owner)
            .ok_or("stream_not_found")
    }

    fn status(&self, owner: &str, requested: Option<u64>) -> Value {
        let streams: Vec<_> = self.streams.iter().filter(|(id,stream)| stream.owner == owner && requested.is_none_or(|requested| requested == **id)).map(|(id,stream)| {
            let health = stream.source.health();
            let failure = stream.failure.or(health.failure);
            let endpoint = health.active_endpoint.as_deref().unwrap_or(&stream.endpoint);
            json!({"stream":id,"endpoint":endpoint,"requested_endpoint":stream.requested_endpoint,"virtual_sink":stream.virtual_sink,"channels":stream.channels,"host_audio_enabled":stream.host_audio_enabled,"capture_sink_without_routing":stream.capture_sink_without_routing,"continuous_audio":stream.continuous_audio,"format":stream.format,"state":if failure.is_some() { "failed" } else if health.recovering { "recovering" } else { "capturing" },"failure":failure,"discontinuities":health.discontinuities,"dropped_packets":health.dropped_packets})
        }).collect();
        json!({"ok":true,"enabled":self.enabled,"audio_route_restore_pending":platform::route_restore_pending(),"streams":streams})
    }

    pub fn shutdown(&mut self) {
        self.enabled = false;
        self.streams.clear();
    }
    /// # Safety
    /// The sink context and callback must remain live through this synchronous
    /// call and may only borrow samples for its callback's duration.
    pub unsafe fn read(&mut self, owner: &str, id: u64, sink: AudioSink) -> Status {
        let Ok(stream) = self.owned(owner, id) else {
            return MEDIA_CLOSED;
        };
        // A retained packet or queued source packet may still be deliverable
        // after capture fails. Let the source report failure once it drains.
        if stream.pending.is_none() && stream.failure.is_some() {
            return MEDIA_BACKEND_ERROR;
        }
        if stream.pending.is_none() {
            let packet = if let Some(continuous) = &mut stream.continuous {
                continuous.read(stream.source.as_mut())
            } else {
                stream.source.read()
            };
            match packet {
                Ok(packet) => stream.pending = packet,
                Err(error) => {
                    stream.failure = Some(error);
                    return MEDIA_BACKEND_ERROR;
                }
            }
        }
        let Some(packet) = stream.pending.as_ref() else {
            return MEDIA_PENDING;
        };
        let frame = AudioFrame {
            sample_rate: stream.format.sample_rate,
            channels: stream.format.channels,
            format: stream.format.code(),
            frames: packet.frames,
            flags: packet.flags,
            data: packet.bytes.as_ptr(),
            len: packet.bytes.len(),
            timestamp_us: packet.timestamp_us,
            sequence: packet.sequence,
        };
        // SAFETY: pending owns all PCM bytes, retained on sink rejection.
        let status = unsafe { sink.send(&frame) };
        if status == STATUS_OK {
            stream.pending = None;
        }
        status
    }
}

fn open_reply(id: u64, stream: &Stream) -> Value {
    json!({"ok":true,"stream":id,"endpoint":stream.endpoint,"requested_endpoint":stream.requested_endpoint,"virtual_sink":stream.virtual_sink,"channels":stream.channels,"host_audio_enabled":stream.host_audio_enabled,"capture_sink_without_routing":stream.capture_sink_without_routing,"continuous_audio":stream.continuous_audio,"format":stream.format})
}

#[cfg(test)]
mod tests;
