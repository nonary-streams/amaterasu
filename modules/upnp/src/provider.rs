use crate::upnp::UpnpLifecycle;
use amaterasu_module_sdk::abi::{
    self, AbiHeader, BorrowedBytes, NativeInterface, NativeInterfaceQuery, NativeServiceFactory,
    ReplySink, STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT, STATUS_OK, STATUS_UNAVAILABLE,
    Status,
};
use amaterasu_module_sdk::network::Network;
use amaterasu_module_sdk::router_mapping::{self, Request, Response, RouterMappingApi};
use amaterasu_module_sdk::{ManagedModule, ModuleResult, RuntimeClient};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

struct MappingModule {
    _factory: Box<Factory>,
}

struct Factory {
    network: Network,
    bound: Arc<AtomicBool>,
}

#[repr(C)]
struct Lease {
    api: RouterMappingApi,
    lifecycle: Mutex<UpnpLifecycle>,
    bound: Arc<AtomicBool>,
}

impl ManagedModule for MappingModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let amaterasu_module_sdk::Response::Result { payload } =
            runtime.send(amaterasu_module_sdk::Command::GetSettings {})?
        else {
            return Err("router mapping provider settings unavailable".into());
        };
        let mut factory = Box::new(Factory {
            network: Network::from_settings(&runtime, &payload)?,
            bound: Arc::new(AtomicBool::new(false)),
        });
        let registration = NativeServiceFactory {
            header: AbiHeader::of::<NativeServiceFactory>(),
            context: (&mut *factory as *mut Factory).cast(),
            bind: Some(bind),
        };
        // SAFETY: the boxed factory remains alive for the module lifetime, and
        // composition unloads its consumers before the factory.
        unsafe {
            runtime.register_native_typed(
                router_mapping::SERVICE,
                router_mapping::CONTRACT,
                &registration,
            )?
        };
        Ok(Self { _factory: factory })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        Ok(())
    }
}

unsafe extern "C" fn bind(
    factory: *mut c_void,
    _principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guard(|| {
        if factory.is_null() || query.is_null() || output.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: runtime keeps the registered factory live and supplies this
        // readable query and writable output for the call.
        let (factory, query) = unsafe {
            *output = std::ptr::null();
            (&*factory.cast::<Factory>(), &*query)
        };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != router_mapping::MAJOR
            || query.min_minor > router_mapping::MINOR
            || query.min_size as usize > size_of::<RouterMappingApi>()
        {
            return Err(STATUS_UNAVAILABLE);
        }
        if factory.bound.swap(true, Ordering::AcqRel) {
            return Err(STATUS_UNAVAILABLE);
        }
        let mut lease = Box::new(Lease {
            api: RouterMappingApi {
                base: NativeInterface {
                    header: AbiHeader {
                        size: size_of::<RouterMappingApi>() as u32,
                        major: router_mapping::MAJOR,
                        minor: router_mapping::MINOR,
                    },
                    context: std::ptr::null_mut(),
                    release: Some(release),
                },
                request: Some(request),
            },
            lifecycle: Mutex::new(UpnpLifecycle::new(Some(factory.network.clone()))),
            bound: factory.bound.clone(),
        });
        lease.api.base.context = (&mut *lease as *mut Lease).cast();
        // SAFETY: Lease, table and prefix are repr(C) at the same address. The
        // runtime owns the allocation until the last callback and calls release.
        unsafe { *output = Box::into_raw(lease).cast() };
        Ok(())
    })
}

unsafe extern "C" fn release(lease: *mut c_void) {
    let _ = guard(|| {
        // SAFETY: the runtime transfers exact, unique ownership after all
        // requests return. Drop joins both workers before admitting a new lease.
        let lease = unsafe { Box::from_raw(lease.cast::<Lease>()) };
        let bound = lease.bound.clone();
        drop(lease);
        bound.store(false, Ordering::Release);
        Ok(())
    });
}

unsafe extern "C" fn request(lease: *mut c_void, bytes: BorrowedBytes, reply: ReplySink) -> Status {
    guard(|| {
        if lease.is_null() || bytes.len > router_mapping::MAX_REQUEST_BYTES {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: the runtime keeps this lease live during synchronous calls.
        let lease = unsafe { &*lease.cast::<Lease>() };
        // SAFETY: the trusted SDK provides the request allocation for this call.
        let bytes = unsafe { bytes.read()? };
        let request = serde_json::from_slice(bytes).map_err(|_| STATUS_INVALID_ARGUMENT)?;
        let mut lifecycle = lease.lifecycle.try_lock().map_err(|_| STATUS_UNAVAILABLE)?;
        let response = match request {
            Request::Update { desired } => {
                desired.validate().map_err(|_| STATUS_INVALID_ARGUMENT)?;
                lifecycle.update(desired);
                Response::Status {
                    status: lifecycle.status().clone(),
                }
            }
            Request::Status => {
                lifecycle.refresh_status();
                Response::Status {
                    status: lifecycle.status().clone(),
                }
            }
            Request::PrepareShutdown { remaining_ms } => {
                lifecycle.prepare_shutdown(
                    Instant::now() + Duration::from_millis(remaining_ms.min(5000)),
                );
                Response::Accepted
            }
            Request::PollShutdown => match lifecycle.poll_shutdown() {
                Ok(ready) => Response::Shutdown { ready, error: None },
                Err(error) => Response::Shutdown {
                    ready: false,
                    error: Some(error),
                },
            },
        };
        let response = serde_json::to_vec(&response).map_err(|_| STATUS_INTERNAL_ERROR)?;
        // SAFETY: the caller keeps the reply sink alive for this callback.
        unsafe { reply.send(&response) };
        Ok(())
    })
}

fn guard(action: impl FnOnce() -> Result<(), Status>) -> Status {
    match catch_unwind(AssertUnwindSafe(action)) {
        Ok(Ok(())) => STATUS_OK,
        Ok(Err(status)) => status,
        Err(_) => abi::STATUS_INTERNAL_ERROR,
    }
}

amaterasu_module_sdk::export_module!(MappingModule);
