//! Native physical KMS readback into owned BGRA frames.
//! Native modules are trusted code; runtime API grants are not a native-code sandbox.
mod backend;
mod controller;
#[cfg(all(test, feature = "isolated-helper", target_os = "linux", amaterasu_kms))]
mod helper_client;
#[cfg(all(test, feature = "isolated-helper", target_os = "linux", amaterasu_kms))]
mod helper_process;
#[cfg(all(test, feature = "isolated-helper", target_os = "linux", amaterasu_kms))]
mod metadata_task;
mod model;
mod native;
#[cfg(all(test, feature = "isolated-helper", target_os = "linux", amaterasu_kms))]
mod open_task;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use backend::NativeBackend;
use controller::Controller;
use model::{CaptureError, Operation};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

const SERVICE: &str = "amaterasu.capture-kms/1";
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
    shutting_down: bool,
}

impl ManagedModule for CaptureModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("KMS capture settings unavailable".into());
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
                self._factory
                    .retirement_ready()
                    .map_err(|status| format!("capture resources did not retire: {status}"))
            }
            Err(std::sync::TryLockError::WouldBlock) => Ok(false),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                Err("KMS capture controller poisoned during shutdown".into())
            }
        }
    }

    fn tick(&mut self) -> ModuleResult<()> {
        for _ in 0..8 {
            // A native frame read can hold the controller while a driver call
            // waits. Do not dequeue work until it can be handled without
            // blocking the host's cooperative shutdown loop.
            let mut controller = match self.controller.try_lock() {
                Ok(controller) => controller,
                Err(std::sync::TryLockError::WouldBlock) => return Ok(()),
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err("KMS capture controller poisoned".into());
                }
            };
            let (id, caller, service, payload) = match self.runtime.send(Command::Poll {})? {
                Response::Pending => break,
                Response::Request {
                    id,
                    caller,
                    service,
                    payload,
                    ..
                } => (id, caller, service, payload),
                response => return Err(format!("KMS capture polling failed: {response:?}")),
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
                    Ok(Operation::Open { .. }) if self.shutting_down => {
                        CaptureError::new("capture_shutdown", "KMS capture is shutting down")
                            .payload()
                    }
                    Ok(operation) => {
                        let reply = controller.handle(&caller, operation, &mut || match self
                            .runtime
                            .send(Command::CheckRequest { request: id })
                        {
                            Ok(Response::Ok) => Ok(()),
                            _ => Err(CaptureError::new("cancelled", "KMS capture request ended")),
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
                        let _ = controller.close(&caller, stream);
                    }
                    if !matches!(result, Ok(Response::Error { ref code, .. }) if code == "request_ended" || code == "not_found")
                    {
                        return Err(format!("KMS capture completion failed: {result:?}"));
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

// This router is deliberately test-only while the isolated provider's native
// composition and security gates remain under review.
#[cfg(all(test, feature = "isolated-helper", target_os = "linux", amaterasu_kms))]
mod isolated_router {
    use super::{CaptureError, Command, ModuleResult, Operation, Response, SERVICE};
    use crate::model::CaptureResult;
    use serde_json::Value;
    use std::collections::BTreeMap;

    const MAX_REQUESTS: usize = 8;

    pub(crate) struct Completion {
        pub id: u64,
        pub caller: String,
        pub payload: Value,
        pub opened_stream: Option<u64>,
    }

    pub(crate) trait Controller {
        fn submit(
            &mut self,
            id: u64,
            caller: String,
            operation: Operation,
        ) -> CaptureResult<Option<Value>>;
        fn poll(
            &mut self,
            active: &mut impl FnMut(u64) -> CaptureResult<bool>,
        ) -> CaptureResult<Vec<Completion>>;
        fn cancel(&mut self, id: u64);
        fn rollback(&mut self, caller: &str, stream: u64);
        fn shutdown(&mut self) -> CaptureResult<bool>;
    }

    struct Request {
        id: u64,
        caller: String,
        service: String,
        payload: Value,
    }

    struct Pending {
        caller: String,
        completion: Option<Completion>,
    }

    pub(crate) struct Router<C> {
        controller: C,
        raw: Option<Request>,
        pending: BTreeMap<u64, Pending>,
        shutting_down: bool,
    }

    fn active(
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        id: u64,
    ) -> CaptureResult<bool> {
        match send(Command::CheckRequest { request: id }) {
            Ok(Response::Ok) => Ok(true),
            Ok(Response::Error { code, .. }) if code == "not_found" || code == "request_ended" => {
                Ok(false)
            }
            result => Err(CaptureError::new(
                "broker_unavailable",
                format!("request check failed: {result:?}"),
            )),
        }
    }

    impl<C: Controller> Router<C> {
        pub fn new(controller: C) -> Self {
            Self {
                controller,
                raw: None,
                pending: BTreeMap::new(),
                shutting_down: false,
            }
        }

        pub fn tick(
            &mut self,
            send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        ) -> ModuleResult<()> {
            // Drive retirement even if broker checks currently fail. The driver
            // retains worker and child owners when its poll reports an error.
            let completions = self
                .controller
                .poll(&mut |id| active(send, id))
                .map_err(|error| error.message)?;
            for completion in completions {
                if let Some(pending) = self.pending.get_mut(&completion.id) {
                    if pending.caller == completion.caller && pending.completion.is_none() {
                        pending.completion = Some(completion);
                        continue;
                    }
                }
                if let Some(stream) = completion.opened_stream {
                    self.controller.rollback(&completion.caller, stream);
                }
            }

            for id in self.pending.keys().copied().collect::<Vec<_>>() {
                match active(send, id) {
                    Ok(false) => {
                        let pending = self.pending.remove(&id).expect("pending request");
                        self.controller.cancel(id);
                        if let Some(completion) = pending.completion
                            && let Some(stream) = completion.opened_stream
                        {
                            self.controller.rollback(&completion.caller, stream);
                        }
                    }
                    Ok(true) => {
                        let Some(completion) = self
                            .pending
                            .get(&id)
                            .and_then(|pending| pending.completion.as_ref())
                        else {
                            continue;
                        };
                        let result = send(Command::Complete {
                            request: id,
                            payload: completion.payload.clone(),
                        });
                        match result {
                            Ok(Response::Ok) => {
                                self.pending.remove(&id);
                            }
                            Ok(Response::Error { code, .. })
                                if code == "request_ended" || code == "not_found" =>
                            {
                                let pending = self.pending.remove(&id).expect("pending request");
                                if let Some(completion) = pending.completion
                                    && let Some(stream) = completion.opened_stream
                                {
                                    self.controller.rollback(&completion.caller, stream);
                                }
                            }
                            result => return Err(format!("KMS completion failed: {result:?}")),
                        }
                    }
                    Err(error) => return Err(error.message),
                }
            }

            if self.shutting_down || self.pending.len() >= MAX_REQUESTS {
                return Ok(());
            }
            let request = match self.raw.take() {
                Some(request) => request,
                None => match send(Command::Poll {})? {
                    Response::Pending => return Ok(()),
                    Response::Request {
                        id,
                        caller,
                        service,
                        payload,
                        ..
                    } => Request {
                        id,
                        caller,
                        service,
                        payload,
                    },
                    response => return Err(format!("KMS polling failed: {response:?}")),
                },
            };
            match active(send, request.id) {
                Ok(false) => return Ok(()),
                Err(error) => {
                    self.raw = Some(request);
                    return Err(error.message);
                }
                Ok(true) => {}
            }
            let completion = if request.service != SERVICE {
                Some(CaptureError::new("invalid_service", "unsupported capture service").payload())
            } else {
                match serde_json::from_value::<Operation>(request.payload.clone()) {
                    Ok(operation) => {
                        match self
                            .controller
                            .submit(request.id, request.caller.clone(), operation)
                        {
                            Ok(result) => result,
                            Err(error) => Some(error.payload()),
                        }
                    }
                    Err(error) => {
                        Some(CaptureError::new("invalid_request", error.to_string()).payload())
                    }
                }
            };
            self.pending.insert(
                request.id,
                Pending {
                    caller: request.caller.clone(),
                    completion: completion.map(|payload| Completion {
                        id: request.id,
                        caller: request.caller,
                        payload,
                        opened_stream: None,
                    }),
                },
            );
            Ok(())
        }

        pub fn prepare_shutdown(&mut self) {
            self.shutting_down = true;
            for (&id, pending) in &mut self.pending {
                self.controller.cancel(id);
                if let Some(completion) = pending.completion.take()
                    && let Some(stream) = completion.opened_stream
                {
                    self.controller.rollback(&completion.caller, stream);
                }
                pending.completion = Some(Completion {
                    id,
                    caller: pending.caller.clone(),
                    payload: CaptureError::new("capture_shutdown", "KMS capture is shutting down")
                        .payload(),
                    opened_stream: None,
                });
            }
            if let Some(request) = self.raw.take() {
                self.pending.insert(
                    request.id,
                    Pending {
                        caller: request.caller.clone(),
                        completion: Some(Completion {
                            id: request.id,
                            caller: request.caller,
                            payload: CaptureError::new(
                                "capture_shutdown",
                                "KMS capture is shutting down",
                            )
                            .payload(),
                            opened_stream: None,
                        }),
                    },
                );
            }
        }

        pub fn quiesce(&mut self) -> CaptureResult<bool> {
            self.prepare_shutdown();
            Ok(self.controller.shutdown()? && self.raw.is_none() && self.pending.is_empty())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{frame_layout, validate_operation_id, validate_output_id};

    #[test]
    fn output_ids_require_a_specific_card_connector_crtc_and_plane() {
        assert!(validate_output_id("kms:card1/conn=34/crtc=56/plane=78").is_ok());
        for invalid in [
            "",
            "kms:card0/conn=1/crtc=2",
            "kms:card0/conn=1/crtc=0/plane=3",
            "kms:card0/conn=1/plane=3/crtc=2/extra=4",
            "kms:card../conn=1/crtc=2/plane=3",
            "kms:card00/conn=1/crtc=2/plane=3",
            "kms:card0/conn=01/crtc=2/plane=3",
        ] {
            assert!(validate_output_id(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn frame_layout_rejects_invalid_or_unbounded_dimensions() {
        assert_eq!(frame_layout(1920, 1080).unwrap(), (7680, 8_294_400));
        assert!(frame_layout(0, 1).is_err());
        assert!(frame_layout(8193, 1).is_err());
        assert!(frame_layout(u32::MAX, u32::MAX).is_err());
    }

    #[test]
    fn operation_ids_reject_controls_and_unbounded_values() {
        assert!(validate_operation_id("operation-123").is_ok());
        assert!(validate_operation_id("bad\nid").is_err());
        assert!(validate_operation_id(&"x".repeat(129)).is_err());
    }
}
