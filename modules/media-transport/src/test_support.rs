//! Test-only access to the production module and native factories.
use crate::{MediaTransportModule, model::Operation};
use amaterasu_module_sdk::{ManagedModule, RuntimeClient, network::Network};
use serde_json::Value;

/// Test-only handle to the production controller and ordered audio path.
pub struct DirectTransport {
    controller: crate::controller::Controller,
    owner: String,
    stream: u64,
}
impl DirectTransport {
    pub fn open(open: Value, owner: &str) -> Result<Self, String> {
        let settings = crate::model::Settings {
            enabled: true,
            allowed_principals: vec![owner.into()],
            ..Default::default()
        };
        let mut controller =
            crate::controller::Controller::with_network(settings, Network::test_provider());
        let operation: Operation =
            serde_json::from_value(open).map_err(|error| error.to_string())?;
        let stream = controller
            .handle(owner, operation)
            .map_err(str::to_owned)?
            .created
            .ok_or("no stream")?;
        Ok(Self {
            controller,
            owner: owner.into(),
            stream,
        })
    }
    pub fn stream(&self) -> u64 {
        self.stream
    }
    pub fn probe(
        &self,
        stream: u64,
        selection: &amaterasu_media::AudioTransportSelectionV2,
    ) -> amaterasu_module_sdk::abi::Status {
        self.controller
            .probe_ordered_audio(&self.owner, stream, selection)
    }
    pub fn prepare(
        &mut self,
        stream: u64,
        selection: &amaterasu_media::AudioTransportSelectionV2,
    ) -> amaterasu_module_sdk::abi::Status {
        self.controller
            .prepare_ordered_audio(&self.owner, stream, selection)
    }
    /// # Safety
    /// Packet and borrowed bytes remain live through the synchronous call.
    pub unsafe fn submit(
        &mut self,
        stream: u64,
        packet: &amaterasu_media::NegotiatedAudioPacket,
    ) -> amaterasu_module_sdk::abi::Status {
        unsafe {
            self.controller
                .submit_ordered_audio(&self.owner, stream, packet)
        }
    }
    pub fn tick(&mut self) {
        self.controller.tick();
    }
}

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

/// Drive the normal managed control service using the test loopback provider.
pub struct ManagedTransport(MediaTransportModule);
impl ManagedTransport {
    pub fn start(runtime: RuntimeClient) -> Result<Self, String> {
        MediaTransportModule::start_configured(runtime, Some(Network::test_provider())).map(Self)
    }
    pub fn tick(&mut self) -> Result<(), String> {
        self.0.tick()
    }
}
