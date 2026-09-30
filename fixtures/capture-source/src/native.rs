use crate::{
    controller::Controller,
    model::{FPS, HEIGHT, WIDTH, geometry as fixture_geometry},
};
use amaterasu_media::{
    CAPTURE_INTERFACE_MINOR, CaptureApi, CaptureGeometry, CpuFrame, FrameSink, INTERFACE_MAJOR,
    MEDIA_BACKEND_ERROR, MEDIA_BUSY, MEDIA_CLOSED, MEDIA_UNSUPPORTED, PIXEL_NV12,
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
    api: CaptureApi,
    controller: SharedController,
    caller: String,
}

impl Binding {
    fn new(controller: SharedController, caller: String) -> Box<Self> {
        let mut binding = Box::new(Self {
            api: CaptureApi {
                base: NativeInterface {
                    header: AbiHeader {
                        size: size_of::<CaptureApi>() as u32,
                        major: INTERFACE_MAJOR,
                        minor: CAPTURE_INTERFACE_MINOR,
                    },
                    context: std::ptr::null_mut(),
                    release: Some(release),
                },
                read_frame,
                geometry,
            },
            controller,
            caller,
        });
        binding.api.base.context = (&mut *binding as *mut Self).cast();
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
            || query.min_minor > CAPTURE_INTERFACE_MINOR
            || query.min_size as usize > size_of::<CaptureApi>()
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
        let interface = &binding.api.base as *const NativeInterface;
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

unsafe extern "C" fn read_frame(context: *mut c_void, stream: u64, sink: FrameSink) -> Status {
    guarded(|| {
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
        // SAFETY: this owned immutable frame allocation lives until the synchronous
        // callback returns. No controller lock is held during foreign code. The
        // consumer must copy bytes it needs after return, never retain the pointer.
        let status = unsafe { sink.send(&frame) };
        delivery.finish(status)
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
