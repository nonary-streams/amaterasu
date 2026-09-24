use crate::service::handle_payload;
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};

const SERVICE: &str = "amaterasu.frame-limiter/1";

/// Registers the policy service and answers requests without claiming provider effects.
pub struct FrameLimiterModule {
    runtime: RuntimeClient,
}

impl ManagedModule for FrameLimiterModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self { runtime })
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
                if !self.request_is_active(id)? {
                    return Ok(());
                }

                let response = if service != SERVICE {
                    json!({
                        "ok": false,
                        "applied": false,
                        "error": {
                            "code": "invalid_service",
                            "message": "unsupported frame-limiter service"
                        }
                    })
                } else {
                    parse_and_handle(&caller, payload)
                };

                self.complete(id, response)
            }
            response => Err(format!("frame-limiter poll failed: {response:?}")),
        }
    }
}

impl FrameLimiterModule {
    fn request_is_active(&self, id: u64) -> ModuleResult<bool> {
        match self.runtime.send(Command::CheckRequest { request: id })? {
            Response::Ok => Ok(true),
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(false)
            }
            response => Err(format!("frame-limiter request check failed: {response:?}")),
        }
    }

    fn complete(&self, id: u64, payload: Value) -> ModuleResult<()> {
        match self.runtime.send(Command::Complete {
            request: id,
            payload,
        })? {
            Response::Ok => Ok(()),
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(())
            }
            response => Err(format!("frame-limiter completion failed: {response:?}")),
        }
    }
}

fn parse_and_handle(caller: &str, payload: Value) -> Value {
    handle_payload(caller, payload)
}
