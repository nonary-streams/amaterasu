//! Owned, encrypted GameStream control transport. No product code in the shell.
mod controller;
mod model;
mod native;
mod stream;

use amaterasu_module_sdk::{
    Command, ManagedModule, ModuleResult, Response, RuntimeClient, network::Network,
};
use controller::{Controller, Reply};
use model::{Operation, Settings};
use serde_json::json;
use std::sync::{Arc, Mutex};

const SERVICE: &str = "amaterasu.control-transport/1";

struct ControlTransport {
    runtime: RuntimeClient,
    controller: native::Shared,
    _factory: Box<native::Factory>,
}

impl ManagedModule for ControlTransport {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        Self::start_configured(runtime, None)
    }

    fn tick(&mut self) -> ModuleResult<()> {
        let _priority = amaterasu_module_sdk::threading::ScopedThreadPriority::best_effort(
            amaterasu_module_sdk::threading::ThreadPriority::Critical,
        );
        self.controller
            .lock()
            .map_err(|_| "control state unavailable")?
            .tick();
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
                let reply = self.dispatch(&caller, &service, payload);
                let Reply { payload, created } = reply.unwrap_or_else(|code| Reply {
                    payload: json!({"ok": false, "code": code}),
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
                    // A cancelled/lost open response must not leave hidden sockets.
                    let _ = self.cleanup_controller().close(&caller, stream);
                }
                Ok(())
            }
            _ => Err("control transport poll failed".into()),
        }
    }
}

impl ControlTransport {
    // Cleanup must still release sockets after a panic poisoned normal dispatch.
    fn cleanup_controller(&self) -> std::sync::MutexGuard<'_, Controller> {
        self.controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn dispatch(
        &self,
        owner: &str,
        service: &str,
        payload: serde_json::Value,
    ) -> model::ControlResult<Reply> {
        if service != SERVICE {
            return Err("invalid_service");
        }
        let operation: Operation =
            serde_json::from_value(payload).map_err(|_| "invalid_request")?;
        self.controller
            .lock()
            .map_err(|_| "control_state_unavailable")?
            .handle(owner, operation)
    }
}

impl ControlTransport {
    fn start_configured(
        runtime: RuntimeClient,
        selected_network: Option<Network>,
    ) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("control transport settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload.clone())
            .map_err(|_| "invalid control transport settings")?;
        settings.validate()?;
        let network = match selected_network {
            Some(network) => network,
            None => Network::from_settings(&runtime, &payload)?,
        };
        let controller = Arc::new(Mutex::new(Controller::with_network(settings, network)));
        let factory = native::Factory::new(controller.clone());
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        // SAFETY: boxed factory lives through unload. Signed dependencies require
        // consumers to release bindings before provider code unloads.
        unsafe {
            amaterasu_module_sdk::transport::register_control(&runtime, SERVICE, &factory.api)?;
        }
        Ok(Self {
            runtime,
            controller,
            _factory: factory,
        })
    }
}

impl Drop for ControlTransport {
    fn drop(&mut self) {
        self.cleanup_controller().shutdown();
    }
}

amaterasu_module_sdk::export_module!(ControlTransport);

#[cfg(test)]
mod module_tests;
