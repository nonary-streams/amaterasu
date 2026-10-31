//! Synchronous, schema-agnostic capture-to-consumer forwarding.
mod processor;
use amaterasu_media::{
    CaptureApiV1_8, CaptureResourceFrame, CaptureResourceFrameSink, CaptureResourceOffer,
    CaptureResourceOfferSink, FRAME_ACCESS_CPU, FRAME_ACCESS_GPU, FrameResourceConsumerApi,
    FrameResourceProcessorApi, MEDIA_UNSUPPORTED,
};
use amaterasu_module_sdk::abi::{STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
pub(super) use processor::ProcessorBridge;
use processor::ProcessorDelivery;
use std::{ffi::c_void, marker::PhantomData, rc::Rc};

const MAX_OFFERS: usize = 64;

/// A protocol or native operation failure. A native status is preserved so the
/// caller can distinguish backpressure from a terminal provider failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionError {
    Protocol,
    Native(Status),
    NoCompatibleOffer,
    Stale,
}

struct Ids {
    ids: Vec<u64>,
    invalid: bool,
    eligible: Vec<u64>,
    required: u32,
}

impl Ids {
    fn new(required: u32) -> Self {
        Self {
            ids: Vec::new(),
            invalid: false,
            eligible: Vec::new(),
            required,
        }
    }
    fn add(&mut self, offer: &CaptureResourceOffer) -> bool {
        if offer.id == 0
            || offer.reserved != 0
            || offer.accessibility & !(FRAME_ACCESS_CPU | FRAME_ACCESS_GPU) != 0
            || self.ids.len() >= MAX_OFFERS
            || self.ids.contains(&offer.id)
        {
            self.invalid = true;
            return false;
        }
        self.ids.push(offer.id);
        if offer.accessibility & self.required == self.required {
            self.eligible.push(offer.id);
        }
        true
    }
}

unsafe extern "C" fn collect(context: *mut c_void, offer: *const CaptureResourceOffer) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: enumeration supplies a live context.
    let ids = unsafe { &mut *context.cast::<Ids>() };
    if offer.is_null() {
        ids.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: enumeration supplies a live offer.
    let offer = unsafe { &*offer };
    if ids.add(offer) {
        STATUS_OK
    } else {
        STATUS_INVALID_ARGUMENT
    }
}

enum Producer<'a> {
    Capture(&'a CaptureApiV1_8),
    Processor(&'a FrameResourceProcessorApi),
}

impl Producer<'_> {
    /// Bindings, stream and the synchronous sink must remain live for this call.
    unsafe fn enumerate(
        &self,
        stream: u64,
        sink: CaptureResourceOfferSink,
        generation: &mut u64,
    ) -> Status {
        match self {
            Self::Capture(api) => unsafe {
                (api.enumerate_resources)(api.base.base.base.context, stream, sink, generation)
            },
            Self::Processor(api) => unsafe {
                (api.enumerate_outputs)(api.base.base.context, stream, sink, generation)
            },
        }
    }

    /// Binding and stream must remain live through selection.
    unsafe fn select(&self, stream: u64, generation: u64, offer: u64) -> Status {
        match self {
            Self::Capture(api) => unsafe {
                (api.select_resource)(api.base.base.base.context, stream, generation, offer)
            },
            Self::Processor(api) => unsafe {
                (api.select_output)(api.base.base.context, stream, generation, offer)
            },
        }
    }
}

struct Configure<'a> {
    ids: Ids,
    consumer_ids: &'a [u64],
    consumer: &'a FrameResourceConsumerApi,
    stream: u64,
    generation: u64,
    producer_generation: u64,
    required: u32,
    selected: Option<u64>,
    failure: Option<Status>,
}
unsafe extern "C" fn configure(context: *mut c_void, offer: *const CaptureResourceOffer) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: enumeration supplies a live context.
    let state = unsafe { &mut *context.cast::<Configure<'_>>() };
    if offer.is_null() {
        state.ids.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: enumeration supplies a live offer.
    let offer = unsafe { &*offer };
    if !state.ids.add(offer) {
        return STATUS_INVALID_ARGUMENT;
    }
    if state.selected.is_some() || offer.accessibility & state.required != state.required {
        return STATUS_OK;
    }
    for id in state.consumer_ids {
        // SAFETY: both bindings and the borrowed offer remain live for this call.
        let status = unsafe {
            (state.consumer.configure)(
                state.consumer.base.context,
                state.stream,
                state.generation,
                *id,
                state.producer_generation,
                offer,
            )
        };
        if status == STATUS_OK {
            state.selected = Some(offer.id);
            break;
        }
        if status != MEDIA_UNSUPPORTED {
            state.failure = Some(status);
            return status;
        }
    }
    STATUS_OK
}

fn valid_color_metadata(color: &amaterasu_media::FrameColorMetadata) -> bool {
    match color.version {
        0 => {
            color.size == 0
                && color.primaries == 0
                && color.transfer == 0
                && color.matrix == 0
                && color.range == 0
        }
        1 => color.size as usize == std::mem::size_of::<amaterasu_media::FrameColorMetadata>(),
        _ => true,
    }
}

fn valid_clock_metadata(clock: &amaterasu_media::FrameClockMetadata) -> bool {
    match clock.version {
        0 => {
            clock.size == 0 && clock.domain == 0 && clock.reserved == 0 && clock.epoch_id == [0; 16]
        }
        1 => {
            clock.size as usize == std::mem::size_of::<amaterasu_media::FrameClockMetadata>()
                && clock.reserved == 0
        }
        _ => true,
    }
}

struct Delivery<'a> {
    consumer: &'a FrameResourceConsumerApi,
    stream: u64,
    generation: u64,
    offer: u64,
    flags: u32,
    expected_epoch: [u8; 16],
    last_sequence: u64,
    accepted_sequence: Option<u64>,
    calls: u32,
    result: Status,
    invalid: bool,
    processor: Option<ProcessorDelivery<'a>>,
    output_delivered: bool,
}

unsafe extern "C" fn deliver(context: *mut c_void, frame: *const CaptureResourceFrame) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: read supplies this stack context throughout the callback.
    let state = unsafe { &mut *context.cast::<Delivery<'_>>() };
    state.calls += 1;
    if frame.is_null() || state.calls != 1 {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: producer supplies the frame for the synchronous callback.
    let frame = unsafe { &*frame };
    if frame.offer_generation != state.generation
        || frame.offer_id != state.offer
        || frame.reserved != 0
        || !valid_color_metadata(&frame.color)
        || !valid_clock_metadata(&frame.clock)
        || frame.clock.version != 1
        || frame.clock.domain != amaterasu_media::FRAME_CLOCK_HOST_MONOTONIC
        || frame.clock.epoch_id != state.expected_epoch
        || frame.sequence <= state.last_sequence
        || !valid_clock_metadata(&frame.pacing_clock)
    {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    if let Some(processor) = &state.processor {
        // SAFETY: all bindings and the borrowed frame remain live through process.
        match unsafe { processor::process(processor, frame, state.flags) } {
            Ok((status, delivered)) => {
                state.output_delivered = delivered;
                state.result = status;
            }
            Err(()) => {
                state.invalid = true;
                return STATUS_INVALID_ARGUMENT;
            }
        }
    } else {
        // SAFETY: the consumer binding and borrowed frame remain live during submission.
        state.result = unsafe {
            (state.consumer.submit)(
                state.consumer.base.context,
                state.stream,
                frame,
                state.flags,
            )
        };
        state.output_delivered = state.result == STATUS_OK;
    }
    if state.result == STATUS_OK {
        state.accepted_sequence = Some(frame.sequence);
    }
    state.result
}

/// Per-stream negotiation state. The caller must cancel before releasing either
/// binding or closing its stream. No native table or frame pointer is retained.
/// Keep this object on the providers' owning thread.
pub(super) struct ResourceBridge {
    capture_stream: u64,
    consumer_stream: u64,
    selected: Option<(u64, u64)>,
    active: bool,
    retired: bool,
    required: u32,
    expected_epoch: [u8; 16],
    last_sequence: u64,
    pending_flags: u32,
    output_delivered: bool,
    thread: PhantomData<Rc<()>>,
}

impl ResourceBridge {
    pub(super) fn new(
        capture_stream: u64,
        consumer_stream: u64,
        required: u32,
        expected_host_epoch: [u8; 16],
    ) -> Result<Self, ConnectionError> {
        if required & !(FRAME_ACCESS_CPU | FRAME_ACCESS_GPU) != 0 || expected_host_epoch == [0; 16]
        {
            return Err(ConnectionError::Protocol);
        }
        Ok(Self {
            capture_stream,
            consumer_stream,
            selected: None,
            active: false,
            retired: false,
            required,
            expected_epoch: expected_host_epoch,
            last_sequence: 0,
            pending_flags: 0,
            output_delivered: false,
            thread: PhantomData,
        })
    }

    pub(super) fn is_retired(&self) -> bool {
        self.retired
    }

    /// # Safety
    /// The authenticated consumer binding and stream must still be live, and
    /// this must run on the provider's owning thread before either is released.
    pub(super) unsafe fn cancel(&mut self, consumer: &FrameResourceConsumerApi) {
        if self.active {
            // SAFETY: caller guarantees the binding and stream are live.
            unsafe { (consumer.cancel)(consumer.base.context, self.consumer_stream) };
            self.active = false;
            self.retired = true;
        }
        self.selected = None;
    }

    unsafe fn enumerate_producer(
        &self,
        producer: &Producer<'_>,
        sink: CaptureResourceOfferSink,
    ) -> Result<u64, ConnectionError> {
        let mut generation = 0;
        // SAFETY: caller guarantees the binding and stream; callback is synchronous.
        let status = unsafe { producer.enumerate(self.capture_stream, sink, &mut generation) };
        if status != STATUS_OK {
            return Err(ConnectionError::Native(status));
        }
        if generation == 0 {
            return Err(ConnectionError::Protocol);
        }
        Ok(generation)
    }

    /// # Safety
    /// Both independently authenticated bindings and their streams must be live,
    /// belong to the intended principal/session, and be called on their owning
    /// thread. The caller must cancel before releasing either binding.
    pub(super) unsafe fn negotiate(
        &mut self,
        capture: &CaptureApiV1_8,
        consumer: &FrameResourceConsumerApi,
    ) -> Result<(), ConnectionError> {
        unsafe { self.negotiate_producer(&Producer::Capture(capture), consumer) }
    }

    unsafe fn negotiate_producer(
        &mut self,
        producer: &Producer<'_>,
        consumer: &FrameResourceConsumerApi,
    ) -> Result<(), ConnectionError> {
        if self.retired {
            return Err(ConnectionError::Stale);
        }
        if self.selected.is_some() {
            return Ok(());
        }
        let mut consumer_ids = Ids::new(self.required);
        let mut consumer_generation = 0;
        let sink = CaptureResourceOfferSink {
            context: (&mut consumer_ids as *mut Ids).cast(),
            write: collect,
        };
        // SAFETY: the caller guarantees a live binding and callbacks are synchronous.
        let status = unsafe {
            (consumer.enumerate_resources)(
                consumer.base.context,
                self.consumer_stream,
                sink,
                &mut consumer_generation,
            )
        };
        if consumer_ids.invalid {
            return Err(ConnectionError::Protocol);
        }
        if status != STATUS_OK {
            return Err(ConnectionError::Native(status));
        }
        if consumer_generation == 0 {
            return Err(ConnectionError::Protocol);
        }
        let mut first = Ids::new(0);
        let sink = CaptureResourceOfferSink {
            context: (&mut first as *mut Ids).cast(),
            write: collect,
        };
        // SAFETY: the caller guarantees a live capture binding.
        let first_result = unsafe { self.enumerate_producer(producer, sink) };
        if first.invalid {
            return Err(ConnectionError::Protocol);
        }
        let producer_generation = first_result?;
        let mut state = Configure {
            ids: Ids::new(0),
            consumer_ids: &consumer_ids.eligible,
            consumer,
            stream: self.consumer_stream,
            generation: consumer_generation,
            producer_generation,
            required: self.required,
            selected: None,
            failure: None,
        };
        let sink = CaptureResourceOfferSink {
            context: (&mut state as *mut Configure<'_>).cast(),
            write: configure,
        };
        // SAFETY: the caller guarantees both live bindings.
        let second = unsafe { self.enumerate_producer(producer, sink) };
        self.active = state.selected.is_some();
        let error = if state.ids.invalid {
            Some(ConnectionError::Protocol)
        } else {
            state.failure.map(ConnectionError::Native)
        };
        if let Some(error) = error {
            // SAFETY: the caller guarantees the live consumer binding.
            unsafe { self.cancel(consumer) };
            return Err(error);
        }
        let second_generation = match second {
            Ok(generation) => generation,
            Err(error) => {
                // SAFETY: the caller guarantees the live consumer binding.
                unsafe { self.cancel(consumer) };
                return Err(error);
            }
        };
        first.ids.sort_unstable();
        state.ids.ids.sort_unstable();
        if producer_generation != second_generation || first.ids != state.ids.ids {
            // SAFETY: the caller guarantees the live consumer binding.
            unsafe { self.cancel(consumer) };
            return Err(ConnectionError::Stale);
        }
        let Some(offer) = state.selected else {
            return Err(ConnectionError::NoCompatibleOffer);
        };
        // SAFETY: the caller guarantees the live capture binding and stream.
        let status = unsafe { producer.select(self.capture_stream, producer_generation, offer) };
        if status != STATUS_OK {
            // SAFETY: the caller guarantees the live consumer binding.
            unsafe { self.cancel(consumer) };
            return Err(if status == MEDIA_UNSUPPORTED {
                ConnectionError::Stale
            } else {
                ConnectionError::Native(status)
            });
        }
        self.selected = Some((producer_generation, offer));
        Ok(())
    }

    /// # Safety
    /// The requirements of `negotiate` apply throughout this synchronous call.
    pub(super) unsafe fn forward(
        &mut self,
        capture: &CaptureApiV1_8,
        consumer: &FrameResourceConsumerApi,
        flags: u32,
    ) -> Result<Status, ConnectionError> {
        unsafe { self.forward_with_processor(capture, consumer, flags, None) }
    }

    unsafe fn forward_with_processor(
        &mut self,
        capture: &CaptureApiV1_8,
        consumer: &FrameResourceConsumerApi,
        flags: u32,
        processor: Option<ProcessorDelivery<'_>>,
    ) -> Result<Status, ConnectionError> {
        if self.selected.is_none() {
            // SAFETY: caller supplies both live bindings.
            unsafe { self.negotiate(capture, consumer) }?;
        }
        let (generation, offer) = self.selected.expect("negotiated selection");
        self.output_delivered = false;
        let mut delivery = Delivery {
            consumer,
            stream: self.consumer_stream,
            generation,
            offer,
            flags: flags | self.pending_flags,
            expected_epoch: self.expected_epoch,
            last_sequence: self.last_sequence,
            accepted_sequence: None,
            calls: 0,
            result: STATUS_OK,
            invalid: false,
            processor,
            output_delivered: false,
        };
        let sink = CaptureResourceFrameSink {
            context: (&mut delivery as *mut Delivery<'_>).cast(),
            write: deliver,
        };
        // SAFETY: bindings and callback context remain live through this synchronous read.
        let status = unsafe {
            (capture.read_resource_frame)(capture.base.base.base.context, self.capture_stream, sink)
        };
        if delivery.invalid
            || delivery.calls > 1
            || (delivery.calls == 1 && status != delivery.result)
            || (delivery.calls == 0 && status == STATUS_OK)
        {
            // A composed bridge must retire the downstream encoder first.
            if delivery.processor.is_none() {
                unsafe { self.cancel(consumer) };
            }
            return Err(ConnectionError::Protocol);
        }
        if delivery.calls == 0 && status == MEDIA_UNSUPPORTED {
            if delivery.processor.is_none() {
                unsafe { self.cancel(consumer) };
            }
            return Err(ConnectionError::Stale);
        }
        if let Some(sequence) = delivery.accepted_sequence {
            self.last_sequence = sequence;
            self.output_delivered = delivery.output_delivered;
            self.pending_flags = if delivery.output_delivered {
                0
            } else {
                delivery.flags
            };
        }
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_media::{
        FRAME_OWNERSHIP_RETAINABLE, FRAME_RESOURCE_VENDOR_BASE, FRAME_SYNC_NONE, FrameDescriptor,
    };
    use amaterasu_module_sdk::abi::{AbiHeader, NativeInterface};
    use std::ptr;

    #[derive(Default)]
    struct Consumer {
        configured: u32,
        submitted: u32,
        transfer: u32,
        epoch: [u8; 16],
        cancelled: u32,
        reject: bool,
        busy: bool,
        seen_stream: u64,
        seen_generation: u64,
        seen_producer_generation: u64,
        namespace: Option<[u8; 16]>,
        last_flags: u32,
        hold: bool,
        held: Option<CaptureResourceFrame>,
        cancel_log: Option<std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>>,
    }
    unsafe extern "C" fn enumerate(
        _: *mut c_void,
        _: u64,
        _: CaptureResourceOfferSink,
        _: *mut u64,
    ) -> Status {
        STATUS_OK
    }
    unsafe extern "C" fn config(
        context: *mut c_void,
        stream: u64,
        generation: u64,
        id: u64,
        producer_generation: u64,
        offer: *const CaptureResourceOffer,
    ) -> Status {
        // SAFETY: fixture supplies both pointers for the call.
        let (consumer, offer) = unsafe { (&mut *context.cast::<Consumer>(), &*offer) };
        consumer.seen_stream = stream;
        consumer.seen_generation = generation;
        consumer.seen_producer_generation = producer_generation;
        if consumer.reject
            || consumer
                .namespace
                .is_some_and(|ns| ns != offer.resource.namespace)
            || id != 8
            || offer.resource.kind != FRAME_RESOURCE_VENDOR_BASE
        {
            return MEDIA_UNSUPPORTED;
        }
        consumer.configured += 1;
        STATUS_OK
    }
    unsafe extern "C" fn submit(
        context: *mut c_void,
        _: u64,
        frame: *const CaptureResourceFrame,
        flags: u32,
    ) -> Status {
        // SAFETY: fixture supplies the consumer context and borrowed frame.
        unsafe {
            let consumer = &mut *context.cast::<Consumer>();
            consumer.last_flags = flags;
            if consumer.busy {
                return amaterasu_media::MEDIA_BUSY;
            }
            if consumer.hold {
                let status = ((*frame).retain)((*frame).context);
                if status != STATUS_OK {
                    return status;
                }
                consumer.held = Some(*frame);
            }
            consumer.submitted += 1;
            consumer.transfer = (*frame).color.transfer;
            consumer.epoch = (*frame).pacing_clock.epoch_id;
        }
        STATUS_OK
    }
    unsafe extern "C" fn cancel(context: *mut c_void, _: u64) {
        // SAFETY: fixture supplies the consumer context.
        unsafe {
            let consumer = &mut *context.cast::<Consumer>();
            consumer.cancelled += 1;
            if let Some(frame) = consumer.held.take() {
                let completion = descriptor(amaterasu_media::FRAME_SYNC_VENDOR_BASE);
                (frame.release)(frame.context, &completion);
            }
            if let Some(log) = &consumer.cancel_log {
                log.borrow_mut().push("encoder");
            }
        }
    }
    fn table(consumer: &mut Consumer) -> FrameResourceConsumerApi {
        FrameResourceConsumerApi {
            base: NativeInterface {
                header: AbiHeader::of::<FrameResourceConsumerApi>(),
                context: (consumer as *mut Consumer).cast(),
                release: None,
            },
            enumerate_resources: enumerate,
            configure: config,
            submit,
            cancel,
        }
    }
    fn descriptor(kind: u32) -> FrameDescriptor {
        FrameDescriptor {
            kind,
            version: 0,
            size: 0,
            reserved: 0,
            namespace: [0; 16],
            data: ptr::null(),
        }
    }
    fn offer() -> CaptureResourceOffer {
        CaptureResourceOffer {
            id: 5,
            ownership: FRAME_OWNERSHIP_RETAINABLE,
            accessibility: 0,
            reserved: 0,
            resource: descriptor(FRAME_RESOURCE_VENDOR_BASE),
            acquire_sync: descriptor(FRAME_SYNC_NONE),
            completion_sync: descriptor(FRAME_SYNC_NONE),
        }
    }
    unsafe extern "C" fn retain(_: *mut c_void) -> Status {
        STATUS_OK
    }
    unsafe extern "C" fn release(_: *mut c_void, _: *const FrameDescriptor) {}
    #[test]
    fn custom_offer_configures_and_frame_is_submitted_unchanged() {
        let mut consumer = Consumer::default();
        let api = table(&mut consumer);
        let mut state = Configure {
            ids: Ids::new(0),
            consumer_ids: &[7, 8],
            consumer: &api,
            stream: 3,
            generation: 4,
            producer_generation: 9,
            required: 0,
            selected: None,
            failure: None,
        };
        let offer = offer();
        // SAFETY: fixture objects outlive each synchronous callback.
        assert_eq!(
            unsafe { configure((&mut state as *mut Configure<'_>).cast(), &offer) },
            STATUS_OK
        );
        assert_eq!(state.selected, Some(5));
        let frame = CaptureResourceFrame {
            offer_generation: 9,
            offer_id: 5,
            timestamp_us: 1,
            capture_pacing_timestamp_us: 1,
            sequence: 1,
            width: 1,
            height: 1,
            freshness: 1,
            reserved: 0,
            color: amaterasu_media::FrameColorMetadata {
                version: 1,
                size: std::mem::size_of::<amaterasu_media::FrameColorMetadata>() as u32,
                transfer: amaterasu_media::FRAME_TRANSFER_PQ,
                ..Default::default()
            },
            clock: amaterasu_media::FrameClockMetadata {
                version: 1,
                size: std::mem::size_of::<amaterasu_media::FrameClockMetadata>() as u32,
                domain: amaterasu_media::FRAME_CLOCK_HOST_MONOTONIC,
                epoch_id: [1; 16],
                ..Default::default()
            },
            pacing_clock: amaterasu_media::FrameClockMetadata {
                version: 1,
                size: std::mem::size_of::<amaterasu_media::FrameClockMetadata>() as u32,
                domain: amaterasu_media::FRAME_CLOCK_HOST_MONOTONIC,
                epoch_id: [42; 16],
                ..Default::default()
            },
            resource: offer.resource,
            acquire_sync: offer.acquire_sync,
            context: ptr::null_mut(),
            retain,
            release,
        };
        let mut delivery = Delivery {
            consumer: &api,
            stream: 3,
            generation: 9,
            offer: 5,
            flags: 0,
            expected_epoch: [1; 16],
            last_sequence: 0,
            accepted_sequence: None,
            calls: 0,
            result: STATUS_OK,
            invalid: false,
            processor: None,
            output_delivered: false,
        };
        assert_eq!(
            unsafe { deliver((&mut delivery as *mut Delivery<'_>).cast(), &frame) },
            STATUS_OK
        );
        assert_eq!(consumer.configured, 1);
        assert_eq!(consumer.submitted, 1);
        assert_eq!(consumer.transfer, amaterasu_media::FRAME_TRANSFER_PQ);
        assert_eq!(consumer.epoch, [42; 16]);
        assert_eq!(
            unsafe { deliver((&mut delivery as *mut Delivery<'_>).cast(), &frame) },
            STATUS_INVALID_ARGUMENT
        );
        assert!(delivery.invalid);
    }
    #[test]
    fn rejects_duplicate_offers_and_incompatible_consumer() {
        let mut consumer = Consumer {
            reject: true,
            ..Consumer::default()
        };
        let api = table(&mut consumer);
        let mut state = Configure {
            ids: Ids::new(0),
            consumer_ids: &[8],
            consumer: &api,
            stream: 3,
            generation: 4,
            producer_generation: 9,
            required: 0,
            selected: None,
            failure: None,
        };
        let offer = offer();
        // SAFETY: fixture objects outlive each synchronous callback.
        assert_eq!(
            unsafe { configure((&mut state as *mut Configure<'_>).cast(), &offer) },
            STATUS_OK
        );
        assert!(state.selected.is_none());
        assert_eq!(
            unsafe { configure((&mut state as *mut Configure<'_>).cast(), &offer) },
            STATUS_INVALID_ARGUMENT
        );
        assert!(state.ids.invalid);
    }
    #[test]
    fn gpu_requirement_rejects_unspecified_and_cpu_access() {
        let mut ids = Ids::new(FRAME_ACCESS_GPU);
        let mut candidate = offer();
        candidate.accessibility = 0;
        assert!(ids.add(&candidate));
        candidate.id += 1;
        candidate.accessibility = FRAME_ACCESS_CPU;
        assert!(ids.add(&candidate));
        assert!(ids.eligible.is_empty());
        candidate.id += 1;
        candidate.accessibility = FRAME_ACCESS_GPU;
        assert!(ids.add(&candidate));
        assert_eq!(ids.eligible, vec![candidate.id]);
        candidate.id += 1;
        candidate.accessibility = 0x8000_0000;
        assert!(!ids.add(&candidate));
    }
    #[test]
    fn null_offer_poisoning_survives_a_swallowed_callback_error() {
        let mut ids = Ids::new(0);
        // SAFETY: the callback context remains live.
        assert_eq!(
            unsafe { collect((&mut ids as *mut Ids).cast(), ptr::null()) },
            STATUS_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { collect((&mut ids as *mut Ids).cast(), &offer()) },
            STATUS_OK
        );
        assert!(ids.invalid);
        let mut consumer = Consumer::default();
        let api = table(&mut consumer);
        let mut state = Configure {
            ids: Ids::new(0),
            consumer_ids: &[8],
            consumer: &api,
            stream: 3,
            generation: 4,
            producer_generation: 9,
            required: 0,
            selected: None,
            failure: None,
        };
        assert_eq!(
            unsafe { configure((&mut state as *mut Configure<'_>).cast(), ptr::null()) },
            STATUS_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { configure((&mut state as *mut Configure<'_>).cast(), &offer()) },
            STATUS_OK
        );
        assert!(state.ids.invalid);
    }

    #[derive(Default)]
    struct Producer {
        selected: bool,
        stale: bool,
        selection_fails: bool,
        enumerate_calls: u32,
        change_generation: bool,
        wrong_frame: bool,
        no_frame_busy: bool,
        frame_sequence: u64,
        wrong_epoch: bool,
    }
    unsafe extern "C" fn producer_enumerate(
        context: *mut c_void,
        _: u64,
        sink: CaptureResourceOfferSink,
        generation: *mut u64,
    ) -> Status {
        // SAFETY: the fixture owns the context and output pointer.
        let producer = unsafe { &mut *context.cast::<Producer>() };
        producer.enumerate_calls += 1;
        let mut candidate = offer();
        candidate.accessibility = FRAME_ACCESS_GPU;
        let status = unsafe { (sink.write)(sink.context, &candidate) };
        if status != STATUS_OK {
            return status;
        }
        unsafe {
            *generation =
                if producer.change_generation && producer.enumerate_calls.is_multiple_of(2) {
                    10
                } else {
                    9
                };
        }
        STATUS_OK
    }
    unsafe extern "C" fn producer_select(
        context: *mut c_void,
        _: u64,
        generation: u64,
        id: u64,
    ) -> Status {
        let producer = unsafe { &mut *context.cast::<Producer>() };
        if producer.selection_fails || generation != 9 || id != 5 {
            return MEDIA_UNSUPPORTED;
        }
        producer.selected = true;
        STATUS_OK
    }
    unsafe extern "C" fn producer_read(
        context: *mut c_void,
        _: u64,
        sink: CaptureResourceFrameSink,
    ) -> Status {
        let producer = unsafe { &mut *context.cast::<Producer>() };
        if producer.stale || !producer.selected {
            return MEDIA_UNSUPPORTED;
        }
        if producer.no_frame_busy {
            return amaterasu_media::MEDIA_BUSY;
        }
        producer.frame_sequence += 1;
        let descriptor = descriptor(FRAME_RESOURCE_VENDOR_BASE);
        let frame = CaptureResourceFrame {
            offer_generation: if producer.wrong_frame { 10 } else { 9 },
            offer_id: 5,
            timestamp_us: 1,
            capture_pacing_timestamp_us: 1,
            sequence: producer.frame_sequence,
            width: 1,
            height: 1,
            freshness: 1,
            reserved: 0,
            color: Default::default(),
            clock: amaterasu_media::FrameClockMetadata {
                version: 1,
                size: std::mem::size_of::<amaterasu_media::FrameClockMetadata>() as u32,
                domain: amaterasu_media::FRAME_CLOCK_HOST_MONOTONIC,
                epoch_id: if producer.wrong_epoch {
                    [2; 16]
                } else {
                    [1; 16]
                },
                ..Default::default()
            },
            pacing_clock: Default::default(),
            resource: descriptor,
            acquire_sync: self::descriptor(FRAME_SYNC_NONE),
            context: ptr::null_mut(),
            retain,
            release,
        };
        unsafe { (sink.write)(sink.context, &frame) }
    }
    unsafe extern "C" fn consumer_enumerate(
        _: *mut c_void,
        _: u64,
        sink: CaptureResourceOfferSink,
        generation: *mut u64,
    ) -> Status {
        let mut candidate = offer();
        candidate.id = 8;
        candidate.accessibility = FRAME_ACCESS_GPU;
        let status = unsafe { (sink.write)(sink.context, &candidate) };
        unsafe {
            *generation = 4;
        }
        status
    }
    unsafe extern "C" fn unused_frame(
        _: *mut c_void,
        _: u64,
        _: amaterasu_media::FrameSink,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_geometry(
        _: *mut c_void,
        _: u64,
        _: *mut amaterasu_media::CaptureGeometry,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_fresh(
        _: *mut c_void,
        _: u64,
        _: amaterasu_media::FreshFrameSinkV1_2,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_timestamp(
        _: *mut c_void,
        _: u64,
        _: amaterasu_media::FreshFrameSinkV1_3,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_dmabuf(
        _: *mut c_void,
        _: u64,
        _: amaterasu_media::DmaBufFrameSink,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_presentation(
        _: *mut c_void,
        _: u64,
        _: *mut amaterasu_media::PresentationStatus,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_lease(
        _: *mut c_void,
        _: u64,
        _: amaterasu_media::DmaBufLeaseSink,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_subscribe(
        _: *mut c_void,
        _: u64,
        _: amaterasu_media::CaptureEventSink,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_unsubscribe(_: *mut c_void, _: u64) {}
    unsafe extern "C" fn unused_swap(
        _: *mut c_void,
        _: u64,
        _: u64,
        _: u32,
        _: amaterasu_module_sdk::abi::BorrowedBytes,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    fn capture_table(producer: &mut Producer) -> CaptureApiV1_8 {
        CaptureApiV1_8 {
            base: amaterasu_media::CaptureApiV1_7 {
                base: amaterasu_media::CaptureApiV1_6 {
                    base: NativeInterface {
                        header: AbiHeader::of::<CaptureApiV1_8>(),
                        context: (producer as *mut Producer).cast(),
                        release: None,
                    },
                    read_frame: unused_frame,
                    geometry: unused_geometry,
                    read_frame_with_freshness: unused_fresh,
                    read_frame_with_capture_timestamp: unused_timestamp,
                    read_dmabuf_frame: unused_dmabuf,
                    latest_presentation: unused_presentation,
                    read_dmabuf_frame_leased: unused_lease,
                },
                subscribe_events: unused_subscribe,
                unsubscribe_events: unused_unsubscribe,
                resolve_swap: unused_swap,
            },
            enumerate_resources: producer_enumerate,
            select_resource: producer_select,
            read_resource_frame: producer_read,
        }
    }
    #[test]
    fn connection_forwards_then_stale_cancels_once_without_reactivation() {
        let mut producer = Producer::default();
        let capture = capture_table(&mut producer);
        let mut consumer = Consumer::default();
        let mut api = table(&mut consumer);
        api.enumerate_resources = consumer_enumerate;
        // SAFETY: fixture tables and contexts outlive the connection.
        let mut connection = ResourceBridge::new(1, 3, FRAME_ACCESS_GPU, [1; 16]).unwrap();
        assert_eq!(consumer.cancelled, 0);
        assert_eq!(
            unsafe { connection.forward(&capture, &api, 0) },
            Ok(STATUS_OK)
        );
        assert_eq!(consumer.submitted, 1);
        assert_eq!(consumer.cancelled, 0);
        producer.stale = true;
        std::hint::black_box(&producer);
        assert_eq!(
            unsafe { connection.forward(&capture, &api, 0) },
            Err(ConnectionError::Stale)
        );
        assert_eq!(consumer.cancelled, 1);
        assert_eq!(
            unsafe { connection.forward(&capture, &api, 0) },
            Err(ConnectionError::Stale)
        );
        // SAFETY: both fixture bindings remain live.
        unsafe { connection.cancel(&api) };
        assert_eq!(consumer.cancelled, 1);
    }
    #[test]
    fn failed_selection_and_changed_generation_cancel_configured_consumer_once() {
        for generation_changes in [false, true] {
            let mut producer = Producer {
                selection_fails: !generation_changes,
                change_generation: generation_changes,
                ..Producer::default()
            };
            let capture = capture_table(&mut producer);
            let mut consumer = Consumer::default();
            let mut api = table(&mut consumer);
            api.enumerate_resources = consumer_enumerate;
            // SAFETY: fixture tables and contexts outlive the connection.
            let mut connection = ResourceBridge::new(1, 3, FRAME_ACCESS_GPU, [1; 16]).unwrap();
            assert_eq!(
                unsafe { connection.negotiate(&capture, &api) },
                Err(ConnectionError::Stale)
            );
            assert_eq!(consumer.configured, 1);
            assert_eq!(consumer.cancelled, 1);
            // SAFETY: both fixture bindings remain live.
            unsafe { connection.cancel(&api) };
            assert_eq!(consumer.cancelled, 1);
        }
        let mut producer = Producer::default();
        let capture = capture_table(&mut producer);
        let mut consumer = Consumer {
            reject: true,
            ..Consumer::default()
        };
        let mut api = table(&mut consumer);
        api.enumerate_resources = consumer_enumerate;
        let mut connection = ResourceBridge::new(1, 3, FRAME_ACCESS_GPU, [1; 16]).unwrap();
        assert_eq!(
            unsafe { connection.negotiate(&capture, &api) },
            Err(ConnectionError::NoCompatibleOffer)
        );
        // SAFETY: both fixture bindings remain live.
        unsafe { connection.cancel(&api) };
        assert_eq!(consumer.cancelled, 0);
    }
    #[test]
    fn malformed_known_metadata_is_rejected_and_unknown_versions_remain_opaque() {
        let mut color = amaterasu_media::FrameColorMetadata {
            version: 1,
            size: 1,
            ..Default::default()
        };
        assert!(!valid_color_metadata(&color));
        color.version = 42;
        assert!(valid_color_metadata(&color));
        let mut clock = amaterasu_media::FrameClockMetadata {
            version: 0,
            epoch_id: [1; 16],
            ..Default::default()
        };
        assert!(!valid_clock_metadata(&clock));
        clock.version = 1;
        clock.size = std::mem::size_of::<amaterasu_media::FrameClockMetadata>() as u32;
        clock.reserved = 1;
        assert!(!valid_clock_metadata(&clock));
        clock.version = 42;
        assert!(valid_clock_metadata(&clock));
    }
    #[test]
    fn opaque_namespace_and_stream_generations_are_consumer_decisions() {
        let mut producer = Producer::default();
        let capture = capture_table(&mut producer);
        let mut consumer = Consumer {
            namespace: Some([1; 16]),
            ..Consumer::default()
        };
        let mut api = table(&mut consumer);
        api.enumerate_resources = consumer_enumerate;
        let mut bridge = ResourceBridge::new(91, 37, FRAME_ACCESS_GPU, [1; 16]).unwrap();
        // SAFETY: fixture bindings and stream contexts live throughout the operation.
        assert_eq!(
            unsafe { bridge.negotiate(&capture, &api) },
            Err(ConnectionError::NoCompatibleOffer)
        );
        assert!(!producer.selected);
        assert_eq!(
            (
                consumer.seen_stream,
                consumer.seen_generation,
                consumer.seen_producer_generation
            ),
            (37, 4, 9)
        );
        consumer.namespace = Some([0; 16]);
        assert_eq!(unsafe { bridge.negotiate(&capture, &api) }, Ok(()));
        assert!(producer.selected);
        unsafe { bridge.cancel(&api) };
        assert_eq!(consumer.cancelled, 1);
    }

    #[test]
    fn busy_does_not_submit_and_wrong_frame_retires_the_consumer() {
        let mut producer = Producer::default();
        let capture = capture_table(&mut producer);
        let mut consumer = Consumer {
            busy: true,
            ..Consumer::default()
        };
        let mut api = table(&mut consumer);
        api.enumerate_resources = consumer_enumerate;
        let mut bridge = ResourceBridge::new(1, 3, FRAME_ACCESS_GPU, [1; 16]).unwrap();
        // SAFETY: fixture bindings and stream contexts live throughout these calls.
        assert_eq!(
            unsafe { bridge.forward(&capture, &api, 0) },
            Ok(amaterasu_media::MEDIA_BUSY)
        );
        assert_eq!(consumer.submitted, 0);
        producer.no_frame_busy = true;
        std::hint::black_box(&producer);
        assert_eq!(
            unsafe { bridge.forward(&capture, &api, 0) },
            Ok(amaterasu_media::MEDIA_BUSY)
        );
        producer.no_frame_busy = false;
        producer.wrong_frame = true;
        std::hint::black_box(&producer);
        assert_eq!(
            unsafe { bridge.forward(&capture, &api, 0) },
            Err(ConnectionError::Protocol)
        );
        assert_eq!(consumer.submitted, 0);
        assert_eq!(consumer.cancelled, 1);
        unsafe { bridge.cancel(&api) };
        assert_eq!(consumer.cancelled, 1);
    }
    #[test]
    fn equal_offer_ids_on_distinct_streams_do_not_share_bridge_state() {
        let mut producer_a = Producer::default();
        let mut producer_b = Producer::default();
        let capture_a = capture_table(&mut producer_a);
        let capture_b = capture_table(&mut producer_b);
        let mut consumer_a = Consumer::default();
        let mut consumer_b = Consumer::default();
        let mut api_a = table(&mut consumer_a);
        let mut api_b = table(&mut consumer_b);
        api_a.enumerate_resources = consumer_enumerate;
        api_b.enumerate_resources = consumer_enumerate;
        let mut bridge_a = ResourceBridge::new(1, 31, FRAME_ACCESS_GPU, [1; 16]).unwrap();
        let mut bridge_b = ResourceBridge::new(2, 32, FRAME_ACCESS_GPU, [1; 16]).unwrap();
        // SAFETY: each independently bound fixture and stream stays live through cancellation.
        assert_eq!(
            unsafe { bridge_a.forward(&capture_a, &api_a, 0) },
            Ok(STATUS_OK)
        );
        assert_eq!(
            unsafe { bridge_b.forward(&capture_b, &api_b, 0) },
            Ok(STATUS_OK)
        );
        assert_eq!((consumer_a.seen_stream, consumer_b.seen_stream), (31, 32));
        unsafe { bridge_a.cancel(&api_a) };
        assert_eq!((consumer_a.cancelled, consumer_b.cancelled), (1, 0));
        assert_eq!(
            unsafe { bridge_b.forward(&capture_b, &api_b, 0) },
            Ok(STATUS_OK)
        );
        unsafe { bridge_b.cancel(&api_b) };
        assert_eq!((consumer_a.submitted, consumer_b.submitted), (1, 2));
    }
    #[test]
    fn epoch_and_successful_sequence_are_enforced_before_submission() {
        assert!(ResourceBridge::new(1, 2, 0, [0; 16]).is_err());
        let mut producer = Producer::default();
        let capture = capture_table(&mut producer);
        let mut consumer = Consumer {
            busy: true,
            ..Consumer::default()
        };
        let mut api = table(&mut consumer);
        api.enumerate_resources = consumer_enumerate;
        let mut bridge = ResourceBridge::new(1, 2, FRAME_ACCESS_GPU, [1; 16]).unwrap();
        // SAFETY: fixture bindings and contexts remain live until cancellation.
        assert_eq!(
            unsafe { bridge.forward(&capture, &api, 0) },
            Ok(amaterasu_media::MEDIA_BUSY)
        );
        producer.frame_sequence = 0;
        consumer.busy = false;
        std::hint::black_box((&producer, &consumer));
        assert_eq!(unsafe { bridge.forward(&capture, &api, 0) }, Ok(STATUS_OK));
        producer.frame_sequence = 0;
        std::hint::black_box(&producer);
        assert_eq!(
            unsafe { bridge.forward(&capture, &api, 0) },
            Err(ConnectionError::Protocol)
        );
        assert_eq!(consumer.submitted, 1);
        assert_eq!(consumer.cancelled, 1);

        let mut producer = Producer {
            wrong_epoch: true,
            ..Producer::default()
        };
        let capture = capture_table(&mut producer);
        let mut consumer = Consumer::default();
        let mut api = table(&mut consumer);
        api.enumerate_resources = consumer_enumerate;
        let mut bridge = ResourceBridge::new(1, 2, FRAME_ACCESS_GPU, [1; 16]).unwrap();
        assert_eq!(
            unsafe { bridge.forward(&capture, &api, 0) },
            Err(ConnectionError::Protocol)
        );
        assert_eq!((consumer.submitted, consumer.cancelled), (0, 1));
    }

    #[derive(Default)]
    struct TestProcessor {
        input: Consumer,
        selected: bool,
        transfer: u32,
        drop_output: bool,
        swallow_error: bool,
        corrupt_clock: bool,
        lease: Option<*mut TestLease>,
        cancellations: u32,
        log: Option<std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>>,
    }

    unsafe extern "C" fn processor_config(
        context: *mut c_void,
        stream: u64,
        generation: u64,
        id: u64,
        source_generation: u64,
        offer: *const CaptureResourceOffer,
    ) -> Status {
        // SAFETY: test context points to a live processor for this call.
        let processor = unsafe { &mut *context.cast::<TestProcessor>() };
        unsafe {
            config(
                (&mut processor.input as *mut Consumer).cast(),
                stream,
                generation,
                id,
                source_generation,
                offer,
            )
        }
    }
    unsafe extern "C" fn processor_cancel(context: *mut c_void, _: u64) {
        let processor = unsafe { &mut *context.cast::<TestProcessor>() };
        processor.cancellations += 1;
        if let Some(log) = &processor.log {
            log.borrow_mut().push("processor");
        }
    }
    unsafe extern "C" fn processor_outputs(
        _: *mut c_void,
        _: u64,
        sink: CaptureResourceOfferSink,
        generation: *mut u64,
    ) -> Status {
        let mut candidate = offer();
        candidate.accessibility = FRAME_ACCESS_GPU;
        unsafe {
            *generation = 9;
            (sink.write)(sink.context, &candidate)
        }
    }
    unsafe extern "C" fn processor_select(
        context: *mut c_void,
        _: u64,
        generation: u64,
        offer: u64,
    ) -> Status {
        let processor = unsafe { &mut *context.cast::<TestProcessor>() };
        if generation != 9 || offer != 5 {
            return MEDIA_UNSUPPORTED;
        }
        processor.selected = true;
        STATUS_OK
    }
    unsafe extern "C" fn processor_process(
        context: *mut c_void,
        _: u64,
        input: *const CaptureResourceFrame,
        sink: CaptureResourceFrameSink,
    ) -> Status {
        let processor = unsafe { &mut *context.cast::<TestProcessor>() };
        if !processor.selected {
            return MEDIA_UNSUPPORTED;
        }
        if processor.drop_output {
            return STATUS_OK;
        }
        let mut output = unsafe { *input };
        output.offer_generation = 9;
        output.offer_id = 5;
        output.color = amaterasu_media::FrameColorMetadata {
            version: 1,
            size: std::mem::size_of::<amaterasu_media::FrameColorMetadata>() as u32,
            transfer: processor.transfer,
            ..Default::default()
        };
        if processor.corrupt_clock {
            output.clock.epoch_id = [7; 16];
        }
        if let Some(lease) = processor.lease {
            output.context = lease.cast();
            output.retain = lease_retain;
            output.release = lease_release;
        }
        let status = unsafe { (sink.write)(sink.context, &output) };
        if processor.swallow_error {
            STATUS_OK
        } else {
            status
        }
    }
    fn processor_table(processor: &mut TestProcessor) -> FrameResourceProcessorApi {
        FrameResourceProcessorApi {
            base: FrameResourceConsumerApi {
                base: NativeInterface {
                    header: AbiHeader {
                        size: size_of::<FrameResourceProcessorApi>() as u32,
                        major: 1,
                        minor: 1,
                    },
                    context: (processor as *mut TestProcessor).cast(),
                    release: None,
                },
                enumerate_resources: consumer_enumerate,
                configure: processor_config,
                submit,
                cancel: processor_cancel,
            },
            enumerate_outputs: processor_outputs,
            select_output: processor_select,
            process: processor_process,
        }
    }

    #[test]
    fn two_processors_negotiate_and_forward_with_retry_and_ordered_retirement() {
        for transfer in [
            amaterasu_media::FRAME_TRANSFER_PQ,
            amaterasu_media::FRAME_TRANSFER_SRGB,
        ] {
            let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let mut source = Producer::default();
            let capture = capture_table(&mut source);
            let mut processor = TestProcessor {
                transfer,
                log: Some(log.clone()),
                ..Default::default()
            };
            let processor_api = processor_table(&mut processor);
            let mut encoder = Consumer {
                busy: true,
                cancel_log: Some(log.clone()),
                ..Default::default()
            };
            let mut encoder_api = table(&mut encoder);
            encoder_api.enumerate_resources = consumer_enumerate;
            let mut bridge = ProcessorBridge::new(1, 2, 3, [1; 16]).unwrap();
            let force = amaterasu_media::FRAME_RESOURCE_SUBMIT_FORCE_IDR;
            assert_eq!(
                unsafe { bridge.forward(&capture, &processor_api, &encoder_api, force) },
                Ok(amaterasu_media::MEDIA_BUSY)
            );
            assert_eq!(encoder.submitted, 0);
            encoder.busy = false;
            std::hint::black_box(&encoder);
            assert_eq!(
                unsafe { bridge.forward(&capture, &processor_api, &encoder_api, force) },
                Ok(STATUS_OK)
            );
            assert_eq!(
                (encoder.submitted, encoder.transfer, encoder.last_flags),
                (1, transfer, force)
            );
            assert_eq!(processor.input.configured, 1);
            assert_eq!(encoder.configured, 1);
            unsafe { bridge.cancel(&processor_api, &encoder_api) };
            assert_eq!(&*log.borrow(), &["encoder", "processor"]);
            unsafe { bridge.cancel(&processor_api, &encoder_api) };
            assert_eq!(&*log.borrow(), &["encoder", "processor"]);
        }
    }

    #[test]
    fn processor_rejection_violation_and_bad_clock_fail_closed() {
        for bad_clock in [false, true] {
            let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let mut source = Producer::default();
            let capture = capture_table(&mut source);
            let mut processor = TestProcessor {
                swallow_error: !bad_clock,
                corrupt_clock: bad_clock,
                log: Some(log.clone()),
                ..Default::default()
            };
            let processor_api = processor_table(&mut processor);
            let mut encoder = Consumer {
                busy: true,
                cancel_log: Some(log.clone()),
                ..Default::default()
            };
            let mut encoder_api = table(&mut encoder);
            encoder_api.enumerate_resources = consumer_enumerate;
            let mut bridge = ProcessorBridge::new(1, 2, 3, [1; 16]).unwrap();
            assert_eq!(
                unsafe { bridge.forward(&capture, &processor_api, &encoder_api, 0) },
                Err(ConnectionError::Protocol)
            );
            assert_eq!(encoder.submitted, 0);
            assert_eq!(&*log.borrow(), &["encoder", "processor"]);
            assert!(bridge.is_retired());
        }
    }

    #[test]
    fn processor_drop_preserves_keyframe_request_until_output() {
        let mut source = Producer::default();
        let capture = capture_table(&mut source);
        let mut processor = TestProcessor {
            drop_output: true,
            ..Default::default()
        };
        let processor_api = processor_table(&mut processor);
        let mut encoder = Consumer::default();
        let mut encoder_api = table(&mut encoder);
        encoder_api.enumerate_resources = consumer_enumerate;
        let mut bridge = ProcessorBridge::new(1, 2, 3, [1; 16]).unwrap();
        let force = amaterasu_media::FRAME_RESOURCE_SUBMIT_FORCE_IDR;
        assert_eq!(
            unsafe { bridge.forward(&capture, &processor_api, &encoder_api, force) },
            Ok(STATUS_OK)
        );
        processor.drop_output = false;
        std::hint::black_box(&processor);
        assert_eq!(
            unsafe { bridge.forward(&capture, &processor_api, &encoder_api, 0) },
            Ok(STATUS_OK)
        );
        assert_eq!(encoder.last_flags, force);
        unsafe { bridge.cancel(&processor_api, &encoder_api) };
    }

    struct PipelineHost {
        capture: CaptureApiV1_8,
        encoder: amaterasu_media::EncoderApi,
        consumer: FrameResourceConsumerApi,
        processor: *const FrameResourceProcessorApi,
    }
    unsafe extern "C" fn runtime_invoke(
        context: *mut c_void,
        bytes: amaterasu_module_sdk::abi::BorrowedBytes,
        sink: amaterasu_module_sdk::abi::ReplySink,
    ) -> Status {
        use amaterasu_module_sdk::{Command, Response};
        let host = unsafe { &mut *context.cast::<PipelineHost>() };
        let bytes = match unsafe { bytes.read() } {
            Ok(bytes) => bytes,
            Err(_) => return STATUS_INVALID_ARGUMENT,
        };
        let command: Command = match serde_json::from_slice(bytes) {
            Ok(command) => command,
            Err(_) => return STATUS_INVALID_ARGUMENT,
        };
        let response = match command {
            Command::NativeBindTyped {
                service, contract, ..
            } => {
                let address = match (service.as_str(), contract.as_str()) {
                    ("example.capture/1", "amaterasu.video-capture.native/1") => {
                        &host.capture as *const _ as usize
                    }
                    (
                        "amaterasu.encode/1" | "example.encode/1",
                        "amaterasu.video-encoder.native/1",
                    ) => &host.encoder as *const _ as usize,
                    (
                        "example.resource-consumer/1",
                        "amaterasu.frame-resource-consumer.native/1",
                    ) => &host.consumer as *const _ as usize,
                    ("example.processor/1", "amaterasu.frame-resource-processor.native/1") => {
                        host.processor as usize
                    }
                    _ => 0,
                };
                if address == 0 {
                    Response::error("unexpected_binding", "unknown service")
                } else {
                    Response::NativeBound {
                        binding: address as u64,
                        address,
                    }
                }
            }
            Command::NativeRelease { .. } => Response::Ok,
            _ => Response::error("unexpected_request", "test only"),
        };
        unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
        STATUS_OK
    }
    unsafe extern "C" fn legacy_submit(
        _: *mut c_void,
        _: u64,
        _: *const amaterasu_media::CpuFrame,
        _: u32,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn legacy_receive(
        _: *mut c_void,
        _: u64,
        _: amaterasu_media::PacketSink,
    ) -> Status {
        amaterasu_media::MEDIA_PENDING
    }
    unsafe extern "C" fn legacy_drain(_: *mut c_void, _: u64) -> Status {
        STATUS_OK
    }

    fn run_video_plane_with_processor(processor: *const FrameResourceProcessorApi, transfer: u32) {
        use crate::plane::{FramePath, NativePlane, Plane, Pump};
        use amaterasu_module_sdk::RuntimeClient;
        use std::sync::{Arc, Mutex};
        let mut source = Box::new(Producer::default());
        let mut consumer = Box::new(Consumer::default());
        let mut capture = capture_table(&mut source);
        capture.base.base.base.header = AbiHeader {
            size: size_of::<CaptureApiV1_8>() as u32,
            major: 1,
            minor: 8,
        };
        let mut consumer_api = table(&mut consumer);
        consumer_api.enumerate_resources = consumer_enumerate;
        let mut host = Box::new(PipelineHost {
            capture,
            encoder: amaterasu_media::EncoderApi {
                base: NativeInterface {
                    header: AbiHeader::of::<amaterasu_media::EncoderApi>(),
                    context: ptr::null_mut(),
                    release: None,
                },
                submit: legacy_submit,
                receive: legacy_receive,
                drain: legacy_drain,
            },
            consumer: consumer_api,
            processor,
        });
        let runtime = unsafe {
            RuntimeClient::from_endpoint(amaterasu_module_sdk::abi::HostApi {
                header: AbiHeader::of::<amaterasu_module_sdk::abi::HostApi>(),
                context: (&mut *host as *mut PipelineHost).cast(),
                invoke: runtime_invoke,
            })
        };
        let settings: crate::model::Settings = serde_json::from_value(serde_json::json!({
            "capture_providers": [],
            "external_capture_providers": {"vendor": {"service":"example.capture/1", "frame_transport":"generic_cpu"}},
            "external_encoder_providers": {"vendor":"example.encode/1"},
            "external_encoder_consumers": {"vendor":"example.resource-consumer/1"},
            "generic_processor_service":"example.processor/1"
        })).unwrap();
        settings.validate().unwrap();
        let providers = settings.resolved_providers();
        let provider = *providers.first().unwrap();
        let mut plane = NativePlane::bind(
            &runtime,
            Arc::new(Mutex::new(crate::queue::Queues::default())),
            &providers,
            &settings.external_encoder_providers,
            &settings.source_encoder_providers,
            &settings.external_encoder_consumers,
            None,
            None,
            settings.generic_processor_service.as_deref(),
        )
        .unwrap();
        plane.select_encoder("example.encode/1");
        plane.set_stream_clock(1, 0, [1; 16]);
        let configuration: crate::model::Configuration = serde_json::from_value(
            serde_json::json!({"width":16,"height":16,"fps":60,"bitrate":100000}),
        )
        .unwrap();
        let progress = plane
            .pump(Pump {
                video: 1,
                provider,
                frame_path: FramePath::Generic,
                capture: 1,
                encoder: 3,
                configuration: &configuration,
                capture_due: true,
                constant_rate: false,
                repeat_due: false,
                repeat_frame: None,
                timestamp_us: 1,
                sequence: 1,
                force_idr: false,
            })
            .unwrap();
        assert!(progress.submitted);
        assert_eq!((consumer.submitted, consumer.transfer), (1, transfer));
        plane.finish_stream(1);
        assert_eq!(consumer.cancelled, 1);
        drop(plane);
    }

    #[test]
    fn selected_processor_runs_in_actual_video_plane_with_two_replacements() {
        for transfer in [
            amaterasu_media::FRAME_TRANSFER_PQ,
            amaterasu_media::FRAME_TRANSFER_SRGB,
        ] {
            let mut processor = Box::new(TestProcessor {
                transfer,
                ..Default::default()
            });
            let api = processor_table(&mut processor);
            run_video_plane_with_processor(&api, transfer);
            assert_eq!(processor.cancellations, 1);
        }
    }

    #[test]
    #[ignore = "requires separately built processor provider artifacts"]
    fn unchanged_video_plane_loads_two_processor_provider_artifacts() {
        for (variable, transfer) in [
            ("PROCESSOR_FIXTURE_PQ", amaterasu_media::FRAME_TRANSFER_PQ),
            (
                "PROCESSOR_FIXTURE_SRGB",
                amaterasu_media::FRAME_TRANSFER_SRGB,
            ),
        ] {
            let path = std::env::var(variable).expect("processor fixture path");
            // SAFETY: test libraries expose these C signatures; they remain loaded
            // through the complete plane lifetime and provider destruction.
            unsafe {
                let library = libloading::Library::new(path).unwrap();
                let create: libloading::Symbol<
                    unsafe extern "C" fn() -> *mut FrameResourceProcessorApi,
                > = library.get(b"fixture_processor_create\0").unwrap();
                let destroy: libloading::Symbol<
                    unsafe extern "C" fn(*mut FrameResourceProcessorApi),
                > = library.get(b"fixture_processor_destroy\0").unwrap();
                let cancellations: libloading::Symbol<
                    unsafe extern "C" fn(*mut FrameResourceProcessorApi) -> u32,
                > = library.get(b"fixture_processor_cancellations\0").unwrap();
                let processor = create();
                assert!(!processor.is_null());
                run_video_plane_with_processor(processor, transfer);
                assert_eq!(cancellations(processor), 1);
                destroy(processor);
            }
        }
    }

    #[derive(Default)]
    struct TestLease {
        references: u32,
        completion: u32,
    }
    unsafe extern "C" fn lease_retain(context: *mut c_void) -> Status {
        let lease = unsafe { &mut *context.cast::<TestLease>() };
        lease.references += 1;
        STATUS_OK
    }
    unsafe extern "C" fn lease_release(context: *mut c_void, completion: *const FrameDescriptor) {
        let lease = unsafe { &mut *context.cast::<TestLease>() };
        lease.references -= 1;
        lease.completion = unsafe { (*completion).kind };
    }
    #[test]
    fn cancellation_releases_retained_processed_frame_with_completion_before_processor() {
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut source = Producer::default();
        let capture = capture_table(&mut source);
        let mut lease = TestLease::default();
        let mut processor = TestProcessor {
            lease: Some(&mut lease),
            log: Some(log.clone()),
            ..Default::default()
        };
        let processor_api = processor_table(&mut processor);
        let mut encoder = Consumer {
            hold: true,
            cancel_log: Some(log.clone()),
            ..Default::default()
        };
        let mut encoder_api = table(&mut encoder);
        encoder_api.enumerate_resources = consumer_enumerate;
        let mut bridge = ProcessorBridge::new(1, 2, 3, [1; 16]).unwrap();
        assert_eq!(
            unsafe { bridge.forward(&capture, &processor_api, &encoder_api, 0) },
            Ok(STATUS_OK)
        );
        assert_eq!(lease.references, 1);
        unsafe { bridge.cancel(&processor_api, &encoder_api) };
        assert_eq!(lease.references, 0);
        assert_eq!(lease.completion, amaterasu_media::FRAME_SYNC_VENDOR_BASE);
        assert_eq!(&*log.borrow(), &["encoder", "processor"]);
    }
}
