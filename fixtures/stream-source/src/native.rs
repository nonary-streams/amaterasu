use crate::controller::{Controller, Kind};
use amaterasu_media::*;
use amaterasu_module_sdk::abi::*;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, TryLockError},
};

pub type Shared = Arc<Mutex<Controller>>;

pub struct Factory {
    pub api: NativeServiceFactory,
    state: Shared,
    kind: Kind,
}

impl Factory {
    pub fn new(state: Shared, kind: Kind) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind),
            },
            state,
            kind,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}

struct Binding {
    state: Shared,
    owner: String,
    video: VideoApi,
    audio: AudioCaptureApi,
}

unsafe extern "C" fn bind(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guarded(|| {
        if output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller supplies writable output and retains factory/query bytes.
        unsafe {
            *output = std::ptr::null();
        }
        if context.is_null() || query.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let factory = unsafe { &*context.cast::<Factory>() };
        let query = unsafe { &*query };
        let size = match factory.kind {
            Kind::Video => size_of::<VideoApi>(),
            Kind::Audio => size_of::<AudioCaptureApi>(),
        };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != 1
            || query.min_minor != 0
            || query.min_size as usize > size
        {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: runtime borrows its authenticated immediate principal for bind.
        let Some(owner) = (unsafe { principal.read() })
            .ok()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .filter(|owner| !owner.is_empty() && owner.len() <= 128)
        else {
            return STATUS_INVALID_ARGUMENT;
        };
        let mut binding = Box::new(Binding {
            state: factory.state.clone(),
            owner: owner.into(),
            video: VideoApi {
                base: prefix::<VideoApi>(),
                receive: video,
            },
            audio: AudioCaptureApi {
                base: prefix::<AudioCaptureApi>(),
                read_audio: audio,
            },
        });
        let context = (&mut *binding as *mut Binding).cast();
        binding.video.base.context = context;
        binding.audio.base.context = context;
        let interface = match factory.kind {
            Kind::Video => &binding.video.base,
            Kind::Audio => &binding.audio.base,
        } as *const NativeInterface;
        let _ = Box::into_raw(binding);
        // SAFETY: boxed binding owns the selected table until unique release.
        unsafe {
            *output = interface;
        }
        STATUS_OK
    })
}

fn prefix<T>() -> NativeInterface {
    NativeInterface {
        header: AbiHeader::of::<T>(),
        context: std::ptr::null_mut(),
        release: Some(release),
    }
}

unsafe extern "C" fn release(context: *mut c_void) {
    if !context.is_null() {
        // SAFETY: runtime releases the unique binding once after callbacks stop.
        drop(unsafe { Box::from_raw(context.cast::<Binding>()) });
    }
}

unsafe extern "C" fn video(context: *mut c_void, stream: u64, sink: PacketSink) -> Status {
    guarded(|| {
        // SAFETY: caller retains binding/sink for synchronous delivery only.
        unsafe { access(context, |state, owner| state.video(owner, stream, sink)) }
    })
}
unsafe extern "C" fn audio(context: *mut c_void, stream: u64, sink: AudioSink) -> Status {
    guarded(|| {
        // SAFETY: caller retains binding and audio callback through return.
        unsafe { access(context, |state, owner| state.audio(owner, stream, sink)) }
    })
}

unsafe fn access(
    context: *mut c_void,
    operation: impl FnOnce(&mut Controller, &str) -> Status,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: context points to the live immutable native binding.
    let binding = unsafe { &*context.cast::<Binding>() };
    match binding.state.try_lock() {
        Ok(mut state) => operation(&mut state, &binding.owner),
        Err(TryLockError::WouldBlock) => MEDIA_BUSY,
        Err(TryLockError::Poisoned(_)) => MEDIA_BACKEND_ERROR,
    }
}

fn guarded(operation: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(MEDIA_BACKEND_ERROR)
}
