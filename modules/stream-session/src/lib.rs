#![recursion_limit = "256"]
//! Streaming behavior belongs to a managed library, not the native shell.
mod model;
mod pipeline;
mod plane;
mod providers;

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

#[cfg(all(feature = "feedback-qualification", not(debug_assertions)))]
compile_error!("feedback-qualification is a debug-only test artifact");

#[cfg(not(feature = "feedback-qualification"))]
amaterasu_module_sdk::export_module!(StreamSession);

#[cfg(feature = "feedback-qualification")]
#[unsafe(no_mangle)]
pub extern "C" fn amaterasu_qualification_module_v1() -> *const amaterasu_module_sdk::abi::ModuleApi
{
    use amaterasu_module_sdk::{abi, exports};
    static API: abi::ModuleApi = abi::ModuleApi {
        header: abi::AbiHeader::of::<abi::ModuleApi>(),
        create: exports::create::<StreamSession>,
        tick: exports::tick::<StreamSession>,
        health: exports::health::<StreamSession>,
        destroy: exports::destroy::<StreamSession>,
    };
    &API
}
