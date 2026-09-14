//! Native pointer handling is confined to this trusted-code ABI boundary.
//! Registry locks protect metadata only; no provider callback runs under a lock.
use amaterasu_abi::*;
use amaterasu_contracts::{Command, Grants, Response, valid_name};
use std::collections::BTreeMap;
use std::sync::Mutex;

const MAX_FACTORIES: usize = 256;
const MAX_BINDINGS: usize = 1024;

type BindCallback = unsafe extern "C" fn(
    *mut std::ffi::c_void,
    BorrowedBytes,
    *const NativeInterfaceQuery,
    *mut *const NativeInterface,
) -> Status;

/// Copy the small descriptor at registration. Only provider context/code need
/// stable lifetimes; moving a Rust wrapper cannot invalidate the registry entry.
#[derive(Clone, Copy)]
struct Factory {
    context: usize,
    bind: BindCallback,
}

struct Binding {
    owner: String,
    interface: Option<usize>,
}

#[derive(Default)]
struct Registry {
    factories: BTreeMap<String, Factory>,
    bindings: BTreeMap<u64, Binding>,
    next_binding: u64,
}

pub(crate) struct NativeServices {
    grants: BTreeMap<String, Grants>,
    registry: Mutex<Registry>,
}

impl NativeServices {
    pub fn new(grants: BTreeMap<String, Grants>) -> Self {
        Self {
            grants,
            registry: Mutex::new(Registry::default()),
        }
    }

    /// # Safety
    /// Native-granted endpoints must obey the factory/table allocation and callback
    /// contracts. A grant is authority to supply native pointers, not a sandbox.
    pub unsafe fn dispatch(&self, caller: &str, command: Command) -> Response {
        match command {
            Command::NativeRegister { service, factory } => unsafe {
                self.register(caller, service, factory)
            },
            Command::NativeBind {
                service,
                interface_major,
                min_minor,
                min_size,
            } => {
                let query = NativeInterfaceQuery {
                    header: AbiHeader::of::<NativeInterfaceQuery>(),
                    interface_major,
                    min_minor,
                    min_size,
                };
                unsafe { self.bind(caller, &service, query) }
            }
            Command::NativeRelease { binding } => unsafe { self.release(caller, binding) },
            _ => Response::error("invalid_request", "not a native operation"),
        }
    }

    fn permitted(&self, caller: &str, service: &str, provide: bool) -> bool {
        valid_name(service)
            && self.grants.get(caller).is_some_and(|grants| {
                if provide {
                    grants.native_provide.contains(service)
                } else {
                    grants.native_use.contains(service)
                }
            })
    }

    unsafe fn register(&self, caller: &str, service: String, address: usize) -> Response {
        // Authorization precedes even the first pointer read.
        if !self.permitted(caller, &service, true) {
            return denied();
        }
        if !aligned::<NativeServiceFactory>(address) {
            return invalid();
        }
        let pointer = address as *const NativeServiceFactory;
        // SAFETY: authorized provider promises readable header storage at this address.
        if !unsafe { &*pointer.cast::<AbiHeader>() }.supports::<NativeServiceFactory>() {
            return invalid();
        }
        // SAFETY: the validated header advertises the full live provider-owned factory.
        let descriptor = unsafe { *pointer };
        let Some(bind) = descriptor.bind else {
            return invalid();
        };
        let factory = Factory {
            context: descriptor.context as usize,
            bind,
        };
        let Ok(mut registry) = self.registry.lock() else {
            return unavailable();
        };
        if registry.factories.contains_key(&service) {
            return Response::error(
                "already_registered",
                "native factory cannot be replaced while live",
            );
        }
        if registry.factories.len() >= MAX_FACTORIES {
            return capacity();
        }
        registry.factories.insert(service, factory);
        Response::Ok
    }

    fn reserve(&self, caller: &str, service: &str) -> Result<(u64, Factory), Response> {
        if !self.permitted(caller, service, false) {
            return Err(denied());
        }
        let mut registry = self.registry.lock().map_err(|_| unavailable())?;
        let factory = *registry
            .factories
            .get(service)
            .ok_or_else(|| Response::error("not_found", "native interface is not registered"))?;
        if registry.bindings.len() >= MAX_BINDINGS {
            return Err(capacity());
        }
        let id = registry.next_binding.checked_add(1).ok_or_else(capacity)?;
        registry.next_binding = id;
        registry.bindings.insert(
            id,
            Binding {
                owner: caller.into(),
                interface: None,
            },
        );
        Ok((id, factory))
    }

    unsafe fn bind(&self, caller: &str, service: &str, query: NativeInterfaceQuery) -> Response {
        if query.min_size < size_of::<NativeInterface>() as u32
            || query.min_size > MAX_MESSAGE as u32
        {
            return invalid();
        }
        let (binding, factory) = match self.reserve(caller, service) {
            Ok(reservation) => reservation,
            Err(error) => return error,
        };
        // SAFETY: the registered descriptor was copied. Provider context and code
        // remain live; no registry or broker lock is held during the callback.
        let mut output = std::ptr::null();
        let status = unsafe {
            (factory.bind)(
                factory.context as *mut std::ffi::c_void,
                BorrowedBytes::new(caller.as_bytes()),
                &query,
                &mut output,
            )
        };
        let valid = status == STATUS_OK && unsafe { valid_interface(output, &query) };
        let Ok(mut registry) = self.registry.lock() else {
            if valid {
                unsafe { release_interface(output) };
            }
            return unavailable();
        };
        if valid {
            registry
                .bindings
                .get_mut(&binding)
                .expect("reserved binding")
                .interface = Some(output as usize);
            Response::NativeBound {
                binding,
                address: output as usize,
            }
        } else {
            registry.bindings.remove(&binding);
            drop(registry);
            // An ABI-compatible prefix can be released even when the requested version
            // mismatches. Invalid/truncated pointers violate the provider contract and
            // cannot safely be traversed to find a release function.
            if status == STATUS_OK && unsafe { releasable(output) } {
                unsafe { release_interface(output) };
            }
            Response::error(
                "native_bind_failed",
                "provider rejected binding or returned an incompatible interface",
            )
        }
    }

    unsafe fn release(&self, caller: &str, id: u64) -> Response {
        let address = {
            let Ok(mut registry) = self.registry.lock() else {
                return unavailable();
            };
            let Some(binding) = registry.bindings.get(&id) else {
                return Response::error("not_found", "unknown native binding");
            };
            if binding.owner != caller {
                return denied();
            }
            let Some(address) = binding.interface else {
                return Response::error("busy", "native binding is being created");
            };
            registry.bindings.remove(&id);
            address
        };
        // SAFETY: the caller owns this live binding, removed exactly once before its
        // callback. The callback may reenter the runtime without any lock being held.
        unsafe { release_interface(address as *const NativeInterface) };
        Response::Ok
    }
}
fn aligned<T>(address: usize) -> bool {
    address != 0 && address.is_multiple_of(align_of::<T>())
}
unsafe fn releasable(pointer: *const NativeInterface) -> bool {
    if !aligned::<NativeInterface>(pointer as usize) {
        return false;
    }
    // SAFETY: provider promises a readable header for every non-null output pointer.
    if (unsafe { (*pointer.cast::<AbiHeader>()).size } as usize) < size_of::<NativeInterface>() {
        return false;
    }
    // SAFETY: header advertises a complete prefix owned by the provider.
    unsafe { (*pointer).release.is_some() }
}
unsafe fn valid_interface(pointer: *const NativeInterface, query: &NativeInterfaceQuery) -> bool {
    if !unsafe { releasable(pointer) } {
        return false;
    }
    // SAFETY: releasable verified the readable complete prefix.
    let header = unsafe { &(*pointer).header };
    header.major == query.interface_major
        && header.minor >= query.min_minor
        && header.size >= query.min_size
        && header.size <= MAX_MESSAGE as u32
}
unsafe fn release_interface(pointer: *const NativeInterface) {
    // SAFETY: this binding is live and exclusively being released. Copy prefix before
    // calling release because that callback may free the entire provider table.
    let interface = unsafe { *pointer };
    unsafe { (interface.release.expect("validated interface"))(interface.context) };
}
fn denied() -> Response {
    Response::error(
        "permission_denied",
        "native operation not granted to this endpoint",
    )
}
fn invalid() -> Response {
    Response::error("invalid_native_interface", "invalid native ABI descriptor")
}
fn capacity() -> Response {
    Response::error("resource_exhausted", "native registry capacity reached")
}
fn unavailable() -> Response {
    Response::error("runtime_unavailable", "native registry unavailable")
}

#[cfg(test)]
mod tests;
