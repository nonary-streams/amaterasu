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

/// Owned output is borrowed to the host only during the plan callback.
pub struct OwnedFramePlan {
    pub framed_bytes: Vec<u8>,
    pub record_starts: Vec<u8>,
    pub critical_prefix_shards: u32,
    pub blocks: Vec<VideoFecBlock>,
}

#[derive(Clone, Copy)]
pub struct WireBudget {
    pub wire_kbps: u32,
    pub fps: u32,
    pub audio_payload_bps: u32,
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
    /// Whether an otherwise supported profile can use negotiated link budgeting.
    fn supports_wire_budget(&self, _: EncoderFormatId, _: EncoderFormatId) -> bool {
        false
    }
    /// Translate a negotiated link limit without mutating provider state.
    fn encoder_bitrate(
        &self,
        _principal: &str,
        _codec: EncoderFormatId,
        _framing: EncoderFormatId,
        _limits: &VideoFramingLimits,
        _budget: WireBudget,
    ) -> Result<u32, Status> {
        Err(MEDIA_UNSUPPORTED)
    }
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
            || query.min_minor > VIDEO_FRAMING_INTERFACE_MINOR
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
                                minor: VIDEO_FRAMING_INTERFACE_MINOR,
                            },
                            context: std::ptr::null_mut(),
                            release: Some(release::<P>),
                        },
                        plan: plan::<P>,
                    },
                    probe: probe::<P>,
                },
                encoder_bitrate: encoder_bitrate::<P>,
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

unsafe extern "C" fn encoder_bitrate<P: FramingProvider>(
    context: *mut c_void,
    codec: *const EncoderFormatId,
    framing: *const EncoderFormatId,
    limits: *const VideoFramingLimits,
    wire_kbps: u32,
    fps: u32,
    audio_payload_bps: u32,
    output: *mut u32,
) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if output.is_null() || !(output as usize).is_multiple_of(align_of::<u32>()) {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the same caller-owned descriptors are required by probe.
        let status = unsafe { probe::<P>(context, codec, framing, limits) };
        if status != STATUS_OK {
            return status;
        }
        // SAFETY: the successful probe validated the descriptors and context.
        let (binding, codec, framing, limits) =
            unsafe { (&*context.cast::<Binding<P>>(), *codec, *framing, &*limits) };
        if !binding.provider.supports_wire_budget(codec, framing) {
            return MEDIA_UNSUPPORTED;
        }
        if wire_kbps == 0 && fps == 0 && audio_payload_bps == 0 {
            return STATUS_OK;
        }
        if wire_kbps == 0 || fps == 0 {
            return STATUS_INVALID_ARGUMENT;
        }
        let budget = WireBudget {
            wire_kbps,
            fps,
            audio_payload_bps,
        };
        match binding
            .provider
            .encoder_bitrate(&binding.principal, codec, framing, limits, budget)
        {
            Ok(bitrate) if valid_budget_bitrate(bitrate, wire_kbps) => {
                // SAFETY: the caller lends an aligned writable output.
                unsafe { *output = bitrate };
                STATUS_OK
            }
            Ok(_) => STATUS_INVALID_ARGUMENT,
            Err(status) => status,
        }
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}

fn valid_budget_bitrate(bitrate: u32, wire_kbps: u32) -> bool {
    bitrate != 0 && u64::from(bitrate) <= u64::from(wire_kbps) * 1_000
}

/// Ask the selected framer how much of the total wire budget can be given
/// to the encoder. Older framers may omit this independently of frame planning.
pub fn plan_encoder_bitrate(
    runtime: &RuntimeClient,
    service: &str,
    codec: EncoderFormatId,
    framing: EncoderFormatId,
    limits: &VideoFramingLimits,
    budget: WireBudget,
) -> ModuleResult<u32> {
    if budget.wire_kbps == 0 || budget.fps == 0 {
        return Err("invalid wire budget".into());
    }
    call_encoder_bitrate(runtime, service, codec, framing, limits, Some(budget))
}

/// Probe budgeting support without assuming the client's eventual link rate.
pub fn probe_encoder_bitrate(
    runtime: &RuntimeClient,
    service: &str,
    codec: EncoderFormatId,
    framing: EncoderFormatId,
    limits: &VideoFramingLimits,
) -> ModuleResult<()> {
    call_encoder_bitrate(runtime, service, codec, framing, limits, None).map(|_| ())
}

fn call_encoder_bitrate(
    runtime: &RuntimeClient,
    service: &str,
    codec: EncoderFormatId,
    framing: EncoderFormatId,
    limits: &VideoFramingLimits,
    budget: Option<WireBudget>,
) -> ModuleResult<u32> {
    // SAFETY: the typed runtime contract authenticates the table family.
    let binding = unsafe {
        runtime.bind_native_typed(
            service,
            FRAMING_CONTRACT,
            VIDEO_FRAMING_INTERFACE_MAJOR,
            2,
            size_of::<VideoFramingApiV2_2>() as u32,
        )?
    };
    // SAFETY: the bind above requires the v2.2 layout, and all descriptors
    // remain live for the synchronous call.
    let api = unsafe { binding.table::<VideoFramingApiV2_2>()? };
    let mut bitrate = 0;
    let status = unsafe {
        (api.encoder_bitrate)(
            binding.context(),
            &codec,
            &framing,
            limits,
            budget.map_or(0, |budget| budget.wire_kbps),
            budget.map_or(0, |budget| budget.fps),
            budget.map_or(0, |budget| budget.audio_payload_bps),
            &mut bitrate,
        )
    };
    if status == STATUS_OK
        && budget.is_none_or(|budget| valid_budget_bitrate(bitrate, budget.wire_kbps))
    {
        Ok(bitrate)
    } else {
        Err(format!("framer wire budget rejected ({status})"))
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
        if unsafe { plan.validate_shape(limits) }.is_err() {
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
