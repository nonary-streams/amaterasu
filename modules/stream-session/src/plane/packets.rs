use super::{PlaneResult, *};

pub(super) struct VideoPacket {
    metadata: EncodedPacket,
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
            bytes: bytes.to_vec(),
        })
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
            || !matches!(frame.channels, 2 | 6 | 8)
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

pub(super) struct OrderedPcmPacket {
    metadata: OrderedAudioFrameV2,
    bytes: Vec<u8>,
}

impl OrderedPcmPacket {
    /// # Safety
    /// Capture lends initialized immutable samples through this callback.
    pub(super) unsafe fn copy(frame: &OrderedAudioFrameV2) -> PlaneResult<Self> {
        frame.validate().map_err(|_| "invalid_ordered_pcm")?;
        if frame.frame.sample_rate != 48_000 || !matches!(frame.frame.format, PCM_F32 | PCM_I16) {
            return Err("unsupported_pcm_format");
        }
        // SAFETY: caller guarantees packet bytes during this synchronous copy.
        let bytes = unsafe { frame.frame.bytes() }
            .map_err(|_| "invalid_ordered_pcm")?
            .to_vec();
        let mut metadata = *frame;
        metadata.frame.data = std::ptr::null();
        Ok(Self { metadata, bytes })
    }

    pub(super) fn header(&self) -> OrderedAudioFrameV2 {
        let mut header = self.metadata;
        header.frame.data = self.bytes.as_ptr();
        header
    }
}

pub(super) struct OrderedAudioPacket {
    metadata: NegotiatedAudioPacket,
    bytes: Vec<u8>,
}

impl OrderedAudioPacket {
    /// # Safety
    /// Encoder owns immutable packet bytes for the synchronous callback.
    pub(super) unsafe fn copy(
        packet: &NegotiatedAudioPacket,
        candidate: &amaterasu_module_sdk::audio::OrderedAudioCandidate,
    ) -> PlaneResult<Self> {
        let format = candidate.format().descriptor();
        format
            .validate_packet(
                packet,
                candidate.stream(),
                candidate.generation(),
                candidate.offer_id(),
            )
            .map_err(|_| "invalid_ordered_audio_packet")?;
        // SAFETY: caller guarantees live packet bytes during this copy.
        let bytes = unsafe {
            packet.bytes(
                candidate.stream(),
                candidate.generation(),
                candidate.offer_id(),
                &format.format,
            )
        }
        .map_err(|_| "invalid_ordered_audio_packet")?
        .to_vec();
        let mut metadata = *packet;
        metadata.data = amaterasu_module_sdk::abi::BorrowedBytes::new(&[]);
        Ok(Self { metadata, bytes })
    }

    pub(super) fn header(&self) -> NegotiatedAudioPacket {
        NegotiatedAudioPacket {
            data: amaterasu_module_sdk::abi::BorrowedBytes::new(&self.bytes),
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
        if !matches!(packet.channels, 2 | 6 | 8) {
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

/// Exact video-unit selection supplied by trusted session composition.
#[derive(Clone, Copy)]
pub(crate) struct VideoUnitSelection {
    pub stream: u64,
    pub generation: u64,
    pub offer_id: u64,
    pub codec: EncoderFormatId,
    pub framing: EncoderFormatId,
    pub source_id: u64,
    pub config_generation: u64,
    pub clock_epoch: [u8; 16],
    pub metadata_required: bool,
    pub metadata_schema: Option<amaterasu_media::FrameMetadataSchemaId>,
    pub source_generation: Option<u64>,
}

impl VideoUnitSelection {
    pub(super) fn valid(&self) -> bool {
        let valid_id =
            |id: EncoderFormatId| id.namespace_id != 0 && id.value != 0 && id.schema != 0;
        self.stream != 0
            && self.generation != 0
            && self.offer_id != 0
            && self.source_id != 0
            && self.config_generation != 0
            && self.clock_epoch != [0; 16]
            && valid_id(self.codec)
            && valid_id(self.framing)
            && match (
                self.metadata_required,
                self.metadata_schema,
                self.source_generation,
            ) {
                (false, None, None) => true,
                (true, Some(schema), Some(generation)) => {
                    schema.namespace_id != 0
                        && schema.value != 0
                        && schema.schema != 0
                        && generation != 0
                }
                _ => false,
            }
    }
    pub(super) fn accepts(&self, unit: &NegotiatedVideoUnit) -> bool {
        self.valid()
            && unit
                .validate_for(
                    self.stream,
                    self.generation,
                    self.offer_id,
                    self.codec,
                    self.framing,
                )
                .is_ok()
            && unit.source_id == self.source_id
            && unit.config_generation == self.config_generation
            && unit.clock.epoch_id == self.clock_epoch
    }
}

/// Owns the copied unit until transport admission succeeds.
pub(super) struct VideoUnitPacket {
    metadata: NegotiatedVideoUnit,
    bytes: Vec<u8>,
    side_data: Option<Vec<u8>>,
}

impl VideoUnitPacket {
    /// # Safety
    /// The unit and any nonempty byte allocation remain initialized and
    /// immutable throughout this synchronous copy.
    pub(super) unsafe fn copy(
        unit: &NegotiatedVideoUnit,
        selection: &VideoUnitSelection,
    ) -> PlaneResult<Self> {
        if selection.metadata_required || !selection.accepts(unit) {
            return Err("invalid_negotiated_video_unit");
        }
        let bytes = if unit.data.len == 0 {
            Vec::new()
        } else {
            // SAFETY: caller guarantees the validated byte allocation lifetime.
            unsafe { std::slice::from_raw_parts(unit.data.ptr, unit.data.len) }.to_vec()
        };
        let mut metadata = *unit;
        metadata.data = amaterasu_module_sdk::abi::BorrowedBytes::new(&[]);
        Ok(Self {
            metadata,
            bytes,
            side_data: None,
        })
    }

    /// # Safety
    /// The provider lends both initialized allocations synchronously.
    pub(super) unsafe fn copy_metadata(
        wrapped: &amaterasu_media::NegotiatedVideoMetadataUnitV1,
        selection: &VideoUnitSelection,
    ) -> PlaneResult<Self> {
        use amaterasu_module_sdk::media::metadata::{CarriageOwner, FrameMetadataBundle};
        if !selection.metadata_required
            || !selection.accepts(&wrapped.unit)
            || wrapped.size as usize
                != std::mem::size_of::<amaterasu_media::NegotiatedVideoMetadataUnitV1>()
            || wrapped.version != 1
        {
            return Err("invalid_negotiated_video_unit");
        }
        let mut plain_selection = *selection;
        plain_selection.metadata_required = false;
        plain_selection.metadata_schema = None;
        plain_selection.source_generation = None;
        let mut packet = unsafe { Self::copy(&wrapped.unit, &plain_selection)? };
        if wrapped.unit.flags == amaterasu_media::VIDEO_UNIT_END_OF_STREAM {
            if wrapped.metadata.len != 0 {
                return Err("invalid_negotiated_video_metadata");
            }
            return Ok(packet);
        }
        if wrapped.metadata.len == 0
            || wrapped.metadata.len > amaterasu_media::FRAME_METADATA_MAX_BUNDLE_BYTES
            || wrapped.metadata.ptr.is_null()
        {
            return Err("invalid_negotiated_video_metadata");
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(wrapped.metadata.ptr, wrapped.metadata.len) }
                .to_vec();
        let bundle =
            FrameMetadataBundle::decode(&bytes).map_err(|_| "invalid_negotiated_video_metadata")?;
        let schema = selection
            .metadata_schema
            .ok_or("invalid_negotiated_video_metadata")?;
        if bundle.owner != CarriageOwner::FramerInsert
            || bundle.identity.session_id != wrapped.unit.stream
            || bundle.identity.submission_id != wrapped.unit.submission_id
            || bundle.identity.source_id != wrapped.unit.source_id
            || Some(bundle.identity.source_generation) != selection.source_generation
            || bundle.identity.config_generation != wrapped.unit.config_generation
            || !bundle.items.iter().any(|item| {
                item.required
                    && (
                        item.schema.namespace_id,
                        item.schema.value,
                        item.schema.schema,
                    ) == (schema.namespace_id, schema.value, schema.schema)
            })
        {
            return Err("invalid_negotiated_video_metadata");
        }
        packet.side_data = Some(bytes);
        Ok(packet)
    }

    pub(super) fn side_data(&self) -> Option<&[u8]> {
        self.side_data.as_deref()
    }

    pub(super) fn header(&self) -> NegotiatedVideoUnit {
        NegotiatedVideoUnit {
            data: amaterasu_module_sdk::abi::BorrowedBytes::new(&self.bytes),
            ..self.metadata
        }
    }
}
