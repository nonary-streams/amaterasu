//! XRandR-selected X11 desktop capture using server readback into owned BGRA frames.
//! Native modules are trusted code; runtime API grants are not a native-code sandbox.
mod backend;
mod controller;
mod model;
mod native;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use backend::NativeBackend;
use controller::Controller;
use model::{CaptureError, Operation};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

const SERVICE: &str = "amaterasu.capture-x11/1";
type SharedController = Arc<Mutex<Controller>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    enabled: bool,
}

struct CaptureModule {
    runtime: RuntimeClient,
    controller: SharedController,
    _factory: Box<native::Factory<NativeBackend>>,
}

impl ManagedModule for CaptureModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("X11 capture settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        let controller = Arc::new(Mutex::new(Controller::new(
            NativeBackend::new(),
            settings.enabled,
        )));
        let factory = native::Factory::new(controller.clone());
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        // SAFETY: factory remains boxed through module lifetime. Each binding
        // clones the controller and is uniquely released by its broker owner.
        unsafe {
            runtime.register_native_typed(
                SERVICE,
                amaterasu_module_sdk::media::native_contracts::CAPTURE,
                &factory.api,
            )
        }?;
        Ok(Self {
            runtime,
            controller,
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
                response => return Err(format!("X11 capture polling failed: {response:?}")),
            };
            if !matches!(
                self.runtime.send(Command::CheckRequest { request: id })?,
                Response::Ok
            ) {
                continue;
            }
            let mut opened_stream = None;
            let reply = if service != SERVICE {
                CaptureError::new("invalid_service", "unsupported capture service").payload()
            } else {
                match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) => {
                        let mut controller = self
                            .controller
                            .lock()
                            .map_err(|_| "X11 capture controller poisoned")?;
                        let reply = controller.handle(&caller, operation, &mut || match self
                            .runtime
                            .send(Command::CheckRequest { request: id })
                        {
                            Ok(Response::Ok) => Ok(()),
                            _ => Err(CaptureError::new("cancelled", "X11 capture request ended")),
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
                            .map_err(|_| "X11 capture controller poisoned")?
                            .close(&caller, stream);
                    }
                    if !matches!(result, Ok(Response::Error { ref code, .. }) if code == "request_ended" || code == "not_found")
                    {
                        return Err(format!("X11 capture completion failed: {result:?}"));
                    }
                }
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
