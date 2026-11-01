//! Explicit loopback-only fixture for broker and consumer integration tests.
use crate::{
    controller::Controller,
    model::{Operation, Settings},
};
use amaterasu_module_sdk::network::Network;
use serde_json::{Value, json};
use std::net::SocketAddr;

#[derive(Clone)]
pub struct ControlFixture {
    controller: std::sync::Arc<std::sync::Mutex<Controller>>,
}
impl ControlFixture {
    pub fn new(owner: &str) -> Self {
        let settings = Settings {
            network_provider: None,
            enabled: true,
            allow_remote: false,
            allowed_principals: vec![owner.into()],
        };
        Self {
            controller: std::sync::Arc::new(std::sync::Mutex::new(Controller::with_network(
                settings,
                Network::test_provider(),
            ))),
        }
    }
    pub fn open(&mut self, owner: &str) -> Result<Value, &'static str> {
        self.handle(owner,json!({"action":"open","peer_ip":"127.0.0.1","connect_data":17,"key_hex":"0123456789abcdef0123456789abcdef","operation_id":"fixture-open"}))
    }
    pub fn handle(&mut self, owner: &str, payload: Value) -> Result<Value, &'static str> {
        let operation: Operation =
            serde_json::from_value(payload).map_err(|_| "invalid_request")?;
        self.controller
            .lock()
            .map_err(|_| "control_state_unavailable")?
            .handle(owner, operation)
            .map(|reply| reply.payload)
    }
    pub fn stage_feedback(
        &mut self,
        owner: &str,
        stream: u64,
        destination: SocketAddr,
    ) -> Result<(), &'static str> {
        self.controller
            .lock()
            .map_err(|_| "control_state_unavailable")?
            .stage_feedback_for_test(owner, stream, destination)
    }
    pub fn pause_local_send(
        &mut self,
        owner: &str,
        stream: u64,
        entered: std::sync::mpsc::SyncSender<()>,
        resume: std::sync::mpsc::Receiver<()>,
    ) -> Result<(), &'static str> {
        self.controller
            .lock()
            .map_err(|_| "control_state_unavailable")?
            .pause_local_send_for_test(owner, stream, entered, resume)
    }
    pub fn tick(&mut self) {
        self.controller.lock().expect("test controller").tick();
    }
}

/// Managed provider entry point for integration harnesses that supply a runtime.
pub struct ManagedControl(crate::ControlTransport);
impl ManagedControl {
    pub fn start(runtime: amaterasu_module_sdk::RuntimeClient) -> Result<Self, String> {
        crate::ControlTransport::start_configured(runtime, Some(Network::test_provider())).map(Self)
    }
    pub fn tick(&mut self) -> Result<(), String> {
        amaterasu_module_sdk::ManagedModule::tick(&mut self.0)
    }
}
