//! Streaming behavior belongs to a managed library, not the native shell.
mod model;
mod pipeline;
mod plane;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use model::Settings;
use pipeline::Pipeline;

const SERVICE: &str = "amaterasu.stream-session/1";
struct StreamSession {
    runtime: RuntimeClient,
    pipeline: Pipeline,
}
impl ManagedModule for StreamSession {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("stream session settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid stream session settings")?;
        settings.validate()?;
        let pipeline = Pipeline::new(settings)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self { runtime, pipeline })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.pipeline.tick(&self.runtime)
    }
}
impl Drop for StreamSession {
    fn drop(&mut self) {
        self.pipeline.shutdown(&self.runtime);
    }
}

amaterasu_module_sdk::export_module!(StreamSession);
