//! Managed encrypted RTSP endpoint. Network data never supplies broker authority.
#![recursion_limit = "256"]

mod controller;
mod model;
mod rpc;
mod transport;

use amaterasu_module_sdk::network::Network;
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use controller::Controller;
use model::Settings;
use std::time::Instant;

const SERVICE: &str = "amaterasu.rtsp/1";
struct RtspModule {
    runtime: RuntimeClient,
    controller: Controller,
    started: Instant,
}

impl ManagedModule for RtspModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("RTSP settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload.clone()).map_err(|_| "invalid RTSP settings")?;
        let network = Network::from_settings(&runtime, &payload)?;
        let controller = Controller::with_network(settings, network)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            controller,
            started: Instant::now(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        let now = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        self.controller.tick(&self.runtime, now)
    }
}

impl Drop for RtspModule {
    fn drop(&mut self) {
        self.controller.shutdown(&self.runtime);
    }
}

amaterasu_module_sdk::export_module!(RtspModule);

#[cfg(test)]
mod network_startup_tests {
    use super::*;
    use amaterasu_module_sdk::abi::{
        AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INVALID_ARGUMENT, STATUS_OK, Status,
    };
    use serde_json::json;
    use std::ffi::c_void;

    #[derive(Default)]
    struct Host {
        bound_service: Option<String>,
        bound_contract: Option<String>,
        registered: bool,
    }

    unsafe extern "C" fn invoke(
        context: *mut c_void,
        input: BorrowedBytes,
        reply: ReplySink,
    ) -> Status {
        // SAFETY: the test retains host, input and reply storage throughout this call.
        let Ok(bytes) = (unsafe { input.read() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let host = unsafe { &mut *context.cast::<Host>() };
        let response = match command {
            Command::GetSettings {} => Response::Result {
                payload: json!({"enabled":true,"bind":"127.0.0.1:0","network_provider":"example.network/1"}),
            },
            Command::NativeBindTyped {
                service, contract, ..
            } => {
                host.bound_service = Some(service);
                host.bound_contract = Some(contract);
                Response::error("permission_denied", "test provider unavailable")
            }
            Command::Register { .. } => {
                host.registered = true;
                Response::Ok
            }
            _ => Response::error("unexpected_command", "test operation rejected"),
        };
        let bytes = serde_json::to_vec(&response).unwrap();
        unsafe { reply.send(&bytes) };
        STATUS_OK
    }

    #[test]
    fn startup_selects_configured_provider_and_fails_closed_when_binding_is_denied() {
        let mut host = Host::default();
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut host as *mut Host).cast(),
            invoke,
        };
        // SAFETY: boxed-equivalent stack host remains live until startup returns;
        // no provider binding succeeds and no module escapes this scope.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        assert!(RtspModule::start(runtime).is_err());
        assert_eq!(host.bound_service.as_deref(), Some("example.network/1"));
        assert_eq!(
            host.bound_contract.as_deref(),
            Some("amaterasu.network-interface/1")
        );
        assert!(!host.registered);
    }
}
