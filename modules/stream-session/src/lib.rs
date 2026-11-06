#![recursion_limit = "256"]
//! Streaming behavior belongs to a managed library, not the native shell.
mod model;
mod pipeline;
mod plane;
mod providers;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use model::Settings;
use pipeline::Pipeline;

pub(crate) fn public_api_methods() -> Vec<amaterasu_module_sdk::ApiMethod> {
    use amaterasu_module_sdk::{ApiFieldKind, operation_api};
    vec![
        operation_api::method("nonary.stream-status/1", "snapshot", &[], &[]),
        operation_api::method(
            "nonary.stream-status/1",
            "stop_stream",
            &[("session_id", ApiFieldKind::String)],
            &[],
        ),
    ]
}

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
        let mut pipeline = Pipeline::new(settings)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        for method in public_api_methods() {
            runtime.require_success(Command::RegisterApi { method })?;
        }
        // A host may select this provider as its non-disruptive activation
        // barrier. Older hosts and profiles continue without the capability.
        if matches!(
            runtime.send(Command::HostPackageBarrierRegister {}),
            Ok(Response::Ok)
        ) {
            pipeline.enable_package_barrier();
        }
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
