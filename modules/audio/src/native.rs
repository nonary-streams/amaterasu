use crate::controller::Controller;
use amaterasu_media::{
    AudioCaptureApi, AudioSink, INTERFACE_MAJOR, INTERFACE_MINOR, MEDIA_BACKEND_ERROR, MEDIA_BUSY,
    MEDIA_UNSUPPORTED,
};
use amaterasu_module_sdk::abi::*;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, TryLockError},
};

pub mod ordered;

pub type Shared = Arc<Mutex<Controller>>;
pub struct Factory {
    pub api: NativeServiceFactory,
    shared: Shared,
}

impl Factory {
    pub fn new(shared: Shared) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind),
            },
            shared,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}

struct Binding {
    api: AudioCaptureApi,
    shared: Shared,
    owner: String,
}

unsafe extern "C" fn bind(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guarded(|| {
        if context.is_null() || query.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime provides live factory/query/output pointers and a
        // callback-lifetime principal; the binding copies principal and Arc.
        unsafe {
            *output = std::ptr::null();
            let query = &*query;
            if !query.header.supports::<NativeInterfaceQuery>()
                || query.interface_major != INTERFACE_MAJOR
                || query.min_minor > INTERFACE_MINOR
                || query.min_size as usize > size_of::<AudioCaptureApi>()
            {
                return MEDIA_UNSUPPORTED;
            }
            let owner = match principal
                .read()
                .ok()
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
            {
                Some(owner) if !owner.is_empty() && owner.len() <= 128 => owner.to_owned(),
                _ => return STATUS_INVALID_ARGUMENT,
            };
            let shared = (&*context.cast::<Factory>()).shared.clone();
            let mut binding = Box::new(Binding {
                api: AudioCaptureApi {
                    base: NativeInterface {
                        header: AbiHeader {
                            size: size_of::<AudioCaptureApi>() as u32,
                            major: INTERFACE_MAJOR,
                            minor: INTERFACE_MINOR,
                        },
                        context: std::ptr::null_mut(),
                        release: Some(release),
                    },
                    read_audio,
                },
                shared,
                owner,
            });
            binding.api.base.context = (&mut *binding as *mut Binding).cast();
            let pointer = Box::into_raw(binding);
            *output = &(*pointer).api.base;
        }
        STATUS_OK
    })
}

unsafe extern "C" fn release(context: *mut c_void) {
    let _ = guarded(|| {
        if !context.is_null() {
            // SAFETY: runtime releases this unique binding after its calls end.
            drop(unsafe { Box::from_raw(context.cast::<Binding>()) });
        }
        STATUS_OK
    });
}

unsafe extern "C" fn read_audio(context: *mut c_void, stream: u64, sink: AudioSink) -> Status {
    guarded(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: binding and sink remain live throughout native call.
        let binding = unsafe { &*context.cast::<Binding>() };
        let mut controller = match binding.shared.try_lock() {
            Ok(controller) => controller,
            Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
            Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
        };
        unsafe { controller.read(&binding.owner, stream, sink) }
    })
}

fn guarded(operation: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(STATUS_INTERNAL_ERROR)
}
