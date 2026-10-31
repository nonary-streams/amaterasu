//! Native pointer handling is confined to this trusted-code ABI boundary.
//! Registry locks protect metadata only; no provider callback runs under a lock.
use amaterasu_abi::*;
use amaterasu_contracts::{Command, Grants, Response, ServiceSelections, valid_name};
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
#[derive(Clone)]
struct Factory {
    context: usize,
    bind: BindCallback,
    contract: Option<String>,
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
    legacy_contracts: BTreeMap<String, BTreeMap<String, String>>,
    selections: ServiceSelections,
    registry: Mutex<Registry>,
}

impl NativeServices {
    #[cfg(test)]
    pub fn new(
        grants: BTreeMap<String, Grants>,
        legacy_contracts: BTreeMap<String, BTreeMap<String, String>>,
    ) -> Self {
        Self::with_selections(grants, legacy_contracts, ServiceSelections::default())
    }

    pub fn with_selections(
        grants: BTreeMap<String, Grants>,
        legacy_contracts: BTreeMap<String, BTreeMap<String, String>>,
        selections: ServiceSelections,
    ) -> Self {
        Self {
            grants,
            legacy_contracts,
            selections,
            registry: Mutex::new(Registry::default()),
        }
    }

    /// # Safety
    /// Native-granted endpoints must obey the factory/table allocation and callback
    /// contracts. A grant is authority to supply native pointers, not a sandbox.
    pub unsafe fn dispatch(&self, caller: &str, command: Command) -> Response {
        match command {
            Command::NativeRegister { service, factory } => unsafe {
                let contract = self
                    .legacy_contracts
                    .get(caller)
                    .and_then(|services| services.get(&service))
                    .cloned();
                self.register(caller, service, factory, contract)
            },
            Command::NativeRegisterTyped {
                service,
                factory,
                contract,
            } => unsafe { self.register(caller, service, factory, Some(contract)) },
            Command::NativeBindTyped {
                service,
                interface_major,
                min_minor,
                min_size,
                contract,
            } => {
                if !valid_name(&contract) {
                    return invalid();
                }
                let query = NativeInterfaceQuery {
                    header: AbiHeader::of::<NativeInterfaceQuery>(),
                    interface_major,
                    min_minor,
                    min_size,
                };
                unsafe { self.bind(caller, &service, query, Some(&contract)) }
            }
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
                unsafe { self.bind(caller, &service, query, None) }
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

    unsafe fn register(
        &self,
        caller: &str,
        service: String,
        address: usize,
        contract: Option<String>,
    ) -> Response {
        // Authorization precedes even the first pointer read.
        if !self.permitted(caller, &service, true)
            || !self
                .selections
                .allows_native(&service, caller, contract.as_deref())
        {
            return denied();
        }
        if contract
            .as_deref()
            .is_some_and(|contract| !valid_name(contract))
        {
            return invalid();
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
            contract,
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

    fn reserve(
        &self,
        caller: &str,
        service: &str,
        contract: Option<&str>,
    ) -> Result<(u64, Factory), Response> {
        if !self.permitted(caller, service, false) {
            return Err(denied());
        }
        // A selected service has an explicit contract identity. A size and
        // version alone cannot identify it, and provider legacy approvals do
        // not authorize a consumer to omit its requested contract.
        if contract.is_none() && matches!(self.selections.native.get(service), Some(Some(_))) {
            return Err(Response::error(
                "native_contract_mismatch",
                "selected native services require a typed binding",
            ));
        }
        let mut registry = self.registry.lock().map_err(|_| unavailable())?;
        let factory =
            registry.factories.get(service).cloned().ok_or_else(|| {
                Response::error("not_found", "native interface is not registered")
            })?;
        if contract.is_some() && contract != factory.contract.as_deref() {
            return Err(Response::error(
                "native_contract_mismatch",
                "native interface contract does not match the requested family",
            ));
        }
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

    unsafe fn bind(
        &self,
        caller: &str,
        service: &str,
        query: NativeInterfaceQuery,
        contract: Option<&str>,
    ) -> Response {
        if query.min_size < size_of::<NativeInterface>() as u32
            || query.min_size > MAX_MESSAGE as u32
        {
            return invalid();
        }
        let (binding, factory) = match self.reserve(caller, service, contract) {
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

#[cfg(test)]
mod encoder_sdk_tests;

#[cfg(test)]
mod data_plane_tests;

#[cfg(test)]
mod network_typed_tests;
