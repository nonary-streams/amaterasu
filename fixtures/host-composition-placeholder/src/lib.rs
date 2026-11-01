//! No-op signed-package component used only by the host/NVHTTP composition test.
use amaterasu_module_sdk::{ManagedModule, ModuleResult, RuntimeClient};

struct Placeholder;

impl ManagedModule for Placeholder {
    fn start(_runtime: RuntimeClient) -> ModuleResult<Self> {
        Ok(Self)
    }

    fn tick(&mut self) -> ModuleResult<()> {
        Ok(())
    }
}

amaterasu_module_sdk::export_module!(Placeholder);
