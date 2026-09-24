//! Caller-owned Windows WASAPI loopback audio and borrowed native PCM frames.
mod controller;
mod model;
mod native;
mod open_policy;
#[cfg(any(windows, test))]
mod packet;
mod platform;
#[cfg(any(windows, test))]
mod route_policy;
mod virtual_sink;
mod worker;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use controller::{ControlReply, Controller};
use model::Operation;
use serde::Deserialize;
use serde_json::json;
use std::sync::{Arc, Mutex};

const SERVICE: &str = "amaterasu.audio/1";

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    enabled: bool,
}

struct AudioModule {
    runtime: RuntimeClient,
    controller: native::Shared,
    _factory: Box<native::Factory>,
}

impl ManagedModule for AudioModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("audio settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid audio settings")?;
        let controller = Arc::new(Mutex::new(Controller::new(settings.enabled)));
        let factory = native::Factory::new(controller.clone());
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        // SAFETY: boxed factory/context outlive bindings; shared controller
        // synchronizes callbacks. Signed dependency order requires consumers to
        // release their bindings before provider unload.
        unsafe {
            runtime.register_native(SERVICE, &factory.api)?;
        }
        Ok(Self {
            runtime,
            controller,
            _factory: factory,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                service,
                payload,
                ..
            } => {
                if !matches!(
                    self.runtime.send(Command::CheckRequest { request: id })?,
                    Response::Ok
                ) {
                    return Ok(());
                }
                let result = if service == SERVICE {
                    serde_json::from_value::<Operation>(payload)
                        .map_err(|_| "invalid_request")
                        .map(open_policy::normalize)
                        .and_then(|operation| {
                            self.controller
                                .lock()
                                .map_err(|_| "audio_state_failed")?
                                .handle(&caller, operation)
                        })
                } else {
                    Err("invalid_service")
                };
                let ControlReply { payload, created } =
                    result.unwrap_or_else(|code| ControlReply {
                        payload: json!({"ok":false,"code":code}),
                        created: None,
                    });
                if !matches!(
                    self.runtime.send(Command::Complete {
                        request: id,
                        payload
                    }),
                    Ok(Response::Ok)
                ) && let Some(stream) = created
                {
                    let _ = self
                        .controller
                        .lock()
                        .map(|mut controller| controller.close(&caller, stream));
                }
                Ok(())
            }
            _ => Err("audio poll failed".into()),
        }
    }
}

impl Drop for AudioModule {
    fn drop(&mut self) {
        if let Ok(mut controller) = self.controller.lock() {
            controller.shutdown();
        }
    }
}

amaterasu_module_sdk::export_module!(AudioModule);
