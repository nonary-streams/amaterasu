//! Safe module-facing API. ABI ownership and panic containment live in `exports`.
pub mod application_library;
pub mod audio;
pub mod capture;
pub mod control_error;
pub mod data_plane;
pub mod diagnostics;
pub mod display;
pub mod display_profile;
pub mod display_settings;
pub mod encode;
pub mod events;
pub mod exports;
pub mod foreground_context;
pub mod frame_processing;
pub mod hooks;
pub mod input;
pub mod launch_options;
pub mod lifecycle_ledger;
pub mod media;
pub mod media_profile;
mod native;
pub mod network;
#[cfg(feature = "network-http")]
pub mod network_http;
pub mod operation_api;
pub mod pairing_authority;
#[cfg(feature = "process")]
pub mod process;
pub mod processing;
pub mod processing_composition;
pub mod processing_provider;
pub mod protocol_handler;
pub mod requests;
pub mod retained_options;
pub mod retirement;
pub mod service_worker;
pub mod session_presentation;
pub mod threading;
pub mod trace;
pub mod transport;
pub mod video;
mod work;
pub use work::WorkNotifier;
pub mod video_framing;
pub mod virtual_display;
pub use native::NativeBinding;

pub use amaterasu_abi as abi;
pub use amaterasu_contracts::encoder::{
    DynamicRange, EncoderBackend, EncoderIdentity, EncoderSelection, NvencSettings,
    NvencSettingsOverrides, NvencSplitEncode, NvencTwoPass, SOFTWARE_ENCODER_ID,
    SoftwareEncoderOptions, VideoCodec, VideoStreamFormat, validate_min_threads,
    validate_software_options,
};
pub use amaterasu_contracts::{
    ApiFieldKind, ApiMethod, ApiPrivilege, ApiSchema, Command, DIRECT_UNAVAILABLE,
    HOST_PACKAGE_BARRIER_SERVICE, HostLifecycleAction, MAX_BROKER_RESULT_BYTES, REFUSED_PREFIX,
    Response, RunningModuleIdentity, RuntimeHostContext,
};
pub use amaterasu_extension_api as extension_api;
pub use amaterasu_media as media_abi;
pub use control_error::{BrokerError, ControlError, ControlResult, ErrorCode, ErrorOrigin};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

const LOG_EMITTER_CAPACITY: usize = 128;
const LOG_FLUSH_BATCH: usize = 16;
const LOG_TARGET_LIMIT: usize = 128;
const LOG_MESSAGE_LIMIT: usize = 7_500;

pub type ModuleResult<T> = Result<T, String>;

/// Generic wire severity accepted by the host log transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LogLevel {
    Verbose = 0,
    Debug = 1,
    Info = 2,
    Warning = 3,
    Error = 4,
    Fatal = 5,
}

pub struct RuntimeClient {
    endpoint: abi::HostApi,
}

impl RuntimeClient {
    /// # Safety
    /// The endpoint's runtime must outlive this client and all its calls.
    /// Its invoke callback and context must support concurrent calls from any
    /// consumer worker thread. Native interface guards may release a binding on
    /// the last owning worker; endpoint identity and authorization remain bound
    /// to this table, never to the calling OS thread.
    pub unsafe fn from_endpoint(endpoint: abi::HostApi) -> Self {
        Self { endpoint }
    }

    pub fn send(&self, command: Command) -> ModuleResult<Response> {
        if matches!(
            command,
            Command::NativeRegister { .. }
                | Command::NativeBind { .. }
                | Command::NativeRegisterTyped { .. }
                | Command::NativeBindTyped { .. }
                | Command::NativeRelease { .. }
        ) {
            return Err("native control operations require the unsafe native SDK API".into());
        }
        self.send_native(command)
    }

    fn send_native(&self, command: Command) -> ModuleResult<Response> {
        let input = serde_json::to_vec(&command).map_err(|error| error.to_string())?;

        // SAFETY: the host keeps the runtime alive until every module is destroyed.
        // The ABI copies reply bytes while the provider's allocation is still valid.
        let output = unsafe { self.endpoint.call(&input) }
            .map_err(|status| format!("runtime transport failed with status {status}"))?;
        serde_json::from_slice(&output).map_err(|error| error.to_string())
    }

    /// Send a command and separate the three ways it can go wrong: a lost
    /// exchange, a broker refusal, and (left to the caller) an unexpected reply.
    /// Prefer the typed helpers in [`requests`] where one fits.
    pub fn send_checked(&self, command: Command) -> ControlResult<Response> {
        control_error::checked(self.send(command))
    }

    /// Decode the startup settings trusted composition selected for this
    /// module. A module without configured settings receives an empty object,
    /// so give every field of `T` a default.
    pub fn settings<T: serde::de::DeserializeOwned>(&self) -> ControlResult<T> {
        match self.send_checked(Command::GetSettings {})? {
            Response::Result { payload } => {
                serde_json::from_value(payload).map_err(ControlError::payload)
            }
            response => Err(ControlError::unexpected(&response)),
        }
    }

    pub fn require_success(&self, command: Command) -> ModuleResult<()> {
        match self.send(command)? {
            Response::Ok => Ok(()),
            Response::Error { code, message } => Err(format!("{code}: {message}")),
            unexpected => Err(format!("unexpected runtime response: {unexpected:?}")),
        }
    }

    /// Emits a bounded product log record through the host's trusted transport.
    /// Messages must omit credentials, secret values, and request payloads.
    pub fn log(&self, level: LogLevel, target: &str, message: &str) -> ModuleResult<()> {
        validate_log_record(target, message)?;
        self.require_success(Command::Log {
            level: level as u8,
            target: target.to_owned(),
            message: message.to_owned(),
        })
    }

    /// Forwards an accepted persisted logging revision to the trusted host.
    /// Runtime authorization permits this update only for Diagnostics.
    pub fn apply_logging_policy(
        &self,
        revision: u64,
        minimum_level: u8,
        log_path: &str,
    ) -> ModuleResult<()> {
        self.require_success(Command::LoggingPolicyChanged {
            revision,
            minimum_level,
            log_path: log_path.to_owned(),
        })
    }

    /// Wake a waiting host after publishing a frame or completing asynchronous
    /// work. Unsupported older runtimes retain their normal polling behavior.
    pub fn work_notifier(&self) -> WorkNotifier {
        WorkNotifier::new(self.endpoint)
    }

    /// Creates a bounded nonblocking logger for native workers and its module-
    /// thread drain. Host transport work never runs in the callback.
    pub fn log_emitter(&self) -> (LogEmitter, LogQueue) {
        let (sender, receiver) = mpsc::sync_channel(LOG_EMITTER_CAPACITY);
        let stats = Arc::new(LogCounters::default());
        (
            LogEmitter {
                sender,
                stats: stats.clone(),
            },
            LogQueue { receiver, stats },
        )
    }

    /// Requests one fixed host lifecycle action. The host honors it only when
    /// this module's verified effective call grants include the lifecycle service.
    pub fn request_host_lifecycle(&self, action: HostLifecycleAction) -> ModuleResult<()> {
        self.require_success(Command::HostLifecycle { action })
    }

    /// Returns the state location chosen by the trusted host for this worker.
    /// The runtime exposes it to modules with the effective
    /// `amaterasu.host-state-context/1` call grant, as well as the legacy state
    /// module and Linux audio cleanup. This grant does not expose package
    /// metadata. Callers cannot substitute a setting or package-supplied path.
    pub fn runtime_state_root(&self) -> ModuleResult<std::path::PathBuf> {
        let Response::Result { payload } = self.send(Command::GetHostContext {})? else {
            return Err("host context request did not return a result".into());
        };
        let context: RuntimeHostContext = serde_json::from_value(payload)
            .map_err(|error| format!("invalid host context: {error}"))?;
        if context.schema != 1 {
            return Err("host context schema is incompatible".into());
        }
        let root = context
            .state_root
            .filter(|root| !root.is_empty())
            .ok_or("host did not provide a runtime state root")?;
        Ok(root.into())
    }

    /// Calls the provider of `service` synchronously on this thread, with the
    /// same `call` grant an ordinary request needs.
    ///
    /// `Unavailable` proves the provider was not entered: the runtime or the
    /// provider does not support direct calls, this is not the provider's
    /// thread, or the provider declined this operation. Submit an ordinary
    /// request instead. An error means the provider was entered and its
    /// outcome is unknown, exactly like a failed request.
    pub fn call_direct(
        &self,
        service: &str,
        payload: serde_json::Value,
    ) -> ModuleResult<DirectReply> {
        self.direct_call(service, payload, false)
    }

    /// Pass the direct call this layer is serving to the next hop beneath it,
    /// which is shown that call's caller. Use it from
    /// [`ManagedModule::serve_direct`] in a module that
    /// [wraps](Self::wrap) `service`; the result is what to return.
    ///
    /// The three outcomes stay apart. `Declined` proves the next hop was not
    /// entered, so the consumer's ordinary request is safe. `Failed` means it
    /// was entered and its outcome is unknown, and the consumer is told the
    /// same instead of being invited to repeat the call.
    pub fn forward_direct(&self, service: &str, payload: serde_json::Value) -> DirectService {
        match self.direct_call(service, payload, true) {
            Ok(DirectReply::Completed(reply)) => DirectService::Replied(reply),
            Ok(DirectReply::Unavailable { .. }) => DirectService::Declined,
            Err(_) => DirectService::Failed,
        }
    }

    fn direct_call(
        &self,
        service: &str,
        payload: serde_json::Value,
        forward: bool,
    ) -> ModuleResult<DirectReply> {
        match self.send(Command::CallDirect {
            service: service.to_owned(),
            payload,
            forward,
        })? {
            Response::Result { payload } => Ok(DirectReply::Completed(payload)),
            Response::Error { code, .. }
                if matches!(
                    code.as_str(),
                    DIRECT_UNAVAILABLE
                        | "invalid_request"
                        | "permission_denied"
                        | "unavailable"
                        | "runtime_endpoint_required"
                ) =>
            {
                Ok(DirectReply::Unavailable { code })
            }
            Response::Error { code, message } => Err(format!("{code}: {message}")),
            unexpected => Err(format!("unexpected runtime response: {unexpected:?}")),
        }
    }

    /// For internal calls with unique operation IDs that will never be retried
    /// after completion. Consumes terminal request tracking after copying a result.
    /// Ordinary `send` retains the runtime's five-minute retry behavior.
    pub fn send_ephemeral(&self, command: Command) -> ModuleResult<Response> {
        let result_request = match &command {
            Command::TakeResult { request } => Some(*request),
            _ => None,
        };
        let response = self.send(command)?;
        // A serving principal's refusal is as final as a cancellation.
        let terminal = matches!(&response, Response::Result { .. })
            || matches!(&response, Response::Error { code, .. }
                if code == "cancelled"
                    || code == "deadline_exceeded"
                    || code.starts_with(REFUSED_PREFIX));
        if terminal && let Some(request) = result_request {
            self.require_success(Command::ReleaseResult { request })?;
        }
        Ok(response)
    }
}

/// Thread-safe, bounded producer for logs from module-owned native workers.
#[derive(Clone)]
pub struct LogEmitter {
    sender: SyncSender<PendingLog>,
    stats: Arc<LogCounters>,
}

/// A snapshot of local queue outcomes. Acceptance does not guarantee persistence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LogQueueStats {
    pub queue_full: u64,
    pub queue_disconnected: u64,
    pub host_accepted: u64,
    pub host_rejected: u64,
}

#[derive(Default)]
struct LogCounters {
    queue_full: AtomicU64,
    queue_disconnected: AtomicU64,
    host_accepted: AtomicU64,
    host_rejected: AtomicU64,
}

impl LogCounters {
    fn snapshot(&self) -> LogQueueStats {
        LogQueueStats {
            queue_full: self.queue_full.load(Ordering::Relaxed),
            queue_disconnected: self.queue_disconnected.load(Ordering::Relaxed),
            host_accepted: self.host_accepted.load(Ordering::Relaxed),
            host_rejected: self.host_rejected.load(Ordering::Relaxed),
        }
    }
}

fn count_log(counter: &AtomicU64) {
    let mut previous = counter.load(Ordering::Relaxed);
    loop {
        match counter.compare_exchange_weak(
            previous,
            previous.saturating_add(1),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => previous = actual,
        }
    }
}

impl LogEmitter {
    /// Snapshot shared local loss and host-admission counters.
    pub fn stats(&self) -> LogQueueStats {
        self.stats.snapshot()
    }

    /// Bind a target once for repeated worker log events.
    pub fn logger(&self, target: &str) -> ModuleResult<Logger> {
        validate_log_record(target, "")?;
        Ok(Logger {
            emitter: self.clone(),
            target: target.to_owned(),
        })
    }

    pub fn emit(&self, level: LogLevel, target: &str, message: &str) -> ModuleResult<()> {
        validate_log_record(target, message)?;
        self.sender
            .try_send(PendingLog {
                level,
                target: target.to_owned(),
                message: message.to_owned(),
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    count_log(&self.stats.queue_full);
                    "native log queue is full".into()
                }
                TrySendError::Disconnected(_) => {
                    count_log(&self.stats.queue_disconnected);
                    "native log queue is unavailable".into()
                }
            })
    }
}

/// A target-bound, nonblocking producer. Clone it for module worker threads.
#[derive(Clone)]
pub struct Logger {
    emitter: LogEmitter,
    target: String,
}

impl Logger {
    pub fn emit(&self, level: LogLevel, message: &str) -> ModuleResult<()> {
        self.emitter.emit(level, &self.target, message)
    }

    pub fn stats(&self) -> LogQueueStats {
        self.emitter.stats()
    }
}

fn validate_log_record(target: &str, message: &str) -> ModuleResult<()> {
    if target.is_empty()
        || target.len() > LOG_TARGET_LIMIT
        || target.chars().any(char::is_control)
        || message.len() > LOG_MESSAGE_LIMIT
        || message.contains('\0')
    {
        return Err("log record is invalid".into());
    }
    Ok(())
}

struct PendingLog {
    level: LogLevel,
    target: String,
    message: String,
}

/// Module-thread consumer paired with `LogEmitter`; it sends a bounded number
/// of events per tick through the generic runtime transport.
pub struct LogQueue {
    receiver: Receiver<PendingLog>,
    stats: Arc<LogCounters>,
}

impl LogQueue {
    pub fn flush(&mut self, runtime: &RuntimeClient) {
        self.flush_with(|record| runtime.log(record.level, &record.target, &record.message));
    }

    /// Snapshot shared local loss and host-admission counters.
    pub fn stats(&self) -> LogQueueStats {
        self.stats.snapshot()
    }

    fn flush_with(&mut self, mut send: impl FnMut(PendingLog) -> ModuleResult<()>) {
        for _ in 0..LOG_FLUSH_BATCH {
            match self.receiver.try_recv() {
                Ok(record) => {
                    if send(record).is_ok() {
                        count_log(&self.stats.host_accepted);
                    } else {
                        count_log(&self.stats.host_rejected);
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }
}

/// Outcome of [`RuntimeClient::call_direct`].
#[derive(Clone, Debug, PartialEq)]
pub enum DirectReply {
    /// The provider handled the call; this is its reply payload.
    Completed(serde_json::Value),
    /// The provider was not entered. `code` says why, for diagnostics only.
    Unavailable { code: String },
}

/// What a module did with a direct call it was offered.
#[derive(Clone, Debug, PartialEq)]
pub enum DirectService {
    /// The reply, which follows the service's existing wire contract.
    Replied(serde_json::Value),
    /// Declined before any effect. The caller submits an ordinary request.
    Declined,
    /// The call was taken and its outcome is unknown. The caller is told the
    /// call failed and must not assume that nothing happened.
    Failed,
}

/// One synchronous request delivered to [`ManagedModule::call_direct`].
pub struct DirectCall<'a> {
    /// The runtime-authenticated principal of the calling module.
    pub caller: &'a str,
    /// The broker service the caller addressed, which this module provides.
    pub service: &'a str,
    pub payload: serde_json::Value,
}

pub trait ManagedModule: Sized {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self>;
    fn tick(&mut self) -> ModuleResult<()>;

    /// Serve one request of a broker service this module provides, on the
    /// caller's thread and before returning. Use it for short operations where
    /// a queued request's round trip is the cost, such as delivering input.
    ///
    /// Return `None` to decline: the caller then submits an ordinary request,
    /// so declining must leave no effect behind. A reply follows the service's
    /// existing wire contract. The call must be bounded and must not wait for
    /// another module; anything that needs a later tick belongs to the
    /// request path. It is never invoked while another call into this module
    /// is active, nor from a thread other than the one that ticks it.
    fn call_direct(&mut self, _call: DirectCall<'_>) -> Option<serde_json::Value> {
        None
    }

    /// Serve a direct call with all three outcomes. Implement this instead of
    /// [`call_direct`](Self::call_direct) when the module can take a call and
    /// then not know how it ended, as a layer that passes the call on with
    /// [`RuntimeClient::forward_direct`] can. The rules of `call_direct`
    /// apply. The default serves `call_direct`.
    fn serve_direct(&mut self, call: DirectCall<'_>) -> DirectService {
        match self.call_direct(call) {
            Some(reply) => DirectService::Replied(reply),
            None => DirectService::Declined,
        }
    }

    /// Announce host shutdown once, before any consumer begins quiescing.
    /// Record the remaining host budget and stop admitting external work only.
    /// Keep serving declared consumers and retain owned resources until quiesce;
    /// this bounded, nonblocking announcement must not begin provider cleanup.
    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        Ok(())
    }

    /// Begin or poll shutdown while dependencies continue receiving ticks.
    /// The host begins this after declared consumers have drained, and begins
    /// dependency shutdown only after this module reports ready.
    /// Stop admitting new work, release owned effects, then return true. A false
    /// result keeps the instance and all providers alive for another host tick.
    fn quiesce(&mut self) -> ModuleResult<bool> {
        Ok(true)
    }

    fn health(&self) -> ModuleResult<()> {
        Ok(())
    }
}

/// Exports the common lifecycle table; module authors implement ordinary Rust methods.
#[macro_export]
macro_rules! export_module {
    ($module:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn amaterasu_module_v1() -> *const $crate::abi::ModuleApi {
            static API: $crate::abi::ModuleApi = $crate::abi::ModuleApi {
                header: $crate::abi::AbiHeader::of::<$crate::abi::ModuleApi>(),
                create: $crate::exports::create::<$module>,
                tick: $crate::exports::tick::<$module>,
                health: $crate::exports::health::<$module>,
                destroy: $crate::exports::destroy::<$module>,
            };
            &API
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn amaterasu_module_direct_v1() -> *const $crate::abi::ModuleDirectApi {
            static API: $crate::abi::ModuleDirectApi = $crate::abi::ModuleDirectApi {
                header: $crate::abi::AbiHeader::of::<$crate::abi::ModuleDirectApi>(),
                call: $crate::exports::direct::<$module>,
            };
            &API
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn amaterasu_module_shutdown_v1() -> *const $crate::abi::ModuleShutdownApi {
            static API: $crate::abi::ModuleShutdownApi = $crate::abi::ModuleShutdownApi {
                header: $crate::abi::AbiHeader::of::<$crate::abi::ModuleShutdownApi>(),
                prepare: $crate::exports::prepare_shutdown::<$module>,
            };
            &API
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn amaterasu_module_quiesce_v1() -> *const $crate::abi::ModuleQuiesceApi {
            static API: $crate::abi::ModuleQuiesceApi = $crate::abi::ModuleQuiesceApi {
                header: $crate::abi::AbiHeader::of::<$crate::abi::ModuleQuiesceApi>(),
                poll: $crate::exports::quiesce::<$module>,
            };
            &API
        }
    };
}

#[cfg(test)]
mod logging_tests {
    use super::{LogLevel, validate_log_record};

    #[test]
    fn bounded_queue_reports_backpressure_host_errors_and_disconnect() {
        use super::{LOG_EMITTER_CAPACITY, LOG_FLUSH_BATCH, LogCounters, LogEmitter, LogQueue};
        use std::sync::{Arc, mpsc};
        let (sender, receiver) = mpsc::sync_channel(LOG_EMITTER_CAPACITY);
        let stats = Arc::new(LogCounters::default());
        let emitter = LogEmitter {
            sender,
            stats: stats.clone(),
        };
        let mut queue = LogQueue { receiver, stats };
        let logger = emitter.logger("video.timing").unwrap();
        assert!(emitter.logger("bad\ntarget").is_err());
        for _ in 0..LOG_EMITTER_CAPACITY {
            logger.emit(LogLevel::Info, "frame").unwrap();
        }
        assert!(logger.emit(LogLevel::Info, "overflow").is_err());
        assert_eq!(logger.stats().queue_full, 1);
        let mut n = 0;
        queue.flush_with(|record| {
            assert_eq!(record.target, "video.timing");
            n += 1;
            if n == 1 {
                Err("unavailable".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(n, LOG_FLUSH_BATCH);
        assert_eq!(queue.stats().host_rejected, 1);
        assert_eq!(queue.stats().host_accepted, (LOG_FLUSH_BATCH - 1) as u64);
        drop(queue);
        assert!(logger.emit(LogLevel::Warning, "disconnected").is_err());
        assert_eq!(logger.stats().queue_disconnected, 1);
    }

    #[test]
    fn severity_values_match_the_generic_host_transport() {
        assert_eq!(LogLevel::Verbose as u8, 0);
        assert_eq!(LogLevel::Debug as u8, 1);
        assert_eq!(LogLevel::Info as u8, 2);
        assert_eq!(LogLevel::Warning as u8, 3);
        assert_eq!(LogLevel::Error as u8, 4);
        assert_eq!(LogLevel::Fatal as u8, 5);
    }

    #[test]
    fn direct_and_native_log_records_share_transport_bounds() {
        assert!(validate_log_record("module.target", "ready").is_ok());
        assert!(validate_log_record("bad\ntarget", "ready").is_err());
        assert!(validate_log_record("module.target", &"x".repeat(7_501)).is_err());
        assert!(validate_log_record("module.target", "private\0credential").is_err());
    }
}

/// Shared binary artwork provider and consumer adapter.
pub mod artwork_transfer;
