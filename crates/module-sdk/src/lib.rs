//! Safe module-facing API. ABI ownership and panic containment live in `exports`.
pub mod exports;
mod native;
pub use native::NativeBinding;

pub use amaterasu_abi as abi;
pub use amaterasu_contracts::encoder::{
    DynamicRange, EncoderBackend, EncoderIdentity, EncoderSelection, NvencSettings,
    NvencSettingsOverrides, NvencSplitEncode, NvencTwoPass, SOFTWARE_ENCODER_ID,
    SoftwareEncoderOptions, VideoCodec, VideoStreamFormat, validate_min_threads,
    validate_software_options,
};
pub use amaterasu_contracts::{Command, HostLifecycleAction, Response, RuntimeHostContext};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};

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
    pub unsafe fn from_endpoint(endpoint: abi::HostApi) -> Self {
        Self { endpoint }
    }

    pub fn send(&self, command: Command) -> ModuleResult<Response> {
        if matches!(
            command,
            Command::NativeRegister { .. }
                | Command::NativeBind { .. }
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

    /// Creates a bounded nonblocking logger for native workers and its module-
    /// thread drain. Host transport work never runs in the callback.
    pub fn log_emitter(&self) -> (LogEmitter, LogQueue) {
        let (sender, receiver) = mpsc::sync_channel(LOG_EMITTER_CAPACITY);
        (LogEmitter { sender }, LogQueue { receiver })
    }

    /// Requests one fixed host lifecycle action. The host honors it only when
    /// this module's verified effective call grants include the lifecycle service.
    pub fn request_host_lifecycle(&self, action: HostLifecycleAction) -> ModuleResult<()> {
        self.require_success(Command::HostLifecycle { action })
    }

    /// Returns the state location chosen by the trusted host for this worker.
    /// The control-plane runtime exposes it to the state module and, on Linux,
    /// to audio for its cleanup journal. Callers cannot substitute a JSON
    /// setting or a package-supplied path.
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

    /// For internal calls with unique operation IDs that will never be retried
    /// after completion. Consumes terminal request tracking after copying a result.
    /// Ordinary `send` retains the runtime's five-minute retry behavior.
    pub fn send_ephemeral(&self, command: Command) -> ModuleResult<Response> {
        let result_request = match &command {
            Command::TakeResult { request } => Some(*request),
            _ => None,
        };
        let response = self.send(command)?;
        let terminal = matches!(&response, Response::Result { .. })
            || matches!(&response, Response::Error { code, .. }
                if code == "cancelled" || code == "deadline_exceeded");
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
}

impl LogEmitter {
    pub fn emit(&self, level: LogLevel, target: &str, message: &str) -> ModuleResult<()> {
        validate_log_record(target, message)?;
        self.sender
            .try_send(PendingLog {
                level,
                target: target.to_owned(),
                message: message.to_owned(),
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => "native log queue is full".into(),
                TrySendError::Disconnected(_) => "native log queue is unavailable".into(),
            })
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
}

impl LogQueue {
    pub fn flush(&mut self, runtime: &RuntimeClient) {
        for _ in 0..LOG_FLUSH_BATCH {
            match self.receiver.try_recv() {
                Ok(record) => {
                    let _ = runtime.log(record.level, &record.target, &record.message);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }
}

pub trait ManagedModule: Sized {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self>;
    fn tick(&mut self) -> ModuleResult<()>;

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
    };
}

#[cfg(test)]
mod logging_tests {
    use super::{LogLevel, validate_log_record};

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
