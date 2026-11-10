use crate::{
    datagrams::{Delivery, Medium},
    model::*,
    opus_packet::OpusPacket,
};
use amaterasu_gamestream::{
    audio::{AudioConfig, AudioError, AudioPacketizer},
    crypto::{AudioIvPolicy, AudioSealer, VIDEO_PREFIX_BYTES, VideoProfile, VideoSealer},
    video::{VideoCodec as WireVideoCodec, VideoConfig, VideoPacketizer},
};
use amaterasu_media::*;
use amaterasu_module_sdk::diagnostics::LatencyMetric;
use amaterasu_module_sdk::{
    abi::{STATUS_INVALID_ARGUMENT, STATUS_OK, Status},
    network::Network,
};
use std::{
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

pub struct Stream {
    pub owner: String,
    pub peer: IpAddr,
    pub video_settings: VideoSettings,
    pub video_batch_size_fallback: bool,
    pub audio_settings: AudioSettings,
    pub stream_audio: bool,
    pub ping_timeout_ms: u64,
    pub video_local: SocketAddr,
    pub audio_local: SocketAddr,
    pub delivery: Delivery,
    video: VideoPacketizer,
    video_stream_format: Option<amaterasu_module_sdk::VideoStreamFormat>,
    video_format_locked: bool,
    video_codec: Option<VideoCodec>,
    audio: AudioPacketizer,
    video_sealer: Option<VideoSealer>,
    audio_sealer: Option<AudioSealer>,
    pub permit_legacy_audio_iv_wrap: bool,
    video_ended: bool,
    audio_ended: bool,
    video_sequence: Option<u64>,
    audio_sequence: Option<u64>,
    video_timestamp: Option<u64>,
    pending_video: Option<PendingVideo>,
    next_frame_index: Option<u32>,
    pub discarded_audio_fec_shards: u64,
    processing_latency: ProcessingLatency,
}

/// Upper bounds, in tenths of a millisecond, of the latency histogram buckets.
/// A final overflow bucket holds everything slower than the last bound.
pub(crate) const LATENCY_BUCKET_BOUNDS_TENTHS_MS: [u16; 10] =
    [20, 30, 40, 50, 60, 80, 100, 150, 250, 500];

/// Wire latency in tenths of a millisecond. Unknown timestamps are tracked
/// separately so averages use known samples only.
#[derive(Default)]
pub(crate) struct ProcessingLatency {
    pub samples: u64,
    pub packetize_fec_encrypt_us: LatencyMetric,
    pub socket_flush_us: LatencyMetric,
    pub delivery: serde_json::Value,
    pub total_tenths_ms: u64,
    pub max_tenths_ms: u16,
    pub zero_samples: u64,
    pub unknown_samples: u64,
    /// A stall is a tail property, so averages and a single maximum cannot
    /// show how often it happens.
    pub buckets: [u64; LATENCY_BUCKET_BOUNDS_TENTHS_MS.len() + 1],
    /// Time spent handing admitted frames to the socket.
    pub flush_total_us: u64,
    pub flush_max_us: u64,
    /// Datagrams the socket refused at admission; they wait for a later tick.
    pub flush_deferred_datagrams: u64,
}

impl ProcessingLatency {
    fn record(&mut self, latency: Option<u16>) {
        self.samples = self.samples.saturating_add(1);
        if let Some(latency) = latency {
            self.total_tenths_ms = self.total_tenths_ms.saturating_add(u64::from(latency));
            self.max_tenths_ms = self.max_tenths_ms.max(latency);
            self.zero_samples = self.zero_samples.saturating_add(u64::from(latency == 0));
            let bucket = LATENCY_BUCKET_BOUNDS_TENTHS_MS
                .iter()
                .position(|bound| latency <= *bound)
                .unwrap_or(LATENCY_BUCKET_BOUNDS_TENTHS_MS.len());
            self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        } else {
            self.unknown_samples = self.unknown_samples.saturating_add(1);
        }
    }

    fn record_flush(&mut self, elapsed_us: u64, deferred_datagrams: usize) {
        self.socket_flush_us.record(elapsed_us);
        self.flush_total_us = self.flush_total_us.saturating_add(elapsed_us);
        self.flush_max_us = self.flush_max_us.max(elapsed_us);
        self.flush_deferred_datagrams = self
            .flush_deferred_datagrams
            .saturating_add(deferred_datagrams as u64);
    }

    /// Renders the histogram as `<=2.0ms:12,...,>50.0ms:0` for the window log.
    pub(crate) fn histogram(&self) -> String {
        let mut parts: Vec<String> = LATENCY_BUCKET_BOUNDS_TENTHS_MS
            .iter()
            .zip(self.buckets)
            .map(|(bound, count)| format!("<={:.1}ms:{count}", f64::from(*bound) / 10.0))
            .collect();
        let last_bound = LATENCY_BUCKET_BOUNDS_TENTHS_MS[LATENCY_BUCKET_BOUNDS_TENTHS_MS.len() - 1];
        parts.push(format!(
            ">{:.1}ms:{}",
            f64::from(last_bound) / 10.0,
            self.buckets[LATENCY_BUCKET_BOUNDS_TENTHS_MS.len()]
        ));
        parts.join(",")
    }
}

struct PendingVideo {
    sequence: u64,
    timestamp_us: u64,
    processing_started_host_us: u64,
    keyframe: bool,
    bytes: Vec<u8>,
}

impl Stream {
    #[cfg(test)]
    pub fn open(owner: &str, request: &Open) -> TransportResult<Self> {
        Self::open_with_network(&Network::test_provider(), owner, request)
    }

    pub fn open_with_network(
        network: &Network,
        owner: &str,
        request: &Open,
    ) -> TransportResult<Self> {
        if let Some(format) = request.video_stream_format {
            validate_video_stream_format(format)?;
        }
        request.validate_video_encryption_policy()?;
        let ping_timeout_ms = validate_ping_timeout_ms(request.ping_timeout_ms)?;
        let (video, audio) = packetizers(request)?;
        let (video_sealer, audio_sealer) = ciphers(request)?;
        let mut delivery = match (&request.discovery, request.video_port, request.audio_port) {
            (Some(discovery), None, None) => Delivery::discover_with_network(
                network,
                request.local(true),
                request.local(false),
                request.peer_ip,
                discovery.tokens()?,
                Duration::from_millis(ping_timeout_ms),
            ),
            (None, Some(video_port), Some(audio_port)) if video_port != 0 && audio_port != 0 => {
                Delivery::bind_with_network(
                    network,
                    request.local(true),
                    request.local(false),
                    SocketAddr::new(request.peer_ip, video_port),
                    SocketAddr::new(request.peer_ip, audio_port),
                )
            }
            _ => return Err("invalid_destination"),
        }
        .map_err(|_| "udp_bind_failed")?;
        let video_settings = request.video.effective();
        let video_codec = (video_settings.offered_codecs().len() == 1)
            .then(|| video_settings.offered_codecs()[0]);
        delivery.set_video_max_batch_size_kb(video_settings.max_batch_size_kb);
        let (video_local, audio_local) = delivery.addresses().map_err(|_| "udp_bind_failed")?;
        let mut stream = Self {
            owner: owner.into(),
            peer: request.peer_ip,
            video_settings,
            video_batch_size_fallback: request.video.batch_size_fallback(),
            audio_settings: request.audio,
            stream_audio: request.stream_audio,
            ping_timeout_ms,
            video_local,
            audio_local,
            delivery,
            video,
            video_stream_format: None,
            video_format_locked: false,
            video_codec,
            audio,
            video_sealer,
            audio_sealer,
            permit_legacy_audio_iv_wrap: request
                .encryption
                .as_ref()
                .is_some_and(|settings| settings.permit_legacy_audio_iv_wrap),
            video_ended: false,
            audio_ended: false,
            video_sequence: None,
            audio_sequence: None,
            video_timestamp: None,
            pending_video: None,
            next_frame_index: Some(1),
            discarded_audio_fec_shards: 0,
            processing_latency: ProcessingLatency::default(),
        };
        if let Some(format) = request.video_stream_format {
            stream.configure_video_format(format)?;
        }
        Ok(stream)
    }

    /// The codec this stream carries, once negotiated or fixed at open.
    pub fn video_codec(&self) -> Option<VideoCodec> {
        self.video_codec
    }

    pub fn encryption(&self) -> (bool, bool) {
        (self.video_sealer.is_some(), self.audio_sealer.is_some())
    }

    pub fn peer_port(&self, medium: Medium) -> Option<u16> {
        self.delivery.peer_port(medium)
    }

    pub fn peer_discovery(&self) -> bool {
        self.delivery.is_discovery()
    }

    pub fn ready(&self) -> bool {
        self.delivery.ready()
    }

    pub fn audio_iv_reuse_started(&self) -> bool {
        self.audio_sealer
            .as_ref()
            .is_some_and(AudioSealer::iv_reuse_started)
    }

    pub fn state(&self) -> &'static str {
        if self.delivery.failure.is_some() {
            "failed"
        } else if !self.delivery.ready() {
            "discovering"
        } else if self.video_ended && self.audio_ended {
            if self.delivery.queued() == 0 {
                "drained"
            } else {
                "draining"
            }
        } else {
            "sending"
        }
    }

    pub fn drain(&mut self) -> Status {
        if self.delivery.failure.is_some() {
            return MEDIA_BACKEND_ERROR;
        }
        if self.pending_video.is_some() {
            return STATUS_INVALID_ARGUMENT;
        }
        self.video_ended = true;
        self.audio_ended = true;
        STATUS_OK
    }

    pub fn configure_video_format(
        &mut self,
        format: amaterasu_module_sdk::VideoStreamFormat,
    ) -> TransportResult<()> {
        validate_video_stream_format(format)?;
        if self.video_sequence.is_some() || self.pending_video.is_some() || self.video_ended {
            return Err("video_format_already_in_use");
        }
        let codec = match format.codec {
            amaterasu_module_sdk::VideoCodec::H264 => VideoCodec::H264,
            amaterasu_module_sdk::VideoCodec::Hevc => VideoCodec::Hevc,
            amaterasu_module_sdk::VideoCodec::Av1 => VideoCodec::Av1,
        };
        if self
            .video_stream_format
            .is_some_and(|selected| selected != format)
            || (self.video_format_locked
                && self.video_codec.is_some_and(|selected| selected != codec))
        {
            return Err("video_format_conflict");
        }
        if !self.video_settings.offered_codecs().contains(&codec)
            && !self.video_settings.codecs.is_empty()
        {
            return Err("video_format_conflict");
        }
        self.video = video_packetizer(&self.video_settings, codec)?;
        self.video_settings.codec = codec;
        self.video_settings.codecs.clear();
        self.video_codec = Some(codec);
        self.video_stream_format = Some(format);
        self.video_format_locked = true;
        Ok(())
    }

    /// # Safety
    /// The packet and its initialized immutable bytes live through this call.
    pub unsafe fn submit_video(&mut self, packet: &EncodedPacket) -> Status {
        // Legacy packets provide no independent host processing clock.
        unsafe { self.submit_video_with_start(packet, 0) }
    }

    /// # Safety
    /// The packet and its initialized immutable bytes live through this call.
    pub unsafe fn submit_video_with_start(
        &mut self,
        packet: &EncodedPacket,
        processing_started_host_us: u64,
    ) -> Status {
        if self.delivery.failure.is_some() {
            return MEDIA_BACKEND_ERROR;
        }
        if !self.delivery.ready() {
            return MEDIA_BUSY;
        }
        if self.video_ended {
            return MEDIA_CLOSED;
        }
        if let Err(status) = packet.validate() {
            return status;
        }
        let codec = match self.select_video_codec(packet.codec) {
            Ok(codec) => codec,
            Err(status) => return status,
        };
        if packet.flags & FLAG_END_OF_STREAM != 0 {
            if self.pending_video.is_some() {
                return STATUS_INVALID_ARGUMENT;
            }
            self.video_ended = true;
            return STATUS_OK;
        }
        // HEVC and AV1 encoders may emit several fragments for one access
        // unit. Retain only a bounded owned copy until its final fragment;
        // neither a sequence nor a frame index is committed early. Legacy
        // H.264 packets without a boundary flag remain complete access units.
        let keyframe = packet.flags & FLAG_KEYFRAME != 0;
        if codec != VideoCodec::H264 {
            let end = packet.flags & FLAG_END_OF_ACCESS_UNIT != 0;
            let previous = self.pending_video.as_ref();
            if previous.is_some_and(|pending| {
                pending.sequence != packet.sequence
                    || pending.timestamp_us != packet.timestamp_us
                    || pending.processing_started_host_us != processing_started_host_us
            }) {
                return STATUS_INVALID_ARGUMENT;
            }
            let current_len = previous.map_or(0, |pending| pending.bytes.len());
            let Some(total_len) = current_len.checked_add(packet.len) else {
                return STATUS_INVALID_ARGUMENT;
            };
            if total_len > MAX_QUEUE_BYTES {
                return STATUS_INVALID_ARGUMENT;
            }
            // SAFETY: validated packet bytes are borrowed only during this call.
            let fragment = unsafe { packet.bytes().unwrap() };
            if !end {
                if previous.is_none()
                    && (self
                        .video_sequence
                        .is_some_and(|sequence| packet.sequence <= sequence)
                        || self
                            .video_timestamp
                            .is_some_and(|timestamp| packet.timestamp_us < timestamp))
                {
                    return STATUS_INVALID_ARGUMENT;
                }
                let pending = self.pending_video.get_or_insert_with(|| PendingVideo {
                    sequence: packet.sequence,
                    timestamp_us: packet.timestamp_us,
                    processing_started_host_us,
                    keyframe: false,
                    bytes: Vec::new(),
                });
                pending.keyframe |= keyframe;
                pending.bytes.extend_from_slice(fragment);
                return STATUS_OK;
            }
            if let Some(pending) = previous {
                let mut access_unit = Vec::with_capacity(total_len);
                access_unit.extend_from_slice(&pending.bytes);
                access_unit.extend_from_slice(fragment);
                let status = self.submit_access_unit(
                    &access_unit,
                    keyframe || pending.keyframe,
                    packet.sequence,
                    packet.timestamp_us,
                    processing_started_host_us,
                );
                if status == STATUS_OK {
                    self.pending_video = None;
                }
                return status;
            }
        }
        // SAFETY: caller's packet allocation is borrowed only through packetization.
        let access_unit = unsafe { packet.bytes().unwrap() };
        self.submit_access_unit(
            access_unit,
            keyframe,
            packet.sequence,
            packet.timestamp_us,
            processing_started_host_us,
        )
    }

    /// Match a packet's ABI codec against the stream. The first packet of an
    /// offered codec selects it; the packetizer has consumed no state yet, so
    /// it is rebuilt for that codec.
    fn select_video_codec(&mut self, abi_codec: u32) -> Result<VideoCodec, Status> {
        let codec = self
            .video_settings
            .offered_codecs()
            .iter()
            .copied()
            .find(|codec| codec.abi_id() == abi_codec)
            .ok_or(STATUS_INVALID_ARGUMENT)?;
        match self.video_codec {
            Some(selected) if selected == codec => Ok(codec),
            Some(_) => Err(STATUS_INVALID_ARGUMENT),
            None => {
                self.video = video_packetizer(&self.video_settings, codec)
                    .map_err(|_| STATUS_INVALID_ARGUMENT)?;
                self.video_codec = Some(codec);
                Ok(codec)
            }
        }
    }

    fn submit_access_unit(
        &mut self,
        access_unit: &[u8],
        keyframe: bool,
        sequence: u64,
        timestamp_us: u64,
        processing_started_host_us: u64,
    ) -> Status {
        if self
            .video_sequence
            .is_some_and(|previous| sequence <= previous)
            || self
                .video_timestamp
                .is_some_and(|previous| timestamp_us < previous)
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let Some(frame_index) = self.next_frame_index else {
            return self.fail("video_index_exhausted");
        };
        let reservation = match self.video.reservation(access_unit.len()) {
            Ok(reservation) => reservation,
            Err(_) => return STATUS_INVALID_ARGUMENT,
        };
        let bytes = reservation.bytes
            + if self.video_sealer.is_some() {
                reservation.datagrams * VIDEO_PREFIX_BYTES
            } else {
                0
            };
        if reservation.datagrams > MAX_DATAGRAMS || bytes > MAX_QUEUE_BYTES {
            return STATUS_INVALID_ARGUMENT;
        }
        if !self.delivery.can_queue(reservation.datagrams, bytes) {
            return MEDIA_BUSY;
        }
        let clock = (u128::from(timestamp_us) * 90_000 / 1_000_000) as u32;
        // Presentation time is the RTP clock; only the independent host-clock
        // capture start can describe processing time. Zero means unavailable.
        let now_us = host_monotonic_micros();
        let latency = if processing_started_host_us != 0
            && now_us != 0
            && now_us
                .checked_sub(processing_started_host_us)
                .is_some_and(|elapsed| elapsed <= 5_000_000)
        {
            Some(host_processing_latency_tenths_ms(
                processing_started_host_us,
                now_us,
            ))
        } else {
            None
        };
        let packetize_started = Instant::now();
        let datagrams = match self.video.packetize_with_latency(
            access_unit,
            keyframe,
            frame_index,
            clock,
            latency.unwrap_or(0),
        ) {
            Ok(datagrams) => datagrams,
            Err(_) => return STATUS_INVALID_ARGUMENT,
        };
        let datagrams = match self.seal_video(frame_index, datagrams) {
            Ok(datagrams) => datagrams,
            Err(code) => return self.fail(code),
        };
        self.delivery.push_video(frame_index, datagrams);
        self.processing_latency
            .packetize_fec_encrypt_us
            .record_since(packetize_started);
        self.processing_latency.record(latency);
        self.video_sequence = Some(sequence);
        self.video_timestamp = Some(timestamp_us);
        self.next_frame_index = frame_index.checked_add(1);
        self.flush_admitted_video();
        STATUS_OK
    }

    fn flush_admitted_video(&mut self) {
        let started = Instant::now();
        self.delivery.flush();
        let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.processing_latency
            .record_flush(elapsed_us, self.delivery.queued());
    }

    pub(crate) fn take_processing_latency(&mut self) -> ProcessingLatency {
        let mut sample = std::mem::take(&mut self.processing_latency);
        sample.delivery = self.delivery.take_timing();
        sample
    }

    /// # Safety
    /// The packet and its initialized immutable bytes live through this call.
    pub unsafe fn submit_audio(&mut self, packet: &OpusPacket) -> Status {
        if self.delivery.failure.is_some() {
            return MEDIA_BACKEND_ERROR;
        }
        if !self.delivery.ready() {
            return MEDIA_BUSY;
        }
        if self.audio_ended {
            return MEDIA_CLOSED;
        }
        if let Err(status) = packet.validate() {
            return status;
        }
        // GameStream's audio packetizer accepts only its negotiated Opus clock.
        if packet.codec != CODEC_OPUS || packet.sample_rate != 48_000 {
            return MEDIA_UNSUPPORTED;
        }
        if packet.flags & AUDIO_PACKET_END_OF_STREAM != 0 {
            self.audio_ended = true;
            return STATUS_OK;
        }
        if !self.stream_audio {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: packet validation established bounded shape; caller owns allocation validity.
        let opus = unsafe { packet.bytes().unwrap() };
        let discontinuity = match self.reserve_audio(packet, opus) {
            Ok(discontinuity) => discontinuity,
            Err(status) => return status,
        };
        if discontinuity {
            let reset = self.audio.reset_discontinuity();
            self.discarded_audio_fec_shards += u64::from(reset.discarded_shards);
            if reset.skipped_packets > 0
                && let Some(sealer) = &mut self.audio_sealer
                && sealer.skip_to(reset.next_sequence).is_err()
            {
                return self.fail("audio_iv_exhausted");
            }
        }
        let result = if let Some(sealer) = &mut self.audio_sealer {
            self.audio
                .packetize_sealed(opus, packet.frames, |sequence, bytes| {
                    sealer
                        .encrypt(sequence, bytes)
                        .map_err(|_| AudioError::EncryptionFailed)
                })
        } else {
            self.audio.packetize(opus, packet.frames)
        };
        match result {
            Ok(datagrams) => {
                self.delivery.push_audio(datagrams);
                self.audio_sequence = Some(packet.sequence);
                self.delivery.flush();
                STATUS_OK
            }
            Err(_) => self.fail("audio_packetization_failed"),
        }
    }

    fn reserve_audio(&self, packet: &OpusPacket, opus: &[u8]) -> Result<bool, Status> {
        if packet.channels != u32::from(self.audio_settings.channels) {
            return Err(MEDIA_UNSUPPORTED);
        }
        let discontinuity = packet.flags & AUDIO_PACKET_DISCONTINUITY != 0;
        if !discontinuity
            && self
                .audio_sequence
                .is_some_and(|previous| previous.checked_add(1) != Some(packet.sequence))
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if self.audio.validate_input(opus, packet.frames).is_err() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let wire_length = if self.audio_sealer.is_some() {
            (opus.len() / 16 + 1) * 16
        } else {
            opus.len()
        };
        if wire_length != self.audio_settings.wire_payload_bytes {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let count = if discontinuity {
            1
        } else {
            self.audio.required_datagrams()
        };
        if !self
            .delivery
            .can_queue(count, count * self.audio.maximum_datagram_bytes())
        {
            return Err(MEDIA_BUSY);
        }
        Ok(discontinuity)
    }

    fn seal_video(
        &mut self,
        frame_index: u32,
        datagrams: Vec<Vec<u8>>,
    ) -> TransportResult<Vec<Vec<u8>>> {
        let Some(sealer) = &mut self.video_sealer else {
            return Ok(datagrams);
        };
        datagrams
            .into_iter()
            .map(|packet| {
                sealer
                    .seal(frame_index, &packet)
                    .map_err(|_| "video_encryption_failed")
            })
            .collect()
    }

    fn fail(&mut self, code: &'static str) -> Status {
        self.delivery.fail(code);
        MEDIA_BACKEND_ERROR
    }
}

fn packetizers(request: &Open) -> TransportResult<(VideoPacketizer, AudioPacketizer)> {
    let encrypted_video = request
        .encryption
        .as_ref()
        .is_some_and(|settings| settings.video);
    if encrypted_video && request.video.packet_size > 65_507 - 16 - VIDEO_PREFIX_BYTES {
        return Err("invalid_video_configuration");
    }
    let codec = request.video.offered_codecs()[0];
    let video = video_packetizer(&request.video, codec)?;
    let audio = AudioPacketizer::new(AudioConfig {
        sample_rate: 48_000,
        channels: request.audio.channels,
        frames_per_packet: request.audio.packet_ms * 48,
        wire_payload_bytes: request.audio.wire_payload_bytes,
        ssrc: request.audio.ssrc,
    })
    .map_err(|_| "invalid_audio_configuration")?;
    Ok((video, audio))
}

/// Packet framing and FEC are codec independent; the codec selects only the
/// bitstream validation and frame-type source.
fn video_packetizer(
    settings: &VideoSettings,
    codec: VideoCodec,
) -> TransportResult<VideoPacketizer> {
    let wire_codec = match codec {
        VideoCodec::H264 => WireVideoCodec::H264,
        VideoCodec::Hevc => WireVideoCodec::Hevc,
        VideoCodec::Av1 => WireVideoCodec::Av1,
    };
    VideoPacketizer::new_with_codec(
        VideoConfig {
            packet_size: settings.packet_size,
            fec_percentage: settings.fec_percentage,
            min_parity_shards: settings.min_parity_shards,
            max_fec_blocks: settings.max_fec_blocks,
            encrypted: false,
        },
        wire_codec,
    )
    .map_err(|_| "invalid_video_configuration")
}

fn ciphers(request: &Open) -> TransportResult<(Option<VideoSealer>, Option<AudioSealer>)> {
    let (video_sealer, audio_sealer) = if let Some(settings) = &request.encryption {
        let key = settings.key()?;
        (
            if settings.video {
                Some(
                    VideoSealer::new(&key, VideoProfile::SunshineHostToClient)
                        .map_err(|_| "encryption_failed")?,
                )
            } else {
                None
            },
            (request.stream_audio && settings.audio).then(|| {
                AudioSealer::new(
                    &key,
                    settings.key_id,
                    if settings.permit_legacy_audio_iv_wrap {
                        AudioIvPolicy::PermitLegacyWrap
                    } else {
                        AudioIvPolicy::RejectReuse
                    },
                )
            }),
        )
    } else {
        (None, None)
    };
    Ok((video_sealer, audio_sealer))
}

#[cfg(test)]
pub(crate) mod tests;
