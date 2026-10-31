//! Synthetic metadata-changing provider, not an HDR algorithm.
use amaterasu_abi::*;
use amaterasu_media::*;
use std::{ffi::c_void, mem::size_of, ptr};
#[repr(C)]
struct Processor {
    api: FrameResourceProcessorApiV1_2,
    capabilities: FrameMetadataCapabilitiesApiV1,
    input: bool,
    output: bool,
    cancellations: u32,
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
fn offer(id: u64) -> CaptureResourceOffer {
    CaptureResourceOffer {
        id,
        ownership: FRAME_OWNERSHIP_RETAINABLE,
        accessibility: FRAME_ACCESS_GPU,
        reserved: 0,
        resource: descriptor(FRAME_RESOURCE_VENDOR_BASE),
        acquire_sync: descriptor(FRAME_SYNC_NONE),
        completion_sync: descriptor(FRAME_SYNC_NONE),
    }
}
unsafe extern "C" fn inputs(
    _: *mut c_void,
    _: u64,
    sink: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    unsafe {
        *generation = 4;
        (sink.write)(sink.context, &offer(8))
    }
}
unsafe extern "C" fn configure(
    ctx: *mut c_void,
    _: u64,
    generation: u64,
    id: u64,
    source_generation: u64,
    source: *const CaptureResourceOffer,
) -> Status {
    if source.is_null()
        || (generation, id, source_generation) != (4, 8, 9)
        || unsafe { (*source).id } != 5
    {
        return MEDIA_UNSUPPORTED;
    }
    unsafe {
        (*ctx.cast::<Processor>()).input = true;
    }
    STATUS_OK
}
unsafe extern "C" fn unused_submit(
    _: *mut c_void,
    _: u64,
    _: *const CaptureResourceFrame,
    _: u32,
) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn cancel(ctx: *mut c_void, _: u64) {
    unsafe {
        (*ctx.cast::<Processor>()).cancellations += 1;
    }
}
unsafe extern "C" fn outputs(
    ctx: *mut c_void,
    _: u64,
    sink: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    if !unsafe { (*ctx.cast::<Processor>()).input } {
        return MEDIA_UNSUPPORTED;
    }
    unsafe {
        *generation = 9;
        (sink.write)(sink.context, &offer(5))
    }
}
unsafe extern "C" fn select(ctx: *mut c_void, _: u64, generation: u64, id: u64) -> Status {
    if (generation, id) != (9, 5) {
        return MEDIA_UNSUPPORTED;
    }
    unsafe {
        (*ctx.cast::<Processor>()).output = true;
    }
    STATUS_OK
}
unsafe extern "C" fn process(
    ctx: *mut c_void,
    _: u64,
    input: *const CaptureResourceFrame,
    sink: CaptureResourceFrameSink,
) -> Status {
    if input.is_null() || !unsafe { (*ctx.cast::<Processor>()).output } {
        return MEDIA_UNSUPPORTED;
    }
    let mut output = unsafe { *input };
    output.offer_generation = 9;
    output.offer_id = 5;
    output.color = FrameColorMetadata {
        version: 1,
        size: size_of::<FrameColorMetadata>() as u32,
        transfer: FRAME_TRANSFER_PQ,
        ..Default::default()
    };
    unsafe { (sink.write)(sink.context, &output) }
}
unsafe extern "C" fn process_metadata(
    ctx: *mut c_void,
    _: u64,
    input: *const FrameResourceMetadataFrameV1,
    sink: FrameResourceMetadataSinkV1,
) -> Status {
    if input.is_null() || !unsafe { (*ctx.cast::<Processor>()).output } {
        return MEDIA_UNSUPPORTED;
    }
    // Frame and metadata remain borrowed through the callback; no ownership is transferred.
    let mut output = unsafe { *input };
    output.frame.offer_generation = 9;
    output.frame.offer_id = 5;
    output.frame.color = FrameColorMetadata {
        version: 1,
        size: size_of::<FrameColorMetadata>() as u32,
        transfer: FRAME_TRANSFER_PQ,
        ..Default::default()
    };
    unsafe { (sink.write)(sink.context, &output) }
}
unsafe extern "C" fn metadata_capabilities(
    _: *mut c_void,
    _: u64,
    sink: FrameMetadataCapabilitySinkV1,
    generation: *mut u64,
) -> Status {
    unsafe {
        *generation = 77;
    }
    for value in [
        FRAME_METADATA_SCHEMA_HDR10PLUS_T35,
        FRAME_METADATA_SCHEMA_HDR_VIVID_T35,
    ] {
        let capability = FrameMetadataCapabilityV1 {
            schema: FrameMetadataSchemaId {
                namespace_id: 1,
                value,
                schema: 1,
            },
            operations: FRAME_METADATA_CAP_FORWARD,
            reserved: 0,
        };
        let status = unsafe { (sink.write)(sink.context, &capability) };
        if status != STATUS_OK {
            return status;
        }
    }
    STATUS_OK
}
#[unsafe(no_mangle)]
pub extern "C" fn fixture_processor_create() -> *mut FrameResourceProcessorApi {
    let mut value = Box::new(Processor {
        api: FrameResourceProcessorApiV1_2 {
            base: FrameResourceProcessorApi {
                base: FrameResourceConsumerApi {
                    base: NativeInterface {
                        header: AbiHeader {
                            size: size_of::<FrameResourceProcessorApiV1_2>() as u32,
                            major: 1,
                            minor: 2,
                        },
                        context: ptr::null_mut(),
                        release: None,
                    },
                    enumerate_resources: inputs,
                    configure,
                    submit: unused_submit,
                    cancel,
                },
                enumerate_outputs: outputs,
                select_output: select,
                process,
            },
            process_with_metadata: process_metadata,
        },
        capabilities: FrameMetadataCapabilitiesApiV1 {
            base: NativeInterface {
                header: AbiHeader {
                    size: size_of::<FrameMetadataCapabilitiesApiV1>() as u32,
                    major: 1,
                    minor: 0,
                },
                context: ptr::null_mut(),
                release: None,
            },
            enumerate: metadata_capabilities,
        },
        input: false,
        output: false,
        cancellations: 0,
    });
    value.api.base.base.base.context = (&mut *value as *mut Processor).cast();
    value.capabilities.base.context = (&mut *value as *mut Processor).cast();
    Box::into_raw(value).cast()
}
/// # Safety
/// The pointer must be a live fixture processor returned by the constructor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_processor_capabilities(
    api: *mut FrameResourceProcessorApi,
) -> *const FrameMetadataCapabilitiesApiV1 {
    if api.is_null() {
        return ptr::null();
    }
    unsafe { &(*api.cast::<Processor>()).capabilities }
}
/// # Safety
/// The pointer must be a live fixture processor returned by the constructor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_processor_cancellations(
    api: *mut FrameResourceProcessorApi,
) -> u32 {
    unsafe { (*api.cast::<Processor>()).cancellations }
}
/// # Safety
/// The pointer must be an unfreed fixture processor with no work in flight.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_processor_destroy(api: *mut FrameResourceProcessorApi) {
    if !api.is_null() {
        drop(unsafe { Box::from_raw(api.cast::<Processor>()) });
    }
}
