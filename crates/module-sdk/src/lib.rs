//! Safe module-facing API. ABI ownership and panic containment live in `exports`.
pub mod exports;
mod native;
pub use native::NativeBinding;

pub use amaterasu_abi as abi;
pub use amaterasu_contracts::encoder::{
    EncoderBackend, EncoderIdentity, EncoderSelection, SOFTWARE_ENCODER_ID, SoftwareEncoderOptions,
    validate_min_threads, validate_software_options,
};
pub use amaterasu_contracts::{Command, Response, RuntimeHostContext};

pub type ModuleResult<T> = Result<T, String>;

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

    /// Returns the state location chosen by the trusted host for this worker.
    /// The control-plane runtime exposes it only to the state module; callers
    /// cannot substitute a JSON setting or a package-supplied path.
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
