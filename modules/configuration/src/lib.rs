//! Per-caller schemas and values, committed through the state library.
mod migration;
mod pipeline;
mod protocol;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, RuntimeClient};
use pipeline::Pipeline;

const SERVICE: &str = "amaterasu.configuration/1";
const CHANGED_TOPIC: &str = "amaterasu.configuration.changed/1";
const STATE_SERVICE: &str = "amaterasu.state/1";

struct ConfigurationModule {
    runtime: RuntimeClient,
    pipeline: Pipeline,
}

impl ManagedModule for ConfigurationModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            pipeline: Pipeline::default(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.pipeline
            .tick(|command| self.runtime.send_ephemeral(command))
    }
}

amaterasu_module_sdk::export_module!(ConfigurationModule);
