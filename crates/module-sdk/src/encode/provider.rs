//! Native table adapter. Authentication and session lifecycle belong to the
//! provider's signed service and authorized control service.
use super::{FormatId, OutputQueue, SessionMode};
use crate::abi::{
    AbiHeader, BorrowedBytes, NativeInterface, STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT,
    STATUS_OK, Status,
};
use amaterasu_media::*;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub mode: SessionMode,
    pub codec: FormatId,
    pub framing: FormatId,
    pub source_id: u64,
    pub config_generation: u64,
    pub options_schema: FormatId,
    pub options: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlRequest {
    pub kind: u32,
    pub request_id: u64,
    pub expected_generation: u64,
    pub schema: FormatId,
    pub payload: ControlPayload,
}

/// FRAME_OPTIONS is decoded while its nested borrow is live. Equality compares
/// content, not addresses, so providers can deduplicate requests by value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlPayload {
    Opaque(Vec<u8>),
    FrameOptions(OwnedFrameOptions),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedFrameClock {
    pub version: u32,
    pub size: u32,
    pub domain: u32,
    pub epoch_id: [u8; 16],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedFrameOptions {
    pub submission_id: u64,
    pub source_id: u64,
    pub source_generation: u64,
    pub timestamp_us: u64,
    pub clock: OwnedFrameClock,
    pub side_data_schema: FormatId,
    pub side_data: Vec<u8>,
    /// Self-contained, pointer-free v1 extension bytes, retained for deduplication.
    /// Providers must reject required semantics they do not understand.
    pub extension: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ControlResponse {
    pub kind: u32,
    pub request_id: u64,
    pub effective_generation: u64,
    pub effective_submission_id: u64,
    pub outcome: u32,
    pub schema: FormatId,
    pub data: Vec<u8>,
}

/// Implementations must authorize every session against the binding's owner,
/// perform control deduplication, and retain control results across sink
/// rejection. Destroy must cancel resources and retire workers synchronously.
/// Methods may be called concurrently; implementations own their synchronization.
/// `output` must return the same queue until destroy/unsubscribe completes, even
/// after input admission ends. Frame options are decoded to owned data before dispatch.
/// Teardown callbacks are synchronous barriers. A panic or failed release aborts
/// the worker process instead of returning a false barrier to the runtime.
/// The runtime may unload the provider library immediately after release, so
/// leaking a binding alone cannot keep its callbacks and code alive.
pub trait EncoderProvider: Send + Sync + 'static {
    fn probe(&self, config: SessionConfig) -> Result<ControlResponse, Status>;
    fn configure(&self, session: u64, config: SessionConfig) -> Result<(), Status>;
    fn output(&self, session: u64) -> Result<OutputQueue, Status>;
    /// `None` certifies no subscription or callback exists for this owner's
    /// session, including unknown or closed IDs. An error cannot prove that.
    fn notification_queue(&self, session: u64) -> Result<Option<OutputQueue>, Status> {
        self.output(session).map(Some)
    }
    fn control(&self, session: u64, request: ControlRequest) -> Result<ControlResponse, Status>;
    /// Optional exact cumulative measurements for the authenticated session.
    /// Default providers explicitly leave telemetry unavailable.
    fn encode_timing(&self, _session: u64) -> Result<Option<EncodeTimingSnapshotV1>, Status> {
        Ok(None)
    }
    fn drain(&self, session: u64) -> Result<(), Status>;
    fn destroy(&self, session: u64);
    /// Retire all sessions, callbacks and workers synchronously. An error aborts
    /// the process because safe destruction has not been proven.
    fn release(&self) -> Result<(), Status>;
}

/// Owned provider adapter. Each call to `into_binding` transfers one adapter
/// into one runtime-owned native binding.
pub struct ProviderAdapter<P: EncoderProvider> {
    provider: P,
}

#[repr(C)]
struct Binding<P: EncoderProvider> {
    table: EncoderProviderApiV1_1,
    provider: P,
}

impl<P: EncoderProvider> ProviderAdapter<P> {
    pub fn new(provider: P) -> Self {
        Self { provider }
    }

    /// Transfers ownership to the runtime. The runtime must call the table's
    /// release exactly once, after ending all concurrent calls. Failed or
    /// panicking teardown aborts the process instead of returning.
    pub fn into_binding(self) -> *const EncoderProviderApi {
        let mut binding = Box::new(Binding {
            table: EncoderProviderApiV1_1 {
                base: EncoderProviderApi {
                    base: NativeInterface {
                        header: AbiHeader {
                            size: std::mem::size_of::<EncoderProviderApiV1_1>() as u32,
                            major: ENCODER_PROVIDER_INTERFACE_MAJOR,
                            minor: ENCODER_PROVIDER_TIMING_INTERFACE_MINOR,
                        },
                        context: std::ptr::null_mut(),
                        release: Some(release::<P>),
                    },
                    probe: probe::<P>,
                    configure_session: configure::<P>,
                    receive: receive::<P>,
                    control: control::<P>,
                    subscribe: Some(subscribe::<P>),
                    unsubscribe: Some(unsubscribe::<P>),
                    drain: drain::<P>,
                    destroy_session: destroy::<P>,
                },
                encode_timing: encode_timing::<P>,
            },
            provider: self.provider,
        });
        binding.table.base.base.context = (&mut *binding as *mut Binding<P>).cast();
        Box::into_raw(binding).cast::<EncoderProviderApi>()
    }
}

unsafe extern "C" fn encode_timing<P: EncoderProvider>(
    context: *mut c_void,
    session: u64,
    output: *mut EncodeTimingSnapshotV1,
) -> Status {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime retains this adapter during the call; output is a
        // caller-owned writable v1 structure. Provider authorizes the session.
        let binding = unsafe { &*context.cast::<Binding<P>>() };
        match binding.provider.encode_timing(session) {
            Ok(Some(snapshot)) => match snapshot.validate() {
                Ok(()) => {
                    unsafe { output.write(snapshot) };
                    STATUS_OK
                }
                Err(status) => status,
            },
            Ok(None) => MEDIA_UNSUPPORTED,
            Err(status) => status,
        }
    }))
    .unwrap_or(STATUS_INTERNAL_ERROR)
}

unsafe extern "C" fn release<P: EncoderProvider>(context: *mut c_void) {
    barrier(|| {
        if context.is_null() {
            std::process::abort();
        }
        // SAFETY: runtime calls release exactly once after ending table calls.
        let binding = unsafe { &*context.cast::<Binding<P>>() };
        if binding.provider.release().is_err() {
            std::process::abort();
        }
        // SAFETY: successful release establishes all provider lifetime barriers.
        drop(unsafe { Box::from_raw(context.cast::<Binding<P>>()) });
    });
}

/// Void ABI barriers cannot report failure. Never return after a panic.
fn barrier(f: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        std::process::abort();
    }
}
fn guard(f: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(STATUS_INTERNAL_ERROR)
}
fn format(id: EncoderFormatId) -> FormatId {
    FormatId {
        namespace_id: id.namespace_id,
        value: id.value,
        schema: id.schema,
    }
}
fn native_format(id: FormatId) -> EncoderFormatId {
    EncoderFormatId {
        namespace_id: id.namespace_id,
        value: id.value,
        schema: id.schema,
    }
}
// SAFETY: ABI callers supply readable descriptor and bounded byte pointers for the call.
unsafe fn config(raw: *const EncoderSessionConfig) -> Result<SessionConfig, Status> {
    let raw = unsafe { raw.as_ref() }.ok_or(STATUS_INVALID_ARGUMENT)?;
    if !encoder_session_config_valid(raw) {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let options = unsafe { raw.options.read() }?.to_vec();
    Ok(SessionConfig {
        mode: if raw.mode == ENCODER_SESSION_SOURCE {
            SessionMode::Source
        } else {
            SessionMode::Submitted
        },
        codec: format(raw.codec),
        framing: format(raw.framing),
        source_id: raw.source_id,
        config_generation: raw.config_generation,
        options_schema: format(raw.options_schema),
        options,
    })
}
// SAFETY: ABI callers supply a readable descriptor and bounded data for the call.
unsafe fn request(raw: *const EncoderControlRequest) -> Result<ControlRequest, Status> {
    let raw = unsafe { raw.as_ref() }.ok_or(STATUS_INVALID_ARGUMENT)?;
    if !encoder_control_request_valid(raw) {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let is_frame_schema = raw.schema.namespace_id == ENCODER_FORMAT_NAMESPACE_STANDARD
        && raw.schema.value == ENCODER_SCHEMA_FRAME_OPTIONS
        && raw.schema.schema == ENCODER_FORMAT_SCHEMA_V1;
    let payload = if raw.kind == ENCODER_CONTROL_FRAME_OPTIONS {
        if !is_frame_schema {
            return Err(MEDIA_UNSUPPORTED);
        }
        ControlPayload::FrameOptions(unsafe { frame_options(raw.data)? })
    } else {
        if is_frame_schema {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        ControlPayload::Opaque(unsafe { raw.data.read() }?.to_vec())
    };
    Ok(ControlRequest {
        kind: raw.kind,
        request_id: raw.request_id,
        expected_generation: raw.expected_generation,
        schema: format(raw.schema),
        payload,
    })
}

// SAFETY: the caller guarantees the outer and nested ABI borrows are readable
// for this call. Copy the fixed v1 prefix without assuming its byte alignment.
unsafe fn frame_options(data: BorrowedBytes) -> Result<OwnedFrameOptions, Status> {
    let expected = std::mem::size_of::<EncoderFrameOptionsV1>();
    if data.len < expected || data.len > ENCODER_PROVIDER_MAX_OPAQUE_BYTES || data.ptr.is_null() {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let frame = unsafe { data.ptr.cast::<EncoderFrameOptionsV1>().read_unaligned() };
    if frame.version != 1
        || (frame.size as usize) < expected
        || frame.size as usize > data.len
        || frame.submission_id == 0
        || frame.clock.reserved != 0
        || !encoder_borrowed_bytes_valid(frame.side_data, ENCODER_PROVIDER_MAX_OPAQUE_BYTES)
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let clock = frame.clock;
    if !((clock.version == 0 && clock.size == 0 && clock.domain == 0 && clock.epoch_id == [0; 16])
        || (clock.version == 1 && clock.size as usize == std::mem::size_of::<FrameClockMetadata>()))
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    Ok(OwnedFrameOptions {
        submission_id: frame.submission_id,
        source_id: frame.source_id,
        source_generation: frame.source_generation,
        timestamp_us: frame.timestamp_us,
        clock: OwnedFrameClock {
            version: clock.version,
            size: clock.size,
            domain: clock.domain,
            epoch_id: clock.epoch_id,
        },
        side_data_schema: format(frame.side_data_schema),
        side_data: unsafe { frame.side_data.read() }?.to_vec(),
        // SAFETY: the ABI caller guarantees the full bounded outer allocation.
        extension: unsafe {
            std::slice::from_raw_parts(data.ptr.add(expected), data.len - expected)
        }
        .to_vec(),
    })
}
// SAFETY: caller must supply the live adapter context provided by table().
unsafe fn provider<'a, P: EncoderProvider>(context: *mut c_void) -> Result<&'a P, Status> {
    let adapter =
        unsafe { context.cast::<Binding<P>>().as_ref() }.ok_or(STATUS_INVALID_ARGUMENT)?;
    Ok(&adapter.provider)
}
// SAFETY: sink is synchronous and remains valid through the call.
unsafe fn respond(response: ControlResponse, sink: EncoderControlSink) -> Status {
    if response.data.len() > ENCODER_PROVIDER_MAX_OPAQUE_BYTES {
        return STATUS_INVALID_ARGUMENT;
    }
    let raw = EncoderControlResponse {
        size: std::mem::size_of::<EncoderControlResponse>() as u32,
        kind: response.kind,
        request_id: response.request_id,
        effective_generation: response.effective_generation,
        effective_submission_id: response.effective_submission_id,
        outcome: response.outcome,
        reserved: 0,
        schema: native_format(response.schema),
        data: BorrowedBytes::new(&response.data),
    };
    unsafe { (sink.write)(sink.context, &raw) }
}
unsafe extern "C" fn probe<P: EncoderProvider>(
    ctx: *mut c_void,
    raw: *const EncoderSessionConfig,
    sink: EncoderControlSink,
) -> Status {
    guard(|| {
        let result =
            unsafe { provider::<P>(ctx).and_then(|p| config(raw).and_then(|c| p.probe(c))) };
        match result {
            Ok(value) => unsafe { respond(value, sink) },
            Err(status) => status,
        }
    })
}
unsafe extern "C" fn configure<P: EncoderProvider>(
    ctx: *mut c_void,
    session: u64,
    raw: *const EncoderSessionConfig,
) -> Status {
    guard(|| {
        if session == 0 {
            return STATUS_INVALID_ARGUMENT;
        }
        match unsafe {
            provider::<P>(ctx).and_then(|p| config(raw).and_then(|c| p.configure(session, c)))
        } {
            Ok(()) => STATUS_OK,
            Err(s) => s,
        }
    })
}
unsafe extern "C" fn receive<P: EncoderProvider>(
    ctx: *mut c_void,
    session: u64,
    max: usize,
    sink: EncoderOutputSink,
) -> Status {
    guard(|| {
        if session == 0 {
            return STATUS_INVALID_ARGUMENT;
        }
        match unsafe { provider::<P>(ctx) }.and_then(|p| p.output(session)) {
            Ok(q) => unsafe { q.receive_native(session, max, sink) },
            Err(s) => s,
        }
    })
}
unsafe extern "C" fn control<P: EncoderProvider>(
    ctx: *mut c_void,
    session: u64,
    raw: *const EncoderControlRequest,
    sink: EncoderControlSink,
) -> Status {
    guard(|| {
        if session == 0 {
            return STATUS_INVALID_ARGUMENT;
        }
        let result = unsafe {
            provider::<P>(ctx).and_then(|p| request(raw).and_then(|r| p.control(session, r)))
        };
        match result {
            Ok(value) => unsafe { respond(value, sink) },
            Err(status) => status,
        }
    })
}
unsafe extern "C" fn subscribe<P: EncoderProvider>(
    ctx: *mut c_void,
    session: u64,
    sink: EncoderEventSink,
) -> Status {
    guard(|| {
        if session == 0 {
            return STATUS_INVALID_ARGUMENT;
        }
        match unsafe { provider::<P>(ctx) }.and_then(|p| p.output(session)) {
            Ok(q) => unsafe { q.subscribe_native(session, sink) },
            Err(s) => s,
        }
    })
}
unsafe extern "C" fn unsubscribe<P: EncoderProvider>(ctx: *mut c_void, session: u64) {
    barrier(|| {
        let Ok(queue) = (unsafe { provider::<P>(ctx) }).and_then(|p| p.notification_queue(session))
        else {
            std::process::abort();
        };
        if let Some(q) = queue {
            q.unsubscribe_native();
        }
    });
}
unsafe extern "C" fn drain<P: EncoderProvider>(ctx: *mut c_void, session: u64) -> Status {
    guard(|| {
        if session == 0 {
            return STATUS_INVALID_ARGUMENT;
        }
        match unsafe { provider::<P>(ctx) }.and_then(|p| p.drain(session)) {
            Ok(()) => STATUS_OK,
            Err(s) => s,
        }
    })
}
unsafe extern "C" fn destroy<P: EncoderProvider>(ctx: *mut c_void, session: u64) {
    barrier(|| {
        let Ok(p) = (unsafe { provider::<P>(ctx) }) else {
            std::process::abort();
        };
        let Ok(queue) = p.notification_queue(session) else {
            std::process::abort();
        };
        if let Some(q) = queue {
            q.unsubscribe_native();
        }
        p.destroy(session);
    });
}
