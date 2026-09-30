use crate::{backend::Backend, controller::Controller};
use amaterasu_media::{
    CAPTURE_INTERFACE_MINOR, CaptureApi, CaptureGeometry, CpuFrame, FrameSink, INTERFACE_MAJOR,
    MEDIA_BACKEND_ERROR, MEDIA_BUSY, MEDIA_CLOSED, MEDIA_UNSUPPORTED, PIXEL_BGRA8,
};
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, NativeInterface, NativeInterfaceQuery, NativeServiceFactory,
    STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT, STATUS_OK, Status,
};
use std::{
    ffi::c_void,
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
};

type SharedController<B> = Arc<Mutex<Controller<B>>>;

pub struct Factory<B: Backend> {
    pub api: NativeServiceFactory,
    controller: SharedController<B>,
}

impl<B: Backend + 'static> Factory<B> {
    pub fn new(controller: SharedController<B>) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind::<B>),
            },
            controller,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}

struct Binding<B: Backend> {
    api: CaptureApi,
    controller: SharedController<B>,
    caller: String,
}

impl<B: Backend> Binding<B> {
    fn new(controller: SharedController<B>, caller: String) -> Box<Self> {
        let mut binding = Box::new(Self {
            api: CaptureApi {
                base: NativeInterface {
                    header: AbiHeader {
                        size: size_of::<CaptureApi>() as u32,
                        major: INTERFACE_MAJOR,
                        minor: CAPTURE_INTERFACE_MINOR,
                    },
                    context: std::ptr::null_mut(),
                    release: Some(release::<B>),
                },
                read_frame: read_frame::<B>,
                geometry: geometry::<B>,
            },
            controller,
            caller,
        });
        binding.api.base.context = (&mut *binding as *mut Self).cast();
        binding
    }
}

fn capture_status(error: &crate::model::CaptureError) -> Status {
    match error.code {
        "not_found" | "access_lost" | "device_lost" | "output_unavailable" => MEDIA_CLOSED,
        "unsupported_platform" | "unsupported_pixel_format" | "invalid_geometry" => {
            MEDIA_UNSUPPORTED
        }
        _ => MEDIA_BACKEND_ERROR,
    }
}

unsafe extern "C" fn bind<B: Backend + 'static>(
    context: *mut c_void,
    caller: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() || query.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: ABI caller supplies aligned, writable output and query records;
        // this function retains a boxed factory through the module lifetime.
        unsafe { *output = std::ptr::null() };
        let query = unsafe { &*query };
        if !supports_query(query) {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: caller bytes remain valid until this synchronous bind returns.
        let caller = match unsafe { caller.read() }
            .ok()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        {
            Some(caller) if !caller.is_empty() => caller.to_owned(),
            _ => return STATUS_INVALID_ARGUMENT,
        };
        // SAFETY: context was created from the retained Box<Factory<B>>.
        let factory = unsafe { &*context.cast::<Factory<B>>() };
        let binding = Binding::new(factory.controller.clone(), caller);
        let table = &binding.api.base as *const NativeInterface;
        let _ = Box::into_raw(binding);
        // SAFETY: transfers this unique stable binding to the broker; release
        // frees it after the broker has stopped invoking its callbacks.
        unsafe { *output = table };
        STATUS_OK
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}

unsafe extern "C" fn release<B: Backend>(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !context.is_null() {
            // SAFETY: broker releases each unique binding exactly once.
            unsafe { drop(Box::from_raw(context.cast::<Binding<B>>())) };
        }
    }));
}

unsafe extern "C" fn read_frame<B: Backend>(
    context: *mut c_void,
    stream: u64,
    sink: FrameSink,
) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: broker retains this binding through the synchronous callback.
        let binding = unsafe { &*context.cast::<Binding<B>>() };
        let frame = {
            let mut controller = match binding.controller.try_lock() {
                Ok(controller) => controller,
                Err(std::sync::TryLockError::WouldBlock) => return MEDIA_BUSY,
                Err(std::sync::TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
            };
            match controller.read(&binding.caller, stream) {
                Ok(frame) => frame,
                Err(error) => return capture_status(&error),
            }
        };
        let borrowed = CpuFrame {
            width: frame.width,
            height: frame.height,
            format: PIXEL_BGRA8,
            stride: frame.stride,
            data: frame.bytes.as_ptr(),
            len: frame.bytes.len(),
            timestamp_us: frame.timestamp_us,
            sequence: frame.sequence,
        };
        // SAFETY: the frame allocation stays alive for this callback; consumers
        // may borrow pixels only until FrameSink::send returns.
        unsafe { sink.send(&borrowed) }
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}

unsafe extern "C" fn geometry<B: Backend>(
    context: *mut c_void,
    stream: u64,
    output: *mut CaptureGeometry,
) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: broker retains this binding through this synchronous callback.
        let binding = unsafe { &*context.cast::<Binding<B>>() };
        let controller = match binding.controller.try_lock() {
            Ok(controller) => controller,
            Err(std::sync::TryLockError::WouldBlock) => return MEDIA_BUSY,
            Err(std::sync::TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
        };
        let geometry = match controller.geometry(&binding.caller, stream) {
            Ok(geometry) => geometry,
            Err(error) => return capture_status(&error),
        };
        // SAFETY: ABI caller supplies one aligned writable CaptureGeometry slot.
        unsafe { output.write(geometry) };
        STATUS_OK
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}

fn supports_query(query: &NativeInterfaceQuery) -> bool {
    query.header.supports::<NativeInterfaceQuery>()
        && query.interface_major == INTERFACE_MAJOR
        && query.min_minor <= CAPTURE_INTERFACE_MINOR
        && query.min_size as usize <= size_of::<CaptureApi>()
}
