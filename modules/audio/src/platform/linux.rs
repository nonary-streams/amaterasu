//! PulseAudio capture for Linux, including PipeWire's Pulse compatibility server.
mod cleanup_journal;
mod cleanup_retry;

use super::RecoveryConfig;
use crate::{
    model::{AudioResult, Endpoint, Format, Packet, PcmCaptureFormat},
    virtual_sink::{self, select_endpoint},
};
use amaterasu_media::{AUDIO_SILENT, AUDIO_TIMESTAMP_ESTIMATED, PCM_F32};
use cleanup_journal::{
    CleanupJournal, CleanupState, RouteIntent, RouteRestoreAction, ServerIdentity,
    SinkCleanupAction, SinkIntent, SinkObservation, can_add_sink_intent, can_start_sink_load,
    deactivate_prior_process_sinks, has_unquiesced_sink_load, route_restore_action,
    sink_cleanup_action, sink_cleanup_action_on_server, sink_cleanup_requested,
    validate_sink_intent_replacement,
};
use cleanup_retry::{SinkCleanupCoordinator, UnloadEvidence};
use std::{
    any::Any,
    collections::{BTreeMap, VecDeque},
    ffi::{CStr, CString, c_char, c_int, c_void},
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    ptr,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const PA_CHANNELS_MAX: usize = 32;
const PA_INVALID_INDEX: u32 = u32::MAX;
const PA_CONTEXT_NOAUTOSPAWN: c_int = 1;
const PA_CONTEXT_UNCONNECTED: c_int = 0;
const PA_CONTEXT_CONNECTING: c_int = 1;
const PA_CONTEXT_AUTHORIZING: c_int = 2;
const PA_CONTEXT_SETTING_NAME: c_int = 3;
const PA_CONTEXT_READY: c_int = 4;
const PA_CONTEXT_FAILED: c_int = 5;
const PA_CONTEXT_TERMINATED: c_int = 6;
const PA_ERR_ACCESS: c_int = 1;
const PA_ERR_TIMEOUT: c_int = 8;
const PA_ERR_AUTHKEY: c_int = 9;
const PA_ERR_INVALIDSERVER: c_int = 13;
const PA_OPERATION_RUNNING: c_int = 0;
const PA_OPERATION_DONE: c_int = 1;
const PA_OPERATION_CANCELLED: c_int = 2;
const PA_STREAM_READY: c_int = 2;
const PA_STREAM_FAILED: c_int = 3;
const PA_STREAM_TERMINATED: c_int = 4;
const PA_SAMPLE_FLOAT32LE: c_int = 5;
const AUDIO_RATE: u32 = 48_000;
const FRAMES_PER_PACKET: u32 = 480;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
const ROUTE_RESTORE_TIMEOUT: Duration = Duration::from_millis(750);
const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_QUERY_FAILURE_LIMIT: u8 = 3;
const STARTUP_OPEN_MAX_ATTEMPTS: usize = 2;
const STARTUP_OPEN_RETRY_DELAY: Duration = Duration::from_millis(200);
const STARTUP_OPEN_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(1_800);
const STARTUP_OPEN_TOTAL_TIMEOUT: Duration = Duration::from_secs(4);
const MAX_CAPTURE_CHUNK: usize = 1024 * 1024;
static NEXT_MANAGED_SINK: AtomicU64 = AtomicU64::new(1);
static NEXT_SINK_CLEANUP_SLOT: AtomicU64 = AtomicU64::new(0);
static CLEANUP_JOURNAL: OnceLock<Mutex<Option<SinkCleanupCoordinator>>> = OnceLock::new();
static CLEANUP_JOURNAL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[repr(C)]
struct PaMainloop {
    _private: [u8; 0],
}

#[repr(C)]
struct PaMainloopApi {
    _private: [u8; 0],
}

#[repr(C)]
struct PaContext {
    _private: [u8; 0],
}

#[repr(C)]
struct PaOperation {
    _private: [u8; 0],
}

#[repr(C)]
struct PaStream {
    _private: [u8; 0],
}

#[repr(C)]
struct PaProplist {
    _private: [u8; 0],
}

#[repr(C)]
struct PaSampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

#[repr(C)]
struct PaChannelMap {
    channels: u8,
    positions: [c_int; PA_CHANNELS_MAX],
}

#[repr(C)]
struct PaCVolume {
    channels: u8,
    values: [u32; PA_CHANNELS_MAX],
}

#[repr(C)]
struct PaBufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

#[allow(
    dead_code,
    reason = "fields preserve the installed libpulse C ABI layout"
)]
#[repr(C)]
struct PaSinkInfo {
    name: *const c_char,
    index: u32,
    description: *const c_char,
    sample_spec: PaSampleSpec,
    channel_map: PaChannelMap,
    owner_module: u32,
    volume: PaCVolume,
    mute: c_int,
    monitor_source: u32,
    monitor_source_name: *const c_char,
    latency: u64,
    driver: *const c_char,
    flags: c_int,
    proplist: *mut PaProplist,
    configured_latency: u64,
    base_volume: u32,
    state: c_int,
    volume_steps: u32,
    card: u32,
    port_count: u32,
    ports: *mut *mut c_void,
    active_port: *mut c_void,
    format_count: u8,
    formats: *mut *mut c_void,
}

#[allow(
    dead_code,
    reason = "fields preserve the installed libpulse C ABI layout"
)]
#[repr(C)]
struct PaServerInfo {
    user_name: *const c_char,
    host_name: *const c_char,
    server_version: *const c_char,
    server_name: *const c_char,
    sample_spec: PaSampleSpec,
    default_sink_name: *const c_char,
    default_source_name: *const c_char,
    cookie: u32,
    channel_map: PaChannelMap,
}

#[allow(
    dead_code,
    reason = "fields preserve the installed libpulse C ABI layout"
)]
#[repr(C)]
struct PaModuleInfo {
    index: u32,
    name: *const c_char,
    argument: *const c_char,
    n_used: u32,
    auto_unload: c_int,
    proplist: *mut PaProplist,
}

#[link(name = "pulse")]
unsafe extern "C" {
    fn pa_mainloop_new() -> *mut PaMainloop;
    fn pa_mainloop_free(mainloop: *mut PaMainloop);
    fn pa_mainloop_get_api(mainloop: *mut PaMainloop) -> *mut PaMainloopApi;
    fn pa_mainloop_iterate(mainloop: *mut PaMainloop, block: c_int, retval: *mut c_int) -> c_int;

    fn pa_context_new(api: *mut PaMainloopApi, name: *const c_char) -> *mut PaContext;
    fn pa_context_unref(context: *mut PaContext);
    fn pa_context_connect(
        context: *mut PaContext,
        server: *const c_char,
        flags: c_int,
        spawn_api: *const c_void,
    ) -> c_int;
    fn pa_context_disconnect(context: *mut PaContext);
    fn pa_context_get_state(context: *const PaContext) -> c_int;
    fn pa_context_errno(context: *const PaContext) -> c_int;
    fn pa_context_get_server(context: *const PaContext) -> *const c_char;
    fn pa_context_get_module_info_list(
        context: *mut PaContext,
        callback: Option<
            unsafe extern "C" fn(*mut PaContext, *const PaModuleInfo, c_int, *mut c_void),
        >,
        userdata: *mut c_void,
    ) -> *mut PaOperation;
    fn pa_context_get_server_info(
        context: *mut PaContext,
        callback: Option<unsafe extern "C" fn(*mut PaContext, *const PaServerInfo, *mut c_void)>,
        userdata: *mut c_void,
    ) -> *mut PaOperation;
    fn pa_context_get_sink_info_list(
        context: *mut PaContext,
        callback: Option<
            unsafe extern "C" fn(*mut PaContext, *const PaSinkInfo, c_int, *mut c_void),
        >,
        userdata: *mut c_void,
    ) -> *mut PaOperation;
    fn pa_context_set_default_sink(
        context: *mut PaContext,
        sink_name: *const c_char,
        callback: Option<unsafe extern "C" fn(*mut PaContext, c_int, *mut c_void)>,
        userdata: *mut c_void,
    ) -> *mut PaOperation;
    fn pa_context_load_module(
        context: *mut PaContext,
        module_name: *const c_char,
        arguments: *const c_char,
        callback: Option<unsafe extern "C" fn(*mut PaContext, u32, *mut c_void)>,
        userdata: *mut c_void,
    ) -> *mut PaOperation;
    fn pa_context_unload_module(
        context: *mut PaContext,
        index: u32,
        callback: Option<unsafe extern "C" fn(*mut PaContext, c_int, *mut c_void)>,
        userdata: *mut c_void,
    ) -> *mut PaOperation;
    fn pa_operation_get_state(operation: *const PaOperation) -> c_int;
    fn pa_operation_cancel(operation: *mut PaOperation);
    fn pa_operation_unref(operation: *mut PaOperation);
    fn pa_proplist_gets(proplist: *const PaProplist, key: *const c_char) -> *const c_char;
}

#[link(name = "pulse")]
unsafe extern "C" {
    fn pa_stream_new(
        context: *mut PaContext,
        name: *const c_char,
        sample_spec: *const PaSampleSpec,
        channel_map: *const PaChannelMap,
    ) -> *mut PaStream;
    fn pa_stream_unref(stream: *mut PaStream);
    fn pa_stream_disconnect(stream: *mut PaStream);
    fn pa_stream_connect_record(
        stream: *mut PaStream,
        device: *const c_char,
        buffer_attributes: *const PaBufferAttr,
        flags: c_int,
    ) -> c_int;
    fn pa_stream_get_state(stream: *const PaStream) -> c_int;
    fn pa_stream_readable_size(stream: *const PaStream) -> usize;
    fn pa_stream_peek(stream: *mut PaStream, data: *mut *const c_void, length: *mut usize)
    -> c_int;
    fn pa_stream_drop(stream: *mut PaStream) -> c_int;
}

#[derive(Clone, Debug)]
struct Sink {
    endpoint: Endpoint,
    monitor_source: String,
    channel_positions: Vec<c_int>,
    owner_module: u32,
    managed_token: Option<String>,
}

#[derive(Default)]
struct ServerInfoResult {
    completed: bool,
    failed: bool,
    default_sink: Option<String>,
    instance_cookie: Option<u32>,
}

struct ModulePresenceResult {
    target_index: u32,
    present: bool,
    completed: bool,
    failed: bool,
}

impl ModulePresenceResult {
    fn presence(&self) -> AudioResult<bool> {
        if self.failed || !self.completed {
            return Err("audio_module_inventory_unavailable");
        }
        Ok(self.present)
    }
}

struct PendingServerInfoQuery {
    operation: Operation,
    result: Box<ServerInfoResult>,
    started_at: Instant,
}

enum QueryPoll<T> {
    Pending,
    Complete(AudioResult<T>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PulseOperationState {
    Running,
    Done,
    Cancelled,
    Unknown,
}

fn pulse_operation_state(state: c_int) -> PulseOperationState {
    match state {
        PA_OPERATION_RUNNING => PulseOperationState::Running,
        PA_OPERATION_DONE => PulseOperationState::Done,
        PA_OPERATION_CANCELLED => PulseOperationState::Cancelled,
        _ => PulseOperationState::Unknown,
    }
}

fn default_query_timed_out(started_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(started_at) >= DEFAULT_QUERY_TIMEOUT
}

fn next_default_query_failure_count(failures: u8) -> u8 {
    failures.saturating_add(1).min(DEFAULT_QUERY_FAILURE_LIMIT)
}

fn default_query_failures_after_result(failures: u8, succeeded: bool) -> u8 {
    if succeeded {
        0
    } else {
        next_default_query_failure_count(failures)
    }
}

fn default_queries_disabled(failures: u8) -> bool {
    failures >= DEFAULT_QUERY_FAILURE_LIMIT
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DefaultRouteDecision {
    Pending,
    Unchanged,
    Changed,
    Retry,
    Reopen,
    Failed(&'static str),
}

fn transient_default_query_error(error: &str) -> bool {
    matches!(
        error,
        "audio_operation_timeout"
            | "audio_server_timeout"
            | "audio_server_unavailable"
            | "audio_backend_failed"
    )
}

fn default_route_decision(
    result: Option<AudioResult<Option<String>>>,
    expected_sink: &str,
    retries_exhausted: bool,
) -> DefaultRouteDecision {
    match result {
        None if retries_exhausted => DefaultRouteDecision::Reopen,
        None => DefaultRouteDecision::Pending,
        Some(Ok(Some(current))) if current == expected_sink => DefaultRouteDecision::Unchanged,
        Some(Ok(_)) => DefaultRouteDecision::Changed,
        Some(Err(error)) if transient_default_query_error(error) && retries_exhausted => {
            DefaultRouteDecision::Reopen
        }
        Some(Err(error)) if transient_default_query_error(error) => DefaultRouteDecision::Retry,
        Some(Err(error)) => DefaultRouteDecision::Failed(error),
    }
}

fn startup_open_is_side_effect_free(
    requested: &str,
    virtual_sink: Option<&str>,
    host_audio_enabled: bool,
    capture_sink_without_routing: bool,
) -> bool {
    virtual_sink.is_none()
        && host_audio_enabled
        && ((requested.is_empty() || requested == "default") || capture_sink_without_routing)
}

fn transient_startup_open_error(error: &str) -> bool {
    matches!(
        error,
        "audio_server_timeout"
            | "audio_server_unavailable"
            | "audio_operation_timeout"
            | "audio_backend_failed"
            | "audio_capture_unavailable"
    )
}

fn should_retry_startup_open(error: &str, attempt: usize, side_effect_free: bool) -> bool {
    side_effect_free && attempt < STARTUP_OPEN_MAX_ATTEMPTS && transient_startup_open_error(error)
}

fn startup_operation_timeout(deadline: Instant, now: Instant) -> AudioResult<Duration> {
    let remaining = deadline
        .checked_duration_since(now)
        .filter(|remaining| !remaining.is_zero())
        .ok_or("audio_operation_timeout")?;
    Ok(CONTROL_TIMEOUT.min(remaining))
}

fn wait_for_startup_open_retry(stop: &AtomicBool, deadline: Instant) -> bool {
    let retry_deadline = Instant::now() + STARTUP_OPEN_RETRY_DELAY;
    if retry_deadline >= deadline {
        return false;
    }
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        let remaining = retry_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        thread::park_timeout(remaining.min(Duration::from_millis(10)));
    }
}

#[derive(Default)]
struct SinkListResult {
    completed: bool,
    failed: bool,
    sinks: Vec<Sink>,
}

#[derive(Default)]
struct BooleanResult {
    completed: bool,
    succeeded: bool,
}

#[derive(Default)]
struct IndexResult {
    completed: bool,
    index: Option<u32>,
}

#[derive(Default)]
struct PendingCallbackStates {
    states: Vec<Box<dyn Any>>,
}

impl PendingCallbackStates {
    fn retain<T: 'static>(&mut self, state: Box<T>) {
        self.states.push(state);
    }
}

fn finish_operation_wait<T: 'static>(
    result: AudioResult<()>,
    callback_state: Box<T>,
    pending_callback_states: &mut PendingCallbackStates,
) -> AudioResult<Box<T>> {
    match result {
        Ok(()) => Ok(callback_state),
        Err(error) => {
            pending_callback_states.retain(callback_state);
            Err(error)
        }
    }
}

struct Operation(*mut PaOperation);

impl Drop for Operation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this guard owns the unique operation reference returned
            // by libpulse. Error paths cancel before unref and retain userdata
            // until this client's mainloop has been destroyed.
            unsafe { pa_operation_unref(self.0) };
        }
    }
}

struct PulseClient {
    mainloop: *mut PaMainloop,
    context: *mut PaContext,
    pending_callback_states: PendingCallbackStates,
    pending_default_query: Option<PendingServerInfoQuery>,
    default_queries_disabled: bool,
    default_query_failures: u8,
}

impl PulseClient {
    fn connect() -> AudioResult<Self> {
        Self::connect_with_timeout(CONTROL_TIMEOUT)
    }

    fn connect_with_timeout(timeout: Duration) -> AudioResult<Self> {
        let name = CString::new("Amaterasu Audio").expect("static client name contains no NUL");
        // SAFETY: all returned handles are checked and then owned by PulseClient.
        let mainloop = unsafe { pa_mainloop_new() };
        if mainloop.is_null() {
            return Err("audio_backend_unavailable");
        }
        // SAFETY: the mainloop is live and owns the API returned here.
        let api = unsafe { pa_mainloop_get_api(mainloop) };
        if api.is_null() {
            // SAFETY: mainloop was allocated above and no context refers to it.
            unsafe { pa_mainloop_free(mainloop) };
            return Err("audio_backend_unavailable");
        }
        // SAFETY: API and application name remain valid for context creation.
        let context = unsafe { pa_context_new(api, name.as_ptr()) };
        if context.is_null() {
            // SAFETY: the mainloop is still owned here and no context was made.
            unsafe { pa_mainloop_free(mainloop) };
            return Err("audio_backend_unavailable");
        }
        let mut client = Self {
            mainloop,
            context,
            pending_callback_states: PendingCallbackStates::default(),
            pending_default_query: None,
            default_queries_disabled: false,
            default_query_failures: 0,
        };
        // Disable PulseAudio's autospawn behavior: the audio module connects to
        // the user's existing service and never starts or restarts it.
        // SAFETY: context is live; null selects the user's configured server.
        if unsafe { pa_context_connect(context, ptr::null(), PA_CONTEXT_NOAUTOSPAWN, ptr::null()) }
            < 0
        {
            return Err(context_connection_error(context));
        }
        client.wait_until_connected(timeout)?;
        Ok(client)
    }

    fn wait_until_connected(&mut self, timeout: Duration) -> AudioResult<()> {
        let deadline = Instant::now() + timeout;
        loop {
            // Connecting, authorizing, and setting the application name are
            // normal asynchronous states. Unlike operation waits below, this
            // wait must drive the mainloop before requiring READY.
            // SAFETY: context remains owned by this client.
            let state = unsafe { pa_context_get_state(self.context) };
            if let Some(result) =
                context_connection_state(state, unsafe { pa_context_errno(self.context) })
            {
                return result;
            }
            let mut retval = 0;
            // SAFETY: the live mainloop and output pointer stay valid here.
            if unsafe { pa_mainloop_iterate(self.mainloop, 0, &mut retval) } < 0 {
                return Err(context_connection_error(self.context));
            }
            if Instant::now() >= deadline {
                return Err("audio_server_timeout");
            }
            if retval == 0 {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn is_ready(&self) -> bool {
        // SAFETY: context remains owned by this client.
        unsafe { pa_context_get_state(self.context) == PA_CONTEXT_READY }
    }

    fn iterate(&mut self) -> AudioResult<bool> {
        if !self.is_ready() {
            return Err("device_lost");
        }
        let mut retval = 0;
        // SAFETY: the live mainloop is owned by this client; retval is writable.
        let result = unsafe { pa_mainloop_iterate(self.mainloop, 0, &mut retval) };
        if result < 0 || !self.is_ready() {
            return Err("device_lost");
        }
        Ok(result > 0)
    }

    fn wait_until<T>(
        &mut self,
        timeout: Duration,
        mut poll: impl FnMut() -> Option<AudioResult<T>>,
    ) -> AudioResult<T> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(result) = poll() {
                return result;
            }
            if !self.is_ready() {
                return Err("audio_server_unavailable");
            }
            let mut retval = 0;
            // SAFETY: the loop and output pointer remain live during iteration.
            if unsafe { pa_mainloop_iterate(self.mainloop, 0, &mut retval) } < 0 {
                return Err("audio_server_unavailable");
            }
            if Instant::now() >= deadline {
                return Err("audio_operation_timeout");
            }
            if retval == 0 {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn wait_operation<T: 'static>(
        &mut self,
        pointer: *mut PaOperation,
        callback_state: Box<T>,
    ) -> AudioResult<Box<T>> {
        self.wait_operation_with_timeout(pointer, callback_state, CONTROL_TIMEOUT)
    }

    fn wait_operation_with_timeout<T: 'static>(
        &mut self,
        pointer: *mut PaOperation,
        callback_state: Box<T>,
        timeout: Duration,
    ) -> AudioResult<Box<T>> {
        if pointer.is_null() {
            return Err("audio_backend_failed");
        }
        let operation = Operation(pointer);
        let result = self.wait_until(timeout, || {
            // SAFETY: operation guard keeps the operation alive until this call ends.
            match unsafe { pa_operation_get_state(operation.0) } {
                PA_OPERATION_DONE => Some(Ok(())),
                PA_OPERATION_CANCELLED => Some(Err("audio_backend_failed")),
                _ => None,
            }
        });
        if result.is_err() {
            // SAFETY: this operation is uniquely owned and all PulseAudio API
            // access occurs on this client thread. Cancel disables its result
            // callback; retaining the boxed state until mainloop teardown also
            // covers any callback already queued before cancellation.
            unsafe { pa_operation_cancel(operation.0) };
        }
        finish_operation_wait(result, callback_state, &mut self.pending_callback_states)
    }

    fn default_sink(&mut self) -> AudioResult<Option<String>> {
        self.default_sink_with_timeout(CONTROL_TIMEOUT)
    }

    fn default_sink_with_timeout(&mut self, timeout: Duration) -> AudioResult<Option<String>> {
        let result = self.server_info_with_timeout(timeout)?;
        Ok(result.default_sink)
    }

    fn start_default_sink_query(&mut self) -> AudioResult<()> {
        if self.pending_default_query.is_some() || self.default_queries_disabled {
            return Ok(());
        }
        let mut result = Box::new(ServerInfoResult::default());
        // SAFETY: callback userdata remains owned by this client until the
        // query completes or the mainloop is destroyed during client teardown.
        let operation = unsafe {
            pa_context_get_server_info(
                self.context,
                Some(server_info_callback),
                (&mut *result as *mut ServerInfoResult).cast(),
            )
        };
        if operation.is_null() {
            self.note_default_query_failure();
            return Err("audio_backend_failed");
        }
        self.pending_default_query = Some(PendingServerInfoQuery {
            operation: Operation(operation),
            result,
            started_at: Instant::now(),
        });
        Ok(())
    }

    fn poll_default_sink_query(&mut self) -> QueryPoll<Option<String>> {
        let (operation_state, started_at) = {
            let Some(query) = self.pending_default_query.as_ref() else {
                return QueryPoll::Pending;
            };
            // SAFETY: the pending query uniquely owns a live operation reference.
            (
                pulse_operation_state(unsafe { pa_operation_get_state(query.operation.0) }),
                query.started_at,
            )
        };
        match operation_state {
            PulseOperationState::Running | PulseOperationState::Unknown
                if !default_query_timed_out(started_at, Instant::now()) =>
            {
                QueryPoll::Pending
            }
            PulseOperationState::Running | PulseOperationState::Unknown => {
                let query = self
                    .pending_default_query
                    .take()
                    .expect("the timed-out default query is still present");
                // SAFETY: this client exclusively owns the operation and all
                // Pulse calls run on this thread. Retain userdata because a
                // callback queued before cancellation can still arrive.
                unsafe { pa_operation_cancel(query.operation.0) };
                self.pending_callback_states.retain(query.result);
                self.note_default_query_failure();
                QueryPoll::Complete(Err("audio_operation_timeout"))
            }
            PulseOperationState::Done => {
                let mut query = self
                    .pending_default_query
                    .take()
                    .expect("the completed default query is still present");
                if query.result.failed || !query.result.completed {
                    self.note_default_query_failure();
                    QueryPoll::Complete(Err("audio_backend_failed"))
                } else {
                    self.default_query_failures =
                        default_query_failures_after_result(self.default_query_failures, true);
                    self.default_queries_disabled =
                        default_queries_disabled(self.default_query_failures);
                    QueryPoll::Complete(Ok(query.result.default_sink.take()))
                }
            }
            PulseOperationState::Cancelled => {
                let query = self
                    .pending_default_query
                    .take()
                    .expect("the cancelled default query is still present");
                // A callback already queued by libpulse can still run after
                // cancellation; retain its userdata until this client's mainloop
                // is destroyed, matching the synchronous operation path.
                self.pending_callback_states.retain(query.result);
                self.note_default_query_failure();
                QueryPoll::Complete(Err("audio_backend_failed"))
            }
        }
    }

    fn note_default_query_failure(&mut self) {
        self.default_query_failures =
            default_query_failures_after_result(self.default_query_failures, false);
        self.default_queries_disabled = default_queries_disabled(self.default_query_failures);
    }

    /// Completes a server query queued after earlier commands on this context.
    /// A successful result fences effects submitted earlier on this connection;
    /// queries on a replacement connection are not equivalent.
    fn quiesce_prior_commands(&mut self) -> AudioResult<()> {
        self.server_info().map(|_| ())
    }

    fn server_info(&mut self) -> AudioResult<Box<ServerInfoResult>> {
        self.server_info_with_timeout(CONTROL_TIMEOUT)
    }

    fn server_info_with_timeout(
        &mut self,
        timeout: Duration,
    ) -> AudioResult<Box<ServerInfoResult>> {
        let mut result = Box::new(ServerInfoResult::default());
        // SAFETY: callback userdata points to heap-owned result state retained
        // until completion or until this client's mainloop has been destroyed.
        let operation = unsafe {
            pa_context_get_server_info(
                self.context,
                Some(server_info_callback),
                (&mut *result as *mut ServerInfoResult).cast(),
            )
        };
        let result = self.wait_operation_with_timeout(operation, result, timeout)?;
        if result.failed || !result.completed {
            return Err("audio_backend_failed");
        }
        Ok(result)
    }

    fn server_identity(&mut self) -> AudioResult<ServerIdentity> {
        // SAFETY: libpulse returns the resolved server string owned by this
        // connected context. Copy it before any callback or context teardown.
        let address = unsafe { c_string(pa_context_get_server(self.context)) }
            .filter(|address| !address.is_empty() && address.len() <= 1024)
            .ok_or("audio_server_identity_unavailable")?;
        let instance_cookie = self
            .server_info()?
            .instance_cookie
            .ok_or("audio_server_identity_unavailable")?;
        // pa_server_info.cookie identifies a server instance, not the separate
        // authentication cookie. Match both it and the resolved endpoint before
        // interpreting a persisted module index on another connection.
        Ok(ServerIdentity {
            address,
            instance_cookie,
        })
    }

    fn module_index_present(&mut self, target_index: u32) -> AudioResult<bool> {
        let mut result = Box::new(ModulePresenceResult {
            target_index,
            present: false,
            completed: false,
            failed: false,
        });
        // SAFETY: this client owns the operation; heap callback state is kept
        // until successful completion or retained through canceled teardown.
        let operation = unsafe {
            pa_context_get_module_info_list(
                self.context,
                Some(module_info_callback),
                (&mut *result as *mut ModulePresenceResult).cast(),
            )
        };
        self.wait_operation(operation, result)?.presence()
    }

    fn sinks(&mut self) -> AudioResult<Vec<Sink>> {
        self.sinks_with_timeout(CONTROL_TIMEOUT)
    }

    fn sinks_with_timeout(&mut self, timeout: Duration) -> AudioResult<Vec<Sink>> {
        let mut result = Box::new(SinkListResult::default());
        // SAFETY: callback userdata points to heap-owned result state retained
        // until completion or until this client's mainloop has been destroyed.
        let operation = unsafe {
            pa_context_get_sink_info_list(
                self.context,
                Some(sink_info_callback),
                (&mut *result as *mut SinkListResult).cast(),
            )
        };
        let result = self.wait_operation_with_timeout(operation, result, timeout)?;
        if result.failed || !result.completed {
            return Err("audio_backend_failed");
        }
        Ok(result.sinks)
    }

    fn set_default_sink(&mut self, name: &str) -> AudioResult<()> {
        self.set_default_sink_with_timeout(name, CONTROL_TIMEOUT)
    }

    fn set_default_sink_with_timeout(&mut self, name: &str, timeout: Duration) -> AudioResult<()> {
        let name = CString::new(name).map_err(|_| "invalid_endpoint")?;
        let mut result = Box::new(BooleanResult::default());
        // SAFETY: the endpoint CString remains live through operation creation;
        // boxed callback state is retained through completion or cancellation.
        let operation = unsafe {
            pa_context_set_default_sink(
                self.context,
                name.as_ptr(),
                Some(success_callback),
                (&mut *result as *mut BooleanResult).cast(),
            )
        };
        let result = self.wait_operation_with_timeout(operation, result, timeout)?;
        if result.completed && result.succeeded {
            Ok(())
        } else {
            Err("audio_route_failed")
        }
    }

    fn load_null_sink(&mut self, name: &str, channels: u32, owner_token: &str) -> AudioResult<u32> {
        let layout = null_sink_layout(channels)?;
        let arguments = CString::new(format!(
            "sink_name={name} rate={AUDIO_RATE} format=float32le channels={channels} channel_map={} sink_properties=\"device.description=AmaterasuAudio amaterasu.audio.owner={owner_token}\"",
            layout.pulse_map
        ))
        .map_err(|_| "invalid_virtual_sink")?;
        let module = CString::new("module-null-sink").expect("static module name contains no NUL");
        let mut result = Box::new(IndexResult::default());
        // SAFETY: module arguments live through operation creation; boxed
        // callback state is retained through completion or cancellation.
        let operation = unsafe {
            pa_context_load_module(
                self.context,
                module.as_ptr(),
                arguments.as_ptr(),
                Some(index_callback),
                (&mut *result as *mut IndexResult).cast(),
            )
        };
        let result = self.wait_operation(operation, result)?;
        result.index.ok_or("virtual_sink_unavailable")
    }

    fn unload_module(&mut self, index: u32) -> AudioResult<()> {
        let mut result = Box::new(BooleanResult::default());
        // SAFETY: boxed callback state is retained through completion or
        // cancellation of the module operation.
        let operation = unsafe {
            pa_context_unload_module(
                self.context,
                index,
                Some(success_callback),
                (&mut *result as *mut BooleanResult).cast(),
            )
        };
        let result = self.wait_operation(operation, result)?;
        if result.completed && result.succeeded {
            Ok(())
        } else {
            Err("virtual_sink_cleanup_failed")
        }
    }

    fn record_stream_with_timeout(
        &mut self,
        source: &str,
        channels: u32,
        positions: &[c_int],
        timeout: Duration,
    ) -> AudioResult<PulseStream> {
        if positions.len() != channels as usize || !(1..=8).contains(&channels) {
            return Err("unsupported_channel_layout");
        }
        let stream_name =
            CString::new("Amaterasu Audio Monitor").expect("static stream name contains no NUL");
        let source = CString::new(source).map_err(|_| "invalid_endpoint")?;
        let spec = PaSampleSpec {
            format: PA_SAMPLE_FLOAT32LE,
            rate: AUDIO_RATE,
            channels: channels as u8,
        };
        let channel_map = pa_channel_map(channels, positions)?;
        let packet_bytes = FRAMES_PER_PACKET as usize * channels as usize * size_of::<f32>();
        let attributes = PaBufferAttr {
            maxlength: u32::try_from(packet_bytes.saturating_mul(4))
                .map_err(|_| "unsupported_packet_size")?,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: u32::try_from(packet_bytes).map_err(|_| "unsupported_packet_size")?,
        };
        // SAFETY: context, spec, map, and name are live for stream creation.
        let stream =
            unsafe { pa_stream_new(self.context, stream_name.as_ptr(), &spec, &channel_map) };
        if stream.is_null() {
            return Err("audio_capture_unavailable");
        }
        let capture = PulseStream(stream);
        // SAFETY: stream and all borrowed C arguments stay live through this call.
        if unsafe { pa_stream_connect_record(capture.0, source.as_ptr(), &attributes, 0) } < 0 {
            return Err("audio_capture_unavailable");
        }
        self.wait_until(timeout, || {
            // SAFETY: PulseStream guard owns a live stream until setup completes.
            match unsafe { pa_stream_get_state(capture.0) } {
                PA_STREAM_READY => Some(Ok(())),
                PA_STREAM_FAILED | PA_STREAM_TERMINATED => Some(Err("audio_capture_unavailable")),
                _ => None,
            }
        })?;
        Ok(capture)
    }
}

fn context_connection_state(state: c_int, error: c_int) -> Option<AudioResult<()>> {
    match state {
        PA_CONTEXT_READY => Some(Ok(())),
        PA_CONTEXT_FAILED | PA_CONTEXT_TERMINATED => Some(Err(context_error_code(error))),
        PA_CONTEXT_UNCONNECTED
        | PA_CONTEXT_CONNECTING
        | PA_CONTEXT_AUTHORIZING
        | PA_CONTEXT_SETTING_NAME => None,
        _ => Some(Err("audio_server_unavailable")),
    }
}

fn context_connection_error(context: *const PaContext) -> &'static str {
    // SAFETY: caller passes the live context held by PulseClient.
    context_error_code(unsafe { pa_context_errno(context) })
}

fn context_error_code(error: c_int) -> &'static str {
    match error {
        PA_ERR_ACCESS | PA_ERR_AUTHKEY => "audio_server_access_denied",
        PA_ERR_TIMEOUT => "audio_server_timeout",
        PA_ERR_INVALIDSERVER => "audio_server_configuration_invalid",
        _ => "audio_server_unavailable",
    }
}

impl Drop for PulseClient {
    fn drop(&mut self) {
        // SAFETY: context and mainloop are uniquely owned and destroyed in
        // dependency order. `pending_callback_states` is a field and therefore
        // remains alive through this body, including mainloop teardown, before
        // Rust drops that field after this method returns.
        unsafe {
            if let Some(query) = &self.pending_default_query {
                // SAFETY: the pending query uniquely owns this operation and
                // callback userdata remains in the query until teardown ends.
                pa_operation_cancel(query.operation.0);
            }
            if !self.context.is_null() {
                pa_context_disconnect(self.context);
                pa_context_unref(self.context);
            }
            if !self.mainloop.is_null() {
                pa_mainloop_free(self.mainloop);
            }
        }
    }
}

struct PulseStream(*mut PaStream);

impl Drop for PulseStream {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this stream guard owns the stream reference; disconnect
            // ends server delivery before the final local reference is freed.
            unsafe {
                pa_stream_disconnect(self.0);
                pa_stream_unref(self.0);
            }
        }
    }
}

unsafe extern "C" fn server_info_callback(
    _context: *mut PaContext,
    info: *const PaServerInfo,
    userdata: *mut c_void,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if userdata.is_null() {
            return;
        }
        // SAFETY: userdata points to ServerInfoResult owned by the waiting call.
        let result = unsafe { &mut *userdata.cast::<ServerInfoResult>() };
        result.completed = true;
        if info.is_null() {
            result.failed = true;
            return;
        }
        // SAFETY: libpulse keeps the info and its string fields live for this callback.
        result.default_sink = unsafe { c_string((*info).default_sink_name) };
        // SAFETY: the server info is live for this callback; copy the scalar.
        result.instance_cookie = Some(unsafe { (*info).cookie });
    }));
}

unsafe extern "C" fn sink_info_callback(
    _context: *mut PaContext,
    info: *const PaSinkInfo,
    end_of_list: c_int,
    userdata: *mut c_void,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if userdata.is_null() {
            return;
        }
        // SAFETY: userdata points to SinkListResult owned by the waiting call.
        let result = unsafe { &mut *userdata.cast::<SinkListResult>() };
        if info.is_null() {
            result.completed = end_of_list != 0;
            result.failed = end_of_list < 0;
            return;
        }
        // SAFETY: libpulse owns info and its nested fields for this callback.
        if let Some(sink) = unsafe { read_sink_info(&*info) } {
            result.sinks.push(sink);
        } else {
            result.failed = true;
        }
    }));
}

unsafe extern "C" fn module_info_callback(
    _context: *mut PaContext,
    info: *const PaModuleInfo,
    end_of_list: c_int,
    userdata: *mut c_void,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if userdata.is_null() {
            return;
        }
        // SAFETY: userdata is the boxed query result kept alive by PulseClient.
        let result = unsafe { &mut *userdata.cast::<ModulePresenceResult>() };
        if info.is_null() {
            result.completed = end_of_list > 0;
            result.failed |= end_of_list < 0;
            return;
        }
        // SAFETY: the native module descriptor remains live for the callback.
        result.present |= unsafe { (*info).index == result.target_index };
    }));
}

unsafe extern "C" fn success_callback(
    _context: *mut PaContext,
    success: c_int,
    userdata: *mut c_void,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if userdata.is_null() {
            return;
        }
        // SAFETY: userdata points to BooleanResult owned by the waiting call.
        let result = unsafe { &mut *userdata.cast::<BooleanResult>() };
        result.completed = true;
        result.succeeded = success != 0;
    }));
}

unsafe extern "C" fn index_callback(_context: *mut PaContext, index: u32, userdata: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if userdata.is_null() {
            return;
        }
        // SAFETY: userdata points to IndexResult owned by the waiting call.
        let result = unsafe { &mut *userdata.cast::<IndexResult>() };
        result.completed = true;
        if index != PA_INVALID_INDEX {
            result.index = Some(index);
        }
    }));
}

unsafe fn read_sink_info(info: &PaSinkInfo) -> Option<Sink> {
    let id = unsafe { c_string(info.name)? };
    let description = unsafe { c_string(info.description) };
    let adapter_name = unsafe {
        proplist_string(info.proplist, c"device.product.name")
            .or_else(|| proplist_string(info.proplist, c"device.description"))
    };
    let name = description.clone().unwrap_or_else(|| id.clone());
    let endpoint = Endpoint {
        id,
        name,
        description,
        adapter_name,
        default_multimedia: false,
    };
    let monitor_source = unsafe { c_string(info.monitor_source_name) }?;
    let channel_count = usize::from(info.channel_map.channels);
    let sample_channels = usize::from(info.sample_spec.channels);
    if channel_count == 0 || channel_count > PA_CHANNELS_MAX || channel_count != sample_channels {
        return None;
    }
    let channel_positions = info.channel_map.positions[..channel_count].to_vec();
    Some(Sink {
        endpoint,
        monitor_source,
        channel_positions,
        owner_module: info.owner_module,
        managed_token: unsafe { proplist_string(info.proplist, c"amaterasu.audio.owner") },
    })
}

unsafe fn proplist_string(proplist: *const PaProplist, key: &CStr) -> Option<String> {
    if proplist.is_null() {
        return None;
    }
    // SAFETY: proplist is owned by libpulse for the callback; key is NUL-terminated.
    let value = unsafe { pa_proplist_gets(proplist, key.as_ptr()) };
    unsafe { c_string(value) }
}

unsafe fn c_string(value: *const c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // SAFETY: every input is a libpulse-owned NUL-terminated string valid for
    // this callback or operation result.
    Some(
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[derive(Clone, Copy, Debug)]
struct NullSinkLayout {
    pulse_map: &'static str,
    positions: &'static [c_int],
}

const STEREO_POSITIONS: &[c_int] = &[1, 2];
const SURROUND_51_POSITIONS: &[c_int] = &[1, 2, 3, 7, 5, 6];
const SURROUND_71_POSITIONS: &[c_int] = &[1, 2, 3, 7, 5, 6, 10, 11];

fn null_sink_layout(channels: u32) -> AudioResult<NullSinkLayout> {
    match channels {
        2 => Ok(NullSinkLayout {
            pulse_map: "front-left,front-right",
            positions: STEREO_POSITIONS,
        }),
        6 => Ok(NullSinkLayout {
            pulse_map: "front-left,front-right,front-center,lfe,rear-left,rear-right",
            positions: SURROUND_51_POSITIONS,
        }),
        8 => Ok(NullSinkLayout {
            pulse_map: "front-left,front-right,front-center,lfe,rear-left,rear-right,side-left,side-right",
            positions: SURROUND_71_POSITIONS,
        }),
        _ => Err("unsupported_channel_layout"),
    }
}

fn capture_channel_layout(
    endpoint_positions: &[c_int],
    virtual_channels: Option<u32>,
    requested: Option<PcmCaptureFormat>,
) -> AudioResult<(Vec<c_int>, u32)> {
    if let Some(channels) = virtual_channels {
        let layout = null_sink_layout(channels)?;
        if let Some(requested) = requested {
            requested.validate()?;
            if requested.channels != channels {
                return Err("invalid_capture_format");
            }
        }
        return Ok((layout.positions.to_vec(), channels));
    }

    if let Some(requested) = requested {
        requested.validate()?;
        let layout = null_sink_layout(requested.channels)?;
        return Ok((layout.positions.to_vec(), requested.channels));
    }

    if !(1..=8).contains(&endpoint_positions.len()) {
        return Err("unsupported_format");
    }
    Ok((endpoint_positions.to_vec(), endpoint_positions.len() as u32))
}

fn pa_channel_map(channels: u32, positions: &[c_int]) -> AudioResult<PaChannelMap> {
    if positions.len() != channels as usize || positions.len() > PA_CHANNELS_MAX {
        return Err("unsupported_channel_layout");
    }
    let mut map = PaChannelMap {
        channels: channels as u8,
        positions: [0; PA_CHANNELS_MAX],
    };
    map.positions[..positions.len()].copy_from_slice(positions);
    Ok(map)
}

fn channel_mask(positions: &[c_int]) -> u32 {
    let mut mask = 0;
    for position in positions {
        let speaker = match *position {
            0 => 0x0004,  // mono maps to front center
            1 => 0x0001,  // front left
            2 => 0x0002,  // front right
            3 => 0x0004,  // front center
            7 => 0x0008,  // low frequency
            5 => 0x0010,  // rear left
            6 => 0x0020,  // rear right
            8 => 0x0040,  // front left of center
            9 => 0x0080,  // front right of center
            4 => 0x0100,  // rear center
            10 => 0x0200, // side left
            11 => 0x0400, // side right
            _ => return 0,
        };
        mask |= speaker;
    }
    mask
}

#[derive(Debug)]
pub(super) struct ManagedVirtualSink {
    name: String,
    owner_token: String,
    channels: u32,
    module_index: Mutex<Option<u32>>,
}

impl ManagedVirtualSink {
    fn ensure(&self, client: &mut PulseClient) -> AudioResult<()> {
        let state = read_cleanup_state()?;
        let existing = state.sinks.get(&self.name);
        if existing.is_some_and(|intent| intent.module_index.is_none() && !intent.load_quiesced) {
            return Err("audio_sink_load_pending");
        }
        let server_identity = client.server_identity()?;
        validate_sink_intent_replacement(existing, &self.owner_token, Some(&server_identity))?;
        let sinks = client.sinks()?;
        if sinks.iter().any(|sink| {
            sink.endpoint.id == self.name
                && sink.managed_token.as_deref() == Some(self.owner_token.as_str())
        }) {
            return Ok(());
        }
        if sinks.iter().any(|sink| sink.endpoint.id == self.name) {
            return Err("virtual_sink_name_conflict");
        }
        persist_sink_intent(
            &self.name,
            &self.owner_token,
            Some(&server_identity),
            None,
            false,
            true,
        )?;
        let index = match client.load_null_sink(&self.name, self.channels, &self.owner_token) {
            Ok(index) => index,
            Err(error) => {
                if client.quiesce_prior_commands().is_ok() {
                    let _ = mark_sink_load_quiesced(&self.name, &self.owner_token);
                }
                retry_virtual_sink_cleanups_with_client(client, Some(&self.name));
                return Err(error);
            }
        };
        if let Err(error) = persist_sink_intent(
            &self.name,
            &self.owner_token,
            Some(&server_identity),
            Some(index),
            true,
            false,
        ) {
            let _ = request_sink_cleanup(&self.name, &self.owner_token);
            retry_virtual_sink_cleanups_with_client(client, Some(&self.name));
            return Err(error);
        }
        *self
            .module_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(index);
        Ok(())
    }
}

impl Drop for ManagedVirtualSink {
    fn drop(&mut self) {
        if self
            .module_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            return;
        }
        let _ = request_sink_cleanup(&self.name, &self.owner_token);
        let _cleanup_guard = virtual_sink_cleanup_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(mut client) = PulseClient::connect() {
            retry_virtual_sink_cleanups_with_client(&mut client, Some(&self.name));
        }
    }
}

static VIRTUAL_SINK_CLEANUP_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn virtual_sink_cleanup_lock() -> &'static Mutex<()> {
    VIRTUAL_SINK_CLEANUP_LOCK.get_or_init(|| Mutex::new(()))
}

fn cleanup_journal_lock() -> &'static Mutex<()> {
    CLEANUP_JOURNAL_LOCK.get_or_init(|| Mutex::new(()))
}

fn read_cleanup_state() -> AudioResult<CleanupState> {
    let _guard = cleanup_journal_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let journal = CLEANUP_JOURNAL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    journal
        .as_ref()
        .ok_or("audio_cleanup_journal_unavailable")?
        .read()
}

fn update_cleanup_state(update: impl FnOnce(&mut CleanupState)) -> AudioResult<()> {
    let _guard = cleanup_journal_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut journal = CLEANUP_JOURNAL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let journal = journal
        .as_mut()
        .ok_or("audio_cleanup_journal_unavailable")?;
    journal.update(|state| {
        update(state);
        Ok(())
    })
}

fn persist_sink_intent(
    name: &str,
    owner_token: &str,
    server_identity: Option<&ServerIdentity>,
    module_index: Option<u32>,
    lease_active: bool,
    cleanup_requested: bool,
) -> AudioResult<()> {
    let _guard = cleanup_journal_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut journal = CLEANUP_JOURNAL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    journal
        .as_mut()
        .ok_or("audio_cleanup_journal_unavailable")?
        .update(|state| {
            validate_sink_intent_replacement(state.sinks.get(name), owner_token, server_identity)?;
            if !can_add_sink_intent(state, name) {
                return Err("audio_cleanup_journal_capacity_reached");
            }
            state.sinks.insert(
                name.to_owned(),
                SinkIntent {
                    owner_token: owner_token.to_owned(),
                    server_identity: server_identity.cloned(),
                    module_index,
                    lease_active,
                    cleanup_requested,
                    load_quiesced: module_index.is_some(),
                    unload_submitted: false,
                    unload_confirmed: false,
                    unload_quiesced: false,
                },
            );
            Ok(())
        })
}

fn mark_sink_load_quiesced(name: &str, owner_token: &str) -> AudioResult<()> {
    let mut found = false;
    update_cleanup_state(|state| {
        if let Some(intent) = state
            .sinks
            .get_mut(name)
            .filter(|intent| intent.owner_token == owner_token && intent.module_index.is_none())
        {
            found = true;
            intent.load_quiesced = true;
        }
    })?;
    if found {
        Ok(())
    } else {
        Err("audio_cleanup_journal_conflict")
    }
}

fn request_sink_cleanup(name: &str, owner_token: &str) -> AudioResult<()> {
    let _guard = cleanup_journal_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut journal = CLEANUP_JOURNAL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    journal
        .as_mut()
        .ok_or("audio_cleanup_journal_unavailable")?
        .request_retirement(name, owner_token)
}

fn mark_sink_unload_submitted(name: &str, owner_token: &str) -> AudioResult<()> {
    let mut found = false;
    update_cleanup_state(|state| {
        if let Some(intent) = state
            .sinks
            .get_mut(name)
            .filter(|intent| intent.owner_token == owner_token)
        {
            found = true;
            intent.unload_submitted = true;
            intent.unload_confirmed = false;
            intent.unload_quiesced = false;
        }
    })?;
    if found {
        Ok(())
    } else {
        Err("audio_cleanup_journal_conflict")
    }
}

fn record_sink_unload_evidence(
    name: &str,
    owner_token: &str,
    module_index: u32,
    server_identity: Option<&ServerIdentity>,
    evidence: UnloadEvidence,
) -> AudioResult<()> {
    let _guard = cleanup_journal_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut journal = CLEANUP_JOURNAL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    journal
        .as_mut()
        .ok_or("audio_cleanup_journal_unavailable")?
        .record_unload(name, owner_token, module_index, server_identity, evidence)
}

fn remove_sink_intent(name: &str, owner_token: &str) -> AudioResult<()> {
    update_cleanup_state(|state| {
        if state
            .sinks
            .get(name)
            .is_some_and(|intent| intent.owner_token == owner_token)
        {
            state.sinks.remove(name);
        }
    })
}

fn persist_route_intent(target_sink: &str, original_sink: &str) -> AudioResult<()> {
    update_cleanup_state(|state| {
        state.route = Some(RouteIntent {
            target_sink: target_sink.to_owned(),
            original_sink: original_sink.to_owned(),
            assignment_confirmed: false,
            assignment_quiesced: false,
            restoration_submitted: false,
            restoration_confirmed: false,
            restoration_quiesced: false,
        });
    })
}

fn mark_route_assignment_quiesced(target_sink: &str) -> AudioResult<()> {
    let mut found = false;
    update_cleanup_state(|state| {
        if let Some(route) = state
            .route
            .as_mut()
            .filter(|route| route.target_sink == target_sink)
        {
            found = true;
            route.assignment_quiesced = true;
        }
    })?;
    if found {
        Ok(())
    } else {
        Err("audio_cleanup_journal_conflict")
    }
}

fn confirm_route_intent(target_sink: &str) -> AudioResult<()> {
    let mut found = false;
    update_cleanup_state(|state| {
        if let Some(route) = state
            .route
            .as_mut()
            .filter(|route| route.target_sink == target_sink)
        {
            found = true;
            route.assignment_confirmed = true;
        }
    })?;
    if found {
        Ok(())
    } else {
        Err("audio_cleanup_journal_conflict")
    }
}

fn mark_route_restoration_submitted(target_sink: &str) -> AudioResult<()> {
    let mut found = false;
    update_cleanup_state(|state| {
        if let Some(route) = state
            .route
            .as_mut()
            .filter(|route| route.target_sink == target_sink)
        {
            found = true;
            route.restoration_submitted = true;
            route.restoration_confirmed = false;
            route.restoration_quiesced = false;
        }
    })?;
    if found {
        Ok(())
    } else {
        Err("audio_cleanup_journal_conflict")
    }
}

fn mark_route_restoration_quiesced(target_sink: &str) -> AudioResult<()> {
    let mut found = false;
    update_cleanup_state(|state| {
        if let Some(route) = state
            .route
            .as_mut()
            .filter(|route| route.target_sink == target_sink)
        {
            found = true;
            route.restoration_quiesced = true;
        }
    })?;
    if found {
        Ok(())
    } else {
        Err("audio_cleanup_journal_conflict")
    }
}

fn mark_route_restoration_confirmed(target_sink: &str) -> AudioResult<()> {
    let mut found = false;
    update_cleanup_state(|state| {
        if let Some(route) = state
            .route
            .as_mut()
            .filter(|route| route.target_sink == target_sink)
        {
            found = true;
            route.restoration_confirmed = true;
        }
    })?;
    if found {
        Ok(())
    } else {
        Err("audio_cleanup_journal_conflict")
    }
}

fn clear_route_intent(target_sink: &str) -> AudioResult<()> {
    let mut conflict = false;
    update_cleanup_state(|state| match state.route.as_ref() {
        Some(route) if route.target_sink == target_sink => state.route = None,
        Some(_) => conflict = true,
        None => (),
    })?;
    if conflict {
        Err("audio_cleanup_journal_conflict")
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn owns_virtual_sink_module(sinks: &[Sink], sink_name: &str, module_index: u32) -> bool {
    sinks
        .iter()
        .any(|sink| sink.endpoint.id == sink_name && sink.owner_module == module_index)
}

fn retry_deferred_sink_cleanup() -> AudioResult<()> {
    let _guard = cleanup_journal_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut journal = CLEANUP_JOURNAL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    journal
        .as_mut()
        .ok_or("audio_cleanup_journal_unavailable")?
        .retry_pending()
}

fn reconcile_absent_sink_module(
    client: &mut PulseClient,
    name: &str,
    intent: &SinkIntent,
) -> AudioResult<()> {
    let Some(expected_server) = intent.server_identity.as_ref() else {
        return Ok(());
    };
    let Some(module_index) = intent
        .module_index
        .filter(|_| intent.load_quiesced && sink_cleanup_requested(intent))
    else {
        return Ok(());
    };
    let current_server = client.server_identity()?;
    if &current_server != expected_server || client.module_index_present(module_index)? {
        return Ok(());
    }
    // A complete module inventory plus this sink readback proves physical
    // retirement. A lost unload reply alone is not completion, and a query on
    // a different server cannot interpret this saved native module index.
    if client.sinks()?.iter().any(|sink| sink.endpoint.id == name) {
        return Ok(());
    }
    let observation = SinkObservation::SinkAndModuleAbsent {
        server_identity: &current_server,
        module_index,
    };
    if sink_cleanup_action(intent, observation) != SinkCleanupAction::ClearConfirmedAbsent {
        return Ok(());
    }
    update_cleanup_state(|state| {
        // Do not retire a changed owner, module, server, or reacquired lease
        // using a proof collected for the previous intent.
        if state.sinks.get(name) == Some(intent) {
            state.sinks.remove(name);
        }
    })
}

fn retry_virtual_sink_cleanups_with_client(client: &mut PulseClient, name_filter: Option<&str>) {
    if retry_deferred_sink_cleanup().is_err() {
        return;
    }
    let Ok(state) = read_cleanup_state() else {
        return;
    };
    let pending = state
        .sinks
        .into_iter()
        .filter(|(_, intent)| sink_cleanup_requested(intent))
        .filter(|(name, _)| name_filter.is_none_or(|filter| name == filter))
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return;
    }
    let cleanup_slot =
        NEXT_SINK_CLEANUP_SLOT.fetch_add(1, Ordering::Relaxed) as usize % pending.len();
    let pending = pending.into_iter().skip(cleanup_slot).take(1);
    let Ok(sinks) = client.sinks() else {
        return;
    };
    for (name, intent) in pending {
        let sink = sinks.iter().find(|sink| sink.endpoint.id == name);
        let observation = sink.map_or(SinkObservation::Absent, |sink| {
            match sink.managed_token.as_deref() {
                Some(token) => SinkObservation::Owned {
                    owner_token: token,
                    module_index: sink.owner_module,
                },
                None => SinkObservation::Foreign,
            }
        });
        let current_server = intent
            .server_identity
            .as_ref()
            .and_then(|_| client.server_identity().ok());
        match sink_cleanup_action_on_server(&intent, current_server.as_ref(), observation) {
            SinkCleanupAction::KeepTombstone => {
                if observation == SinkObservation::Absent {
                    let _ = reconcile_absent_sink_module(client, &name, &intent);
                }
                continue;
            }
            SinkCleanupAction::ClearConfirmedAbsent => {
                let _ = remove_sink_intent(&name, &intent.owner_token);
                continue;
            }
            SinkCleanupAction::Unload(module_index) => {
                if intent.module_index.is_none()
                    && persist_sink_intent(
                        &name,
                        &intent.owner_token,
                        intent.server_identity.as_ref(),
                        Some(module_index),
                        false,
                        true,
                    )
                    .is_err()
                {
                    continue;
                }
                if mark_sink_unload_submitted(&name, &intent.owner_token).is_err() {
                    continue;
                }
                if client.unload_module(module_index).is_err() {
                    if client.quiesce_prior_commands().is_ok() {
                        let _ = record_sink_unload_evidence(
                            &name,
                            &intent.owner_token,
                            module_index,
                            intent.server_identity.as_ref(),
                            UnloadEvidence::Quiesced,
                        );
                    } else {
                        continue;
                    }
                } else if record_sink_unload_evidence(
                    &name,
                    &intent.owner_token,
                    module_index,
                    intent.server_identity.as_ref(),
                    UnloadEvidence::Confirmed,
                )
                .is_err()
                {
                    continue;
                }
            }
        }
        let Ok(after_unload) = client.sinks() else {
            continue;
        };
        if !after_unload.iter().any(|remaining| {
            remaining.endpoint.id == name
                && remaining.managed_token.as_deref() == Some(intent.owner_token.as_str())
        }) {
            let _ = remove_sink_intent(&name, &intent.owner_token);
        }
    }
}

fn retry_virtual_sink_cleanups() {
    if !virtual_sink_cleanup_pending() {
        return;
    }
    let _cleanup_guard = virtual_sink_cleanup_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if retry_deferred_sink_cleanup().is_err() {
        return;
    }
    let Ok(mut client) = PulseClient::connect() else {
        return;
    };
    retry_virtual_sink_cleanups_with_client(&mut client, None);
}

pub fn virtual_sink_cleanup_pending() -> bool {
    let _guard = cleanup_journal_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let journal = CLEANUP_JOURNAL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    journal.as_ref().is_some_and(|journal| {
        journal.has_deferred_cleanup()
            || journal
                .read()
                .is_ok_and(|state| state.sinks.values().any(sink_cleanup_requested))
    })
}

static VIRTUAL_SINKS: OnceLock<Mutex<BTreeMap<String, Weak<ManagedVirtualSink>>>> = OnceLock::new();

fn managed_virtual_sink(
    client: &mut PulseClient,
    channels: u32,
) -> AudioResult<Arc<ManagedVirtualSink>> {
    let layout = null_sink_layout(channels)?;
    let sequence = NEXT_MANAGED_SINK.fetch_add(1, Ordering::Relaxed);
    let token = format!("{}-{sequence}", std::process::id());
    let name = format!(
        "amaterasu_stream_{}_{}ch_{sequence}",
        std::process::id(),
        channels
    );
    let cleanup_guard = virtual_sink_cleanup_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    retry_virtual_sink_cleanups_with_client(client, Some(&name));
    if read_cleanup_state()?.sinks.contains_key(&name) {
        return Err("virtual_sink_cleanup_pending");
    }
    let state = read_cleanup_state()?;
    if !can_start_sink_load(&state, &name) {
        if has_unquiesced_sink_load(&state) {
            return Err("audio_sink_load_pending");
        }
        return Err("audio_cleanup_journal_capacity_reached");
    }
    let registry = VIRTUAL_SINKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut registry = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = registry.get(&name).and_then(Weak::upgrade) {
        let ensure_result = existing.ensure(client);
        drop(registry);
        drop(cleanup_guard);
        ensure_result?;
        return Ok(existing);
    }

    if client.sinks()?.iter().any(|sink| sink.endpoint.id == name) {
        return Err("virtual_sink_name_conflict");
    }
    let server_identity = client.server_identity()?;
    persist_sink_intent(&name, &token, Some(&server_identity), None, false, true)?;
    let index = match client.load_null_sink(&name, channels, &token) {
        Ok(index) => index,
        Err(error) => {
            if client.quiesce_prior_commands().is_ok() {
                let _ = mark_sink_load_quiesced(&name, &token);
            }
            retry_virtual_sink_cleanups_with_client(client, Some(&name));
            return Err(error);
        }
    };
    if let Err(error) = persist_sink_intent(
        &name,
        &token,
        Some(&server_identity),
        Some(index),
        true,
        false,
    ) {
        let _ = request_sink_cleanup(&name, &token);
        retry_virtual_sink_cleanups_with_client(client, Some(&name));
        return Err(error);
    }
    let module_index = Some(index);
    let lease = Arc::new(ManagedVirtualSink {
        name: name.clone(),
        owner_token: token,
        channels,
        module_index: Mutex::new(module_index),
    });
    // The generated null-sink map is fixed by its requested channel layout.
    debug_assert_eq!(layout.positions.len(), channels as usize);
    registry.insert(name, Arc::downgrade(&lease));
    drop(registry);
    drop(cleanup_guard);
    Ok(lease)
}

#[derive(Debug)]
struct ActiveRoute {
    endpoint: String,
    original: Option<String>,
    references: usize,
    assignment_confirmed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RoutePlan {
    Skip,
    Track,
    Assign,
}

fn route_plan(
    endpoint: &str,
    current_default: Option<&str>,
    capture_only: bool,
    explicit_route: bool,
    virtual_route: bool,
) -> RoutePlan {
    if capture_only || (!explicit_route && !virtual_route) {
        RoutePlan::Skip
    } else if current_default == Some(endpoint) {
        if virtual_route {
            RoutePlan::Track
        } else {
            RoutePlan::Skip
        }
    } else {
        RoutePlan::Assign
    }
}

#[derive(Default, Debug)]
struct RouteRegistry {
    active: Option<ActiveRoute>,
}

impl RouteRegistry {
    fn restore_pending(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.references == 0)
    }

    fn abandon_idle_restore(&mut self) {
        if self.restore_pending() {
            // Keep the durable intent until a new assignment replaces it.
            // Failed cleanup is diagnostic state, not an admission lock.
            eprintln!("Amaterasu audio route restore remains pending; allowing a new stream");
            self.active = None;
        }
    }
}

fn record_pending_route(registry: &mut RouteRegistry, endpoint: &str, original: Option<String>) {
    registry.active = Some(ActiveRoute {
        endpoint: endpoint.to_owned(),
        original,
        references: 0,
        assignment_confirmed: false,
    });
}

#[derive(Debug)]
pub(super) struct RouteLease {
    endpoint: String,
}

static ROUTES: OnceLock<Mutex<RouteRegistry>> = OnceLock::new();

fn route_registry() -> &'static Mutex<RouteRegistry> {
    ROUTES.get_or_init(|| Mutex::new(RouteRegistry::default()))
}

// PipeWire's PulseAudio compatibility server can acknowledge a default change
// before policy has published it through server info. Observe that transition;
// never submit a second write or wait through a third-party route change.
fn wait_for_route_readback(
    desired: &str,
    previous: &str,
    timeout: Duration,
    mut read: impl FnMut(Duration) -> AudioResult<Option<String>>,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match read(remaining.min(Duration::from_millis(50))) {
            Ok(Some(current)) if current == desired => return true,
            Ok(Some(current)) if current != previous => return false,
            Err(_) => return false,
            _ => {}
        }
        thread::park_timeout(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(5)),
        );
    }
}

fn acquire_route(
    client: &mut PulseClient,
    endpoint: &str,
    current_default: Option<&str>,
    capture_only: bool,
    explicit_route: bool,
    virtual_route: bool,
) -> AudioResult<Option<Arc<RouteLease>>> {
    let plan = route_plan(
        endpoint,
        current_default,
        capture_only,
        explicit_route,
        virtual_route,
    );
    if plan == RoutePlan::Skip {
        return Ok(None);
    }
    let mut registry = route_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    retry_route_restore(client, &mut registry);
    registry.abandon_idle_restore();

    if registry.active.is_none() {
        let _ = reconcile_persisted_route(client);
        if read_cleanup_state()?.route.is_some() {
            eprintln!("Amaterasu audio route recovery remains pending; allowing a new stream");
        }
    }

    if let Some(active) = &mut registry.active {
        if active.endpoint != endpoint {
            return Err("audio_route_conflict");
        }
        active.references = active
            .references
            .checked_add(1)
            .ok_or("audio_route_conflict")?;
        return Ok(Some(Arc::new(RouteLease {
            endpoint: endpoint.to_owned(),
        })));
    }

    // Recovery above can change the default. Do not use the caller's snapshot
    // as the original for this stream or reject its successful restoration.
    let observed_default = client.default_sink()?;
    let pending_route = read_cleanup_state()?.route;
    let original = replacement_route_original(pending_route.as_ref(), observed_default.as_deref());
    let plan = route_plan(
        endpoint,
        observed_default.as_deref(),
        capture_only,
        explicit_route,
        virtual_route,
    );
    let needs_assignment = plan == RoutePlan::Assign;
    if needs_assignment && original.is_none() {
        // PulseAudio has no API to restore an unset default sink. Keep capture
        // available without claiming ownership of a route we cannot restore.
        return Ok(None);
    }
    if needs_assignment {
        let original_sink = original.as_deref().ok_or("audio_route_failed")?;
        if client.default_sink()? != observed_default {
            return Err("audio_route_changed");
        }
        // Durable intent precedes the global route mutation. If the callback
        // is lost, a restart can check the target before attempting restore.
        persist_route_intent(endpoint, original_sink)?;
        record_pending_route(&mut registry, endpoint, original.clone());
        if client.set_default_sink(endpoint).is_err() {
            eprintln!("Amaterasu audio route assignment was not acknowledged");
            if client.quiesce_prior_commands().is_ok() {
                let _ = mark_route_assignment_quiesced(endpoint);
            }
            retry_route_restore(client, &mut registry);
            return Err("audio_route_failed");
        }
        if confirm_route_intent(endpoint).is_err() {
            retry_route_restore(client, &mut registry);
            return Err("audio_cleanup_journal_unavailable");
        }
        if let Some(active) = registry.active.as_mut() {
            active.assignment_confirmed = true;
        }
        if !wait_for_route_readback(
            endpoint,
            observed_default
                .as_deref()
                .expect("assignment requires a previous route"),
            Duration::from_millis(250),
            |timeout| client.default_sink_with_timeout(timeout),
        ) {
            eprintln!("Amaterasu audio route assignment acknowledged but readback differs");
            retry_route_restore(client, &mut registry);
            return Err("audio_route_failed");
        }
        let Some(active) = registry.active.as_mut() else {
            return Err("audio_route_failed");
        };
        active.references = 1;
    } else if !virtual_route {
        return Ok(None);
    } else {
        registry.active = Some(ActiveRoute {
            endpoint: endpoint.to_owned(),
            original,
            references: 1,
            assignment_confirmed: true,
        });
    }
    Ok(Some(Arc::new(RouteLease {
        endpoint: endpoint.to_owned(),
    })))
}

impl Drop for RouteLease {
    fn drop(&mut self) {
        let registry = route_registry();
        let mut registry = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(active) = &mut registry.active else {
            return;
        };
        if active.endpoint != self.endpoint || active.references == 0 {
            return;
        }
        active.references -= 1;
        if active.references == 0
            && let Ok(mut client) = PulseClient::connect_with_timeout(ROUTE_RESTORE_TIMEOUT)
        {
            retry_route_restore(&mut client, &mut registry);
        }
    }
}

fn retry_route_restore(client: &mut PulseClient, registry: &mut RouteRegistry) {
    let Some(active) = registry
        .active
        .as_ref()
        .filter(|active| active.references == 0)
    else {
        return;
    };
    let intent = match read_cleanup_state().ok().and_then(|state| state.route) {
        Some(intent) => intent,
        None => {
            let Some(original_sink) = active.original.clone() else {
                registry.active = None;
                return;
            };
            RouteIntent {
                target_sink: active.endpoint.clone(),
                original_sink,
                assignment_confirmed: active.assignment_confirmed,
                assignment_quiesced: false,
                restoration_submitted: false,
                restoration_confirmed: false,
                restoration_quiesced: false,
            }
        }
    };
    if reconcile_route_intent(client, &intent) {
        registry.active = None;
    }
}

fn reconcile_route_intent(client: &mut PulseClient, intent: &RouteIntent) -> bool {
    let deadline = Instant::now() + ROUTE_RESTORE_TIMEOUT;
    let remaining = || deadline.saturating_duration_since(Instant::now());
    let Ok(Some(current_default)) = client.default_sink_with_timeout(remaining()) else {
        return false;
    };
    match route_restore_action(intent, Some(&current_default)) {
        RouteRestoreAction::Wait | RouteRestoreAction::KeepTombstone => false,
        RouteRestoreAction::Release => clear_route_intent(&intent.target_sink).is_ok(),
        RouteRestoreAction::RestoreOriginal => {
            // Re-read immediately before the write. PulseAudio has no compare-
            // and-set route API, so this prevents overwriting any user change
            // already visible to us at restoration time.
            let Ok(Some(current_default)) = client.default_sink_with_timeout(remaining()) else {
                return false;
            };
            match route_restore_action(intent, Some(&current_default)) {
                RouteRestoreAction::Release => clear_route_intent(&intent.target_sink).is_ok(),
                RouteRestoreAction::KeepTombstone | RouteRestoreAction::Wait => false,
                RouteRestoreAction::RestoreOriginal => {
                    if mark_route_restoration_submitted(&intent.target_sink).is_err() {
                        return false;
                    }
                    if let Err(code) =
                        client.set_default_sink_with_timeout(&intent.original_sink, remaining())
                    {
                        eprintln!("Amaterasu audio route restore was not acknowledged: {code}");
                        if client.server_info_with_timeout(remaining()).is_ok() {
                            let _ = mark_route_restoration_quiesced(&intent.target_sink);
                        }
                        return false;
                    }
                    if mark_route_restoration_confirmed(&intent.target_sink).is_err() {
                        return false;
                    }
                    if wait_for_route_readback(
                        &intent.original_sink,
                        &intent.target_sink,
                        remaining().min(Duration::from_millis(250)),
                        |timeout| client.default_sink_with_timeout(timeout),
                    ) {
                        clear_route_intent(&intent.target_sink).is_ok()
                    } else {
                        eprintln!(
                            "Amaterasu audio route restore acknowledged; waiting for readback"
                        );
                        false
                    }
                }
            }
        }
    }
}

fn reconcile_persisted_route(client: &mut PulseClient) -> bool {
    let Ok(state) = read_cleanup_state() else {
        return false;
    };
    let Some(intent) = state.route else {
        return true;
    };
    reconcile_route_intent(client, &intent)
}

fn replacement_route_original(
    pending: Option<&RouteIntent>,
    current_default: Option<&str>,
) -> Option<String> {
    // Carry the real original forward when failed restoration left the old
    // virtual sink selected. The next stream must not restore to that sink.
    match pending {
        Some(intent)
            if intent.assignment_confirmed
                && current_default == Some(intent.target_sink.as_str()) =>
        {
            Some(intent.original_sink.clone())
        }
        _ => current_default.map(str::to_owned),
    }
}

fn resolve_virtual<'a>(sinks: &'a [Sink], selector: &str) -> Option<&'a Sink> {
    let endpoints = sinks
        .iter()
        .map(|sink| sink.endpoint.clone())
        .collect::<Vec<_>>();
    let selected = select_endpoint(&endpoints, selector)?;
    sinks.iter().find(|sink| sink.endpoint.id == selected.id)
}

fn resolve_regular<'a>(
    sinks: &'a [Sink],
    requested: &str,
    current_default: Option<&str>,
) -> Option<&'a Sink> {
    let endpoint = if requested.is_empty() || requested == "default" {
        current_default?
    } else {
        requested
    };
    sinks.iter().find(|sink| sink.endpoint.id == endpoint)
}

fn endpoint_reply(sinks: &mut [Sink], default_sink: Option<&str>) -> Vec<Endpoint> {
    for sink in sinks.iter_mut() {
        sink.endpoint.default_multimedia = default_sink == Some(sink.endpoint.id.as_str());
    }
    sinks.iter().map(|sink| sink.endpoint.clone()).collect()
}

pub fn enumerate() -> AudioResult<Vec<Endpoint>> {
    let mut client = PulseClient::connect()?;
    let default_sink = client.default_sink()?;
    let mut sinks = client.sinks()?;
    Ok(endpoint_reply(&mut sinks, default_sink.as_deref()))
}

pub fn route_restore_pending() -> bool {
    let registry = route_registry();
    let mut registry = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if registry
        .active
        .as_ref()
        .is_some_and(|active| active.references > 0)
    {
        return false;
    }
    let persisted_route_pending = read_cleanup_state().is_ok_and(|state| state.route.is_some());
    if !registry.restore_pending() && !persisted_route_pending {
        return false;
    }
    if let Ok(mut client) = PulseClient::connect_with_timeout(ROUTE_RESTORE_TIMEOUT) {
        if registry.restore_pending() {
            retry_route_restore(&mut client, &mut registry);
        } else {
            let _ = reconcile_persisted_route(&mut client);
        }
    }
    registry.restore_pending() || read_cleanup_state().is_ok_and(|state| state.route.is_some())
}

pub struct Capture {
    stream: PulseStream,
    client: PulseClient,
    endpoint: String,
    format: Format,
    pending: VecDeque<u8>,
    next_sequence: u64,
    started_at: Instant,
    follow_console_default: bool,
    next_default_check: Instant,
    recovery_config: RecoveryConfig,
    _route_lease: Option<Arc<RouteLease>>,
    _virtual_sink_lease: Option<Arc<ManagedVirtualSink>>,
}

impl Capture {
    pub fn open_with_bounded_retry(
        requested: &str,
        virtual_sink: Option<&str>,
        channels: Option<u32>,
        capture_format: Option<PcmCaptureFormat>,
        host_audio_enabled: bool,
        capture_sink_without_routing: bool,
        stop: &AtomicBool,
    ) -> AudioResult<Self> {
        let side_effect_free = startup_open_is_side_effect_free(
            requested,
            virtual_sink,
            host_audio_enabled,
            capture_sink_without_routing,
        );
        if stop.load(Ordering::Acquire) {
            return Err("audio_worker_stopped");
        }
        if !side_effect_free {
            return Self::open(
                requested,
                virtual_sink,
                channels,
                capture_format,
                host_audio_enabled,
                capture_sink_without_routing,
            );
        }
        let startup_deadline = Instant::now() + STARTUP_OPEN_TOTAL_TIMEOUT;
        for attempt in 1..=STARTUP_OPEN_MAX_ATTEMPTS {
            if stop.load(Ordering::Acquire) {
                return Err("audio_worker_stopped");
            }
            let attempt_deadline =
                (Instant::now() + STARTUP_OPEN_ATTEMPT_TIMEOUT).min(startup_deadline);
            match Self::open_with_deadline(
                requested,
                virtual_sink,
                channels,
                capture_format,
                host_audio_enabled,
                capture_sink_without_routing,
                Some(attempt_deadline),
            ) {
                Ok(capture) => return Ok(capture),
                Err(error) if should_retry_startup_open(error, attempt, side_effect_free) => {
                    if stop.load(Ordering::Acquire) {
                        return Err("audio_worker_stopped");
                    }
                    if !wait_for_startup_open_retry(stop, startup_deadline) {
                        if stop.load(Ordering::Acquire) {
                            return Err("audio_worker_stopped");
                        }
                        return Err(error);
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err("audio_backend_failed")
    }

    pub fn open(
        requested: &str,
        virtual_sink: Option<&str>,
        channels: Option<u32>,
        capture_format: Option<PcmCaptureFormat>,
        host_audio_enabled: bool,
        capture_sink_without_routing: bool,
    ) -> AudioResult<Self> {
        Self::open_with_deadline(
            requested,
            virtual_sink,
            channels,
            capture_format,
            host_audio_enabled,
            capture_sink_without_routing,
            None,
        )
    }

    fn open_with_deadline(
        requested: &str,
        virtual_sink: Option<&str>,
        channels: Option<u32>,
        capture_format: Option<PcmCaptureFormat>,
        host_audio_enabled: bool,
        capture_sink_without_routing: bool,
        deadline: Option<Instant>,
    ) -> AudioResult<Self> {
        if requested.len() > 4096 || requested.chars().any(char::is_control) {
            return Err("invalid_endpoint");
        }
        let mut client = PulseClient::connect_with_timeout(startup_operation_timeout(
            deadline.unwrap_or_else(|| Instant::now() + CONTROL_TIMEOUT),
            Instant::now(),
        )?)?;
        let default_sink = client.default_sink_with_timeout(startup_operation_timeout(
            deadline.unwrap_or_else(|| Instant::now() + CONTROL_TIMEOUT),
            Instant::now(),
        )?)?;
        let mut sinks = client.sinks_with_timeout(startup_operation_timeout(
            deadline.unwrap_or_else(|| Instant::now() + CONTROL_TIMEOUT),
            Instant::now(),
        )?)?;
        let mut managed_sink = None;
        let (selected, is_virtual, virtual_channels, effective_capture_only) =
            if let Some(selector) = virtual_sink {
                let layout_channels = channels.unwrap_or(2);
                virtual_sink::channel_layout(layout_channels)?;
                let selected = resolve_virtual(&sinks, selector).ok_or("endpoint_not_found")?;
                (selected.clone(), true, Some(layout_channels), false)
            } else if !host_audio_enabled {
                let layout_channels = channels.unwrap_or(2);
                virtual_sink::channel_layout(layout_channels)?;
                let lease = managed_virtual_sink(&mut client, layout_channels)?;
                lease.ensure(&mut client)?;
                sinks = client.sinks()?;
                let selected = sinks
                    .iter()
                    .find(|sink| sink.endpoint.id == lease.name)
                    .cloned()
                    .ok_or("virtual_sink_unavailable")?;
                managed_sink = Some(lease);
                (selected, true, Some(layout_channels), false)
            } else {
                if channels.is_some() {
                    return Err("invalid_channel_layout");
                }
                let selected = resolve_regular(&sinks, requested, default_sink.as_deref())
                    .ok_or("endpoint_not_found")?
                    .clone();
                (selected, false, None, capture_sink_without_routing)
            };

        let (channel_positions, channels) = capture_channel_layout(
            &selected.channel_positions,
            virtual_channels,
            capture_format,
        )?;
        let mut format = Format::new(AUDIO_RATE, channels, PCM_F32)?;
        format.channel_mask = channel_mask(&channel_positions);

        let follows_default = virtual_sink.is_none()
            && host_audio_enabled
            && !effective_capture_only
            && (requested.is_empty() || requested == "default");
        let explicit_route = is_virtual || !(requested.is_empty() || requested == "default");
        let route_lease = acquire_route(
            &mut client,
            &selected.endpoint.id,
            default_sink.as_deref(),
            effective_capture_only,
            explicit_route,
            is_virtual,
        )?;

        open_selected_with_timeout(
            CaptureSetup {
                client,
                selected,
                format,
                channel_positions,
                virtual_channels,
                capture_format,
                follow_console_default: follows_default,
                capture_sink_without_routing: effective_capture_only,
                route_lease,
                virtual_sink_lease: managed_sink,
            },
            startup_operation_timeout(
                deadline.unwrap_or_else(|| Instant::now() + CONTROL_TIMEOUT),
                Instant::now(),
            )?,
        )
    }

    pub fn reopen(config: &RecoveryConfig) -> AudioResult<Self> {
        let mut client = PulseClient::connect()?;
        let mut sinks = client.sinks()?;
        if let Some(lease) = &config.virtual_sink_lease {
            lease.ensure(&mut client)?;
            sinks = client.sinks()?;
        }
        let endpoint = if config.follow_console_default {
            client.default_sink()?.ok_or("endpoint_not_found")?
        } else {
            config.endpoint.clone()
        };
        let selected = sinks
            .into_iter()
            .find(|sink| sink.endpoint.id == endpoint)
            .ok_or("endpoint_not_found")?;
        let channel_count =
            u32::try_from(config.channel_positions.len()).map_err(|_| "unsupported_format")?;
        let mut format = Format::new(AUDIO_RATE, channel_count, PCM_F32)?;
        format.channel_mask = channel_mask(&config.channel_positions);
        open_selected(CaptureSetup {
            client,
            selected,
            format,
            channel_positions: config.channel_positions.clone(),
            virtual_channels: config.virtual_channels,
            capture_format: config.capture_format,
            follow_console_default: config.follow_console_default,
            capture_sink_without_routing: config.capture_sink_without_routing,
            route_lease: config.route_lease.clone(),
            virtual_sink_lease: config.virtual_sink_lease.clone(),
        })
    }

    pub fn recovery_config(&self) -> RecoveryConfig {
        self.recovery_config.clone()
    }

    pub fn format(&self) -> Format {
        self.format
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn read(&mut self) -> AudioResult<Option<Packet>> {
        self.client.iterate()?;
        // SAFETY: the capture stream remains live and belongs to this client.
        if matches!(
            unsafe { pa_stream_get_state(self.stream.0) },
            PA_STREAM_FAILED | PA_STREAM_TERMINATED
        ) {
            return Err("device_lost");
        }
        let mut query_start_error = None;
        if self.follow_console_default && Instant::now() >= self.next_default_check {
            self.next_default_check = Instant::now() + DEFAULT_CHECK_INTERVAL;
            query_start_error = self.client.start_default_sink_query().err();
        }
        if self.follow_console_default {
            let decision = if let Some(error) = query_start_error {
                default_route_decision(
                    Some(Err(error)),
                    &self.endpoint,
                    self.client.default_queries_disabled,
                )
            } else {
                match self.client.poll_default_sink_query() {
                    QueryPoll::Pending => default_route_decision(
                        None,
                        &self.endpoint,
                        self.client.default_queries_disabled,
                    ),
                    QueryPoll::Complete(result) => default_route_decision(
                        Some(result),
                        &self.endpoint,
                        self.client.default_queries_disabled,
                    ),
                }
            };
            match decision {
                DefaultRouteDecision::Changed => return Err("default_endpoint_changed"),
                // Reopen the same follow-default capture through its existing
                // recovery path after the bounded probe budget is exhausted.
                // That drops this client and its retained callbacks, then
                // resolves the current default without mutating the route.
                DefaultRouteDecision::Reopen => return Err("default_endpoint_changed"),
                DefaultRouteDecision::Retry => {
                    // A failed control-plane probe does not establish that the
                    // monitor stream failed. Keep capturing and retry later;
                    // Pulse context/stream loss is detected independently above.
                    self.next_default_check = Instant::now() + DEFAULT_CHECK_INTERVAL;
                }
                DefaultRouteDecision::Failed(error) => return Err(error),
                DefaultRouteDecision::Pending | DefaultRouteDecision::Unchanged => {}
            }
        }
        let frame_bytes = self.format.packet_bytes(FRAMES_PER_PACKET)?;
        self.fill_pending(frame_bytes)?;
        if self.pending.len() < frame_bytes {
            return Ok(None);
        }
        let bytes = self.pending.drain(..frame_bytes).collect::<Vec<_>>();
        let mut flags = AUDIO_TIMESTAMP_ESTIMATED;
        if packet_is_silent(&bytes) {
            flags |= AUDIO_SILENT;
        }
        let timestamp_us = self
            .started_at
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or("sequence_exhausted")?;
        Ok(Some(Packet {
            frames: FRAMES_PER_PACKET,
            flags,
            bytes,
            timestamp_us,
            sequence,
        }))
    }

    fn fill_pending(&mut self, frame_bytes: usize) -> AudioResult<()> {
        while self.pending.len() < frame_bytes {
            // SAFETY: readable size is queried for the live stream owned here.
            let readable = unsafe { pa_stream_readable_size(self.stream.0) };
            if readable == usize::MAX {
                return Err("device_lost");
            }
            if readable == 0 {
                return Ok(());
            }
            let mut data = ptr::null();
            let mut length = 0;
            // SAFETY: output pointers are writable; data is borrowed only until
            // pa_stream_drop below, so bytes are copied before dropping it.
            if unsafe { pa_stream_peek(self.stream.0, &mut data, &mut length) } < 0 {
                return Err("device_lost");
            }
            if length == 0 {
                // PulseAudio defines a zero-length peek as an empty buffer and
                // explicitly forbids dropping it. Let the next worker poll
                // retry if readable size raced with the peek.
                return Ok(());
            }
            if length > MAX_CAPTURE_CHUNK {
                // SAFETY: this live stream owns a successful positive-length
                // peek, which must be dropped even when rejecting its size.
                if unsafe { pa_stream_drop(self.stream.0) } < 0 {
                    return Err("device_lost");
                }
                return Err("invalid_audio_buffer");
            }
            if data.is_null() {
                self.pending.resize(self.pending.len() + length, 0);
            } else {
                // SAFETY: libpulse guarantees this buffer remains valid until
                // drop; `length` is bounded above and copied into owned storage.
                let chunk = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) };
                self.pending.extend(chunk.iter().copied());
            }
            // SAFETY: every nonempty successful peek is paired with one drop.
            if unsafe { pa_stream_drop(self.stream.0) } < 0 {
                return Err("device_lost");
            }
        }
        Ok(())
    }
}

struct CaptureSetup {
    client: PulseClient,
    selected: Sink,
    format: Format,
    channel_positions: Vec<c_int>,
    virtual_channels: Option<u32>,
    capture_format: Option<PcmCaptureFormat>,
    follow_console_default: bool,
    capture_sink_without_routing: bool,
    route_lease: Option<Arc<RouteLease>>,
    virtual_sink_lease: Option<Arc<ManagedVirtualSink>>,
}

fn open_selected(setup: CaptureSetup) -> AudioResult<Capture> {
    open_selected_with_timeout(setup, CONTROL_TIMEOUT)
}

fn open_selected_with_timeout(setup: CaptureSetup, timeout: Duration) -> AudioResult<Capture> {
    let CaptureSetup {
        mut client,
        selected,
        format,
        channel_positions,
        virtual_channels,
        capture_format,
        follow_console_default,
        capture_sink_without_routing,
        route_lease,
        virtual_sink_lease,
    } = setup;
    let stream = client.record_stream_with_timeout(
        &selected.monitor_source,
        format.channels,
        &channel_positions,
        timeout,
    )?;
    let now = Instant::now();
    let recovery_config = RecoveryConfig {
        endpoint: selected.endpoint.id.clone(),
        follow_console_default,
        virtual_channels,
        capture_format,
        preferred_valid_bits: None,
        capture_sink_without_routing,
        explicit_route: route_lease.is_some(),
        route_lease: route_lease.clone(),
        virtual_sink_lease: virtual_sink_lease.clone(),
        channel_positions: channel_positions.clone(),
    };
    Ok(Capture {
        stream,
        client,
        endpoint: selected.endpoint.id,
        format,
        pending: VecDeque::new(),
        next_sequence: 0,
        started_at: now,
        follow_console_default,
        next_default_check: now + DEFAULT_CHECK_INTERVAL,
        recovery_config,
        _route_lease: route_lease,
        _virtual_sink_lease: virtual_sink_lease,
    })
}

fn packet_is_silent(bytes: &[u8]) -> bool {
    let (samples, remainder) = bytes.as_chunks::<4>();
    remainder.is_empty()
        && samples
            .iter()
            .all(|sample| f32::from_le_bytes(*sample) == 0.0)
}

struct RestoreWorker {
    stop: Sender<()>,
    thread: JoinHandle<()>,
    owners: usize,
}

static RESTORE_WORKER: OnceLock<Mutex<Option<RestoreWorker>>> = OnceLock::new();

pub fn startup(state_root: &Path) -> AudioResult<()> {
    let workers = RESTORE_WORKER.get_or_init(|| Mutex::new(None));
    let mut workers = workers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(worker) = workers.as_mut() {
        worker.owners = worker.owners.saturating_add(1);
        return Ok(());
    }

    let journal = CleanupJournal::open(state_root)?;
    let mut state = journal.read()?;
    // No capture leases from the previous module instance can survive this
    // process start. Convert its active sink ownership into cleanup work.
    deactivate_prior_process_sinks(&mut state);
    journal.write(&state)?;
    {
        let mut configured = CLEANUP_JOURNAL
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Deferred evidence belongs to the journal that admitted its sink.
        // Persist it before reconfiguration; never move it to another root or
        // discard it because the module is starting again in this process.
        if let Some(previous) = configured.as_mut() {
            previous.retry_pending()?;
        }
        *configured = Some(SinkCleanupCoordinator::new(journal));
    }
    if let Ok(mut client) = PulseClient::connect() {
        let _ = reconcile_persisted_route(&mut client);
        retry_virtual_sink_cleanups_with_client(&mut client, None);
    }
    let (stop, stopped) = mpsc::channel();
    let Ok(worker_thread) = thread::Builder::new()
        .name("amaterasu-audio-route-restore".into())
        .spawn(move || {
            loop {
                match stopped.recv_timeout(Duration::from_secs(1)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let _ = route_restore_pending();
                        retry_virtual_sink_cleanups();
                    }
                }
            }
        })
    else {
        return Err("audio_restore_worker_unavailable");
    };
    *workers = Some(RestoreWorker {
        stop,
        thread: worker_thread,
        owners: 1,
    });
    Ok(())
}

pub fn shutdown() {
    let (worker, final_shutdown) = RESTORE_WORKER.get().map_or((None, false), |workers| {
        let mut workers = workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(worker) = workers.as_mut() else {
            return (None, false);
        };
        if worker.owners > 1 {
            worker.owners -= 1;
            (None, false)
        } else {
            (workers.take(), true)
        }
    });
    if let Some(worker) = worker {
        let _ = worker.stop.send(());
        let _ = worker.thread.join();
    }
    if final_shutdown {
        // One final bounded pass. Failed or indeterminate operations remain in
        // the durable journal for the next worker start.
        if let Ok(mut client) = PulseClient::connect() {
            let _ = reconcile_persisted_route(&mut client);
            retry_virtual_sink_cleanups_with_client(&mut client, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledged_route_can_publish_after_stale_server_info() {
        let mut observations = [None, Some("old"), Some("new")].into_iter();
        assert!(wait_for_route_readback(
            "new",
            "old",
            Duration::from_millis(100),
            |_| Ok(observations
                .next()
                .expect("bounded sequence")
                .map(str::to_owned)),
        ));
        assert!(observations.next().is_none());
    }

    #[test]
    fn route_confirmation_stops_at_a_user_change_or_backend_failure() {
        for observation in [
            Ok(Some("user-selected".to_owned())),
            Err("audio_backend_failed"),
        ] {
            let mut reads = 0;
            assert!(!wait_for_route_readback(
                "new",
                "old",
                Duration::from_millis(100),
                |_| {
                    reads += 1;
                    observation.clone()
                },
            ));
            assert_eq!(reads, 1);
        }
    }

    #[test]
    fn unchanged_route_confirmation_has_a_deadline() {
        let started = Instant::now();
        let mut reads = 0;
        assert!(!wait_for_route_readback(
            "new",
            "old",
            Duration::from_millis(15),
            |budget| {
                assert!(budget <= Duration::from_millis(15));
                reads += 1;
                Ok(Some("old".into()))
            },
        ));
        assert!(reads > 0);
        assert!(started.elapsed() >= Duration::from_millis(15));
    }

    #[test]
    fn failed_operation_wait_keeps_userdata_valid_for_a_late_callback() {
        for failure in ["audio_operation_timeout", "audio_server_unavailable"] {
            let mut callback_state = Box::new(BooleanResult::default());
            let userdata = (&mut *callback_state as *mut BooleanResult).cast();
            let mut pending = PendingCallbackStates::default();

            let outcome = finish_operation_wait(Err(failure), callback_state, &mut pending);
            assert!(outcome.is_err());

            // Model libpulse delivering a callback that was already queued
            // when the operation wait returned. The client-owned box keeps
            // this exact production callback's userdata valid.
            unsafe { success_callback(ptr::null_mut(), 1, userdata) };

            let state = pending.states[0]
                .downcast_ref::<BooleanResult>()
                .expect("the pending callback state retains its concrete type");
            assert!(state.completed);
            assert!(state.succeeded);
        }
    }

    #[test]
    fn completed_operation_wait_returns_callback_state_to_its_caller() {
        let mut pending = PendingCallbackStates::default();
        let callback_state = Box::new(BooleanResult {
            completed: true,
            succeeded: true,
        });

        let result = finish_operation_wait(Ok(()), callback_state, &mut pending)
            .expect("completed operation returns its result state");

        assert!(result.completed);
        assert!(result.succeeded);
        assert!(pending.states.is_empty());
    }

    #[test]
    fn module_inventory_requires_successful_end_of_list_and_preserves_present_indexes() {
        let mut result = ModulePresenceResult {
            target_index: 71,
            present: false,
            completed: false,
            failed: false,
        };
        assert!(
            result.presence().is_err(),
            "partial inventory cannot prove absence"
        );
        let module = PaModuleInfo {
            index: 71,
            name: ptr::null(),
            argument: ptr::null(),
            n_used: 0,
            auto_unload: 0,
            proplist: ptr::null_mut(),
        };
        // SAFETY: callback descriptor and output state are live test allocations.
        unsafe {
            module_info_callback(
                ptr::null_mut(),
                &module,
                0,
                (&mut result as *mut ModulePresenceResult).cast(),
            );
        }
        assert!(result.presence().is_err());
        // A matching index may be a foreign/reused module; either way it forbids
        // declaring absence. The inventory does not authorize unloading it.
        unsafe {
            module_info_callback(
                ptr::null_mut(),
                ptr::null(),
                1,
                (&mut result as *mut ModulePresenceResult).cast(),
            );
        }
        assert_eq!(result.presence(), Ok(true));
        result.present = false;
        result.completed = false;
        unsafe {
            module_info_callback(
                ptr::null_mut(),
                ptr::null(),
                -1,
                (&mut result as *mut ModulePresenceResult).cast(),
            );
        }
        assert!(
            result.presence().is_err(),
            "failed inventory cannot prove absence"
        );
        result.failed = false;
        unsafe {
            module_info_callback(
                ptr::null_mut(),
                ptr::null(),
                1,
                (&mut result as *mut ModulePresenceResult).cast(),
            );
        }
        assert_eq!(result.presence(), Ok(false));
    }

    #[test]
    fn context_connect_wait_accepts_every_documented_transition_state() {
        for state in [
            PA_CONTEXT_UNCONNECTED,
            PA_CONTEXT_CONNECTING,
            PA_CONTEXT_AUTHORIZING,
            PA_CONTEXT_SETTING_NAME,
        ] {
            assert_eq!(context_connection_state(state, 0), None);
        }
        assert_eq!(context_connection_state(PA_CONTEXT_READY, 0), Some(Ok(())));
        assert_eq!(
            context_connection_state(PA_CONTEXT_FAILED, PA_ERR_ACCESS),
            Some(Err("audio_server_access_denied"))
        );
        assert_eq!(
            context_connection_state(PA_CONTEXT_TERMINATED, PA_ERR_TIMEOUT),
            Some(Err("audio_server_timeout"))
        );
        assert_eq!(
            context_connection_state(-1, 0),
            Some(Err("audio_server_unavailable"))
        );
    }

    #[test]
    fn pulse_connection_errors_distinguish_auth_and_invalid_server() {
        assert_eq!(
            context_error_code(PA_ERR_AUTHKEY),
            "audio_server_access_denied"
        );
        assert_eq!(
            context_error_code(PA_ERR_INVALIDSERVER),
            "audio_server_configuration_invalid"
        );
        assert_eq!(context_error_code(0), "audio_server_unavailable");
    }

    #[test]
    fn pulse_virtual_layouts_match_the_advertised_pcm_channel_masks() {
        let stereo = null_sink_layout(2).unwrap();
        let surround51 = null_sink_layout(6).unwrap();
        let surround71 = null_sink_layout(8).unwrap();
        assert_eq!(stereo.positions, &[1, 2]);
        assert_eq!(surround51.positions, &[1, 2, 3, 7, 5, 6]);
        assert_eq!(surround71.positions, &[1, 2, 3, 7, 5, 6, 10, 11]);
        assert_eq!(channel_mask(stereo.positions), 0x0003);
        assert_eq!(channel_mask(surround51.positions), 0x003f);
        assert_eq!(channel_mask(surround71.positions), 0x063f);
        assert_eq!(channel_mask(&[0]), 0x0004);
        assert_eq!(
            null_sink_layout(4).unwrap_err(),
            "unsupported_channel_layout"
        );
    }

    #[test]
    fn ordinary_capture_keeps_endpoint_channels_unless_a_session_format_is_requested() {
        let endpoint_positions = [1, 2, 3, 7, 5, 6];
        assert_eq!(
            capture_channel_layout(&endpoint_positions, None, None).unwrap(),
            (endpoint_positions.to_vec(), 6)
        );

        let requested = PcmCaptureFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: crate::model::PcmSampleFormat::F32,
        };
        assert_eq!(
            capture_channel_layout(&endpoint_positions, None, Some(requested)).unwrap(),
            (STEREO_POSITIONS.to_vec(), 2)
        );
    }

    #[test]
    fn virtual_capture_rejects_a_session_format_with_different_channels() {
        let requested = PcmCaptureFormat {
            sample_rate: 48_000,
            channels: 6,
            sample_format: crate::model::PcmSampleFormat::F32,
        };
        assert_eq!(
            capture_channel_layout(STEREO_POSITIONS, Some(2), Some(requested)),
            Err("invalid_capture_format")
        );
    }

    #[test]
    fn default_route_probe_keeps_capture_live_while_query_is_pending_or_fails() {
        assert_eq!(
            default_route_decision(None, "sink.selected", false),
            DefaultRouteDecision::Pending
        );
        for error in [
            "audio_operation_timeout",
            "audio_server_unavailable",
            "audio_backend_failed",
        ] {
            assert_eq!(
                default_route_decision(Some(Err(error)), "sink.selected", false),
                DefaultRouteDecision::Retry
            );
        }
        assert_eq!(
            default_route_decision(
                Some(Err("audio_server_access_denied")),
                "sink.selected",
                false
            ),
            DefaultRouteDecision::Failed("audio_server_access_denied")
        );
        assert_eq!(
            default_route_decision(
                Some(Ok(Some("sink.selected".into()))),
                "sink.selected",
                false
            ),
            DefaultRouteDecision::Unchanged
        );
    }

    #[test]
    fn stalled_default_query_times_out_and_cancellation_retries_are_bounded() {
        let started_at = Instant::now();
        assert!(!default_query_timed_out(
            started_at,
            started_at + DEFAULT_QUERY_TIMEOUT - Duration::from_nanos(1)
        ));
        assert!(default_query_timed_out(
            started_at,
            started_at + DEFAULT_QUERY_TIMEOUT
        ));

        let mut failures = 0;
        for attempt in 1..=DEFAULT_QUERY_FAILURE_LIMIT {
            assert_eq!(
                default_route_decision(Some(Err("audio_backend_failed")), "sink.selected", false),
                DefaultRouteDecision::Retry
            );
            failures = default_query_failures_after_result(failures, false);
            assert_eq!(failures, attempt);
            assert_eq!(
                default_queries_disabled(failures),
                attempt == DEFAULT_QUERY_FAILURE_LIMIT,
            );
        }

        // A successful probe after transient failures resets the retry budget
        // and allows follow-default monitoring to continue checking the route.
        let failures = default_query_failures_after_result(failures, true);
        assert_eq!(failures, 0);
        assert!(!default_queries_disabled(failures));
        assert_eq!(
            default_route_decision(None, "sink.selected", false),
            DefaultRouteDecision::Pending
        );
        assert_eq!(
            default_route_decision(None, "sink.selected", true),
            DefaultRouteDecision::Reopen
        );
        assert_eq!(
            default_route_decision(Some(Err("audio_backend_failed")), "sink.selected", true),
            DefaultRouteDecision::Reopen
        );
        assert_eq!(
            default_route_decision(
                Some(Err("audio_server_access_denied")),
                "sink.selected",
                true
            ),
            DefaultRouteDecision::Failed("audio_server_access_denied")
        );
    }

    #[test]
    fn pulse_operation_state_mapping_distinguishes_running_done_and_cancelled() {
        assert_eq!(
            pulse_operation_state(PA_OPERATION_RUNNING),
            PulseOperationState::Running
        );
        assert_eq!(
            pulse_operation_state(PA_OPERATION_DONE),
            PulseOperationState::Done
        );
        assert_eq!(
            pulse_operation_state(PA_OPERATION_CANCELLED),
            PulseOperationState::Cancelled
        );
        assert_eq!(pulse_operation_state(-1), PulseOperationState::Unknown);
    }

    #[test]
    fn default_route_probe_only_reopens_when_a_completed_query_observes_a_change() {
        assert_eq!(
            default_route_decision(
                Some(Ok(Some("sink.user_choice".into()))),
                "sink.selected",
                false
            ),
            DefaultRouteDecision::Changed
        );
        assert_eq!(
            default_route_decision(Some(Ok(None)), "sink.selected", false),
            DefaultRouteDecision::Changed
        );
    }

    #[test]
    fn startup_open_retries_only_capture_modes_that_skip_route_and_sink_mutation() {
        assert!(startup_open_is_side_effect_free(
            "default", None, true, false
        ));
        assert!(startup_open_is_side_effect_free(
            "speaker.fixed",
            None,
            true,
            true
        ));
        assert!(!startup_open_is_side_effect_free(
            "speaker.fixed",
            None,
            true,
            false
        ));
        assert!(!startup_open_is_side_effect_free(
            "default", None, false, false
        ));
        assert!(!startup_open_is_side_effect_free(
            "default",
            Some("virtual.sink"),
            true,
            false
        ));

        assert_eq!(
            route_plan("speaker.fixed", Some("other"), false, false, false),
            RoutePlan::Skip
        );
        assert_eq!(
            route_plan("speaker.fixed", Some("other"), true, true, false),
            RoutePlan::Skip
        );
    }

    #[test]
    fn startup_open_retries_only_known_transient_pulse_failures_and_is_bounded() {
        for error in [
            "audio_server_timeout",
            "audio_server_unavailable",
            "audio_operation_timeout",
            "audio_backend_failed",
            "audio_capture_unavailable",
        ] {
            assert!(should_retry_startup_open(error, 1, true), "{error}");
            assert!(!should_retry_startup_open(error, 2, true), "{error}");
            assert!(!should_retry_startup_open(error, 1, false), "{error}");
        }
        for error in [
            "audio_server_access_denied",
            "audio_server_configuration_invalid",
            "endpoint_not_found",
            "invalid_capture_format",
        ] {
            assert!(!should_retry_startup_open(error, 1, true), "{error}");
        }
    }

    #[test]
    fn startup_open_operations_share_a_bounded_deadline() {
        let now = Instant::now();
        assert_eq!(
            startup_operation_timeout(now + Duration::from_secs(2), now),
            Ok(Duration::from_secs(2))
        );
        assert_eq!(
            startup_operation_timeout(now + Duration::from_secs(10), now),
            Ok(CONTROL_TIMEOUT)
        );
        assert_eq!(
            startup_operation_timeout(now, now),
            Err("audio_operation_timeout")
        );
    }

    #[test]
    fn startup_retry_budget_stays_inside_the_resource_open_rpc_timeout() {
        const RESOURCE_OPEN_RPC_TIMEOUT: Duration = Duration::from_secs(5);
        let max_attempts_and_delay =
            STARTUP_OPEN_ATTEMPT_TIMEOUT + STARTUP_OPEN_RETRY_DELAY + STARTUP_OPEN_ATTEMPT_TIMEOUT;

        assert!(max_attempts_and_delay < RESOURCE_OPEN_RPC_TIMEOUT);
        assert!(STARTUP_OPEN_TOTAL_TIMEOUT < RESOURCE_OPEN_RPC_TIMEOUT);
        assert!(max_attempts_and_delay <= STARTUP_OPEN_TOTAL_TIMEOUT);
    }

    #[test]
    fn pulse_capture_uses_sized_float_pcm_and_estimated_silence_flags() {
        let format = Format::new(AUDIO_RATE, 6, PCM_F32).unwrap();
        assert_eq!(format.packet_bytes(FRAMES_PER_PACKET).unwrap(), 480 * 6 * 4);
        assert!(packet_is_silent(&vec![0; 480 * 6 * 4]));
        assert!(!packet_is_silent(&[0, 0, 0, 0, 0, 0, 128, 63]));
    }

    #[test]
    fn explicit_endpoint_and_virtual_selector_resolution_remain_strict() {
        let sink = Sink {
            endpoint: Endpoint {
                id: "render.device".into(),
                name: "Speakers".into(),
                description: Some("Speakers".into()),
                adapter_name: Some("Realtek".into()),
                default_multimedia: false,
            },
            monitor_source: "render.device.monitor".into(),
            channel_positions: STEREO_POSITIONS.to_vec(),
            owner_module: PA_INVALID_INDEX,
            managed_token: None,
        };
        let sinks = [sink];
        assert!(resolve_regular(&sinks, "render.device", None).is_some());
        assert!(resolve_regular(&sinks, "missing", None).is_none());
        assert!(resolve_virtual(&sinks, "missing").is_none());
        assert!(resolve_virtual(&sinks, "").is_none());
    }

    #[test]
    fn sink_list_callback_distinguishes_completion_from_error() {
        let mut completed = SinkListResult::default();
        // SAFETY: each callback receives a live result pointer; null info is the
        // documented end/error marker and the callback does not dereference it.
        unsafe {
            sink_info_callback(
                ptr::null_mut(),
                ptr::null(),
                1,
                (&mut completed as *mut SinkListResult).cast(),
            );
        }
        assert!(completed.completed);
        assert!(!completed.failed);

        let mut failed = SinkListResult::default();
        // SAFETY: same callback boundary as the positive completion case.
        unsafe {
            sink_info_callback(
                ptr::null_mut(),
                ptr::null(),
                -1,
                (&mut failed as *mut SinkListResult).cast(),
            );
        }
        assert!(failed.completed);
        assert!(failed.failed);

        let mut pending = SinkListResult::default();
        // SAFETY: eol=0 with no info must not be mistaken for completion.
        unsafe {
            sink_info_callback(
                ptr::null_mut(),
                ptr::null(),
                0,
                (&mut pending as *mut SinkListResult).cast(),
            );
        }
        assert!(!pending.completed);
        assert!(!pending.failed);
    }

    #[test]
    fn virtual_sink_cleanup_only_targets_its_owned_module() {
        let sink = Sink {
            endpoint: Endpoint {
                id: "amaterasu_stream_42_2ch".into(),
                name: "Amaterasu Audio".into(),
                description: Some("Amaterasu Audio".into()),
                adapter_name: None,
                default_multimedia: false,
            },
            monitor_source: "amaterasu_stream_42_2ch.monitor".into(),
            channel_positions: STEREO_POSITIONS.to_vec(),
            owner_module: 19,
            managed_token: Some("test-owner".into()),
        };

        assert!(owns_virtual_sink_module(
            std::slice::from_ref(&sink),
            "amaterasu_stream_42_2ch",
            19,
        ));
        assert!(!owns_virtual_sink_module(
            std::slice::from_ref(&sink),
            "amaterasu_stream_42_2ch",
            20,
        ));
        assert!(!owns_virtual_sink_module(
            std::slice::from_ref(&sink),
            "a-different-sink",
            19,
        ));
        assert!(!owns_virtual_sink_module(
            &[],
            "amaterasu_stream_42_2ch",
            19
        ));
    }

    #[test]
    fn route_acquisition_policy_skips_capture_only_and_host_default_opens() {
        assert_eq!(
            route_plan("sink", Some("sink"), true, true, false),
            RoutePlan::Skip
        );
        assert_eq!(
            route_plan("sink", Some("other"), false, false, false),
            RoutePlan::Skip
        );
        assert_eq!(
            route_plan("sink", Some("other"), false, true, false),
            RoutePlan::Assign
        );
        assert_eq!(
            route_plan("sink", Some("other"), false, false, true),
            RoutePlan::Assign
        );
        assert_eq!(
            route_plan("sink", Some("sink"), false, false, true),
            RoutePlan::Track
        );
    }

    #[test]
    fn periodic_route_reconciler_does_not_release_a_live_stream_route() {
        let registry = RouteRegistry {
            active: Some(ActiveRoute {
                endpoint: "selected".into(),
                original: Some("original".into()),
                references: 1,
                assignment_confirmed: true,
            }),
        };
        assert!(!registry.restore_pending());
        assert_eq!(registry.active.as_ref().unwrap().references, 1);
    }

    #[test]
    fn failed_restore_does_not_reserve_the_next_stream_route() {
        let mut registry = RouteRegistry::default();
        record_pending_route(&mut registry, "old-stream", Some("headset".into()));
        registry.abandon_idle_restore();
        assert!(registry.active.is_none());

        record_pending_route(&mut registry, "live-stream", Some("headset".into()));
        registry.active.as_mut().unwrap().references = 1;
        registry.abandon_idle_restore();
        assert_eq!(registry.active.as_ref().unwrap().endpoint, "live-stream");
    }

    #[test]
    fn replacement_stream_restores_headset_instead_of_obsolete_virtual_sink() {
        let pending = RouteIntent {
            target_sink: "old-stream".into(),
            original_sink: "headset".into(),
            assignment_confirmed: true,
            restoration_submitted: true,
            ..RouteIntent::default()
        };
        assert_eq!(
            replacement_route_original(Some(&pending), Some("old-stream")).as_deref(),
            Some("headset")
        );
        assert_eq!(
            replacement_route_original(Some(&pending), Some("new-user-choice")).as_deref(),
            Some("new-user-choice")
        );
        assert_eq!(replacement_route_original(Some(&pending), None), None);
    }

    #[test]
    #[ignore = "requires the user Pulse server; temporarily routes audio to private sinks and restores the original default"]
    fn pending_restore_recovers_and_two_successive_streams_capture_and_clean_up() {
        use std::os::unix::fs::PermissionsExt;

        let mut client = PulseClient::connect().expect("user Pulse server is available");
        let original = client.default_sink().unwrap().expect("default sink exists");
        let state_root = std::env::temp_dir().join(format!(
            "amaterasu-audio-recovery-probe-{}",
            std::process::id()
        ));
        std::fs::create_dir(&state_root).unwrap();
        std::fs::set_permissions(&state_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        struct CleanupProbe {
            state_root: std::path::PathBuf,
            original: String,
        }
        impl Drop for CleanupProbe {
            fn drop(&mut self) {
                shutdown();
                if let Ok(mut client) = PulseClient::connect() {
                    let _ = client.set_default_sink(&self.original);
                }
                let _ = std::fs::remove_dir_all(&self.state_root);
            }
        }
        let _cleanup = CleanupProbe {
            state_root: state_root.clone(),
            original: original.clone(),
        };
        let journal = CleanupJournal::open(&state_root).unwrap();
        journal
            .write(&CleanupState {
                route: Some(RouteIntent {
                    target_sink: "amaterasu-obsolete-recovery-probe".into(),
                    original_sink: original.clone(),
                    assignment_confirmed: true,
                    restoration_submitted: true,
                    ..RouteIntent::default()
                }),
                ..CleanupState::default()
            })
            .unwrap();
        startup(&state_root).unwrap();
        assert!(!route_restore_pending(), "uncertain restore is reconciled");
        // An unacknowledged assignment remains unresolved by design. Its
        // cleanup must still allow a subsequent stream to acquire a route.
        journal
            .write(&CleanupState {
                route: Some(RouteIntent {
                    target_sink: "amaterasu-obsolete-recovery-probe".into(),
                    original_sink: original.clone(),
                    ..RouteIntent::default()
                }),
                ..CleanupState::default()
            })
            .unwrap();
        assert!(route_restore_pending());
        record_pending_route(
            &mut route_registry().lock().unwrap(),
            "amaterasu-obsolete-recovery-probe",
            Some(original.clone()),
        );
        for _ in 0..2 {
            let mut capture = Capture::open("", None, None, None, false, false).unwrap();
            let endpoint = capture.endpoint().to_owned();
            assert_ne!(endpoint, original);
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(packet) = capture.read().unwrap() {
                    assert_eq!(packet.frames, FRAMES_PER_PACKET);
                    break;
                }
                assert!(Instant::now() < deadline, "capture produced no PCM");
                thread::sleep(Duration::from_millis(2));
            }
            drop(capture);
            assert!(!route_restore_pending());
            assert_eq!(
                client.default_sink().unwrap().as_deref(),
                Some(original.as_str())
            );
            assert!(
                !client
                    .sinks()
                    .unwrap()
                    .iter()
                    .any(|sink| sink.endpoint.id == endpoint)
            );
            assert!(journal.read().unwrap().sinks.is_empty());
        }
    }

    #[test]
    #[ignore = "requires an accessible per-user Pulse server; creates only a private null sink and never changes the default"]
    fn captures_silence_from_a_private_virtual_monitor() {
        let mut client = PulseClient::connect().expect("user Pulse server is available");
        let name = format!("amaterasu_test_{}_2ch", std::process::id());
        let module_index = client
            .load_null_sink(&name, 2, "ignored-test-owner")
            .expect("test null sink loads");
        let result = (|| {
            let mut capture = Capture::open(&name, None, None, None, true, true)
                .expect("capture-only open binds to the virtual monitor");
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(packet) = capture.read().expect("virtual monitor remains live") {
                    assert_eq!(packet.frames, FRAMES_PER_PACKET);
                    assert_eq!(packet.bytes.len(), 480 * 2 * 4);
                    assert_ne!(packet.flags & AUDIO_TIMESTAMP_ESTIMATED, 0);
                    assert_ne!(packet.flags & AUDIO_SILENT, 0);
                    return Ok::<(), &'static str>(());
                }
                if Instant::now() >= deadline {
                    return Err("virtual_monitor_timeout");
                }
                thread::sleep(Duration::from_millis(2));
            }
        })();
        let _ = client.unload_module(module_index);
        result.expect("monitor produces silent virtual packets");
    }

    #[test]
    #[ignore = "60-second native Pulse worker qualification; creates an owned null sink without changing defaults or routing playback"]
    fn sustains_private_monitor_worker_for_one_minute() {
        use crate::worker::{Source, Worker};
        let mut client = PulseClient::connect().expect("user Pulse server is available");
        let name = format!("amaterasu_qual_{}", std::process::id());
        let module_index = client
            .load_null_sink(&name, 2, "sustained-test-owner")
            .expect("owned null sink loads");
        // Unwind inside the scope so even assertion failures unload our sink.
        let result = std::panic::catch_unwind(|| {
            let (mut worker, endpoint, format) =
                Worker::open(name.clone(), None, None, None, true, true)
                    .expect("worker binds only the private monitor");
            assert_eq!(endpoint, name);
            assert_eq!(format.sample_rate, 48_000);
            let started = Instant::now();
            let mut packets = 0;
            let mut last_timestamp = None;
            let mut next_pause = Duration::from_secs(1);
            while started.elapsed() < Duration::from_secs(60) {
                for _ in 0..32 {
                    let Some(packet) = worker.read().expect("native PCM worker remains healthy")
                    else {
                        break;
                    };
                    assert_eq!(packet.sequence, packets);
                    if let Some(previous) = last_timestamp {
                        assert!(packet.timestamp_us >= previous);
                    }
                    last_timestamp = Some(packet.timestamp_us);
                    packets += 1;
                }
                assert_eq!(worker.health().failure, None);
                if started.elapsed() >= next_pause {
                    // Model ordinary downstream scheduling delay, then catch up.
                    thread::sleep(Duration::from_millis(100));
                    next_pause += Duration::from_secs(1);
                } else {
                    thread::sleep(Duration::from_millis(20));
                }
            }
            assert!(packets >= 4_000, "insufficient PCM progress: {packets}");
            assert_eq!(worker.health().dropped_packets, 0);
        });
        client
            .unload_module(module_index)
            .expect("owned null sink unloads");
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod abi_size_tests {
    use super::*;

    #[test]
    fn pulse_wire_structs_match_x86_64_header_layout() {
        assert_eq!(size_of::<PaModuleInfo>(), 40);
        assert_eq!(std::mem::offset_of!(PaModuleInfo, proplist), 32);
        assert_eq!(size_of::<PaSampleSpec>(), 12);
        assert_eq!(size_of::<PaChannelMap>(), 132);
        assert_eq!(size_of::<PaCVolume>(), 132);
        assert_eq!(size_of::<PaBufferAttr>(), 20);
        assert_eq!(std::mem::offset_of!(PaSinkInfo, monitor_source_name), 312);
        assert_eq!(std::mem::offset_of!(PaServerInfo, default_sink_name), 48);
    }
}
