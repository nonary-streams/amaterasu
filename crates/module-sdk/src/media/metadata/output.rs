//! Association of admitted frame metadata with one encoded access unit.
use super::{CarriageOwner, FrameMetadataAdmission, FrameMetadataBundle, MetadataError};
use amaterasu_media::{EncoderFormatId, NegotiatedVideoUnit, VIDEO_UNIT_END_OF_STREAM};

/// The exact independently selected encoded-unit identity. This is supplied by
/// trusted session composition, never inferred from the encoded unit.
#[derive(Clone, Copy)]
pub struct EncodedMetadataSelection {
    pub stream: u64,
    pub generation: u64,
    pub offer_id: u64,
    pub codec: EncoderFormatId,
    pub framing: EncoderFormatId,
    pub clock_epoch: [u8; 16],
}

/// An assertion made by the selected provider, not proof of codec syntax.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataApplication {
    Forwarded,
    EncoderApplied,
    FramerApplied,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Stage {
    Encoder,
    Framer,
    Ready,
    Retired,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct UnitBinding {
    access_unit_id: u64,
    timestamp_us: u64,
    flags: u32,
    payload_len: usize,
}

impl From<&NegotiatedVideoUnit> for UnitBinding {
    fn from(unit: &NegotiatedVideoUnit) -> Self {
        Self {
            access_unit_id: unit.access_unit_id,
            timestamp_us: unit.timestamp_us,
            flags: unit.flags,
            payload_len: unit.data.len,
        }
    }
}

/// One bounded association, created from a live encoder-stage frame admission.
/// The caller retains provider work and the frame lease independently. Do not
/// advance a stage until the corresponding provider operation has succeeded.
pub struct EncodedMetadataAssociation {
    selection: EncodedMetadataSelection,
    capability_generation: u64,
    bundle: FrameMetadataBundle,
    bytes: Vec<u8>,
    unit_binding: Option<UnitBinding>,
    stage: Stage,
}

impl EncodedMetadataAssociation {
    pub fn new(
        admission: &mut FrameMetadataAdmission,
        capability_generation: u64,
        selection: EncodedMetadataSelection,
    ) -> Result<Self, MetadataError> {
        let valid_format =
            |id: EncoderFormatId| id.namespace_id != 0 && id.value != 0 && id.schema != 0;
        if selection.stream == 0
            || selection.generation == 0
            || selection.offer_id == 0
            || selection.clock_epoch == [0; 16]
            || !valid_format(selection.codec)
            || !valid_format(selection.framing)
        {
            return Err(MetadataError::Invalid);
        }
        let (bundle, bytes) = {
            let frame = admission
                .pending(capability_generation)?
                .ok_or(MetadataError::Invalid)?;
            frame.encoder_options()?;
            let bundle = FrameMetadataBundle::decode(frame.bytes())?;
            if bundle.identity != frame.identity() {
                return Err(MetadataError::Invalid);
            }
            (bundle, frame.bytes().to_vec())
        };
        admission.claim_output(bundle.identity.submission_id, capability_generation)?;
        Ok(Self {
            selection,
            capability_generation,
            bundle,
            bytes,
            unit_binding: None,
            stage: Stage::Encoder,
        })
    }

    fn check(
        &self,
        unit: &NegotiatedVideoUnit,
        metadata: &[u8],
        capability_generation: u64,
    ) -> Result<(), MetadataError> {
        if self.stage == Stage::Retired {
            return Err(MetadataError::Closed);
        }
        if capability_generation != self.capability_generation {
            return Err(MetadataError::Stale);
        }
        let selection = self.selection;
        let identity = self.bundle.identity;
        unit.validate_for(
            selection.stream,
            selection.generation,
            selection.offer_id,
            selection.codec,
            selection.framing,
        )
        .map_err(|_| MetadataError::Invalid)?;
        if unit.flags == VIDEO_UNIT_END_OF_STREAM
            || unit.submission_id != identity.submission_id
            || unit.source_id != identity.source_id
            || unit.config_generation != identity.config_generation
            || unit.clock.epoch_id != selection.clock_epoch
            || metadata != self.bytes
            || self
                .unit_binding
                .is_some_and(|binding| binding != unit.into())
        {
            return Err(MetadataError::Foreign);
        }
        // AFM1 carries the capture clock; the unit carries the separately
        // selected stream clock. Their timestamps must not be equated.
        Ok(())
    }

    pub fn accept_encoder_output(
        &mut self,
        unit: &NegotiatedVideoUnit,
        metadata: &[u8],
        application: MetadataApplication,
        capability_generation: u64,
    ) -> Result<(), MetadataError> {
        self.check(unit, metadata, capability_generation)?;
        if self.stage != Stage::Encoder {
            return Err(MetadataError::Duplicate);
        }
        match (self.bundle.owner, application) {
            (CarriageOwner::EncoderEmbed, MetadataApplication::EncoderApplied)
            | (CarriageOwner::FramerInsert, MetadataApplication::Forwarded) => {}
            _ => return Err(MetadataError::WrongOwner),
        }
        self.stage = Stage::Framer;
        self.unit_binding = Some(unit.into());
        Ok(())
    }

    pub fn accept_framer_output(
        &mut self,
        unit: &NegotiatedVideoUnit,
        metadata: &[u8],
        application: MetadataApplication,
        capability_generation: u64,
    ) -> Result<(), MetadataError> {
        self.check(unit, metadata, capability_generation)?;
        if self.stage != Stage::Framer {
            return Err(MetadataError::Duplicate);
        }
        let expected = match self.bundle.owner {
            CarriageOwner::EncoderEmbed => MetadataApplication::EncoderApplied,
            CarriageOwner::FramerInsert => MetadataApplication::FramerApplied,
        };
        if application != expected {
            return Err(MetadataError::WrongOwner);
        }
        self.stage = Stage::Ready;
        Ok(())
    }

    /// Check before every transport attempt, including retries on BUSY.
    pub fn check_transport(
        &self,
        unit: &NegotiatedVideoUnit,
        metadata: &[u8],
        capability_generation: u64,
    ) -> Result<(), MetadataError> {
        self.check(unit, metadata, capability_generation)?;
        if self.stage != Stage::Ready {
            return Err(MetadataError::WrongOwner);
        }
        Ok(())
    }

    /// Retire only after local transport admission, not on BUSY. This operation
    /// does not claim remote delivery or inspect the encoded bitstream.
    pub fn retire(
        &mut self,
        unit: &NegotiatedVideoUnit,
        metadata: &[u8],
        capability_generation: u64,
    ) -> Result<(), MetadataError> {
        self.check_transport(unit, metadata, capability_generation)?;
        self.stage = Stage::Retired;
        self.bytes.clear();
        Ok(())
    }
}
