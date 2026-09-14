//! Integration fixture, not an implementation of a streaming feature.
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde_json::json;

struct EchoModule {
    runtime: RuntimeClient,
}

impl ManagedModule for EchoModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: "example.echo/1".into(),
        })?;
        Ok(Self { runtime })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if cfg!(feature = "stall-tick") {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }

        if let Response::Request {
            id: request_id,
            payload,
            ..
        } = self.runtime.send(Command::Poll {})?
        {
            let revision = if cfg!(feature = "revision-two") { 2 } else { 1 };
            self.runtime.require_success(Command::Complete {
                request: request_id,
                payload: json!({"revision": revision, "echo": payload}),
            })?;
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
