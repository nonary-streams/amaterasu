//! Exact, bounded selection of independently implemented encoded-audio formats.
use amaterasu_abi::{BorrowedBytes, STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use amaterasu_media::{
    AUDIO_FORMAT_MAX_CONFIG_BYTES, AUDIO_FORMAT_MAX_OFFERS, AUDIO_FORMAT_VERSION,
    AudioFormatDescriptor, AudioFormatOffer, AudioFormatOfferSink, MEDIA_UNSUPPORTED,
    NegotiatedAudioEncoderApi, NegotiatedAudioPacket,
};
use std::{ffi::c_void, mem::size_of};

/// Owned exact-format preference. Unknown codecs are meaningful only when the
/// consumer explicitly supplies an agreed namespace, framing and configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    pub namespace: [u8; 16],
    pub codec: u32,
    pub framing: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub channel_mask: u64,
    pub frame_samples: u32,
    pub config: Vec<u8>,
}

impl AudioFormat {
    pub fn descriptor(&self) -> AudioFormatDescriptor {
        AudioFormatDescriptor {
            size: size_of::<AudioFormatDescriptor>() as u32,
            version: AUDIO_FORMAT_VERSION,
            namespace: self.namespace,
            codec: self.codec,
            framing: self.framing,
            sample_rate: self.sample_rate,
            channels: self.channels,
            channel_mask: self.channel_mask,
            frame_samples: self.frame_samples,
            reserved: 0,
            config: BorrowedBytes::new(&self.config),
        }
    }

    /// # Safety
    /// The descriptor and any pointed-to configuration must be valid and
    /// immutable for this synchronous copy.
    pub(super) unsafe fn copy(format: &AudioFormatDescriptor) -> Result<Self, Status> {
        // SAFETY: the caller guarantees provider-owned config lifetime.
        let config = unsafe { format.config_bytes()? }.to_vec();
        Ok(Self {
            namespace: format.namespace,
            codec: format.codec,
            framing: format.framing,
            sample_rate: format.sample_rate,
            channels: format.channels,
            channel_mask: format.channel_mask,
            frame_samples: format.frame_samples,
            config,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSelection {
    pub stream: u64,
    pub generation: u64,
    pub offer_id: u64,
    pub format: AudioFormat,
}

/// An encoder offer copied during read-only discovery. It is not a configured
/// encoder and does not establish downstream or client support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCandidate {
    stream: u64,
    generation: u64,
    offer_id: u64,
    format: AudioFormat,
}

impl AudioCandidate {
    pub fn stream(&self) -> u64 {
        self.stream
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn offer_id(&self) -> u64 {
        self.offer_id
    }

    pub fn format(&self) -> &AudioFormat {
        &self.format
    }
}
impl AudioSelection {
    /// # Safety
    /// Packet bytes must remain live and immutable during the returned borrow.
    pub unsafe fn packet_bytes<'a>(
        &self,
        packet: &'a NegotiatedAudioPacket,
    ) -> Result<&'a [u8], Status> {
        // SAFETY: caller guarantees packet storage; the descriptor is used only for validation.
        unsafe {
            packet.bytes(
                self.stream,
                self.generation,
                self.offer_id,
                &self.format.descriptor(),
            )
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioNegotiationError {
    Invalid,
    NoCompatibleFormat,
    Stale,
    Provider(Status),
}
struct Offers {
    values: Vec<(u64, AudioFormat)>,
    invalid: bool,
    total_config: usize,
}
unsafe extern "C" fn collect(context: *mut c_void, offer: *const AudioFormatOffer) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: enumeration keeps this state live throughout the synchronous callback.
    let state = unsafe { &mut *context.cast::<Offers>() };
    if offer.is_null() || state.values.len() >= AUDIO_FORMAT_MAX_OFFERS {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: a trusted provider supplies the borrowed offer during callback.
    let offer = unsafe { &*offer };
    if offer.id == 0
        || state.values.iter().any(|(id, _)| *id == offer.id)
        || offer.format.validate().is_err()
        || state.total_config.saturating_add(offer.format.config.len)
            > AUDIO_FORMAT_MAX_CONFIG_BYTES * AUDIO_FORMAT_MAX_OFFERS
    {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: provider promises initialized config storage during this callback.
    let format = match unsafe { AudioFormat::copy(&offer.format) } {
        Ok(format) => format,
        Err(status) => {
            state.invalid = true;
            return status;
        }
    };
    state.total_config += format.config.len();
    state.values.push((offer.id, format));
    STATUS_OK
}

/// Discover the first caller-preferred exact format without configuring the
/// provider. The candidate does not authorize advertisement by itself: trusted
/// composition must separately prove the downstream path and client agreement.
///
/// # Safety
/// The table and context obey the native contract, remain live through selection
/// and stream lifetime, and are called on the binding's owning thread. Offers
/// and their bytes must be readable and immutable for each synchronous callback.
pub unsafe fn probe(
    api: &NegotiatedAudioEncoderApi,
    stream: u64,
    preferences: &[AudioFormat],
) -> Result<AudioCandidate, AudioNegotiationError> {
    if stream == 0
        || preferences.is_empty()
        || preferences.len() > AUDIO_FORMAT_MAX_OFFERS
        || preferences
            .iter()
            .any(|format| format.descriptor().validate().is_err())
    {
        return Err(AudioNegotiationError::Invalid);
    }
    let mut offers = Offers {
        values: Vec::new(),
        invalid: false,
        total_config: 0,
    };
    let mut generation = 0;
    let sink = AudioFormatOfferSink {
        context: (&mut offers as *mut Offers).cast(),
        write: collect,
    };
    // SAFETY: caller supplies a live binding; enumeration callback is synchronous.
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
    let selected = preferences.iter().find_map(|preferred| {
        offers
            .values
            .iter()
            .find(|(_, offered)| offered == preferred)
    });
    let Some((id, format)) = selected else {
        return Err(AudioNegotiationError::NoCompatibleFormat);
    };
    Ok(AudioCandidate {
        stream,
        generation,
        offer_id: *id,
        format: format.clone(),
    })
}

/// Commit a previously discovered exact offer after downstream and client
/// agreement. A changed provider generation must be rejected by the provider;
/// BUSY or an unsupported offer must leave its selection unchanged.
///
/// # Safety
/// The table and context must be the same live binding used to discover the
/// candidate, and must be used on its owning thread. The stream remains live.
pub unsafe fn configure(
    api: &NegotiatedAudioEncoderApi,
    candidate: AudioCandidate,
) -> Result<AudioSelection, AudioNegotiationError> {
    if candidate.stream == 0
        || candidate.generation == 0
        || candidate.offer_id == 0
        || candidate.format.descriptor().validate().is_err()
    {
        return Err(AudioNegotiationError::Invalid);
    }
    // SAFETY: provider validates id and generation, leaving selection unchanged on failure.
    let status = unsafe {
        (api.configure)(
            api.base.context,
            candidate.stream,
            candidate.generation,
            candidate.offer_id,
        )
    };
    if status != STATUS_OK {
        return Err(if status == MEDIA_UNSUPPORTED {
            AudioNegotiationError::Stale
        } else {
            AudioNegotiationError::Provider(status)
        });
    }
    Ok(AudioSelection {
        stream: candidate.stream,
        generation: candidate.generation,
        offer_id: candidate.offer_id,
        format: candidate.format,
    })
}

/// Immediately discover and select an exact format. There is no implicit
/// fallback. Use [`probe`] and [`configure`] separately when authorization or
/// client acceptance occurs between discovery and selection.
///
/// # Safety
/// The table and context obey the native contract, remain live through selection
/// and stream lifetime, and are called on the binding's owning thread. Offers
/// and their bytes must be readable and immutable for each synchronous callback.
pub unsafe fn negotiate(
    api: &NegotiatedAudioEncoderApi,
    stream: u64,
    preferences: &[AudioFormat],
) -> Result<AudioSelection, AudioNegotiationError> {
    // SAFETY: the caller provides the required binding and stream lifetime.
    let candidate = unsafe { probe(api, stream, preferences)? };
    // SAFETY: the same live binding is used immediately for selection.
    unsafe { configure(api, candidate) }
}

#[cfg(test)]
mod tests;
