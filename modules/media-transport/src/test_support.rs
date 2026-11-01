//! Test-only access to the production module and native factories.
use crate::{MediaTransportModule, model::Operation};
use amaterasu_module_sdk::{RuntimeClient, network::Network};
use serde_json::Value;

pub struct Transport {
    module: MediaTransportModule,
}
impl Transport {
    pub fn start(runtime: RuntimeClient, open: Value, owner: &str) -> Result<(Self, u64), String> {
        let module =
            MediaTransportModule::start_configured(runtime, Some(Network::test_provider()))?;
        let operation: Operation = serde_json::from_value(open).map_err(|e| e.to_string())?;
        let stream = module
            .controller
            .lock()
            .map_err(|_| "poisoned")?
            .handle(owner, operation)
            .map_err(str::to_owned)?
            .created
            .ok_or("no stream")?;
        Ok((Self { module }, stream))
    }
    pub fn tick(&self) {
        self.module.controller.lock().unwrap().tick();
    }
}
