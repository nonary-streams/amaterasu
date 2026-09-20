//! Moonlight Gen7 host pairing cryptographic state machine.
mod crypto;
mod engine;
mod identity;
pub mod migration;
mod pipeline;
mod protocol;
mod transcript;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use pipeline::Pipeline;
use protocol::Settings;
pub const SERVICE: &str = "amaterasu.pairing/1";
const STATE_SERVICE: &str = "amaterasu.state/1";

pub struct PairingModule {
    runtime: RuntimeClient,
    pipeline: Pipeline,
    _identity_factory: Box<identity::Factory>,
}
impl ManagedModule for PairingModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("pairing settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid pairing settings")?;
        settings.validate().map_err(str::to_owned)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        let shared = std::sync::Arc::new(identity::Shared::default());
        let factory = identity::Factory::new(shared.clone(), settings.transport_principals.clone());
        // SAFETY: boxed factory context is stable; signed dependency order stops
        // native consumers before pairing destruction. Callbacks share immutable data.
        unsafe { runtime.register_native(amaterasu_identity::SERVICE, &factory.api) }?;
        Ok(Self {
            runtime,
            pipeline: Pipeline::with_identity(settings, shared),
            _identity_factory: factory,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.pipeline
            .tick(|command| self.runtime.send_ephemeral(command))
    }
}
impl Drop for PairingModule {
    fn drop(&mut self) {
        self.pipeline.invalidate_identity();
    }
}

amaterasu_module_sdk::export_module!(PairingModule);
