//! Current-user ScreenCast portal capture through its restricted PipeWire remote.
//! Native modules are trusted code; runtime API grants are not a native-code sandbox.
mod admission;
mod backend;
mod buffers;
mod controller;
mod model;
mod native;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use backend::NativeBackend;
use controller::Controller;
use model::{Operation, Settings};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

const SERVICE: &str = "amaterasu.capture-pipewire/1";

struct CaptureModule {
    runtime: RuntimeClient,
    controller: native::Shared<NativeBackend>,
    _factory: Box<native::Factory<NativeBackend>>,
    epoch: Instant,
}

impl ManagedModule for CaptureModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("PipeWire settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        settings.validate().map_err(str::to_owned)?;
        let controller = Arc::new(Mutex::new(Controller::new(NativeBackend::new(), settings)));
        let factory = native::Factory::new(controller.clone());
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        // SAFETY: boxed factory remains stable until destruction; every independent
        // binding retains its own shared controller after factory release.
        unsafe { runtime.register_native(SERVICE, &factory.api) }?;
        Ok(Self {
            runtime,
            controller,
            _factory: factory,
            epoch: Instant::now(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        let now = self.epoch.elapsed().as_millis().min(u64::MAX as u128) as u64;
        self.controller
            .lock()
            .map_err(|_| "PipeWire controller poisoned")?
            .refresh(now);
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
                response => return Err(format!("PipeWire poll failed: {response:?}")),
            };
            if !matches!(
                self.runtime.send(Command::CheckRequest { request: id })?,
                Response::Ok
            ) {
                continue;
            }
            let reply = if service != SERVICE {
                Err("invalid_service")
            } else {
                match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) => self
                        .controller
                        .lock()
                        .map_err(|_| "PipeWire controller poisoned")?
                        .handle(&caller, operation, now),
                    Err(_) => Err("invalid_request"),
                }
            };
            let (payload, created) = match reply {
                Ok(reply) => (reply.payload, reply.created),
                Err(code) => (
                    json!({"ok":false,"error":{"code":code,"message":code}}),
                    None,
                ),
            };
            let completion = self.runtime.send(Command::Complete {
                request: id,
                payload,
            });
            if !matches!(completion, Ok(Response::Ok)) {
                self.controller
                    .lock()
                    .map_err(|_| "PipeWire controller poisoned")?
                    .rollback(&caller, created);
                if !matches!(completion, Ok(Response::Error { ref code, .. }) if code == "request_ended" || code == "not_found")
                {
                    return Err(format!("PipeWire completion failed: {completion:?}"));
                }
            }
        }
        Ok(())
    }
}

impl Drop for CaptureModule {
    fn drop(&mut self) {
        self.controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutdown();
    }
}
amaterasu_module_sdk::export_module!(CaptureModule);

#[cfg(test)]
mod test_support;
