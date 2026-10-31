//! Catalog-backed application launch and session ownership.
mod pipeline;
mod protocol;
pub mod provider_evidence;
mod records;
mod responses;
mod workflow;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use pipeline::Pipeline;
use protocol::Settings;
pub const SERVICE: &str = "amaterasu.launch/1";
pub struct LaunchModule {
    runtime: RuntimeClient,
    pipeline: Pipeline,
}
impl ManagedModule for LaunchModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("launch settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid launch settings")?;
        settings.validate().map_err(str::to_owned)?;
        let mut pipeline = Pipeline::new(settings.environment).map_err(str::to_owned)?;
        pipeline.set_retirement_observers(settings.retirement_observers);
        pipeline.set_provider_routes(settings.provider_routes);
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self { runtime, pipeline })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.pipeline
            .tick(|command| self.runtime.send_ephemeral(command))
    }
}
amaterasu_module_sdk::export_module!(LaunchModule);
