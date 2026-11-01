//! Selected 32 ms audio on the production transport. The selected encoder emits
//! complete access units; this transport owns RTP, FEC, encryption and delivery.
use super::*;
use amaterasu_module_sdk::abi::BorrowedBytes;

#[derive(Clone, Copy)]
pub(super) struct OrderedAudioState {
    stream: u64,
    generation: u64,
    offer_id: u64,
    codec: u32,
    framing: u32,
    layout: AudioChannelLayoutV2,
    fixed_bytes: u32,
    anchor_us: Option<u64>,
    previous_us: Option<u64>,
    previous_sequence: Option<u64>,
}

impl OrderedAudioState {
    fn format(self) -> AudioFormatDescriptorV2 {
        AudioFormatDescriptorV2 {
            size: size_of::<AudioFormatDescriptorV2>() as u32,
            version: AUDIO_ORDERED_VERSION,
            format: AudioFormatDescriptor {
                size: size_of::<AudioFormatDescriptor>() as u32,
                version: AUDIO_FORMAT_VERSION,
                namespace: [0; 16],
                codec: self.codec,
                framing: self.framing,
                sample_rate: 48_000,
                channels: self.layout.channels,
                channel_mask: self.layout.mask,
                frame_samples: 1536,
                reserved: 0,
                config: BorrowedBytes::new(&[]),
            },
            layout: self.layout,
            fixed_packet_bytes: self.fixed_bytes,
            reserved: 0,
        }
    }
}

impl Stream {
    /// Probe is read-only and does not imply client or encoder availability.
    pub fn probe_ordered_audio(
        &self,
        selection: &AudioTransportSelectionV2,
    ) -> TransportResult<()> {
        selection
            .validate()
            .map_err(|_| "invalid_audio_selection")?;
        let format = &selection.format;
        if self.delivery.failure.is_some()
            || !self.stream_audio
            || self.audio_ended
            || self.audio_sequence.is_some()
            || self.ordered_audio.is_some()
            || format.format.namespace != [0; 16]
            || !matches!(
                (format.format.codec, format.format.framing),
                (AUDIO_CODEC_AC3, AUDIO_FRAMING_AC3_SYNCFRAME)
                    | (AUDIO_CODEC_EAC3, AUDIO_FRAMING_EAC3_SYNCFRAME)
            )
            || format.format.config.len != 0
            || format.format.sample_rate != 48_000
            || format.format.frame_samples != 1536
            || format.format.channels != u32::from(self.audio_settings.channels)
            || format.fixed_packet_bytes == 0
            || format.fixed_packet_bytes as usize
                > amaterasu_gamestream::audio::MAX_SURROUND_AUDIO_BYTES
        {
            return Err("unsupported_audio_selection");
        }
        let expected = match format.layout.channels {
            2 => [1, 2, 0, 0, 0, 0, 0, 0],
            6 => [1, 2, 4, 8, 16, 32, 0, 0],
            _ => return Err("unsupported_audio_layout"),
        };
        if format.layout.lanes != expected {
            return Err("unsupported_audio_layout");
        }
        Ok(())
    }

    /// Prepare only after the exact client acceptance and encoder offer have
    /// been authenticated. No packet, FEC or cipher state has advanced yet.
    pub fn prepare_ordered_audio(
        &mut self,
        selection: &AudioTransportSelectionV2,
    ) -> TransportResult<()> {
        self.probe_ordered_audio(selection)?;
        let format = selection.format;
        let plain = format.fixed_packet_bytes as usize;
        let wire_bytes = if self.audio_sealer.is_some() {
            (plain / 16 + 1) * 16
        } else {
            plain
        };
        let packetizer = AudioPacketizer::new_surround(AudioConfig {
            sample_rate: 48_000,
            channels: self.audio_settings.channels,
            frames_per_packet: 1536,
            wire_payload_bytes: wire_bytes,
            ssrc: self.audio_settings.ssrc,
        })
        .map_err(|_| "unsupported_audio_selection")?;
        self.audio = packetizer;
        self.audio_settings.packet_ms = 32;
        self.audio_settings.wire_payload_bytes = wire_bytes;
        self.ordered_audio = Some(OrderedAudioState {
            stream: selection.stream,
            generation: selection.generation,
            offer_id: selection.offer_id,
            codec: format.format.codec,
            framing: format.format.framing,
            layout: format.layout,
            fixed_bytes: format.fixed_packet_bytes,
            anchor_us: None,
            previous_us: None,
            previous_sequence: None,
        });
        Ok(())
    }

    /// # Safety
    /// The packet and all nonempty bytes stay live and immutable for this call.
    pub unsafe fn submit_ordered_audio(&mut self, packet: &NegotiatedAudioPacket) -> Status {
        if self.delivery.failure.is_some() {
            return MEDIA_BACKEND_ERROR;
        }
        if !self.delivery.ready() {
            return MEDIA_BUSY;
        }
        if self.audio_ended {
            return MEDIA_CLOSED;
        }
        let Some(state) = self.ordered_audio else {
            return MEDIA_UNSUPPORTED;
        };
        let format = state.format();
        if format
            .validate_packet(packet, state.stream, state.generation, state.offer_id)
            .is_err()
        {
            return STATUS_INVALID_ARGUMENT;
        }
        if packet.flags == AUDIO_PACKET_END_OF_STREAM {
            self.audio_ended = true;
            return STATUS_OK;
        }
        let discontinuity = packet.flags & AUDIO_PACKET_DISCONTINUITY != 0;
        if let Some(previous) = state.previous_us
            && (packet.timestamp_us <= previous
                || state
                    .previous_sequence
                    .is_some_and(|value| packet.sequence <= value)
                || (!discontinuity && previous.checked_add(32_000) != Some(packet.timestamp_us))
                || (!discontinuity
                    && state
                        .previous_sequence
                        .and_then(|value| value.checked_add(1))
                        != Some(packet.sequence)))
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let anchor = state.anchor_us.unwrap_or(packet.timestamp_us);
        let Some(relative_us) = packet.timestamp_us.checked_sub(anchor) else {
            return STATUS_INVALID_ARGUMENT;
        };
        // SAFETY: caller guarantees bytes throughout the synchronous borrow.
        let bytes = match unsafe {
            packet.bytes(
                state.stream,
                state.generation,
                state.offer_id,
                &format.format,
            )
        } {
            Ok(bytes) => bytes,
            Err(status) => return status,
        };
        if self.audio.validate_input(bytes, packet.frames).is_err() {
            return STATUS_INVALID_ARGUMENT;
        }
        let wire_length = if self.audio_sealer.is_some() {
            (bytes.len() / 16 + 1) * 16
        } else {
            bytes.len()
        };
        if wire_length != self.audio_settings.wire_payload_bytes {
            return STATUS_INVALID_ARGUMENT;
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
            return MEDIA_BUSY;
        }
        if discontinuity {
            let reset = match self
                .audio
                .reset_surround_discontinuity_at((relative_us / 1000) as u32)
            {
                Ok(reset) => reset,
                Err(_) => return self.fail("audio_discontinuity_failed"),
            };
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
                .packetize_sealed(bytes, packet.frames, |sequence, bytes| {
                    sealer
                        .encrypt_surround(sequence, bytes)
                        .map_err(|_| AudioError::EncryptionFailed)
                })
        } else {
            self.audio.packetize(bytes, packet.frames)
        };
        match result {
            Ok(datagrams) => {
                self.delivery.push_audio(datagrams);
                let active = self.ordered_audio.as_mut().expect("selected audio state");
                active.anchor_us = Some(anchor);
                active.previous_us = Some(packet.timestamp_us);
                active.previous_sequence = Some(packet.sequence);
                STATUS_OK
            }
            Err(_) => self.fail("audio_packetization_failed"),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
