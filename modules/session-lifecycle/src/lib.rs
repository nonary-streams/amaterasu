//! Durable, broker-admitted session lifecycle authority.
pub mod action_authority;
mod adapter;
mod coordinator;
pub mod input_authority;
pub mod obligations;
pub mod permits;
pub mod plan_validation;
pub mod planning;
pub mod prepare_authority;
pub mod prepare_driver;
pub mod provider_dispatch;
pub mod receipts;
mod store;
pub mod transitions;

pub use adapter::{ConsumerSettings, LifecycleSettings, MethodSettings, ParticipantSettings};
use adapter::{LifecycleAdapter, LifecycleHandler};
use amaterasu_module_sdk::ModuleResult;
pub use coordinator::Coordinator;
use serde_json::Value;

impl LifecycleHandler for Coordinator {
    fn from_settings(settings: &LifecycleSettings) -> ModuleResult<Self> {
        Self::configured(settings)
    }

    fn handle(&mut self, caller: &str, initiator: &str, payload: Value) -> Value {
        Coordinator::handle(self, caller, initiator, payload)
    }

    fn advance(
        &mut self,
        send: &mut dyn FnMut(
            amaterasu_module_sdk::Command,
        ) -> ModuleResult<amaterasu_module_sdk::Response>,
    ) -> ModuleResult<()> {
        Coordinator::advance(self, send)
    }
}

type SessionLifecycleModule = LifecycleAdapter<Coordinator>;
amaterasu_module_sdk::export_module!(SessionLifecycleModule);

#[cfg(test)]
mod coordinator_regression_tests;
