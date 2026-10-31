use crate::state::State;
use amaterasu_abi::*;
use amaterasu_media::*;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
};

pub type Shared = Arc<Mutex<State>>;
pub struct Registration {
    pub factory: NativeServiceFactory,
    _context: Box<Shared>,
}
impl Registration {
    pub fn new(shared: Shared) -> Self {
        let mut context = Box::new(shared);
        let factory = NativeServiceFactory {
            header: AbiHeader::of::<NativeServiceFactory>(),
            context: (&mut *context as *mut Shared).cast(),
            bind: Some(bind),
        };
        Self {
            factory,
            _context: context,
        }
    }
}
struct Binding {
    api: EncoderApi,
    owner: String,
    shared: Shared,
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
        // SAFETY: the runtime supplies valid pointers and principal storage for this call.
        unsafe {
            *output = std::ptr::null();
            let query = &*query;
            if !query.header.supports::<NativeInterfaceQuery>()
                || query.interface_major != INTERFACE_MAJOR
                || query.min_minor > 0
                || query.min_size > ENCODER_API_V1_0_SIZE
            {
                return MEDIA_UNSUPPORTED;
            }
            let Some(owner) = principal
                .read()
                .ok()
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .filter(|s| !s.is_empty() && s.len() <= 128)
            else {
                return STATUS_INVALID_ARGUMENT;
            };
            let shared = (&*context.cast::<Shared>()).clone();
            let mut binding = Box::new(Binding {
                api: EncoderApi {
                    base: NativeInterface {
                        header: AbiHeader::of::<EncoderApi>(),
                        context: std::ptr::null_mut(),
                        release: Some(release),
                    },
                    submit,
                    receive,
                    drain,
                },
                owner: owner.into(),
                shared,
            });
            binding.api.base.context = (&mut *binding as *mut Binding).cast();
            let binding = Box::into_raw(binding);
            *output = &(*binding).api.base;
        }
        STATUS_OK
    })
}
unsafe extern "C" fn release(context: *mut c_void) {
    let _ = guarded(|| {
        if !context.is_null() {
            // SAFETY: runtime calls release once after all calls through this binding finish.
            drop(unsafe { Box::from_raw(context.cast::<Binding>()) });
        }
        STATUS_OK
    });
}
unsafe extern "C" fn submit(
    context: *mut c_void,
    id: u64,
    frame: *const CpuFrame,
    force: u32,
) -> Status {
    guarded(|| {
        if context.is_null() || frame.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime keeps binding and borrowed frame alive during the call.
        let binding = unsafe { &*context.cast::<Binding>() };
        let Ok(mut state) = binding.shared.try_lock() else {
            return MEDIA_BUSY;
        };
        // SAFETY: caller supplies a live frame descriptor.
        state.submit(&binding.owner, id, unsafe { &*frame }, force)
    })
}
unsafe extern "C" fn receive(context: *mut c_void, id: u64, sink: PacketSink) -> Status {
    guarded(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime keeps the binding alive during the call.
        let binding = unsafe { &*context.cast::<Binding>() };
        let Ok(mut state) = binding.shared.try_lock() else {
            return MEDIA_BUSY;
        };
        // SAFETY: the caller supplies a live callback/context for this call.
        unsafe { state.receive(&binding.owner, id, sink) }
    })
}
unsafe extern "C" fn drain(context: *mut c_void, id: u64) -> Status {
    guarded(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime keeps the binding alive during the call.
        let binding = unsafe { &*context.cast::<Binding>() };
        let Ok(mut state) = binding.shared.try_lock() else {
            return MEDIA_BUSY;
        };
        state.drain(&binding.owner, id)
    })
}
fn guarded(operation: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(STATUS_INTERNAL_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_bindings_use_runtime_principal_for_session_ownership() {
        let shared = Arc::new(Mutex::new(State::default()));
        let request = json!({"action":"open","encoder":{"clsid":"example.fake-h264","hardware_url":null,"friendly_name":"Example fake H.264"},"configuration":{"width":16,"height":16,"fps":60,"bitrate":1000000},"operation_id":"op"});
        let id = shared.lock().unwrap().control("alice", request).0["session_id"]
            .as_u64()
            .unwrap();
        let registration = Registration::new(shared);
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: INTERFACE_MAJOR,
            min_minor: 0,
            min_size: ENCODER_API_V1_0_SIZE,
        };
        let mut pointers = Vec::new();
        for owner in ["alice", "bob"] {
            let mut pointer = std::ptr::null();
            // SAFETY: factory, query, principal and output all live through bind.
            assert_eq!(
                unsafe {
                    registration.factory.bind.unwrap()(
                        registration.factory.context,
                        BorrowedBytes::new(owner.as_bytes()),
                        &query,
                        &mut pointer,
                    )
                },
                STATUS_OK
            );
            assert!(!pointer.is_null());
            pointers.push(pointer);
        }
        // SAFETY: these pointers are successful live EncoderApi v1 bindings.
        let alice = unsafe { &*pointers[0].cast::<EncoderApi>() };
        // SAFETY: same contract for the second binding.
        let bob = unsafe { &*pointers[1].cast::<EncoderApi>() };
        // SAFETY: bindings remain live for each call.
        assert_eq!(unsafe { (bob.drain)(bob.base.context, id) }, MEDIA_CLOSED);
        // SAFETY: bindings remain live for each call.
        assert_eq!(unsafe { (alice.drain)(alice.base.context, id) }, STATUS_OK);
        for pointer in pointers {
            // SAFETY: each successfully bound interface is released exactly once.
            unsafe { ((*pointer).release.unwrap())((*pointer).context) };
        }
    }
}
