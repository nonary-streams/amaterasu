//! Required frame metadata on the selected capture -> processor -> encoder path.
use super::*;
use amaterasu_media::{
    CaptureApiV1_11, FRAME_CLOCK_HOST_MONOTONIC, FrameResourceConsumerApiV1_1,
    FrameResourceMetadataFrameV1, FrameResourceMetadataSinkV1, FrameResourceProcessorApiV1_2,
    MEDIA_BUSY, MEDIA_PENDING,
};
use amaterasu_module_sdk::{
    abi::BorrowedBytes,
    encode::FormatId,
    media::metadata::{
        EncodedMetadataAssociation, EncodedMetadataSelection, FrameMetadataAdmission,
        FrameMetadataBundle, FrameMetadataContext, FrameMetadataIdentity, MetadataPolicy,
        MetadataStage, copy_frame_metadata, verify_processor_transition,
    },
};

/// Generations come from fresh queries to the selected independent providers.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct MetadataCapabilities {
    pub capture: u64,
    pub processor: u64,
    pub encoder: u64,
}

/// One accepted frame is retained until the associated output is retired.
/// Provider leases and output bytes have their own independent lifetimes.
pub(crate) struct MetadataFlow {
    context: FrameMetadataContext,
    capture_policy: MetadataPolicy,
    processor_policy: MetadataPolicy,
    encoder_policy: MetadataPolicy,
    required: Vec<FormatId>,
    admission: FrameMetadataAdmission,
    configured: bool,
    submitted: bool,
}

impl MetadataFlow {
    pub(crate) fn new(
        context: FrameMetadataContext,
        capture_policy: MetadataPolicy,
        processor_policy: MetadataPolicy,
        encoder_policy: MetadataPolicy,
        required: Vec<FormatId>,
    ) -> Result<Self, ConnectionError> {
        if required.is_empty()
            || capture_policy.stage != MetadataStage::Capture
            || processor_policy.stage != MetadataStage::Processor
            || encoder_policy.stage != MetadataStage::Encoder
            || capture_policy.owner != processor_policy.owner
            || capture_policy.owner != encoder_policy.owner
            || capture_policy.session_id != context.session_id
            || processor_policy.session_id != context.session_id
            || encoder_policy.session_id != context.session_id
        {
            return Err(ConnectionError::Protocol);
        }
        let schemas: Vec<_> = required.iter().map(|schema| (*schema, true)).collect();
        context
            .capture_setup(&capture_policy, &schemas)
            .map_err(|_| ConnectionError::Protocol)?;
        let admission = FrameMetadataAdmission::with_required_schemas(
            context,
            encoder_policy.clone(),
            2 * amaterasu_media::FRAME_METADATA_MAX_BUNDLE_BYTES,
            &required,
        )
        .map_err(|_| ConnectionError::Protocol)?;
        Ok(Self {
            context,
            capture_policy,
            processor_policy,
            encoder_policy,
            required,
            admission,
            configured: false,
            submitted: false,
        })
    }
    pub(crate) fn versions_match(&self, current: MetadataCapabilities) -> bool {
        current.capture == self.capture_policy.capability_generation
            && current.processor == self.processor_policy.capability_generation
            && current.encoder == self.encoder_policy.capability_generation
    }
    pub(crate) fn is_current(&self, current: MetadataCapabilities) -> bool {
        self.submitted && self.versions_match(current)
    }
    /// Copy the admitted encoder envelope before claiming its unique output.
    pub(crate) fn output_bytes(
        &self,
        current: MetadataCapabilities,
    ) -> Result<Vec<u8>, ConnectionError> {
        if !self.submitted || !self.versions_match(current) {
            return Err(ConnectionError::Stale);
        }
        self.admission
            .pending(current.encoder)
            .map_err(|_| ConnectionError::Protocol)?
            .map(|frame| frame.bytes().to_vec())
            .ok_or(ConnectionError::Protocol)
    }
    /// Claim exactly one output association after successful input submission.
    pub(crate) fn claim_output(
        &mut self,
        selection: EncodedMetadataSelection,
        current: MetadataCapabilities,
    ) -> Result<EncodedMetadataAssociation, ConnectionError> {
        if !self.submitted || !self.versions_match(current) {
            return Err(ConnectionError::Stale);
        }
        EncodedMetadataAssociation::new(&mut self.admission, current.encoder, selection)
            .map_err(|_| ConnectionError::Protocol)
    }
    /// Retire only after the same association confirms local transport admission.
    pub(crate) fn retire(
        &mut self,
        association: &mut EncodedMetadataAssociation,
        unit: &amaterasu_media::NegotiatedVideoUnit,
        metadata: &[u8],
        current: MetadataCapabilities,
    ) -> Result<(), ConnectionError> {
        if !self.submitted || !self.versions_match(current) {
            return Err(ConnectionError::Stale);
        }
        association
            .retire(unit, metadata, current.encoder)
            .map_err(|_| ConnectionError::Protocol)?;
        self.admission
            .retire(unit.submission_id)
            .map_err(|_| ConnectionError::Protocol)?;
        self.submitted = false;
        Ok(())
    }
    pub(crate) fn cancel(&mut self) {
        self.admission.cancel();
        self.submitted = false;
    }
}

struct CaptureDelivery<'a> {
    flow: &'a mut MetadataFlow,
    processor: &'a FrameResourceProcessorApiV1_2,
    processor_stream: u64,
    encoder: &'a FrameResourceConsumerApiV1_1,
    encoder_stream: u64,
    output_generation: u64,
    output_offer: u64,
    input_generation: u64,
    input_offer: u64,
    flags: u32,
    last_sequence: u64,
    accepted_sequence: Option<u64>,
    calls: usize,
    result: Status,
    invalid: bool,
}
struct ProcessDelivery<'a> {
    flow: &'a mut MetadataFlow,
    input: &'a FrameResourceMetadataFrameV1,
    input_bundle: FrameMetadataBundle,
    expected: FrameMetadataIdentity,
    encoder: &'a FrameResourceConsumerApiV1_1,
    encoder_stream: u64,
    output_generation: u64,
    output_offer: u64,
    flags: u32,
    calls: usize,
    result: Status,
    invalid: bool,
}
unsafe extern "C" fn processed(
    context: *mut c_void,
    frame: *const FrameResourceMetadataFrameV1,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: process owns the live stack delivery for this synchronous callback.
    let delivery = unsafe { &mut *context.cast::<ProcessDelivery<'_>>() };
    delivery.calls += 1;
    if delivery.calls != 1 || frame.is_null() {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the selected processor lends a descriptor and metadata during callback.
    let output = unsafe { &*frame };
    if !valid_output(
        &delivery.input.frame,
        &output.frame,
        delivery.output_generation,
        delivery.output_offer,
    ) {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the processor promises its callback borrows remain live here.
    let output_bundle = match unsafe {
        copy_frame_metadata(output, delivery.expected, &delivery.flow.processor_policy)
    } {
        Ok((bundle, _)) => bundle,
        Err(_) => {
            delivery.invalid = true;
            return STATUS_INVALID_ARGUMENT;
        }
    };
    if verify_processor_transition(
        &delivery.input_bundle,
        &output_bundle,
        delivery.expected,
        &delivery.flow.processor_policy,
    )
    .is_err()
    {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: output and metadata remain valid until this callback returns.
    if unsafe {
        delivery.flow.admission.prepare(
            output,
            delivery.expected,
            delivery.flow.encoder_policy.capability_generation,
        )
    }
    .is_err()
    {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    let pending = match delivery
        .flow
        .admission
        .pending(delivery.flow.encoder_policy.capability_generation)
    {
        Ok(Some(pending)) => pending,
        _ => {
            delivery.invalid = true;
            return STATUS_INVALID_ARGUMENT;
        }
    };
    let forwarded = FrameResourceMetadataFrameV1 {
        metadata: BorrowedBytes::new(pending.bytes()),
        ..*output
    };
    // SAFETY: frame, bounded owned metadata and encoder binding live through the call.
    delivery.result = unsafe {
        (delivery.encoder.submit_with_metadata)(
            delivery.encoder.base.base.context,
            delivery.encoder_stream,
            &forwarded,
            delivery.flags,
        )
    };
    delivery.result
}
unsafe extern "C" fn captured(
    context: *mut c_void,
    frame: *const FrameResourceMetadataFrameV1,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: capture owns the stack delivery for this synchronous callback.
    let delivery = unsafe { &mut *context.cast::<CaptureDelivery<'_>>() };
    delivery.calls += 1;
    if delivery.calls != 1 || frame.is_null() {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the capture provider lends the descriptor through this callback.
    let frame = unsafe { &*frame };
    let resource = &frame.frame;
    if resource.offer_generation != delivery.input_generation
        || resource.offer_id != delivery.input_offer
        || resource.sequence <= delivery.last_sequence
        || resource.clock.domain != FRAME_CLOCK_HOST_MONOTONIC
        || resource.clock.epoch_id != delivery.flow.context.clock_epoch
        || !valid_color_metadata(&resource.color)
        || !valid_clock_metadata(&resource.pacing_clock)
    {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    let context = delivery.flow.context;
    let expected = FrameMetadataIdentity {
        session_id: context.session_id,
        submission_id: resource.sequence,
        source_id: context.source_id,
        source_generation: context.source_generation,
        config_generation: context.config_generation,
        timestamp_us: resource.timestamp_us,
        clock_domain: context.clock_domain,
        clock_epoch: context.clock_epoch,
    };
    // SAFETY: capture promises the bounded wrapper and bytes remain borrowed here.
    let input_bundle =
        match unsafe { copy_frame_metadata(frame, expected, &delivery.flow.capture_policy) } {
            Ok((bundle, _)) => bundle,
            Err(_) => {
                delivery.invalid = true;
                return STATUS_INVALID_ARGUMENT;
            }
        };
    if delivery.flow.required.iter().any(|schema| {
        !input_bundle
            .items
            .iter()
            .any(|item| item.schema == *schema && item.required)
    }) {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    let mut output = ProcessDelivery {
        flow: delivery.flow,
        input: frame,
        input_bundle,
        expected,
        encoder: delivery.encoder,
        encoder_stream: delivery.encoder_stream,
        output_generation: delivery.output_generation,
        output_offer: delivery.output_offer,
        flags: delivery.flags,
        calls: 0,
        result: STATUS_OK,
        invalid: false,
    };
    let sink = FrameResourceMetadataSinkV1 {
        context: (&mut output as *mut ProcessDelivery<'_>).cast(),
        write: processed,
    };
    // SAFETY: all bindings and both nested callback contexts stay live here.
    let status = unsafe {
        (delivery.processor.process_with_metadata)(
            delivery.processor.base.base.base.context,
            delivery.processor_stream,
            frame,
            sink,
        )
    };
    if output.invalid
        || output.calls > 1
        || (output.calls == 1 && status != output.result)
        || (output.calls == 0 && status == STATUS_OK)
    {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    delivery.result = status;
    if status == STATUS_OK {
        delivery.accepted_sequence = Some(resource.sequence);
    }
    status
}

impl ProcessorBridge {
    /// Select both resource edges before binding AFM1 to the source generation.
    /// # Safety
    /// All selected bindings and sessions remain live until cancellation.
    pub(crate) unsafe fn negotiate_metadata(
        &mut self,
        capture: &CaptureApiV1_11,
        processor: &FrameResourceProcessorApiV1_2,
        encoder: &FrameResourceConsumerApiV1_1,
    ) -> Result<u64, ConnectionError> {
        // SAFETY: v1.9 has the complete v1.8 resource prefix and v1.11 covers it.
        let capture_resource = unsafe {
            &*((&capture.base.base as *const amaterasu_media::CaptureApiV1_9)
                .cast::<amaterasu_media::CaptureApiV1_8>())
        };
        unsafe {
            self.input
                .negotiate(capture_resource, &processor.base.base)?;
        }
        unsafe {
            self.output
                .negotiate_producer(&Producer::Processor(&processor.base), &encoder.base)?;
        }
        self.input
            .selected
            .map(|selection| selection.0)
            .ok_or(ConnectionError::Protocol)
    }

    /// Configure capture only after the actual source resource was selected.
    /// # Safety
    /// The capture table and selected session remain live until cancellation.
    pub(crate) unsafe fn setup_metadata(
        &mut self,
        capture: &CaptureApiV1_11,
        flow: &mut MetadataFlow,
    ) -> Result<(), ConnectionError> {
        let (generation, _) = self.input.selected.ok_or(ConnectionError::Protocol)?;
        if generation != flow.context.source_generation
            || flow.context.source_id != self.input.capture_stream
            || flow.context.clock_epoch != self.input.expected_epoch
        {
            return Err(ConnectionError::Stale);
        }
        if !flow.configured {
            let schemas: Vec<_> = flow.required.iter().map(|id| (*id, true)).collect();
            let setup = flow
                .context
                .capture_setup(&flow.capture_policy, &schemas)
                .map_err(|_| ConnectionError::Protocol)?;
            // SAFETY: setup is bounded and borrowed only for this callback.
            let status = unsafe {
                (capture.configure_frame_metadata)(
                    capture.base.base.base.base.base.context,
                    self.input.capture_stream,
                    &setup,
                )
            };
            if status != STATUS_OK {
                return Err(ConnectionError::Native(status));
            }
            flow.configured = true;
        }
        Ok(())
    }

    /// # Safety
    /// The context is selected by trusted composition. All authenticated tables,
    /// providers and any retained frame resources outlive this operation and
    /// downstream-first cancellation. Generations must come from fresh queries.
    pub(crate) unsafe fn forward_metadata(
        &mut self,
        capture: &CaptureApiV1_11,
        processor: &FrameResourceProcessorApiV1_2,
        encoder: &FrameResourceConsumerApiV1_1,
        flow: &mut MetadataFlow,
        current: MetadataCapabilities,
        flags: u32,
    ) -> Result<Status, ConnectionError> {
        if !flow.versions_match(current) {
            unsafe { self.cancel(&processor.base, &encoder.base) };
            flow.cancel();
            return Err(ConnectionError::Stale);
        }
        if flow.submitted {
            return Ok(MEDIA_BUSY);
        }
        // SAFETY: CaptureApiV1_9 preserves the entire flattened v1.8 prefix;
        // the caller validated the v1.11 table and retains its binding.
        let capture_resource = unsafe {
            &*((&capture.base.base as *const amaterasu_media::CaptureApiV1_9)
                .cast::<amaterasu_media::CaptureApiV1_8>())
        };
        if let Err(error) = unsafe { self.input.negotiate(capture_resource, &processor.base.base) }
        {
            if !matches!(error, ConnectionError::Native(MEDIA_BUSY | MEDIA_PENDING)) {
                unsafe { self.cancel(&processor.base, &encoder.base) };
                flow.cancel();
            }
            return Err(error);
        }
        if let Err(error) = unsafe {
            self.output
                .negotiate_producer(&Producer::Processor(&processor.base), &encoder.base)
        } {
            if !matches!(error, ConnectionError::Native(MEDIA_BUSY | MEDIA_PENDING)) {
                unsafe { self.cancel(&processor.base, &encoder.base) };
                flow.cancel();
            }
            return Err(error);
        }
        let (input_generation, input_offer) = self.input.selected.expect("negotiated input");
        if let Err(error) = unsafe { self.setup_metadata(capture, flow) } {
            if !matches!(error, ConnectionError::Native(MEDIA_BUSY | MEDIA_PENDING)) {
                unsafe { self.cancel(&processor.base, &encoder.base) };
                flow.cancel();
            }
            return Err(error);
        }
        let (output_generation, output_offer) = self.output.selected.expect("negotiated output");
        let mut delivery = CaptureDelivery {
            flow,
            processor,
            processor_stream: self.processor_stream,
            encoder,
            encoder_stream: self.output.consumer_stream,
            output_generation,
            output_offer,
            input_generation,
            input_offer,
            flags: flags | self.input.pending_flags,
            last_sequence: self.input.last_sequence,
            accepted_sequence: None,
            calls: 0,
            result: STATUS_OK,
            invalid: false,
        };
        let sink = FrameResourceMetadataSinkV1 {
            context: (&mut delivery as *mut CaptureDelivery<'_>).cast(),
            write: captured,
        };
        // SAFETY: the bindings and callback are retained until the synchronous read returns.
        let status = unsafe {
            (capture.base.read_resource_frame_with_metadata)(
                capture_resource.base.base.base.context,
                self.input.capture_stream,
                sink,
            )
        };
        if delivery.invalid
            || delivery.calls > 1
            || (delivery.calls == 1 && status != delivery.result)
            || (delivery.calls == 0 && status == STATUS_OK)
            || (delivery.calls == 0 && status == amaterasu_media::MEDIA_UNSUPPORTED)
        {
            unsafe { self.cancel(&processor.base, &encoder.base) };
            delivery.flow.cancel();
            return Err(ConnectionError::Protocol);
        }
        if !matches!(status, STATUS_OK | MEDIA_BUSY | MEDIA_PENDING) {
            unsafe { self.cancel(&processor.base, &encoder.base) };
            delivery.flow.cancel();
            return Err(ConnectionError::Native(status));
        }
        if let Some(sequence) = delivery.accepted_sequence {
            self.input.last_sequence = sequence;
            self.input.output_delivered = true;
            self.input.pending_flags = 0;
            delivery.flow.submitted = true;
        }
        Ok(status)
    }
}
