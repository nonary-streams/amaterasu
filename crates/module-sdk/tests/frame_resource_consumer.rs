//! A synthetic consumer contract test. It does not exercise a real encoder or GPU.
use amaterasu_module_sdk::{
    abi::{AbiHeader, NativeInterface, STATUS_OK, Status},
    media_abi::{
        CaptureResourceFrame, CaptureResourceOffer, CaptureResourceOfferSink, CpuFrame,
        CpuResourceCapabilities, FRAME_ACCESS_CPU, FRAME_ACCESS_GPU, FRAME_OWNERSHIP_RETAINABLE,
        FRAME_RESOURCE_CPU, FRAME_RESOURCE_VENDOR_BASE, FRAME_SYNC_NONE, FrameDescriptor,
        FrameResourceConsumerApi, MEDIA_BUSY, MEDIA_UNSUPPORTED, PIXEL_BGRA8,
    },
};
use std::{ffi::c_void, mem::size_of, ptr};

const GENERATION: u64 = 7;
const STREAM: u64 = 3;
const VENDOR: [u8; 16] = [4; 16];
const CUSTOM_KIND: u32 = FRAME_RESOURCE_VENDOR_BASE + 1;

fn none() -> FrameDescriptor {
    FrameDescriptor {
        kind: FRAME_SYNC_NONE,
        version: 0,
        size: 0,
        reserved: 0,
        namespace: [0; 16],
        data: ptr::null(),
    }
}
fn resource<T>(kind: u32, namespace: [u8; 16], data: &T) -> FrameDescriptor {
    FrameDescriptor {
        kind,
        version: 1,
        size: size_of::<T>() as u32,
        reserved: 0,
        namespace,
        data: (data as *const T).cast(),
    }
}
fn offer(id: u64, descriptor: FrameDescriptor) -> CaptureResourceOffer {
    CaptureResourceOffer {
        id,
        ownership: FRAME_OWNERSHIP_RETAINABLE,
        accessibility: if descriptor.kind == FRAME_RESOURCE_CPU {
            FRAME_ACCESS_CPU
        } else {
            FRAME_ACCESS_GPU
        },
        reserved: 0,
        resource: descriptor,
        acquire_sync: none(),
        completion_sync: none(),
    }
}
#[repr(C)]
struct CustomCapability {
    device: u64,
}

struct Consumer {
    cpu: CpuResourceCapabilities,
    custom: CustomCapability,
    selected: Option<(u64, u64, u64)>,
    pending: Option<CaptureResourceFrame>,
}

unsafe extern "C" fn enumerate(
    context: *mut c_void,
    stream: u64,
    sink: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    if stream != STREAM || generation.is_null() {
        return MEDIA_UNSUPPORTED;
    }
    // SAFETY: test binds context and generation for the synchronous call.
    let consumer = unsafe { &*context.cast::<Consumer>() };
    for offered in [
        offer(11, resource(FRAME_RESOURCE_CPU, [0; 16], &consumer.cpu)),
        offer(12, resource(CUSTOM_KIND, VENDOR, &consumer.custom)),
    ] {
        // SAFETY: sink and offer remain live for this callback.
        let status = unsafe { (sink.write)(sink.context, &offered) };
        if status != STATUS_OK {
            return status;
        }
    }
    // SAFETY: generation was checked and is valid for the call.
    unsafe {
        *generation = GENERATION;
    }
    STATUS_OK
}

unsafe extern "C" fn configure(
    context: *mut c_void,
    stream: u64,
    consumer_generation: u64,
    consumer_id: u64,
    producer_generation: u64,
    producer: *const CaptureResourceOffer,
) -> Status {
    if stream != STREAM
        || consumer_generation != GENERATION
        || producer_generation == 0
        || producer.is_null()
    {
        return MEDIA_UNSUPPORTED;
    }
    // SAFETY: test supplies live consumer and offer for this call.
    let (consumer, producer) = unsafe { (&mut *context.cast::<Consumer>(), &*producer) };
    let descriptor = producer.resource;
    let compatible = producer.ownership == FRAME_OWNERSHIP_RETAINABLE
        && producer.acquire_sync.kind == FRAME_SYNC_NONE
        && producer.completion_sync.kind == FRAME_SYNC_NONE
        && descriptor.version == 1
        && !descriptor.data.is_null()
        && match consumer_id {
            11 if producer.accessibility & FRAME_ACCESS_CPU != 0
                && descriptor.kind == FRAME_RESOURCE_CPU
                && descriptor.namespace == [0; 16]
                && descriptor.size as usize == size_of::<CpuResourceCapabilities>() =>
            {
                // SAFETY: exact CPU schema and size are checked above; this trusted test producer supplies a valid payload.
                let caps = unsafe { &*descriptor.data.cast::<CpuResourceCapabilities>() };
                caps.format == consumer.cpu.format
                    && caps.min_width <= 2
                    && caps.max_width >= 2
                    && caps.min_height <= 2
                    && caps.max_height >= 2
            }
            12 if producer.accessibility & FRAME_ACCESS_GPU != 0
                && descriptor.kind == CUSTOM_KIND
                && descriptor.namespace == VENDOR
                && descriptor.size as usize == size_of::<CustomCapability>() =>
            {
                // SAFETY: the agreed vendor schema and size were checked above.
                unsafe {
                    (*descriptor.data.cast::<CustomCapability>()).device == consumer.custom.device
                }
            }
            _ => false,
        };
    if !compatible {
        return MEDIA_UNSUPPORTED;
    }
    consumer.selected = Some((producer_generation, producer.id, consumer_id));
    STATUS_OK
}

unsafe extern "C" fn submit(
    context: *mut c_void,
    stream: u64,
    frame: *const CaptureResourceFrame,
    _: u32,
) -> Status {
    if stream != STREAM || frame.is_null() {
        return MEDIA_UNSUPPORTED;
    }
    // SAFETY: test supplies live pointers for the synchronous call.
    let (consumer, frame) = unsafe { (&mut *context.cast::<Consumer>(), &*frame) };
    let Some((generation, id, selected)) = consumer.selected else {
        return MEDIA_UNSUPPORTED;
    };
    if generation != frame.offer_generation
        || id != frame.offer_id
        || frame.resource.version != 1
        || !((selected == 11
            && frame.resource.kind == FRAME_RESOURCE_CPU
            && frame.resource.namespace == [0; 16])
            || (selected == 12
                && frame.resource.kind == CUSTOM_KIND
                && frame.resource.namespace == VENDOR))
    {
        return MEDIA_UNSUPPORTED;
    }
    if consumer.pending.is_some() {
        return MEDIA_BUSY;
    }
    // SAFETY: provider's retain callback and context are live during submission.
    let status = unsafe { (frame.retain)(frame.context) };
    if status != STATUS_OK {
        return status;
    }
    consumer.pending = Some(*frame);
    STATUS_OK
}

unsafe extern "C" fn cancel(context: *mut c_void, _: u64) {
    // SAFETY: consumer context is live until cancellation completes.
    let consumer = unsafe { &mut *context.cast::<Consumer>() };
    if let Some(frame) = consumer.pending.take() {
        // All simulated work is complete before releasing with NONE.
        // SAFETY: this matches the successful retain in submit.
        unsafe {
            (frame.release)(frame.context, &none());
        }
    }
}

struct Owner {
    refs: u32,
    releases: u32,
    reject: bool,
}
unsafe extern "C" fn retain(context: *mut c_void) -> Status {
    // SAFETY: owner outlives the simulated consumer's retained reference.
    let owner = unsafe { &mut *context.cast::<Owner>() };
    if owner.reject {
        return MEDIA_BUSY;
    }
    owner.refs += 1;
    STATUS_OK
}
unsafe extern "C" fn release(context: *mut c_void, _: *const FrameDescriptor) {
    // SAFETY: owner remains live and each release matches a retained reference.
    let owner = unsafe { &mut *context.cast::<Owner>() };
    owner.refs -= 1;
    owner.releases += 1;
}
unsafe extern "C" fn count_offer(context: *mut c_void, _: *const CaptureResourceOffer) -> Status {
    // SAFETY: the test passes a live counter as context.
    unsafe {
        *context.cast::<u32>() += 1;
    }
    STATUS_OK
}

#[test]
fn negotiated_consumer_retains_cpu_and_vendor_resources_without_copying() {
    let caps = CpuResourceCapabilities {
        format: PIXEL_BGRA8,
        min_width: 2,
        max_width: 2,
        min_height: 2,
        max_height: 2,
    };
    let mut consumer = Consumer {
        cpu: caps,
        custom: CustomCapability { device: 9 },
        selected: None,
        pending: None,
    };
    let api = FrameResourceConsumerApi {
        base: NativeInterface {
            header: AbiHeader::of::<FrameResourceConsumerApi>(),
            context: (&mut consumer as *mut Consumer).cast(),
            release: None,
        },
        enumerate_resources: enumerate,
        configure,
        submit,
        cancel,
    };
    let mut count = 0_u32;
    let mut generation = 0;
    let sink = CaptureResourceOfferSink {
        context: (&mut count as *mut u32).cast(),
        write: count_offer,
    };
    // SAFETY: all pointers and callback contexts live through each synchronous call; cancellation retires retained references.
    unsafe {
        assert_eq!(
            (api.enumerate_resources)(api.base.context, STREAM, sink, &mut generation),
            STATUS_OK
        );
        assert_eq!((generation, count), (GENERATION, 2));
        let producer = offer(42, resource(FRAME_RESOURCE_CPU, [0; 16], &caps));
        assert_eq!(
            (api.configure)(api.base.context, STREAM, generation, 11, 100, &producer),
            STATUS_OK
        );
        assert_eq!(
            (api.configure)(api.base.context, STREAM, generation - 1, 11, 100, &producer),
            MEDIA_UNSUPPORTED
        );
        let mut wrong = producer;
        wrong.resource.version = 2;
        assert_eq!(
            (api.configure)(api.base.context, STREAM, generation, 11, 100, &wrong),
            MEDIA_UNSUPPORTED
        );
        wrong = producer;
        wrong.resource.namespace = VENDOR;
        assert_eq!(
            (api.configure)(api.base.context, STREAM, generation, 11, 100, &wrong),
            MEDIA_UNSUPPORTED
        );
        let pixels = [91_u8; 16];
        let cpu = CpuFrame {
            width: 2,
            height: 2,
            format: PIXEL_BGRA8,
            stride: 8,
            data: pixels.as_ptr(),
            len: pixels.len(),
            timestamp_us: 10,
            sequence: 1,
        };
        let mut owner = Owner {
            refs: 0,
            releases: 0,
            reject: true,
        };
        let mut frame = CaptureResourceFrame {
            offer_generation: 100,
            offer_id: 42,
            timestamp_us: 10,
            capture_pacing_timestamp_us: 10,
            sequence: 1,
            width: 2,
            height: 2,
            freshness: 0,
            reserved: 0,
            color: Default::default(),
            clock: Default::default(),
            pacing_clock: Default::default(),
            resource: resource(FRAME_RESOURCE_CPU, [0; 16], &cpu),
            acquire_sync: none(),
            context: (&mut owner as *mut Owner).cast(),
            retain,
            release,
        };
        frame.offer_generation = 99;
        assert_eq!(
            (api.submit)(api.base.context, STREAM, &frame, 0),
            MEDIA_UNSUPPORTED
        );
        frame.offer_generation = 100;
        assert_eq!(
            (api.submit)(api.base.context, STREAM, &frame, 0),
            MEDIA_BUSY
        );
        assert_eq!(owner.refs, 0);
        owner.reject = false;
        assert_eq!((api.submit)(api.base.context, STREAM, &frame, 0), STATUS_OK);
        assert_eq!((owner.refs, owner.releases), (1, 0));
        assert_eq!(
            (api.submit)(api.base.context, STREAM, &frame, 0),
            MEDIA_BUSY
        );
        let saved = consumer.pending.unwrap();
        assert_eq!(
            (*saved.resource.data.cast::<CpuFrame>()).data,
            pixels.as_ptr()
        );
        assert_eq!(*(*saved.resource.data.cast::<CpuFrame>()).data, 91);
        (api.cancel)(api.base.context, STREAM);
        assert_eq!((owner.refs, owner.releases), (0, 1));
        let custom = CustomCapability { device: 9 };
        let custom_offer = offer(43, resource(CUSTOM_KIND, VENDOR, &custom));
        assert_eq!(
            (api.configure)(api.base.context, STREAM, generation, 12, 101, &custom_offer),
            STATUS_OK
        );
        let incompatible = CustomCapability { device: 10 };
        let bad_offer = offer(43, resource(CUSTOM_KIND, VENDOR, &incompatible));
        assert_eq!(
            (api.configure)(api.base.context, STREAM, generation, 12, 101, &bad_offer),
            MEDIA_UNSUPPORTED
        );
        frame.offer_generation = 101;
        frame.offer_id = 43;
        frame.resource = resource(CUSTOM_KIND, VENDOR, &custom);
        assert_eq!((api.submit)(api.base.context, STREAM, &frame, 0), STATUS_OK);
        assert_eq!(
            consumer.pending.unwrap().resource.data,
            (&custom as *const CustomCapability).cast()
        );
        (api.cancel)(api.base.context, STREAM);
        assert_eq!((owner.refs, owner.releases), (0, 2));
    }
}
