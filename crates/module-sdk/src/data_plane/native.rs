//! C boundary, principal capture and synchronous teardown barriers.
use super::{Channel, Hub, NativeFactory, abi::*};
use amaterasu_abi::{
    AbiHeader, BorrowedBytes, NativeInterface, NativeInterfaceQuery, STATUS_INTERNAL_ERROR,
    STATUS_INVALID_ARGUMENT, STATUS_OK, Status,
};
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

#[repr(C)]
struct Binding {
    api: TransferApi,
    hub: Hub,
    owner: String,
    id: u64,
}

pub(super) unsafe extern "C" fn bind(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guarded(|| {
        if context.is_null() || query.is_null() || output.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: runtime guarantees a live factory, readable query and writable output.
        let (factory, query) = unsafe {
            *output = std::ptr::null();
            (&*context.cast::<NativeFactory>(), &*query)
        };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != INTERFACE_MAJOR
            || query.min_minor > INTERFACE_MINOR
            || query.min_size > API_SIZE
        {
            return Err(UNSUPPORTED);
        }
        // SAFETY: principal is runtime-attested and borrowed during this callback.
        let owner = std::str::from_utf8(unsafe { principal.read()? })
            .map_err(|_| STATUS_INVALID_ARGUMENT)?;
        if !amaterasu_contracts::valid_name(owner) {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let id = factory.hub.new_binding()?;
        let mut binding = Box::new(Binding {
            api: TransferApi {
                base: NativeInterface {
                    header: AbiHeader {
                        size: API_SIZE,
                        major: INTERFACE_MAJOR,
                        minor: INTERFACE_MINOR,
                    },
                    context: std::ptr::null_mut(),
                    release: Some(release),
                },
                describe,
                submit,
                receive,
                snapshot,
                wait,
                finish,
                cancel,
            },
            hub: factory.hub.clone(),
            owner: owner.into(),
            id,
        });
        binding.api.base.context = (&mut *binding as *mut Binding).cast();
        // SAFETY: repr(C) places the stable table first; release owns the box.
        unsafe { *output = Box::into_raw(binding).cast() };
        Ok(())
    })
}

unsafe fn channel(context: *mut c_void, id: ChannelId) -> Result<Channel, Status> {
    if context.is_null() {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    // SAFETY: native caller holds the live binding through all concurrent calls.
    let binding = unsafe { &*context.cast::<Binding>() };
    binding.hub.channel(&binding.owner, binding.id, id)
}
unsafe extern "C" fn describe(
    context: *mut c_void,
    id: ChannelId,
    output: *mut Description,
) -> Status {
    guarded(|| {
        if output.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: caller supplies writable output and a live binding.
        unsafe { *output = channel(context, id)?.description() };
        Ok(())
    })
}
unsafe extern "C" fn submit(context: *mut c_void, id: ChannelId, chunk: *const Chunk) -> Status {
    guarded(|| {
        if chunk.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: caller guarantees live binding; domain ownership is checked first.
        let channel = unsafe { channel(context, id)? };
        if channel.description().direction != SINK {
            return Err(DENIED);
        }
        // SAFETY: native caller guarantees descriptor/lease ownership contract.
        result(unsafe { channel.submit(&*chunk) })
    })
}
unsafe extern "C" fn receive(context: *mut c_void, id: ChannelId, sink: ChunkSink) -> Status {
    guarded(|| {
        // SAFETY: caller retains binding and sink through this synchronous call.
        let channel = unsafe { channel(context, id)? };
        if channel.description().direction != SOURCE {
            return Err(DENIED);
        }
        result(unsafe { channel.receive(|chunk| (sink.write)(sink.context, chunk)) })
    })
}
unsafe extern "C" fn snapshot(
    context: *mut c_void,
    id: ChannelId,
    output: *mut Snapshot,
) -> Status {
    // SAFETY: wait with zero timeout performs an immediate snapshot.
    unsafe { wait(context, id, 0, 0, output) }
}
unsafe extern "C" fn wait(
    context: *mut c_void,
    id: ChannelId,
    generation: u64,
    timeout_ms: u32,
    output: *mut Snapshot,
) -> Status {
    guarded(|| {
        if output.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: caller supplies writable output and retains binding during wait.
        unsafe { *output = channel(context, id)?.wait(generation, timeout_ms)? };
        Ok(())
    })
}
unsafe extern "C" fn finish(context: *mut c_void, id: ChannelId) -> Status {
    guarded(|| {
        // SAFETY: caller retains the binding.
        let channel = unsafe { channel(context, id)? };
        if channel.description().direction != SINK {
            return Err(DENIED);
        }
        channel.finish()
    })
}
unsafe extern "C" fn cancel(context: *mut c_void, id: ChannelId) -> Status {
    guarded(|| {
        // SAFETY: caller retains binding; denied/unknown IDs have no authority.
        unsafe { channel(context, id)? }.cancel();
        Ok(())
    })
}
unsafe extern "C" fn release(context: *mut c_void) {
    barrier(|| {
        if context.is_null() {
            std::process::abort();
        }
        // SAFETY: runtime releases exactly once after ending concurrent calls.
        let binding = unsafe { Box::from_raw(context.cast::<Binding>()) };
        binding.hub.release_binding(binding.id);
    });
}
fn guarded(operation: impl FnOnce() -> Result<(), Status>) -> Status {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(())) => STATUS_OK,
        Ok(Err(status)) => status,
        Err(_) => STATUS_INTERNAL_ERROR,
    }
}
fn barrier(operation: impl FnOnce()) {
    // A failed retirement must not allow the host to unload still-live code.
    if catch_unwind(AssertUnwindSafe(operation)).is_err() {
        std::process::abort();
    }
}
fn result(status: Status) -> Result<(), Status> {
    if status == STATUS_OK {
        Ok(())
    } else {
        Err(status)
    }
}
