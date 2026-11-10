//! Credential and bearer-token authority for trusted native modules.
mod accounts;
mod crypto;
mod engine;
mod legacy_tokens;
pub mod migration;
mod pipeline;
mod product_settings;
mod protocol;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use pipeline::Pipeline;
use protocol::Settings;

pub const SERVICE: &str = "amaterasu.authentication/1";
const STATE_SERVICE: &str = "amaterasu.state/1";

pub struct AuthenticationModule {
    runtime: RuntimeClient,
    pipeline: Pipeline,
}

impl ManagedModule for AuthenticationModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("authentication settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid authentication settings")?;
        settings.validate().map_err(str::to_owned)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            pipeline: Pipeline::new(settings),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.pipeline
            .tick(|command| self.runtime.send_ephemeral(command))
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        self.pipeline.prepare_shutdown();
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.pipeline
            .quiesce(|command| self.runtime.send_ephemeral(command))
    }
}

amaterasu_module_sdk::export_module!(
    amaterasu_module_sdk::service_worker::ServiceWorker<AuthenticationModule>
);
