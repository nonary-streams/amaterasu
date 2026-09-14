use super::{PlaneResult, *};
const MAX_ACCESS_UNIT: usize = 8 * 1024 * 1024;
const MAX_FRAGMENTS: usize = 1024;

pub(super) struct VideoPacket {
    metadata: EncodedPacket,
    bytes: Vec<u8>,
}

impl VideoPacket {
    /// # Safety
    /// The provider keeps its header and initialized bytes live through this copy.
    pub(super) unsafe fn copy(packet: &EncodedPacket) -> PlaneResult<Self> {
        let bytes = unsafe { packet.bytes() }.map_err(|_| "invalid_video_packet")?;
        if bytes.len() > MAX_ACCESS_UNIT {
            return Err("video_access_unit_limit");
        }
        let mut metadata = *packet;
        metadata.data = std::ptr::null();
        Ok(Self {
            metadata,
            bytes: bytes.to_vec(),
        })
    }
}

#[derive(Default)]
pub(super) struct VideoAssembly {
    packet: Option<VideoPacket>,
    fragments: usize,
}

impl VideoAssembly {
    pub(super) fn ready(&self) -> bool {
        self.packet.as_ref().is_some_and(|packet| {
            packet.metadata.flags & (FLAG_END_OF_ACCESS_UNIT | FLAG_END_OF_STREAM) != 0
        })
    }

    pub(super) fn push(&mut self, packet: VideoPacket) -> PlaneResult<()> {
        if self.ready() {
            return Err("video_pending_output");
        }
        if let Some(current) = &mut self.packet {
            if packet.metadata.flags == FLAG_END_OF_STREAM
                || packet.metadata.sequence != current.metadata.sequence
                || packet.metadata.timestamp_us != current.metadata.timestamp_us
            {
                return Err("video_access_unit_incomplete");
            }
            if self.fragments >= MAX_FRAGMENTS
                || current.bytes.len().saturating_add(packet.bytes.len()) > MAX_ACCESS_UNIT
            {
                return Err("video_access_unit_limit");
            }
            current.bytes.extend_from_slice(&packet.bytes);
            current.metadata.flags |= packet.metadata.flags;
        } else {
            self.packet = Some(packet);
        }
        self.fragments += 1;
        Ok(())
    }

    pub(super) fn header(&self) -> Option<EncodedPacket> {
        let packet = self.packet.as_ref()?;
        Some(EncodedPacket {
            data: packet.bytes.as_ptr(),
            len: packet.bytes.len(),
            ..packet.metadata
        })
    }

    pub(super) fn clear(&mut self) {
        self.packet = None;
        self.fragments = 0;
    }
}

pub(super) struct PcmPacket {
    metadata: AudioFrame,
    bytes: Vec<u8>,
}

impl PcmPacket {
    /// # Safety
    /// The provider supplies a live header and initialized immutable samples.
    pub(super) unsafe fn copy(frame: &AudioFrame) -> PlaneResult<Self> {
        let bytes = unsafe { frame.bytes() }.map_err(|_| "invalid_pcm_frame")?;
        if frame.sample_rate != 48_000
            || frame.channels != 2
            || !matches!(frame.format, PCM_F32 | PCM_I16)
        {
            return Err("unsupported_pcm_format");
        }
        let mut metadata = *frame;
        metadata.data = std::ptr::null();
        Ok(Self {
            metadata,
            bytes: bytes.to_vec(),
        })
    }

    pub(super) fn header(&self) -> AudioFrame {
        AudioFrame {
            data: self.bytes.as_ptr(),
            ..self.metadata
        }
    }
}

pub(super) struct AudioPacket {
    metadata: EncodedAudioPacket,
    bytes: Vec<u8>,
}

impl AudioPacket {
    /// # Safety
    /// Provider packet allocation remains live and immutable during this copy.
    pub(super) unsafe fn copy(packet: &EncodedAudioPacket) -> PlaneResult<Self> {
        let bytes = unsafe { packet.bytes() }.map_err(|_| "invalid_audio_packet")?;
        if packet.channels != 2 {
            return Err("unsupported_audio_channels");
        }
        let mut metadata = *packet;
        metadata.data = std::ptr::null();
        Ok(Self {
            metadata,
            bytes: bytes.to_vec(),
        })
    }

    pub(super) fn header(&self) -> EncodedAudioPacket {
        EncodedAudioPacket {
            data: self.bytes.as_ptr(),
            ..self.metadata
        }
    }
}
