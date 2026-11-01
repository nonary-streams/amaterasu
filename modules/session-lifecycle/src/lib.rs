//! Durable, broker-admitted session lifecycle authority.
pub mod action_authority;
#[cfg_attr(feature = "clipboard-qualification", allow(dead_code))]
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

#[cfg(feature = "clipboard-qualification")]
use adapter::LifecycleHandler;
pub use adapter::{ConsumerSettings, LifecycleSettings, MethodSettings, ParticipantSettings};
#[cfg(not(feature = "clipboard-qualification"))]
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

#[cfg(not(feature = "clipboard-qualification"))]
type SessionLifecycleModule = LifecycleAdapter<Coordinator>;
#[cfg(all(feature = "clipboard-qualification", not(debug_assertions)))]
compile_error!("clipboard-qualification is a debug-only test artifact");
#[cfg(not(feature = "clipboard-qualification"))]
amaterasu_module_sdk::export_module!(SessionLifecycleModule);

#[cfg(test)]
mod coordinator_regression_tests;
