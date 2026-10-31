//! Default network provider. Consumer modules depend on the network contract,
//! not this concrete component or its OS socket implementation.
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default = "default_handles")]
    max_handles: u32,
}
fn default_handles() -> u32 {
    1024
}
struct NetworkModule {
    _factory: Box<amaterasu_network_sockets::Factory>,
}
impl ManagedModule for NetworkModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("network provider settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload).map_err(|e| e.to_string())?;
        if settings.max_handles == 0 || settings.max_handles > 65536 {
            return Err("invalid network handle budget".into());
        }
        let factory = amaterasu_network_sockets::Factory::with_limits(settings.max_handles);
        // SAFETY: boxed factory retains all provider code/state until module destroy;
        // signed dependencies order every native consumer before provider destruction.
        unsafe {
            runtime.register_native_typed(
                amaterasu_network_api::SERVICE,
                amaterasu_network_api::CONTRACT,
                &factory.api,
            )?;
        }
        Ok(Self { _factory: factory })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        Ok(())
    }
}
amaterasu_module_sdk::export_module!(NetworkModule);
