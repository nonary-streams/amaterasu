//! Caller-owned keyboard and mouse injection with bounded leases and cleanup.
mod backend;
mod controller;
mod model;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use backend::NativeBackend;
use controller::Controller;
use model::{InputError, Operation};
use serde::Deserialize;
use std::time::Instant;

const SERVICE: &str = "amaterasu.input/1";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    allow_injection: bool,
    #[serde(default = "default_uinput_path")]
    uinput_path: String,
}
fn default_uinput_path() -> String {
    "/dev/uinput".into()
}
struct InputModule {
    runtime: RuntimeClient,
    controller: Controller<NativeBackend>,
}
impl ManagedModule for InputModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("input module settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload)
            .map_err(|error| format!("invalid input settings: {error}"))?;
        if cfg!(target_os = "linux") && !std::path::Path::new(&settings.uinput_path).is_absolute() {
            return Err("uinput_path must be absolute".into());
        }
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            controller: Controller::new(
                NativeBackend::new(settings.uinput_path),
                settings.allow_injection,
            ),
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.controller.expire(Instant::now());
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
                response => return Err(format!("input poll failed: {response:?}")),
            };
            let operation: Result<Operation, _> = serde_json::from_value(payload);
            let cleanup_if_completion_is_lost = operation.as_ref().is_ok_and(|operation| {
                !matches!(
                    operation,
                    Operation::Status
                        | Operation::Lookup { .. }
                        | Operation::Acquire {
                            operation_id: Some(_),
                            ..
                        }
                )
            });
            let reply = if service != SERVICE {
                InputError::new("invalid_service", "unsupported input service").payload()
            } else {
                match operation {
                    Ok(operation) => {
                        self.controller
                            .handle(&caller, operation, Instant::now(), &mut || match self
                                .runtime
                                .send(Command::CheckRequest { request: id })
                            {
                                Ok(Response::Ok) => Ok(()),
                                Ok(Response::Error { message, .. }) => {
                                    Err(InputError::new("cancelled", message))
                                }
                                Ok(_) => Err(InputError::new(
                                    "runtime_error",
                                    "unexpected request guard reply",
                                )),
                                Err(error) => Err(InputError::new("runtime_error", error)),
                            })
                    }
                    Err(error) => InputError::new("invalid_request", error.to_string()).payload(),
                }
            };
            let cleanup_on_loss =
                completion_requires_cleanup(cleanup_if_completion_is_lost, &reply);
            match self.runtime.send(Command::Complete {
                request: id,
                payload: reply,
            }) {
                Ok(Response::Ok) => (),
                Ok(Response::Error { code, .. })
                    if code == "request_ended" || code == "not_found" =>
                {
                    if cleanup_on_loss {
                        self.controller.cancel_owner(&caller);
                    }
                }
                failure => {
                    if cleanup_on_loss {
                        self.controller.cancel_owner(&caller);
                    }
                    return Err(format!("input completion failed: {failure:?}"));
                }
            }
        }
        Ok(())
    }
}
// A rejected request may refer to a lease that predates the request. Losing
// its error reply must not revoke that lease. Failed native operations already
// attempt cleanup; unresolved releases still warrant another cleanup pass.
fn completion_requires_cleanup(mutating: bool, reply: &serde_json::Value) -> bool {
    mutating && (reply["ok"] == true || reply["cleanup_required"] == true)
}

amaterasu_module_sdk::export_module!(InputModule);
