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
    factory: Box<amaterasu_network_sockets::Factory>,
}
impl ManagedModule for NetworkModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("network provider settings unavailable".into());
        };
        if !payload.is_object() {
            return Err("network provider settings must be an object".into());
        }
        let settings: Settings = serde_json::from_value(payload).map_err(|e| e.to_string())?;
        if settings.max_handles == 0 || settings.max_handles > 65536 {
            return Err("invalid network handle budget".into());
        }
        let state_root = runtime.runtime_state_root()?;
        if !state_root.is_absolute()
            || state_root.as_os_str().to_string_lossy().contains('\0')
            || state_root
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err("network resolver state root is not a canonical absolute path".into());
        }
        let helper_root = state_root.join("native-network/resolver");
        let factory = amaterasu_network_sockets::Factory::with_runtime_state(
            settings.max_handles,
            &helper_root,
        )
        .map_err(|error| format!("network resolver helper unavailable: {error}"))?;
        // SAFETY: boxed factory retains all provider code/state until module destroy;
        // signed dependencies order every native consumer before provider destruction.
        unsafe {
            runtime.register_native_typed(
                amaterasu_network_api::SERVICE,
                amaterasu_network_api::CONTRACT,
                &factory.api,
            )?;
        }
        Ok(Self { factory })
    }
    fn prepare_shutdown(&mut self, remaining_ms: u64) -> ModuleResult<()> {
        self.factory
            .prepare_shutdown(remaining_ms)
            .map_err(|error| error.to_string())
    }
    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.factory.quiesce().map_err(|error| error.to_string())
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.factory.poll().map_err(|error| error.to_string())
    }
}
amaterasu_module_sdk::export_module!(NetworkModule);

#[cfg(test)]
mod tests;
