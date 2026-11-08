use super::{MAX_PROCESSING_OFFERS, MAX_PROCESSING_STREAMS, ProcessingFrame, ResourceOutputFrame};
use crate::{ModuleResult, RuntimeClient, abi::*, media::native_contracts};
use amaterasu_media::*;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

/// Provider-owned processing behind the same resource contract for every capture
/// backend. Offers must own stable schema payloads through enumeration; configure
/// copies the negotiated information it needs. Process queues GPU dependencies,
/// never waits for GPU completion, and retains input before asynchronous access.
/// Cancel and shutdown retire all input reads and downstream-owned output leases.
pub trait FrameProcessor: 'static {
    fn configure_stream(&mut self, stream: u64, json: &[u8]) -> Result<(), Status>;
    fn update_stream(&mut self, stream: u64, json: &[u8]) -> Result<(), Status>;
    fn input_offers(&mut self, stream: u64) -> Result<(u64, Vec<CaptureResourceOffer>), Status>;
    fn configure_input(
        &mut self,
        stream: u64,
        generation: u64,
        offer: u64,
        source_generation: u64,
        source: &CaptureResourceOffer,
    ) -> Result<(), Status>;
    fn output_offers(&mut self, stream: u64) -> Result<(u64, Vec<CaptureResourceOffer>), Status>;
    fn select_output(&mut self, stream: u64, generation: u64, offer: u64) -> Result<(), Status>;
    fn process(
        &mut self,
        stream: u64,
        input: ProcessingFrame<'_>,
    ) -> Result<ResourceOutputFrame, Status>;
    fn hdr_metadata(&self, stream: u64) -> Option<HdrMetadata>;
    fn cancel(&mut self, stream: u64);
    fn shutdown(&mut self);
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Schema {
    namespace: [u8; 16],
    kind: u32,
    version: u32,
}
impl From<FrameDescriptor> for Schema {
    fn from(d: FrameDescriptor) -> Self {
        Self {
            namespace: d.namespace,
            kind: d.kind,
            version: d.version,
        }
    }
}
#[derive(Clone, Copy)]
struct InputSelection {
    generation: u64,
    offer: u64,
    resource: Schema,
    acquire: Schema,
}
#[derive(Default)]
struct Stream {
    configured: bool,
    input: Option<InputSelection>,
    output: Option<(u64, u64)>,
}
#[repr(C)]
struct Binding<P> {
    api: FrameProcessorApi,
    processor: RefCell<P>,
    streams: RefCell<BTreeMap<u64, Stream>>,
    poisoned: Cell<bool>,
    retired: RefCell<BTreeSet<u64>>,
}
fn guarded<P>(b: &Binding<P>, call: impl FnOnce() -> Status) -> Status {
    if b.poisoned.get() {
        return STATUS_INTERNAL_ERROR;
    }
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(s) => s,
        Err(_) => {
            b.poisoned.set(true);
            STATUS_INTERNAL_ERROR
        }
    }
}
fn status(result: Result<(), Status>) -> Status {
    match result {
        Ok(()) => STATUS_OK,
        Err(STATUS_OK) => STATUS_INTERNAL_ERROR,
        Err(s) => s,
    }
}
fn active<P>(b: &Binding<P>, stream: u64) -> bool {
    !b.retired.borrow().contains(&stream)
        && b.streams
            .borrow()
            .get(&stream)
            .is_some_and(|s| s.configured)
}
unsafe extern "C" fn configure<P: FrameProcessor, const UPDATE: bool>(
    context: *mut c_void,
    stream: u64,
    json: *const u8,
    len: usize,
) -> Status {
    if context.is_null()
        || stream == 0
        || json.is_null()
        || len == 0
        || len > FRAME_PROCESSOR_MAX_CONFIGURATION_BYTES
    {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: native caller lends these bytes through this synchronous call.
    let json = unsafe { std::slice::from_raw_parts(json, len) };
    if serde_json::from_slice::<serde_json::Value>(json).is_err() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the runtime retains this exact typed binding through the call.
    let b = unsafe { &*context.cast::<Binding<P>>() };
    guarded(b, || {
        if b.retired.borrow().contains(&stream) {
            return MEDIA_CLOSED;
        }
        if UPDATE && !active(b, stream) || !UPDATE && active(b, stream) {
            return STATUS_INVALID_ARGUMENT;
        }
        if !b.streams.borrow().contains_key(&stream)
            && (b.streams.borrow().len() >= MAX_PROCESSING_STREAMS
                || b.streams.borrow().len() + b.retired.borrow().len() >= 4096)
        {
            return MEDIA_BUSY;
        }
        let Ok(mut p) = b.processor.try_borrow_mut() else {
            return MEDIA_BUSY;
        };
        let result = if UPDATE {
            p.update_stream(stream, json)
        } else {
            p.configure_stream(stream, json)
        };
        if result.is_ok() {
            b.streams.borrow_mut().entry(stream).or_default().configured = true;
        }
        status(result)
    })
}
unsafe extern "C" fn enumerate<P: FrameProcessor, const OUTPUT: bool>(
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
    // SAFETY: runtime retains this SDK-owned typed context during callbacks.
    let b = unsafe { &*context.cast::<Binding<P>>() };
    guarded(b, || {
        if !active(b, stream) {
            return MEDIA_CLOSED;
        }
        let offers = {
            let Ok(mut p) = b.processor.try_borrow_mut() else {
                return MEDIA_BUSY;
            };
            if OUTPUT {
                p.output_offers(stream)
            } else {
                p.input_offers(stream)
            }
        };
        let (version, offers) = match offers {
            Ok(v) => v,
            Err(s) => return s,
        };
        if version == 0 || offers.len() > MAX_PROCESSING_OFFERS {
            return STATUS_INTERNAL_ERROR;
        }
        let mut ids = BTreeSet::new();
        for offer in &offers {
            if offer.id == 0 || offer.reserved != 0 || !ids.insert(offer.id) {
                return STATUS_INTERNAL_ERROR;
            }
        }
        for offer in &offers {
            // SAFETY: provider owns the offer payloads for enumeration; no
            // provider borrow is held while invoking the consumer callback.
            let result = unsafe { (sink.write)(sink.context, offer) };
            if result != STATUS_OK {
                return result;
            }
        }
        unsafe { generation.write(version) };
        STATUS_OK
    })
}
unsafe extern "C" fn configure_input<P: FrameProcessor>(
    context: *mut c_void,
    stream: u64,
    generation: u64,
    offer: u64,
    source_generation: u64,
    source: *const CaptureResourceOffer,
) -> Status {
    if context.is_null()
        || source.is_null()
        || !(source as usize).is_multiple_of(align_of::<CaptureResourceOffer>())
        || generation == 0
        || offer == 0
        || source_generation == 0
    {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: all native arguments are borrowed for the duration of this call.
    let (b, source) = unsafe { (&*context.cast::<Binding<P>>(), &*source) };
    guarded(b, || {
        if !active(b, stream) {
            return MEDIA_CLOSED;
        }
        if source.id == 0 || source.reserved != 0 {
            return STATUS_INVALID_ARGUMENT;
        }
        let Ok(mut p) = b.processor.try_borrow_mut() else {
            return MEDIA_BUSY;
        };
        let result = p.configure_input(stream, generation, offer, source_generation, source);
        if result.is_ok() {
            b.streams
                .borrow_mut()
                .get_mut(&stream)
                .expect("active")
                .input = Some(InputSelection {
                generation: source_generation,
                offer: source.id,
                resource: source.resource.into(),
                acquire: source.acquire_sync.into(),
            });
        }
        status(result)
    })
}
unsafe extern "C" fn select_output<P: FrameProcessor>(
    context: *mut c_void,
    stream: u64,
    generation: u64,
    offer: u64,
) -> Status {
    if context.is_null() || generation == 0 || offer == 0 {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: runtime keeps the typed binding live during selection.
    let b = unsafe { &*context.cast::<Binding<P>>() };
    guarded(b, || {
        if !active(b, stream) {
            return MEDIA_CLOSED;
        }
        let Ok(mut p) = b.processor.try_borrow_mut() else {
            return MEDIA_BUSY;
        };
        let result = p.select_output(stream, generation, offer);
        if result.is_ok() {
            b.streams
                .borrow_mut()
                .get_mut(&stream)
                .expect("active")
                .output = Some((generation, offer));
        }
        status(result)
    })
}
unsafe extern "C" fn process<P: FrameProcessor>(
    context: *mut c_void,
    stream: u64,
    input: *const CaptureResourceFrame,
    sink: CaptureResourceFrameSink,
) -> Status {
    if context.is_null()
        || input.is_null()
        || !(input as usize).is_multiple_of(align_of::<CaptureResourceFrame>())
    {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: native caller lends the complete descriptor and typed context.
    let (b, input) = unsafe { (&*context.cast::<Binding<P>>(), &*input) };
    guarded(b, || {
        if b.retired.borrow().contains(&stream) {
            return MEDIA_CLOSED;
        }
        let (selection, output) = match b.streams.borrow().get(&stream) {
            Some(s) if s.configured && s.input.is_some() && s.output.is_some() => {
                (s.input.unwrap(), s.output.unwrap())
            }
            _ => return MEDIA_UNSUPPORTED,
        };
        if input.offer_generation != selection.generation
            || input.offer_id != selection.offer
            || Schema::from(input.resource) != selection.resource
            || Schema::from(input.acquire_sync) != selection.acquire
            || input.reserved != 0
            || !matches!(
                input.freshness,
                CAPTURE_FRAME_FRESH | CAPTURE_FRAME_CACHED_REUSE
            )
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let result = {
            let Ok(mut p) = b.processor.try_borrow_mut() else {
                return MEDIA_BUSY;
            };
            p.process(stream, ProcessingFrame { frame: input })
        };
        let owned = match result {
            Ok(v) => v,
            Err(STATUS_OK) => return STATUS_INTERNAL_ERROR,
            Err(s) => return s,
        };
        let frame = owned.frame();
        if frame.offer_generation != output.0
            || frame.offer_id != output.1
            || frame.reserved != 0
            || frame.sequence != input.sequence
            || frame.timestamp_us != input.timestamp_us
            || frame.capture_pacing_timestamp_us != input.capture_pacing_timestamp_us
            || frame.freshness != input.freshness
            || !super::session::same_clock(frame.clock, input.clock)
            || !super::session::same_clock(frame.pacing_clock, input.pacing_clock)
        {
            return STATUS_INTERNAL_ERROR;
        }
        // SAFETY: output owns its descriptor and payloads through this call.
        // The sink explicitly retains any asynchronous reference it needs.
        unsafe { (sink.write)(sink.context, frame) }
    })
}
unsafe extern "C" fn submit(
    _: *mut c_void,
    _: u64,
    _: *const CaptureResourceFrame,
    _: u32,
) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn metadata<P: FrameProcessor>(
    context: *mut c_void,
    stream: u64,
    output: *mut HdrMetadata,
) -> Status {
    if context.is_null()
        || output.is_null()
        || !(output as usize).is_multiple_of(align_of::<HdrMetadata>())
    {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the typed context and writable result are live for this call.
    let b = unsafe { &*context.cast::<Binding<P>>() };
    guarded(b, || {
        if !active(b, stream) {
            return MEDIA_CLOSED;
        }
        let Ok(p) = b.processor.try_borrow() else {
            return MEDIA_BUSY;
        };
        match p.hdr_metadata(stream) {
            Some(m) if m.enabled <= 1 && m.reserved == 0 => {
                unsafe { output.write(m) };
                STATUS_OK
            }
            Some(_) => STATUS_INTERNAL_ERROR,
            None => MEDIA_UNSUPPORTED,
        }
    })
}
unsafe extern "C" fn cancel<P: FrameProcessor>(context: *mut c_void, stream: u64) {
    // SAFETY: runtime retains the exact binding until this barrier returns.
    let b = unsafe { &*context.cast::<Binding<P>>() };
    if !b.streams.borrow().contains_key(&stream) {
        return;
    }
    if catch_unwind(AssertUnwindSafe(|| b.processor.borrow_mut().cancel(stream))).is_err() {
        std::process::abort();
    }
    b.streams.borrow_mut().remove(&stream);
    // Admission bounds active plus retired stream IDs before reserving state.
    b.retired.borrow_mut().insert(stream);
}
unsafe extern "C" fn release<P: FrameProcessor>(context: *mut c_void) {
    // SAFETY: runtime transfers the unique binding after stopping callbacks.
    let b = unsafe { Box::from_raw(context.cast::<Binding<P>>()) };
    if catch_unwind(AssertUnwindSafe(|| b.processor.borrow_mut().shutdown())).is_err() {
        std::process::abort();
    }
}
pub fn into_frame_processor_binding<P: FrameProcessor>(processor: P) -> *const FrameProcessorApi {
    let mut b = Box::new(Binding {
        api: FrameProcessorApi {
            base: FrameResourceProcessorApi {
                base: FrameResourceConsumerApi {
                    base: NativeInterface {
                        header: AbiHeader {
                            major: FRAME_PROCESSOR_INTERFACE_MAJOR,
                            minor: FRAME_PROCESSOR_INTERFACE_MINOR,
                            size: FRAME_PROCESSOR_API_SIZE,
                        },
                        context: std::ptr::null_mut(),
                        release: Some(release::<P>),
                    },
                    enumerate_resources: enumerate::<P, false>,
                    configure: configure_input::<P>,
                    submit,
                    cancel: cancel::<P>,
                },
                enumerate_outputs: enumerate::<P, true>,
                select_output: select_output::<P>,
                process: process::<P>,
            },
            configure_stream: configure::<P, false>,
            update_stream: configure::<P, true>,
            hdr_metadata: metadata::<P>,
        },
        processor: RefCell::new(processor),
        streams: RefCell::new(BTreeMap::new()),
        poisoned: Cell::new(false),
        retired: RefCell::new(BTreeSet::new()),
    });
    b.api.base.base.base.context = std::ptr::from_mut(&mut *b).cast();
    Box::into_raw(b).cast()
}
pub struct FrameProcessorFactory<F> {
    pub api: NativeServiceFactory,
    create: F,
}
impl<F, P> FrameProcessorFactory<F>
where
    F: Fn(&str) -> Result<P, Status> + Send + Sync + 'static,
    P: FrameProcessor,
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
    /// Retain this boxed factory through all bindings and native retirement.
    pub unsafe fn register(&self, runtime: &RuntimeClient, service: &str) -> ModuleResult<()> {
        unsafe {
            runtime.register_native_typed(service, native_contracts::FRAME_PROCESSOR, &self.api)
        }
    }
}
unsafe extern "C" fn bind<F, P>(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status
where
    F: Fn(&str) -> Result<P, Status> + Send + Sync + 'static,
    P: FrameProcessor,
{
    if output.is_null() || !(output as usize).is_multiple_of(align_of::<*const NativeInterface>()) {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe { output.write(std::ptr::null()) };
    if context.is_null()
        || query.is_null()
        || !(query as usize).is_multiple_of(align_of::<NativeInterfaceQuery>())
    {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: runtime owns the query, authenticated principal and factory.
    let query = unsafe { &*query };
    if !query.header.supports::<NativeInterfaceQuery>()
        || query.interface_major != FRAME_PROCESSOR_INTERFACE_MAJOR
        || query.min_minor > FRAME_PROCESSOR_INTERFACE_MINOR
        || query.min_size > FRAME_PROCESSOR_API_SIZE
    {
        return MEDIA_UNSUPPORTED;
    }
    let Some(principal) = (unsafe { principal.read() })
        .ok()
        .and_then(|p| std::str::from_utf8(p).ok())
        .filter(|p| !p.is_empty() && p.len() <= 128 && !p.chars().any(char::is_control))
    else {
        return STATUS_INVALID_ARGUMENT;
    };
    let factory = unsafe { &*context.cast::<FrameProcessorFactory<F>>() };
    match catch_unwind(AssertUnwindSafe(|| (factory.create)(principal))) {
        Ok(Ok(p)) => {
            unsafe { output.write(into_frame_processor_binding(p).cast()) };
            STATUS_OK
        }
        Ok(Err(STATUS_OK)) | Err(_) => STATUS_INTERNAL_ERROR,
        Ok(Err(s)) => s,
    }
}
