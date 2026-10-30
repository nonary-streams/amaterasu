use crate::{
    controller::Controller,
    model::{FPS, HEIGHT, WIDTH, geometry as fixture_geometry},
};
use amaterasu_media::{
    CAPTURE_FRAME_FRESH, CAPTURE_RESOURCE_INTERFACE_MINOR, CaptureApiV1_6, CaptureApiV1_7,
    CaptureApiV1_8, CaptureEventSink, CaptureFrameV1_3, CaptureGeometry, CaptureResourceFrame,
    CaptureResourceFrameSink, CaptureResourceOffer, CaptureResourceOfferSink, CpuFrame,
    CpuResourceCapabilities, DmaBufFrameSink, DmaBufLeaseSink, FRAME_ACCESS_CPU,
    FRAME_OWNERSHIP_RETAINABLE, FRAME_RESOURCE_CPU, FRAME_SYNC_NONE, FrameDescriptor, FrameSink,
    FreshFrameSinkV1_2, FreshFrameSinkV1_3, INTERFACE_MAJOR, MEDIA_BACKEND_ERROR, MEDIA_BUSY,
    MEDIA_CLOSED, MEDIA_UNSUPPORTED, PIXEL_NV12, PresentationStatus,
};
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, NativeInterface, NativeInterfaceQuery, NativeServiceFactory,
    STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT, STATUS_OK, Status,
};
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, TryLockError},
};

pub type SharedController = Arc<Mutex<Controller>>;

pub struct Factory {
    pub api: NativeServiceFactory,
    controller: SharedController,
}

impl Factory {
    pub fn new(controller: SharedController) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind),
            },
            controller,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}

struct Binding {
    api: CaptureApiV1_8,
    controller: SharedController,
    caller: String,
}

impl Binding {
    fn new(controller: SharedController, caller: String) -> Box<Self> {
        let mut binding = Box::new(Self {
            api: CaptureApiV1_8 {
                base: CaptureApiV1_7 {
                    base: CaptureApiV1_6 {
                        base: NativeInterface {
                            header: AbiHeader {
                                size: size_of::<CaptureApiV1_8>() as u32,
                                major: INTERFACE_MAJOR,
                                minor: CAPTURE_RESOURCE_INTERFACE_MINOR,
                            },
                            context: std::ptr::null_mut(),
                            release: Some(release),
                        },
                        read_frame,
                        geometry,
                        read_frame_with_freshness: read_fresh,
                        read_frame_with_capture_timestamp: read_timed,
                        read_dmabuf_frame: unsupported_dmabuf,
                        latest_presentation: unsupported_presentation,
                        read_dmabuf_frame_leased: unsupported_leased,
                    },
                    subscribe_events: unsupported_events,
                    unsubscribe_events: unsubscribe,
                    resolve_swap: unsupported_swap,
                },
                enumerate_resources,
                select_resource,
                read_resource_frame,
            },
            controller,
            caller,
        });
        binding.api.base.base.base.context = (&mut *binding as *mut Self).cast();
        binding
    }
}

unsafe extern "C" fn bind(
    context: *mut c_void,
    caller: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guarded(|| {
        if output.is_null()
            || !(output as usize).is_multiple_of(align_of::<*const NativeInterface>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the broker supplies an aligned writable output slot for this call.
        unsafe { output.write(std::ptr::null()) };
        if context.is_null()
            || query.is_null()
            || !(query as usize).is_multiple_of(align_of::<NativeInterfaceQuery>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the broker retains this aligned query and boxed factory until return.
        let query = unsafe { &*query };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != INTERFACE_MAJOR
            || query.min_minor > CAPTURE_RESOURCE_INTERFACE_MINOR
            || query.min_size as usize > size_of::<CaptureApiV1_8>()
        {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: authenticated caller bytes are borrowed from the broker for this call.
        let Some(caller) = (unsafe { caller.read() })
            .ok()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .filter(|caller| !caller.is_empty() && caller.len() <= 128)
        else {
            return STATUS_INVALID_ARGUMENT;
        };
        // SAFETY: context is the boxed factory retained by the provider.
        let factory = unsafe { &*context.cast::<Factory>() };
        let binding = Binding::new(factory.controller.clone(), caller.into());
        let interface = &binding.api.base.base.base as *const NativeInterface;
        let _ = Box::into_raw(binding);
        // SAFETY: one boxed binding is transferred to the broker for unique release.
        unsafe { output.write(interface) };
        STATUS_OK
    })
}

unsafe extern "C" fn release(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !context.is_null() {
            // SAFETY: the broker uniquely releases the boxed binding after calls end.
            unsafe { drop(Box::from_raw(context.cast::<Binding>())) };
        }
    }));
}

fn deliver(
    context: *mut c_void,
    stream: u64,
    sink: impl FnOnce(&crate::controller::FrameDelivery, &CpuFrame) -> Status,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the broker retains the boxed binding through this synchronous call.
    let binding = unsafe { &*context.cast::<Binding>() };
    let delivery = {
        let controller = match binding.controller.try_lock() {
            Ok(controller) => controller,
            Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
            Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
        };
        match controller.prepare_frame(&binding.caller, stream) {
            Ok(delivery) => delivery,
            Err(status) => return status,
        }
    };
    if delivery.is_closed() {
        return MEDIA_CLOSED;
    }
    let frame = CpuFrame {
        width: WIDTH,
        height: HEIGHT,
        format: PIXEL_NV12,
        stride: WIDTH,
        data: delivery.bytes.as_ptr(),
        len: delivery.bytes.len(),
        timestamp_us: delivery.sequence * 1_000_000 / FPS,
        sequence: delivery.sequence,
    };
    let status = sink(&delivery, &frame);
    delivery.finish(status)
}

unsafe extern "C" fn read_frame(context: *mut c_void, stream: u64, sink: FrameSink) -> Status {
    guarded(|| {
        deliver(context, stream, |_, frame| {
            // SAFETY: the frame and sink are valid until this synchronous callback returns.
            unsafe { sink.send(frame) }
        })
    })
}

unsafe extern "C" fn read_fresh(
    context: *mut c_void,
    stream: u64,
    sink: FreshFrameSinkV1_2,
) -> Status {
    guarded(|| {
        deliver(context, stream, |_, frame| {
            // SAFETY: both the frame and sink remain valid during the callback.
            unsafe { sink.send(frame, CAPTURE_FRAME_FRESH) }
        })
    })
}

unsafe extern "C" fn read_timed(
    context: *mut c_void,
    stream: u64,
    sink: FreshFrameSinkV1_3,
) -> Status {
    guarded(|| {
        deliver(context, stream, |_, frame| {
            let timed = CaptureFrameV1_3 {
                frame: *frame,
                capture_pacing_timestamp_us: frame.timestamp_us,
            };
            // SAFETY: the timed frame and sink remain valid during the callback.
            unsafe { sink.send(&timed, CAPTURE_FRAME_FRESH) }
        })
    })
}

unsafe extern "C" fn geometry(
    context: *mut c_void,
    stream: u64,
    output: *mut CaptureGeometry,
) -> Status {
    guarded(|| {
        if context.is_null()
            || output.is_null()
            || !(output as usize).is_multiple_of(align_of::<CaptureGeometry>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the broker retains this boxed binding until return.
        let binding = unsafe { &*context.cast::<Binding>() };
        let controller = match binding.controller.try_lock() {
            Ok(controller) => controller,
            Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
            Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
        };
        if controller.authorize(&binding.caller, stream).is_err() {
            return MEDIA_CLOSED;
        }
        // SAFETY: output is aligned, writable, and borrowed for this call only.
        unsafe { output.write(fixture_geometry()) };
        STATUS_OK
    })
}

fn guarded(operation: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(STATUS_INTERNAL_ERROR)
}

#[cfg(test)]
mod tests;

// The fixture has no GPU frames or asynchronous event source. These optional
// operations report unsupported, while the legacy CPU prefix remains usable.
unsafe extern "C" fn unsupported_dmabuf(_: *mut c_void, _: u64, _: DmaBufFrameSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn unsupported_leased(_: *mut c_void, _: u64, _: DmaBufLeaseSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn unsupported_presentation(
    _: *mut c_void,
    _: u64,
    _: *mut PresentationStatus,
) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn unsupported_events(_: *mut c_void, _: u64, _: CaptureEventSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn unsubscribe(_: *mut c_void, _: u64) {}
unsafe extern "C" fn unsupported_swap(
    _: *mut c_void,
    _: u64,
    _: u64,
    _: u32,
    _: BorrowedBytes,
) -> Status {
    MEDIA_UNSUPPORTED
}

fn none_descriptor() -> FrameDescriptor {
    FrameDescriptor {
        kind: FRAME_SYNC_NONE,
        version: 0,
        size: 0,
        reserved: 0,
        namespace: [0; 16],
        data: std::ptr::null(),
    }
}

unsafe extern "C" fn enumerate_resources(
    context: *mut c_void,
    stream: u64,
    sink: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    guarded(|| {
        if context.is_null()
            || generation.is_null()
            || !(generation as usize).is_multiple_of(align_of::<u64>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the binding is live for the call and generation is a writable output.
        let binding = unsafe { &*context.cast::<Binding>() };
        let controller = match binding.controller.try_lock() {
            Ok(value) => value,
            Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
            Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
        };
        if controller.authorize(&binding.caller, stream).is_err() {
            return MEDIA_CLOSED;
        }
        drop(controller);
        let capabilities = CpuResourceCapabilities {
            format: PIXEL_NV12,
            min_width: WIDTH,
            max_width: WIDTH,
            min_height: HEIGHT,
            max_height: HEIGHT,
        };
        let offer = CaptureResourceOffer {
            id: 1,
            ownership: FRAME_OWNERSHIP_RETAINABLE,
            accessibility: FRAME_ACCESS_CPU,
            reserved: 0,
            resource: FrameDescriptor {
                kind: FRAME_RESOURCE_CPU,
                version: 1,
                size: size_of::<CpuResourceCapabilities>() as u32,
                reserved: 0,
                namespace: [0; 16],
                data: (&capabilities as *const CpuResourceCapabilities).cast(),
            },
            acquire_sync: none_descriptor(),
            completion_sync: none_descriptor(),
        };
        // SAFETY: offer and sink live throughout this synchronous callback; no lock is held.
        let status = unsafe { (sink.write)(sink.context, &offer) };
        if status == STATUS_OK {
            // SAFETY: generation is aligned and writable for this call.
            unsafe { generation.write(1) };
        }
        status
    })
}

unsafe extern "C" fn select_resource(
    context: *mut c_void,
    stream: u64,
    generation: u64,
    offer: u64,
) -> Status {
    guarded(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the broker retains the binding during this call.
        let binding = unsafe { &*context.cast::<Binding>() };
        let mut controller = match binding.controller.try_lock() {
            Ok(value) => value,
            Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
            Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
        };
        controller.select_resource(&binding.caller, stream, generation, offer)
    })
}

struct CpuLease {
    // Kept alongside the descriptor for the entire retained lifetime.
    _bytes: Box<[u8]>,
    frame: CpuFrame,
    retained: std::sync::atomic::AtomicUsize,
}

// SAFETY: frame.data points into this lease's immutable boxed byte allocation,
// which never moves or mutates; the only mutable state is atomic reference accounting.
unsafe impl Send for CpuLease {}
// SAFETY: all shared access is to immutable pixels/metadata or atomic accounting.
unsafe impl Sync for CpuLease {}

#[allow(deprecated)]
unsafe extern "C" fn retain_cpu(context: *mut c_void) -> Status {
    guarded(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the callback is called only while the provider's initial or a retained reference lives.
        let lease = unsafe { &*context.cast::<CpuLease>() };
        if lease
            .retained
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |count| (count < 16).then_some(count + 1),
            )
            .is_err()
        {
            return MEDIA_BUSY;
        }
        // SAFETY: context points at an Arc allocation kept alive by the active reference.
        unsafe { Arc::increment_strong_count(context.cast::<CpuLease>()) };
        STATUS_OK
    })
}

#[allow(deprecated)]
unsafe extern "C" fn release_cpu(context: *mut c_void, completion: *const FrameDescriptor) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if context.is_null()
            || completion.is_null()
            || !(completion as usize).is_multiple_of(align_of::<FrameDescriptor>())
        {
            return;
        }
        // SAFETY: caller supplies one retained reference and a borrowed completion descriptor.
        let descriptor = unsafe { &*completion };
        if descriptor.kind != FRAME_SYNC_NONE
            || descriptor.version != 0
            || descriptor.size != 0
            || descriptor.reserved != 0
            || descriptor.namespace != [0; 16]
            || !descriptor.data.is_null()
        {
            return;
        }
        // SAFETY: a valid retained reference keeps the lease alive through this operation.
        let lease = unsafe { &*context.cast::<CpuLease>() };
        if lease
            .retained
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |count| count.checked_sub(1),
            )
            .is_ok()
        {
            // SAFETY: this consumes exactly one reference previously acquired by retain_cpu.
            unsafe { Arc::decrement_strong_count(context.cast::<CpuLease>()) };
        }
    }));
}

unsafe extern "C" fn read_resource_frame(
    context: *mut c_void,
    stream: u64,
    sink: CaptureResourceFrameSink,
) -> Status {
    guarded(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the binding is retained throughout the callback.
        let binding = unsafe { &*context.cast::<Binding>() };
        let selected = {
            let controller = match binding.controller.try_lock() {
                Ok(value) => value,
                Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
                Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
            };
            match controller.resource_selected(&binding.caller, stream) {
                Ok(value) => value,
                Err(status) => return status,
            }
        };
        if !selected {
            return MEDIA_UNSUPPORTED;
        }
        deliver(context, stream, |delivery, frame| {
            let bytes = delivery.bytes.clone().into_boxed_slice();
            let lease = Arc::new(CpuLease {
                frame: CpuFrame {
                    data: bytes.as_ptr(),
                    ..*frame
                },
                _bytes: bytes,
                retained: std::sync::atomic::AtomicUsize::new(0),
            });
            let resource = FrameDescriptor {
                kind: FRAME_RESOURCE_CPU,
                version: 1,
                size: size_of::<CpuFrame>() as u32,
                reserved: 0,
                namespace: [0; 16],
                data: (&lease.frame as *const CpuFrame).cast(),
            };
            let offered = CaptureResourceFrame {
                offer_generation: 1,
                offer_id: 1,
                timestamp_us: frame.timestamp_us,
                capture_pacing_timestamp_us: frame.timestamp_us,
                sequence: frame.sequence,
                width: WIDTH,
                height: HEIGHT,
                freshness: CAPTURE_FRAME_FRESH,
                reserved: 0,
                color: Default::default(),
                clock: Default::default(),
                pacing_clock: Default::default(),
                resource,
                acquire_sync: none_descriptor(),
                context: Arc::as_ptr(&lease).cast_mut().cast(),
                retain: retain_cpu,
                release: release_cpu,
            };
            // SAFETY: provider's Arc owns the immutable bytes and descriptor through the call.
            unsafe { (sink.write)(sink.context, &offered) }
        })
    })
}
