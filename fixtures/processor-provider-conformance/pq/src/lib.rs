//! Synthetic metadata-changing provider, not an HDR algorithm.
use amaterasu_abi::*;
use amaterasu_media::*;
use std::{ffi::c_void, mem::size_of, ptr};
#[repr(C)]
struct Processor {
    api: FrameResourceProcessorApi,
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
#[unsafe(no_mangle)]
pub extern "C" fn fixture_processor_create() -> *mut FrameResourceProcessorApi {
    let mut value = Box::new(Processor {
        api: FrameResourceProcessorApi {
            base: FrameResourceConsumerApi {
                base: NativeInterface {
                    header: AbiHeader {
                        size: size_of::<FrameResourceProcessorApi>() as u32,
                        major: 1,
                        minor: 1,
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
        input: false,
        output: false,
        cancellations: 0,
    });
    value.api.base.base.context = (&mut *value as *mut Processor).cast();
    Box::into_raw(value).cast()
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
