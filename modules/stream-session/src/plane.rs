//! Bounded, synchronous native media forwarding. Provider resources belong to the
//! pipeline; this plane owns only bindings and bounded copied pending output.
mod diagnostics;
mod native;
mod packets;

use amaterasu_gamestream::control::{
    FeedbackEncodeError, HostFeedback, HostFeedbackDelivery, HostFeedbackMessage,
    encode_host_feedback,
};
use amaterasu_media::*;
use amaterasu_module_sdk::RuntimeClient;
use packets::{AudioPacket, PcmPacket, VideoAssembly, VideoPacket};
use std::collections::VecDeque;
use std::time::Instant;

pub type PlaneResult<T> = Result<T, &'static str>;
const READ_BUDGET: usize = 4;
// High-rate mouse/controller traffic must not share the four-packet media
// budget. Drain a bounded burst before media work so input cannot accumulate
// seconds of latency and overflow the transport's 256-message queue.
const CONTROL_READ_BUDGET: usize = 64;
const FEEDBACK_SEND_BUDGET: usize = 4;
const MAX_PENDING_FEEDBACK_MESSAGES: usize = 64;
const MAX_PENDING_FEEDBACK_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackEnqueueOutcome {
    /// The plane owns the validated event and will retry it until the transport
    /// accepts it or the stream is stopped.
    Queued,
    /// Best-effort PCM was dropped under bounded queue pressure. Sequence gaps
    /// are expected, and the caller may acknowledge this event locally.
    DroppedBestEffort,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackEnqueueError {
    Invalid(FeedbackEncodeError),
    QueueFull,
    Stopped,
}
// A 10 ms PCM source must be drained faster than video frames arrive. Keep
// this bounded so one active session cannot monopolize the module tick.
const PCM_READ_BUDGET: usize = 32;

#[derive(Clone, Copy)]
pub struct Handles {
    pub video: u64,
    pub hdr: bool,
    pub audio_capture: Option<u64>,
    pub audio_encoder: Option<u64>,
    pub audio_processor: Option<u64>,
    pub media_transport: u64,
    pub control_transport: u64,
    pub stream_audio: bool,
    pub audio_channels: u32,
    pub audio_sample_rate: u32,
    pub audio_format: Option<AudioFormatId>,
    pub audio_generation: u64,
    pub audio_max_unit_bytes: usize,
    pub audio_max_metadata_bytes: usize,
    pub audio_pcm: Option<(AudioFormatId, u32, AudioFormatId)>,
    pub encoded_audio_source: bool,
    pub processed_audio: Option<(u32, u32, AudioFormatId, u32, AudioFormatId)>,
    /// ABI codec identifier of the negotiated video codec.
    pub video_codec: u32,
}

#[derive(Debug)]
pub struct ControlRecord {
    pub message_type: u16,
    pub payload: Vec<u8>,
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RumbleCommand {
    pub slot: u16,
    pub low_frequency: u16,
    pub high_frequency: u16,
}

#[derive(Debug, Default)]
pub struct Progress {
    pub encode_timing: Option<EncodeTimingSnapshotV1>,
    pub controls: Vec<ControlRecord>,
    pub desktop_geometry: Option<CaptureGeometry>,
    /// Complete non-EOS access units admitted to the local transport queue.
    pub accepted_video_access_units: u64,
    /// Encoded audio and video payload bytes admitted to the local transport queue.
    pub accepted_media_payload_bytes: u64,
    /// Forward sequence gaps between accepted video access units.
    pub observed_video_sequence_gaps: u64,
}

#[derive(Debug)]
pub struct TickFailure {
    pub code: &'static str,
    /// Allocate only on failure; the successful per-frame path keeps telemetry
    /// inline without making the error variant as large as a full snapshot.
    pub progress: Box<Progress>,
}

pub struct Plane {
    pump: Pump<native::NativeBackend>,
}

impl Plane {
    pub fn new(
        runtime: &RuntimeClient,
        handles: Handles,
        providers: &crate::providers::ProviderServices,
    ) -> PlaneResult<Self> {
        let mut pump = Pump::with_media(
            native::NativeBackend::new(runtime, handles, providers)?,
            handles.stream_audio,
            handles.audio_channels,
            handles.video_codec,
        );
        pump.encoded_source = handles.encoded_audio_source;
        pump.processing = handles.audio_processor.is_some();
        pump.frame_trace = Some(diagnostics::FrameTrace::new(
            amaterasu_module_sdk::trace::Tracer::bind(runtime),
            handles.video,
        ));
        Ok(Self { pump })
    }

    pub fn timing_summary(&mut self) -> Option<serde_json::Value> {
        let mut summary = self.pump.timing.take_if_due(Instant::now())?;
        summary["pending_pcm"] = self.pump.pending_pcm.is_some().into();
        summary["pending_processed_pcm"] = self.pump.pending_processed.is_some().into();
        summary["pending_audio"] = self.pump.pending_audio.is_some().into();
        summary["pending_feedback_messages"] = self.pump.pending_feedback.len().into();
        Some(summary)
    }

    pub fn tick(&mut self, media_active: bool) -> Result<Progress, TickFailure> {
        self.pump.tick(media_active)
    }

    pub fn send_rumble(&mut self, command: RumbleCommand) -> PlaneResult<bool> {
        self.pump.backend.send_rumble(command)
    }

    /// Transfers one event into this session's bounded queue. `Queued` means the
    /// plane owns it; it does not mean the transport or client accepted it.
    #[allow(dead_code)] // The coordinator-owned Input poll call site is landing separately.
    pub fn enqueue_feedback(
        &mut self,
        feedback: HostFeedback,
    ) -> Result<FeedbackEnqueueOutcome, FeedbackEnqueueError> {
        self.pump.enqueue_feedback(feedback)
    }

    /// Stops new PCM capture and cancels pending feedback. The pipeline separately
    /// drains the video provider.
    pub fn stop(&mut self) {
        self.pump.stopping = true;
        self.pump.cancel_feedback();
    }

    /// Both EOS markers were accepted and transport draining has started. The
    /// pipeline must still wait for the transport's local UDP queue to empty.
    pub fn is_drained(&self) -> bool {
        self.pump.transport_draining
    }

    pub fn recover_audio(&mut self) {
        self.pump.audio_recovery_needed = false;
    }
}

enum ProcessorOutput {
    Frame(PcmPacket),
    Pending,
    End,
}

trait Backend {
    fn encode_timing(&mut self) -> PlaneResult<Option<EncodeTimingSnapshotV1>> {
        Ok(None)
    }
    fn desktop_geometry(&mut self) -> PlaneResult<Option<CaptureGeometry>> {
        Ok(None)
    }
    fn control(&mut self) -> PlaneResult<Option<ControlRecord>>;
    fn send_feedback(&mut self, message: &HostFeedbackMessage) -> PlaneResult<bool>;
    fn send_rumble(&mut self, command: RumbleCommand) -> PlaneResult<bool>;
    fn video(&mut self) -> PlaneResult<Option<VideoPacket>>;
    fn pcm(&mut self) -> PlaneResult<Option<PcmPacket>>;
    fn audio(&mut self) -> PlaneResult<Option<AudioPacket>>;
    fn submit_video(
        &mut self,
        packet: &EncodedPacket,
        processing_started_host_us: u64,
    ) -> PlaneResult<bool>;
    fn submit_pcm(&mut self, frame: &AudioFrame) -> PlaneResult<bool>;
    fn submit_processor(&mut self, _frame: &AudioFrame) -> PlaneResult<bool> {
        Err("audio_processor_unavailable")
    }
    fn processor_output(&mut self) -> PlaneResult<ProcessorOutput> {
        Err("audio_processor_unavailable")
    }
    fn drain_processor(&mut self) -> PlaneResult<bool> {
        Err("audio_processor_unavailable")
    }
    fn submit_audio(&mut self, packet: &AudioUnit) -> PlaneResult<bool>;
    fn drain_encoder(&mut self) -> PlaneResult<bool>;
    fn drain_transport(&mut self) -> PlaneResult<bool>;
}

struct Pump<B> {
    timing: diagnostics::ForwardingTiming,
    backend: B,
    stream_audio: bool,
    encoded_source: bool,
    processing: bool,
    audio_channels: u32,
    video: VideoAssembly,
    pending_pcm: Option<PcmPacket>,
    pending_processed: Option<PcmPacket>,
    processor_draining: bool,
    processor_ended: bool,
    pending_audio: Option<AudioPacket>,
    pending_feedback: VecDeque<HostFeedbackMessage>,
    pending_feedback_bytes: usize,
    stopping: bool,
    encoder_draining: bool,
    video_ended: bool,
    audio_ended: bool,
    transport_draining: bool,
    failure: Option<&'static str>,
    audio_recovery_needed: bool,
    last_accepted_video_sequence: Option<u64>,
    frame_trace: Option<diagnostics::FrameTrace>,
}

impl<B: Backend> Pump<B> {
    #[cfg(test)]
    fn new(backend: B) -> Self {
        Self::with_media(backend, true, 2, CODEC_H264)
    }

    fn with_media(backend: B, stream_audio: bool, audio_channels: u32, video_codec: u32) -> Self {
        Self {
            timing: diagnostics::ForwardingTiming::default(),
            backend,
            stream_audio,
            encoded_source: false,
            processing: false,
            audio_channels,
            video: VideoAssembly::new(video_codec),
            pending_pcm: None,
            pending_processed: None,
            processor_draining: false,
            processor_ended: false,
            pending_audio: None,
            pending_feedback: VecDeque::new(),
            pending_feedback_bytes: 0,
            stopping: false,
            encoder_draining: false,
            video_ended: false,
            audio_ended: false,
            transport_draining: false,
            failure: None,
            audio_recovery_needed: false,
            last_accepted_video_sequence: None,
            frame_trace: None,
        }
    }

    fn tick(&mut self, media_active: bool) -> Result<Progress, TickFailure> {
        self.timing.cadence.observe(Instant::now());
        if let Some(code) = self.failure {
            return Err(TickFailure {
                code,
                progress: Box::default(),
            });
        }
        let result = self.advance(media_active);
        if let Err(failure) = &result {
            if failure.code == "audio_encoder_failed" {
                self.audio_recovery_needed = true;
            } else {
                self.failure = Some(failure.code);
            }
        }
        result
    }

    fn advance(&mut self, media_active: bool) -> Result<Progress, TickFailure> {
        // Check the authenticated provider before any media effect. A failed or
        // closed control stream ends pumping; routing metadata grants no authority.
        let mut progress = Progress::default();
        // Telemetry is optional; failure never interrupts media forwarding.
        progress.encode_timing = self.backend.encode_timing().ok().flatten();
        progress.desktop_geometry =
            self.backend
                .desktop_geometry()
                .map_err(|code| TickFailure {
                    code,
                    progress: Box::default(),
                })?;
        let control_started = Instant::now();
        for _ in 0..CONTROL_READ_BUDGET {
            let message = match self.backend.control() {
                Ok(message) => message,
                Err(code) => {
                    return Err(TickFailure {
                        code,
                        progress: Box::new(progress),
                    });
                }
            };
            let Some(message) = message else {
                break;
            };
            progress.controls.push(message);
        }
        self.timing
            .window
            .control_read_us
            .record_since(control_started);
        self.timing.window.control_budget_exhausted = self
            .timing
            .window
            .control_budget_exhausted
            .saturating_add(u64::from(progress.controls.len() == CONTROL_READ_BUDGET));
        if let Err(code) = self.pump_feedback() {
            return Err(TickFailure {
                code,
                progress: Box::new(progress),
            });
        }
        if !media_active {
            return Ok(progress);
        }
        let video_started = Instant::now();
        let video_result = self.pump_video(&mut progress);
        self.timing
            .window
            .video_forward_us
            .record_since(video_started);
        if let Err(code) = video_result {
            return Err(TickFailure {
                code,
                progress: Box::new(progress),
            });
        }
        if !self.audio_recovery_needed {
            let audio_started = Instant::now();
            let audio_result = self.pump_audio(&mut progress);
            self.timing
                .window
                .audio_forward_us
                .record_since(audio_started);
            if let Err(code) = audio_result {
                return Err(TickFailure {
                    code,
                    progress: Box::new(progress),
                });
            }
        }
        if self.video_ended && self.audio_ended && !self.transport_draining {
            self.transport_draining = match self.backend.drain_transport() {
                Ok(draining) => draining,
                Err(code) => {
                    return Err(TickFailure {
                        code,
                        progress: Box::new(progress),
                    });
                }
            };
        }
        Ok(progress)
    }

    fn enqueue_feedback(
        &mut self,
        feedback: HostFeedback,
    ) -> Result<FeedbackEnqueueOutcome, FeedbackEnqueueError> {
        if self.stopping || self.failure.is_some() {
            return Err(FeedbackEnqueueError::Stopped);
        }
        let message = encode_host_feedback(feedback).map_err(FeedbackEnqueueError::Invalid)?;
        let message_bytes = message.payload.len();
        if message_bytes > MAX_PENDING_FEEDBACK_BYTES {
            return Err(FeedbackEnqueueError::QueueFull);
        }
        if message.delivery == HostFeedbackDelivery::SequencedUnreliable
            && self.would_exceed_feedback_bounds(message_bytes)
        {
            return Ok(FeedbackEnqueueOutcome::DroppedBestEffort);
        }
        while self.would_exceed_feedback_bounds(message_bytes) {
            let pcm_index = self
                .pending_feedback
                .iter()
                .position(|pending| pending.delivery == HostFeedbackDelivery::SequencedUnreliable);
            let Some(pcm_index) = pcm_index else {
                return Err(FeedbackEnqueueError::QueueFull);
            };
            let dropped = self
                .pending_feedback
                .remove(pcm_index)
                .expect("located PCM entry");
            self.pending_feedback_bytes -= dropped.payload.len();
        }
        self.pending_feedback_bytes += message_bytes;
        self.pending_feedback.push_back(message);
        Ok(FeedbackEnqueueOutcome::Queued)
    }

    fn would_exceed_feedback_bounds(&self, incoming_bytes: usize) -> bool {
        self.pending_feedback.len() >= MAX_PENDING_FEEDBACK_MESSAGES
            || self.pending_feedback_bytes.saturating_add(incoming_bytes)
                > MAX_PENDING_FEEDBACK_BYTES
    }

    fn cancel_feedback(&mut self) {
        self.pending_feedback.clear();
        self.pending_feedback_bytes = 0;
    }

    fn pump_feedback(&mut self) -> PlaneResult<()> {
        for _ in 0..FEEDBACK_SEND_BUDGET {
            let Some(message) = self.pending_feedback.front() else {
                break;
            };
            if !self.backend.send_feedback(message)? {
                break;
            }
            let accepted = self
                .pending_feedback
                .pop_front()
                .expect("front was present");
            self.pending_feedback_bytes -= accepted.payload.len();
        }
        Ok(())
    }

    fn pump_video(&mut self, progress: &mut Progress) -> PlaneResult<()> {
        for _ in 0..READ_BUDGET {
            if self.video_ended {
                break;
            }
            if self.video.ready() {
                let header = self.video.header().expect("ready assembly");
                if !self
                    .backend
                    .submit_video(&header, self.video.processing_started_host_us())?
                {
                    return Ok(());
                }
                self.record_video_admission(&header, progress);
                self.video_ended = header.flags == FLAG_END_OF_STREAM;
                self.video.clear();
                if self.video_ended {
                    break;
                }
            }
            let Some(packet) = self.backend.video()? else {
                break;
            };
            if let (Some(trace), Some(sequence)) =
                (self.frame_trace.as_mut(), packet.frame_sequence())
            {
                trace.packet_read(sequence);
            }
            self.video.push(packet)?;
        }
        // A complete single output sample is forwarded this tick, without waiting
        // for a subsequent frame or introducing an artificial frame of latency.
        if self.video.ready() && !self.video_ended {
            let header = self.video.header().expect("ready assembly");
            if self
                .backend
                .submit_video(&header, self.video.processing_started_host_us())?
            {
                self.record_video_admission(&header, progress);
                self.video_ended = header.flags == FLAG_END_OF_STREAM;
                self.video.clear();
            }
        }
        Ok(())
    }

    fn record_video_admission(&mut self, packet: &EncodedPacket, progress: &mut Progress) {
        if packet.flags == FLAG_END_OF_STREAM {
            return;
        }
        if let Some(trace) = &self.frame_trace {
            trace.transport_admitted(packet.sequence);
        }
        progress.accepted_video_access_units =
            progress.accepted_video_access_units.saturating_add(1);
        progress.accepted_media_payload_bytes = progress
            .accepted_media_payload_bytes
            .saturating_add(u64::try_from(packet.len).unwrap_or(u64::MAX));
        if let Some(previous) = self.last_accepted_video_sequence
            && packet.sequence > previous.saturating_add(1)
        {
            progress.observed_video_sequence_gaps = progress
                .observed_video_sequence_gaps
                .saturating_add(packet.sequence - previous - 1);
        }
        self.last_accepted_video_sequence = Some(packet.sequence);
    }

    fn record_audio_admission(&self, packet: &AudioUnit, progress: &mut Progress) {
        if packet.payload.len != 0 {
            progress.accepted_media_payload_bytes = progress
                .accepted_media_payload_bytes
                .saturating_add(u64::try_from(packet.payload.len).unwrap_or(u64::MAX));
        }
    }

    fn pump_audio(&mut self, progress: &mut Progress) -> PlaneResult<()> {
        if !self.stream_audio {
            if !self.audio_ended {
                let end = AudioUnit {
                    generation: 1,
                    format: AUDIO_FORMAT_OPUS,
                    metadata_schema: AudioFormatId {
                        namespace: 0,
                        value: 0,
                        version: 0,
                    },
                    flags: AUDIO_EXTENSIBLE_END,
                    duration_us: 0,
                    payload: amaterasu_module_sdk::abi::BorrowedBytes::new(&[]),
                    metadata: amaterasu_module_sdk::abi::BorrowedBytes::new(&[]),
                    timestamp_us: 0,
                    sequence: 0,
                };
                if !self
                    .timing
                    .window
                    .audio_transport_admission_us
                    .measure(|| self.backend.submit_audio(&end))?
                {
                    return Ok(());
                }
                self.audio_ended = true;
            }
            return Ok(());
        }

        // Output backpressure stops both encoder polling and further PCM capture.
        // Drain several PCM packets per tick: video encoding may make ticks far
        // slower than the capture device's 10 ms packet cadence.
        if self.encoded_source && self.stopping && !self.encoder_draining {
            self.encoder_draining = self.backend.drain_encoder()?;
        }
        for _ in 0..PCM_READ_BUDGET {
            for _ in 0..READ_BUDGET {
                if let Some(packet) = &self.pending_audio {
                    let header = packet.unit_header();
                    if !self
                        .timing
                        .window
                        .audio_transport_admission_us
                        .measure(|| self.backend.submit_audio(&header))?
                    {
                        return Ok(());
                    }
                    self.record_audio_admission(&header, progress);
                    self.audio_ended = header.flags & AUDIO_EXTENSIBLE_END != 0;
                    self.pending_audio = None;
                }
                if self.audio_ended {
                    return Ok(());
                }
                let Some(packet) = self
                    .timing
                    .window
                    .audio_encoded_read_us
                    .measure(|| self.backend.audio())?
                else {
                    break;
                };
                self.pending_audio = Some(packet);
            }
            if self.encoded_source {
                break;
            }
            if self.processing {
                let mut advanced = false;
                if self.pending_processed.is_none() && !self.processor_ended {
                    match self
                        .timing
                        .window
                        .audio_processor_read_us
                        .measure(|| self.backend.processor_output())?
                    {
                        ProcessorOutput::Frame(packet) => self.pending_processed = Some(packet),
                        ProcessorOutput::Pending => {}
                        ProcessorOutput::End if self.processor_draining => {
                            self.processor_ended = true
                        }
                        ProcessorOutput::End => return Err("audio_processor_closed"),
                    }
                }
                if let Some(packet) = &self.pending_processed {
                    if !self
                        .timing
                        .window
                        .audio_encoder_submit_us
                        .measure(|| self.backend.submit_pcm(&packet.header()))?
                    {
                        return Ok(());
                    }
                    self.pending_processed = None;
                    advanced = true;
                }
                if self.pending_pcm.is_none() && !self.stopping {
                    self.pending_pcm = self
                        .timing
                        .window
                        .audio_capture_us
                        .measure(|| self.backend.pcm())?;
                }
                if let Some(packet) = &self.pending_pcm {
                    if packet.header().channels != self.audio_channels {
                        return Err("unsupported_audio_channels");
                    }
                    if !self
                        .timing
                        .window
                        .audio_processor_submit_us
                        .measure(|| self.backend.submit_processor(&packet.header()))?
                    {
                        return Ok(());
                    }
                    self.pending_pcm = None;
                    advanced = true;
                } else if self.stopping && !self.processor_draining {
                    self.processor_draining = self.backend.drain_processor()?;
                    advanced |= self.processor_draining;
                }
                if !advanced {
                    break;
                }
                continue;
            }
            if self.pending_pcm.is_none() && !self.stopping {
                self.pending_pcm = self
                    .timing
                    .window
                    .audio_capture_us
                    .measure(|| self.backend.pcm())?;
            }
            let Some(packet) = &self.pending_pcm else {
                break;
            };
            if packet.header().channels != self.audio_channels {
                return Err("unsupported_audio_channels");
            }
            if !self
                .timing
                .window
                .audio_encoder_submit_us
                .measure(|| self.backend.submit_pcm(&packet.header()))?
            {
                return Ok(());
            }
            self.pending_pcm = None;
        }
        if !self.encoded_source
            && self.stopping
            && !self.encoder_draining
            && (!self.processing || self.processor_ended)
        {
            self.encoder_draining = self.backend.drain_encoder()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
