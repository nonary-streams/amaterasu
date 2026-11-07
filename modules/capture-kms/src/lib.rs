//! Native physical KMS readback into owned BGRA frames.
//! Native modules are trusted code; runtime API grants are not a native-code sandbox.
mod backend;
mod controller;
mod model;
mod native;

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
                Ok(true)
            }
            Err(std::sync::TryLockError::WouldBlock) => Ok(false),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                Err("KMS capture controller poisoned during shutdown".into())
            }
        }
    }

    fn tick(&mut self) -> ModuleResult<()> {
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
                        let mut controller = self
                            .controller
                            .lock()
                            .map_err(|_| "KMS capture controller poisoned")?;
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
                        let _ = self
                            .controller
                            .lock()
                            .map_err(|_| "KMS capture controller poisoned")?
                            .close(&caller, stream);
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
