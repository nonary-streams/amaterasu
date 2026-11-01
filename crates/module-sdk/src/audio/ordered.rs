//! Owned, exact v2 offers: discovery does not configure or authorize a route.
use super::{AudioFormat, AudioNegotiationError};
use amaterasu_abi::{STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use amaterasu_media::*;
use std::{ffi::c_void, mem::size_of};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderedAudioFormat {
    pub format: AudioFormat,
    pub layout: AudioChannelLayoutV2,
    /// Zero explicitly denotes variable output size.
    pub fixed_packet_bytes: u32,
}
impl OrderedAudioFormat {
    pub fn descriptor(&self) -> AudioFormatDescriptorV2 {
        AudioFormatDescriptorV2 {
            size: size_of::<AudioFormatDescriptorV2>() as u32,
            version: AUDIO_ORDERED_VERSION,
            format: self.format.descriptor(),
            layout: self.layout,
            fixed_packet_bytes: self.fixed_packet_bytes,
            reserved: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderedAudioCandidate {
    stream: u64,
    generation: u64,
    offer_id: u64,
    format: OrderedAudioFormat,
}
impl OrderedAudioCandidate {
    pub fn stream(&self) -> u64 {
        self.stream
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn offer_id(&self) -> u64 {
        self.offer_id
    }
    pub fn format(&self) -> &OrderedAudioFormat {
        &self.format
    }

    /// This borrowed selection is valid only while `self` remains live and
    /// immutable. Clock epoch must come from trusted session composition.
    pub fn transport_selection(&self, clock_epoch: [u8; 16]) -> AudioTransportSelectionV2 {
        AudioTransportSelectionV2 {
            size: size_of::<AudioTransportSelectionV2>() as u32,
            version: AUDIO_ORDERED_VERSION,
            stream: self.stream,
            generation: self.generation,
            offer_id: self.offer_id,
            clock_epoch,
            format: self.format.descriptor(),
        }
    }
}

struct Offers {
    values: Vec<(u64, OrderedAudioFormat)>,
    bytes: usize,
    invalid: bool,
}
unsafe extern "C" fn collect(context: *mut c_void, offer: *const AudioFormatOfferV2) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: enumeration owns the callback state for the duration of the call.
    let output = unsafe { &mut *context.cast::<Offers>() };
    if offer.is_null() || !(offer as usize).is_multiple_of(align_of::<AudioFormatOfferV2>()) {
        output.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: provider lends one initialized offer until callback returns.
    let offer = unsafe { &*offer };
    if offer.id == 0
        || output.values.len() >= AUDIO_FORMAT_MAX_OFFERS
        || output.values.iter().any(|(id, _)| *id == offer.id)
        || offer.format.validate().is_err()
        || output.bytes.saturating_add(offer.format.format.config.len)
            > AUDIO_FORMAT_MAX_CONFIG_BYTES * AUDIO_FORMAT_MAX_OFFERS
    {
        output.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: provider lends initialized configuration through this callback.
    let base = match unsafe { AudioFormat::copy(&offer.format.format) } {
        Ok(base) => base,
        Err(status) => {
            output.invalid = true;
            return status;
        }
    };
    output.bytes += base.config.len();
    output.values.push((
        offer.id,
        OrderedAudioFormat {
            format: base,
            layout: offer.format.layout,
            fixed_packet_bytes: offer.format.fixed_packet_bytes,
        },
    ));
    STATUS_OK
}

/// Discover the first exact preferred output and layout without configuring it.
/// Fixed size is read from the actual offer and separately checked downstream.
///
/// # Safety
/// Table and context remain live and used on the binding's owning thread;
/// provider offers and config bytes are valid during synchronous callbacks.
pub unsafe fn probe_ordered(
    api: &OrderedAudioEncoderApiV2,
    stream: u64,
    preferences: &[(AudioFormat, AudioChannelLayoutV2)],
) -> Result<OrderedAudioCandidate, AudioNegotiationError> {
    if stream == 0
        || preferences.is_empty()
        || preferences.len() > AUDIO_FORMAT_MAX_OFFERS
        || preferences.iter().any(|(format, layout)| {
            format.descriptor().validate().is_err()
                || layout.validate().is_err()
                || format.channels != layout.channels
                || format.channel_mask != layout.mask
        })
    {
        return Err(AudioNegotiationError::Invalid);
    }
    let mut offers = Offers {
        values: Vec::new(),
        bytes: 0,
        invalid: false,
    };
    let mut generation = 0;
    let sink = AudioFormatOfferSinkV2 {
        context: (&mut offers as *mut Offers).cast(),
        write: collect,
    };
    // SAFETY: binding and sink are live through the synchronous enumeration.
    let status = unsafe { (api.enumerate)(api.base.context, stream, sink, &mut generation) };
    if offers.invalid {
        return Err(AudioNegotiationError::Invalid);
    }
    if status != STATUS_OK {
        return Err(AudioNegotiationError::Provider(status));
    }
    if generation == 0 {
        return Err(AudioNegotiationError::Invalid);
    }
    let Some((id, format)) = preferences.iter().find_map(|(preferred, layout)| {
        offers
            .values
            .iter()
            .find(|(_, offered)| &offered.format == preferred && &offered.layout == layout)
    }) else {
        return Err(AudioNegotiationError::NoCompatibleFormat);
    };
    Ok(OrderedAudioCandidate {
        stream,
        generation,
        offer_id: *id,
        format: format.clone(),
    })
}

/// Configure the exact discovered offer with a specific capture stream and
/// ordered lane map after client acceptance and downstream preparation.
///
/// # Safety
/// Use the same live binding and encoder stream used for discovery. Capture
/// identity and layout must come from its live ordered provider binding.
pub unsafe fn configure_ordered(
    api: &OrderedAudioEncoderApiV2,
    candidate: &OrderedAudioCandidate,
    input: &AudioInputSelectionV2,
) -> Result<(), AudioNegotiationError> {
    if input.size as usize != size_of::<AudioInputSelectionV2>()
        || input.version != AUDIO_ORDERED_VERSION
        || input.stream == 0
        || input.generation == 0
        || input.layout.validate().is_err()
    {
        return Err(AudioNegotiationError::Invalid);
    }
    // SAFETY: caller guarantees the binding and input selection remain live.
    let status = unsafe {
        (api.configure)(
            api.base.context,
            candidate.stream,
            candidate.generation,
            candidate.offer_id,
            input,
        )
    };
    if status == STATUS_OK {
        Ok(())
    } else {
        Err(AudioNegotiationError::Provider(status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_abi::{AbiHeader, BorrowedBytes, NativeInterface};

    struct Encoder {
        generation: u64,
        configured: bool,
        codec: u32,
        framing: u32,
    }

    unsafe extern "C" fn enumerate(
        context: *mut c_void,
        _: u64,
        sink: AudioFormatOfferSinkV2,
        generation: *mut u64,
    ) -> Status {
        // SAFETY: test keeps the provider and output live during the callback.
        let provider = unsafe { &*context.cast::<Encoder>() };
        let format = OrderedAudioFormat {
            format: preference(provider.codec, provider.framing),
            layout: layout(),
            fixed_packet_bytes: 640,
        };
        let offer = AudioFormatOfferV2 {
            id: 4,
            format: format.descriptor(),
        };
        // SAFETY: offer config and output remain live through the callback.
        unsafe {
            *generation = provider.generation;
            (sink.write)(sink.context, &offer)
        }
    }
    unsafe extern "C" fn configure(
        context: *mut c_void,
        stream: u64,
        generation: u64,
        id: u64,
        input: *const AudioInputSelectionV2,
    ) -> Status {
        // SAFETY: test retains provider and supplies a live selection.
        let provider = unsafe { &mut *context.cast::<Encoder>() };
        if stream != 8 || generation != provider.generation || id != 4 || input.is_null() {
            return MEDIA_UNSUPPORTED;
        }
        let input = unsafe { &*input };
        if input.layout != layout() {
            return MEDIA_UNSUPPORTED;
        }
        provider.configured = true;
        STATUS_OK
    }
    unsafe extern "C" fn submit(_: *mut c_void, _: u64, _: *const OrderedAudioFrameV2) -> Status {
        MEDIA_PENDING
    }
    unsafe extern "C" fn receive(_: *mut c_void, _: u64, _: NegotiatedAudioPacketSink) -> Status {
        MEDIA_PENDING
    }
    unsafe extern "C" fn drain(_: *mut c_void, _: u64) -> Status {
        STATUS_OK
    }
    fn layout() -> AudioChannelLayoutV2 {
        AudioChannelLayoutV2 {
            size: size_of::<AudioChannelLayoutV2>() as u32,
            version: AUDIO_ORDERED_VERSION,
            channels: 2,
            reserved: 0,
            mask: 3,
            lanes: [1, 2, 0, 0, 0, 0, 0, 0],
        }
    }
    fn preference(codec: u32, framing: u32) -> AudioFormat {
        AudioFormat {
            namespace: [0; 16],
            codec,
            framing,
            sample_rate: 48_000,
            channels: 2,
            channel_mask: 3,
            frame_samples: 1536,
            config: vec![],
        }
    }

    #[test]
    fn ordered_discovery_copies_size_and_rejects_stale_or_wrong_input() {
        for (codec, framing) in [
            (AUDIO_CODEC_AC3, AUDIO_FRAMING_AC3_SYNCFRAME),
            (AUDIO_CODEC_EAC3, AUDIO_FRAMING_EAC3_SYNCFRAME),
        ] {
            let mut encoder = Encoder {
                generation: 1,
                configured: false,
                codec,
                framing,
            };
            let api = OrderedAudioEncoderApiV2 {
                base: NativeInterface {
                    header: AbiHeader::of::<OrderedAudioEncoderApiV2>(),
                    context: (&mut encoder as *mut Encoder).cast(),
                    release: None,
                },
                enumerate,
                configure,
                submit,
                receive,
                drain,
            };
            // SAFETY: test-owned binding and provider stay live through calls.
            let candidate =
                unsafe { probe_ordered(&api, 8, &[(preference(codec, framing), layout())]) }
                    .unwrap();
            assert!(!encoder.configured);
            assert_eq!(candidate.format().fixed_packet_bytes, 640);
            let selected = candidate.transport_selection([9; 16]);
            assert!(selected.validate().is_ok());
            assert_eq!(
                selected.format.format.config.len,
                BorrowedBytes::new(&[]).len
            );
            let mut input = AudioInputSelectionV2 {
                size: size_of::<AudioInputSelectionV2>() as u32,
                version: AUDIO_ORDERED_VERSION,
                stream: 2,
                generation: 3,
                layout: layout(),
            };
            input.layout.lanes.swap(0, 1);
            assert!(unsafe { configure_ordered(&api, &candidate, &input) }.is_err());
            input.layout = layout();
            encoder.generation = 2;
            assert!(unsafe { configure_ordered(&api, &candidate, &input) }.is_err());
            assert!(!encoder.configured);
            encoder.generation = 1;
            assert!(unsafe { configure_ordered(&api, &candidate, &input) }.is_ok());
            assert!(encoder.configured);
        }
    }
}
