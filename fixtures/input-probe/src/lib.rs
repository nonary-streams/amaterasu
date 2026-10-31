//! Managed in-memory input provider for signed protocol integration tests.
mod controller;
mod model;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use controller::Controller;
use model::{FixtureError, Operation, Settings};
use serde_json::{Value, json};
use std::time::Instant;

const INPUT_SERVICE: &str = amaterasu_module_sdk::input::INPUT_SERVICE;
const METRICS_SERVICE: &str = "test.input-probe/1";

struct InputProbe {
    runtime: RuntimeClient,
    controller: Controller,
    settings: Settings,
}

impl ManagedModule for InputProbe {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("input probe settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload)
            .map_err(|error| format!("invalid input probe settings: {error}"))?;
        for service in [INPUT_SERVICE, METRICS_SERVICE] {
            runtime.require_success(Command::Register {
                service: service.into(),
            })?;
        }
        Ok(Self {
            runtime,
            controller: Controller::new(settings.enabled),
            settings,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.controller.expire(Instant::now());
        for _ in 0..8 {
            let (id, caller, service, payload) = match self.runtime.send(Command::Poll {})? {
                Response::Pending => return Ok(()),
                Response::Request {
                    id,
                    caller,
                    service,
                    payload,
                    ..
                } => (id, caller, service, payload),
                response => return Err(format!("input probe poll failed: {response:?}")),
            };
            if !matches!(
                self.runtime.send(Command::CheckRequest { request: id })?,
                Response::Ok
            ) {
                continue;
            }
            let (reply, cleanup_on_loss) = self.dispatch(&caller, &service, payload);
            match self.runtime.send(Command::Complete {
                request: id,
                payload: reply,
            }) {
                Ok(Response::Ok) => {}
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
                    return Err(format!("input probe completion failed: {failure:?}"));
                }
            }
        }
        Ok(())
    }
}

impl InputProbe {
    fn dispatch(&mut self, caller: &str, service: &str, payload: Value) -> (Value, bool) {
        if service == METRICS_SERVICE {
            return if payload == json!({"action": "status"}) {
                (self.controller.metrics(Instant::now()), false)
            } else {
                (
                    FixtureError::new("invalid_request", "metrics supports only status").payload(),
                    false,
                )
            };
        }
        if service != INPUT_SERVICE {
            return (
                FixtureError::new("invalid_service", "unsupported input probe service").payload(),
                false,
            );
        }
        if payload.get("action").and_then(Value::as_str) == Some("settings_get") {
            if caller != "amaterasu.management" && caller != "amaterasu.nvhttp" {
                return (
                    FixtureError::new(
                        "permission_denied",
                        "input settings require the management or NVHTTP principal",
                    )
                    .payload(),
                    false,
                );
            }
            if payload != json!({"action":"settings_get"}) {
                return (
                    FixtureError::new("invalid_request", "settings_get accepts no fields")
                        .payload(),
                    false,
                );
            }
            return (self.settings.settings_get_response(), false);
        }
        let operation = match serde_json::from_value::<Operation>(payload) {
            Ok(operation) => operation,
            Err(error) => {
                return (
                    FixtureError::new("invalid_request", error.to_string()).payload(),
                    false,
                );
            }
        };
        let mutating = operation.mutates();
        let reply = self.controller.handle(caller, operation, Instant::now());
        let cleanup_on_loss = mutating && reply["ok"] == true;
        (reply, cleanup_on_loss)
    }
}

amaterasu_module_sdk::export_module!(InputProbe);
