//! Bounded caller-owned UDP transport through borrowed native media admission.
mod controller;
mod datagrams;
mod model;
mod native;
mod stream;

#[cfg(feature = "test-support")]
pub mod test_support;

use amaterasu_module_sdk::{
    Command, ManagedModule, ModuleResult, Response, RuntimeClient, network::Network,
};
use controller::{ControlReply, Controller};
use model::{Operation, Settings};
use serde_json::json;
use std::sync::{Arc, Mutex};

const SERVICE: &str = "amaterasu.media-transport/1";

struct MediaTransportModule {
    runtime: RuntimeClient,
    controller: native::Shared,
    _factory: Box<native::Factory>,
    _plan_factory: Option<Box<native::plan::Factory>>,
}

impl ManagedModule for MediaTransportModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        Self::start_configured(runtime, None)
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.controller
            .lock()
            .map_err(|_| "transport state unavailable")?
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
                let result = if service == SERVICE {
                    serde_json::from_value::<Operation>(payload)
                        .map_err(|_| "invalid_request")
                        .and_then(|operation| {
                            self.controller
                                .lock()
                                .map_err(|_| "transport_state_failed")?
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
                    // A lost/cancelled open response cannot leave a hidden UDP sender.
                    let _ = self
                        .controller
                        .lock()
                        .map(|mut controller| controller.close(&caller, stream));
                }
                Ok(())
            }
            _ => Err("media transport poll failed".into()),
        }
    }
}

impl MediaTransportModule {
    fn start_configured(
        runtime: RuntimeClient,
        selected_network: Option<Network>,
    ) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("media transport settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload.clone())
            .map_err(|_| "invalid media transport settings")?;
        settings.validate()?;
        let network = match selected_network {
            Some(network) => network,
            None => Network::from_settings(&runtime, &payload)?,
        };
        let plan_service = settings.plan_service.clone();
        let controller = Arc::new(Mutex::new(Controller::with_network(settings, network)));
        let factory = native::Factory::new(controller.clone());
        let plan_factory = plan_service
            .as_ref()
            .map(|_| native::plan::Factory::new(controller.clone()));
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        // SAFETY: boxed factory and synchronized controller live until unload.
        // Signed dependency ordering requires consumers to release all native
        // bindings before provider unload; grants are not a native-code sandbox.
        unsafe {
            amaterasu_module_sdk::transport::register_media(&runtime, SERVICE, &factory.api)?;
        }
        if let (Some(service), Some(factory)) = (plan_service, &plan_factory) {
            // SAFETY: factory is retained for module lifetime and callbacks borrow synchronously.
            unsafe {
                amaterasu_module_sdk::video_framing::register_plan_transport(
                    &runtime,
                    &service,
                    &factory.api,
                )
            }?;
        }
        Ok(Self {
            runtime,
            controller,
            _factory: factory,
            _plan_factory: plan_factory,
        })
    }
}

impl Drop for MediaTransportModule {
    fn drop(&mut self) {
        if let Ok(mut controller) = self.controller.lock() {
            controller.shutdown();
        }
    }
}

#[cfg(not(feature = "test-support"))]
amaterasu_module_sdk::export_module!(MediaTransportModule);

#[cfg(test)]
mod module_tests;
