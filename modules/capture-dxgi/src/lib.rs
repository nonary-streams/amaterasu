//! Windows desktop duplication with caller-owned streams and synchronous CPU frames.
mod backend;
mod controller;
#[cfg(any(windows, test))]
mod cursor;
mod format;
mod model;
mod native;
mod preflight;
#[cfg(test)]
mod resource_tests;
#[cfg(any(windows, test))]
mod staging;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use backend::NativeBackend;
use controller::Controller;
use model::{CaptureError, Operation};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

const SERVICE: &str = "amaterasu.capture-dxgi/1";
type SharedController = Arc<Mutex<Controller<NativeBackend>>>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    enabled: bool,
}

struct CaptureModule {
    runtime: RuntimeClient,
    controller: SharedController,
    shutdown_requested: bool,
    _preflight_factory: Box<
        amaterasu_module_sdk::capture::SourcePreflightFactory<preflight::Provider<NativeBackend>>,
    >,
    _factory: Box<native::Factory<NativeBackend>>,
}

impl ManagedModule for CaptureModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("capture settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        let controller = Arc::new(Mutex::new(Controller::new(NativeBackend, settings.enabled)));
        let factory = native::Factory::new(controller.clone());
        // SAFETY: the original factory's bind allocates binding metadata only.
        // Both boxed factories outlive all bindings; the query provider retains
        // caller-scoped controller state and never opens a capture source.
        let preflight_factory = unsafe {
            amaterasu_module_sdk::capture::SourcePreflightFactory::new(
                &factory.api,
                Arc::new(preflight::Provider::new(controller.clone())),
            )
        };
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        // SAFETY: factory is boxed and retained through module destruction;
        // bindings own independent references to controller state.
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
            shutdown_requested: false,
            _factory: factory,
            _preflight_factory: preflight_factory,
        })
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        self.shutdown_requested = true;
        self.stop_admission_when_available()?;
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.shutdown_requested = true;
        match self.controller.try_lock() {
            Ok(mut controller) => {
                controller.shutdown();
                Ok(true)
            }
            Err(std::sync::TryLockError::WouldBlock) => Ok(false),
            Err(_) => Err("capture controller poisoned".into()),
        }
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.stop_admission_when_available()?;
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
                response => return Err(format!("capture poll failed: {response:?}")),
            };
            let mut opened_stream = None;
            let reply = if service != SERVICE {
                CaptureError::new("invalid_service", "unsupported capture service").payload()
            } else {
                match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) => {
                        let mut controller = self
                            .controller
                            .lock()
                            .map_err(|_| "capture controller poisoned")?;
                        if self.shutdown_requested {
                            controller.stop_admission();
                        }
                        let reply = controller.handle(&caller, operation, &mut || match self
                            .runtime
                            .send(Command::CheckRequest { request: id })
                        {
                            Ok(Response::Ok) => Ok(()),
                            response => Err(CaptureError::new(
                                "cancelled",
                                format!("request is no longer active: {response:?}"),
                            )),
                        });
                        opened_stream = controller.take_opened_stream();
                        reply
                    }
                    Err(error) => CaptureError::new("invalid_request", error.to_string()).payload(),
                }
            };
            match self.runtime.send(Command::Complete {
                request: id,
                payload: reply,
            }) {
                Ok(Response::Ok) => (),
                result => {
                    if let Some(stream) = opened_stream {
                        let _ = self
                            .controller
                            .lock()
                            .map_err(|_| "capture controller poisoned")?
                            .close(&caller, stream);
                    }
                    if !matches!(result, Ok(Response::Error { ref code, .. }) if code == "request_ended" || code == "not_found")
                    {
                        return Err(format!("capture completion failed: {result:?}"));
                    }
                }
            }
        }
        Ok(())
    }
}

impl CaptureModule {
    fn stop_admission_when_available(&self) -> ModuleResult<()> {
        if self.shutdown_requested {
            match self.controller.try_lock() {
                Ok(mut controller) => controller.stop_admission(),
                // A native read already owns this mutex. The module flag
                // rejects Open when its control request is processed later.
                Err(std::sync::TryLockError::WouldBlock) => (),
                Err(_) => return Err("capture controller poisoned".into()),
            }
        }
        Ok(())
    }
}

impl Drop for CaptureModule {
    fn drop(&mut self) {
        if let Ok(mut controller) = self.controller.lock() {
            controller.shutdown();
        }
    }
}
amaterasu_module_sdk::export_module!(CaptureModule);

#[cfg(test)]
mod tests;
