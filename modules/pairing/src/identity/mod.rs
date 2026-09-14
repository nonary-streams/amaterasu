//! Native TLS identity boundary: immutable bindings share only committed snapshots.
mod snapshot;
use amaterasu_identity::*;
use amaterasu_module_sdk::abi::*;
pub use snapshot::{Shared, Snapshot};
use std::{
    collections::BTreeSet,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

pub struct Factory {
    pub api: NativeServiceFactory,
    shared: Arc<Shared>,
    transports: BTreeSet<String>,
}
impl Factory {
    pub fn new(shared: Arc<Shared>, transports: BTreeSet<String>) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind),
            },
            shared,
            transports,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}
struct Binding {
    api: IdentityApi,
    shared: Arc<Shared>,
}
unsafe extern "C" fn bind(
    context: *mut c_void,
    caller: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guarded(|| {
        if context.is_null() || query.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime borrows query/output and provider retains boxed factory.
        unsafe {
            *output = std::ptr::null();
        }
        let factory = unsafe { &*context.cast::<Factory>() };
        let caller = match unsafe { caller.read() }
            .ok()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        {
            Some(caller) => caller,
            None => return STATUS_INVALID_ARGUMENT,
        };
        if !factory.transports.contains(caller) {
            return IDENTITY_FORBIDDEN;
        }
        let query = unsafe { &*query };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != INTERFACE_MAJOR
            || query.min_minor > INTERFACE_MINOR
            || query.min_size as usize > size_of::<IdentityApi>()
        {
            return IDENTITY_UNSUPPORTED;
        }
        let mut binding = Box::new(Binding {
            api: IdentityApi {
                base: NativeInterface {
                    header: AbiHeader::of::<IdentityApi>(),
                    context: std::ptr::null_mut(),
                    release: Some(release),
                },
                certificate,
                sign,
                verify_client,
            },
            shared: factory.shared.clone(),
        });
        binding.api.base.context = (&mut *binding as *mut Binding).cast();
        let table = &binding.api.base as *const NativeInterface;
        let _ = Box::into_raw(binding);
        // SAFETY: transfer one provider allocation, released once after calls stop.
        unsafe {
            *output = table;
        }
        STATUS_OK
    })
}
unsafe extern "C" fn release(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !context.is_null() {
            // SAFETY: runtime releases the unique binding after concurrent calls finish.
            unsafe {
                drop(Box::from_raw(context.cast::<Binding>()));
            }
        }
    }));
}
unsafe extern "C" fn certificate(context: *mut c_void, sink: ReplySink) -> Status {
    guarded_result(|| {
        // SAFETY: native consumer retains this binding throughout the call.
        let binding = unsafe { binding(context) }?;
        let snapshot = binding.shared.get(binding.shared.now()?)?;
        if !binding.shared.current(&snapshot, binding.shared.now()?) {
            return Err(IDENTITY_UNAVAILABLE);
        }
        // SAFETY: snapshot pins DER bytes and consumer supplies live sink. No lock
        // is held while invoking foreign code; bytes may not be retained by sink.
        unsafe {
            sink.send(snapshot.certificate());
        }
        Ok(())
    })
}
unsafe extern "C" fn sign(
    context: *mut c_void,
    scheme: u32,
    message: BorrowedBytes,
    sink: ReplySink,
) -> Status {
    guarded_result(|| {
        if message.len > MAX_MESSAGE_BYTES {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: native consumer owns message bytes for this synchronous call.
        let message = unsafe { message.read() }?;
        let binding = unsafe { binding(context) }?;
        let snapshot = binding.shared.get(binding.shared.now()?)?;
        let signature = snapshot.sign(scheme, message)?;
        if !binding.shared.current(&snapshot, binding.shared.now()?) {
            return Err(IDENTITY_UNAVAILABLE);
        }
        // SAFETY: owned signature and consumer sink remain live until callback returns.
        unsafe {
            sink.send(&signature);
        }
        Ok(())
    })
}
unsafe extern "C" fn verify_client(context: *mut c_void, certificate: BorrowedBytes) -> Status {
    guarded_result(|| {
        if certificate.len == 0 || certificate.len > MAX_CERTIFICATE_BYTES {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: native consumer supplies readable DER bytes for this call.
        let certificate = unsafe { certificate.read() }?;
        let binding = unsafe { binding(context) }?;
        let snapshot = binding.shared.get(binding.shared.now()?)?;
        let now = binding.shared.now()?;
        if !snapshot.verify_client(certificate, now) {
            return Err(IDENTITY_UNTRUSTED);
        }
        if !binding.shared.current(&snapshot, now) {
            return Err(IDENTITY_UNAVAILABLE);
        }
        Ok(())
    })
}
unsafe fn binding<'a>(context: *mut c_void) -> Result<&'a Binding, Status> {
    if context.is_null() {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    // SAFETY: native binding contract guarantees allocation lifetime and alignment.
    Ok(unsafe { &*context.cast::<Binding>() })
}
fn guarded(operation: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(STATUS_INTERNAL_ERROR)
}
fn guarded_result(operation: impl FnOnce() -> Result<(), Status>) -> Status {
    guarded(|| operation().map_or_else(|status| status, |()| STATUS_OK))
}

#[cfg(test)]
mod tests;
