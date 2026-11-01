//! Bounded, canonical frame metadata and trusted stage-policy validation.
mod admission;
mod capabilities;
mod output;
use crate::encode::FormatId;
pub use admission::{
    BorrowedEncoderFrameOptions, FrameMetadataAdmission, FrameMetadataContext,
    MetadataAdmissionResult, PreparedFrameMetadata,
};
use amaterasu_media::{
    CAPTURE_METADATA_INTERFACE_MINOR, ENCODER_FORMAT_NAMESPACE_STANDARD, ENCODER_FORMAT_SCHEMA_V1,
    ENCODER_SCHEMA_FRAME_METADATA_BUNDLE, FRAME_CLOCK_HOST_MONOTONIC, FRAME_CLOCK_STREAM_RELATIVE,
    FRAME_METADATA_ITEM_REQUIRED, FRAME_METADATA_MAX_BUNDLE_BYTES, FRAME_METADATA_MAX_ITEM_BYTES,
    FRAME_METADATA_MAX_ITEMS, FRAME_METADATA_OWNER_ENCODER, FRAME_METADATA_OWNER_FRAMER,
    FRAME_RESOURCE_METADATA_FRAME_V1_SIZE, FrameClockMetadata, FrameResourceMetadataFrameV1,
};
pub use capabilities::{
    CAPABILITIES_CONTRACT, MetadataCapabilityClaim, MetadataCapabilitySnapshot, bind_capabilities,
    enumerate_capabilities, register_capabilities,
};
pub use output::{EncodedMetadataAssociation, EncodedMetadataSelection, MetadataApplication};

const MAGIC: &[u8; 4] = b"AFM1";
const HEADER_BYTES: usize = 80;
const ITEM_HEADER_BYTES: usize = 32;

/// Exact side-data schema passed to EncoderFrameOptionsV1; bytes are AFM1.
pub const BUNDLE_SCHEMA: FormatId = FormatId {
    namespace_id: ENCODER_FORMAT_NAMESPACE_STANDARD,
    value: ENCODER_SCHEMA_FRAME_METADATA_BUNDLE,
    schema: ENCODER_FORMAT_SCHEMA_V1,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CarriageOwner {
    EncoderEmbed,
    FramerInsert,
}
impl CarriageOwner {
    fn raw(self) -> u32 {
        match self {
            Self::EncoderEmbed => FRAME_METADATA_OWNER_ENCODER,
            Self::FramerInsert => FRAME_METADATA_OWNER_FRAMER,
        }
    }
    fn from_raw(raw: u32) -> Result<Self, MetadataError> {
        match raw {
            FRAME_METADATA_OWNER_ENCODER => Ok(Self::EncoderEmbed),
            FRAME_METADATA_OWNER_FRAMER => Ok(Self::FramerInsert),
            _ => Err(MetadataError::Invalid),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameMetadataIdentity {
    pub session_id: u64,
    pub submission_id: u64,
    pub source_id: u64,
    pub source_generation: u64,
    pub config_generation: u64,
    pub timestamp_us: u64,
    pub clock_domain: u32,
    pub clock_epoch: [u8; 16],
}
impl FrameMetadataIdentity {
    fn valid(self) -> bool {
        self.session_id != 0
            && self.submission_id != 0
            && self.source_id != 0
            && self.source_generation != 0
            && self.config_generation != 0
            && matches!(
                self.clock_domain,
                FRAME_CLOCK_HOST_MONOTONIC | FRAME_CLOCK_STREAM_RELATIVE
            )
            && self.clock_epoch != [0; 16]
    }
    fn matches_frame(self, frame: &amaterasu_media::CaptureResourceFrame) -> bool {
        let clock = frame.clock;
        self.submission_id == frame.sequence
            && self.timestamp_us == frame.timestamp_us
            && clock.version == 1
            && clock.size as usize == std::mem::size_of::<FrameClockMetadata>()
            && clock.reserved == 0
            && clock.domain == self.clock_domain
            && clock.epoch_id == self.clock_epoch
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameMetadataItem {
    pub schema: FormatId,
    pub required: bool,
    pub payload: Vec<u8>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameMetadataBundle {
    pub identity: FrameMetadataIdentity,
    pub owner: CarriageOwner,
    pub items: Vec<FrameMetadataItem>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataStage {
    Capture,
    Processor,
    Encoder,
    Framer,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataTreatment {
    Forward,
    Apply,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetadataCapability {
    pub schema: FormatId,
    pub treatment: MetadataTreatment,
}
/// Constructed from trusted startup composition and independently established
/// provider capabilities, never from a provider's metadata envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataPolicy {
    /// Authenticated session for which the capability generation was queried.
    pub session_id: u64,
    /// Generation of the independently queried provider capabilities.
    pub capability_generation: u64,
    pub owner: CarriageOwner,
    pub stage: MetadataStage,
    pub capabilities: Vec<MetadataCapability>,
    pub allow_drop_unknown_optional: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataDisposition {
    Forward,
    Apply,
    DropOptional,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataError {
    Invalid,
    TooLarge,
    Foreign,
    Unsupported,
    Duplicate,
    WrongOwner,
    Busy,
    Closed,
    Conflict,
    Stale,
    Provider(amaterasu_abi::Status),
}

fn schema_valid(schema: FormatId) -> bool {
    schema.namespace_id != 0 && schema.value != 0 && schema.schema != 0
}
fn schema_key(schema: FormatId) -> (u64, u64, u64) {
    (schema.namespace_id, schema.value, schema.schema)
}
impl MetadataPolicy {
    /// Return the explicitly negotiated treatment of each item. `Apply` is
    /// legal only at the selected carriage owner; stages before it can forward.
    /// Unknown required data is always rejected. Dropping unknown optional data
    /// requires the explicit trusted policy bit.
    pub fn validate(
        &self,
        bundle: &FrameMetadataBundle,
    ) -> Result<Vec<MetadataDisposition>, MetadataError> {
        bundle.validate()?;
        if self.session_id == 0 || self.capability_generation == 0 {
            return Err(MetadataError::Invalid);
        }
        if self.session_id != bundle.identity.session_id {
            return Err(MetadataError::Foreign);
        }
        if self.owner != bundle.owner {
            return Err(MetadataError::WrongOwner);
        }
        if self.capabilities.len() > FRAME_METADATA_MAX_ITEMS {
            return Err(MetadataError::TooLarge);
        }
        for (index, capability) in self.capabilities.iter().enumerate() {
            if !schema_valid(capability.schema) {
                return Err(MetadataError::Invalid);
            }
            if self.capabilities[..index]
                .iter()
                .any(|prior| prior.schema == capability.schema)
            {
                return Err(MetadataError::Duplicate);
            }
            let expected = match (self.stage, self.owner) {
                (MetadataStage::Encoder, CarriageOwner::EncoderEmbed)
                | (MetadataStage::Framer, CarriageOwner::FramerInsert) => MetadataTreatment::Apply,
                _ => MetadataTreatment::Forward,
            };
            if capability.treatment != expected {
                return Err(MetadataError::WrongOwner);
            }
        }
        let mut dispositions = Vec::with_capacity(bundle.items.len());
        for item in &bundle.items {
            match self
                .capabilities
                .iter()
                .find(|cap| cap.schema == item.schema)
            {
                Some(cap) => dispositions.push(match cap.treatment {
                    MetadataTreatment::Forward => MetadataDisposition::Forward,
                    MetadataTreatment::Apply => MetadataDisposition::Apply,
                }),
                None if !item.required && self.allow_drop_unknown_optional => {
                    dispositions.push(MetadataDisposition::DropOptional)
                }
                None => return Err(MetadataError::Unsupported),
            }
        }
        Ok(dispositions)
    }
}

impl FrameMetadataBundle {
    fn validate(&self) -> Result<(), MetadataError> {
        if !self.identity.valid() {
            return Err(MetadataError::Invalid);
        }
        if self.items.len() > FRAME_METADATA_MAX_ITEMS {
            return Err(MetadataError::TooLarge);
        }
        let mut bytes = HEADER_BYTES;
        let mut previous = None;
        for item in &self.items {
            if !schema_valid(item.schema) || item.payload.is_empty() {
                return Err(MetadataError::Invalid);
            }
            if item.payload.len() > FRAME_METADATA_MAX_ITEM_BYTES {
                return Err(MetadataError::TooLarge);
            }
            let key = schema_key(item.schema);
            if previous.is_some_and(|prior| prior >= key) {
                return Err(MetadataError::Duplicate);
            }
            previous = Some(key);
            bytes = bytes
                .checked_add(ITEM_HEADER_BYTES)
                .and_then(|v| v.checked_add(item.payload.len()))
                .ok_or(MetadataError::TooLarge)?;
            if bytes > FRAME_METADATA_MAX_BUNDLE_BYTES {
                return Err(MetadataError::TooLarge);
            }
        }
        Ok(())
    }

    /// Canonical little-endian, pointer-free envelope. Items must be strictly
    /// ordered by namespace/value/schema to make retry identity unambiguous.
    pub fn encode(&self) -> Result<Vec<u8>, MetadataError> {
        self.validate()?;
        let mut out = Vec::with_capacity(HEADER_BYTES);
        out.extend_from_slice(MAGIC);
        put_u32(&mut out, self.owner.raw());
        for value in [
            self.identity.session_id,
            self.identity.submission_id,
            self.identity.source_id,
            self.identity.source_generation,
            self.identity.config_generation,
            self.identity.timestamp_us,
        ] {
            put_u64(&mut out, value);
        }
        put_u32(&mut out, self.identity.clock_domain);
        out.extend_from_slice(&self.identity.clock_epoch);
        put_u32(&mut out, self.items.len() as u32);
        for item in &self.items {
            for value in [
                item.schema.namespace_id,
                item.schema.value,
                item.schema.schema,
            ] {
                put_u64(&mut out, value);
            }
            put_u32(
                &mut out,
                if item.required {
                    FRAME_METADATA_ITEM_REQUIRED
                } else {
                    0
                },
            );
            put_u32(&mut out, item.payload.len() as u32);
            out.extend_from_slice(&item.payload);
        }
        Ok(out)
    }

    /// Decode bounded canonical bytes without interpreting codec metadata syntax.
    pub fn decode(bytes: &[u8]) -> Result<Self, MetadataError> {
        if bytes.len() > FRAME_METADATA_MAX_BUNDLE_BYTES {
            return Err(MetadataError::TooLarge);
        }
        let mut reader = Reader { bytes, offset: 0 };
        if reader.take(4)? != MAGIC {
            return Err(MetadataError::Invalid);
        }
        let owner = CarriageOwner::from_raw(reader.u32()?)?;
        let session_id = reader.u64()?;
        let submission_id = reader.u64()?;
        let source_id = reader.u64()?;
        let source_generation = reader.u64()?;
        let config_generation = reader.u64()?;
        let timestamp_us = reader.u64()?;
        let clock_domain = reader.u32()?;
        let mut clock_epoch = [0; 16];
        clock_epoch.copy_from_slice(reader.take(16)?);
        let count = reader.u32()? as usize;
        if count > FRAME_METADATA_MAX_ITEMS {
            return Err(MetadataError::TooLarge);
        }
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            let schema = FormatId {
                namespace_id: reader.u64()?,
                value: reader.u64()?,
                schema: reader.u64()?,
            };
            let flags = reader.u32()?;
            if flags & !FRAME_METADATA_ITEM_REQUIRED != 0 {
                return Err(MetadataError::Invalid);
            }
            let length = reader.u32()? as usize;
            if length > FRAME_METADATA_MAX_ITEM_BYTES {
                return Err(MetadataError::TooLarge);
            }
            items.push(FrameMetadataItem {
                schema,
                required: flags == FRAME_METADATA_ITEM_REQUIRED,
                payload: reader.take(length)?.to_vec(),
            });
        }
        if reader.offset != bytes.len() {
            return Err(MetadataError::Invalid);
        }
        let bundle = Self {
            identity: FrameMetadataIdentity {
                session_id,
                submission_id,
                source_id,
                source_generation,
                config_generation,
                timestamp_us,
                clock_domain,
                clock_epoch,
            },
            owner,
            items,
        };
        bundle.validate()?;
        Ok(bundle)
    }

    /// Validate against the authenticated stream's frozen identity and policy.
    pub fn verify(
        &self,
        expected: FrameMetadataIdentity,
        policy: &MetadataPolicy,
    ) -> Result<Vec<MetadataDisposition>, MetadataError> {
        self.validate()?;
        if self.identity != expected {
            return Err(MetadataError::Foreign);
        }
        policy.validate(self)
    }
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}
fn put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], MetadataError> {
        let end = self.offset.checked_add(n).ok_or(MetadataError::Invalid)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(MetadataError::Invalid)?;
        self.offset = end;
        Ok(bytes)
    }
    fn u32(&mut self) -> Result<u32, MetadataError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| MetadataError::Invalid)?,
        ))
    }
    fn u64(&mut self) -> Result<u64, MetadataError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| MetadataError::Invalid)?,
        ))
    }
}

/// Copy and validate the envelope while the provider's synchronous callback is
/// live. Retaining the frame never retains the metadata allocation. `expected`
/// must come from authenticated, trusted session state, not the provider.
///
/// # Safety
/// The wrapper, descriptors and metadata bytes must be valid and immutable for
/// this call. This validates structure and identity, not native memory safety.
pub unsafe fn copy_frame_metadata(
    frame: &FrameResourceMetadataFrameV1,
    expected: FrameMetadataIdentity,
    policy: &MetadataPolicy,
) -> Result<(FrameMetadataBundle, Vec<MetadataDisposition>), MetadataError> {
    if frame.size != FRAME_RESOURCE_METADATA_FRAME_V1_SIZE
        || frame.version != 1
        || frame.metadata.len > FRAME_METADATA_MAX_BUNDLE_BYTES
        || (frame.metadata.len != 0 && frame.metadata.ptr.is_null())
    {
        return Err(MetadataError::Invalid);
    }
    if !expected.matches_frame(&frame.frame) {
        return Err(MetadataError::Foreign);
    }
    if frame.frame.offer_generation == 0
        || frame.frame.offer_id == 0
        || frame.frame.width == 0
        || frame.frame.height == 0
        || frame.frame.reserved != 0
        || frame.frame.resource.reserved != 0
        || frame.frame.acquire_sync.reserved != 0
    {
        return Err(MetadataError::Invalid);
    }
    let bytes = if frame.metadata.len == 0 {
        &[]
    } else {
        // SAFETY: caller supplies a valid immutable provider borrow.
        unsafe { std::slice::from_raw_parts(frame.metadata.ptr, frame.metadata.len) }
    };
    let bundle = FrameMetadataBundle::decode(bytes)?;
    let dispositions = bundle.verify(expected, policy)?;
    Ok((bundle, dispositions))
}

/// Version and size are both required before reading a capture metadata tail.
pub fn capture_metadata_available(header: amaterasu_abi::AbiHeader) -> bool {
    header.major == 1
        && header.minor >= CAPTURE_METADATA_INTERFACE_MINOR
        && header.size >= amaterasu_media::CAPTURE_API_V1_10_SIZE
}

/// Bind the additive capture metadata tail; the typed capture family stays the same.
///
/// # Safety
/// Provider and runtime remain live until binding release and all borrowed frames retire.
pub unsafe fn bind_capture_with_metadata(
    runtime: &crate::RuntimeClient,
    service: &str,
) -> crate::ModuleResult<crate::NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            super::native_contracts::CAPTURE,
            1,
            CAPTURE_METADATA_INTERFACE_MINOR,
            amaterasu_media::CAPTURE_API_V1_10_SIZE,
        )
    }
}
/// Bind capture with the authenticated context-setup and metadata-read tails.
///
/// # Safety
/// Provider and runtime remain live until binding release and every captured
/// frame and metadata borrow has retired.
pub unsafe fn bind_capture_with_metadata_context(
    runtime: &crate::RuntimeClient,
    service: &str,
) -> crate::ModuleResult<crate::NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            super::native_contracts::CAPTURE,
            1,
            amaterasu_media::CAPTURE_METADATA_CONTEXT_INTERFACE_MINOR,
            amaterasu_media::CAPTURE_API_V1_11_SIZE,
        )
    }
}

/// Bind an existing resource-consumer family with its metadata tail.
///
/// # Safety
/// Provider and runtime outlive all calls, retained frames, and cancellation.
pub unsafe fn bind_consumer_with_metadata(
    runtime: &crate::RuntimeClient,
    service: &str,
) -> crate::ModuleResult<crate::NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            super::native_contracts::FRAME_RESOURCE_CONSUMER,
            1,
            amaterasu_media::FRAME_RESOURCE_CONSUMER_METADATA_MINOR,
            amaterasu_media::FRAME_RESOURCE_CONSUMER_API_V1_1_SIZE,
        )
    }
}
/// Bind a retry-safe processor with its additive metadata tail.
///
/// # Safety
/// Provider and runtime outlive all calls, retained frames, and cancellation.
pub unsafe fn bind_processor_with_metadata(
    runtime: &crate::RuntimeClient,
    service: &str,
) -> crate::ModuleResult<crate::NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            super::native_contracts::FRAME_RESOURCE_PROCESSOR,
            1,
            amaterasu_media::FRAME_RESOURCE_PROCESSOR_METADATA_MINOR,
            amaterasu_media::FRAME_RESOURCE_PROCESSOR_API_V1_2_SIZE,
        )
    }
}

/// Check a processor's output against the input and frozen processor policy.
/// Forwarded items must preserve their required bit and exact bytes; only an
/// explicitly droppable optional item may disappear. Additional items require
/// the same policy validation. This checks association and bytes, not whether
/// a provider honestly analyzed the pixels or completed GPU work.
pub fn verify_processor_transition(
    input: &FrameMetadataBundle,
    output: &FrameMetadataBundle,
    expected: FrameMetadataIdentity,
    policy: &MetadataPolicy,
) -> Result<(), MetadataError> {
    if policy.stage != MetadataStage::Processor {
        return Err(MetadataError::WrongOwner);
    }
    let input_actions = input.verify(expected, policy)?;
    output.verify(expected, policy)?;
    for (item, action) in input.items.iter().zip(input_actions) {
        if action == MetadataDisposition::DropOptional {
            continue;
        }
        if !output.items.iter().any(|candidate| candidate == item) {
            return Err(MetadataError::Conflict);
        }
    }
    Ok(())
}
