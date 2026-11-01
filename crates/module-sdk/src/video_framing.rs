//! Typed native binding and author adapter for negotiated video framing.
use crate::{ModuleResult, NativeBinding, RuntimeClient, abi::*};
use amaterasu_media::*;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

pub const FRAMING_CONTRACT: &str = "amaterasu.video-framing.native/2";
pub const VIDEO_UNIT_CONTRACT: &str = "amaterasu.video-unit.native/1";
pub const VIDEO_TRANSPORT_CONTRACT: &str = "amaterasu.video-unit-transport.native/1";
pub const VIDEO_PLAN_TRANSPORT_CONTRACT: &str = "amaterasu.video-plan-transport.native/1";

/// # Safety
/// Factory implements the exact plan-transport table and outlives bindings.
pub unsafe fn register_plan_transport(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, VIDEO_PLAN_TRANSPORT_CONTRACT, factory) }
}

/// # Safety
/// Runtime and provider outlive the binding and all synchronous calls.
pub unsafe fn bind_plan_transport(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            VIDEO_PLAN_TRANSPORT_CONTRACT,
            1,
            0,
            size_of::<NegotiatedVideoPlanTransportApi>() as u32,
        )
    }
}

pub const VIDEO_PLAN_TRANSPORT_V2_CONTRACT: &str = "amaterasu.video-plan-transport.native/2";

/// # Safety
/// Factory implements the v2 plan-transport table and outlives bindings.
pub unsafe fn register_plan_transport_v2(
    runtime: &RuntimeClient,
    service: &str,
    factory: &NativeServiceFactory,
) -> ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, VIDEO_PLAN_TRANSPORT_V2_CONTRACT, factory) }
}

/// # Safety
/// Runtime and provider outlive the binding and all synchronous calls.
pub unsafe fn bind_plan_transport_v2(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            VIDEO_PLAN_TRANSPORT_V2_CONTRACT,
            2,
            0,
            size_of::<NegotiatedVideoPlanTransportApiV2>() as u32,
        )
    }
}

/// # Safety
/// Runtime and provider outlive the metadata-aware transport binding.
pub unsafe fn bind_plan_transport_metadata(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            VIDEO_PLAN_TRANSPORT_V2_CONTRACT,
            2,
            1,
            size_of::<NegotiatedVideoPlanTransportApiV2_1>() as u32,
        )
    }
}

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
    /// Default denies production advertisement until explicitly implemented.
    fn probe(
        &self,
        _principal: &str,
        _codec: EncoderFormatId,
        _framing: EncoderFormatId,
        _limits: &VideoFramingLimits,
    ) -> Result<(), Status> {
        Err(MEDIA_UNSUPPORTED)
    }
    fn plan(
        &self,
        principal: &str,
        unit: &NegotiatedVideoUnit,
        bytes: &[u8],
        limits: &VideoFramingLimits,
    ) -> Result<OwnedFramePlan, Status>;

    /// Metadata carriage is separately opt-in for the exact schema and format.
    fn probe_metadata(
        &self,
        _principal: &str,
        _codec: EncoderFormatId,
        _framing: EncoderFormatId,
        _schema: FrameMetadataSchemaId,
        _limits: &VideoFramingLimits,
    ) -> Result<(), Status> {
        Err(MEDIA_UNSUPPORTED)
    }

    /// Return a plan with the selected AFM1 applied by this codec adapter.
    fn plan_metadata(
        &self,
        _principal: &str,
        _unit: &NegotiatedVideoUnit,
        _bytes: &[u8],
        _metadata: &crate::media::metadata::FrameMetadataBundle,
        _limits: &VideoFramingLimits,
    ) -> Result<OwnedFramePlan, Status> {
        Err(MEDIA_UNSUPPORTED)
    }
}

pub struct Factory<P: FramingProvider> {
    pub api: NativeServiceFactory,
    provider: Arc<P>,
}
struct Binding<P: FramingProvider> {
    api: VideoFramingApiV2_2,
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
            || query.interface_major != VIDEO_FRAMING_INTERFACE_MAJOR
            || query.min_minor > VIDEO_FRAMING_METADATA_INTERFACE_MINOR
            || query.min_size as usize > size_of::<VideoFramingApiV2_2>()
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: registered factory stays live during binding.
        let factory = unsafe { &*context.cast::<Factory<P>>() };
        let mut binding = Box::new(Binding {
            api: VideoFramingApiV2_2 {
                base: VideoFramingApiV2_1 {
                    base: VideoFramingApi {
                        base: NativeInterface {
                            header: AbiHeader {
                                size: size_of::<VideoFramingApiV2_2>() as u32,
                                major: VIDEO_FRAMING_INTERFACE_MAJOR,
                                minor: VIDEO_FRAMING_METADATA_INTERFACE_MINOR,
                            },
                            context: std::ptr::null_mut(),
                            release: Some(release::<P>),
                        },
                        plan: plan::<P>,
                    },
                    probe: probe::<P>,
                },
                probe_metadata: probe_metadata::<P>,
                plan_metadata: plan_metadata::<P>,
            },
            provider: factory.provider.clone(),
            principal,
        });
        binding.api.base.base.base.context = (&mut *binding as *mut Binding<P>).cast();
        // SAFETY: runtime owns output and resulting binding.
        unsafe {
            *output = &(*Box::into_raw(binding)).api.base.base.base;
        }
        STATUS_OK
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}
unsafe extern "C" fn probe<P: FramingProvider>(
    context: *mut c_void,
    codec: *const EncoderFormatId,
    framing: *const EncoderFormatId,
    limits: *const VideoFramingLimits,
) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null()
            || codec.is_null()
            || framing.is_null()
            || limits.is_null()
            || !(codec as usize).is_multiple_of(align_of::<EncoderFormatId>())
            || !(framing as usize).is_multiple_of(align_of::<EncoderFormatId>())
            || !(limits as usize).is_multiple_of(align_of::<VideoFramingLimits>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller lends initialized descriptors during this call.
        let (binding, codec, framing, limits) =
            unsafe { (&*context.cast::<Binding<P>>(), *codec, *framing, &*limits) };
        if codec.namespace_id == 0
            || codec.value == 0
            || codec.schema == 0
            || framing.namespace_id == 0
            || framing.value == 0
            || framing.schema == 0
            || limits.size as usize != size_of::<VideoFramingLimits>()
            || limits.version != VIDEO_FRAMING_LIMITS_VERSION
        {
            return STATUS_INVALID_ARGUMENT;
        }
        match binding
            .provider
            .probe(&binding.principal, codec, framing, limits)
        {
            Ok(()) => STATUS_OK,
            Err(status) => status,
        }
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}
unsafe extern "C" fn release<P: FramingProvider>(context: *mut c_void) {
    if !context.is_null() {
        // SAFETY: runtime releases each binding once after callbacks retire.
        drop(unsafe { Box::from_raw(context.cast::<Binding<P>>()) });
    }
}

unsafe extern "C" fn probe_metadata<P: FramingProvider>(
    context: *mut c_void,
    codec: *const EncoderFormatId,
    framing: *const EncoderFormatId,
    schema: *const FrameMetadataSchemaId,
    limits: *const VideoFramingLimits,
) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null()
            || codec.is_null()
            || framing.is_null()
            || schema.is_null()
            || limits.is_null()
            || !(codec as usize).is_multiple_of(align_of::<EncoderFormatId>())
            || !(framing as usize).is_multiple_of(align_of::<EncoderFormatId>())
            || !(schema as usize).is_multiple_of(align_of::<FrameMetadataSchemaId>())
            || !(limits as usize).is_multiple_of(align_of::<VideoFramingLimits>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller supplies live, aligned descriptors for this call.
        let (binding, codec, framing, schema, limits) = unsafe {
            (
                &*context.cast::<Binding<P>>(),
                *codec,
                *framing,
                *schema,
                &*limits,
            )
        };
        if codec.namespace_id == 0
            || codec.value == 0
            || codec.schema == 0
            || framing.namespace_id == 0
            || framing.value == 0
            || framing.schema == 0
            || schema.namespace_id == 0
            || schema.value == 0
            || schema.schema == 0
            || limits.size as usize != size_of::<VideoFramingLimits>()
            || limits.version != VIDEO_FRAMING_LIMITS_VERSION
        {
            return STATUS_INVALID_ARGUMENT;
        }
        binding
            .provider
            .probe_metadata(&binding.principal, codec, framing, schema, limits)
            .map_or_else(|status| status, |_| STATUS_OK)
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}

/// # Safety
/// The sink is valid for one synchronous call and must not retain plan data.
unsafe fn emit_plan(
    owned: OwnedFramePlan,
    limits: &VideoFramingLimits,
    sink: VideoFramePlanSink,
) -> Status {
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
    // SAFETY: owned retains both byte buffers for validation and the callback.
    if unsafe { plan.validate(limits) }.is_err() {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe { (sink.write)(sink.context, &plan) }
}

unsafe extern "C" fn plan_metadata<P: FramingProvider>(
    context: *mut c_void,
    stream: u64,
    wrapped: *const NegotiatedVideoMetadataUnitV1,
    limits: *const VideoFramingLimits,
    sink: VideoFramePlanSink,
) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null()
            || wrapped.is_null()
            || limits.is_null()
            || !(wrapped as usize).is_multiple_of(align_of::<NegotiatedVideoMetadataUnitV1>())
            || !(limits as usize).is_multiple_of(align_of::<VideoFramingLimits>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller lends aligned initialized descriptors and their data.
        let (binding, wrapped, limits) =
            unsafe { (&*context.cast::<Binding<P>>(), &*wrapped, &*limits) };
        let unit = &wrapped.unit;
        if wrapped.size as usize != size_of::<NegotiatedVideoMetadataUnitV1>()
            || wrapped.version != 1
            || unit
                .validate_for(
                    stream,
                    unit.generation,
                    unit.offer_id,
                    unit.codec,
                    unit.framing,
                )
                .is_err()
            || unit.flags & VIDEO_UNIT_END_OF_STREAM != 0
            || wrapped.metadata.len == 0
            || wrapped.metadata.len > FRAME_METADATA_MAX_BUNDLE_BYTES
            || wrapped.metadata.ptr.is_null()
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller guarantees these validated allocations are live through the call.
        let (bytes, metadata) = unsafe {
            (
                std::slice::from_raw_parts(unit.data.ptr, unit.data.len),
                std::slice::from_raw_parts(wrapped.metadata.ptr, wrapped.metadata.len),
            )
        };
        let Ok(bundle) = crate::media::metadata::FrameMetadataBundle::decode(metadata) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if bundle.owner != crate::media::metadata::CarriageOwner::FramerInsert
            || bundle.identity.session_id != stream
            || bundle.identity.submission_id != unit.submission_id
            || bundle.identity.source_id != unit.source_id
            || bundle.identity.config_generation != unit.config_generation
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let plan =
            match binding
                .provider
                .plan_metadata(&binding.principal, unit, bytes, &bundle, limits)
            {
                Ok(plan) => plan,
                Err(status) => return status,
            };
        // SAFETY: the caller retains the synchronous sink.
        unsafe { emit_plan(plan, limits, sink) }
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
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
        // SAFETY: host sink and owned plan remain live through the callback.
        unsafe { emit_plan(owned, limits, sink) }
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
            VIDEO_FRAMING_INTERFACE_MAJOR,
            0,
            size_of::<VideoFramingApi>() as u32,
        )
    }
}

/// # Safety
/// Provider and runtime outlive the binding; the probe callback obeys v2.1.
pub unsafe fn bind_with_probe(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            FRAMING_CONTRACT,
            VIDEO_FRAMING_INTERFACE_MAJOR,
            1,
            size_of::<VideoFramingApiV2_1>() as u32,
        )
    }
}

/// # Safety
/// Provider and runtime outlive the binding and all synchronous metadata calls.
pub unsafe fn bind_with_metadata(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            FRAMING_CONTRACT,
            VIDEO_FRAMING_INTERFACE_MAJOR,
            VIDEO_FRAMING_METADATA_INTERFACE_MINOR,
            size_of::<VideoFramingApiV2_2>() as u32,
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
/// Runtime and provider outlive the metadata reservation and its retirement.
pub unsafe fn bind_video_metadata(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            VIDEO_UNIT_CONTRACT,
            1,
            1,
            size_of::<NegotiatedVideoApiV1_1>() as u32,
        )
    }
}

/// # Safety
/// The provider implements atomic metadata admission and outlives the binding.
pub unsafe fn bind_video_metadata_admission(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            VIDEO_UNIT_CONTRACT,
            1,
            2,
            size_of::<NegotiatedVideoApiV1_2>() as u32,
        )
    }
}

/// # Safety
/// The provider implements the epoch-bound v1.3 phase feedback tail and
/// outlives the binding and all synchronous calls.
pub unsafe fn bind_video_phase_feedback(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            VIDEO_UNIT_CONTRACT,
            1,
            NEGOTIATED_VIDEO_PHASE_FEEDBACK_INTERFACE_MINOR,
            size_of::<NegotiatedVideoApiV1_3>() as u32,
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
