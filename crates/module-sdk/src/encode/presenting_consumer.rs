//! Encoder-edge presentation for a resource consumer supplied by a plugin.
//!
//! [`super::EncoderResourceConsumer`] presents processed frames to a native
//! encoder. This adapter gives a `FrameResourceConsumer` encoder the same
//! position at the end of a processing chain: negotiation and cancellation pass
//! through unchanged, and each submitted DMA-BUF frame is re-presented on the
//! stream's host video timeline with [`PresentedDmaBufResource`], so the
//! consumer may retain it under the original capture lease. Other resource
//! kinds are declined rather than converted.
use super::{HostVideoPresentation, PresentedDmaBufResource};
use crate::abi::*;
use amaterasu_media::*;
use std::{cell::Cell, ffi::c_void, marker::PhantomData, rc::Rc, time::Instant};

struct State {
    api: FrameResourceConsumerApi,
    consumer: *const FrameResourceConsumerApi,
    presentation: Cell<Option<HostVideoPresentation>>,
    submit_us: Cell<u64>,
    submitted_to_provider: Cell<bool>,
}

pub struct PresentingResourceConsumer {
    state: Box<State>,
    module_thread: PhantomData<Rc<()>>,
}

impl PresentingResourceConsumer {
    /// # Safety
    /// The consumer binding and its stream outlive this adapter and every
    /// processing session using its table, including their cancellation
    /// barriers. Calls arrive on the owning module thread.
    pub unsafe fn new(consumer: &FrameResourceConsumerApi) -> Result<Self, Status> {
        let header = consumer.base.header;
        if header.major != FRAME_RESOURCE_CONSUMER_INTERFACE_MAJOR
            || header.size < FRAME_RESOURCE_CONSUMER_API_V1_0_SIZE
        {
            return Err(MEDIA_UNSUPPORTED);
        }
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
            consumer,
            presentation: Cell::new(None),
            submit_us: Cell::new(0),
            submitted_to_provider: Cell::new(false),
        });
        state.api.base.context = std::ptr::from_mut(&mut *state).cast();
        Ok(Self {
            state,
            module_thread: PhantomData,
        })
    }

    pub fn api(&self) -> &FrameResourceConsumerApi {
        &self.state.api
    }

    pub fn submit_us(&self) -> u64 {
        self.state.submit_us.get()
    }

    /// Whether the last submission for this presentation reached the provider.
    pub fn submitted_to_provider(&self) -> bool {
        self.state.submitted_to_provider.get()
    }

    /// Assign the stream timeline for the next submission. Without a
    /// presentation, submissions are declined: a frame on the capture clock
    /// would be rejected or misordered by a timeline-checking encoder.
    pub fn set_presentation(&mut self, presentation: HostVideoPresentation) {
        self.state.submit_us.set(0);
        self.state.submitted_to_provider.set(false);
        self.state.presentation.set(Some(presentation));
    }
}

/// # Safety
/// `context` is this adapter's boxed state, and the constructor's caller keeps
/// the wrapped consumer live for as long as the adapter table is in use.
unsafe fn inner<'a>(context: *mut c_void) -> (&'a State, &'a FrameResourceConsumerApi) {
    let state = unsafe { &*context.cast::<State>() };
    (state, unsafe { &*state.consumer })
}

unsafe extern "C" fn enumerate(
    context: *mut c_void,
    stream: u64,
    sink: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    let (_, consumer) = unsafe { inner(context) };
    unsafe { (consumer.enumerate_resources)(consumer.base.context, stream, sink, generation) }
}

unsafe extern "C" fn configure(
    context: *mut c_void,
    stream: u64,
    generation: u64,
    offer: u64,
    producer_generation: u64,
    producer: *const CaptureResourceOffer,
) -> Status {
    let (_, consumer) = unsafe { inner(context) };
    unsafe {
        (consumer.configure)(
            consumer.base.context,
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
    let (state, consumer) = unsafe { inner(context) };
    if frame.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let Some(presentation) = state.presentation.get() else {
        return MEDIA_UNSUPPORTED;
    };
    // SAFETY: the upstream stage lends this descriptor for the duration of the
    // call; the view retains the original lease for any consumer reference.
    let presented = match unsafe { PresentedDmaBufResource::new(&*frame, presentation) } {
        Ok(view) => view,
        Err(STATUS_INVALID_ARGUMENT)
            if unsafe { (*frame).resource.kind } != FRAME_RESOURCE_DMABUF =>
        {
            return MEDIA_UNSUPPORTED;
        }
        Err(status) => return status,
    };
    let started = Instant::now();
    state.submitted_to_provider.set(true);
    let status =
        unsafe { (consumer.submit)(consumer.base.context, stream, presented.descriptor(), flags) };
    state
        .submit_us
        .set(started.elapsed().as_micros().min(u64::MAX as u128) as u64);
    status
}

unsafe extern "C" fn cancel(context: *mut c_void, stream: u64) {
    let (_, consumer) = unsafe { inner(context) };
    unsafe { (consumer.cancel)(consumer.base.context, stream) };
}

#[cfg(test)]
mod tests;
