//! Synchronous, schema-agnostic capture-to-consumer forwarding.
use amaterasu_abi::{STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use amaterasu_media::{
    CaptureApiV1_8, CaptureResourceFrame, CaptureResourceFrameSink, CaptureResourceOffer,
    CaptureResourceOfferSink, FRAME_ACCESS_CPU, FRAME_ACCESS_GPU, FrameResourceConsumerApi,
    MEDIA_BUSY, MEDIA_PENDING, MEDIA_UNSUPPORTED,
};
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
    uncertain: bool,
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
    if let Some(status) = state.failure {
        return status;
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
            // Only UNSUPPORTED is specified to leave the previous
            // configuration unchanged. Other errors may have effects.
            state.uncertain = true;
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
    calls: u32,
    result: Status,
    invalid: bool,
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
        || !valid_clock_metadata(&frame.pacing_clock)
    {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the consumer binding and borrowed frame remain live during submission.
    state.result = unsafe {
        (state.consumer.submit)(
            state.consumer.base.context,
            state.stream,
            frame,
            state.flags,
        )
    };
    state.result
}

/// A connection pins both native tables by borrowing them, and cancels consumer
/// work before releasing those borrows. Keep the stream owners alive for its
/// entire lifetime. All calls and destruction occur on the owning module thread.
///
/// This coordinator forwards opaque resource descriptors without copying pixels;
/// the consumer is responsible for schema, device, synchronization and retention.
pub struct ResourceConnection<'a> {
    capture: &'a CaptureApiV1_8,
    consumer: &'a FrameResourceConsumerApi,
    capture_stream: u64,
    consumer_stream: u64,
    selected: Option<(u64, u64)>,
    active: bool,
    retired: bool,
    required: u32,
    thread: PhantomData<Rc<()>>,
}

impl<'a> ResourceConnection<'a> {
    /// # Safety
    /// Both tables must be validated native bindings and their providers, stream
    /// owners, callbacks and contexts must remain live until this object is dropped.
    /// The caller must not close either stream or release the bindings while this
    /// object exists, and must use the providers' owning module thread.
    pub unsafe fn new(
        capture: &'a CaptureApiV1_8,
        capture_stream: u64,
        consumer: &'a FrameResourceConsumerApi,
        consumer_stream: u64,
        required: u32,
    ) -> Result<Self, ConnectionError> {
        if required & !(FRAME_ACCESS_CPU | FRAME_ACCESS_GPU) != 0 {
            return Err(ConnectionError::Protocol);
        }
        Ok(Self {
            capture,
            consumer,
            capture_stream,
            consumer_stream,
            selected: None,
            active: false,
            retired: false,
            required,
            thread: PhantomData,
        })
    }

    fn cancel(&mut self) {
        if self.active {
            // SAFETY: the constructor guarantees the consumer remains live.
            unsafe { (self.consumer.cancel)(self.consumer.base.context, self.consumer_stream) };
            self.active = false;
            self.retired = true;
        }
        self.selected = None;
    }

    fn negotiation_error(&mut self, error: ConnectionError) -> ConnectionError {
        if !matches!(error, ConnectionError::Native(MEDIA_BUSY | MEDIA_PENDING)) {
            self.cancel();
        }
        error
    }

    fn enumerate_capture(&self, sink: CaptureResourceOfferSink) -> Result<u64, ConnectionError> {
        let mut generation = 0;
        // SAFETY: sink context is live and enumeration is synchronous.
        let status = unsafe {
            (self.capture.enumerate_resources)(
                self.capture.base.base.base.context,
                self.capture_stream,
                sink,
                &mut generation,
            )
        };
        if status != STATUS_OK {
            return Err(ConnectionError::Native(status));
        }
        if generation == 0 {
            return Err(ConnectionError::Protocol);
        }
        Ok(generation)
    }

    /// Select a compatible offer on a fresh consumer stream. Cancellation retires
    /// the stream; after a stale selection, reopen it and create a new connection.
    pub fn negotiate(&mut self) -> Result<(), ConnectionError> {
        if self.retired {
            return Err(ConnectionError::Stale);
        }
        if self.selected.is_some() {
            return Ok(());
        }
        {
            let mut consumer_ids = Ids::new(self.required);
            let mut consumer_generation = 0;
            let sink = CaptureResourceOfferSink {
                context: (&mut consumer_ids as *mut Ids).cast(),
                write: collect,
            };
            // SAFETY: enumeration and callbacks are synchronous.
            let status = unsafe {
                (self.consumer.enumerate_resources)(
                    self.consumer.base.context,
                    self.consumer_stream,
                    sink,
                    &mut consumer_generation,
                )
            };
            if consumer_ids.invalid {
                self.cancel();
                return Err(ConnectionError::Protocol);
            }
            if status != STATUS_OK {
                return Err(self.negotiation_error(ConnectionError::Native(status)));
            }
            if consumer_generation == 0 {
                self.cancel();
                return Err(ConnectionError::Protocol);
            }
            let mut first = Ids::new(0);
            let sink = CaptureResourceOfferSink {
                context: (&mut first as *mut Ids).cast(),
                write: collect,
            };
            let first_result = self.enumerate_capture(sink);
            if first.invalid {
                self.cancel();
                return Err(ConnectionError::Protocol);
            }
            let producer_generation = match first_result {
                Ok(generation) => generation,
                Err(error) => return Err(self.negotiation_error(error)),
            };
            let mut state = Configure {
                ids: Ids::new(0),
                consumer_ids: &consumer_ids.eligible,
                consumer: self.consumer,
                stream: self.consumer_stream,
                generation: consumer_generation,
                producer_generation,
                required: self.required,
                selected: None,
                failure: None,
                uncertain: false,
            };
            let sink = CaptureResourceOfferSink {
                context: (&mut state as *mut Configure<'_>).cast(),
                write: configure,
            };
            let second = self.enumerate_capture(sink);
            self.active |= state.selected.is_some() || state.uncertain;
            if state.ids.invalid {
                self.cancel();
                return Err(ConnectionError::Protocol);
            }
            if let Some(status) = state.failure {
                self.cancel();
                return Err(ConnectionError::Native(status));
            }
            let second_generation = match second {
                Ok(generation) => generation,
                Err(error) => {
                    return Err(self.negotiation_error(error));
                }
            };
            first.ids.sort_unstable();
            state.ids.ids.sort_unstable();
            if producer_generation != second_generation || first.ids != state.ids.ids {
                self.cancel();
                return Err(ConnectionError::Stale);
            }
            let Some(offer) = state.selected else {
                self.cancel();
                return Err(ConnectionError::NoCompatibleOffer);
            };
            // SAFETY: both tables and stream owners remain live for this operation.
            let status = unsafe {
                (self.capture.select_resource)(
                    self.capture.base.base.base.context,
                    self.capture_stream,
                    producer_generation,
                    offer,
                )
            };
            if status == MEDIA_UNSUPPORTED {
                self.cancel();
                return Err(ConnectionError::Stale);
            }
            if status != STATUS_OK {
                return Err(self.negotiation_error(ConnectionError::Native(status)));
            }
            self.selected = Some((producer_generation, offer));
            Ok(())
        }
    }

    /// Read and synchronously submit at most one frame. Native statuses such as
    /// PENDING and BUSY are returned unchanged. A stale producer selection
    /// retires the consumer stream. A consumer error is never mistaken for stale capture.
    pub fn forward(&mut self, flags: u32) -> Result<Status, ConnectionError> {
        {
            if self.selected.is_none() {
                self.negotiate()?;
            }
            let (generation, offer) = self.selected.expect("negotiated selection");
            let mut delivery = Delivery {
                consumer: self.consumer,
                stream: self.consumer_stream,
                generation,
                offer,
                flags,
                calls: 0,
                result: STATUS_OK,
                invalid: false,
            };
            let sink = CaptureResourceFrameSink {
                context: (&mut delivery as *mut Delivery<'_>).cast(),
                write: deliver,
            };
            // SAFETY: callback context and both bindings remain live throughout read.
            let status = unsafe {
                (self.capture.read_resource_frame)(
                    self.capture.base.base.base.context,
                    self.capture_stream,
                    sink,
                )
            };
            if delivery.invalid
                || delivery.calls > 1
                || (delivery.calls == 1 && status != delivery.result)
                || (delivery.calls == 0 && status == STATUS_OK)
            {
                self.cancel();
                return Err(ConnectionError::Protocol);
            }
            if delivery.calls == 0 && status == MEDIA_UNSUPPORTED {
                self.cancel();
                return Err(ConnectionError::Stale);
            }
            Ok(status)
        }
    }
}
impl Drop for ResourceConnection<'_> {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_abi::{AbiHeader, NativeInterface};
    use amaterasu_media::{
        FRAME_OWNERSHIP_RETAINABLE, FRAME_RESOURCE_VENDOR_BASE, FRAME_SYNC_NONE, FrameDescriptor,
    };
    use std::ptr;

    #[derive(Default)]
    struct Consumer {
        configured: u32,
        submitted: u32,
        transfer: u32,
        epoch: [u8; 16],
        cancelled: u32,
        reject: bool,
        configure_error: Option<Status>,
        producer_offer: u64,
        producer_generation: u64,
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
        _: u64,
        _: u64,
        id: u64,
        producer_generation: u64,
        offer: *const CaptureResourceOffer,
    ) -> Status {
        // SAFETY: fixture supplies both pointers for the call.
        let (consumer, offer) = unsafe { (&mut *context.cast::<Consumer>(), &*offer) };
        if consumer.reject || id != 8 || offer.resource.kind != FRAME_RESOURCE_VENDOR_BASE {
            return MEDIA_UNSUPPORTED;
        }
        consumer.configured += 1;
        consumer.producer_offer = offer.id;
        consumer.producer_generation = producer_generation;
        if let Some(error) = consumer.configure_error.take() {
            return error;
        }
        STATUS_OK
    }
    unsafe extern "C" fn submit(
        context: *mut c_void,
        _: u64,
        frame: *const CaptureResourceFrame,
        _: u32,
    ) -> Status {
        // SAFETY: fixture supplies the consumer context and borrowed frame.
        unsafe {
            let consumer = &mut *context.cast::<Consumer>();
            consumer.submitted += 1;
            consumer.transfer = (*frame).color.transfer;
            consumer.epoch = (*frame).pacing_clock.epoch_id;
        }
        STATUS_OK
    }
    unsafe extern "C" fn cancel(context: *mut c_void, _: u64) {
        // SAFETY: fixture supplies the consumer context.
        unsafe {
            (*context.cast::<Consumer>()).cancelled += 1;
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
            uncertain: false,
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
            clock: Default::default(),
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
            calls: 0,
            result: STATUS_OK,
            invalid: false,
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
            uncertain: false,
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
    fn swallowed_configure_error_cannot_trigger_another_configuration() {
        let mut consumer = Consumer {
            configure_error: Some(MEDIA_BUSY),
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
            uncertain: false,
        };
        let mut candidate = offer();
        // SAFETY: the context and offers are valid for each synchronous callback.
        assert_eq!(
            unsafe { configure((&mut state as *mut Configure<'_>).cast(), &candidate) },
            MEDIA_BUSY
        );
        candidate.id += 1;
        assert_eq!(
            unsafe { configure((&mut state as *mut Configure<'_>).cast(), &candidate) },
            MEDIA_BUSY
        );
        assert_eq!(consumer.configured, 1);
        assert!(state.uncertain);
        assert!(state.selected.is_none());
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
            uncertain: false,
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
        enumerate_errors: std::collections::BTreeMap<u32, Status>,
        select_error: Option<Status>,
        generation: u64,
        offer_id: u64,
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
        candidate.id = if producer.offer_id == 0 {
            5
        } else {
            producer.offer_id
        };
        candidate.accessibility = FRAME_ACCESS_GPU;
        let status = unsafe { (sink.write)(sink.context, &candidate) };
        if status != STATUS_OK {
            return status;
        }
        if let Some(error) = producer.enumerate_errors.remove(&producer.enumerate_calls) {
            return error;
        }
        unsafe {
            *generation =
                if producer.change_generation && producer.enumerate_calls.is_multiple_of(2) {
                    10
                } else if producer.generation == 0 {
                    9
                } else {
                    producer.generation
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
        if let Some(error) = producer.select_error.take() {
            return error;
        }
        let current_generation = if producer.generation == 0 {
            9
        } else {
            producer.generation
        };
        let current_offer = if producer.offer_id == 0 {
            5
        } else {
            producer.offer_id
        };
        if producer.selection_fails || generation != current_generation || id != current_offer {
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
        let producer = unsafe { &*context.cast::<Producer>() };
        if producer.stale || !producer.selected {
            return MEDIA_UNSUPPORTED;
        }
        let descriptor = descriptor(FRAME_RESOURCE_VENDOR_BASE);
        let frame = CaptureResourceFrame {
            offer_generation: if producer.generation == 0 {
                9
            } else {
                producer.generation
            },
            offer_id: if producer.offer_id == 0 {
                5
            } else {
                producer.offer_id
            },
            timestamp_us: 1,
            capture_pacing_timestamp_us: 1,
            sequence: 1,
            width: 1,
            height: 1,
            freshness: 1,
            reserved: 0,
            color: Default::default(),
            clock: Default::default(),
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
        _: amaterasu_abi::BorrowedBytes,
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
        let mut connection =
            unsafe { ResourceConnection::new(&capture, 1, &api, 3, FRAME_ACCESS_GPU).unwrap() };
        assert_eq!(consumer.cancelled, 0);
        assert_eq!(connection.forward(0), Ok(STATUS_OK));
        assert_eq!(consumer.submitted, 1);
        assert_eq!(consumer.cancelled, 0);
        producer.stale = true;
        std::hint::black_box(&producer);
        assert_eq!(connection.forward(0), Err(ConnectionError::Stale));
        assert_eq!(consumer.cancelled, 1);
        assert_eq!(connection.forward(0), Err(ConnectionError::Stale));
        drop(connection);
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
            let mut connection =
                unsafe { ResourceConnection::new(&capture, 1, &api, 3, FRAME_ACCESS_GPU).unwrap() };
            assert_eq!(connection.negotiate(), Err(ConnectionError::Stale));
            assert_eq!(consumer.configured, 1);
            assert_eq!(consumer.cancelled, 1);
            drop(connection);
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
        let mut connection =
            unsafe { ResourceConnection::new(&capture, 1, &api, 3, FRAME_ACCESS_GPU).unwrap() };
        assert_eq!(
            connection.negotiate(),
            Err(ConnectionError::NoCompatibleOffer)
        );
        drop(connection);
        assert_eq!(consumer.cancelled, 0);
    }
    #[test]
    fn transient_producer_failures_keep_configuration_owned_until_retry_or_drop() {
        for status in [MEDIA_BUSY, MEDIA_PENDING] {
            let mut producer = Producer::default();
            // The second pass invokes configure, then reports an invalidated
            // enumeration. The next attempt fails on its first pass as well.
            producer.enumerate_errors.insert(2, status);
            producer.enumerate_errors.insert(3, status);
            let capture = capture_table(&mut producer);
            let mut consumer = Consumer::default();
            let mut api = table(&mut consumer);
            api.enumerate_resources = consumer_enumerate;
            // SAFETY: fixture tables and their contexts outlive this connection.
            let mut connection =
                unsafe { ResourceConnection::new(&capture, 1, &api, 3, FRAME_ACCESS_GPU).unwrap() };
            assert_eq!(connection.forward(0), Err(ConnectionError::Native(status)));
            assert_eq!(consumer.configured, 1);
            assert_eq!(consumer.cancelled, 0);
            assert_eq!(consumer.submitted, 0);
            assert_eq!(connection.forward(0), Err(ConnectionError::Native(status)));
            assert_eq!(consumer.cancelled, 0);
            assert_eq!(consumer.submitted, 0);
            assert_eq!(connection.forward(0), Ok(STATUS_OK));
            assert_eq!(consumer.configured, 2);
            assert_eq!(consumer.submitted, 1);
            drop(connection);
            assert_eq!(consumer.cancelled, 1);

            let mut producer = Producer::default();
            producer.enumerate_errors.insert(2, status);
            let capture = capture_table(&mut producer);
            let mut consumer = Consumer::default();
            let mut api = table(&mut consumer);
            api.enumerate_resources = consumer_enumerate;
            let mut connection =
                unsafe { ResourceConnection::new(&capture, 1, &api, 3, FRAME_ACCESS_GPU).unwrap() };
            assert_eq!(connection.negotiate(), Err(ConnectionError::Native(status)));
            drop(connection);
            assert_eq!(consumer.configured, 1);
            assert_eq!(consumer.cancelled, 1);
            assert_eq!(consumer.submitted, 0);
        }
    }
    #[test]
    fn transient_select_retries_but_uncertain_configure_is_retired() {
        for status in [MEDIA_BUSY, MEDIA_PENDING] {
            let mut producer = Producer {
                select_error: Some(status),
                ..Producer::default()
            };
            let capture = capture_table(&mut producer);
            let mut consumer = Consumer::default();
            let mut api = table(&mut consumer);
            api.enumerate_resources = consumer_enumerate;
            // SAFETY: fixture tables and their contexts outlive this connection.
            let mut connection =
                unsafe { ResourceConnection::new(&capture, 1, &api, 3, FRAME_ACCESS_GPU).unwrap() };
            assert_eq!(connection.forward(0), Err(ConnectionError::Native(status)));
            assert_eq!(consumer.configured, 1);
            assert_eq!(consumer.cancelled, 0);
            assert_eq!(consumer.submitted, 0);
            assert_eq!(connection.forward(0), Ok(STATUS_OK));
            drop(connection);
            assert_eq!(consumer.cancelled, 1);

            let mut producer = Producer::default();
            let capture = capture_table(&mut producer);
            let mut consumer = Consumer {
                configure_error: Some(status),
                ..Consumer::default()
            };
            let mut api = table(&mut consumer);
            api.enumerate_resources = consumer_enumerate;
            let mut connection =
                unsafe { ResourceConnection::new(&capture, 1, &api, 3, FRAME_ACCESS_GPU).unwrap() };
            assert_eq!(connection.negotiate(), Err(ConnectionError::Native(status)));
            assert_eq!(consumer.configured, 1);
            assert_eq!(consumer.cancelled, 1);
            assert_eq!(connection.negotiate(), Err(ConnectionError::Stale));
            drop(connection);
            assert_eq!(consumer.cancelled, 1);
        }
    }
    #[test]
    fn terminal_error_after_pending_retires_the_retained_configuration() {
        let mut producer = Producer::default();
        producer.enumerate_errors.insert(2, MEDIA_BUSY);
        producer
            .enumerate_errors
            .insert(3, amaterasu_media::MEDIA_CLOSED);
        let capture = capture_table(&mut producer);
        let mut consumer = Consumer::default();
        let mut api = table(&mut consumer);
        api.enumerate_resources = consumer_enumerate;
        // SAFETY: fixture tables and their contexts outlive this connection.
        let mut connection =
            unsafe { ResourceConnection::new(&capture, 1, &api, 3, FRAME_ACCESS_GPU).unwrap() };
        assert_eq!(
            connection.negotiate(),
            Err(ConnectionError::Native(MEDIA_BUSY))
        );
        assert_eq!(consumer.cancelled, 0);
        assert_eq!(
            connection.negotiate(),
            Err(ConnectionError::Native(amaterasu_media::MEDIA_CLOSED))
        );
        assert_eq!(consumer.cancelled, 1);
        assert_eq!(connection.negotiate(), Err(ConnectionError::Stale));
        drop(connection);
        assert_eq!(consumer.cancelled, 1);
    }
    #[test]
    fn retry_reconfigures_a_replacement_offer_before_first_frame() {
        let mut producer = Producer::default();
        producer.enumerate_errors.insert(2, MEDIA_BUSY);
        let capture = capture_table(&mut producer);
        let mut consumer = Consumer::default();
        let mut api = table(&mut consumer);
        api.enumerate_resources = consumer_enumerate;
        // SAFETY: fixture tables and their contexts outlive this connection.
        let mut connection =
            unsafe { ResourceConnection::new(&capture, 1, &api, 3, FRAME_ACCESS_GPU).unwrap() };
        assert_eq!(
            connection.forward(0),
            Err(ConnectionError::Native(MEDIA_BUSY))
        );
        assert_eq!(
            (consumer.producer_generation, consumer.producer_offer),
            (9, 5)
        );
        assert_eq!(consumer.submitted, 0);
        producer.generation = 10;
        producer.offer_id = 6;
        std::hint::black_box(&producer);
        assert_eq!(connection.forward(0), Ok(STATUS_OK));
        assert_eq!(
            (consumer.producer_generation, consumer.producer_offer),
            (10, 6)
        );
        assert_eq!(consumer.configured, 2);
        assert_eq!(consumer.submitted, 1);
        drop(connection);
        assert_eq!(consumer.cancelled, 1);
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
}
