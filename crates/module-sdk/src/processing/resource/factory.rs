use super::{format::*, *};
use crate::{
    ModuleResult, RuntimeClient,
    abi::*,
    media::{BorrowedCpuFrame, OwnedCpuFrame, native_contracts},
};
use amaterasu_media::*;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

/// A selected CPU implementation of the generic resource processor ABI.
/// Descriptions are bounded metadata only; keep a generation stable until its
/// formats change. Configure must reject atomically. Process performs the real
/// transform into independent output storage and preserves frame identity.
/// Cancel/shutdown retire all native work synchronously. No stock algorithm is
/// supplied, and unknown source/output color is never negotiated here.
pub trait CpuResourceProcessor: 'static {
    fn describe(&self, stream: u64) -> Result<CpuProcessingDescription, Status>;
    fn configure(
        &mut self,
        stream: u64,
        input: CpuResourceCapabilitiesV2,
        output: CpuResourceCapabilitiesV2,
    ) -> Result<(), Status>;
    fn process(
        &mut self,
        stream: u64,
        input: CpuResourceInput<'_>,
        output: CpuResourceCapabilitiesV2,
    ) -> Result<OwnedCpuFrame, Status>;
    fn cancel(&mut self, stream: u64);
    fn shutdown(&mut self);
}
#[derive(Clone, Copy)]
struct Source {
    generation: u64,
    id: u64,
    caps: CpuResourceCapabilitiesV2,
}
#[derive(Clone, Copy)]
struct State {
    generation: u64,
    source: Option<Source>,
    output: Option<CpuProcessingOffer>,
    configured: bool,
}
struct Binding<P> {
    api: FrameResourceProcessorApi,
    processor: RefCell<P>,
    states: RefCell<BTreeMap<u64, State>>,
    poisoned: Cell<bool>,
}
fn guarded<P>(binding: &Binding<P>, call: impl FnOnce() -> Status) -> Status {
    if binding.poisoned.get() {
        return STATUS_INTERNAL_ERROR;
    }
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(status) => status,
        Err(_) => {
            binding.poisoned.set(true);
            STATUS_INTERNAL_ERROR
        }
    }
}
fn description<P: CpuResourceProcessor>(
    b: &Binding<P>,
    stream: u64,
) -> Result<CpuProcessingDescription, Status> {
    if stream == 0 {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let description = b.processor.borrow().describe(stream)?;
    validate_description(&description)?;
    Ok(description)
}
fn commit<P: CpuResourceProcessor>(b: &Binding<P>, stream: u64, mut state: State) -> Status {
    if stream == 0 {
        return STATUS_INVALID_ARGUMENT;
    }
    if !b.states.borrow().contains_key(&stream) && b.states.borrow().len() >= MAX_PROCESSING_STREAMS
    {
        return MEDIA_BUSY;
    }
    if let (Some(source), Some(output)) = (state.source, state.output) {
        if let Err(status) =
            b.processor
                .borrow_mut()
                .configure(stream, source.caps, output.capabilities)
        {
            return status;
        }
        state.configured = true;
    }
    b.states.borrow_mut().insert(stream, state);
    STATUS_OK
}
unsafe extern "C" fn release<P: CpuResourceProcessor>(context: *mut c_void) {
    // SAFETY: runtime uniquely releases this provider binding after callbacks.
    let binding = unsafe { Box::from_raw(context.cast::<Binding<P>>()) };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        binding.processor.borrow_mut().shutdown()
    }));
}
unsafe extern "C" fn enumerate<P: CpuResourceProcessor, const OUTPUT: bool>(
    context: *mut c_void,
    stream: u64,
    sink: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    if context.is_null()
        || generation.is_null()
        || !(generation as usize).is_multiple_of(align_of::<u64>())
    {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the broker retains this exact binding during native callbacks.
    let b = unsafe { &*context.cast::<Binding<P>>() };
    guarded(b, || {
        let description = match description(b, stream) {
            Ok(value) => value,
            Err(status) => return status,
        };
        for offer in if OUTPUT {
            description.outputs
        } else {
            description.inputs
        } {
            let status = with_offer(offer, |offer| unsafe { (sink.write)(sink.context, offer) });
            if status != STATUS_OK {
                return status;
            }
        }
        unsafe { generation.write(description.generation) };
        STATUS_OK
    })
}
unsafe extern "C" fn configure<P: CpuResourceProcessor>(
    context: *mut c_void,
    stream: u64,
    generation: u64,
    id: u64,
    producer_generation: u64,
    producer: *const CaptureResourceOffer,
) -> Status {
    if context.is_null()
        || producer.is_null()
        || !(producer as usize).is_multiple_of(align_of::<CaptureResourceOffer>())
        || producer_generation == 0
    {
        return STATUS_INVALID_ARGUMENT;
    }
    let b = unsafe { &*context.cast::<Binding<P>>() };
    guarded(b, || {
        let d = match description(b, stream) {
            Ok(d) => d,
            Err(status) => return status,
        };
        if d.generation != generation {
            return MEDIA_UNSUPPORTED;
        }
        let Some(input) = d.inputs.iter().find(|offer| offer.id == id) else {
            return MEDIA_UNSUPPORTED;
        };
        let producer = match unsafe { read_offer(&*producer) } {
            Ok(offer) => offer,
            Err(status) => return status,
        };
        if !compatible(producer.capabilities, input.capabilities) {
            return MEDIA_UNSUPPORTED;
        }
        let mut state = b.states.borrow().get(&stream).copied().unwrap_or(State {
            generation,
            source: None,
            output: None,
            configured: false,
        });
        if state.generation != generation {
            return MEDIA_UNSUPPORTED;
        }
        state.source = Some(Source {
            generation: producer_generation,
            id: producer.id,
            caps: producer.capabilities,
        });
        commit(b, stream, state)
    })
}
unsafe extern "C" fn select<P: CpuResourceProcessor>(
    context: *mut c_void,
    stream: u64,
    generation: u64,
    id: u64,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let b = unsafe { &*context.cast::<Binding<P>>() };
    guarded(b, || {
        let d = match description(b, stream) {
            Ok(d) => d,
            Err(status) => return status,
        };
        if d.generation != generation {
            return MEDIA_UNSUPPORTED;
        }
        let Some(output) = d.outputs.iter().find(|offer| offer.id == id).copied() else {
            return MEDIA_UNSUPPORTED;
        };
        let mut state = b.states.borrow().get(&stream).copied().unwrap_or(State {
            generation,
            source: None,
            output: None,
            configured: false,
        });
        if state.generation != generation {
            return MEDIA_UNSUPPORTED;
        }
        state.output = Some(output);
        commit(b, stream, state)
    })
}
unsafe extern "C" fn unsupported(
    _: *mut c_void,
    _: u64,
    _: *const CaptureResourceFrame,
    _: u32,
) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn cancel<P: CpuResourceProcessor>(context: *mut c_void, stream: u64) {
    if context.is_null() {
        return;
    }
    let b = unsafe { &*context.cast::<Binding<P>>() };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let removed = b.states.borrow_mut().remove(&stream).is_some();
        if removed {
            b.processor.borrow_mut().cancel(stream);
        }
        STATUS_OK
    }));
    if result.is_err() {
        b.poisoned.set(true);
    }
}
unsafe extern "C" fn retain(_: *mut c_void) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn release_frame(_: *mut c_void, _: *const FrameDescriptor) {}
unsafe extern "C" fn process<P: CpuResourceProcessor>(
    context: *mut c_void,
    stream: u64,
    frame: *const CaptureResourceFrame,
    sink: CaptureResourceFrameSink,
) -> Status {
    if context.is_null()
        || frame.is_null()
        || !(frame as usize).is_multiple_of(align_of::<CaptureResourceFrame>())
    {
        return STATUS_INVALID_ARGUMENT;
    }
    let b = unsafe { &*context.cast::<Binding<P>>() };
    guarded(b, || {
        let Some(state) = b
            .states
            .borrow()
            .get(&stream)
            .copied()
            .filter(|state| state.configured)
        else {
            return MEDIA_UNSUPPORTED;
        };
        let d = match description(b, stream) {
            Ok(d) => d,
            Err(status) => return status,
        };
        if d.generation != state.generation {
            return MEDIA_UNSUPPORTED;
        }
        let source = state.source.expect("configured source");
        let output = state.output.expect("configured output");
        let frame = unsafe { &*frame };
        if frame.offer_generation != source.generation || frame.offer_id != source.id {
            return MEDIA_UNSUPPORTED;
        }
        let input = match unsafe { input(frame, source.caps) } {
            Ok(input) => input,
            Err(status) => return status,
        };
        let pixels = match b
            .processor
            .borrow_mut()
            .process(stream, input, output.capabilities)
        {
            Ok(pixels) => pixels,
            Err(status) => return status,
        };
        let borrow: BorrowedCpuFrame<'_> = pixels.borrow();
        let cpu = borrow.descriptor();
        let caps = output.capabilities.base;
        if cpu.sequence != frame.sequence
            || cpu.timestamp_us != frame.timestamp_us
            || cpu.format != caps.format
            || cpu.width < caps.min_width
            || cpu.width > caps.max_width
            || cpu.height < caps.min_height
            || cpu.height > caps.max_height
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let output_frame = CaptureResourceFrame {
            offer_generation: state.generation,
            offer_id: output.id,
            timestamp_us: frame.timestamp_us,
            capture_pacing_timestamp_us: frame.capture_pacing_timestamp_us,
            sequence: frame.sequence,
            width: cpu.width,
            height: cpu.height,
            freshness: frame.freshness,
            reserved: 0,
            color: output.capabilities.color,
            clock: frame.clock,
            pacing_clock: frame.pacing_clock,
            resource: FrameDescriptor {
                kind: FRAME_RESOURCE_CPU,
                version: 2,
                size: size_of::<CpuFrame>() as u32,
                reserved: 0,
                namespace: [0; 16],
                data: std::ptr::from_ref(cpu).cast(),
            },
            acquire_sync: FrameDescriptor {
                kind: FRAME_SYNC_NONE,
                version: 0,
                size: 0,
                reserved: 0,
                namespace: [0; 16],
                data: std::ptr::null(),
            },
            context: std::ptr::null_mut(),
            retain,
            release: release_frame,
        };
        // SAFETY: all independent output bytes live until this synchronous sink
        // returns. Provider state is not borrowed across a foreign callback.
        unsafe { (sink.write)(sink.context, &output_frame) }
    })
}
/// Transfer one implementation to a runtime-owned generic processor table.
pub fn into_resource_processor_binding<P: CpuResourceProcessor>(
    processor: P,
) -> *const FrameResourceProcessorApi {
    let mut b = Box::new(Binding {
        api: FrameResourceProcessorApi {
            base: FrameResourceConsumerApi {
                base: NativeInterface {
                    header: AbiHeader {
                        major: FRAME_RESOURCE_PROCESSOR_INTERFACE_MAJOR,
                        minor: FRAME_RESOURCE_PROCESSOR_INTERFACE_MINOR,
                        size: FRAME_RESOURCE_PROCESSOR_API_V1_0_SIZE,
                    },
                    context: std::ptr::null_mut(),
                    release: Some(release::<P>),
                },
                enumerate_resources: enumerate::<P, false>,
                configure: configure::<P>,
                submit: unsupported,
                cancel: cancel::<P>,
            },
            enumerate_outputs: enumerate::<P, true>,
            select_output: select::<P>,
            process: process::<P>,
        },
        processor: RefCell::new(processor),
        states: RefCell::new(BTreeMap::new()),
        poisoned: Cell::new(false),
    });
    b.api.base.base.context = std::ptr::from_mut(&mut *b).cast();
    let pointer = std::ptr::from_ref(&b.api);
    let _ = Box::into_raw(b);
    pointer
}
pub struct ResourceProcessorFactory<F> {
    pub api: NativeServiceFactory,
    create: F,
}
impl<F, P> ResourceProcessorFactory<F>
where
    F: Fn(&str) -> Result<P, Status> + 'static,
    P: CpuResourceProcessor,
{
    pub fn new(create: F) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind::<F, P>),
            },
            create,
        });
        factory.api.context = std::ptr::from_mut(&mut *factory).cast();
        factory
    }
    /// # Safety
    /// Retain the boxed factory through runtime consumer retirement. Native
    /// grants select trusted code; they are not a native-code sandbox.
    pub unsafe fn register(&self, runtime: &RuntimeClient, service: &str) -> ModuleResult<()> {
        unsafe {
            runtime.register_native_typed(
                service,
                native_contracts::FRAME_RESOURCE_PROCESSOR,
                &self.api,
            )
        }
    }
}
unsafe extern "C" fn bind<F, P>(
    context: *mut c_void,
    caller: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status
where
    F: Fn(&str) -> Result<P, Status> + 'static,
    P: CpuResourceProcessor,
{
    if context.is_null()
        || query.is_null()
        || !(query as usize).is_multiple_of(align_of::<NativeInterfaceQuery>())
        || output.is_null()
        || !(output as usize).is_multiple_of(align_of::<*const NativeInterface>())
        || caller.len > 128
    {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe { output.write(std::ptr::null()) };
    catch_unwind(AssertUnwindSafe(|| {
        let query = unsafe { &*query };
        if query.header.size < size_of::<NativeInterfaceQuery>() as u32
            || query.header.major != 1
            || query.interface_major != FRAME_RESOURCE_PROCESSOR_INTERFACE_MAJOR
            || query.min_minor > FRAME_RESOURCE_PROCESSOR_INTERFACE_MINOR
            || query.min_size > FRAME_RESOURCE_PROCESSOR_API_V1_0_SIZE
        {
            return MEDIA_UNSUPPORTED;
        }
        let caller = match unsafe { caller.read() }
            .ok()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
        {
            Some(caller) => caller,
            None => return STATUS_INVALID_ARGUMENT,
        };
        let factory = unsafe { &*context.cast::<ResourceProcessorFactory<F>>() };
        match (factory.create)(caller) {
            Ok(provider) => {
                unsafe { output.write(into_resource_processor_binding(provider).cast()) };
                STATUS_OK
            }
            Err(status) => status,
        }
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}
