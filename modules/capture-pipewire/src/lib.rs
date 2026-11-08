//! Current-user ScreenCast portal capture through its restricted PipeWire remote.
//! Native modules are trusted code; runtime API grants are not a native-code sandbox.
mod admission;
mod backend;
mod buffers;
mod controller;
mod model;
mod native;

use amaterasu_module_sdk::capture::{
    SourcePreflightFactory, SourcePreflightProvider, SourcePreflightRequest,
    SourcePreflightResponse,
};
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

struct PipewirePreflight(native::Shared<NativeBackend>);
impl SourcePreflightProvider for PipewirePreflight {
    fn query(
        &self,
        caller: &str,
        request: &SourcePreflightRequest,
    ) -> Result<SourcePreflightResponse, amaterasu_module_sdk::abi::Status> {
        let controller = self.0.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => amaterasu_media::MEDIA_BUSY,
            std::sync::TryLockError::Poisoned(_) => amaterasu_media::MEDIA_BACKEND_ERROR,
        })?;
        Ok(controller.preflight(caller, request))
    }
}

struct CaptureModule {
    runtime: RuntimeClient,
    controller: native::Shared<NativeBackend>,
    _factory: Box<native::Factory<NativeBackend>>,
    _preflight_factory: Box<SourcePreflightFactory<PipewirePreflight>>,
    epoch: Instant,
    pending_opens: Vec<PendingOpen>,
    polled_request: Option<PolledRequest>,
    shutting_down: bool,
}
#[derive(Clone)]
struct PolledRequest {
    id: u64,
    caller: String,
    service: String,
    payload: serde_json::Value,
}
#[derive(Clone)]
struct PendingOpen {
    request: u64,
    caller: String,
    operation: Operation,
}

impl ManagedModule for CaptureModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("PipeWire settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        settings.validate().map_err(str::to_owned)?;
        let mut backend = NativeBackend::new();
        if settings.remember_source {
            backend.configure_persistence().map_err(str::to_owned)?;
        }
        let controller = Arc::new(Mutex::new(Controller::new(backend, settings)));
        let factory = native::Factory::new(controller.clone());
        // SAFETY: the retained inner factory binds controller metadata only;
        // its preflight inspects registry metadata without opening a source.
        let preflight_factory = unsafe {
            SourcePreflightFactory::new(
                &factory.api,
                Arc::new(PipewirePreflight(controller.clone())),
            )
        };
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        // SAFETY: boxed factory remains stable until destruction; every independent
        // binding retains its own shared controller after factory release.
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
            epoch: Instant::now(),
            pending_opens: Vec::new(),
            polled_request: None,
            shutting_down: false,
        })
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        self.shutting_down = true;
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.shutting_down = true;
        match self.controller.try_lock() {
            Ok(mut controller) => {
                controller.shutdown();
                Ok(controller.retired()
                    && self.pending_opens.is_empty()
                    && self.polled_request.is_none())
            }
            Err(std::sync::TryLockError::WouldBlock) => Ok(false),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                Err("PipeWire capture controller poisoned during shutdown".into())
            }
        }
    }

    fn tick(&mut self) -> ModuleResult<()> {
        let now = self.epoch.elapsed().as_millis().min(u64::MAX as u128) as u64;
        // Acquire before polling or removing pending requests: a concurrent
        // native preflight must not block the host or strand dequeued work.
        let shared = self.controller.clone();
        let mut controller = match shared.try_lock() {
            Ok(controller) => controller,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(()),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err("PipeWire controller poisoned".into());
            }
        };
        controller.refresh(now);
        let mut cancelled = Vec::new();
        let mut pending_opens = std::mem::take(&mut self.pending_opens).into_iter();
        while let Some(pending) = pending_opens.next() {
            let outcome = self.runtime.send(Command::CheckRequest {
                request: pending.request,
            });
            let result = match outcome {
                Ok(Response::Ok) => self.dispatch_locked(
                    &mut controller,
                    pending.request,
                    pending.caller.clone(),
                    pending.operation.clone(),
                    now,
                ),
                Ok(Response::Error { code, .. })
                    if code == "not_found" || code == "request_ended" =>
                {
                    cancelled.push(pending.clone());
                    Ok(())
                }
                Ok(response) => Err(format!("PipeWire request check failed: {response:?}")),
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                // Keep all unresolved identities for retry or shutdown rather than
                // silently dropping the remaining queue after a broker failure.
                self.pending_opens.extend(cancelled);
                self.pending_opens.push(pending);
                self.pending_opens.extend(pending_opens);
                return Err(error);
            }
        }
        for pending in cancelled {
            self.cancel_unreferenced_open_locked(
                &mut controller,
                &pending.caller,
                &pending.operation,
            );
        }
        for _ in 0..8 {
            let request = match self.polled_request.take() {
                Some(request) => request,
                None => match self.runtime.send(Command::Poll {})? {
                    Response::Pending => break,
                    Response::Request {
                        id,
                        caller,
                        service,
                        payload,
                        ..
                    } => PolledRequest {
                        id,
                        caller,
                        service,
                        payload,
                    },
                    response => return Err(format!("PipeWire poll failed: {response:?}")),
                },
            };
            match self.runtime.send(Command::CheckRequest {
                request: request.id,
            }) {
                Ok(Response::Ok) => {}
                Ok(Response::Error { code, .. })
                    if code == "not_found" || code == "request_ended" =>
                {
                    continue;
                }
                result => {
                    self.polled_request = Some(request);
                    return Err(format!("PipeWire request check failed: {result:?}"));
                }
            }
            let result = if request.service == SERVICE
                && let Ok(operation) = serde_json::from_value::<Operation>(request.payload.clone())
            {
                self.dispatch_locked(
                    &mut controller,
                    request.id,
                    request.caller.clone(),
                    operation,
                    now,
                )
            } else {
                self.runtime.send(Command::Complete {request:request.id,payload:json!({"ok":false,"error":{"code":"invalid_request","message":"invalid_request"}})}).and_then(|response| match response {
                    Response::Ok => Ok(()),
                    Response::Error { code, .. } if code == "not_found" || code == "request_ended" => Ok(()),
                    response => Err(format!("PipeWire completion failed: {response:?}")),
                })
            };
            if let Err(error) = result {
                self.polled_request = Some(request);
                return Err(error);
            }
        }
        Ok(())
    }
}

impl CaptureModule {
    #[cfg(test)]
    fn dispatch(
        &mut self,
        id: u64,
        caller: String,
        operation: Operation,
        now: u64,
    ) -> ModuleResult<()> {
        let shared = self.controller.clone();
        let mut controller = shared.lock().map_err(|_| "PipeWire controller poisoned")?;
        self.dispatch_locked(&mut controller, id, caller, operation, now)
    }

    fn dispatch_locked(
        &mut self,
        controller: &mut Controller<NativeBackend>,
        id: u64,
        caller: String,
        operation: Operation,
        now: u64,
    ) -> ModuleResult<()> {
        let reply = if self.shutting_down
            && matches!(
                operation,
                Operation::Open { .. } | Operation::SelectSource { .. }
            ) {
            Err("capture_shutdown")
        } else {
            controller.handle(&caller, operation.clone(), now)
        };
        if matches!(
            reply,
            Err("source_restore_pending" | "named_source_pending" | "source_retirement_pending")
        ) {
            if self.pending_opens.len() < 8 {
                self.pending_opens.push(PendingOpen {
                    request: id,
                    caller,
                    operation,
                });
                return Ok(());
            }
            self.cancel_unreferenced_open_locked(controller, &caller, &operation);
        }
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
            controller.rollback(&caller, created);
            if !matches!(completion, Ok(Response::Error { ref code, .. }) if code == "request_ended" || code == "not_found")
            {
                return Err(format!("PipeWire completion failed: {completion:?}"));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn cancel_unreferenced_open(
        &mut self,
        caller: &str,
        operation: &Operation,
    ) -> ModuleResult<()> {
        let shared = self.controller.clone();
        let mut controller = shared.lock().map_err(|_| "PipeWire controller poisoned")?;
        self.cancel_unreferenced_open_locked(&mut controller, caller, operation);
        Ok(())
    }

    fn cancel_unreferenced_open_locked(
        &mut self,
        controller: &mut Controller<NativeBackend>,
        caller: &str,
        operation: &Operation,
    ) {
        let operation_id = match operation {
            Operation::Open {
                output,
                operation_id: Some(id),
                ..
            } if output.starts_with("kwin:") => Some(id),
            _ => None,
        };
        let still_referenced = operation_id.is_some_and(|id| {
            self.pending_opens.iter().any(|pending| {
                pending.caller == caller
                    && matches!(&pending.operation,
                Operation::Open { output, operation_id: Some(other), .. } if output.starts_with("kwin:") && other == id)
            })
        });
        if operation_id.is_some() && !still_referenced {
            controller.cancel_pending_open(caller, operation);
        }
        if matches!(operation, Operation::Open { output, .. } if output == "portal-selected-session-source") && !self.pending_opens.iter().any(|pending| {
            pending.caller == caller && matches!(&pending.operation, Operation::Open { output, .. } if output == "portal-selected-session-source")
        })
        {
            controller.cancel_pending_restore(caller);
        }
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
