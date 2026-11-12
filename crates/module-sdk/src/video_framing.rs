//! Typed native binding and author adapter for negotiated video framing.
use crate::{ModuleResult, NativeBinding, RuntimeClient, abi::*};
use amaterasu_media::*;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

pub const FRAMING_CONTRACT: &str = "amaterasu.video-framing.native/1";
pub const VIDEO_UNIT_CONTRACT: &str = "amaterasu.video-unit.native/1";
pub const VIDEO_TRANSPORT_CONTRACT: &str = "amaterasu.video-unit-transport.native/1";

/// Owned output is borrowed to the host only during the plan callback.
pub struct OwnedFramePlan {
    pub framed_bytes: Vec<u8>,
    pub record_starts: Vec<u8>,
    pub critical_prefix_shards: u32,
    pub blocks: Vec<VideoFecBlock>,
}

/// Providers authorize the stream in their own binding and implement a pure,
/// retry-safe plan operation. They may not retain the input or limits.
pub trait FramingProvider: Send + Sync + 'static {
    fn plan(
        &self,
        principal: &str,
        unit: &NegotiatedVideoUnit,
        bytes: &[u8],
        limits: &VideoFramingLimits,
    ) -> Result<OwnedFramePlan, Status>;
}

pub struct Factory<P: FramingProvider> {
    pub api: NativeServiceFactory,
    provider: Arc<P>,
}
struct Binding<P: FramingProvider> {
    api: VideoFramingApi,
    provider: Arc<P>,
    principal: String,
}

impl<P: FramingProvider> Factory<P> {
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

unsafe extern "C" fn bind_factory<P: FramingProvider>(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() || query.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime supplies writable output for the duration of this call.
        unsafe {
            *output = std::ptr::null();
        }
        // SAFETY: runtime supplies readable query/principal for this call.
        let query = unsafe { &*query };
        let principal = match unsafe { principal.read() }
            .ok()
            .and_then(|value| std::str::from_utf8(value).ok())
            .filter(|value| !value.is_empty() && value.len() <= 128)
        {
            Some(value) => value.to_owned(),
            None => return STATUS_INVALID_ARGUMENT,
        };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != 1
            || query.min_minor > 0
            || query.min_size as usize > size_of::<VideoFramingApi>()
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: registered factory stays live during binding.
        let factory = unsafe { &*context.cast::<Factory<P>>() };
        let mut binding = Box::new(Binding {
            api: VideoFramingApi {
                base: NativeInterface {
                    header: AbiHeader::of::<VideoFramingApi>(),
                    context: std::ptr::null_mut(),
                    release: Some(release::<P>),
                },
                plan: plan::<P>,
            },
            provider: factory.provider.clone(),
            principal,
        });
        binding.api.base.context = (&mut *binding as *mut Binding<P>).cast();
        // SAFETY: runtime owns output and resulting binding.
        unsafe {
            *output = &(*Box::into_raw(binding)).api.base;
        }
        STATUS_OK
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}
unsafe extern "C" fn release<P: FramingProvider>(context: *mut c_void) {
    if !context.is_null() {
        // SAFETY: runtime releases each binding once after callbacks retire.
        drop(unsafe { Box::from_raw(context.cast::<Binding<P>>()) });
    }
}
unsafe extern "C" fn plan<P: FramingProvider>(
    context: *mut c_void,
    stream: u64,
    unit: *const NegotiatedVideoUnit,
    limits: *const VideoFramingLimits,
    sink: VideoFramePlanSink,
) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() || unit.is_null() || limits.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller supplies live descriptors and initialized unit bytes for this synchronous call.
        let (binding, unit, limits) = unsafe { (&*context.cast::<Binding<P>>(), &*unit, &*limits) };
        if unit
            .validate_for(
                stream,
                unit.generation,
                unit.offer_id,
                unit.codec,
                unit.framing,
            )
            .is_err()
            || unit.flags & VIDEO_UNIT_END_OF_STREAM != 0
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: validated nonempty shape; caller guarantees allocation validity.
        let bytes = unsafe { std::slice::from_raw_parts(unit.data.ptr, unit.data.len) };
        let owned = match binding
            .provider
            .plan(&binding.principal, unit, bytes, limits)
        {
            Ok(plan) => plan,
            Err(status) => return status,
        };
        if owned.blocks.len() > VIDEO_FRAME_MAX_BLOCKS {
            return STATUS_INVALID_ARGUMENT;
        }
        let mut blocks = [VideoFecBlock::default(); VIDEO_FRAME_MAX_BLOCKS];
        blocks[..owned.blocks.len()].copy_from_slice(&owned.blocks);
        let plan = VideoFramePlan {
            size: size_of::<VideoFramePlan>() as u32,
            version: 1,
            framed_bytes: BorrowedBytes::new(&owned.framed_bytes),
            record_starts: BorrowedBytes::new(&owned.record_starts),
            critical_prefix_shards: owned.critical_prefix_shards,
            block_count: owned.blocks.len() as u32,
            blocks,
        };
        // SAFETY: both plan byte buffers are owned by `owned` through this call.
        if unsafe { plan.validate(limits) }.is_err() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: host sink and the plan remain live through the synchronous callback.
        unsafe { (sink.write)(sink.context, &plan) }
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}

/// # Safety
/// The registered factory remains live until registration and all bindings retire.
pub unsafe fn register<P: FramingProvider>(
    runtime: &RuntimeClient,
    service: &str,
    factory: &Factory<P>,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, FRAMING_CONTRACT, &factory.api) }
}
/// # Safety
/// The runtime remains live until all operations and the returned binding retire.
pub unsafe fn bind(runtime: &RuntimeClient, service: &str) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            FRAMING_CONTRACT,
            1,
            0,
            size_of::<VideoFramingApi>() as u32,
        )
    }
}

/// # Safety
/// The registered factory implements `NegotiatedVideoApi` and outlives its bindings.
pub unsafe fn register_video(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, VIDEO_UNIT_CONTRACT, factory) }
}
/// # Safety
/// Runtime and provider outlive the binding and all in-flight calls.
pub unsafe fn bind_video(runtime: &RuntimeClient, service: &str) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            VIDEO_UNIT_CONTRACT,
            1,
            0,
            size_of::<NegotiatedVideoApi>() as u32,
        )
    }
}
/// # Safety
/// The registered factory implements `NegotiatedVideoTransportApi` and outlives its bindings.
pub unsafe fn register_transport(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, VIDEO_TRANSPORT_CONTRACT, factory) }
}
/// # Safety
/// Runtime and provider outlive the binding and all in-flight calls.
pub unsafe fn bind_transport(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            VIDEO_TRANSPORT_CONTRACT,
            1,
            0,
            size_of::<NegotiatedVideoTransportApi>() as u32,
        )
    }
}
