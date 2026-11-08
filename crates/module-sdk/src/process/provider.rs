//! Authoring adapter for a trusted, process-lifetime provider implementation.
use super::api;
use amaterasu_abi::{
    AbiHeader, BorrowedBytes, NativeInterface, NativeInterfaceQuery, NativeServiceFactory,
    ReplySink, STATUS_INVALID_ARGUMENT, STATUS_OK, Status,
};
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

/// Native provider owns children, sink leases and operation fingerprints beyond
/// all bindings. Methods must be nonblocking and atomically reject admission
/// before side effects on every non-OK return. `submit` must not synchronously
/// reenter the submitting binding's SDK admission methods. No grant sandboxes
/// native code.
pub trait Provider: Send + Sync {
    fn capabilities(&self) -> Result<api::Capabilities, Status>;
    fn open(&self, caller: &str) -> Result<String, Status>;
    fn close(&self, caller: &str, epoch: &str);
    fn submit(
        &self,
        caller: &str,
        epoch: &str,
        operation_id: &str,
        request: api::Request,
    ) -> Result<(), Status>;
    fn poll(&self, caller: &str, operation_id: &str) -> Result<api::Response, Status>;
    fn inspect(&self, caller: &str, process: &api::ProcessToken) -> Result<api::Response, Status>;
    fn release_operation(&self, caller: &str, operation_id: &str) -> Result<(), Status>;
}

pub struct ProcessFactory<P: Provider> {
    pub api: NativeServiceFactory,
    provider: Arc<P>,
}
impl<P: Provider + 'static> ProcessFactory<P> {
    pub fn new(provider: Arc<P>) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind::<P>),
            },
            provider,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
    /// # Safety
    /// Keep the factory and provider code alive until the returned table is released.
    pub unsafe fn bind_standalone(&self, caller: &str) -> Result<*const api::ProcessApi, Status> {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: api::MAJOR,
            min_minor: api::MINOR,
            min_size: size_of::<api::ProcessApi>() as u32,
        };
        let mut output = std::ptr::null();
        // SAFETY: arguments are live for the call and factory lifetime is caller-owned.
        let status = unsafe {
            bind::<P>(
                self.api.context,
                BorrowedBytes::new(caller.as_bytes()),
                &query,
                &mut output,
            )
        };
        if status == STATUS_OK {
            Ok(output.cast())
        } else {
            Err(status)
        }
    }
}
#[repr(C)]
struct Binding<P: Provider> {
    api: api::ProcessApi,
    provider: Arc<P>,
    caller: String,
    epoch: String,
}

// A panic may happen after a provider admitted work; returning an error would
// falsely prove non-admission. Native provider panic is fatal at this boundary.
fn boundary(f: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| std::process::abort())
}
fn failure(status: Status) -> Status {
    if status == STATUS_OK {
        std::process::abort();
    }
    status
}
fn text(bytes: BorrowedBytes) -> Result<String, Status> {
    if bytes.len > api::MAX_TOKEN_BYTES {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    // SAFETY: callback contract lends readable bytes for this synchronous call.
    let bytes = unsafe { bytes.read()? };
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| STATUS_INVALID_ARGUMENT)
}
fn reply<T: serde::Serialize>(sink: ReplySink, value: &T) -> Status {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return amaterasu_abi::STATUS_INTERNAL_ERROR;
    };
    if bytes.len() > api::MAX_PAYLOAD_BYTES {
        return amaterasu_abi::STATUS_INTERNAL_ERROR;
    }
    // SAFETY: sink and response buffer are borrowed during this call only.
    unsafe { sink.send(&bytes) };
    STATUS_OK
}
unsafe extern "C" fn bind<P: Provider + 'static>(
    context: *mut c_void,
    caller: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    out: *mut *const NativeInterface,
) -> Status {
    boundary(|| {
        if context.is_null() || query.is_null() || out.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller supplies valid query/output and retains factory for bind.
        let query = unsafe { &*query };
        unsafe { out.write(std::ptr::null()) };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != api::MAJOR
            || query.min_minor > api::MINOR
            || query.min_size as usize > size_of::<api::ProcessApi>()
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let Ok(caller) = text(caller) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if !api::valid_identity(&caller) {
            return STATUS_INVALID_ARGUMENT;
        }
        let provider = unsafe { &*context.cast::<ProcessFactory<P>>() }
            .provider
            .clone();
        let epoch = match provider.open(&caller) {
            Ok(epoch) => epoch,
            Err(status) => return failure(status),
        };
        if !api::valid_operation_id(&format!("{epoch}:1")) {
            provider.close(&caller, &epoch);
            return STATUS_INVALID_ARGUMENT;
        }
        let mut binding = Box::new(Binding {
            api: api::ProcessApi {
                base: NativeInterface {
                    header: AbiHeader {
                        size: size_of::<api::ProcessApi>() as u32,
                        major: api::MAJOR,
                        minor: api::MINOR,
                    },
                    context: std::ptr::null_mut(),
                    release: Some(release::<P>),
                },
                capabilities: capabilities::<P>,
                epoch: get_epoch::<P>,
                submit: submit::<P>,
                poll: poll::<P>,
                inspect: inspect::<P>,
                release_operation: release_operation::<P>,
            },
            provider,
            caller,
            epoch,
        });
        binding.api.base.context = (&mut *binding as *mut Binding<P>).cast();
        let ptr = &binding.api.base as *const NativeInterface;
        let _ = Box::into_raw(binding);
        unsafe { out.write(ptr) };
        STATUS_OK
    })
}
unsafe extern "C" fn release<P: Provider>(context: *mut c_void) {
    boundary(|| {
        if context.is_null() {
            std::process::abort();
        }
        let binding = unsafe { Box::from_raw(context.cast::<Binding<P>>()) };
        binding.provider.close(&binding.caller, &binding.epoch);
        STATUS_OK
    });
}
unsafe extern "C" fn capabilities<P: Provider>(context: *mut c_void, sink: ReplySink) -> Status {
    boundary(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let b = unsafe { &*context.cast::<Binding<P>>() };
        match b.provider.capabilities() {
            Ok(value) if value.validate().is_ok() => reply(sink, &value),
            Ok(_) => amaterasu_abi::STATUS_INTERNAL_ERROR,
            Err(status) => failure(status),
        }
    })
}
unsafe extern "C" fn get_epoch<P: Provider>(context: *mut c_void, sink: ReplySink) -> Status {
    boundary(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let b = unsafe { &*context.cast::<Binding<P>>() };
        reply(sink, &b.epoch)
    })
}
unsafe extern "C" fn submit<P: Provider>(
    context: *mut c_void,
    id: BorrowedBytes,
    input: BorrowedBytes,
) -> Status {
    boundary(|| {
        if context.is_null() || input.len > api::MAX_PAYLOAD_BYTES {
            return STATUS_INVALID_ARGUMENT;
        }
        let b = unsafe { &*context.cast::<Binding<P>>() };
        let Ok(id) = text(id) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if !api::valid_operation_id(&id)
            || id.split_once(':').map(|(epoch, _)| epoch) != Some(b.epoch.as_str())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let Ok(bytes) = (unsafe { input.read() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Ok(request) = serde_json::from_slice::<api::Request>(bytes) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if request.validate().is_err() {
            return STATUS_INVALID_ARGUMENT;
        }
        match b.provider.submit(&b.caller, &b.epoch, &id, request) {
            Ok(()) => STATUS_OK,
            Err(status) => failure(status),
        }
    })
}
unsafe extern "C" fn poll<P: Provider>(
    context: *mut c_void,
    id: BorrowedBytes,
    sink: ReplySink,
) -> Status {
    boundary(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let b = unsafe { &*context.cast::<Binding<P>>() };
        let Ok(id) = text(id) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if !api::valid_operation_id(&id) {
            return STATUS_INVALID_ARGUMENT;
        }
        match b.provider.poll(&b.caller, &id) {
            Ok(value) if value.validate().is_ok() => reply(sink, &value),
            Ok(_) => amaterasu_abi::STATUS_INTERNAL_ERROR,
            Err(status) => failure(status),
        }
    })
}
unsafe extern "C" fn inspect<P: Provider>(
    context: *mut c_void,
    token: BorrowedBytes,
    sink: ReplySink,
) -> Status {
    boundary(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let b = unsafe { &*context.cast::<Binding<P>>() };
        let Ok(token) = text(token) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if !api::valid_identity(&token) {
            return STATUS_INVALID_ARGUMENT;
        }
        match b.provider.inspect(&b.caller, &api::ProcessToken(token)) {
            Ok(value) if value.validate().is_ok() => reply(sink, &value),
            Ok(_) => amaterasu_abi::STATUS_INTERNAL_ERROR,
            Err(status) => failure(status),
        }
    })
}
unsafe extern "C" fn release_operation<P: Provider>(
    context: *mut c_void,
    id: BorrowedBytes,
) -> Status {
    boundary(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let b = unsafe { &*context.cast::<Binding<P>>() };
        let Ok(id) = text(id) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if !api::valid_operation_id(&id) {
            return STATUS_INVALID_ARGUMENT;
        }
        match b.provider.release_operation(&b.caller, &id) {
            Ok(()) => STATUS_OK,
            Err(status) => failure(status),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_identity_is_rejected_before_reading_or_allocating() {
        let bytes = BorrowedBytes {
            ptr: std::ptr::null(),
            len: api::MAX_TOKEN_BYTES + 1,
        };
        assert_eq!(text(bytes), Err(STATUS_INVALID_ARGUMENT));
    }
}
