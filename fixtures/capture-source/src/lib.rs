//! Trusted test-only NV12 capture source for the real video/encoder pipeline.
//! This fixture does not establish native desktop capture or codec qualification.
mod controller;
mod model;
mod native;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use controller::Controller;
use model::{Operation, error_payload};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

const CAPTURE_SERVICE: &str = "amaterasu.capture-dxgi/1";
const CUSTOM_CAPTURE_SERVICE: &str = "test.custom-capture/1";
const METRICS_SERVICE: &str = "test.capture-source/1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    custom_service: bool,
}

struct CaptureSource {
    runtime: RuntimeClient,
    controller: native::SharedController,
    capture_service: &'static str,
    _factory: Box<native::Factory>,
}

impl ManagedModule for CaptureSource {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("capture fixture settings unavailable".into());
        };
        let settings = serde_json::from_value::<Settings>(payload)
            .map_err(|error| format!("invalid capture fixture settings: {error}"))?;
        let capture_service = if settings.custom_service {
            CUSTOM_CAPTURE_SERVICE
        } else {
            CAPTURE_SERVICE
        };
        let controller = Arc::new(Mutex::new(Controller::default()));
        let factory = native::Factory::new(controller.clone());
        runtime.require_success(Command::Register {
            service: capture_service.into(),
        })?;
        // SAFETY: the stable boxed factory lives through module destruction;
        // consumers own independent shared references through their bindings.
        unsafe {
            runtime.register_native_typed(
                capture_service,
                amaterasu_module_sdk::media::native_contracts::CAPTURE,
                &factory.api,
            )
        }?;
        runtime.require_success(Command::Register {
            service: METRICS_SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            controller,
            capture_service,
            _factory: factory,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        for _ in 0..8 {
            let (id, caller, service, payload) = match self.runtime.send(Command::Poll {})? {
                Response::Pending => break,
                Response::Request {
                    id,
                    caller,
                    service,
                    payload,
                    ..
                } => (id, caller, service, payload),
                response => return Err(format!("capture fixture poll failed: {response:?}")),
            };
            if !matches!(
                self.runtime.send(Command::CheckRequest { request: id })?,
                Response::Ok
            ) {
                continue;
            }
            let (payload, created) = {
                let mut controller = self
                    .controller
                    .lock()
                    .map_err(|_| "capture fixture controller poisoned")?;
                let reply = match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) if service == self.capture_service => {
                        controller.handle(&caller, operation)
                    }
                    Ok(Operation::Status) if service == METRICS_SERVICE => {
                        Ok((controller.metrics(), None))
                    }
                    Ok(_) if service == METRICS_SERVICE => Err("unsupported_action"),
                    Ok(_) => Err("invalid_service"),
                    Err(_) => Err("invalid_request"),
                };
                reply.unwrap_or_else(|code| (error_payload(code), None))
            };
            let completion = self.runtime.send(Command::Complete {
                request: id,
                payload,
            });
            if !matches!(completion, Ok(Response::Ok)) {
                self.controller
                    .lock()
                    .map_err(|_| "capture fixture controller poisoned")?
                    .rollback_open(&caller, created);
                if !matches!(completion, Ok(Response::Error { ref code, .. })
                    if code == "request_ended" || code == "not_found")
                {
                    return Err(format!("capture fixture completion failed: {completion:?}"));
                }
            }
        }
        Ok(())
    }
}

impl Drop for CaptureSource {
    fn drop(&mut self) {
        self.controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutdown();
    }
}

amaterasu_module_sdk::export_module!(CaptureSource);

#[cfg(test)]
mod tests;
