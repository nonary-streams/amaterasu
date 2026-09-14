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
    pub audio_capture: u64,
    pub audio_encoder: u64,
    pub media_transport: u64,
    pub control_transport: u64,
}

pub struct ControlRecord {
    pub message_type: u16,
    pub payload: Vec<u8>,
    pub sequence: u64,
}

#[derive(Default)]
pub struct Progress {
    pub controls: Vec<ControlRecord>,
}

pub struct Plane {
    pump: Pump<native::NativeBackend>,
}

impl Plane {
    pub fn new(runtime: &RuntimeClient, handles: Handles) -> PlaneResult<Self> {
        Ok(Self {
            pump: Pump::new(native::NativeBackend::new(runtime, handles)?),
        })
    }

    pub fn tick(&mut self, media_active: bool) -> PlaneResult<Progress> {
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
    video: VideoAssembly,
    pending_pcm: Option<PcmPacket>,
    pending_audio: Option<AudioPacket>,
    stopping: bool,
    encoder_draining: bool,
    video_ended: bool,
    audio_ended: bool,
    transport_draining: bool,
    failure: Option<&'static str>,
}

impl<B: Backend> Pump<B> {
    fn new(backend: B) -> Self {
        Self {
            backend,
            video: VideoAssembly::default(),
            pending_pcm: None,
            pending_audio: None,
            stopping: false,
            encoder_draining: false,
            video_ended: false,
            audio_ended: false,
            transport_draining: false,
            failure: None,
        }
    }

    fn tick(&mut self, media_active: bool) -> PlaneResult<Progress> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = self.advance(media_active);
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }

    fn advance(&mut self, media_active: bool) -> PlaneResult<Progress> {
        // Check the authenticated provider before any media effect. A failed or
        // closed control stream ends pumping; routing metadata grants no authority.
        let mut progress = Progress::default();
        for _ in 0..READ_BUDGET {
            let Some(message) = self.backend.control()? else {
                break;
            };
            progress.controls.push(message);
        }
        if !media_active {
            return Ok(progress);
        }
        self.pump_video()?;
        self.pump_audio()?;
        if self.video_ended && self.audio_ended && !self.transport_draining {
            self.transport_draining = self.backend.drain_transport()?;
        }
        Ok(progress)
    }

    fn pump_video(&mut self) -> PlaneResult<()> {
        for _ in 0..READ_BUDGET {
            if self.video_ended {
                break;
            }
            if self.video.ready() {
                let header = self.video.header().expect("ready assembly");
                if !self.backend.submit_video(&header)? {
                    return Ok(());
                }
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
                self.video_ended = header.flags == FLAG_END_OF_STREAM;
                self.video.clear();
            }
        }
        Ok(())
    }

    fn pump_audio(&mut self) -> PlaneResult<()> {
        // Output backpressure stops both encoder polling and further PCM capture.
        for _ in 0..READ_BUDGET {
            if let Some(packet) = &self.pending_audio {
                let header = packet.header();
                if !self.backend.submit_audio(&header)? {
                    return Ok(());
                }
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
