use super::*;
use crate::abi::*;
use amaterasu_media::*;
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    ptr,
    rc::Rc,
};
const NAMESPACE: [u8; 16] = [19; 16];
const NONE: FrameDescriptor = FrameDescriptor {
    kind: FRAME_SYNC_NONE,
    version: 0,
    size: 0,
    reserved: 0,
    namespace: [0; 16],
    data: ptr::null(),
};
fn schema(kind: u32, data: *const c_void) -> FrameDescriptor {
    FrameDescriptor {
        kind,
        version: 1,
        size: 8,
        reserved: 0,
        namespace: NAMESPACE,
        data,
    }
}
fn offer(id: u64, kind: u32) -> CaptureResourceOffer {
    CaptureResourceOffer {
        id,
        ownership: FRAME_OWNERSHIP_RETAINABLE,
        accessibility: FRAME_ACCESS_GPU,
        reserved: 0,
        resource: schema(kind, ptr::null()),
        acquire_sync: schema(FRAME_SYNC_VENDOR_BASE + 7, ptr::null()),
        completion_sync: schema(FRAME_SYNC_VENDOR_BASE + 7, ptr::null()),
    }
}
struct Source {
    kind: u32,
    generation: u64,
    selected: bool,
    sequence: u64,
    handle: u64,
    ready: Cell<bool>,
    references: Cell<usize>,
    pending: RefCell<Vec<*const Cell<bool>>>,
    released: Cell<usize>,
}
impl Source {
    fn new(kind: u32) -> Box<Self> {
        Box::new(Self {
            kind,
            generation: 3,
            selected: false,
            sequence: 41,
            handle: 0x01234567,
            ready: Cell::new(false),
            references: Cell::new(0),
            pending: RefCell::new(Vec::new()),
            released: Cell::new(0),
        })
    }
    fn poll(&self) {
        self.pending.borrow_mut().retain(|ready| {
            if unsafe { &**ready }.get() {
                self.references.set(self.references.get() - 1);
                false
            } else {
                true
            }
        });
    }
}
unsafe extern "C" fn retain_source(context: *mut c_void) -> Status {
    let s = unsafe { &*context.cast::<Source>() };
    s.references.set(s.references.get() + 1);
    STATUS_OK
}
unsafe extern "C" fn release_source(context: *mut c_void, completion: *const FrameDescriptor) {
    let s = unsafe { &*context.cast::<Source>() };
    let completion = unsafe { &*completion };
    s.released.set(s.released.get() + 1);
    if completion.kind == FRAME_SYNC_NONE {
        s.references.set(s.references.get() - 1);
    } else {
        assert_eq!(completion.kind, FRAME_SYNC_VENDOR_BASE + 7);
        s.pending.borrow_mut().push(completion.data.cast());
    }
}
unsafe extern "C" fn enumerate_source(
    context: *mut c_void,
    _: u64,
    sink: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    let s = unsafe { &*context.cast::<Source>() };
    let status = unsafe { (sink.write)(sink.context, &offer(4, s.kind)) };
    unsafe { generation.write(s.generation) };
    status
}
unsafe extern "C" fn select_source(
    context: *mut c_void,
    _: u64,
    generation: u64,
    id: u64,
) -> Status {
    let s = unsafe { &mut *context.cast::<Source>() };
    if (generation, id) != (s.generation, 4) {
        return MEDIA_UNSUPPORTED;
    }
    s.selected = true;
    STATUS_OK
}
unsafe extern "C" fn read_source(
    context: *mut c_void,
    _: u64,
    sink: CaptureResourceFrameSink,
) -> Status {
    let s = unsafe { &*context.cast::<Source>() };
    if !s.selected {
        return MEDIA_UNSUPPORTED;
    }
    let frame = CaptureResourceFrame {
        offer_generation: s.generation,
        offer_id: 4,
        timestamp_us: 123,
        capture_pacing_timestamp_us: 120,
        sequence: s.sequence,
        width: 2,
        height: 2,
        freshness: CAPTURE_FRAME_FRESH,
        reserved: 0,
        color: Default::default(),
        clock: Default::default(),
        pacing_clock: Default::default(),
        resource: schema(s.kind, ptr::from_ref(&s.handle).cast()),
        acquire_sync: schema(FRAME_SYNC_VENDOR_BASE + 7, ptr::from_ref(&s.ready).cast()),
        context,
        retain: retain_source,
        release: release_source,
    };
    unsafe { (sink.write)(sink.context, &frame) }
}
unsafe extern "C" fn cpu(_: *mut c_void, _: u64, _: FrameSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn geometry(_: *mut c_void, _: u64, _: *mut CaptureGeometry) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn fresh(_: *mut c_void, _: u64, _: FreshFrameSinkV1_2) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn timed(_: *mut c_void, _: u64, _: FreshFrameSinkV1_3) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn dmabuf(_: *mut c_void, _: u64, _: DmaBufFrameSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn presentation(_: *mut c_void, _: u64, _: *mut PresentationStatus) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn leased(_: *mut c_void, _: u64, _: DmaBufLeaseSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn subscribe(_: *mut c_void, _: u64, _: CaptureEventSink) -> Status {
    STATUS_OK
}
unsafe extern "C" fn unsubscribe(_: *mut c_void, _: u64) {}
unsafe extern "C" fn resolve(_: *mut c_void, _: u64, _: u64, _: u32, _: BorrowedBytes) -> Status {
    MEDIA_UNSUPPORTED
}
fn capture(source: &mut Source) -> CaptureApiV1_8 {
    CaptureApiV1_8 {
        base: CaptureApiV1_7 {
            base: CaptureApiV1_6 {
                base: NativeInterface {
                    header: AbiHeader {
                        major: 1,
                        minor: 8,
                        size: CAPTURE_API_V1_8_SIZE,
                    },
                    context: ptr::from_mut(source).cast(),
                    release: None,
                },
                read_frame: cpu,
                geometry,
                read_frame_with_freshness: fresh,
                read_frame_with_capture_timestamp: timed,
                read_dmabuf_frame: dmabuf,
                latest_presentation: presentation,
                read_dmabuf_frame_leased: leased,
            },
            subscribe_events: subscribe,
            unsubscribe_events: unsubscribe,
            resolve_swap: resolve,
        },
        enumerate_resources: enumerate_source,
        select_resource: select_source,
        read_resource_frame: read_source,
    }
}
struct Output {
    input: RetainedProcessingInput,
    frame: CaptureResourceFrame,
}
unsafe extern "C" fn retain_output(context: *mut c_void) -> Status {
    // Each output reference owns its own input reference, so completion objects
    // from multiple readers cannot be lost when references retire out of order.
    let output = unsafe { &*context.cast::<Output>() };
    let input = output.input.descriptor();
    let status = unsafe { (input.retain)(input.context) };
    if status == STATUS_OK {
        unsafe { Rc::increment_strong_count(context.cast::<Output>()) };
    }
    status
}
unsafe extern "C" fn release_output(context: *mut c_void, completion: *const FrameDescriptor) {
    let output = unsafe { Rc::from_raw(context.cast::<Output>()) };
    let input = output.input.descriptor();
    unsafe { (input.release)(input.context, completion) };
}
struct Effect {
    kind: Cell<u32>,
    output_kind: u32,
    name: &'static str,
    observers: Option<Rc<RefCell<Vec<RetainedProcessingInput>>>>,
    order: Rc<RefCell<Vec<&'static str>>>,
}
impl FrameProcessor for Effect {
    fn configure_stream(&mut self, _: u64, _: &[u8]) -> Result<(), Status> {
        Ok(())
    }
    fn update_stream(&mut self, _: u64, _: &[u8]) -> Result<(), Status> {
        Err(MEDIA_UNSUPPORTED)
    }
    fn input_offers(&mut self, _: u64) -> Result<(u64, Vec<CaptureResourceOffer>), Status> {
        Ok((
            5,
            vec![
                offer(1, FRAME_RESOURCE_VENDOR_BASE + 10),
                offer(2, FRAME_RESOURCE_VENDOR_BASE + 20),
                offer(3, FRAME_RESOURCE_VENDOR_BASE + 30),
                offer(4, FRAME_RESOURCE_VENDOR_BASE + 40),
            ],
        ))
    }
    fn configure_input(
        &mut self,
        _: u64,
        generation: u64,
        id: u64,
        _: u64,
        source: &CaptureResourceOffer,
    ) -> Result<(), Status> {
        if generation != 5
            || !(1..=4).contains(&id)
            || source.resource.kind != FRAME_RESOURCE_VENDOR_BASE + id as u32 * 10
        {
            return Err(MEDIA_UNSUPPORTED);
        }
        self.kind.set(source.resource.kind);
        Ok(())
    }
    fn output_offers(&mut self, _: u64) -> Result<(u64, Vec<CaptureResourceOffer>), Status> {
        Ok((7, vec![offer(9, self.output_kind)]))
    }
    fn select_output(&mut self, _: u64, g: u64, id: u64) -> Result<(), Status> {
        if (g, id) == (7, 9) {
            Ok(())
        } else {
            Err(MEDIA_UNSUPPORTED)
        }
    }
    fn process(
        &mut self,
        _: u64,
        input: ProcessingFrame<'_>,
    ) -> Result<ResourceOutputFrame, Status> {
        let source = input.descriptor();
        assert_eq!(source.resource.kind, self.kind.get());
        if let Some(observers) = &self.observers {
            observers.borrow_mut().push(input.retain()?);
        }
        let retained = input.retain()?;
        let mut output = Rc::new(Output {
            input: retained,
            frame: CaptureResourceFrame {
                offer_generation: 7,
                offer_id: 9,
                resource: FrameDescriptor {
                    kind: self.output_kind,
                    ..source.resource
                },
                context: ptr::null_mut(),
                retain: retain_output,
                release: release_output,
                ..*source
            },
        });
        let context = Rc::as_ptr(&output).cast_mut().cast();
        Rc::get_mut(&mut output).unwrap().frame.context = context;
        let frame = output.frame;
        let _ = Rc::into_raw(output);
        Ok(unsafe { ResourceOutputFrame::from_owned(frame) })
    }
    fn hdr_metadata(&self, _: u64) -> Option<HdrMetadata> {
        Some(HdrMetadata::default())
    }
    fn cancel(&mut self, _: u64) {
        self.order.borrow_mut().push(self.name);
        if let Some(observers) = &self.observers {
            for input in observers.borrow_mut().drain(..) {
                unsafe { input.release_completed() };
            }
        }
    }
    fn shutdown(&mut self) {
        self.order.borrow_mut().push("shutdown");
    }
}
struct Sink {
    kind: u32,
    held: Option<CaptureResourceFrame>,
    busy: bool,
    saw_pending: bool,
    handle: *const c_void,
    order: Rc<RefCell<Vec<&'static str>>>,
}
unsafe extern "C" fn sink_enumerate(
    context: *mut c_void,
    _: u64,
    sink: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    let kind = unsafe { &*context.cast::<Sink>() }.kind;
    let result = unsafe { (sink.write)(sink.context, &offer(11, kind)) };
    unsafe { generation.write(13) };
    result
}
unsafe extern "C" fn sink_configure(
    context: *mut c_void,
    _: u64,
    g: u64,
    id: u64,
    _: u64,
    producer: *const CaptureResourceOffer,
) -> Status {
    if (g, id) == (13, 11)
        && unsafe { &*producer }.resource.kind == unsafe { &*context.cast::<Sink>() }.kind
    {
        STATUS_OK
    } else {
        MEDIA_UNSUPPORTED
    }
}
unsafe extern "C" fn sink_submit(
    context: *mut c_void,
    _: u64,
    frame: *const CaptureResourceFrame,
    _: u32,
) -> Status {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    if sink.busy {
        return MEDIA_BUSY;
    }
    let frame = unsafe { &*frame };
    assert!(sink.held.is_none());
    sink.saw_pending = !unsafe { &*frame.acquire_sync.data.cast::<Cell<bool>>() }.get();
    sink.handle = frame.resource.data;
    let result = unsafe { (frame.retain)(frame.context) };
    if result == STATUS_OK {
        sink.held = Some(*frame);
    }
    result
}
unsafe extern "C" fn sink_cancel(context: *mut c_void, _: u64) {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    sink.order.borrow_mut().push("consumer");
    if let Some(frame) = sink.held.take() {
        unsafe { (frame.release)(frame.context, &NONE) }
    }
}
fn sink(sink: &mut Sink) -> FrameResourceConsumerApi {
    FrameResourceConsumerApi {
        base: NativeInterface {
            header: AbiHeader::of::<FrameResourceConsumerApi>(),
            context: ptr::from_mut(sink).cast(),
            release: None,
        },
        enumerate_resources: sink_enumerate,
        configure: sink_configure,
        submit: sink_submit,
        cancel: sink_cancel,
    }
}
fn processor(order: Rc<RefCell<Vec<&'static str>>>) -> *const FrameProcessorApi {
    let api = into_frame_processor_binding(Effect {
        kind: Cell::new(0),
        output_kind: FRAME_RESOURCE_VENDOR_BASE + 30,
        name: "processor",
        observers: None,
        order,
    });
    let p = unsafe { &*api };
    assert_eq!(
        unsafe { (p.configure_stream)(p.base.base.base.context, 1, b"{}".as_ptr(), 2) },
        STATUS_OK
    );
    api
}
unsafe fn release_processor(api: *const FrameProcessorApi) {
    let native = unsafe { &(*api).base.base.base };
    unsafe { native.release.unwrap()(native.context) };
}
#[test]
fn unchanged_sdk_and_effect_accept_two_capture_methods_without_waiting_or_copying() {
    for kind in [
        FRAME_RESOURCE_VENDOR_BASE + 10,
        FRAME_RESOURCE_VENDOR_BASE + 20,
    ] {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut source = Source::new(kind);
        let capture = capture(&mut source);
        let processor = processor(order.clone());
        let mut downstream = Sink {
            kind: FRAME_RESOURCE_VENDOR_BASE + 30,
            held: None,
            busy: false,
            saw_pending: false,
            handle: ptr::null(),
            order: order.clone(),
        };
        let sink = sink(&mut downstream);
        let mut session = unsafe {
            FrameProcessingSession::new(
                &capture,
                2,
                &[FrameProcessingStage {
                    processor: &(*processor).base,
                    stream: 1,
                }],
                &sink,
                3,
                FRAME_ACCESS_GPU,
            )
        }
        .unwrap();
        assert_eq!(session.forward(0), Ok(STATUS_OK));
        assert!(downstream.saw_pending);
        assert!(!source.ready.get());
        assert_eq!(downstream.handle, ptr::from_ref(&source.handle).cast());
        assert_eq!(source.references.get(), 1);
        let frame = downstream.held.take().unwrap();
        let completion = schema(
            FRAME_SYNC_VENDOR_BASE + 7,
            ptr::from_ref(&source.ready).cast(),
        );
        unsafe { (frame.release)(frame.context, &completion) };
        assert_eq!(source.references.get(), 1);
        assert!(!source.pending.borrow().is_empty());
        source.ready.set(true);
        source.poll();
        assert_eq!(source.references.get(), 0);
        session.cancel();
        assert_eq!(*order.borrow(), ["consumer", "processor"]);
        drop(session);
        unsafe { release_processor(processor) };
    }
}
#[test]
fn busy_sink_releases_input_and_next_call_uses_newest_frame() {
    let order = Rc::new(RefCell::new(Vec::new()));
    let mut source = Source::new(FRAME_RESOURCE_VENDOR_BASE + 10);
    let capture = capture(&mut source);
    let processor = processor(order.clone());
    let mut downstream = Sink {
        kind: FRAME_RESOURCE_VENDOR_BASE + 30,
        held: None,
        busy: true,
        saw_pending: false,
        handle: ptr::null(),
        order,
    };
    let sink = sink(&mut downstream);
    let mut session = unsafe {
        FrameProcessingSession::new(
            &capture,
            2,
            &[FrameProcessingStage {
                processor: &(*processor).base,
                stream: 1,
            }],
            &sink,
            3,
            FRAME_ACCESS_GPU,
        )
    }
    .unwrap();
    assert_eq!(session.forward(0), Ok(MEDIA_BUSY));
    assert_eq!(source.references.get(), 0);
    assert_eq!(source.released.get(), 1);
    source.sequence += 100;
    downstream.busy = false;
    assert_eq!(session.forward(0), Ok(STATUS_OK));
    assert_eq!(downstream.held.as_ref().unwrap().sequence, 141);
    drop(session);
    assert_eq!(source.references.get(), 0);
    unsafe { release_processor(processor) };
}
#[test]
fn stale_source_generation_retires_downstream_before_processor() {
    let order = Rc::new(RefCell::new(Vec::new()));
    let mut source = Source::new(FRAME_RESOURCE_VENDOR_BASE + 20);
    let capture = capture(&mut source);
    let processor = processor(order.clone());
    let mut downstream = Sink {
        kind: FRAME_RESOURCE_VENDOR_BASE + 30,
        held: None,
        busy: true,
        saw_pending: false,
        handle: ptr::null(),
        order: order.clone(),
    };
    let sink = sink(&mut downstream);
    let mut session = unsafe {
        FrameProcessingSession::new(
            &capture,
            2,
            &[FrameProcessingStage {
                processor: &(*processor).base,
                stream: 1,
            }],
            &sink,
            3,
            FRAME_ACCESS_GPU,
        )
    }
    .unwrap();
    assert_eq!(session.forward(0), Ok(MEDIA_BUSY));
    source.selected = false;
    source.generation += 1;
    assert_eq!(
        session.forward(0),
        Err(crate::capture::ConnectionError::Stale)
    );
    assert!(session.is_retired());
    assert_eq!(*order.borrow(), ["consumer", "processor"]);
    drop(session);
    unsafe { release_processor(processor) };
}

#[test]
fn latest_source_change_preserves_readers_and_settings_then_delivers_new_format() {
    let order = Rc::new(RefCell::new(Vec::new()));
    let mut source = Source::new(FRAME_RESOURCE_VENDOR_BASE + 10);
    let capture = capture(&mut source);
    let processor = processor(order.clone());
    let mut downstream = Sink {
        kind: FRAME_RESOURCE_VENDOR_BASE + 30,
        held: None,
        busy: false,
        saw_pending: false,
        handle: ptr::null(),
        order: order.clone(),
    };
    let sink = sink(&mut downstream);
    // SAFETY: all fixture tables and stream owners outlive the session.
    let mut session = unsafe {
        FrameProcessingSession::new(
            &capture,
            2,
            &[FrameProcessingStage {
                processor: &(*processor).base,
                stream: 1,
            }],
            &sink,
            3,
            FRAME_ACCESS_GPU,
        )
    }
    .unwrap();
    assert_eq!(session.forward_latest(0), Ok(STATUS_OK));
    source.selected = false;
    source.generation += 1;
    source.kind = FRAME_RESOURCE_VENDOR_BASE + 20;
    assert_eq!(session.forward_latest(0), Ok(MEDIA_PENDING));
    assert!(!session.is_retired());
    assert!(order.borrow().is_empty());
    assert_eq!(source.references.get(), 1);
    let mut metadata = HdrMetadata::default();
    assert_eq!(
        unsafe {
            ((*processor).hdr_metadata)((*processor).base.base.base.context, 1, &mut metadata)
        },
        STATUS_OK,
        "the configured processor remains queryable"
    );
    let previous = downstream.held.take().unwrap();
    unsafe { (previous.release)(previous.context, &NONE) };
    source.sequence += 1;
    assert_eq!(session.forward_latest(0), Ok(STATUS_OK));
    assert_eq!(downstream.held.as_ref().unwrap().sequence, source.sequence);
    // An unsupported new source is terminal, with ordered retirement.
    source.selected = false;
    source.generation += 1;
    source.kind = FRAME_RESOURCE_VENDOR_BASE + 99;
    assert_eq!(session.forward_latest(0), Ok(MEDIA_PENDING));
    assert_eq!(
        session.forward_latest(0),
        Err(crate::capture::ConnectionError::NoCompatibleOffer)
    );
    assert!(session.is_retired());
    assert_eq!(*order.borrow(), ["consumer", "processor"]);
    assert_eq!(source.references.get(), 0);
    drop(session);
    unsafe { release_processor(processor) };
}

#[test]
fn multiple_steps_and_shared_readers_forward_dependencies_without_copies_or_completion_waits() {
    let order = Rc::new(RefCell::new(Vec::new()));
    let observers = Rc::new(RefCell::new(Vec::new()));
    let mut source = Source::new(FRAME_RESOURCE_VENDOR_BASE + 10);
    let capture = capture(&mut source);
    let mut effects = Vec::new();
    for (name, output_kind, observe) in [
        ("first", 30, false),
        ("second", 40, true),
        ("third", 50, false),
    ] {
        let api = into_frame_processor_binding(Effect {
            kind: Cell::new(0),
            output_kind: FRAME_RESOURCE_VENDOR_BASE + output_kind,
            name,
            observers: observe.then(|| observers.clone()),
            order: order.clone(),
        });
        let processor = unsafe { &*api };
        assert_eq!(
            unsafe {
                (processor.configure_stream)(processor.base.base.base.context, 1, b"{}".as_ptr(), 2)
            },
            STATUS_OK
        );
        effects.push(api);
    }
    let stages: Vec<_> = effects
        .iter()
        .map(|api| FrameProcessingStage {
            processor: unsafe { &(**api).base },
            stream: 1,
        })
        .collect();
    let mut downstream = Sink {
        kind: FRAME_RESOURCE_VENDOR_BASE + 50,
        held: None,
        busy: false,
        saw_pending: false,
        handle: ptr::null(),
        order: order.clone(),
    };
    let sink = sink(&mut downstream);
    let mut session =
        unsafe { FrameProcessingSession::new(&capture, 2, &stages, &sink, 3, FRAME_ACCESS_GPU) }
            .unwrap();
    assert_eq!(session.forward(0), Ok(STATUS_OK));
    assert!(downstream.saw_pending);
    assert_eq!(downstream.handle, ptr::from_ref(&source.handle).cast());
    assert_eq!(observers.borrow().len(), 1);
    assert!(source.references.get() >= 2);
    let frame = downstream.held.take().unwrap();
    let completion = schema(
        FRAME_SYNC_VENDOR_BASE + 7,
        ptr::from_ref(&source.ready).cast(),
    );
    unsafe { (frame.release)(frame.context, &completion) };
    assert!(!source.pending.borrow().is_empty());
    session.cancel();
    assert_eq!(*order.borrow(), ["consumer", "third", "second", "first"]);
    assert_eq!(
        source.references.get(),
        1,
        "pending work still pins source after the other readers retire"
    );
    source.ready.set(true);
    source.poll();
    assert_eq!(source.references.get(), 0);
    drop(session);
    for effect in effects {
        unsafe { release_processor(effect) };
    }
}

#[test]
fn forward_shares_pixels_and_preserves_each_independent_read_completion() {
    let mut source = Source::new(FRAME_RESOURCE_VENDOR_BASE + 10);
    let capture = capture(&mut source);
    unsafe { (capture.select_resource)(capture.base.base.base.context, 2, 3, 4) };
    let mut copied = None;
    unsafe extern "C" fn copy(c: *mut c_void, frame: *const CaptureResourceFrame) -> Status {
        unsafe { &mut *c.cast::<Option<CaptureResourceFrame>>() }.replace(unsafe { *frame });
        STATUS_OK
    }
    assert_eq!(
        unsafe {
            (capture.read_resource_frame)(
                capture.base.base.base.context,
                2,
                CaptureResourceFrameSink {
                    context: ptr::from_mut(&mut copied).cast(),
                    write: copy,
                },
            )
        },
        STATUS_OK
    );
    let frame = copied.unwrap();
    let output = ProcessingFrame { frame: &frame }
        .forward(FrameResourceSelection {
            generation: 5,
            offer: 8,
        })
        .unwrap();
    assert_eq!(output.frame().resource.data, frame.resource.data);
    assert_eq!(output.frame().acquire_sync.data, frame.acquire_sync.data);
    let one = ProcessingFrame {
        frame: output.frame(),
    }
    .retain()
    .unwrap();
    let two = ProcessingFrame {
        frame: output.frame(),
    }
    .retain()
    .unwrap();
    drop(output);
    let first = Cell::new(false);
    let second = Cell::new(false);
    unsafe {
        one.release_after(&schema(
            FRAME_SYNC_VENDOR_BASE + 7,
            ptr::from_ref(&first).cast(),
        ));
        two.release_after(&schema(
            FRAME_SYNC_VENDOR_BASE + 7,
            ptr::from_ref(&second).cast(),
        ));
    }
    assert_eq!(source.references.get(), 2);
    first.set(true);
    source.poll();
    assert_eq!(source.references.get(), 1);
    second.set(true);
    source.poll();
    assert_eq!(source.references.get(), 0);
}
#[test]
fn consumer_replacement_retires_old_reader_and_keeps_processors_active() {
    let order = Rc::new(RefCell::new(Vec::new()));
    let mut source = Source::new(FRAME_RESOURCE_VENDOR_BASE + 10);
    let capture = capture(&mut source);
    let p = processor(order.clone());
    let stages = [FrameProcessingStage {
        processor: unsafe { &(*p).base },
        stream: 1,
    }];
    let make_sink = || Sink {
        kind: FRAME_RESOURCE_VENDOR_BASE + 30,
        held: None,
        busy: false,
        saw_pending: false,
        handle: ptr::null(),
        order: order.clone(),
    };
    let mut old = make_sink();
    let mut new = make_sink();
    let old_api = sink(&mut old);
    let new_api = sink(&mut new);
    let mut session =
        unsafe { FrameProcessingSession::new(&capture, 2, &stages, &old_api, 3, FRAME_ACCESS_GPU) }
            .unwrap();
    assert_eq!(session.forward(0), Ok(STATUS_OK));
    unsafe { session.replace_consumer(&new_api, 4) }.unwrap();
    assert!(old.held.is_none());
    assert_eq!(source.references.get(), 0);
    assert_eq!(*order.borrow(), ["consumer"]);
    source.sequence += 1;
    assert_eq!(session.forward(0), Ok(STATUS_OK));
    assert!(new.held.is_some());
    session.cancel();
    assert_eq!(source.references.get(), 0);
    assert_eq!(*order.borrow(), ["consumer", "consumer", "processor"]);
    drop(session);
    unsafe { release_processor(p) };
}
#[test]
fn processing_graph_rejects_unbounded_duplicate_and_invalid_native_tables() {
    let order = Rc::new(RefCell::new(Vec::new()));
    let mut source = Source::new(FRAME_RESOURCE_VENDOR_BASE + 10);
    let capture = capture(&mut source);
    let p = processor(order.clone());
    let mut downstream = Sink {
        kind: FRAME_RESOURCE_VENDOR_BASE + 30,
        held: None,
        busy: false,
        saw_pending: false,
        handle: ptr::null(),
        order,
    };
    let mut api = sink(&mut downstream);
    let duplicate = || FrameProcessingStage {
        processor: unsafe { &(*p).base },
        stream: 1,
    };
    assert!(unsafe { FrameProcessingSession::new(&capture, 2, &[], &api, 3, 0) }.is_err());
    assert!(
        unsafe {
            FrameProcessingSession::new(&capture, 2, &[duplicate(), duplicate()], &api, 3, 0)
        }
        .is_err()
    );
    let stages: Vec<_> = (1..=MAX_PROCESSING_STAGES + 1)
        .map(|stream| FrameProcessingStage {
            processor: unsafe { &(*p).base },
            stream: stream as u64,
        })
        .collect();
    assert!(unsafe { FrameProcessingSession::new(&capture, 2, &stages, &api, 3, 0) }.is_err());
    api.base.header.size = 0;
    assert!(
        unsafe { FrameProcessingSession::new(&capture, 2, &[duplicate()], &api, 3, 0) }.is_err()
    );
    unsafe { release_processor(p) };
}

/// A separate implementation forwards references directly to the source owner;
/// Effect above instead owns an intermediate record for each output reference.
struct DirectEffect;
impl FrameProcessor for DirectEffect {
    fn configure_stream(&mut self, _: u64, _: &[u8]) -> Result<(), Status> {
        Ok(())
    }
    fn update_stream(&mut self, _: u64, _: &[u8]) -> Result<(), Status> {
        Err(MEDIA_UNSUPPORTED)
    }
    fn input_offers(&mut self, _: u64) -> Result<(u64, Vec<CaptureResourceOffer>), Status> {
        Ok((5, vec![offer(1, FRAME_RESOURCE_VENDOR_BASE + 10)]))
    }
    fn configure_input(
        &mut self,
        _: u64,
        g: u64,
        id: u64,
        _: u64,
        producer: &CaptureResourceOffer,
    ) -> Result<(), Status> {
        if (g, id) == (5, 1) && producer.resource.kind == FRAME_RESOURCE_VENDOR_BASE + 10 {
            Ok(())
        } else {
            Err(MEDIA_UNSUPPORTED)
        }
    }
    fn output_offers(&mut self, _: u64) -> Result<(u64, Vec<CaptureResourceOffer>), Status> {
        Ok((7, vec![offer(9, FRAME_RESOURCE_VENDOR_BASE + 30)]))
    }
    fn select_output(&mut self, _: u64, g: u64, id: u64) -> Result<(), Status> {
        if (g, id) == (7, 9) {
            Ok(())
        } else {
            Err(MEDIA_UNSUPPORTED)
        }
    }
    fn process(
        &mut self,
        _: u64,
        input: ProcessingFrame<'_>,
    ) -> Result<ResourceOutputFrame, Status> {
        let reference = input.retain()?;
        let descriptor = reference.descriptor();
        let frame = CaptureResourceFrame {
            offer_generation: 7,
            offer_id: 9,
            resource: FrameDescriptor {
                kind: FRAME_RESOURCE_VENDOR_BASE + 30,
                ..descriptor.resource
            },
            ..*descriptor
        };
        // SAFETY: this independent implementation transfers the original owner's
        // reference, retaining its callbacks and each reader's completion signal.
        Ok(unsafe { ResourceOutputFrame::from_owned(frame) })
    }
    fn hdr_metadata(&self, _: u64) -> Option<HdrMetadata> {
        None
    }
    fn cancel(&mut self, _: u64) {}
    fn shutdown(&mut self) {}
}
#[test]
fn unchanged_consumer_accepts_independently_implemented_processors() {
    for implementation in 0..2 {
        let order = Rc::new(RefCell::new(Vec::new()));
        let mut source = Source::new(FRAME_RESOURCE_VENDOR_BASE + 10);
        let capture = capture(&mut source);
        let processor = if implementation == 0 {
            processor(order.clone())
        } else {
            let p = into_frame_processor_binding(DirectEffect);
            assert_eq!(
                unsafe {
                    ((*p).configure_stream)((*p).base.base.base.context, 1, b"{}".as_ptr(), 2)
                },
                STATUS_OK
            );
            p
        };
        let mut downstream = Sink {
            kind: FRAME_RESOURCE_VENDOR_BASE + 30,
            held: None,
            busy: true,
            saw_pending: false,
            handle: ptr::null(),
            order,
        };
        let sink = sink(&mut downstream);
        let mut session = unsafe {
            FrameProcessingSession::new(
                &capture,
                2,
                &[FrameProcessingStage {
                    processor: &(*processor).base,
                    stream: 1,
                }],
                &sink,
                3,
                FRAME_ACCESS_GPU,
            )
        }
        .unwrap();
        assert_eq!(session.forward(0), Ok(MEDIA_BUSY));
        assert_eq!(source.references.get(), 0);
        downstream.busy = false;
        source.sequence += 1;
        assert_eq!(session.forward(0), Ok(STATUS_OK));
        assert!(downstream.saw_pending);
        assert_eq!(downstream.handle, ptr::from_ref(&source.handle).cast());
        session.cancel();
        assert!(downstream.held.is_none());
        assert_eq!(source.references.get(), 0);
        drop(session);
        unsafe { release_processor(processor) };
    }
}
