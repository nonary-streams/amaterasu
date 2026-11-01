//! Owner-scoped plan admission into the production media transport.
use super::{Shared, guarded};
use amaterasu_media::{
    MEDIA_BACKEND_ERROR, MEDIA_BUSY, MEDIA_UNSUPPORTED, NegotiatedVideoPlanTransportApi,
    NegotiatedVideoUnit, VideoFramePlan, VideoFramingLimits, VideoPlanSelection,
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
    api: NegotiatedVideoPlanTransportApi,
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
        // SAFETY: runtime supplies live query and writable output.
        unsafe {
            *output = std::ptr::null();
        }
        let query = unsafe { &*query };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != 1
            || query.min_minor > 0
            || query.min_size as usize > size_of::<NegotiatedVideoPlanTransportApi>()
        {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: runtime lends principal bytes for this synchronous call.
        let owner = match unsafe { principal.read() }
            .ok()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        {
            Some(owner) if !owner.is_empty() && owner.len() <= 128 => owner.to_owned(),
            _ => return STATUS_INVALID_ARGUMENT,
        };
        let factory = unsafe { &*context.cast::<Factory>() };
        let mut binding = Box::new(Binding {
            api: NegotiatedVideoPlanTransportApi {
                base: NativeInterface {
                    header: AbiHeader {
                        size: size_of::<NegotiatedVideoPlanTransportApi>() as u32,
                        major: 1,
                        minor: 0,
                    },
                    context: std::ptr::null_mut(),
                    release: Some(release),
                },
                probe,
                prepare,
                submit,
                drain,
            },
            shared: factory.shared.clone(),
            owner,
        });
        binding.api.base.context = (&mut *binding as *mut Binding).cast();
        let binding = Box::into_raw(binding);
        // SAFETY: table allocation transfers to runtime until release.
        unsafe {
            *output = &(*binding).api.base;
        }
        STATUS_OK
    })
}
unsafe extern "C" fn release(context: *mut c_void) {
    let _ = guarded(|| {
        if !context.is_null() {
            unsafe {
                drop(Box::from_raw(context.cast::<Binding>()));
            }
        }
        STATUS_OK
    });
}

unsafe fn with_binding(
    context: *mut c_void,
    operation: impl FnOnce(&mut super::Controller, &str) -> Status,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: runtime retains the binding until all callbacks return.
    let binding = unsafe { &*context.cast::<Binding>() };
    let mut controller = match binding.shared.try_lock() {
        Ok(controller) => controller,
        Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
        Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
    };
    operation(&mut controller, &binding.owner)
}

unsafe extern "C" fn probe(
    context: *mut c_void,
    stream: u64,
    selection: *const VideoPlanSelection,
    limits: *mut VideoFramingLimits,
) -> Status {
    plan_limits(context, stream, selection, limits, false)
}
unsafe extern "C" fn prepare(
    context: *mut c_void,
    stream: u64,
    selection: *const VideoPlanSelection,
    limits: *mut VideoFramingLimits,
) -> Status {
    plan_limits(context, stream, selection, limits, true)
}
fn plan_limits(
    context: *mut c_void,
    stream: u64,
    selection: *const VideoPlanSelection,
    limits: *mut VideoFramingLimits,
    commit: bool,
) -> Status {
    guarded(|| {
        if selection.is_null()
            || limits.is_null()
            || !(selection as usize).is_multiple_of(align_of::<VideoPlanSelection>())
            || !(limits as usize).is_multiple_of(align_of::<VideoFramingLimits>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: authorized caller keeps descriptors live and output writable.
        unsafe {
            with_binding(context, |controller, owner| {
                let result = if commit {
                    controller.prepare_legacy_video_plan(owner, stream, &*selection)
                } else {
                    controller.probe_legacy_video_plan(owner, stream, &*selection)
                };
                match result {
                    Ok(selected) => {
                        limits.write(selected);
                        STATUS_OK
                    }
                    Err(status) => status,
                }
            })
        }
    })
}
unsafe extern "C" fn submit(
    context: *mut c_void,
    stream: u64,
    unit: *const NegotiatedVideoUnit,
    plan: *const VideoFramePlan,
) -> Status {
    guarded(|| {
        if unit.is_null()
            || plan.is_null()
            || !(unit as usize).is_multiple_of(align_of::<NegotiatedVideoUnit>())
            || !(plan as usize).is_multiple_of(align_of::<VideoFramePlan>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller retains both headers and their nested byte borrows.
        unsafe {
            with_binding(context, |controller, owner| {
                controller.submit_video_plan(owner, stream, &*unit, &*plan)
            })
        }
    })
}
unsafe extern "C" fn drain(context: *mut c_void, stream: u64) -> Status {
    guarded(|| unsafe {
        with_binding(context, |controller, owner| controller.drain(owner, stream))
    })
}
