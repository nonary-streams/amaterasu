//! Global host settings, revisioned through the host-owned state provider.
mod path;
mod pipeline;
mod protocol;
mod settings;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use pipeline::Pipeline;

pub const SERVICE: &str = "amaterasu.host-configuration/1";
const STATE_SERVICE: &str = "amaterasu.state/1";

struct HostConfigurationModule {
    runtime: RuntimeClient,
    pipeline: Pipeline,
}

impl ManagedModule for HostConfigurationModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let settings = match runtime.send(Command::GetSettings {})? {
            Response::Result { payload } => payload,
            response => return Err(format!("host configuration settings failed: {response:?}")),
        };
        let trusted_root = path::trusted_root_from_settings(&settings);
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            pipeline: Pipeline::new(trusted_root),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.pipeline.tick(|command| self.runtime.send(command))
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        self.pipeline.prepare_shutdown();
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.pipeline.quiesce(|command| self.runtime.send(command))
    }
}

amaterasu_module_sdk::export_module!(HostConfigurationModule);
