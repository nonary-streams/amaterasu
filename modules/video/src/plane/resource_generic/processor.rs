//! Negotiated processor composition and synchronous output validation.
use super::*;

pub(super) struct ProcessorDelivery<'a> {
    processor: &'a FrameResourceProcessorApi,
    processor_stream: u64,
    output_consumer: &'a FrameResourceConsumerApi,
    output_stream: u64,
    output_generation: u64,
    output_offer: u64,
}

struct ProcessOutput<'a> {
    destination: &'a ProcessorDelivery<'a>,
    input: &'a CaptureResourceFrame,
    flags: u32,
    calls: u32,
    result: Status,
    invalid: bool,
}

unsafe extern "C" fn deliver_processed(
    context: *mut c_void,
    frame: *const CaptureResourceFrame,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the processor receives this context for a synchronous callback only.
    let state = unsafe { &mut *context.cast::<ProcessOutput<'_>>() };
    state.calls += 1;
    if state.calls != 1 || frame.is_null() {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the processor supplies a borrowed descriptor for this call.
    let output = unsafe { &*frame };
    let input = state.input;
    let same_clock = |a: &amaterasu_media::FrameClockMetadata,
                      b: &amaterasu_media::FrameClockMetadata| {
        a.version == b.version
            && a.size == b.size
            && a.domain == b.domain
            && a.reserved == b.reserved
            && a.epoch_id == b.epoch_id
    };
    if output.offer_generation != state.destination.output_generation
        || output.offer_id != state.destination.output_offer
        || output.reserved != 0
        || !valid_color_metadata(&output.color)
        || !same_clock(&output.clock, &input.clock)
        || !same_clock(&output.pacing_clock, &input.pacing_clock)
        || output.sequence != input.sequence
        || output.timestamp_us != input.timestamp_us
        || output.capture_pacing_timestamp_us != input.capture_pacing_timestamp_us
        || output.freshness != input.freshness
    {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    let consumer = state.destination.output_consumer;
    // SAFETY: negotiated bindings and output descriptor remain live for this callback.
    state.result = unsafe {
        (consumer.submit)(
            consumer.base.context,
            state.destination.output_stream,
            output,
            state.flags,
        )
    };
    state.result
}

/// Invoke one processor while validating its synchronous output contract.
pub(super) unsafe fn process(
    processor: &ProcessorDelivery<'_>,
    frame: &CaptureResourceFrame,
    flags: u32,
) -> Result<(Status, bool), ()> {
    let mut output = ProcessOutput {
        destination: processor,
        input: frame,
        flags,
        calls: 0,
        result: STATUS_OK,
        invalid: false,
    };
    let sink = CaptureResourceFrameSink {
        context: (&mut output as *mut ProcessOutput<'_>).cast(),
        write: deliver_processed,
    };
    // SAFETY: bindings, input and sink remain live until process returns.
    let status = unsafe {
        (processor.processor.process)(
            processor.processor.base.base.context,
            processor.processor_stream,
            frame,
            sink,
        )
    };
    if output.invalid || output.calls > 1 || (output.calls == 1 && status != output.result) {
        return Err(());
    }
    Ok((status, output.calls == 1 && status == STATUS_OK))
}

/// Two independently negotiated edges, retired downstream first so the encoder
/// releases processed frames before the processor retires its own input leases.
pub(crate) struct ProcessorBridge {
    input: ResourceBridge,
    output: ResourceBridge,
    processor_stream: u64,
}

impl ProcessorBridge {
    pub(crate) fn new(
        capture: u64,
        processor: u64,
        encoder: u64,
        epoch: [u8; 16],
    ) -> Result<Self, ConnectionError> {
        Ok(Self {
            input: ResourceBridge::new(capture, processor, 0, epoch)?,
            output: ResourceBridge::new(processor, encoder, 0, epoch)?,
            processor_stream: processor,
        })
    }

    pub(crate) fn is_retired(&self) -> bool {
        self.input.is_retired() || self.output.is_retired()
    }

    pub(crate) fn output_delivered(&self) -> bool {
        self.input.output_delivered
    }

    /// # Safety
    /// All bindings and streams remain live on their owning thread until cancel.
    pub(crate) unsafe fn cancel(
        &mut self,
        processor: &FrameResourceProcessorApi,
        encoder: &FrameResourceConsumerApi,
    ) {
        // SAFETY: caller keeps both bindings alive and cancellation is a barrier.
        unsafe { self.output.cancel(encoder) };
        unsafe { self.input.cancel(&processor.base) };
    }

    /// # Safety
    /// All three authenticated provider bindings remain live for the call and
    /// until cancellation retires every accepted resource.
    pub(crate) unsafe fn forward(
        &mut self,
        capture: &CaptureApiV1_8,
        processor: &FrameResourceProcessorApi,
        encoder: &FrameResourceConsumerApi,
        flags: u32,
    ) -> Result<Status, ConnectionError> {
        // Processor output offers may depend on its selected input, so configure
        // capture -> processor before enumerating processor -> encoder.
        if let Err(error) = unsafe { self.input.negotiate(capture, &processor.base) } {
            if !matches!(
                error,
                ConnectionError::Native(
                    amaterasu_media::MEDIA_BUSY | amaterasu_media::MEDIA_PENDING
                )
            ) || self.is_retired()
            {
                unsafe { self.cancel(processor, encoder) };
            }
            return Err(error);
        }
        if let Err(error) = unsafe {
            self.output
                .negotiate_producer(&Producer::Processor(processor), encoder)
        } {
            if !matches!(
                error,
                ConnectionError::Native(
                    amaterasu_media::MEDIA_BUSY | amaterasu_media::MEDIA_PENDING
                )
            ) || self.is_retired()
            {
                unsafe { self.cancel(processor, encoder) };
            }
            return Err(error);
        }
        let (generation, offer) = self.output.selected.expect("negotiated output");
        let delivery = ProcessorDelivery {
            processor,
            processor_stream: self.processor_stream,
            output_consumer: encoder,
            output_stream: self.output.consumer_stream,
            output_generation: generation,
            output_offer: offer,
        };
        let result = unsafe {
            self.input
                .forward_with_processor(capture, &processor.base, flags, Some(delivery))
        };
        if result.is_err() {
            unsafe { self.cancel(processor, encoder) };
        }
        result
    }
}
