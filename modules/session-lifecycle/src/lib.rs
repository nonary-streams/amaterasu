//! Durable, broker-admitted session lifecycle authority.
mod adapter;
mod coordinator;
pub mod obligations;
pub mod permits;
pub mod planning;
mod store;

use adapter::{LifecycleAdapter, LifecycleHandler};
use amaterasu_module_sdk::ModuleResult;
pub use coordinator::Coordinator;
use serde_json::Value;
use std::path::Path;

impl LifecycleHandler for Coordinator {
    fn from_journal_path(path: &Path) -> ModuleResult<Self> {
        Self::open(path).map_err(|error| error.to_string())
    }

    fn handle(&mut self, caller: &str, initiator: &str, payload: Value) -> Value {
        Coordinator::handle(self, caller, initiator, payload)
    }
}

type SessionLifecycleModule = LifecycleAdapter<Coordinator>;
amaterasu_module_sdk::export_module!(SessionLifecycleModule);
