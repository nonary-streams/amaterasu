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

/// Validate signed startup composition without binding a provider or opening a
/// network listener. Package builders use the same private settings schema and
/// policy validation as the installed module.
pub fn validate_startup_settings(payload: &serde_json::Value) -> Result<(), String> {
    decode_startup_settings(payload).map(|_| ())
}

fn decode_startup_settings(payload: &serde_json::Value) -> ModuleResult<Settings> {
    let settings: Settings =
        serde_json::from_value(payload.clone()).map_err(|_| "invalid RTSP settings".to_owned())?;
    settings.validate().map_err(str::to_owned)?;
    Ok(settings)
}

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
        let mut settings = decode_startup_settings(&payload)?;
        let network = Network::from_settings(&runtime, &payload)?;
        if let Some(composition) = runtime.processing_composition()? {
            use amaterasu_module_sdk::processing_composition as sdk;
            let owner = composition
                .stream_owner
                .as_ref()
                .ok_or("RTSP has no selected stream owner")?;
            settings.stream_service =
                sdk::require_contract(owner, sdk::STREAM_OWNER_CONTRACT, 1, 0)?.to_owned();
            settings.lifecycle_service = composition
                .lifecycle
                .as_ref()
                .map(|binding| {
                    sdk::require_contract(
                        binding,
                        amaterasu_module_sdk::extension_api::SESSION_LIFECYCLE_SERVICE,
                        1,
                        0,
                    )
                    .map(str::to_owned)
                })
                .transpose()?;
        }
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

    fn prepare_shutdown(&mut self, remaining_ms: u64) -> ModuleResult<()> {
        let now = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        self.controller.prepare_shutdown(now, remaining_ms);
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        let now = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        Ok(self.controller.quiesce(&self.runtime, now))
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
        invalid_settings: bool,
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
            Command::GetSettings {} => {
                let mut payload = json!({"enabled":true,"bind":"127.0.0.1:0","network_provider":"example.network/1"});
                if host.invalid_settings {
                    payload["retirement_status_principals"] = json!(["unscoped-observer"]);
                }
                Response::Result { payload }
            }
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

    #[test]
    fn startup_composition_validation_uses_the_owned_schema_without_binding_network() {
        let settings = json!({"enabled":true,"bind":"127.0.0.1:0",
            "network_provider":"example.network/1", "allowed_principals":["amaterasu.nvhttp"],
            "retirement_status_principals":["observer"],
            "retirement_status_owners":{"observer":"amaterasu.nvhttp"},
            "max_live_tickets":32});
        assert!(validate_startup_settings(&settings).is_ok());
        for invalid in [
            json!({"allowed_principals":["amaterasu.nvhttp"],"retirement_status_principals":["observer"]}),
            json!({"allowed_principals":["amaterasu.nvhttp"],"retirement_status_owners":{"observer":"foreign-owner"}}),
            json!({"max_live_tickets":33}),
            json!({"unknown_policy":true}),
        ] {
            assert!(validate_startup_settings(&invalid).is_err(), "{invalid}");
        }
        let mut host = Host {
            invalid_settings: true,
            ..Host::default()
        };
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut host as *mut Host).cast(),
            invoke,
        };
        // SAFETY: the host remains alive during synchronous startup. Invalid
        // policy prevents a provider binding or a module from escaping.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        assert!(RtspModule::start(runtime).is_err());
        assert!(host.bound_service.is_none());
        assert!(!host.registered);
    }
}
