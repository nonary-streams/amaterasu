//! Native, startup-selected source of bounded video-profile offers.
//!
//! A provider offer is only a declaration. Trusted composition must intersect it
//! with independently verified downstream support before advertising it.
use crate::{ModuleResult, NativeBinding, RuntimeClient, abi::*};
use amaterasu_contracts::media_profile::VideoProfileSnapshot;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

pub const CONTRACT: &str = "amaterasu.video-profile.native/1";
const MAX_SNAPSHOT_BYTES: usize = 16 * 1024;

#[repr(C)]
pub struct VideoProfileApi {
    pub base: NativeInterface,
    /// Reply bytes are borrowed only during the sink callback.
    pub snapshot: unsafe extern "C" fn(*mut c_void, ReplySink) -> Status,
}

/// Implementations return an immutable snapshot for the current composition.
pub trait Provider: Send + Sync + 'static {
    fn snapshot(&self) -> Result<VideoProfileSnapshot, Status>;
}

pub struct Factory<P: Provider> {
    pub api: NativeServiceFactory,
    provider: Arc<P>,
}

struct Binding<P: Provider> {
    api: VideoProfileApi,
    provider: Arc<P>,
}

impl<P: Provider> Factory<P> {
    pub fn new(provider: P) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind_factory::<P>),
            },
            provider: Arc::new(provider),
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}

unsafe extern "C" fn bind_factory<P: Provider>(
    context: *mut c_void,
    _principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() || query.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime owns the live factory, query and writable output through this call.
        let query = unsafe { &*query };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != 1
            || query.min_minor > 0
            || query.min_size as usize > size_of::<VideoProfileApi>()
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: factory context was initialized by Factory::new and stays live while registered.
        let factory = unsafe { &*context.cast::<Factory<P>>() };
        let mut binding = Box::new(Binding {
            api: VideoProfileApi {
                base: NativeInterface {
                    header: AbiHeader::of::<VideoProfileApi>(),
                    context: std::ptr::null_mut(),
                    release: Some(release::<P>),
                },
                snapshot: snapshot::<P>,
            },
            provider: factory.provider.clone(),
        });
        binding.api.base.context = (&mut *binding as *mut Binding<P>).cast();
        // SAFETY: output is writable for this call; runtime owns the resulting binding.
        unsafe {
            *output = &(*Box::into_raw(binding)).api.base;
        }
        STATUS_OK
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}

unsafe extern "C" fn release<P: Provider>(context: *mut c_void) {
    if !context.is_null() {
        // SAFETY: runtime releases each successfully bound context exactly once.
        drop(unsafe { Box::from_raw(context.cast::<Binding<P>>()) });
    }
}

unsafe extern "C" fn snapshot<P: Provider>(context: *mut c_void, sink: ReplySink) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime keeps the bound table and context live for this call.
        let provider = unsafe { &*context.cast::<Binding<P>>() };
        let value = match provider.provider.snapshot() {
            Ok(value) if value.validate() => value,
            Ok(_) => return STATUS_INVALID_ARGUMENT,
            Err(status) => return status,
        };
        let bytes = match serde_json::to_vec(&value) {
            Ok(bytes) if bytes.len() <= MAX_SNAPSHOT_BYTES => bytes,
            _ => return STATUS_INVALID_ARGUMENT,
        };
        // SAFETY: sink remains valid and copies the bytes during this call.
        unsafe { sink.send(&bytes) };
        STATUS_OK
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}

/// # Safety
/// Factory and runtime must remain alive until registrations and bindings retire.
pub unsafe fn register(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, CONTRACT, factory) }
}

/// # Safety
/// The runtime must outlive the returned binding and all calls through it.
pub unsafe fn bind(runtime: &RuntimeClient, service: &str) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(service, CONTRACT, 1, 0, size_of::<VideoProfileApi>() as u32)
    }
}

/// Copies one bounded snapshot while the binding remains live.
/// # Safety
/// The binding must implement this exact interface and obey its callback lifetime.
pub unsafe fn read(binding: &NativeBinding) -> ModuleResult<VideoProfileSnapshot> {
    let api = unsafe { binding.table::<VideoProfileApi>() }?;
    struct Output {
        bytes: Vec<u8>,
        calls: usize,
        invalid: bool,
    }
    unsafe extern "C" fn copy(context: *mut c_void, bytes: BorrowedBytes) {
        // SAFETY: caller supplies a live Output for the duration of the synchronous call.
        let output = unsafe { &mut *context.cast::<Output>() };
        output.calls += 1;
        if bytes.len > MAX_SNAPSHOT_BYTES || output.calls != 1 {
            output.invalid = true;
            return;
        }
        // SAFETY: provider promises initialized callback-lifetime bytes.
        match unsafe { bytes.read() } {
            Ok(bytes) => output.bytes.extend_from_slice(bytes),
            Err(_) => output.invalid = true,
        }
    }
    let mut output = Output {
        bytes: Vec::new(),
        calls: 0,
        invalid: false,
    };
    let sink = ReplySink {
        context: (&mut output as *mut Output).cast(),
        write: copy,
    };
    // SAFETY: table, context and sink are valid throughout the synchronous call.
    let status = unsafe { (api.snapshot)(api.base.context, sink) };
    if status != STATUS_OK || output.invalid || output.calls != 1 {
        return Err("invalid video profile provider response".into());
    }
    let snapshot: VideoProfileSnapshot =
        serde_json::from_slice(&output.bytes).map_err(|_| "invalid video profile snapshot")?;
    if !snapshot.validate() {
        return Err("invalid video profile snapshot".into());
    }
    Ok(snapshot)
}
