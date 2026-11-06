//! Explicit generated-black capture for application-independent input streams.
//! This provider opens no displays, sockets, audio devices or input devices.
mod controller;
mod model;
mod native;

use amaterasu_media::host_monotonic_micros;
use amaterasu_module_sdk::capture::{
    SourcePreflightFactory, SourcePreflightProvider, SourcePreflightRequest,
    SourcePreflightResponse,
};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use controller::Controller;
use model::{Operation, SERVICE, Settings, failure};
use std::sync::{Arc, Mutex};

struct BlackPreflight(native::SharedController);
impl SourcePreflightProvider for BlackPreflight {
    fn query(
        &self,
        _caller: &str,
        request: &SourcePreflightRequest,
    ) -> Result<SourcePreflightResponse, amaterasu_module_sdk::abi::Status> {
        let controller = self.0.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => amaterasu_media::MEDIA_BUSY,
            std::sync::TryLockError::Poisoned(_) => amaterasu_media::MEDIA_BACKEND_ERROR,
        })?;
        Ok(controller.preflight(request))
    }
}

struct BlackCapture {
    runtime: RuntimeClient,
    controller: native::SharedController,
    _factory: Box<native::Factory>,
    _preflight_factory: Box<SourcePreflightFactory<BlackPreflight>>,
}

impl ManagedModule for BlackCapture {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("black capture settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        let controller = Arc::new(Mutex::new(Controller::new(settings.enabled)));
        let factory = native::Factory::new(controller.clone());
        // SAFETY: the retained inner factory binds only controller metadata;
        // BlackPreflight reads bounded state and never opens native resources.
        let preflight_factory = unsafe {
            SourcePreflightFactory::new(&factory.api, Arc::new(BlackPreflight(controller.clone())))
        };
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        // SAFETY: this stable boxed factory lives until the module is unloaded;
        // broker-owned bindings independently retain the shared controller.
        unsafe {
            runtime.register_native_typed(
                SERVICE,
                amaterasu_module_sdk::media::native_contracts::CAPTURE,
                &preflight_factory.api,
            )
        }?;
        Ok(Self {
            runtime,
            controller,
            _factory: factory,
            _preflight_factory: preflight_factory,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.controller
            .lock()
            .map_err(|_| "black capture controller poisoned")?
            .maintain(host_monotonic_micros());
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
                _ => return Err("black capture polling failed".into()),
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
                    .map_err(|_| "black capture controller poisoned")?;
                let response = match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) if service == SERVICE => {
                        controller.handle(&caller, operation, host_monotonic_micros(), &mut || {
                            match self.runtime.send(Command::CheckRequest { request: id }) {
                                Ok(Response::Ok) => Ok(()),
                                _ => Err("request_ended"),
                            }
                        })
                    }
                    Ok(_) => Err("invalid_service"),
                    Err(_) => Err("invalid_request"),
                };
                response.unwrap_or_else(|code| (failure(code), None))
            };
            let completion = self.runtime.send(Command::Complete {
                request: id,
                payload,
            });
            if !matches!(completion, Ok(Response::Ok)) {
                self.controller
                    .lock()
                    .map_err(|_| "black capture controller poisoned")?
                    .rollback_open(&caller, created, host_monotonic_micros());
                if !matches!(completion,Ok(Response::Error {ref code,..}) if code=="request_ended"||code=="not_found")
                {
                    return Err("black capture completion failed".into());
                }
            }
        }
        Ok(())
    }
}

impl Drop for BlackCapture {
    fn drop(&mut self) {
        self.controller
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .shutdown();
    }
}

amaterasu_module_sdk::export_module!(BlackCapture);
