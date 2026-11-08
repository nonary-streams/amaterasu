//! Resource input for a selected encoder. This is a local SDK adapter over the
//! exact Encoder resource contract, with no storage conversion or fallback.
use crate::abi::*;
use amaterasu_media::*;
use std::{cell::Cell, ffi::c_void, marker::PhantomData, rc::Rc, time::Instant};
#[derive(Clone, Copy)]
pub struct EncoderPresentation {
    pub timestamp_us: u64,
    pub sequence: u64,
}
struct State {
    api: FrameResourceConsumerApi,
    encoder: *const EncoderApiV1_5,
    presentation: Option<EncoderPresentation>,
    submit_us: Cell<u64>,
}
pub struct EncoderResourceConsumer {
    state: Box<State>,
    module_thread: PhantomData<Rc<()>>,
}
impl EncoderResourceConsumer {
    /// # Safety
    /// The exact Encoder binding and its session outlive this adapter and every
    /// processing session using its table, including their cancellation barriers.
    pub unsafe fn new(encoder: &EncoderApi) -> Result<Self, Status> {
        let header = encoder.base.header;
        if header.major != INTERFACE_MAJOR
            || header.minor < ENCODER_RESOURCE_INTERFACE_MINOR
            || header.size < ENCODER_API_V1_5_SIZE
        {
            return Err(MEDIA_UNSUPPORTED);
        }
        let encoder = std::ptr::from_ref(encoder).cast::<EncoderApiV1_5>();
        let mut state = Box::new(State {
            api: FrameResourceConsumerApi {
                base: NativeInterface {
                    header: AbiHeader::of::<FrameResourceConsumerApi>(),
                    context: std::ptr::null_mut(),
                    release: None,
                },
                enumerate_resources: enumerate,
                configure,
                submit,
                cancel,
            },
            encoder,
            presentation: None,
            submit_us: Cell::new(0),
        });
        state.api.base.context = std::ptr::from_mut(&mut *state).cast();
        Ok(Self {
            state,
            module_thread: PhantomData,
        })
    }
    pub fn submit_us(&self) -> u64 {
        self.state.submit_us.get()
    }
    pub fn api(&self) -> &FrameResourceConsumerApi {
        &self.state.api
    }
    /// The source identity is preserved through processing. Only the encoder
    /// edge assigns the stream's encoded timeline and submission sequence.
    pub fn set_presentation(&mut self, presentation: EncoderPresentation) {
        self.state.submit_us.set(0);
        self.state.presentation = Some(presentation);
    }
}
unsafe extern "C" fn enumerate(
    context: *mut c_void,
    stream: u64,
    sink: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    let state = unsafe { &*context.cast::<State>() };
    let encoder = unsafe { &*state.encoder };
    unsafe {
        (encoder.enumerate_resources)(
            encoder.base.base.base.base.context,
            stream,
            sink,
            generation,
        )
    }
}
unsafe extern "C" fn configure(
    context: *mut c_void,
    stream: u64,
    generation: u64,
    offer: u64,
    producer_generation: u64,
    producer: *const CaptureResourceOffer,
) -> Status {
    let state = unsafe { &*context.cast::<State>() };
    let encoder = unsafe { &*state.encoder };
    unsafe {
        (encoder.configure_resource)(
            encoder.base.base.base.base.context,
            stream,
            generation,
            offer,
            producer_generation,
            producer,
        )
    }
}
unsafe extern "C" fn submit(
    context: *mut c_void,
    stream: u64,
    frame: *const CaptureResourceFrame,
    flags: u32,
) -> Status {
    let state = unsafe { &*context.cast::<State>() };
    let encoder = unsafe { &*state.encoder };
    let mut forwarded = unsafe { *frame };
    if let Some(presentation) = state.presentation {
        forwarded.timestamp_us = presentation.timestamp_us;
        forwarded.sequence = presentation.sequence;
        forwarded.clock = FrameClockMetadata {
            version: 1,
            size: std::mem::size_of::<FrameClockMetadata>() as u32,
            domain: FRAME_CLOCK_STREAM_RELATIVE,
            ..Default::default()
        };
    }
    let started = Instant::now();
    let status = unsafe {
        (encoder.submit_resource)(
            encoder.base.base.base.base.context,
            stream,
            &forwarded,
            flags,
        )
    };
    state
        .submit_us
        .set(started.elapsed().as_micros().min(u64::MAX as u128) as u64);
    status
}
unsafe extern "C" fn cancel(context: *mut c_void, stream: u64) {
    let state = unsafe { &*context.cast::<State>() };
    let encoder = unsafe { &*state.encoder };
    unsafe {
        (encoder.base.base.base.cancel_dmabuf_leases)(encoder.base.base.base.base.context, stream)
    };
}
