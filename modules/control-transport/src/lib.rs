//! Owned, encrypted GameStream control transport. No product code in the shell.
mod controller;
mod diagnostics;
mod model;
mod native;
mod stream;

use amaterasu_module_sdk::{
    Command, ManagedModule, ModuleResult, Response, RuntimeClient, network::Network,
};
use controller::{Controller, Reply};
use model::{Operation, Settings};
use serde_json::json;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

const SERVICE: &str = "amaterasu.control-transport/1";

struct ControlTransport {
    runtime: RuntimeClient,
    controller: native::Shared,
    _factory: Box<native::Factory>,
    shutdown_announced: bool,
    last_timing_log: Instant,
}

impl ManagedModule for ControlTransport {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        Self::start_configured(runtime, None)
    }

    fn tick(&mut self) -> ModuleResult<()> {
        let _priority = amaterasu_module_sdk::threading::ScopedThreadPriority::best_effort(
            amaterasu_module_sdk::threading::ThreadPriority::Critical,
        );
        let mut controller = match self.controller.try_lock() {
            Ok(controller) => controller,
            Err(TryLockError::WouldBlock) => return Ok(()),
            Err(TryLockError::Poisoned(_)) => return Err("control state unavailable".into()),
        };
        controller.tick();
        let now = Instant::now();
        if now.saturating_duration_since(self.last_timing_log) >= Duration::from_secs(10) {
            let elapsed = now
                .saturating_duration_since(self.last_timing_log)
                .as_micros();
            let summaries = controller.timing_summaries();
            drop(controller);
            self.last_timing_log = now;
            for mut summary in summaries {
                summary["window_us"] = serde_json::json!(elapsed.min(u128::from(u64::MAX)) as u64);
                amaterasu_module_sdk::diagnostics::log_window(
                    &self.runtime,
                    "control.timing",
                    &summary,
                );
            }
            controller = match self.controller.try_lock() {
                Ok(controller) => controller,
                Err(TryLockError::WouldBlock) => return Ok(()),
                Err(TryLockError::Poisoned(_)) => return Err("control state unavailable".into()),
            };
        }
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
                let reply = Self::dispatch_locked(
                    &mut controller,
                    self.shutdown_announced,
                    &caller,
                    &service,
                    payload,
                );
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
                    let _ = controller.close(&caller, stream);
                }
                Ok(())
            }
            _ => Err("control transport poll failed".into()),
        }
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        // Consumers still own their existing streams until dependency-ordered
        // quiescence. Only seal new admission during this announcement.
        self.shutdown_announced = true;
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.shutdown_announced = true;
        let mut controller = match self.controller.try_lock() {
            Ok(controller) => controller,
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        controller.shutdown();
        Ok(true)
    }
}

impl ControlTransport {
    // Cleanup must still release sockets after a panic poisoned normal dispatch.
    fn cleanup_controller(&self) -> std::sync::MutexGuard<'_, Controller> {
        self.controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    fn dispatch(
        &self,
        owner: &str,
        service: &str,
        payload: serde_json::Value,
    ) -> model::ControlResult<Reply> {
        let mut controller = self
            .controller
            .lock()
            .map_err(|_| "control_state_unavailable")?;
        Self::dispatch_locked(
            &mut controller,
            self.shutdown_announced,
            owner,
            service,
            payload,
        )
    }

    fn dispatch_locked(
        controller: &mut Controller,
        shutdown_announced: bool,
        owner: &str,
        service: &str,
        payload: serde_json::Value,
    ) -> model::ControlResult<Reply> {
        if service != SERVICE {
            return Err("invalid_service");
        }
        let operation: Operation =
            serde_json::from_value(payload).map_err(|_| "invalid_request")?;
        if shutdown_announced && matches!(operation, Operation::Open(_)) {
            return Err("shutting_down");
        }
        controller.handle(owner, operation)
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
        let mut controller = Controller::with_network(settings, network);
        controller.trace_flows(diagnostics::FlowTrace::new(
            amaterasu_module_sdk::trace::Tracer::bind(&runtime),
        ));
        let controller = Arc::new(Mutex::new(controller));
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
            shutdown_announced: false,
            last_timing_log: Instant::now(),
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
