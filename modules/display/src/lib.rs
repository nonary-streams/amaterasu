//! Physical display changes guarded by a durable recovery journal.
pub mod mode_remapping;
mod model;
mod pipeline;
mod platform;
pub mod refresh_rate_option;

pub use mode_remapping::{
    DisplayMode, ModeRemapping, ModeRemappingEntry, ModeRemappingError, Resolution, ResolverContext,
};
pub use model::Rational;
pub use refresh_rate_option::RefreshRateOption;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use pipeline::Pipeline;
use platform::NativePlatform;
use serde::Deserialize;

const SERVICE: &str = "amaterasu.display/1";
const STATE_SERVICE: &str = "amaterasu.state/1";
const JOURNAL_KEY: &str = "physical-display/recovery";

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    allow_mutation: bool,
}
struct DisplayModule {
    runtime: RuntimeClient,
    pipeline: Pipeline<NativePlatform>,
}
impl ManagedModule for DisplayModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("display settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload)
            .map_err(|error| format!("invalid display settings: {error}"))?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            pipeline: Pipeline::new(NativePlatform, settings.allow_mutation),
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.pipeline
            .tick(|command| self.runtime.send_ephemeral(command))
    }
}
amaterasu_module_sdk::export_module!(DisplayModule);
