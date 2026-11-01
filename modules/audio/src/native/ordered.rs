//! Owner-scoped ordered PCM capture, separate from the legacy table.
use super::{Shared, guarded};
use amaterasu_media::{
    MEDIA_BACKEND_ERROR, MEDIA_BUSY, MEDIA_UNSUPPORTED, OrderedAudioCaptureApiV2,
    OrderedAudioSinkV2,
};
use amaterasu_module_sdk::abi::*;
use std::{ffi::c_void, sync::TryLockError};

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
    api: OrderedAudioCaptureApiV2,
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
        if context.is_null()
            || query.is_null()
            || output.is_null()
            || !(query as usize).is_multiple_of(align_of::<NativeInterfaceQuery>())
            || !(output as usize).is_multiple_of(align_of::<*const NativeInterface>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime supplies initialized query and writable output.
        let query = unsafe { &*query };
        unsafe { *output = std::ptr::null() };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != 2
            || query.min_minor > 0
            || query.min_size as usize > size_of::<OrderedAudioCaptureApiV2>()
        {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: runtime lends principal bytes for this call.
        let owner = match unsafe { principal.read() }
            .ok()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        {
            Some(owner) if !owner.is_empty() && owner.len() <= 128 => owner.to_owned(),
            _ => return STATUS_INVALID_ARGUMENT,
        };
        // SAFETY: factory is retained while bindings are active.
        let factory = unsafe { &*context.cast::<Factory>() };
        let mut binding = Box::new(Binding {
            api: OrderedAudioCaptureApiV2 {
                base: NativeInterface {
                    header: AbiHeader {
                        size: size_of::<OrderedAudioCaptureApiV2>() as u32,
                        major: 2,
                        minor: 0,
                    },
                    context: std::ptr::null_mut(),
                    release: Some(release),
                },
                read_audio,
            },
            shared: factory.shared.clone(),
            owner,
        });
        binding.api.base.context = (&mut *binding as *mut Binding).cast();
        let binding = Box::into_raw(binding);
        // SAFETY: ownership transfers to runtime until release.
        unsafe { *output = &(*binding).api.base };
        STATUS_OK
    })
}

unsafe extern "C" fn release(context: *mut c_void) {
    let _ = guarded(|| {
        if !context.is_null() {
            // SAFETY: runtime releases the unique binding after calls retire.
            unsafe { drop(Box::from_raw(context.cast::<Binding>())) };
        }
        STATUS_OK
    });
}

unsafe extern "C" fn read_audio(
    context: *mut c_void,
    stream: u64,
    sink: OrderedAudioSinkV2,
) -> Status {
    guarded(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: binding remains live until all callbacks retire.
        let binding = unsafe { &*context.cast::<Binding>() };
        let mut controller = match binding.shared.try_lock() {
            Ok(controller) => controller,
            Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
            Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
        };
        // SAFETY: sink and samples remain live throughout the synchronous call.
        unsafe { controller.read_ordered(&binding.owner, stream, sink) }
    })
}
