//! Assign an encoder-edge presentation to an already captured GPU resource.
//! Only descriptor metadata is copied; storage, fences and ownership stay with
//! the capture provider. The original capture clock is preserved as pacing data.
use crate::abi::{STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use amaterasu_media::*;
use std::{
    ffi::c_void,
    marker::PhantomData,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
};

const MAX_ADAPTERS: usize = 128;
const MAX_REFERENCES: usize = 128;
static ADAPTERS: AtomicUsize = AtomicUsize::new(0);

fn reserve(counter: &AtomicUsize, maximum: usize) -> Result<(), Status> {
    let mut count = counter.load(Ordering::Acquire);
    loop {
        if count >= maximum {
            return Err(MEDIA_BUSY);
        }
        match counter.compare_exchange_weak(count, count + 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Ok(()),
            Err(current) => count = current,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HostVideoPresentation {
    pub timestamp_us: u64,
    pub epoch_id: [u8; 16],
    pub sequence: u64,
}

struct Storage {
    source: CaptureResourceFrame,
    payload: DmaBufFrame,
    presented: CaptureResourceFrame,
    references: AtomicUsize,
}

/// A view for one encoder submission. Retaining the view retains the original
/// capture lease and pins this view's header until the consumer releases it.
pub struct PresentedDmaBufResource {
    storage: *mut Storage,
    owner_thread: PhantomData<Rc<()>>,
}

impl PresentedDmaBufResource {
    /// # Safety
    /// `source` and its descriptor data/handles must be valid for this call and
    /// this view's lifetime. Its retain/release callbacks must implement the
    /// Capture resource contract. Keep the capture binding loaded until every
    /// consumer reference is released, including completion work. Construct,
    /// submit and retire references on the providers' owning thread.
    pub unsafe fn new(
        source: &CaptureResourceFrame,
        presentation: HostVideoPresentation,
    ) -> Result<Self, Status> {
        let resource = &source.resource;
        if presentation.timestamp_us == 0
            || presentation.sequence == 0
            || presentation.epoch_id == [0; 16]
            || source.reserved != 0
            || source.sequence == 0
            || resource.kind != FRAME_RESOURCE_DMABUF
            || resource.version != 1
            || resource.size as usize != size_of::<DmaBufFrame>()
            || resource.reserved != 0
            || resource.namespace != [0; 16]
            || resource.data.is_null()
            || !(resource.data as usize).is_multiple_of(align_of::<DmaBufFrame>())
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: caller guarantees the exact standard descriptor stays live.
        let mut payload = unsafe { *resource.data.cast::<DmaBufFrame>() };
        payload.validate()?;
        if payload.width != source.width
            || payload.height != source.height
            || payload.timestamp_us != source.timestamp_us
            || payload.sequence != source.sequence
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        reserve(&ADAPTERS, MAX_ADAPTERS)?;
        payload.timestamp_us = presentation.timestamp_us;
        payload.sequence = presentation.sequence;
        let mut storage = Box::new(Storage {
            source: *source,
            payload,
            presented: *source,
            references: AtomicUsize::new(1),
        });
        storage.presented.timestamp_us = presentation.timestamp_us;
        storage.presented.sequence = presentation.sequence;
        storage.presented.clock = FrameClockMetadata {
            version: 1,
            size: size_of::<FrameClockMetadata>() as u32,
            domain: FRAME_CLOCK_HOST_MONOTONIC,
            reserved: 0,
            epoch_id: presentation.epoch_id,
        };
        if source.pacing_clock.version == 0 {
            storage.presented.capture_pacing_timestamp_us = source.timestamp_us;
            storage.presented.pacing_clock = source.clock;
        }
        storage.presented.resource.data = (&storage.payload as *const DmaBufFrame).cast();
        storage.presented.context = (&mut *storage as *mut Storage).cast();
        storage.presented.retain = retain;
        storage.presented.release = release;
        Ok(Self {
            storage: Box::into_raw(storage),
            owner_thread: PhantomData,
        })
    }

    pub fn descriptor(&self) -> &CaptureResourceFrame {
        // SAFETY: this view owns the sentinel reference until Drop.
        unsafe { &(*self.storage).presented }
    }
}

unsafe fn drop_reference(storage: *mut Storage) {
    // SAFETY: each caller consumes exactly one live adapter reference.
    if unsafe { &*storage }
        .references
        .fetch_sub(1, Ordering::AcqRel)
        == 1
    {
        unsafe { drop(Box::from_raw(storage)) };
        ADAPTERS.fetch_sub(1, Ordering::AcqRel);
    }
}

unsafe extern "C" fn retain(context: *mut c_void) -> Status {
    // SAFETY: a borrowed sentinel or consumer reference pins the allocation.
    let storage = unsafe { &*context.cast::<Storage>() };
    if let Err(status) = reserve(&storage.references, MAX_REFERENCES) {
        return status;
    }
    let status = unsafe { (storage.source.retain)(storage.source.context) };
    if status != STATUS_OK {
        unsafe { drop_reference(context.cast()) };
    }
    status
}

unsafe extern "C" fn release(context: *mut c_void, completion: *const FrameDescriptor) {
    // SAFETY: this consumes one successful retain; completion is borrowed by
    // the original provider exactly as supplied by the downstream consumer.
    let storage = unsafe { &*context.cast::<Storage>() };
    unsafe { (storage.source.release)(storage.source.context, completion) };
    unsafe { drop_reference(context.cast()) };
}

impl Drop for PresentedDmaBufResource {
    fn drop(&mut self) {
        // The sentinel borrowed the original frame; it never retained it.
        unsafe { drop_reference(self.storage) };
    }
}

#[cfg(test)]
mod tests;
