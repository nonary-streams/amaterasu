use super::{PlaneResult, *};

pub(super) struct VideoPacket {
    metadata: EncodedPacket,
    processing_started_host_us: u64,
    bytes: Vec<u8>,
}

impl VideoPacket {
    /// # Safety
    /// The provider keeps its header and initialized bytes live through this copy.
    pub(super) unsafe fn copy(packet: &EncodedPacket) -> PlaneResult<Self> {
        let bytes = unsafe { packet.bytes() }.map_err(|_| "invalid_video_packet")?;
        if bytes.len() > MAX_VIDEO_ACCESS_UNIT_BYTES {
            return Err("video_access_unit_limit");
        }
        let mut metadata = *packet;
        metadata.data = std::ptr::null();
        Ok(Self {
            metadata,
            processing_started_host_us: 0,
            bytes: bytes.to_vec(),
        })
    }

    /// # Safety
    /// The provider keeps the timed header and initialized bytes live during copy.
    pub(super) unsafe fn copy_timed(packet: &TimedEncodedPacket) -> PlaneResult<Self> {
        let mut output = unsafe { Self::copy(&packet.base) }?;
        output.processing_started_host_us = packet.processing_started_host_us;
        Ok(output)
    }
}

/// Joins encoder fragments into one access unit of the negotiated codec.
pub(super) struct VideoAssembly {
    codec: u32,
    packet: Option<VideoPacket>,
    fragments: usize,
}

impl VideoAssembly {
    pub(super) fn new(codec: u32) -> Self {
        Self {
            codec,
            packet: None,
            fragments: 0,
        }
    }

    pub(super) fn ready(&self) -> bool {
        self.packet.as_ref().is_some_and(|packet| {
            packet.metadata.flags & (FLAG_END_OF_ACCESS_UNIT | FLAG_END_OF_STREAM) != 0
        })
    }

    pub(super) fn push(&mut self, packet: VideoPacket) -> PlaneResult<()> {
        if self.ready() {
            return Err("video_pending_output");
        }
        // The transport accepts any codec it offered; only this session knows
        // which one the client negotiated.
        if packet.metadata.codec != self.codec {
            return Err("video_codec_mismatch");
        }
        if let Some(current) = &mut self.packet {
            if packet.metadata.flags == FLAG_END_OF_STREAM
                || packet.metadata.sequence != current.metadata.sequence
                || packet.metadata.timestamp_us != current.metadata.timestamp_us
                || packet.processing_started_host_us != current.processing_started_host_us
            {
                return Err("video_access_unit_incomplete");
            }
            if self.fragments >= MAX_VIDEO_ACCESS_UNIT_FRAGMENTS
                || current.bytes.len().saturating_add(packet.bytes.len())
                    > MAX_VIDEO_ACCESS_UNIT_BYTES
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

    pub(super) fn processing_started_host_us(&self) -> u64 {
        self.packet
            .as_ref()
            .map_or(0, |packet| packet.processing_started_host_us)
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
    bytes: Vec<u8>,
    unit: AudioUnit,
    side_data: Vec<u8>,
}

impl AudioPacket {
    /// # Safety
    /// The provider keeps both unit allocations readable through this copy.
    pub(super) unsafe fn copy_unit(unit: &AudioUnit) -> PlaneResult<Self> {
        unit.validate().map_err(|_| "invalid_audio_packet")?;
        // SAFETY: the provider guarantees initialized callback-lifetime bytes.
        let bytes = unsafe { unit.payload.read() }
            .map_err(|_| "invalid_audio_packet")?
            .to_vec();
        let side_data = unsafe { unit.metadata.read() }
            .map_err(|_| "invalid_audio_packet")?
            .to_vec();
        let mut header = *unit;
        header.payload = amaterasu_module_sdk::abi::BorrowedBytes::new(&[]);
        header.metadata = amaterasu_module_sdk::abi::BorrowedBytes::new(&[]);
        Ok(Self {
            bytes,
            unit: header,
            side_data,
        })
    }

    pub(super) fn unit_header(&self) -> AudioUnit {
        AudioUnit {
            payload: amaterasu_module_sdk::abi::BorrowedBytes::new(&self.bytes),
            metadata: amaterasu_module_sdk::abi::BorrowedBytes::new(&self.side_data),
            ..self.unit
        }
    }
}
