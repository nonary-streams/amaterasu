//! Negotiated resource handoff from a GPU processor (v1.4) to an encoder
//! session (v1.5), shared by built-in and external consumers. Negotiation and
//! delivery are schema-agnostic: the encoder's configure decides whether a
//! processor offer is compatible, so new schemas need no consumer changes.
use super::consumer::ProcessingOutcome;
use crate::abi::{STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use amaterasu_media::{
    CAPTURE_FRAME_CACHED_REUSE, CAPTURE_FRAME_FRESH, CaptureResourceFrame,
    CaptureResourceFrameSink, CaptureResourceOffer, CaptureResourceOfferSink, DmaBufFrameLease,
    ENCODER_API_V1_5_SIZE, ENCODER_RESOURCE_INTERFACE_MINOR, EncoderApi, EncoderApiV1_5,
    GPU_PROCESSOR_API_V1_4_SIZE, GPU_PROCESSOR_INTERFACE_MAJOR,
    GPU_PROCESSOR_RESOURCE_OUTPUT_INTERFACE_MINOR, GpuProcessorApi, GpuProcessorApiV1_4,
    INTERFACE_MAJOR, MEDIA_BUSY, MEDIA_UNSUPPORTED,
};
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

/// Bounds a provider's offer list, matching the frame-resource consumer.
const MAX_OFFERS: usize = 64;

/// The processor offer selected for an encoder session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceHandoff {
    pub generation: u64,
    pub offer: u64,
}

/// The v1.4 tail of a typed GPU processor binding, when it is advertised.
///
/// # Safety
/// `api` must be the prefix of a live binding with the GPU processor contract.
pub unsafe fn resource_output_table(api: &GpuProcessorApi) -> Option<&GpuProcessorApiV1_4> {
    let header = api.base.header;
    (header.major == GPU_PROCESSOR_INTERFACE_MAJOR
        && header.minor >= GPU_PROCESSOR_RESOURCE_OUTPUT_INTERFACE_MINOR
        && header.size >= GPU_PROCESSOR_API_V1_4_SIZE)
        // SAFETY: the advertised minor and complete size cover the v1.4 tail.
        .then(|| unsafe { &*std::ptr::from_ref(api).cast::<GpuProcessorApiV1_4>() })
}

/// The v1.5 tail of a typed encoder binding, when it is advertised.
///
/// # Safety
/// `api` must be the prefix of a live binding with the encoder contract.
pub unsafe fn resource_input_table(api: &EncoderApi) -> Option<&EncoderApiV1_5> {
    let header = api.base.header;
    (header.major == INTERFACE_MAJOR
        && header.minor >= ENCODER_RESOURCE_INTERFACE_MINOR
        && header.size >= ENCODER_API_V1_5_SIZE)
        // SAFETY: the advertised minor and complete size cover the v1.5 tail.
        .then(|| unsafe { &*std::ptr::from_ref(api).cast::<EncoderApiV1_5>() })
}

#[derive(Default)]
struct Ids {
    ids: Vec<u64>,
    invalid: bool,
}

unsafe extern "C" fn collect(context: *mut c_void, offer: *const CaptureResourceOffer) -> Status {
    if context.is_null() || offer.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: enumeration supplies this live context and offer for the call.
    let (ids, offer) = unsafe { (&mut *context.cast::<Ids>(), &*offer) };
    if offer.id == 0 || ids.ids.len() >= MAX_OFFERS || ids.ids.contains(&offer.id) {
        ids.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    ids.ids.push(offer.id);
    STATUS_OK
}

struct Configure<'a> {
    encoder: &'a EncoderApiV1_5,
    session: u64,
    consumer_generation: u64,
    consumer_ids: &'a [u64],
    producer_generation: u64,
    selected: Option<u64>,
    failure: Option<Status>,
}

unsafe extern "C" fn configure(context: *mut c_void, offer: *const CaptureResourceOffer) -> Status {
    if context.is_null() || offer.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: enumeration supplies this live context and offer for the call.
    let (state, offer) = unsafe { (&mut *context.cast::<Configure<'_>>(), &*offer) };
    if state.selected.is_some() {
        return STATUS_OK;
    }
    for &id in state.consumer_ids {
        // SAFETY: the encoder binding and the borrowed offer are live here.
        let status = unsafe {
            (state.encoder.configure_resource)(
                state.encoder.base.base.base.base.context,
                state.session,
                state.consumer_generation,
                id,
                state.producer_generation,
                offer,
            )
        };
        if status == STATUS_OK {
            state.selected = Some(offer.id);
            return STATUS_OK;
        }
        if status != MEDIA_UNSUPPORTED {
            state.failure = Some(status);
            return status;
        }
    }
    STATUS_OK
}

/// Configures the encoder session with the first compatible processor output
/// offer, then selects that offer on the processor. UNSUPPORTED means no pair
/// is compatible (or a side lacks the tails); the caller keeps the lease path.
///
/// # Safety
/// Both typed bindings and their streams must be live and owned by the caller
/// for this call, on the providers' owning thread.
pub unsafe fn negotiate_resource_handoff(
    processor: &GpuProcessorApi,
    stream: u64,
    encoder: &EncoderApi,
    session: u64,
) -> Result<ResourceHandoff, Status> {
    // SAFETY: the caller guarantees live typed bindings.
    let (Some(producer), Some(consumer)) = (unsafe { resource_output_table(processor) }, unsafe {
        resource_input_table(encoder)
    }) else {
        return Err(MEDIA_UNSUPPORTED);
    };
    let processor_context = processor.base.context;
    let mut consumer_ids = Ids::default();
    let mut consumer_generation = 0;
    // SAFETY: the sink context is live for this synchronous enumeration.
    let status = unsafe {
        (consumer.enumerate_resources)(
            encoder.base.context,
            session,
            CaptureResourceOfferSink {
                context: (&mut consumer_ids as *mut Ids).cast(),
                write: collect,
            },
            &mut consumer_generation,
        )
    };
    if status != STATUS_OK {
        return Err(status);
    }
    if consumer_ids.invalid || consumer_generation == 0 {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    // The producer generation is reported only once enumeration returns, and
    // configure needs it: enumerate once to learn it, again to configure.
    let mut producer_ids = Ids::default();
    let mut producer_generation = 0;
    // SAFETY: the sink context is live for this synchronous enumeration.
    let status = unsafe {
        (producer.enumerate_outputs)(
            processor_context,
            stream,
            CaptureResourceOfferSink {
                context: (&mut producer_ids as *mut Ids).cast(),
                write: collect,
            },
            &mut producer_generation,
        )
    };
    if status != STATUS_OK {
        return Err(status);
    }
    if producer_ids.invalid || producer_generation == 0 {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let mut state = Configure {
        encoder: consumer,
        session,
        consumer_generation,
        consumer_ids: &consumer_ids.ids,
        producer_generation,
        selected: None,
        failure: None,
    };
    let mut generation = 0;
    // SAFETY: the configure state and both bindings are live for this call.
    let status = unsafe {
        (producer.enumerate_outputs)(
            processor_context,
            stream,
            CaptureResourceOfferSink {
                context: (&mut state as *mut Configure<'_>).cast(),
                write: configure,
            },
            &mut generation,
        )
    };
    if let Some(failure) = state.failure {
        return Err(failure);
    }
    if status != STATUS_OK {
        return Err(status);
    }
    if generation != producer_generation {
        // The offers changed between enumerations; the configuration is stale.
        return Err(MEDIA_BUSY);
    }
    let Some(offer) = state.selected else {
        return Err(MEDIA_UNSUPPORTED);
    };
    // Configure first, then select, as the frame-resource contract requires.
    // SAFETY: the processor binding and stream are live.
    let status =
        unsafe { (producer.select_output)(processor_context, stream, producer_generation, offer) };
    if status != STATUS_OK {
        return Err(status);
    }
    Ok(ResourceHandoff {
        generation: producer_generation,
        offer,
    })
}

struct Delivery<'a, F> {
    submit: &'a mut F,
    handoff: ResourceHandoff,
    sequence: u64,
    timestamp_us: u64,
    freshness: u32,
    count: usize,
    result: Status,
    invalid: bool,
}

unsafe extern "C" fn deliver<F>(context: *mut c_void, frame: *const CaptureResourceFrame) -> Status
where
    F: FnMut(&CaptureResourceFrame) -> Status,
{
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the dispatch owns this stack context for the synchronous call.
    let delivery = unsafe { &mut *context.cast::<Delivery<'_, F>>() };
    delivery.count += 1;
    if delivery.count != 1 || frame.is_null() {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the provider keeps the frame live and referenced during the call.
    let frame = unsafe { &*frame };
    if frame.offer_generation != delivery.handoff.generation
        || frame.offer_id != delivery.handoff.offer
        || frame.reserved != 0
        || frame.sequence != delivery.sequence
        || frame.timestamp_us != delivery.timestamp_us
        || frame.freshness != delivery.freshness
    {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // Consumer code must not unwind through this extern-C callback.
    delivery.result = catch_unwind(AssertUnwindSafe(|| (delivery.submit)(frame)))
        .unwrap_or(STATUS_INTERNAL_ERROR);
    delivery.result
}

/// Processes one frame into the negotiated resource and hands it to `submit`
/// (normally the encoder's `submit_resource`), which must retain a reference
/// for any asynchronous use before returning.
///
/// # Safety
/// The input lease callbacks and both bindings must stay live for this call,
/// and `handoff` must be the current selection for `stream`.
pub unsafe fn process_gpu_to_resource<F>(
    api: &GpuProcessorApi,
    stream: u64,
    handoff: ResourceHandoff,
    lease: &DmaBufFrameLease,
    freshness: u32,
    mut submit: F,
) -> ProcessingOutcome
where
    F: FnMut(&CaptureResourceFrame) -> Status,
{
    if lease.validate().is_err()
        || !matches!(freshness, CAPTURE_FRAME_FRESH | CAPTURE_FRAME_CACHED_REUSE)
    {
        return ProcessingOutcome::Failed(STATUS_INVALID_ARGUMENT);
    }
    // SAFETY: the caller guarantees a live typed processor binding.
    let Some(table) = (unsafe { resource_output_table(api) }) else {
        return ProcessingOutcome::Failed(MEDIA_UNSUPPORTED);
    };
    let mut delivery = Delivery {
        submit: &mut submit,
        handoff,
        sequence: lease.frame.sequence,
        timestamp_us: lease.frame.timestamp_us,
        freshness,
        count: 0,
        result: STATUS_OK,
        invalid: false,
    };
    let sink = CaptureResourceFrameSink {
        context: (&mut delivery as *mut Delivery<'_, F>).cast(),
        write: deliver::<F>,
    };
    // SAFETY: lease, sink, callbacks and native bindings are live for the call.
    let status =
        unsafe { (table.process_to_resource)(api.base.context, stream, lease, freshness, sink) };
    if delivery.invalid
        || (delivery.count == 1 && status != delivery.result)
        || (delivery.count == 0 && status == STATUS_OK)
    {
        return ProcessingOutcome::ProtocolViolation;
    }
    match status {
        STATUS_OK => ProcessingOutcome::Accepted,
        MEDIA_BUSY => ProcessingOutcome::Busy,
        status => ProcessingOutcome::Failed(status),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processing::{
        GpuFrame, GpuFrameProcessor, GpuOutputFrame, ResourceOutputFrame, into_gpu_binding,
    };
    use amaterasu_abi::{AbiHeader, NativeInterface};
    use amaterasu_media::*;
    use std::{cell::Cell, rc::Rc};

    /// Offer payloads named by kind only; the encoder fixture decides
    /// compatibility exactly as a real configure would.
    const KIND_A: u32 = FRAME_RESOURCE_VENDOR_BASE | 0x41;
    const KIND_B: u32 = FRAME_RESOURCE_VENDOR_BASE | 0x42;
    const NONE: FrameDescriptor = FrameDescriptor {
        kind: FRAME_SYNC_NONE,
        version: 0,
        size: 0,
        reserved: 0,
        namespace: [0; 16],
        data: std::ptr::null(),
    };
    fn descriptor(kind: u32) -> FrameDescriptor {
        FrameDescriptor {
            kind,
            version: 1,
            size: 0,
            reserved: 0,
            namespace: [9; 16],
            data: std::ptr::null(),
        }
    }
    fn offer(id: u64, kind: u32) -> CaptureResourceOffer {
        CaptureResourceOffer {
            id,
            ownership: FRAME_OWNERSHIP_RETAINABLE,
            accessibility: FRAME_ACCESS_GPU,
            reserved: 0,
            resource: descriptor(kind),
            acquire_sync: NONE,
            completion_sync: NONE,
        }
    }

    #[derive(Default)]
    struct Shared {
        references: Cell<i32>,
        selected: Cell<Option<(u64, u64)>>,
        configured: Cell<Option<(u64, u64, u64)>>,
        submitted: Cell<u32>,
        accepted_kind: Cell<u32>,
        reject_submit: Cell<bool>,
    }

    unsafe extern "C" fn output_retain(context: *mut c_void) -> Status {
        let shared = unsafe { &*context.cast::<Shared>() };
        shared.references.set(shared.references.get() + 1);
        STATUS_OK
    }
    unsafe extern "C" fn output_release(context: *mut c_void, completion: *const FrameDescriptor) {
        assert_eq!(unsafe { (*completion).kind }, FRAME_SYNC_NONE);
        let shared = unsafe { &*context.cast::<Shared>() };
        shared.references.set(shared.references.get() - 1);
    }

    /// A processor written only against the public provider trait.
    struct Processor {
        shared: Rc<Shared>,
        offers: Vec<CaptureResourceOffer>,
    }
    impl GpuFrameProcessor for Processor {
        fn process(&mut self, _: u64, _: GpuFrame<'_>) -> Result<GpuOutputFrame, Status> {
            Err(MEDIA_UNSUPPORTED)
        }
        fn cancel(&mut self, _: u64) {}
        fn shutdown(&mut self) {}
        fn output_offers(&mut self, _: u64) -> Result<(u64, Vec<CaptureResourceOffer>), Status> {
            Ok((5, self.offers.clone()))
        }
        fn select_output(&mut self, _: u64, generation: u64, offer: u64) -> Result<(), Status> {
            assert!(
                self.shared.configured.get().is_some(),
                "the encoder is configured before the processor selects"
            );
            self.shared.selected.set(Some((generation, offer)));
            Ok(())
        }
        fn process_to_resource(
            &mut self,
            _: u64,
            input: GpuFrame<'_>,
        ) -> Result<ResourceOutputFrame, Status> {
            let (generation, offer) = self.shared.selected.get().ok_or(MEDIA_UNSUPPORTED)?;
            self.shared.references.set(self.shared.references.get() + 1);
            let frame = CaptureResourceFrame {
                offer_generation: generation,
                offer_id: offer,
                timestamp_us: input.frame().timestamp_us,
                capture_pacing_timestamp_us: 0,
                sequence: input.frame().sequence,
                width: 16,
                height: 16,
                freshness: input.freshness(),
                reserved: 0,
                color: FrameColorMetadata::default(),
                clock: FrameClockMetadata::default(),
                pacing_clock: FrameClockMetadata::default(),
                resource: descriptor(KIND_B),
                acquire_sync: NONE,
                context: Rc::as_ptr(&self.shared).cast_mut().cast(),
                retain: output_retain,
                release: output_release,
            };
            // SAFETY: the fixture's shared counter outlives every reference.
            Ok(unsafe { ResourceOutputFrame::from_owned(frame) })
        }
    }

    unsafe extern "C" fn encoder_offers(
        context: *mut c_void,
        _: u64,
        sink: CaptureResourceOfferSink,
        generation: *mut u64,
    ) -> Status {
        let _ = context;
        unsafe { *generation = 3 };
        let accepted = offer(11, KIND_B);
        unsafe { (sink.write)(sink.context, &accepted) }
    }
    unsafe extern "C" fn encoder_configure(
        context: *mut c_void,
        _: u64,
        consumer_generation: u64,
        consumer_id: u64,
        producer_generation: u64,
        producer: *const CaptureResourceOffer,
    ) -> Status {
        let shared = unsafe { &*context.cast::<Shared>() };
        let producer = unsafe { &*producer };
        assert_eq!((consumer_generation, consumer_id), (3, 11));
        if producer.resource.kind != shared.accepted_kind.get() {
            return MEDIA_UNSUPPORTED;
        }
        shared
            .configured
            .set(Some((consumer_id, producer_generation, producer.id)));
        STATUS_OK
    }
    unsafe extern "C" fn encoder_submit(
        context: *mut c_void,
        _: u64,
        frame: *const CaptureResourceFrame,
        _: u32,
    ) -> Status {
        let shared = unsafe { &*context.cast::<Shared>() };
        if shared.reject_submit.get() {
            return MEDIA_BUSY;
        }
        let frame = unsafe { &*frame };
        // An asynchronous consumer retains before returning; release later.
        assert_eq!(unsafe { (frame.retain)(frame.context) }, STATUS_OK);
        unsafe { (frame.release)(frame.context, &NONE) };
        shared.submitted.set(shared.submitted.get() + 1);
        STATUS_OK
    }
    unsafe extern "C" fn unused_submit(
        _: *mut c_void,
        _: u64,
        _: *const CpuFrame,
        _: u32,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_receive(_: *mut c_void, _: u64, _: PacketSink) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_drain(_: *mut c_void, _: u64) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_dmabuf(
        _: *mut c_void,
        _: u64,
        _: *const DmaBufFrame,
        _: u32,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_leased(
        _: *mut c_void,
        _: u64,
        _: *const DmaBufFrameLease,
        _: u32,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_cancel(_: *mut c_void, _: u64) {}
    unsafe extern "C" fn unused_timing(
        _: *mut c_void,
        _: u64,
        _: *mut EncodeTimingSnapshotV1,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unused_ready(_: *mut c_void, _: u64) -> Status {
        STATUS_OK
    }

    fn encoder_table(shared: &Rc<Shared>, minor: u32) -> EncoderApiV1_5 {
        EncoderApiV1_5 {
            base: EncoderApiV1_4 {
                base: EncoderApiV1_3 {
                    base: EncoderApiV1_2 {
                        base: NativeInterface {
                            header: AbiHeader {
                                major: INTERFACE_MAJOR,
                                minor,
                                size: ENCODER_API_V1_5_SIZE,
                            },
                            context: Rc::as_ptr(shared).cast_mut().cast(),
                            release: None,
                        },
                        submit: unused_submit,
                        receive: unused_receive,
                        drain: unused_drain,
                        submit_dmabuf: unused_dmabuf,
                        submit_dmabuf_leased: unused_leased,
                        cancel_dmabuf_leases: unused_cancel,
                    },
                    encode_timing: unused_timing,
                },
                gpu_input_ready: unused_ready,
            },
            enumerate_resources: encoder_offers,
            configure_resource: encoder_configure,
            submit_resource: encoder_submit,
        }
    }

    unsafe extern "C" fn input_retain(_: *mut c_void) -> Status {
        STATUS_OK
    }
    unsafe extern "C" fn input_release(_: *mut c_void, _: i32) {}
    fn input(sequence: u64) -> DmaBufFrameLease {
        let mut planes = [DmaBufPlane::UNUSED; DMABUF_MAX_PLANES];
        planes[0] = DmaBufPlane {
            fd: 0,
            acquire_fence_fd: -1,
            offset: 0,
            pitch: 64,
        };
        DmaBufFrameLease {
            frame: DmaBufFrame {
                width: 16,
                height: 16,
                fourcc: u32::from_le_bytes(*b"XR24"),
                plane_count: 1,
                modifier: 0,
                planes,
                color: DMABUF_COLOR_SRGB,
                reserved: 0,
                timestamp_us: 40,
                sequence,
            },
            context: std::ptr::dangling_mut::<u8>().cast(),
            retain: Some(input_retain),
            release: Some(input_release),
        }
    }

    fn fixture(shared: &Rc<Shared>) -> *const GpuProcessorApi {
        into_gpu_binding(Processor {
            shared: Rc::clone(shared),
            offers: vec![offer(1, KIND_A), offer(2, KIND_B)],
        })
    }

    #[test]
    fn negotiates_the_offer_the_encoder_accepts_and_delivers_it() {
        let shared = Rc::new(Shared::default());
        shared.accepted_kind.set(KIND_B);
        let raw = fixture(&shared);
        let processor = unsafe { &*raw };
        let table = encoder_table(&shared, ENCODER_RESOURCE_INTERFACE_MINOR);
        let encoder = unsafe { &*std::ptr::from_ref(&table).cast::<EncoderApi>() };
        let handoff = unsafe { negotiate_resource_handoff(processor, 4, encoder, 9) }.unwrap();
        assert_eq!(
            handoff,
            ResourceHandoff {
                generation: 5,
                offer: 2
            }
        );
        assert_eq!(shared.configured.get(), Some((11, 5, 2)));
        assert_eq!(shared.selected.get(), Some((5, 2)));
        let submit = |frame: &CaptureResourceFrame| unsafe {
            (table.submit_resource)(table.base.base.base.base.context, 9, frame, 0)
        };
        let outcome = unsafe {
            process_gpu_to_resource(
                processor,
                4,
                handoff,
                &input(7),
                CAPTURE_FRAME_FRESH,
                submit,
            )
        };
        assert_eq!(outcome, ProcessingOutcome::Accepted);
        assert_eq!(shared.submitted.get(), 1);
        assert_eq!(shared.references.get(), 0, "every reference released");
        shared.reject_submit.set(true);
        let outcome = unsafe {
            process_gpu_to_resource(
                processor,
                4,
                handoff,
                &input(8),
                CAPTURE_FRAME_FRESH,
                submit,
            )
        };
        assert_eq!(
            outcome,
            ProcessingOutcome::Busy,
            "sink rejection propagates"
        );
        assert_eq!(shared.references.get(), 0);
        unsafe { (*raw).base.release.unwrap()((*raw).base.context) };
    }

    #[test]
    fn incompatible_or_older_encoders_keep_the_lease_path() {
        let shared = Rc::new(Shared::default());
        shared.accepted_kind.set(FRAME_RESOURCE_VENDOR_BASE | 0x7f);
        let raw = fixture(&shared);
        let processor = unsafe { &*raw };
        let table = encoder_table(&shared, ENCODER_RESOURCE_INTERFACE_MINOR);
        let encoder = unsafe { &*std::ptr::from_ref(&table).cast::<EncoderApi>() };
        assert_eq!(
            unsafe { negotiate_resource_handoff(processor, 4, encoder, 9) },
            Err(MEDIA_UNSUPPORTED)
        );
        assert_eq!(
            shared.selected.get(),
            None,
            "nothing selected without a match"
        );
        let older = encoder_table(&shared, ENCODER_RESOURCE_INTERFACE_MINOR - 1);
        let older = unsafe { &*std::ptr::from_ref(&older).cast::<EncoderApi>() };
        assert_eq!(
            unsafe { negotiate_resource_handoff(processor, 4, older, 9) },
            Err(MEDIA_UNSUPPORTED)
        );
        unsafe { (*raw).base.release.unwrap()((*raw).base.context) };
    }

    #[test]
    fn a_frame_with_another_identity_is_a_protocol_violation() {
        let shared = Rc::new(Shared::default());
        shared.accepted_kind.set(KIND_B);
        let raw = fixture(&shared);
        let processor = unsafe { &*raw };
        let table = encoder_table(&shared, ENCODER_RESOURCE_INTERFACE_MINOR);
        let encoder = unsafe { &*std::ptr::from_ref(&table).cast::<EncoderApi>() };
        let handoff = unsafe { negotiate_resource_handoff(processor, 4, encoder, 9) }.unwrap();
        let stale = ResourceHandoff {
            generation: handoff.generation + 1,
            ..handoff
        };
        let outcome = unsafe {
            process_gpu_to_resource(processor, 4, stale, &input(7), CAPTURE_FRAME_FRESH, |_| {
                STATUS_OK
            })
        };
        assert_eq!(outcome, ProcessingOutcome::ProtocolViolation);
        assert_eq!(shared.references.get(), 0);
        unsafe { (*raw).base.release.unwrap()((*raw).base.context) };
    }
}
