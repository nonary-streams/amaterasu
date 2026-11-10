//! Integration fixture, not an implementation of a streaming feature.
use amaterasu_module_sdk::{ManagedModule, ModuleResult, RuntimeClient};
use serde_json::json;

struct EchoModule {
    runtime: RuntimeClient,
}

impl ManagedModule for EchoModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.provide("example.echo/1")?;
        Ok(Self { runtime })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if cfg!(feature = "stall-tick") {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }

        if let Some(request) = self.runtime.next_request()? {
            let revision = if cfg!(feature = "revision-two") { 2 } else { 1 };
            request.reply(
                &self.runtime,
                &json!({"revision": revision, "echo": request.payload}),
            )?;
        }
        Ok(())
    }

    fn health(&self) -> ModuleResult<()> {
        if cfg!(feature = "fail-health") {
            return Err("intentional fixture health failure".into());
        }
        Ok(())
    }
}

amaterasu_module_sdk::export_module!(EchoModule);
