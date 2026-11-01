//! Synthetic submitted-frame providers shared by both independent test artifacts.
use amaterasu_media::*;
use amaterasu_module_sdk::abi::*;
use std::{ffi::c_void, mem::size_of, ptr};

unsafe extern "C" fn release_fixture(_: *mut c_void) {}
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
    metadata: Vec<u8>,
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
            release: Some(release_fixture),
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
    metadata_context: Option<amaterasu_media::FrameMetadataCaptureContextV1>,
    wrong_metadata_source: bool,
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
        *generation = if producer.change_generation && producer.enumerate_calls.is_multiple_of(2) {
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
                producer
                    .metadata_context
                    .map_or([1; 16], |c| c.clock.epoch_id)
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
unsafe extern "C" fn unused_frame(_: *mut c_void, _: u64, _: amaterasu_media::FrameSink) -> Status {
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
                    release: Some(release_fixture),
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
#[derive(Default)]
struct TestProcessor {
    input: Consumer,
    selected: bool,
    transfer: u32,
    drop_output: bool,
    swallow_error: bool,
    corrupt_clock: bool,
    corrupt_metadata: bool,
    claim_application: bool,
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
                release: Some(release_fixture),
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
unsafe extern "C" fn metadata_setup(
    context: *mut c_void,
    _: u64,
    setup: *const amaterasu_media::FrameMetadataCaptureContextV1,
) -> Status {
    let producer = unsafe { &mut *context.cast::<Producer>() };
    if setup.is_null() || !producer.selected || unsafe { (*setup).validate() }.is_err() {
        return STATUS_INVALID_ARGUMENT;
    }
    producer.metadata_context = Some(unsafe { *setup });
    STATUS_OK
}
struct MetadataSink {
    setup: amaterasu_media::FrameMetadataCaptureContextV1,
    sink: amaterasu_media::FrameResourceMetadataSinkV1,
    wrong_source: bool,
}
unsafe extern "C" fn capture_to_metadata(
    context: *mut c_void,
    frame: *const CaptureResourceFrame,
) -> Status {
    use amaterasu_module_sdk::{abi::BorrowedBytes, encode::FormatId, media::metadata::*};
    let output = unsafe { &mut *context.cast::<MetadataSink>() };
    let frame = unsafe { &*frame };
    let setup = &output.setup;
    let bytes = FrameMetadataBundle {
        identity: FrameMetadataIdentity {
            session_id: setup.session_id,
            submission_id: frame.sequence,
            source_id: setup.source_id + u64::from(output.wrong_source),
            source_generation: setup.source_generation,
            config_generation: setup.config_generation,
            timestamp_us: frame.timestamp_us,
            clock_domain: setup.clock.domain,
            clock_epoch: setup.clock.epoch_id,
        },
        owner: CarriageOwner::FramerInsert,
        items: vec![FrameMetadataItem {
            schema: FormatId {
                namespace_id: 1,
                value: 1,
                schema: 1,
            },
            required: true,
            payload: vec![0xb5, frame.sequence as u8],
        }],
    }
    .encode()
    .unwrap();
    let wrapped = amaterasu_media::FrameResourceMetadataFrameV1 {
        size: amaterasu_media::FRAME_RESOURCE_METADATA_FRAME_V1_SIZE,
        version: 1,
        frame: *frame,
        metadata: BorrowedBytes::new(&bytes),
    };
    unsafe { (output.sink.write)(output.sink.context, &wrapped) }
}
unsafe extern "C" fn producer_metadata_read(
    context: *mut c_void,
    stream: u64,
    sink: amaterasu_media::FrameResourceMetadataSinkV1,
) -> Status {
    let producer = unsafe { &mut *context.cast::<Producer>() };
    let Some(setup) = producer.metadata_context else {
        return MEDIA_UNSUPPORTED;
    };
    let mut output = MetadataSink {
        setup,
        sink,
        wrong_source: producer.wrong_metadata_source,
    };
    unsafe {
        producer_read(
            context,
            stream,
            CaptureResourceFrameSink {
                context: (&mut output as *mut MetadataSink).cast(),
                write: capture_to_metadata,
            },
        )
    }
}
unsafe extern "C" fn unused_hdr(
    _: *mut c_void,
    _: u64,
    metadata: *mut amaterasu_media::HdrMetadata,
) -> Status {
    if metadata.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe {
        *metadata = amaterasu_media::HdrMetadata::default();
    }
    STATUS_OK
}
fn metadata_capture_table(producer: &mut Producer) -> amaterasu_media::CaptureApiV1_11 {
    let mut original = capture_table(producer);
    original.base.base.base.header = AbiHeader {
        size: size_of::<amaterasu_media::CaptureApiV1_11>() as u32,
        major: 1,
        minor: 11,
    };
    let capture9 = amaterasu_media::CaptureApiV1_9 {
        base: original.base,
        enumerate_resources: original.enumerate_resources,
        select_resource: original.select_resource,
        read_resource_frame: original.read_resource_frame,
        hdr_metadata: unused_hdr,
    };
    amaterasu_media::CaptureApiV1_11 {
        base: amaterasu_media::CaptureApiV1_10 {
            base: capture9,
            read_resource_frame_with_metadata: producer_metadata_read,
        },
        configure_frame_metadata: metadata_setup,
    }
}
unsafe extern "C" fn processor_metadata_process(
    context: *mut c_void,
    _: u64,
    input: *const amaterasu_media::FrameResourceMetadataFrameV1,
    sink: amaterasu_media::FrameResourceMetadataSinkV1,
) -> Status {
    use amaterasu_module_sdk::{abi::BorrowedBytes, media::metadata::FrameMetadataBundle};
    let processor = unsafe { &mut *context.cast::<TestProcessor>() };
    if !processor.selected {
        return MEDIA_UNSUPPORTED;
    }
    let input = unsafe { &*input };
    let mut output = *input;
    output.frame.offer_generation = 9;
    output.frame.offer_id = 5;
    output.frame.color = amaterasu_media::FrameColorMetadata {
        version: 1,
        size: size_of::<amaterasu_media::FrameColorMetadata>() as u32,
        transfer: processor.transfer,
        ..Default::default()
    };
    let bytes = unsafe { input.metadata.read() }.unwrap();
    let changed = if processor.corrupt_metadata || processor.claim_application {
        let mut bundle = FrameMetadataBundle::decode(bytes).unwrap();
        if processor.corrupt_metadata {
            bundle.items[0].payload.push(42);
        }
        if processor.claim_application {
            bundle.owner = amaterasu_module_sdk::media::metadata::CarriageOwner::EncoderEmbed;
        }
        Some(bundle.encode().unwrap())
    } else {
        None
    };
    if let Some(bytes) = &changed {
        output.metadata = BorrowedBytes::new(bytes);
    }
    unsafe { (sink.write)(sink.context, &output) }
}
unsafe extern "C" fn consumer_metadata_submit(
    context: *mut c_void,
    _: u64,
    frame: *const amaterasu_media::FrameResourceMetadataFrameV1,
    flags: u32,
) -> Status {
    let consumer = unsafe { &mut *context.cast::<Consumer>() };
    if consumer.busy {
        return amaterasu_media::MEDIA_BUSY;
    }
    let frame = unsafe { &*frame };
    consumer.metadata = unsafe { frame.metadata.read() }.unwrap().to_vec();
    consumer.last_flags = flags;
    consumer.transfer = frame.frame.color.transfer;
    consumer.submitted += 1;
    STATUS_OK
}
struct SubmittedEncoderFixture {
    consumer: *mut Consumer,
    configured: bool,
    sent: bool,
}
unsafe extern "C" fn submitted_probe(
    _: *mut c_void,
    config: *const amaterasu_media::EncoderSessionConfig,
    sink: amaterasu_media::EncoderControlSink,
) -> Status {
    let config = unsafe { &*config };
    if config.mode != amaterasu_media::ENCODER_SESSION_SUBMITTED
        || config.source_id != 1
        || config.codec.namespace_id != 42
        || config.codec.value != 9
        || config.framing.value != 10
    {
        return MEDIA_UNSUPPORTED;
    }
    let response = amaterasu_media::EncoderControlResponse {
        size: size_of::<amaterasu_media::EncoderControlResponse>() as u32,
        kind: 0,
        request_id: 0,
        effective_generation: 1,
        effective_submission_id: 0,
        outcome: amaterasu_media::ENCODER_OUTCOME_APPLIED,
        reserved: 0,
        schema: amaterasu_media::EncoderFormatId {
            namespace_id: 0,
            value: 0,
            schema: 0,
        },
        data: amaterasu_module_sdk::abi::BorrowedBytes::new(&[]),
    };
    unsafe { (sink.write)(sink.context, &response) }
}
unsafe extern "C" fn submitted_configure(
    context: *mut c_void,
    _: u64,
    config: *const amaterasu_media::EncoderSessionConfig,
) -> Status {
    let fixture = unsafe { &mut *context.cast::<SubmittedEncoderFixture>() };
    if unsafe { (*config).mode } != amaterasu_media::ENCODER_SESSION_SUBMITTED {
        return MEDIA_UNSUPPORTED;
    }
    fixture.configured = true;
    STATUS_OK
}
unsafe extern "C" fn submitted_receive(
    context: *mut c_void,
    session: u64,
    _: usize,
    sink: amaterasu_media::EncoderOutputSink,
) -> Status {
    use amaterasu_media::*;
    use amaterasu_module_sdk::abi::BorrowedBytes;
    let fixture = unsafe { &mut *context.cast::<SubmittedEncoderFixture>() };
    if !fixture.configured || fixture.sent || unsafe { (*fixture.consumer).submitted } == 0 {
        return MEDIA_PENDING;
    }
    let bytes = [10, 20, super::ENCODER_MARKER];
    let chunk = EncoderOutputChunk {
        size: size_of::<EncoderOutputChunk>() as u32,
        flags: ENCODER_CHUNK_ACCESS_UNIT_START
            | ENCODER_CHUNK_ACCESS_UNIT_END
            | ENCODER_CHUNK_KEYFRAME,
        session_id: session,
        access_unit_id: 1,
        submission_id: 1,
        source_id: 1,
        config_generation: 1,
        timestamp_us: 1,
        codec: EncoderFormatId {
            namespace_id: 42,
            value: 9,
            schema: 1,
        },
        framing: EncoderFormatId {
            namespace_id: 42,
            value: 10,
            schema: 1,
        },
        bytes: BorrowedBytes::new(&bytes),
    };
    let status = unsafe { (sink.write)(sink.context, &chunk) };
    if status == STATUS_OK {
        fixture.sent = true;
    }
    status
}
unsafe extern "C" fn submitted_control(
    _: *mut c_void,
    _: u64,
    _: *const amaterasu_media::EncoderControlRequest,
    _: amaterasu_media::EncoderControlSink,
) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn submitted_drain(_: *mut c_void, _: u64) -> Status {
    STATUS_OK
}
unsafe extern "C" fn submitted_destroy(_: *mut c_void, _: u64) {}
struct TestCapabilityState {
    generation: u64,
    pins: usize,
    invalidate_requested: bool,
    fail_stage: u32,
}

unsafe extern "C" fn capability_enumerate(
    context: *mut c_void,
    session: u64,
    sink: amaterasu_media::FrameMetadataCapabilitySinkV1,
    generation: *mut u64,
) -> Status {
    use amaterasu_media::*;
    if session == 0 || generation.is_null() || context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe {
        *generation = (*context.cast::<TestCapabilityState>()).generation;
    }
    let claim = FrameMetadataCapabilityV1 {
        schema: FrameMetadataSchemaId {
            namespace_id: 1,
            value: 1,
            schema: 1,
        },
        operations: FRAME_METADATA_CAP_FORWARD,
        reserved: 0,
    };
    unsafe { (sink.write)(sink.context, &claim) }
}

unsafe extern "C" fn capability_probe(
    _: *mut c_void,
    query: *const amaterasu_media::FrameMetadataProfileProbeV1,
) -> Status {
    use amaterasu_media::*;
    if query.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let query = unsafe { &*query };
    if query.size as usize != size_of::<FrameMetadataProfileProbeV1>()
        || query.version != 1
        || !(FRAME_METADATA_STAGE_CAPTURE..=FRAME_METADATA_STAGE_ENCODER).contains(&query.stage)
        || query.owner != FRAME_METADATA_OWNER_FRAMER
        || (
            query.schema.namespace_id,
            query.schema.value,
            query.schema.schema,
        ) != (1, 1, 1)
        || (
            query.codec.namespace_id,
            query.codec.value,
            query.codec.schema,
        ) != (42, 9, 1)
        || (
            query.framing.namespace_id,
            query.framing.value,
            query.framing.schema,
        ) != (42, 10, 1)
        || query.width == 0
        || query.height == 0
        || query.fps == 0
        || query.bit_depth != 8
        || query.chroma != ENCODER_CHROMA_420
        || query.bitrate == 0
        || query.reserved != 0
    {
        return MEDIA_UNSUPPORTED;
    }
    STATUS_OK
}

unsafe extern "C" fn capability_pin(
    context: *mut c_void,
    request: *const amaterasu_media::FrameMetadataGenerationPinV1,
    lease: *mut u64,
) -> Status {
    if context.is_null() || request.is_null() || lease.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let request = unsafe { &*request };
    let state = unsafe { &mut *context.cast::<TestCapabilityState>() };
    if request.capability_generation != state.generation
        || state.invalidate_requested
        || request.stage == state.fail_stage
        || !(FRAME_METADATA_STAGE_CAPTURE..=FRAME_METADATA_STAGE_ENCODER).contains(&request.stage)
        || request.owner != FRAME_METADATA_OWNER_FRAMER
        || request.provider_session == 0
        || request.host_session == 0
        || request.source_id == 0
        || request.source_generation != 9
        || request.config_generation != 1
        || (
            request.schema.namespace_id,
            request.schema.value,
            request.schema.schema,
        ) != (1, 1, 1)
    {
        return STATUS_INVALID_ARGUMENT;
    }
    state.pins += 1;
    unsafe { *lease = u64::from(request.stage) };
    STATUS_OK
}

unsafe extern "C" fn capability_unpin(context: *mut c_void, _: u64) {
    let state = unsafe { &mut *context.cast::<TestCapabilityState>() };
    state.pins -= 1;
    if state.pins == 0 && state.invalidate_requested {
        state.generation += 1;
    }
}

unsafe extern "C" fn bind_fixture(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    if context.is_null()
        || query.is_null()
        || output.is_null()
        || unsafe { principal.read() }.ok() != Some(b"video")
    {
        return STATUS_INVALID_ARGUMENT;
    }
    let table = unsafe { &*context.cast::<NativeInterface>() };
    let query = unsafe { &*query };
    if !query.header.supports::<NativeInterfaceQuery>()
        || query.interface_major != table.header.major
        || query.min_minor > table.header.minor
        || query.min_size > table.header.size
    {
        return MEDIA_UNSUPPORTED;
    }
    unsafe { *output = table };
    STATUS_OK
}
fn factory(table: *const NativeInterface) -> Box<NativeServiceFactory> {
    Box::new(NativeServiceFactory {
        header: AbiHeader::of::<NativeServiceFactory>(),
        context: table.cast_mut().cast(),
        bind: Some(bind_fixture),
    })
}

pub struct SubmittedBundle {
    _source: Box<Producer>,
    _consumer: Box<Consumer>,
    _processor: Box<TestProcessor>,
    _encoder: Box<SubmittedEncoderFixture>,
    _capabilities: Box<TestCapabilityState>,
    _capture_api: Box<CaptureApiV1_11>,
    _consumer_api: Box<FrameResourceConsumerApiV1_1>,
    _processor_api: Box<FrameResourceProcessorApiV1_2>,
    _encoder_api: Box<EncoderProviderApi>,
    _legacy_encoder_api: Box<EncoderApi>,
    _capabilities_api: Box<FrameMetadataCapabilitiesApiV1_2>,
    pub capture: Box<NativeServiceFactory>,
    pub consumer: Box<NativeServiceFactory>,
    pub processor: Box<NativeServiceFactory>,
    pub encoder: Box<NativeServiceFactory>,
    pub capabilities: Box<NativeServiceFactory>,
    pub legacy_encoder: Box<NativeServiceFactory>,
}
unsafe extern "C" fn unsupported_encoder_submit(
    _: *mut c_void,
    _: u64,
    _: *const CpuFrame,
    _: u32,
) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn unsupported_encoder_receive(_: *mut c_void, _: u64, _: PacketSink) -> Status {
    MEDIA_PENDING
}
unsafe extern "C" fn unsupported_encoder_drain(_: *mut c_void, _: u64) -> Status {
    STATUS_OK
}
impl SubmittedBundle {
    pub fn new() -> Self {
        let mut source = Box::new(Producer::default());
        let mut consumer_state = Box::new(Consumer::default());
        let mut processor_state = Box::new(TestProcessor {
            transfer: FRAME_TRANSFER_SRGB,
            ..Default::default()
        });
        let mut encoder_state = Box::new(SubmittedEncoderFixture {
            consumer: &mut *consumer_state,
            configured: false,
            sent: false,
        });
        let mut capabilities_state = Box::new(TestCapabilityState {
            generation: 1,
            pins: 0,
            invalidate_requested: false,
            fail_stage: 0,
        });
        let capture_api = Box::new(metadata_capture_table(&mut source));
        let mut consumer_base = table(&mut consumer_state);
        consumer_base.enumerate_resources = consumer_enumerate;
        consumer_base.base.header = AbiHeader {
            size: size_of::<FrameResourceConsumerApiV1_1>() as u32,
            major: 1,
            minor: 1,
        };
        let consumer_api = Box::new(FrameResourceConsumerApiV1_1 {
            base: consumer_base,
            submit_with_metadata: consumer_metadata_submit,
        });
        let mut processor_base = processor_table(&mut processor_state);
        processor_base.base.base.header = AbiHeader {
            size: size_of::<FrameResourceProcessorApiV1_2>() as u32,
            major: 1,
            minor: 2,
        };
        let processor_api = Box::new(FrameResourceProcessorApiV1_2 {
            base: processor_base,
            process_with_metadata: processor_metadata_process,
        });
        let encoder_api = Box::new(EncoderProviderApi {
            base: NativeInterface {
                header: AbiHeader::of::<EncoderProviderApi>(),
                context: (&mut *encoder_state as *mut SubmittedEncoderFixture).cast(),
                release: Some(release_fixture),
            },
            probe: submitted_probe,
            configure_session: submitted_configure,
            receive: submitted_receive,
            control: submitted_control,
            subscribe: None,
            unsubscribe: None,
            drain: submitted_drain,
            destroy_session: submitted_destroy,
        });
        let capabilities_api = Box::new(FrameMetadataCapabilitiesApiV1_2 {
            base: FrameMetadataCapabilitiesApiV1_1 {
                base: FrameMetadataCapabilitiesApiV1 {
                    base: NativeInterface {
                        header: AbiHeader {
                            size: size_of::<FrameMetadataCapabilitiesApiV1_2>() as u32,
                            major: 1,
                            minor: 2,
                        },
                        context: (&mut *capabilities_state as *mut TestCapabilityState).cast(),
                        release: Some(release_fixture),
                    },
                    enumerate: capability_enumerate,
                },
                probe_profile: capability_probe,
            },
            pin_generation: capability_pin,
            unpin_generation: capability_unpin,
        });
        let legacy_encoder_api = Box::new(EncoderApi {
            base: NativeInterface {
                header: AbiHeader::of::<EncoderApi>(),
                context: ptr::null_mut(),
                release: Some(release_fixture),
            },
            submit: unsupported_encoder_submit,
            receive: unsupported_encoder_receive,
            drain: unsupported_encoder_drain,
        });

        let capture = factory(&capture_api.base.base.base.base.base);
        let consumer = factory(&consumer_api.base.base);
        let processor = factory(&processor_api.base.base.base);
        let encoder = factory(&encoder_api.base);
        let capabilities = factory(&capabilities_api.base.base.base);
        let legacy_encoder = factory(&legacy_encoder_api.base);
        Self {
            _source: source,
            _consumer: consumer_state,
            _processor: processor_state,
            _encoder: encoder_state,
            _capabilities: capabilities_state,
            _capture_api: capture_api,
            _consumer_api: consumer_api,
            _processor_api: processor_api,
            _encoder_api: encoder_api,
            _legacy_encoder_api: legacy_encoder_api,
            _capabilities_api: capabilities_api,
            capture,
            consumer,
            processor,
            encoder,
            capabilities,
            legacy_encoder,
        }
    }
}
