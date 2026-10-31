//! Source-mode provider. All callbacks are synchronous while the state lock is
//! held, making unsubscribe and destruction barriers without background work.
use crate::native::Shared;
use amaterasu_abi::*;
use amaterasu_media::*;
use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Mutex,
};

pub struct Registration {
    pub factory: NativeServiceFactory,
    _context: Box<Shared>,
}
impl Registration {
    pub fn new(shared: Shared) -> Self {
        let mut context = Box::new(shared);
        Self {
            factory: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: (&mut *context as *mut Shared).cast(),
                bind: Some(bind),
            },
            _context: context,
        }
    }
}
struct Binding {
    api: EncoderProviderApi,
    owner: String,
    shared: Shared,
    configured: Mutex<HashSet<u64>>,
}
#[derive(Clone, PartialEq, Eq)]
struct ControlKey {
    kind: u32,
    expected: u64,
    schema: (u64, u64, u64),
    data: Vec<u8>,
}
const CONTROL_RECORD_LIMIT: usize = 128;
const CONTROL_BYTE_LIMIT: usize = 128 * 1024;
// Includes request/result structures and hash table allocation overhead.
const CONTROL_RECORD_RESERVATION: usize = 256;

struct ControlRecord {
    key: ControlKey,
    outcome: u32,
    generation: u64,
    boundary: u64,
}
#[derive(Clone, Copy)]
struct Event {
    context: usize,
    notify: unsafe extern "C" fn(*mut c_void, u64, u32),
}
pub(crate) struct SourceSession {
    source: u64,
    generation: u64,
    output_generation: u64,
    output: bool,
    draining: bool,
    eos_sent: bool,
    subscription: Option<Event>,
    records: HashMap<u64, ControlRecord>,
    record_bytes: usize,
}
fn format(value: u64) -> EncoderFormatId {
    EncoderFormatId {
        namespace_id: ENCODER_FORMAT_NAMESPACE_STANDARD,
        value,
        schema: ENCODER_FORMAT_SCHEMA_V1,
    }
}
fn same(a: EncoderFormatId, b: EncoderFormatId) -> bool {
    (a.namespace_id, a.value, a.schema) == (b.namespace_id, b.value, b.schema)
}
// SAFETY: the ABI caller guarantees the options bytes are readable for the call.
unsafe fn common_config(config: &EncoderSessionConfig) -> Option<EncoderCommonConfigV1> {
    if !encoder_session_config_valid(config)
        || config.mode != ENCODER_SESSION_SOURCE
        || config.source_id == 0
        || config.config_generation == 0
        || !same(config.codec, format(CODEC_H264 as u64))
        || !same(config.framing, format(ENCODER_FRAMING_ANNEX_B))
        || !same(config.options_schema, format(ENCODER_SCHEMA_COMMON_CONFIG))
        || config.options.len != size_of::<EncoderCommonConfigV1>()
    {
        return None;
    }
    // SAFETY: descriptor length and pointer shape were validated; caller lends bytes.
    let common =
        unsafe { std::ptr::read_unaligned(config.options.ptr.cast::<EncoderCommonConfigV1>()) };
    (encoder_common_config_valid(&common)
        && common.size as usize == size_of::<EncoderCommonConfigV1>()
        && common.flags == 0
        && common.chroma == ENCODER_CHROMA_420
        && common.bit_depth == 8
        && common.output_clock_origin_us != 0
        && common.output_clock_origin_us.checked_add(1_000).is_some())
    .then_some(common)
}

fn response(
    id: u64,
    kind: u32,
    outcome: u32,
    generation: u64,
    boundary: u64,
) -> EncoderControlResponse {
    EncoderControlResponse {
        size: size_of::<EncoderControlResponse>() as u32,
        kind,
        request_id: id,
        effective_generation: generation,
        effective_submission_id: boundary,
        outcome,
        reserved: 0,
        schema: format(0),
        data: BorrowedBytes::new(&[]),
    }
}
unsafe fn emit_control(sink: EncoderControlSink, response: &EncoderControlResponse) -> Status {
    // SAFETY: caller guarantees the callback and context are live for this call.
    unsafe { (sink.write)(sink.context, response) }
}
fn notify(session: &SourceSession, id: u64, event: u32) {
    if let Some(sink) = session.subscription {
        // SAFETY: subscriber keeps its callback/context alive until unsubscribe;
        // calls are synchronous under the shared lock, which is the barrier.
        unsafe { (sink.notify)(sink.context as *mut c_void, id, event) };
    }
}
unsafe extern "C" fn bind(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guarded(|| {
        if context.is_null() || query.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: factory ABI guarantees valid pointers and principal bytes.
        unsafe {
            *output = std::ptr::null();
            let q = &*query;
            if !q.header.supports::<NativeInterfaceQuery>()
                || q.interface_major != ENCODER_PROVIDER_INTERFACE_MAJOR
                || q.min_minor > ENCODER_PROVIDER_INTERFACE_MINOR
                || q.min_size > ENCODER_PROVIDER_API_V1_0_SIZE
            {
                return MEDIA_UNSUPPORTED;
            }
            let Some(owner) = principal
                .read()
                .ok()
                .and_then(|b| std::str::from_utf8(b).ok())
                .filter(|s| !s.is_empty() && s.len() <= 128)
            else {
                return STATUS_INVALID_ARGUMENT;
            };
            let mut binding = Box::new(Binding {
                api: EncoderProviderApi {
                    base: NativeInterface {
                        header: AbiHeader {
                            size: ENCODER_PROVIDER_API_V1_0_SIZE,
                            major: ENCODER_PROVIDER_INTERFACE_MAJOR,
                            minor: ENCODER_PROVIDER_INTERFACE_MINOR,
                        },
                        context: std::ptr::null_mut(),
                        release: Some(release),
                    },
                    probe,
                    configure_session: configure,
                    receive,
                    control,
                    subscribe: Some(subscribe),
                    unsubscribe: Some(unsubscribe),
                    drain,
                    destroy_session: destroy,
                },
                owner: owner.into(),
                shared: (&*context.cast::<Shared>()).clone(),
                configured: Mutex::new(HashSet::new()),
            });
            binding.api.base.context = (&mut *binding as *mut Binding).cast();
            let binding = Box::into_raw(binding);
            *output = &(*binding).api.base;
        }
        STATUS_OK
    })
}
unsafe extern "C" fn release(context: *mut c_void) {
    let _ = guarded(|| {
        if !context.is_null() {
            // SAFETY: runtime releases once after binding calls finish.
            let binding = unsafe { Box::from_raw(context.cast::<Binding>()) };
            let ids = binding
                .configured
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut state = binding
                .shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for id in ids.iter() {
                if state.authorized(&binding.owner, *id) {
                    state.sources.remove(id);
                }
            }
        }
        STATUS_OK
    });
}
unsafe extern "C" fn probe(
    context: *mut c_void,
    config: *const EncoderSessionConfig,
    sink: EncoderControlSink,
) -> Status {
    guarded(|| {
        if context.is_null() || config.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller lends configuration for the duration of this call.
        let config = unsafe { &*config };
        if !encoder_session_config_valid(config) {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller lends readable config options for this call.
        let outcome = if unsafe { common_config(config) }.is_some() {
            ENCODER_OUTCOME_APPLIED
        } else {
            ENCODER_OUTCOME_UNSUPPORTED
        };
        let mut result = response(0, 0, outcome, config.config_generation, 0);
        let limits = EncoderProviderLimitsV1 {
            version: 1,
            size: size_of::<EncoderProviderLimitsV1>() as u32,
            max_control_records: CONTROL_RECORD_LIMIT as u32,
            reserved: 0,
            max_control_bytes: CONTROL_BYTE_LIMIT as u64,
            max_output_bytes: ENCODER_PROVIDER_MAX_OUTPUT_BYTES as u64,
        };
        result.schema = format(ENCODER_SCHEMA_PROVIDER_LIMITS);
        result.data = BorrowedBytes {
            ptr: (&limits as *const EncoderProviderLimitsV1).cast(),
            len: size_of::<EncoderProviderLimitsV1>(),
        };
        // SAFETY: static limits and sink remain live through the callback.
        unsafe { emit_control(sink, &result) }
    })
}
unsafe extern "C" fn configure(
    context: *mut c_void,
    id: u64,
    config: *const EncoderSessionConfig,
) -> Status {
    guarded(|| {
        if context.is_null() || config.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: binding and config are live throughout this call.
        let (binding, config) = unsafe { (&*context.cast::<Binding>(), &*config) };
        if !encoder_session_config_valid(config) {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller lends readable config options for this call.
        let Some(common) = (unsafe { common_config(config) }) else {
            return MEDIA_UNSUPPORTED;
        };
        let mut ids = binding
            .configured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = binding
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.authorized_source(&binding.owner, id, config.source_id) {
            return MEDIA_CLOSED;
        }
        if !state.source_configuration_matches(id, &common) {
            return MEDIA_UNSUPPORTED;
        }
        if state.sources.contains_key(&id) {
            return STATUS_INVALID_ARGUMENT;
        }
        state.sources.insert(
            id,
            SourceSession {
                source: config.source_id,
                generation: config.config_generation,
                output_generation: config.config_generation,
                output: true,
                draining: false,
                eos_sent: false,
                subscription: None,
                records: HashMap::new(),
                record_bytes: 0,
            },
        );
        ids.insert(id);
        STATUS_OK
    })
}
unsafe extern "C" fn receive(
    context: *mut c_void,
    id: u64,
    max_bytes: usize,
    sink: EncoderOutputSink,
) -> Status {
    guarded(|| {
        if context.is_null() || max_bytes == 0 || max_bytes > ENCODER_PROVIDER_MAX_OUTPUT_BYTES {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: binding is live during the call.
        let binding = unsafe { &*context.cast::<Binding>() };
        let Ok(mut state) = binding.shared.try_lock() else {
            return MEDIA_BUSY;
        };
        if !state.authorized(&binding.owner, id) {
            return MEDIA_CLOSED;
        }
        let Some(session) = state.sources.get_mut(&id) else {
            return MEDIA_CLOSED;
        };
        if !session.output && !(session.draining && !session.eos_sent) {
            return if session.eos_sent {
                MEDIA_CLOSED
            } else {
                MEDIA_PENDING
            };
        }
        let payload = b"FAKE-SOURCE-AU\0";
        let eos = !session.output;
        if !eos && max_bytes < payload.len() {
            return MEDIA_BUSY;
        }
        let chunk = EncoderOutputChunk {
            size: size_of::<EncoderOutputChunk>() as u32,
            flags: if eos {
                ENCODER_CHUNK_END_OF_STREAM
            } else {
                ENCODER_CHUNK_ACCESS_UNIT_START
                    | ENCODER_CHUNK_ACCESS_UNIT_END
                    | ENCODER_CHUNK_KEYFRAME
            },
            session_id: id,
            access_unit_id: if eos { 0 } else { 1 },
            submission_id: 0,
            source_id: session.source,
            config_generation: if eos {
                session.generation
            } else {
                session.output_generation
            },
            timestamp_us: if eos { 0 } else { 1_000 },
            codec: format(CODEC_H264 as u64),
            framing: format(ENCODER_FRAMING_ANNEX_B),
            bytes: BorrowedBytes::new(if eos { &[] } else { payload }),
        };
        // SAFETY: sink and chunk bytes live throughout the synchronous call.
        let status = unsafe { (sink.write)(sink.context, &chunk) };
        if status == STATUS_OK {
            if eos {
                session.eos_sent = true;
                notify(session, id, ENCODER_EVENT_STOPPED);
            } else {
                session.output = false;
            }
        }
        status
    })
}
unsafe extern "C" fn control(
    context: *mut c_void,
    id: u64,
    request: *const EncoderControlRequest,
    sink: EncoderControlSink,
) -> Status {
    guarded(|| {
        if context.is_null() || request.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: binding and request descriptor live through the call.
        let (binding, request) = unsafe { (&*context.cast::<Binding>(), &*request) };
        if !encoder_control_request_valid(request) {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller lends readable request bytes during this call.
        let Ok(data) = (unsafe { request.data.read() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let mut state = binding
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.authorized(&binding.owner, id) {
            return MEDIA_CLOSED;
        }
        let Some(session) = state.sources.get_mut(&id) else {
            return MEDIA_CLOSED;
        };
        let key = ControlKey {
            kind: request.kind,
            expected: request.expected_generation,
            schema: (
                request.schema.namespace_id,
                request.schema.value,
                request.schema.schema,
            ),
            data: data.to_vec(),
        };
        if request.kind == ENCODER_CONTROL_POLL {
            if !data.is_empty() {
                return STATUS_INVALID_ARGUMENT;
            }
            let Some(record) = session.records.get(&request.request_id) else {
                return STATUS_INVALID_ARGUMENT;
            };
            let reply = response(
                request.request_id,
                record.key.kind,
                record.outcome,
                record.generation,
                record.boundary,
            );
            // SAFETY: sink lives during the synchronous callback.
            return unsafe { emit_control(sink, &reply) };
        }
        if let Some(record) = session.records.get(&request.request_id) {
            if record.key != key {
                return STATUS_INVALID_ARGUMENT;
            }
            let reply = response(
                request.request_id,
                record.key.kind,
                record.outcome,
                record.generation,
                record.boundary,
            );
            // SAFETY: sink lives during the synchronous callback.
            return unsafe { emit_control(sink, &reply) };
        }
        let reservation = CONTROL_RECORD_RESERVATION + data.len();
        if session.records.len() >= CONTROL_RECORD_LIMIT
            || session.record_bytes + reservation > CONTROL_BYTE_LIMIT
        {
            return MEDIA_BUSY;
        }
        let outcome = if session.draining {
            ENCODER_OUTCOME_FAILED
        } else if request.expected_generation != session.generation {
            ENCODER_OUTCOME_UNKNOWN
        } else {
            match request.kind {
                ENCODER_CONTROL_RECONFIGURE if data.is_empty() => {
                    session.generation = match session.generation.checked_add(1) {
                        Some(value) => value,
                        None => return STATUS_INVALID_ARGUMENT,
                    };
                    ENCODER_OUTCOME_APPLIED
                }
                ENCODER_CONTROL_EFFECTIVE_STATE if data.is_empty() => ENCODER_OUTCOME_APPLIED,
                _ => ENCODER_OUTCOME_UNSUPPORTED,
            }
        };
        let generation = session.generation;
        session.record_bytes += reservation;
        session.records.insert(
            request.request_id,
            ControlRecord {
                key,
                outcome,
                generation,
                boundary: 0,
            },
        );
        if request.kind == ENCODER_CONTROL_RECONFIGURE && outcome == ENCODER_OUTCOME_APPLIED {
            notify(session, id, ENCODER_EVENT_STATE_CHANGED);
        }
        let reply = response(request.request_id, request.kind, outcome, generation, 0);
        // SAFETY: sink lives during the synchronous callback.
        unsafe { emit_control(sink, &reply) }
    })
}
unsafe extern "C" fn subscribe(context: *mut c_void, id: u64, sink: EncoderEventSink) -> Status {
    guarded(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime keeps binding alive through this call.
        let binding = unsafe { &*context.cast::<Binding>() };
        let mut state = binding
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.authorized(&binding.owner, id) {
            return MEDIA_CLOSED;
        }
        let Some(session) = state.sources.get_mut(&id) else {
            return MEDIA_CLOSED;
        };
        if session.subscription.is_some() {
            return STATUS_INVALID_ARGUMENT;
        }
        session.subscription = Some(Event {
            context: sink.context as usize,
            notify: sink.notify,
        });
        if session.output || (session.draining && !session.eos_sent) {
            notify(session, id, ENCODER_EVENT_OUTPUT_READY);
        }
        if session.eos_sent {
            notify(session, id, ENCODER_EVENT_STOPPED);
        }
        STATUS_OK
    })
}
unsafe extern "C" fn unsubscribe(context: *mut c_void, id: u64) {
    let _ = guarded(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime keeps binding alive through this call.
        let binding = unsafe { &*context.cast::<Binding>() };
        let mut state = binding
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.authorized(&binding.owner, id)
            && let Some(session) = state.sources.get_mut(&id)
        {
            session.subscription = None;
        }
        STATUS_OK
    });
}
unsafe extern "C" fn drain(context: *mut c_void, id: u64) -> Status {
    guarded(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime keeps binding alive through this call.
        let binding = unsafe { &*context.cast::<Binding>() };
        let mut state = binding
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.authorized(&binding.owner, id) {
            return MEDIA_CLOSED;
        }
        let Some(session) = state.sources.get_mut(&id) else {
            return MEDIA_CLOSED;
        };
        if !session.draining {
            session.draining = true;
            notify(session, id, ENCODER_EVENT_OUTPUT_READY);
        }
        STATUS_OK
    })
}
unsafe extern "C" fn destroy(context: *mut c_void, id: u64) {
    let _ = guarded(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime keeps binding alive through this call.
        let binding = unsafe { &*context.cast::<Binding>() };
        let mut ids = binding
            .configured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = binding
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.authorized(&binding.owner, id) && ids.remove(&id) {
            state.sources.remove(&id);
        }
        STATUS_OK
    });
}
fn guarded(f: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(STATUS_INTERNAL_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;
    use serde_json::json;
    use std::sync::Arc;

    struct Harness {
        registration: Registration,
        pointers: Vec<*const NativeInterface>,
        id: u64,
    }
    impl Harness {
        fn new() -> Self {
            let shared = Arc::new(Mutex::new(State::default()));
            let reply=shared.lock().unwrap().control("alice",json!({"action":"open","encoder":{"clsid":"example.fake-h264","hardware_url":null,"friendly_name":"Example fake H.264"},"configuration":{"width":16,"height":16,"fps":60,"bitrate":1000000},"operation_id":"source","mode":"source","source_id":42})).0;
            let id = reply["session_id"].as_u64().unwrap();
            let registration = Registration::new(shared);
            let query = NativeInterfaceQuery {
                header: AbiHeader::of::<NativeInterfaceQuery>(),
                interface_major: ENCODER_PROVIDER_INTERFACE_MAJOR,
                min_minor: 0,
                min_size: ENCODER_PROVIDER_API_V1_0_SIZE,
            };
            let mut pointers = Vec::new();
            for name in ["alice", "bob"] {
                let mut pointer = std::ptr::null();
                // SAFETY: factory and all borrowed arguments live through bind.
                assert_eq!(
                    unsafe {
                        registration.factory.bind.unwrap()(
                            registration.factory.context,
                            BorrowedBytes::new(name.as_bytes()),
                            &query,
                            &mut pointer,
                        )
                    },
                    STATUS_OK
                );
                pointers.push(pointer);
            }
            Self {
                registration,
                pointers,
                id,
            }
        }
        fn api(&self, index: usize) -> &EncoderProviderApi {
            // SAFETY: successful bindings live until Harness::drop.
            unsafe { &*self.pointers[index].cast::<EncoderProviderApi>() }
        }
        fn common() -> EncoderCommonConfigV1 {
            EncoderCommonConfigV1 {
                version: 1,
                size: size_of::<EncoderCommonConfigV1>() as u32,
                flags: 0,
                reserved: 0,
                coded_width: 16,
                coded_height: 16,
                crop_x: 0,
                crop_y: 0,
                visible_width: 16,
                visible_height: 16,
                fps_numerator: 60,
                fps_denominator: 1,
                bitrate_bps: 1_000_000,
                profile: 0,
                level: 0,
                chroma: ENCODER_CHROMA_420,
                bit_depth: 8,
                color: FrameColorMetadata::default(),
                output_clock: FrameClockMetadata {
                    version: 1,
                    size: size_of::<FrameClockMetadata>() as u32,
                    domain: FRAME_CLOCK_STREAM_RELATIVE,
                    reserved: 0,
                    epoch_id: [1; 16],
                },
                output_clock_origin_us: 100,
            }
        }
        fn config(common: &EncoderCommonConfigV1) -> EncoderSessionConfig {
            EncoderSessionConfig {
                size: size_of::<EncoderSessionConfig>() as u32,
                mode: ENCODER_SESSION_SOURCE,
                codec: format(CODEC_H264 as u64),
                framing: format(ENCODER_FRAMING_ANNEX_B),
                source_id: 42,
                config_generation: 5,
                options_schema: format(ENCODER_SCHEMA_COMMON_CONFIG),
                options: BorrowedBytes {
                    ptr: (common as *const EncoderCommonConfigV1).cast(),
                    len: size_of::<EncoderCommonConfigV1>(),
                },
            }
        }
    }
    impl Drop for Harness {
        fn drop(&mut self) {
            for pointer in &self.pointers {
                // SAFETY: each successful binding is released once, before registration drops.
                unsafe { ((**pointer).release.unwrap())((**pointer).context) };
            }
            let _ = &self.registration;
        }
    }
    #[derive(Default)]
    struct Output {
        reject: bool,
        chunks: Vec<(Vec<u8>, u32, u64, u64, u64)>,
    }
    unsafe extern "C" fn output(context: *mut c_void, chunk: *const EncoderOutputChunk) -> Status {
        // SAFETY: test harness supplies live callback pointers.
        let (sink, chunk) = unsafe { (&mut *context.cast::<Output>(), &*chunk) };
        // SAFETY: chunk bytes are live through the callback.
        let bytes = unsafe { chunk.bytes.read().unwrap() }.to_vec();
        sink.chunks.push((
            bytes,
            chunk.flags,
            chunk.source_id,
            chunk.config_generation,
            chunk.timestamp_us,
        ));
        if sink.reject { MEDIA_BUSY } else { STATUS_OK }
    }
    #[derive(Default)]
    struct Replies {
        reject: bool,
        values: Vec<(u32, u64, u64)>,
        payloads: Vec<(u64, Vec<u8>)>,
    }
    unsafe extern "C" fn reply(
        context: *mut c_void,
        value: *const EncoderControlResponse,
    ) -> Status {
        // SAFETY: test harness supplies live callback pointers.
        let (sink, value) = unsafe { (&mut *context.cast::<Replies>(), &*value) };
        sink.values
            .push((value.outcome, value.request_id, value.effective_generation));
        // SAFETY: response data remains readable during the callback.
        let data = unsafe { value.data.read().unwrap() }.to_vec();
        sink.payloads.push((value.schema.namespace_id, data));
        if sink.reject { MEDIA_BUSY } else { STATUS_OK }
    }
    unsafe extern "C" fn event(context: *mut c_void, _id: u64, _event: u32) {
        // SAFETY: event count stays live through subscription and unsubscribe.
        unsafe {
            *context.cast::<u32>() += 1;
        }
    }
    fn control_sink(replies: &mut Replies) -> EncoderControlSink {
        EncoderControlSink {
            context: (replies as *mut Replies).cast(),
            write: reply,
        }
    }
    fn output_sink(output_state: &mut Output) -> EncoderOutputSink {
        EncoderOutputSink {
            context: (output_state as *mut Output).cast(),
            write: output,
        }
    }
    #[test]
    fn control_capacity_keeps_retry_and_poll_without_new_side_effects() {
        let h = Harness::new();
        let api = h.api(0);
        let common = Harness::common();
        let config = Harness::config(&common);
        let mut replies = Replies::default();
        let data = vec![7u8; ENCODER_PROVIDER_MAX_OPAQUE_BYTES];
        let first = EncoderControlRequest {
            size: size_of::<EncoderControlRequest>() as u32,
            kind: ENCODER_CONTROL_RECOVERY,
            request_id: 1,
            expected_generation: 5,
            schema: format(0),
            data: BorrowedBytes::new(&data),
        };
        let next = EncoderControlRequest {
            request_id: 2,
            ..first
        };
        // SAFETY: all ABI inputs and sink remain live throughout these calls.
        unsafe {
            assert_eq!(
                (api.configure_session)(api.base.context, h.id, &config),
                STATUS_OK
            );
            replies.reject = true;
            assert_eq!(
                (api.control)(api.base.context, h.id, &first, control_sink(&mut replies)),
                MEDIA_BUSY
            );
            replies.reject = false;
            assert_eq!(
                (api.control)(api.base.context, h.id, &next, control_sink(&mut replies)),
                MEDIA_BUSY
            );
            assert_eq!(
                (api.control)(api.base.context, h.id, &first, control_sink(&mut replies)),
                STATUS_OK
            );
            assert_eq!(replies.values[0], replies.values[1]);
            let poll = EncoderControlRequest {
                kind: ENCODER_CONTROL_POLL,
                data: BorrowedBytes::new(&[]),
                ..first
            };
            assert_eq!(
                (api.control)(api.base.context, h.id, &poll, control_sink(&mut replies)),
                STATUS_OK
            );
            assert_eq!(replies.values[1], replies.values[2]);
            let reconfigure = EncoderControlRequest {
                kind: ENCODER_CONTROL_RECONFIGURE,
                data: BorrowedBytes::new(&[]),
                ..next
            };
            // This small request still fits and increments the generation exactly once.
            assert_eq!(
                (api.control)(
                    api.base.context,
                    h.id,
                    &reconfigure,
                    control_sink(&mut replies)
                ),
                STATUS_OK
            );
            assert_eq!(replies.values[3].2, 6);
        }
    }
    #[test]
    fn record_count_limit_blocks_new_reconfigure_and_preserves_replay() {
        let h = Harness::new();
        let api = h.api(0);
        let common = Harness::common();
        let config = Harness::config(&common);
        let mut replies = Replies::default();
        let mut request = EncoderControlRequest {
            size: size_of::<EncoderControlRequest>() as u32,
            kind: ENCODER_CONTROL_EFFECTIVE_STATE,
            request_id: 1,
            expected_generation: 5,
            schema: format(0),
            data: BorrowedBytes::new(&[]),
        };
        // SAFETY: the binding, borrowed inputs and callback remain alive for each call.
        unsafe {
            assert_eq!(
                (api.configure_session)(api.base.context, h.id, &config),
                STATUS_OK
            );
            for id in 1..=CONTROL_RECORD_LIMIT as u64 {
                request.request_id = id;
                assert_eq!(
                    (api.control)(api.base.context, h.id, &request, control_sink(&mut replies)),
                    STATUS_OK
                );
            }
            request.request_id = CONTROL_RECORD_LIMIT as u64 + 1;
            request.kind = ENCODER_CONTROL_RECONFIGURE;
            let before = replies.values.len();
            assert_eq!(
                (api.control)(api.base.context, h.id, &request, control_sink(&mut replies)),
                MEDIA_BUSY
            );
            assert_eq!(replies.values.len(), before);
            request.request_id = 1;
            request.kind = ENCODER_CONTROL_EFFECTIVE_STATE;
            assert_eq!(
                (api.control)(api.base.context, h.id, &request, control_sink(&mut replies)),
                STATUS_OK
            );
            assert_eq!(replies.values.last().unwrap().2, 5);
            request.kind = ENCODER_CONTROL_POLL;
            assert_eq!(
                (api.control)(api.base.context, h.id, &request, control_sink(&mut replies)),
                STATUS_OK
            );
            assert_eq!(replies.values.last().unwrap().2, 5);
        }
    }
    #[test]
    fn broker_close_retires_subscription_and_rejects_later_calls() {
        let h = Harness::new();
        let api = h.api(0);
        let common = Harness::common();
        let config = Harness::config(&common);
        let mut events = 0u32;
        // SAFETY: the binding, config, and event counter remain live through close.
        unsafe {
            assert_eq!(
                (api.configure_session)(api.base.context, h.id, &config),
                STATUS_OK
            );
            assert_eq!(
                api.subscribe.unwrap()(
                    api.base.context,
                    h.id,
                    EncoderEventSink {
                        context: (&mut events as *mut u32).cast(),
                        notify: event,
                    }
                ),
                STATUS_OK
            );
            assert_eq!(events, 1);
            assert!(
                h.registration
                    ._context
                    .lock()
                    .unwrap()
                    .close("alice", h.id)
                    .is_ok()
            );
            assert_eq!((api.drain)(api.base.context, h.id), MEDIA_CLOSED);
            api.unsubscribe.unwrap()(api.base.context, h.id);
            (api.destroy_session)(api.base.context, h.id);
            assert_eq!(events, 1);
        }
    }
    #[test]
    fn source_retries_control_and_output_and_unsubscribe_is_a_barrier() {
        let h = Harness::new();
        let api = h.api(0);
        let foreign = h.api(1);
        let common = Harness::common();
        let config = Harness::config(&common);
        let mut replies = Replies::default();
        // SAFETY: binding, config, and callback contexts remain live for all calls.
        unsafe {
            assert_eq!(
                (api.probe)(api.base.context, &config, control_sink(&mut replies)),
                STATUS_OK
            );
            assert_eq!(replies.values[0].0, ENCODER_OUTCOME_APPLIED);
            assert_eq!(replies.payloads[0].0, ENCODER_FORMAT_NAMESPACE_STANDARD);
            assert_eq!(
                replies.payloads[0].1.len(),
                size_of::<EncoderProviderLimitsV1>()
            );
            assert_eq!(
                (foreign.configure_session)(foreign.base.context, h.id, &config),
                MEDIA_CLOSED
            );
            for domain in [FRAME_CLOCK_UNKNOWN, FRAME_CLOCK_HOST_MONOTONIC] {
                let mut invalid = common;
                invalid.output_clock.domain = domain;
                let invalid_config = Harness::config(&invalid);
                assert_eq!(
                    (api.configure_session)(api.base.context, h.id, &invalid_config),
                    MEDIA_UNSUPPORTED
                );
            }
            for origin in [0, u64::MAX] {
                let mut invalid = common;
                invalid.output_clock_origin_us = origin;
                let invalid_config = Harness::config(&invalid);
                assert_eq!(
                    (api.configure_session)(api.base.context, h.id, &invalid_config),
                    MEDIA_UNSUPPORTED
                );
            }
            let mut invalid_epoch = common;
            invalid_epoch.output_clock.epoch_id = [0; 16];
            let invalid_config = Harness::config(&invalid_epoch);
            assert_eq!(
                (api.configure_session)(api.base.context, h.id, &invalid_config),
                MEDIA_UNSUPPORTED
            );
            let mut wrong_common = common;
            wrong_common.coded_width = 32;
            wrong_common.visible_width = 32;
            let wrong_config = Harness::config(&wrong_common);
            assert_eq!(
                (api.configure_session)(api.base.context, h.id, &wrong_config),
                MEDIA_UNSUPPORTED
            );
            let mut wrong_source = Harness::config(&common);
            wrong_source.source_id = 43;
            assert_eq!(
                (api.configure_session)(api.base.context, h.id, &wrong_source),
                MEDIA_CLOSED
            );
            assert_eq!(
                (api.configure_session)(api.base.context, h.id, &config),
                STATUS_OK
            );
            assert_eq!((foreign.drain)(foreign.base.context, h.id), MEDIA_CLOSED);
            let mut events = 0u32;
            assert_eq!(
                api.subscribe.unwrap()(
                    api.base.context,
                    h.id,
                    EncoderEventSink {
                        context: (&mut events as *mut u32).cast(),
                        notify: event
                    }
                ),
                STATUS_OK
            );
            assert_eq!(events, 1);
            let request = EncoderControlRequest {
                size: size_of::<EncoderControlRequest>() as u32,
                kind: ENCODER_CONTROL_RECONFIGURE,
                request_id: 7,
                expected_generation: 5,
                schema: format(0),
                data: BorrowedBytes::new(&[]),
            };
            replies.reject = true;
            assert_eq!(
                (api.control)(api.base.context, h.id, &request, control_sink(&mut replies)),
                MEDIA_BUSY
            );
            assert_eq!(events, 2);
            replies.reject = false;
            assert_eq!(
                (api.control)(api.base.context, h.id, &request, control_sink(&mut replies)),
                STATUS_OK
            );
            assert_eq!(replies.values[1], replies.values[2]);
            assert_eq!(replies.values[2], (ENCODER_OUTCOME_APPLIED, 7, 6));
            assert_eq!(events, 2);
            let poll = EncoderControlRequest {
                kind: ENCODER_CONTROL_POLL,
                ..request
            };
            assert_eq!(
                (api.control)(api.base.context, h.id, &poll, control_sink(&mut replies)),
                STATUS_OK
            );
            assert_eq!(replies.values[2], replies.values[3]);
            let mut changed = request;
            changed.expected_generation = 6;
            assert_eq!(
                (api.control)(api.base.context, h.id, &changed, control_sink(&mut replies)),
                STATUS_INVALID_ARGUMENT
            );
            let mut output_state = Output {
                reject: true,
                ..Default::default()
            };
            assert_eq!(
                (api.receive)(api.base.context, h.id, 1, output_sink(&mut output_state)),
                MEDIA_BUSY
            );
            assert!(output_state.chunks.is_empty());
            assert_eq!(
                (api.receive)(api.base.context, h.id, 1024, output_sink(&mut output_state)),
                MEDIA_BUSY
            );
            output_state.reject = false;
            assert_eq!(
                (api.receive)(api.base.context, h.id, 1024, output_sink(&mut output_state)),
                STATUS_OK
            );
            assert_eq!(output_state.chunks[0], output_state.chunks[1]);
            assert_eq!(output_state.chunks[1].2, 42);
            assert_eq!(output_state.chunks[1].3, 5);
            api.unsubscribe.unwrap()(api.base.context, h.id);
            api.unsubscribe.unwrap()(api.base.context, h.id);
            assert_eq!((api.drain)(api.base.context, h.id), STATUS_OK);
            assert_eq!((api.drain)(api.base.context, h.id), STATUS_OK);
            assert_eq!(events, 2);
            assert_eq!(
                (api.receive)(api.base.context, h.id, 1024, output_sink(&mut output_state)),
                STATUS_OK
            );
            assert_eq!(output_state.chunks[2].1, ENCODER_CHUNK_END_OF_STREAM);
            assert_eq!(
                (api.receive)(api.base.context, h.id, 1024, output_sink(&mut output_state)),
                MEDIA_CLOSED
            );
            (api.destroy_session)(api.base.context, h.id);
            (api.destroy_session)(api.base.context, h.id);
            assert_eq!((api.drain)(api.base.context, h.id), MEDIA_CLOSED);
        }
    }
}
