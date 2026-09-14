use crate::{backend::Backend, controller::Controller};
use amaterasu_media::{
    CAPTURE_INTERFACE_MINOR, CaptureApi, CaptureGeometry, CpuFrame, FrameSink, INTERFACE_MAJOR,
    MEDIA_BACKEND_ERROR, MEDIA_BUSY, MEDIA_CLOSED, MEDIA_PENDING, MEDIA_UNSUPPORTED, PIXEL_BGRA8,
};
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, NativeInterface, NativeInterfaceQuery, NativeServiceFactory,
    STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT, STATUS_OK, Status,
};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

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
        let mut binding = Box::new(Binding {
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
        binding.api.base.context = (&mut *binding as *mut Binding<B>).cast();
        binding
    }

    fn poll_frame(&self, stream: u64) -> Result<crate::model::Frame, Status> {
        let mut controller = match self.controller.try_lock() {
            Ok(controller) => controller,
            Err(std::sync::TryLockError::WouldBlock) => return Err(MEDIA_BUSY),
            Err(_) => return Err(MEDIA_BACKEND_ERROR),
        };
        match controller.read(&self.caller, stream, 0) {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) => Err(MEDIA_PENDING),
            Err(error) => Err(capture_error_status(&error)),
        }
    }
}

fn capture_error_status(error: &crate::model::CaptureError) -> Status {
    match error.code {
        "not_found" | "access_lost" | "device_lost" => MEDIA_CLOSED,
        "unsupported_platform" | "unsupported_format" | "geometry_unavailable" => MEDIA_UNSUPPORTED,
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
        // SAFETY: the broker provides valid aligned query/output pointers for
        // this synchronous call and the provider retains its boxed factory.
        unsafe {
            *output = std::ptr::null();
        }
        let query = unsafe { &*query };
        if !supports_query(query) {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: broker caller bytes remain borrowed until this bind returns.
        let caller = match unsafe { caller.read() }
            .ok()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        {
            Some(caller) if !caller.is_empty() => caller.to_owned(),
            _ => return STATUS_INVALID_ARGUMENT,
        };
        let factory = unsafe { &*context.cast::<Factory<B>>() };
        let binding = Binding::new(factory.controller.clone(), caller);
        let table = &binding.api.base as *const NativeInterface;
        let _ = Box::into_raw(binding);
        // SAFETY: transfers one stable boxed binding, released only by release.
        unsafe {
            *output = table;
        }
        STATUS_OK
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}

unsafe extern "C" fn release<B: Backend>(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !context.is_null() {
            // SAFETY: broker releases the unique binding once, after all calls.
            unsafe {
                drop(Box::from_raw(context.cast::<Binding<B>>()));
            }
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
        // SAFETY: binding is retained by broker through this synchronous call.
        let binding = unsafe { &*context.cast::<Binding<B>>() };
        let frame = match binding.poll_frame(stream) {
            Ok(frame) => frame,
            Err(status) => return status,
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
        // SAFETY: sink is supplied by the native consumer for this call only.
        // The owned frame stays alive through callback; no controller lock is
        // held while invoking foreign code. The consumer must not retain bytes.
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
        if context.is_null()
            || output.is_null()
            || !(output as usize).is_multiple_of(align_of::<CaptureGeometry>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the broker retains this binding through the synchronous call.
        let binding = unsafe { &*context.cast::<Binding<B>>() };
        let mut controller = match binding.controller.try_lock() {
            Ok(controller) => controller,
            Err(std::sync::TryLockError::WouldBlock) => return MEDIA_BUSY,
            Err(_) => return MEDIA_BACKEND_ERROR,
        };
        let geometry = match controller.geometry(&binding.caller, stream) {
            Ok(Some(geometry)) if geometry.validate().is_ok() => geometry,
            Ok(Some(_)) => return MEDIA_BACKEND_ERROR,
            Ok(None) => return MEDIA_UNSUPPORTED,
            Err(error) => return capture_error_status(&error),
        };
        // SAFETY: output is non-null, aligned and writable by callback contract.
        unsafe { output.write(geometry) };
        STATUS_OK
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}

fn supports_query(query: &NativeInterfaceQuery) -> bool {
    if query.min_minor > CAPTURE_INTERFACE_MINOR {
        return false;
    }
    query.header.supports::<NativeInterfaceQuery>()
        && query.interface_major == INTERFACE_MAJOR
        && query.min_size as usize <= size_of::<CaptureApi>()
}
