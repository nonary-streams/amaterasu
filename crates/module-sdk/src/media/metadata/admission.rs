//! Owned, bounded one-frame admission and retry state.
use super::*;

/// The authenticated identity that remains fixed for one admission instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameMetadataContext {
    pub session_id: u64,
    pub source_id: u64,
    pub source_generation: u64,
    pub config_generation: u64,
    pub clock_domain: u32,
    pub clock_epoch: [u8; 16],
}
impl FrameMetadataContext {
    fn matches(self, identity: FrameMetadataIdentity) -> bool {
        self.session_id == identity.session_id
            && self.source_id == identity.source_id
            && self.source_generation == identity.source_generation
            && self.config_generation == identity.config_generation
            && self.clock_domain == identity.clock_domain
            && self.clock_epoch == identity.clock_epoch
    }
    fn valid(self) -> bool {
        FrameMetadataIdentity {
            session_id: self.session_id,
            submission_id: 1,
            source_id: self.source_id,
            source_generation: self.source_generation,
            config_generation: self.config_generation,
            timestamp_us: 0,
            clock_domain: self.clock_domain,
            clock_epoch: self.clock_epoch,
        }
        .valid()
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataAdmissionResult {
    Prepared,
    Retry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FrameBinding {
    offer_generation: u64,
    offer_id: u64,
    pacing_timestamp_us: u64,
    width: u32,
    height: u32,
    freshness: u32,
    color: [u32; 6],
    pacing_clock: (u32, u32, u32, u32, [u8; 16]),
    resource: (u32, u32, u32, [u8; 16]),
    acquire: (u32, u32, u32, [u8; 16]),
}
impl FrameBinding {
    fn from_frame(frame: &amaterasu_media::CaptureResourceFrame) -> Self {
        let color = frame.color;
        let clock = frame.pacing_clock;
        let resource = frame.resource;
        let acquire = frame.acquire_sync;
        Self {
            offer_generation: frame.offer_generation,
            offer_id: frame.offer_id,
            pacing_timestamp_us: frame.capture_pacing_timestamp_us,
            width: frame.width,
            height: frame.height,
            freshness: frame.freshness,
            color: [
                color.version,
                color.size,
                color.primaries,
                color.transfer,
                color.matrix,
                color.range,
            ],
            pacing_clock: (
                clock.version,
                clock.size,
                clock.domain,
                clock.reserved,
                clock.epoch_id,
            ),
            resource: (
                resource.kind,
                resource.version,
                resource.size,
                resource.namespace,
            ),
            acquire: (
                acquire.kind,
                acquire.version,
                acquire.size,
                acquire.namespace,
            ),
        }
    }
}

/// Immutable metadata retained for one in-flight submission. Metadata bytes
/// are separately owned from any retained capture frame or GPU resource.
pub struct PreparedFrameMetadata {
    identity: FrameMetadataIdentity,
    frame_binding: FrameBinding,
    source_bytes: Vec<u8>,
    outbound_bytes: Vec<u8>,
    stage: MetadataStage,
    output_claimed: bool,
}
/// A borrowed encoder control descriptor. Its nested pointer cannot outlive the
/// prepared envelope through this safe wrapper.
pub struct BorrowedEncoderFrameOptions<'a> {
    descriptor: amaterasu_media::EncoderFrameOptionsV1,
    _owner: std::marker::PhantomData<&'a PreparedFrameMetadata>,
}
impl BorrowedEncoderFrameOptions<'_> {
    pub fn descriptor(&self) -> &amaterasu_media::EncoderFrameOptionsV1 {
        &self.descriptor
    }
}
impl PreparedFrameMetadata {
    pub fn identity(&self) -> FrameMetadataIdentity {
        self.identity
    }
    pub fn bytes(&self) -> &[u8] {
        &self.outbound_bytes
    }
    /// Borrowed nested bytes remain valid only while this value is retained.
    /// The caller must keep them alive for the synchronous control callback.
    pub fn encoder_options(&self) -> Result<BorrowedEncoderFrameOptions<'_>, MetadataError> {
        if self.stage != MetadataStage::Encoder {
            return Err(MetadataError::WrongOwner);
        }
        let id = self.identity;
        Ok(BorrowedEncoderFrameOptions {
            descriptor: amaterasu_media::EncoderFrameOptionsV1 {
                version: 1,
                size: std::mem::size_of::<amaterasu_media::EncoderFrameOptionsV1>() as u32,
                submission_id: id.submission_id,
                source_id: id.source_id,
                source_generation: id.source_generation,
                timestamp_us: id.timestamp_us,
                clock: FrameClockMetadata {
                    version: 1,
                    size: std::mem::size_of::<FrameClockMetadata>() as u32,
                    domain: id.clock_domain,
                    reserved: 0,
                    epoch_id: id.clock_epoch,
                },
                side_data_schema: amaterasu_media::EncoderFormatId {
                    namespace_id: BUNDLE_SCHEMA.namespace_id,
                    value: BUNDLE_SCHEMA.value,
                    schema: BUNDLE_SCHEMA.schema,
                },
                side_data: amaterasu_abi::BorrowedBytes::new(&self.outbound_bytes),
            },
            _owner: std::marker::PhantomData,
        })
    }
}

/// A bounded one-in-flight admission guard. Use one per authenticated session,
/// source/configuration epoch and clock; IDs must increase when a frame is
/// completed or discarded. This does not retain the frame or authorize a caller.
pub struct FrameMetadataAdmission {
    context: FrameMetadataContext,
    policy: MetadataPolicy,
    max_bytes: usize,
    last_retired: u64,
    pending: Option<PreparedFrameMetadata>,
    closed: bool,
}
impl FrameMetadataAdmission {
    /// `max_bytes` bounds both the original and policy-filtered envelope copies.
    pub fn new(
        context: FrameMetadataContext,
        policy: MetadataPolicy,
        max_bytes: usize,
    ) -> Result<Self, MetadataError> {
        if !context.valid() || max_bytes == 0 || max_bytes > 2 * FRAME_METADATA_MAX_BUNDLE_BYTES {
            return Err(MetadataError::Invalid);
        }
        let empty = FrameMetadataBundle {
            identity: FrameMetadataIdentity {
                session_id: context.session_id,
                submission_id: 1,
                source_id: context.source_id,
                source_generation: context.source_generation,
                config_generation: context.config_generation,
                timestamp_us: 0,
                clock_domain: context.clock_domain,
                clock_epoch: context.clock_epoch,
            },
            owner: policy.owner,
            items: Vec::new(),
        };
        policy.validate(&empty)?;
        Ok(Self {
            context,
            policy,
            max_bytes,
            last_retired: 0,
            pending: None,
            closed: false,
        })
    }
    /// Copy and admit a callback's frame metadata. A retry must have the same
    /// identity and exact original bytes. The caller must also independently
    /// retain or reacquire the same frame resource when required by its provider.
    ///
    /// # Safety
    /// Wrapper and borrowed descriptors/bytes remain valid through this call.
    pub unsafe fn prepare(
        &mut self,
        frame: &FrameResourceMetadataFrameV1,
        expected: FrameMetadataIdentity,
        current_capability_generation: u64,
    ) -> Result<MetadataAdmissionResult, MetadataError> {
        if self.closed {
            return Err(MetadataError::Closed);
        }
        if !self.context.matches(expected) {
            return Err(MetadataError::Foreign);
        }
        if current_capability_generation != self.policy.capability_generation {
            return Err(MetadataError::Stale);
        }
        // SAFETY: caller guarantees wrapper and metadata storage remain live.
        let (mut bundle, dispositions) =
            unsafe { copy_frame_metadata(frame, expected, &self.policy) }?;
        // SAFETY: the caller guarantees initialized live bytes; decoding established a nonempty envelope.
        let source = unsafe { std::slice::from_raw_parts(frame.metadata.ptr, frame.metadata.len) };
        if let Some(pending) = &self.pending {
            if pending.identity.submission_id != expected.submission_id {
                return Err(MetadataError::Busy);
            }
            if pending.identity != expected
                || pending.frame_binding != FrameBinding::from_frame(&frame.frame)
                || pending.source_bytes != source
            {
                return Err(MetadataError::Conflict);
            }
            return Ok(MetadataAdmissionResult::Retry);
        }
        if expected.submission_id <= self.last_retired {
            return Err(MetadataError::Stale);
        }
        let mut index = 0;
        bundle.items.retain(|_| {
            let keep = dispositions[index] != MetadataDisposition::DropOptional;
            index += 1;
            keep
        });
        let outbound = bundle.encode()?;
        if source
            .len()
            .checked_add(outbound.len())
            .is_none_or(|bytes| bytes > self.max_bytes)
        {
            return Err(MetadataError::TooLarge);
        }
        self.pending = Some(PreparedFrameMetadata {
            identity: expected,
            frame_binding: FrameBinding::from_frame(&frame.frame),
            source_bytes: source.to_vec(),
            outbound_bytes: outbound,
            stage: self.policy.stage,
            output_claimed: false,
        });
        Ok(MetadataAdmissionResult::Prepared)
    }
    pub fn pending(
        &self,
        current_capability_generation: u64,
    ) -> Result<Option<&PreparedFrameMetadata>, MetadataError> {
        if self.closed {
            return Err(MetadataError::Closed);
        }
        if current_capability_generation != self.policy.capability_generation {
            return Err(MetadataError::Stale);
        }
        Ok(self.pending.as_ref())
    }
    /// Claim one output association for this admitted submission. A dropped
    /// association cannot be reconstructed; cancel or retire the submission.
    pub(super) fn claim_output(
        &mut self,
        submission_id: u64,
        current_capability_generation: u64,
    ) -> Result<(), MetadataError> {
        if self.closed {
            return Err(MetadataError::Closed);
        }
        if current_capability_generation != self.policy.capability_generation {
            return Err(MetadataError::Stale);
        }
        let pending = self.pending.as_mut().ok_or(MetadataError::Invalid)?;
        if pending.identity.submission_id != submission_id {
            return Err(MetadataError::Foreign);
        }
        if pending.stage != MetadataStage::Encoder {
            return Err(MetadataError::WrongOwner);
        }
        if pending.output_claimed {
            return Err(MetadataError::Duplicate);
        }
        pending.output_claimed = true;
        Ok(())
    }
    /// Mark an accepted or intentionally discarded input as retired exactly once.
    pub fn retire(&mut self, submission_id: u64) -> Result<(), MetadataError> {
        if self.closed {
            return Err(MetadataError::Closed);
        }
        if !self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.identity.submission_id == submission_id)
        {
            return Err(MetadataError::Foreign);
        }
        self.last_retired = submission_id;
        self.pending = None;
        Ok(())
    }
    /// Cancel this guard's metadata and prevent further admission. This does not
    /// cancel provider work or retire any frame/GPU lease; the owning pipeline
    /// must perform those separate barriers before releasing bindings.
    pub fn cancel(&mut self) {
        self.pending = None;
        self.closed = true;
    }
}
