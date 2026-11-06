//! Trusted capture ABI boundary. Bindings retain the controller; each frame
//! owns an immutable pixel reference until its synchronous sink returns.
use crate::controller::{Controller, Delivery};
use amaterasu_media::{
    CAPTURE_FRAME_FRESH, CAPTURE_RESOURCE_INTERFACE_MINOR, CaptureApiV1_6, CaptureApiV1_7,
    CaptureApiV1_8, CaptureEventSink, CaptureFrameV1_3, CaptureGeometry, CaptureResourceFrame,
    CaptureResourceFrameSink, CaptureResourceOffer, CaptureResourceOfferSink, CpuFrame,
    CpuResourceCapabilities, DmaBufFrameSink, DmaBufLeaseSink, FRAME_ACCESS_CPU,
    FRAME_CLOCK_HOST_MONOTONIC, FRAME_OWNERSHIP_BORROWED, FRAME_RESOURCE_CPU, FRAME_SYNC_NONE,
    FrameClockMetadata, FrameDescriptor, FrameSink, FreshFrameSinkV1_2, FreshFrameSinkV1_3,
    INTERFACE_MAJOR, MEDIA_BACKEND_ERROR, MEDIA_BUSY, MEDIA_UNSUPPORTED, PresentationStatus,
    host_monotonic_micros,
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

fn guarded(operation: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(STATUS_INTERNAL_ERROR)
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
        // SAFETY: the broker supplies a writable aligned interface result slot.
        unsafe { output.write(std::ptr::null()) };
        if context.is_null()
            || query.is_null()
            || !(query as usize).is_multiple_of(align_of::<NativeInterfaceQuery>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the broker retains its query for the duration of the call.
        let query = unsafe { &*query };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != INTERFACE_MAJOR
            || query.min_minor > CAPTURE_RESOURCE_INTERFACE_MINOR
            || query.min_size as usize > size_of::<CaptureApiV1_8>()
        {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: caller identity is a synchronous broker-owned byte borrow.
        let Some(caller) = (unsafe { caller.read() })
            .ok()
            .and_then(|value| std::str::from_utf8(value).ok())
            .filter(|value| {
                !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
            })
        else {
            return STATUS_INVALID_ARGUMENT;
        };
        // SAFETY: module retains the boxed factory through all binds.
        let factory = unsafe { &*context.cast::<Factory>() };
        let binding = Binding::new(factory.controller.clone(), caller.into());
        let interface = &binding.api.base.base.base as *const NativeInterface;
        let _ = Box::into_raw(binding);
        // SAFETY: broker uniquely owns and releases this boxed binding.
        unsafe { output.write(interface) };
        STATUS_OK
    })
}

unsafe extern "C" fn release(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !context.is_null() {
            // SAFETY: broker invokes release once, after callbacks on this binding end.
            unsafe { drop(Box::from_raw(context.cast::<Binding>())) };
        }
    }));
}

fn deliver(
    context: *mut c_void,
    stream: u64,
    resource: bool,
    sink: impl FnOnce(&Delivery, &CpuFrame) -> Status,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: native callbacks run only while the broker retains this binding.
    let binding = unsafe { &*context.cast::<Binding>() };
    let delivery = {
        let controller = match binding.controller.try_lock() {
            Ok(value) => value,
            Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
            Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
        };
        match controller.prepare(&binding.caller, stream, host_monotonic_micros(), resource) {
            Ok(value) => value,
            Err(status) => return status,
        }
    };
    let frame = CpuFrame {
        width: delivery.mode.width,
        height: delivery.mode.height,
        format: delivery.mode.format,
        stride: delivery.mode.stride(),
        data: delivery.pixels.as_ptr(),
        len: delivery.pixels.len(),
        timestamp_us: delivery.identity.timestamp_us,
        sequence: delivery.identity.sequence,
    };
    let status = sink(&delivery, &frame);
    delivery.finish(status)
}

unsafe extern "C" fn read_frame(context: *mut c_void, stream: u64, sink: FrameSink) -> Status {
    guarded(|| {
        deliver(context, stream, false, |_, frame| {
            // SAFETY: delivery retains immutable pixels until this callback returns.
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
        deliver(context, stream, false, |_, frame| {
            // SAFETY: delivery retains immutable pixels until this callback returns.
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
        deliver(context, stream, false, |_, frame| {
            let timed = CaptureFrameV1_3 {
                frame: *frame,
                capture_pacing_timestamp_us: frame.timestamp_us,
            };
            // SAFETY: timed descriptor and pixels live through the callback.
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
        // SAFETY: broker retains the boxed binding through this callback.
        let binding = unsafe { &*context.cast::<Binding>() };
        let controller = match binding.controller.try_lock() {
            Ok(value) => value,
            Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
            Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
        };
        let mode = match controller.mode(&binding.caller, stream) {
            Ok(mode) => mode,
            Err(status) => return status,
        };
        // SAFETY: caller supplies an aligned writable geometry output.
        unsafe { output.write(mode.geometry()) };
        STATUS_OK
    })
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
        // SAFETY: broker retains the binding for the callback.
        let binding = unsafe { &*context.cast::<Binding>() };
        let mode = {
            let controller = match binding.controller.try_lock() {
                Ok(value) => value,
                Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
                Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
            };
            match controller.mode(&binding.caller, stream) {
                Ok(mode) => mode,
                Err(status) => return status,
            }
        };
        for (id, format) in [
            (1, amaterasu_media::PIXEL_NV12),
            (2, amaterasu_media::PIXEL_P010),
        ] {
            if (crate::model::SourceMode { format, ..mode })
                .buffer_length()
                .is_err()
            {
                continue;
            }
            let capabilities = CpuResourceCapabilities {
                format,
                min_width: mode.width,
                max_width: mode.width,
                min_height: mode.height,
                max_height: mode.height,
            };
            let offer = CaptureResourceOffer {
                id,
                ownership: FRAME_OWNERSHIP_BORROWED,
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
            // SAFETY: capabilities and offer live until the synchronous sink returns.
            let status = unsafe { (sink.write)(sink.context, &offer) };
            if status != STATUS_OK {
                return status;
            }
        }
        // SAFETY: caller supplies an aligned writable generation output.
        unsafe { generation.write(stream) };
        STATUS_OK
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
        // SAFETY: broker retains the binding through callback completion.
        let binding = unsafe { &*context.cast::<Binding>() };
        let mut controller = match binding.controller.try_lock() {
            Ok(value) => value,
            Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
            Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
        };
        controller.select(&binding.caller, stream, generation, offer)
    })
}

unsafe extern "C" fn read_resource_frame(
    context: *mut c_void,
    stream: u64,
    sink: CaptureResourceFrameSink,
) -> Status {
    guarded(|| {
        deliver(context, stream, true, |delivery, frame| {
            let clock = delivery
                .clock_epoch
                .map_or_else(FrameClockMetadata::default, |epoch_id| FrameClockMetadata {
                    version: 1,
                    size: size_of::<FrameClockMetadata>() as u32,
                    domain: FRAME_CLOCK_HOST_MONOTONIC,
                    reserved: 0,
                    epoch_id,
                });
            let offered = CaptureResourceFrame {
                offer_generation: stream,
                offer_id: if frame.format == amaterasu_media::PIXEL_P010 {
                    2
                } else {
                    1
                },
                timestamp_us: frame.timestamp_us,
                capture_pacing_timestamp_us: frame.timestamp_us,
                sequence: frame.sequence,
                width: frame.width,
                height: frame.height,
                freshness: CAPTURE_FRAME_FRESH,
                reserved: 0,
                // The CPU bridge has no color-conversion contract. Generated
                // neutral codes are limited-range; encoded color is negotiated upstream.
                color: Default::default(),
                clock,
                pacing_clock: clock,
                resource: FrameDescriptor {
                    kind: FRAME_RESOURCE_CPU,
                    version: 1,
                    size: size_of::<CpuFrame>() as u32,
                    reserved: 0,
                    namespace: [0; 16],
                    data: (frame as *const CpuFrame).cast(),
                },
                acquire_sync: none_descriptor(),
                context: std::ptr::null_mut(),
                retain: unsupported_retain,
                release: release_borrowed,
            };
            // SAFETY: synchronous borrowed resource, descriptor and pixels remain live through the sink.
            unsafe { (sink.write)(sink.context, &offered) }
        })
    })
}

unsafe extern "C" fn unsupported_retain(_: *mut c_void) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn release_borrowed(_: *mut c_void, _: *const FrameDescriptor) {}
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

#[cfg(test)]
mod tests;
