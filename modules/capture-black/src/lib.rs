//! Explicit generated-black capture for application-independent input streams.
//! This provider opens no displays, sockets, audio devices or input devices.
mod controller;
mod model;
mod native;

use amaterasu_media::host_monotonic_micros;
use amaterasu_module_sdk::capture::{
    SourcePreflightFactory, SourcePreflightProvider, SourcePreflightRequest,
    SourcePreflightResponse,
};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use controller::Controller;
use model::{Operation, SERVICE, Settings, failure};
use std::sync::{Arc, Mutex};

const MAX_PENDING_CLOSES: usize = 32;

struct PendingClose {
    request: Option<u64>,
    caller: String,
    stream: u64,
    reply: Option<serde_json::Value>,
}

struct BlackPreflight(native::SharedController);
impl SourcePreflightProvider for BlackPreflight {
    fn query(
        &self,
        _caller: &str,
        request: &SourcePreflightRequest,
    ) -> Result<SourcePreflightResponse, amaterasu_module_sdk::abi::Status> {
        let controller = self.0.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => amaterasu_media::MEDIA_BUSY,
            std::sync::TryLockError::Poisoned(_) => amaterasu_media::MEDIA_BACKEND_ERROR,
        })?;
        Ok(controller.preflight(request))
    }
}

struct BlackCapture {
    runtime: RuntimeClient,
    controller: native::SharedController,
    pending_closes: Vec<PendingClose>,
    _factory: Box<native::Factory>,
    _preflight_factory: Box<SourcePreflightFactory<BlackPreflight>>,
}

impl ManagedModule for BlackCapture {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("black capture settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        let controller = Arc::new(Mutex::new(Controller::new(settings.enabled)));
        let factory = native::Factory::new(controller.clone());
        // SAFETY: the retained inner factory binds only controller metadata;
        // BlackPreflight reads bounded state and never opens native resources.
        let preflight_factory = unsafe {
            SourcePreflightFactory::new(&factory.api, Arc::new(BlackPreflight(controller.clone())))
        };
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        // SAFETY: this stable boxed factory lives until the module is unloaded;
        // broker-owned bindings independently retain the shared controller.
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
            pending_closes: Vec::new(),
            _factory: factory,
            _preflight_factory: preflight_factory,
        })
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        self.controller
            .lock()
            .map_err(|_| "black capture controller poisoned")?
            .shutdown();
        Ok(())
    }
    fn quiesce(&mut self) -> ModuleResult<bool> {
        self._factory
            .retirement_ready()
            .map_err(|status| format!("capture resources did not retire: {status}"))
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.controller
            .lock()
            .map_err(|_| "black capture controller poisoned")?
            .maintain(host_monotonic_micros());
        self.poll_pending_closes()?;
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
                _ => return Err("black capture polling failed".into()),
            };
            if !matches!(
                self.runtime.send(Command::CheckRequest { request: id })?,
                Response::Ok
            ) {
                continue;
            }
            let operation = serde_json::from_value::<Operation>(payload);
            let close_stream = if service == SERVICE {
                operation
                    .as_ref()
                    .ok()
                    .and_then(|operation| match operation {
                        Operation::Close { stream } => Some(*stream),
                        _ => None,
                    })
            } else {
                None
            };
            let (payload, created) = {
                let mut controller = self
                    .controller
                    .lock()
                    .map_err(|_| "black capture controller poisoned")?;
                let response = if close_stream.is_some()
                    && self.pending_closes.len() >= MAX_PENDING_CLOSES
                {
                    Err("resource_exhausted")
                } else {
                    match operation {
                        Ok(operation) if service == SERVICE => controller.handle(
                            &caller,
                            operation,
                            host_monotonic_micros(),
                            &mut || match self.runtime.send(Command::CheckRequest { request: id }) {
                                Ok(Response::Ok) => Ok(()),
                                _ => Err("request_ended"),
                            },
                        ),
                        Ok(_) => Err("invalid_service"),
                        Err(_) => Err("invalid_request"),
                    }
                };
                response.unwrap_or_else(|code| (failure(code), None))
            };
            if let Some(stream) = close_stream
                && payload["error"]["code"].as_str() == Some("busy")
            {
                // Close was admitted, so cancellation of its broker waiter
                // must not cancel retirement of this exact stream.
                self.pending_closes.push(PendingClose {
                    request: Some(id),
                    caller,
                    stream,
                    reply: None,
                });
                continue;
            }
            let completion = self.runtime.send(Command::Complete {
                request: id,
                payload,
            });
            if !matches!(completion, Ok(Response::Ok)) {
                self.controller
                    .lock()
                    .map_err(|_| "black capture controller poisoned")?
                    .rollback_open(&caller, created, host_monotonic_micros());
                if !matches!(completion,Ok(Response::Error {ref code,..}) if code=="request_ended"||code=="not_found")
                {
                    return Err("black capture completion failed".into());
                }
            }
        }
        self.poll_pending_closes()?;
        Ok(())
    }
}

impl BlackCapture {
    fn poll_pending_closes(&mut self) -> ModuleResult<()> {
        let now_us = host_monotonic_micros();
        let pending = std::mem::take(&mut self.pending_closes);
        let mut retry = Vec::with_capacity(pending.len());
        let mut first_failure = None;
        let mut pending = pending.into_iter();

        while let Some(mut close) = pending.next() {
            let mut activity_confirmed = close.reply.is_some() || close.request.is_none();
            if close.reply.is_none()
                && let Some(request) = close.request
            {
                match self.runtime.send(Command::CheckRequest { request }) {
                    Ok(Response::Ok) => activity_confirmed = true,
                    Ok(Response::Error { code, .. })
                        if code == "request_ended" || code == "not_found" =>
                    {
                        close.request = None;
                    }
                    response => {
                        first_failure.get_or_insert_with(|| {
                            format!("black capture close activity check failed: {response:?}")
                        });
                    }
                }
            }

            if close.reply.is_none() {
                let result = match self.controller.lock() {
                    Ok(mut controller) => controller.close(&close.caller, close.stream, now_us),
                    Err(_) => {
                        retry.push(close);
                        retry.extend(pending);
                        self.pending_closes = retry;
                        return Err("black capture controller poisoned".into());
                    }
                };
                match result {
                    Ok(()) => close.reply = Some(serde_json::json!({"ok":true})),
                    Err("busy") => (),
                    Err(code) => close.reply = Some(failure(code)),
                }
            }

            match (close.request, close.reply.as_ref(), activity_confirmed) {
                (Some(request), Some(payload), true) => {
                    match self.runtime.send(Command::Complete {
                        request,
                        payload: payload.clone(),
                    }) {
                        Ok(Response::Ok) => (),
                        Ok(Response::Error { code, .. })
                            if code == "request_ended" || code == "not_found" => {}
                        response => {
                            first_failure.get_or_insert_with(|| {
                                format!("black capture close completion failed: {response:?}")
                            });
                            retry.push(close);
                        }
                    }
                }
                (None, Some(_), _) => (),
                _ => retry.push(close),
            }
        }

        self.pending_closes = retry;
        first_failure.map_or(Ok(()), Err)
    }
}

impl Drop for BlackCapture {
    fn drop(&mut self) {
        self.controller
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .shutdown();
    }
}

amaterasu_module_sdk::export_module!(BlackCapture);

#[cfg(test)]
#[path = "module_tests.rs"]
mod module_tests;
