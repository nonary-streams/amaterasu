//! Bounded, synchronous native media forwarding. Provider resources belong to the
//! pipeline; this plane owns only bindings and copied pending output.
mod native;
mod packets;

use amaterasu_media::*;
use amaterasu_module_sdk::RuntimeClient;
use packets::{AudioPacket, PcmPacket, VideoAssembly, VideoPacket};

pub type PlaneResult<T> = Result<T, &'static str>;
const READ_BUDGET: usize = 4;

#[derive(Clone, Copy)]
pub struct Handles {
    pub video: u64,
    pub audio_capture: Option<u64>,
    pub audio_encoder: Option<u64>,
    pub media_transport: u64,
    pub control_transport: u64,
    pub stream_audio: bool,
}

#[derive(Debug)]
pub struct ControlRecord {
    pub message_type: u16,
    pub payload: Vec<u8>,
    pub sequence: u64,
}

#[derive(Debug, Default)]
pub struct Progress {
    pub controls: Vec<ControlRecord>,
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
    pub progress: Progress,
}

pub struct Plane {
    pump: Pump<native::NativeBackend>,
}

impl Plane {
    pub fn new(runtime: &RuntimeClient, handles: Handles) -> PlaneResult<Self> {
        Ok(Self {
            pump: Pump::with_audio(
                native::NativeBackend::new(runtime, handles)?,
                handles.stream_audio,
            ),
        })
    }

    pub fn tick(&mut self, media_active: bool) -> Result<Progress, TickFailure> {
        self.pump.tick(media_active)
    }

    /// Stops new PCM capture. The pipeline separately drains the video provider.
    pub fn stop(&mut self) {
        self.pump.stopping = true;
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

trait Backend {
    fn control(&mut self) -> PlaneResult<Option<ControlRecord>>;
    fn video(&mut self) -> PlaneResult<Option<VideoPacket>>;
    fn pcm(&mut self) -> PlaneResult<Option<PcmPacket>>;
    fn audio(&mut self) -> PlaneResult<Option<AudioPacket>>;
    fn submit_video(&mut self, packet: &EncodedPacket) -> PlaneResult<bool>;
    fn submit_pcm(&mut self, frame: &AudioFrame) -> PlaneResult<bool>;
    fn submit_audio(&mut self, packet: &EncodedAudioPacket) -> PlaneResult<bool>;
    fn drain_encoder(&mut self) -> PlaneResult<bool>;
    fn drain_transport(&mut self) -> PlaneResult<bool>;
}

struct Pump<B> {
    backend: B,
    stream_audio: bool,
    video: VideoAssembly,
    pending_pcm: Option<PcmPacket>,
    pending_audio: Option<AudioPacket>,
    stopping: bool,
    encoder_draining: bool,
    video_ended: bool,
    audio_ended: bool,
    transport_draining: bool,
    failure: Option<&'static str>,
    audio_recovery_needed: bool,
    last_accepted_video_sequence: Option<u64>,
}

impl<B: Backend> Pump<B> {
    #[cfg(test)]
    fn new(backend: B) -> Self {
        Self::with_audio(backend, true)
    }

    fn with_audio(backend: B, stream_audio: bool) -> Self {
        Self {
            backend,
            stream_audio,
            video: VideoAssembly::default(),
            pending_pcm: None,
            pending_audio: None,
            stopping: false,
            encoder_draining: false,
            video_ended: false,
            audio_ended: false,
            transport_draining: false,
            failure: None,
            audio_recovery_needed: false,
            last_accepted_video_sequence: None,
        }
    }

    fn tick(&mut self, media_active: bool) -> Result<Progress, TickFailure> {
        if let Some(code) = self.failure {
            return Err(TickFailure {
                code,
                progress: Progress::default(),
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
        for _ in 0..READ_BUDGET {
            let message = match self.backend.control() {
                Ok(message) => message,
                Err(code) => {
                    return Err(TickFailure { code, progress });
                }
            };
            let Some(message) = message else {
                break;
            };
            progress.controls.push(message);
        }
        if !media_active {
            return Ok(progress);
        }
        if let Err(code) = self.pump_video(&mut progress) {
            return Err(TickFailure { code, progress });
        }
        if !self.audio_recovery_needed
            && let Err(code) = self.pump_audio(&mut progress)
        {
            return Err(TickFailure { code, progress });
        }
        if self.video_ended && self.audio_ended && !self.transport_draining {
            self.transport_draining = match self.backend.drain_transport() {
                Ok(draining) => draining,
                Err(code) => return Err(TickFailure { code, progress }),
            };
        }
        Ok(progress)
    }

    fn pump_video(&mut self, progress: &mut Progress) -> PlaneResult<()> {
        for _ in 0..READ_BUDGET {
            if self.video_ended {
                break;
            }
            if self.video.ready() {
                let header = self.video.header().expect("ready assembly");
                if !self.backend.submit_video(&header)? {
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
            self.video.push(packet)?;
        }
        // A complete single output sample is forwarded this tick, without waiting
        // for a subsequent frame or introducing an artificial frame of latency.
        if self.video.ready() && !self.video_ended {
            let header = self.video.header().expect("ready assembly");
            if self.backend.submit_video(&header)? {
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

    fn record_audio_admission(&self, packet: &EncodedAudioPacket, progress: &mut Progress) {
        if packet.flags != AUDIO_PACKET_END_OF_STREAM {
            progress.accepted_media_payload_bytes = progress
                .accepted_media_payload_bytes
                .saturating_add(u64::try_from(packet.len).unwrap_or(u64::MAX));
        }
    }

    fn pump_audio(&mut self, progress: &mut Progress) -> PlaneResult<()> {
        if !self.stream_audio {
            if !self.audio_ended {
                let end = EncodedAudioPacket {
                    codec: CODEC_OPUS,
                    flags: AUDIO_PACKET_END_OF_STREAM,
                    sample_rate: 48_000,
                    channels: 2,
                    frames: 0,
                    data: std::ptr::null(),
                    len: 0,
                    timestamp_us: 0,
                    sequence: 0,
                };
                if !self.backend.submit_audio(&end)? {
                    return Ok(());
                }
                self.audio_ended = true;
            }
            return Ok(());
        }

        // Output backpressure stops both encoder polling and further PCM capture.
        for _ in 0..READ_BUDGET {
            if let Some(packet) = &self.pending_audio {
                let header = packet.header();
                if !self.backend.submit_audio(&header)? {
                    return Ok(());
                }
                self.record_audio_admission(&header, progress);
                self.audio_ended = header.flags == AUDIO_PACKET_END_OF_STREAM;
                self.pending_audio = None;
            }
            if self.audio_ended {
                return Ok(());
            }
            let Some(packet) = self.backend.audio()? else {
                break;
            };
            self.pending_audio = Some(packet);
        }
        if let Some(packet) = &self.pending_audio {
            let header = packet.header();
            if !self.backend.submit_audio(&header)? {
                return Ok(());
            }
            self.record_audio_admission(&header, progress);
            self.audio_ended = header.flags == AUDIO_PACKET_END_OF_STREAM;
            self.pending_audio = None;
        }
        if self.audio_ended {
            return Ok(());
        }
        if self.pending_pcm.is_none() && !self.stopping {
            self.pending_pcm = self.backend.pcm()?;
        }
        if let Some(packet) = &self.pending_pcm {
            if !self.backend.submit_pcm(&packet.header())? {
                return Ok(());
            }
            self.pending_pcm = None;
        }
        if self.stopping && !self.encoder_draining {
            self.encoder_draining = self.backend.drain_encoder()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
