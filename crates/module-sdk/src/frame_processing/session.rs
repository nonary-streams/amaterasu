//! One negotiated capture → processor → consumer chain. Only selection metadata
//! persists between calls; neither a frame queue nor a GPU wait exists here.
use crate::{
    abi::*,
    capture::{ConnectionError, ResourceConnection},
};
use amaterasu_media::*;
use std::{
    cell::Cell, collections::BTreeSet, ffi::c_void, marker::PhantomData, rc::Rc, time::Instant,
};

type InputState = (Option<(u64, u64)>, bool, bool);
/// One independently bound effect, invoked in source-to-consumer order.
pub struct FrameProcessingStage<'a> {
    pub processor: &'a FrameResourceProcessorApi,
    pub stream: u64,
}
struct Stage {
    processor: *const FrameResourceProcessorApi,
    stream: u64,
    output: Option<(u64, u64)>,
}
pub const MAX_PROCESSING_STAGES: usize = 16;
/// Source identity observed by a pump; contains no resource handle or pixels.
#[derive(Clone, Copy, Debug)]
pub struct FrameDelivery {
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
    pub capture_pacing_timestamp_us: u64,
    pub sequence: u64,
    pub freshness: u32,
}

/// Native owners outlive this thread-confined session and its Drop barrier.
/// The downstream consumer is retired before the processor on every exit path.
pub struct FrameProcessingSession {
    capture: *const CaptureApiV1_8,
    capture_stream: u64,
    stages: Vec<Stage>,
    consumer: *const FrameResourceConsumerApi,
    consumer_stream: u64,
    required: u32,
    input: InputState,
    output_active: bool,
    retired: bool,
    configuration_uncertain: bool,
    last_delivery: Option<FrameDelivery>,
    dispatch_us: Cell<u64>,
    module_thread: PhantomData<Rc<()>>,
}
impl FrameProcessingSession {
    /// # Safety
    /// Tables must be authenticated under their exact native contract identity.
    /// Their bindings, streams, descriptor owners and callback code must outlive
    /// this session and its cancellation barrier, on the owning module thread.
    pub unsafe fn new(
        capture: &CaptureApiV1_8,
        capture_stream: u64,
        stages: &[FrameProcessingStage<'_>],
        consumer: &FrameResourceConsumerApi,
        consumer_stream: u64,
        required: u32,
    ) -> Result<Self, ConnectionError> {
        let capture_header = capture.base.base.base.header;
        if capture_header.major != INTERFACE_MAJOR
            || capture_header.minor < CAPTURE_RESOURCE_INTERFACE_MINOR
            || capture_header.size < CAPTURE_API_V1_8_SIZE
            || !valid_consumer(consumer)
            || stages.is_empty()
            || stages.len() > MAX_PROCESSING_STAGES
            || capture_stream == 0
            || consumer_stream == 0
            || required & !(FRAME_ACCESS_CPU | FRAME_ACCESS_GPU) != 0
        {
            return Err(ConnectionError::Protocol);
        }
        let mut selected = Vec::with_capacity(stages.len());
        for stage in stages {
            let header = stage.processor.base.base.header;
            if stage.stream == 0
                || header.major != FRAME_RESOURCE_PROCESSOR_INTERFACE_MAJOR
                || header.minor != FRAME_RESOURCE_PROCESSOR_INTERFACE_MINOR
                || header.size < FRAME_RESOURCE_PROCESSOR_API_V1_0_SIZE
                || selected.iter().any(|previous: &Stage| {
                    std::ptr::eq(previous.processor, stage.processor)
                        && previous.stream == stage.stream
                })
            {
                return Err(ConnectionError::Protocol);
            }
            selected.push(Stage {
                processor: stage.processor,
                stream: stage.stream,
                output: None,
            });
        }
        Ok(Self {
            capture,
            capture_stream,
            stages: selected,
            consumer,
            consumer_stream,
            required,
            input: (None, false, false),
            output_active: false,
            retired: false,
            configuration_uncertain: false,
            last_delivery: None,
            dispatch_us: Cell::new(0),
            module_thread: PhantomData,
        })
    }
    /// CPU time inside processor callbacks, excluding capture, negotiation and
    /// nested downstream dispatch. This does not measure GPU execution.
    pub fn processor_dispatch_us(&self) -> u64 {
        self.dispatch_us.get()
    }
    pub fn last_delivery(&self) -> Option<FrameDelivery> {
        self.last_delivery
    }
    pub fn is_retired(&self) -> bool {
        self.retired
    }
    pub fn cancel(&mut self) {
        if self.retired {
            return;
        }
        self.retired = true;
        // SAFETY: constructor retains both bindings through this ordered barrier.
        unsafe {
            let downstream = &*self.consumer;
            if self.output_active {
                (downstream.cancel)(downstream.base.context, self.consumer_stream);
            }
            for stage in self.stages.iter().rev() {
                let processor = &*stage.processor;
                (processor.base.cancel)(processor.base.base.context, stage.stream);
            }
        }
        self.input = (None, false, true);
        for stage in &mut self.stages {
            stage.output = None;
        }
        self.output_active = false;
    }
    /// # Safety
    /// The new consumer's binding and stream outlive this session. The old
    /// consumer remains live until this ordered retirement call returns.
    pub unsafe fn replace_consumer(
        &mut self,
        consumer: &FrameResourceConsumerApi,
        stream: u64,
    ) -> Result<(), ConnectionError> {
        if !valid_consumer(consumer) {
            return Err(ConnectionError::Protocol);
        }
        if self.retired || stream == 0 {
            return Err(ConnectionError::Stale);
        }
        if std::ptr::eq(self.consumer, consumer) && self.consumer_stream == stream {
            return Ok(());
        }
        if self.output_active {
            let old = unsafe { &*self.consumer };
            unsafe { (old.cancel)(old.base.context, self.consumer_stream) };
        }
        self.consumer = consumer;
        self.consumer_stream = stream;
        self.output_active = false;
        self.stages
            .last_mut()
            .expect("nonempty processing chain")
            .output = None;
        Ok(())
    }
    /// Immediately forwards the current source frame and its dependencies.
    /// BUSY/PENDING consume no queued SDK work; callers retry the latest source.
    pub fn forward(&mut self, flags: u32) -> Result<Status, ConnectionError> {
        if self.retired {
            return Err(ConnectionError::Stale);
        }
        self.last_delivery = None;
        self.dispatch_us.set(0);
        let bridge = FrameResourceConsumerApi {
            base: NativeInterface {
                header: AbiHeader::of::<FrameResourceConsumerApi>(),
                context: std::ptr::from_mut(self).cast(),
                release: None,
            },
            enumerate_resources: inputs,
            configure: configure_input,
            submit: process,
            cancel,
        };
        // SAFETY: native owners and this local bridge are live through forward;
        // the session takes back cancellation ownership before the bridge drops.
        let mut input = unsafe {
            ResourceConnection::new(
                &*self.capture,
                self.capture_stream,
                &bridge,
                self.stages[0].stream,
                self.required,
            )
        }?;
        input.restore_state(self.input);
        let negotiated = input.negotiate();
        self.input = input.take_state();
        negotiated?;
        if let Err(error) = self.negotiate_outputs() {
            if self.configuration_uncertain
                || !matches!(error, ConnectionError::Native(MEDIA_BUSY | MEDIA_PENDING))
            {
                self.cancel();
            }
            return Err(error);
        }
        input.restore_state(self.input);
        let result = input.forward(flags);
        self.input = input.take_state();
        if !matches!(result, Ok(STATUS_OK | MEDIA_BUSY | MEDIA_PENDING)) {
            self.cancel();
        }
        result
    }
    fn negotiate_outputs(&mut self) -> Result<(), ConnectionError> {
        for index in 0..self.stages.len() {
            if self.stages[index].output.is_some() {
                continue;
            }
            // SAFETY: constructor pins all exact typed bindings until the
            // reverse-order cancellation barrier has retired their references.
            let processor = unsafe { &*self.stages[index].processor };
            let stream = self.stages[index].stream;
            let (consumer, consumer_stream) = if let Some(next) = self.stages.get(index + 1) {
                (unsafe { &(*next.processor).base }, next.stream)
            } else {
                (unsafe { &*self.consumer }, self.consumer_stream)
            };
            let mut inputs = Ids::default();
            let mut input_generation = 0;
            let status = unsafe {
                (consumer.enumerate_resources)(
                    consumer.base.context,
                    consumer_stream,
                    inputs.sink(),
                    &mut input_generation,
                )
            };
            check_enumeration(status, input_generation, &inputs)?;
            let context = processor.base.base.context;
            let mut outputs = Ids::default();
            let mut output_generation = 0;
            let status = unsafe {
                (processor.enumerate_outputs)(
                    context,
                    stream,
                    outputs.sink(),
                    &mut output_generation,
                )
            };
            check_enumeration(status, output_generation, &outputs)?;
            let mut configure = Configure {
                ids: Ids::default(),
                consumer,
                stream: consumer_stream,
                consumer_generation: input_generation,
                consumer_ids: &inputs.ids,
                producer_generation: output_generation,
                selection: None,
                active: false,
                failure: None,
            };
            let sink = CaptureResourceOfferSink {
                context: std::ptr::from_mut(&mut configure).cast(),
                write: configure_output,
            };
            let mut second_generation = 0;
            let status = unsafe {
                (processor.enumerate_outputs)(context, stream, sink, &mut second_generation)
            };
            if index + 1 == self.stages.len() {
                self.output_active |= configure.active;
            }
            if let Some(error) = configure.failure {
                self.configuration_uncertain = configure.active;
                return Err(ConnectionError::Native(error));
            }
            check_enumeration(status, second_generation, &configure.ids)?;
            if output_generation != second_generation || outputs.ids != configure.ids.ids {
                return Err(ConnectionError::Stale);
            }
            let offer = configure
                .selection
                .ok_or(ConnectionError::NoCompatibleOffer)?;
            let status =
                unsafe { (processor.select_output)(context, stream, output_generation, offer) };
            if status != STATUS_OK {
                self.configuration_uncertain = true;
                return Err(ConnectionError::Native(status));
            }
            self.stages[index].output = Some((output_generation, offer));
        }
        Ok(())
    }
}
impl Drop for FrameProcessingSession {
    fn drop(&mut self) {
        self.cancel();
    }
}
#[derive(Default)]
struct Ids {
    ids: BTreeSet<u64>,
    invalid: bool,
}
impl Ids {
    fn add(&mut self, offer: &CaptureResourceOffer) -> Status {
        if offer.id == 0
            || offer.reserved != 0
            || self.ids.len() >= super::MAX_PROCESSING_OFFERS
            || !self.ids.insert(offer.id)
        {
            self.invalid = true;
            STATUS_INVALID_ARGUMENT
        } else {
            STATUS_OK
        }
    }
    fn sink(&mut self) -> CaptureResourceOfferSink {
        CaptureResourceOfferSink {
            context: std::ptr::from_mut(self).cast(),
            write: collect,
        }
    }
}
unsafe extern "C" fn collect(context: *mut c_void, offer: *const CaptureResourceOffer) -> Status {
    if offer.is_null() {
        unsafe { &mut *context.cast::<Ids>() }.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: enumeration lends the collector and offer through this callback.
    unsafe { &mut *context.cast::<Ids>() }.add(unsafe { &*offer })
}
fn check_enumeration(status: Status, generation: u64, ids: &Ids) -> Result<(), ConnectionError> {
    if ids.invalid || generation == 0 && status == STATUS_OK {
        Err(ConnectionError::Protocol)
    } else if status != STATUS_OK {
        Err(ConnectionError::Native(status))
    } else {
        Ok(())
    }
}
struct Configure<'a> {
    ids: Ids,
    consumer: &'a FrameResourceConsumerApi,
    stream: u64,
    consumer_generation: u64,
    consumer_ids: &'a BTreeSet<u64>,
    producer_generation: u64,
    selection: Option<u64>,
    active: bool,
    failure: Option<Status>,
}
unsafe extern "C" fn configure_output(
    context: *mut c_void,
    offer: *const CaptureResourceOffer,
) -> Status {
    // SAFETY: enumeration lends this configuration state throughout the callback.
    let state = unsafe { &mut *context.cast::<Configure<'_>>() };
    if offer.is_null() {
        state.ids.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    let offer = unsafe { &*offer };
    let valid = state.ids.add(offer);
    if valid != STATUS_OK {
        return valid;
    }
    if let Some(error) = state.failure {
        return error;
    }
    if state.selection.is_some() {
        return STATUS_OK;
    }
    for id in state.consumer_ids {
        let status = unsafe {
            (state.consumer.configure)(
                state.consumer.base.context,
                state.stream,
                state.consumer_generation,
                *id,
                state.producer_generation,
                offer,
            )
        };
        if status == STATUS_OK {
            state.active = true;
            state.selection = Some(offer.id);
            return status;
        }
        if status != MEDIA_UNSUPPORTED {
            state.active = true;
            state.failure = Some(status);
            return status;
        }
    }
    STATUS_OK
}
unsafe extern "C" fn inputs(
    context: *mut c_void,
    _: u64,
    sink: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    // SAFETY: the local bridge and processor remain live for this call.
    let session = unsafe { &*context.cast::<FrameProcessingSession>() };
    let processor = unsafe { &*session.stages[0].processor };
    unsafe {
        (processor.base.enumerate_resources)(
            processor.base.base.context,
            session.stages[0].stream,
            sink,
            generation,
        )
    }
}
unsafe extern "C" fn configure_input(
    context: *mut c_void,
    _: u64,
    generation: u64,
    id: u64,
    source_generation: u64,
    source: *const CaptureResourceOffer,
) -> Status {
    // SAFETY: the bridge borrows its session and source offer through configure.
    let session = unsafe { &*context.cast::<FrameProcessingSession>() };
    let processor = unsafe { &*session.stages[0].processor };
    unsafe {
        (processor.base.configure)(
            processor.base.base.context,
            session.stages[0].stream,
            generation,
            id,
            source_generation,
            source,
        )
    }
}
unsafe extern "C" fn cancel(context: *mut c_void, _: u64) {
    // SAFETY: the bridge borrows the session exclusively for its ordered barrier.
    unsafe { &mut *context.cast::<FrameProcessingSession>() }.cancel();
}
struct Delivery<'a> {
    session: &'a FrameProcessingSession,
    index: usize,
    flags: u32,
    input: &'a CaptureResourceFrame,
    calls: usize,
    result: Status,
    invalid: bool,
    downstream_us: u64,
}
unsafe extern "C" fn output(context: *mut c_void, frame: *const CaptureResourceFrame) -> Status {
    // SAFETY: the processor lends this stack state through the synchronous call.
    let state = unsafe { &mut *context.cast::<Delivery<'_>>() };
    state.calls += 1;
    if frame.is_null() || state.calls != 1 {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    let frame = unsafe { &*frame };
    if Some((frame.offer_generation, frame.offer_id)) != state.session.stages[state.index].output
        || frame.reserved != 0
        || frame.sequence != state.input.sequence
        || frame.timestamp_us != state.input.timestamp_us
        || frame.capture_pacing_timestamp_us != state.input.capture_pacing_timestamp_us
        || frame.freshness != state.input.freshness
        || !same_clock(frame.clock, state.input.clock)
        || !same_clock(frame.pacing_clock, state.input.pacing_clock)
    {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    let started = Instant::now();
    state.result = if state.index + 1 < state.session.stages.len() {
        // Each stage queues its own wait on the previous stage's readiness;
        // this bounded stack relay never resolves or copies native resources.
        unsafe { process_stage(state.session, state.index + 1, frame, state.flags) }
    } else {
        let consumer = unsafe { &*state.session.consumer };
        unsafe {
            (consumer.submit)(
                consumer.base.context,
                state.session.consumer_stream,
                frame,
                state.flags,
            )
        }
    };
    state.downstream_us = elapsed_us(started);
    state.result
}
unsafe fn process_stage(
    session: &FrameProcessingSession,
    index: usize,
    input: &CaptureResourceFrame,
    flags: u32,
) -> Status {
    let stage = &session.stages[index];
    let processor = unsafe { &*stage.processor };
    let mut delivery = Delivery {
        session,
        index,
        flags,
        input,
        calls: 0,
        result: STATUS_OK,
        invalid: false,
        downstream_us: 0,
    };
    let sink = CaptureResourceFrameSink {
        context: std::ptr::from_mut(&mut delivery).cast(),
        write: output,
    };
    let started = Instant::now();
    let status =
        unsafe { (processor.process)(processor.base.base.context, stage.stream, input, sink) };
    session.dispatch_us.set(
        session
            .dispatch_us
            .get()
            .saturating_add(elapsed_us(started).saturating_sub(delivery.downstream_us)),
    );
    if delivery.invalid
        || delivery.calls > 1
        || delivery.calls == 1 && status != delivery.result
        || delivery.calls == 0 && status == STATUS_OK
    {
        STATUS_INTERNAL_ERROR
    } else {
        status
    }
}
unsafe extern "C" fn process(
    context: *mut c_void,
    _: u64,
    input: *const CaptureResourceFrame,
    flags: u32,
) -> Status {
    // SAFETY: the capture callback lends both the descriptor and local bridge.
    let session = unsafe { &mut *context.cast::<FrameProcessingSession>() };
    let input = unsafe { &*input };
    session.last_delivery = Some(FrameDelivery {
        width: input.width,
        height: input.height,
        timestamp_us: input.timestamp_us,
        capture_pacing_timestamp_us: input.capture_pacing_timestamp_us,
        sequence: input.sequence,
        freshness: input.freshness,
    });
    unsafe { process_stage(session, 0, input, flags) }
}

fn valid_consumer(api: &FrameResourceConsumerApi) -> bool {
    let h = api.base.header;
    h.major == FRAME_RESOURCE_CONSUMER_INTERFACE_MAJOR
        && h.minor == FRAME_RESOURCE_CONSUMER_INTERFACE_MINOR
        && h.size >= FRAME_RESOURCE_CONSUMER_API_V1_0_SIZE
}
pub(super) fn same_clock(a: FrameClockMetadata, b: FrameClockMetadata) -> bool {
    a.version == b.version
        && a.size == b.size
        && a.domain == b.domain
        && a.reserved == b.reserved
        && a.epoch_id == b.epoch_id
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u64::MAX as u128) as u64
}
