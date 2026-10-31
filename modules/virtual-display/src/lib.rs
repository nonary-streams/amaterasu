//! Caller-owned temporary monitors for the Sunshine Windows virtual display driver.
#[cfg(any(not(amaterasu_libvirtualdisplay), test))]
mod backend;
#[cfg(any(not(amaterasu_libvirtualdisplay), test))]
mod journal;
#[cfg(target_os = "linux")]
pub mod linux_control;
#[cfg(amaterasu_libvirtualdisplay)]
mod linux_manager;
#[cfg(any(not(amaterasu_libvirtualdisplay), test))]
mod manager;
mod model;
#[cfg_attr(not(windows), allow(dead_code))]
mod protocol;
mod scale;
mod service_adapter;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
#[cfg(not(amaterasu_libvirtualdisplay))]
use backend::NativeDriver;
#[cfg(not(amaterasu_libvirtualdisplay))]
use journal::Journal;
#[cfg(not(amaterasu_libvirtualdisplay))]
use manager::Manager;
use model::{Operation, Settings, failure};

use amaterasu_module_sdk::virtual_display::SERVICE;
use amaterasu_module_sdk::{
    requests::IncomingRequest, service::ServiceError, virtual_display_service as display,
};

struct VirtualDisplayModule {
    runtime: RuntimeClient,
    display_adapter: service_adapter::Adapter,
    #[cfg(not(amaterasu_libvirtualdisplay))]
    manager: Manager<NativeDriver>,
    #[cfg(amaterasu_libvirtualdisplay)]
    manager: linux_manager::Manager,
}

impl ManagedModule for VirtualDisplayModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("virtual display settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid virtual display settings")?;
        let legacy_registered = match runtime.send(Command::Register {
            service: SERVICE.into(),
        })? {
            Response::Ok => true,
            Response::Error { code, .. } if code == "permission_denied" => false,
            response => {
                return Err(format!(
                    "legacy virtual display registration failed: {response:?}"
                ));
            }
        };
        let typed_registered = match display::register(&runtime) {
            Ok(()) => true,
            Err(ServiceError::Broker { code, .. }) if code == "permission_denied" => false,
            Err(error) => return Err(format!("virtual display API registration failed: {error}")),
        };
        if !legacy_registered && !typed_registered {
            return Err("no virtual display service was approved for this provider".into());
        }
        #[cfg(amaterasu_libvirtualdisplay)]
        let manager = linux_manager::Manager::start(settings)?;
        #[cfg(not(amaterasu_libvirtualdisplay))]
        let manager = if settings.enabled && cfg!(windows) {
            Manager::with_journal(settings, NativeDriver::new(), Journal::open_default()?)?
        } else {
            Manager::new(settings, NativeDriver::new())
        };
        Ok(Self {
            runtime,
            manager,
            display_adapter: service_adapter::Adapter::default(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.manager.tick();
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                initiator,
                service,
                payload,
                remaining_ms,
            } => {
                let request = IncomingRequest {
                    id,
                    caller,
                    initiator,
                    service,
                    payload,
                    remaining_ms,
                };
                if request.service == display::SERVICE {
                    handle_display_api(
                        &self.runtime,
                        &mut self.manager,
                        &mut self.display_adapter,
                        request,
                    )
                } else {
                    handle_legacy(
                        &self.runtime,
                        &mut self.manager,
                        &mut self.display_adapter,
                        request,
                    )
                }
            }
            _ => Err("virtual display polling failed".into()),
        }
    }
}

amaterasu_module_sdk::export_module!(VirtualDisplayModule);

fn handle_legacy<B: service_adapter::Backend>(
    runtime: &RuntimeClient,
    manager: &mut B,
    adapter: &mut service_adapter::Adapter,
    request: IncomingRequest<serde_json::Value>,
) -> ModuleResult<()> {
    let operation = serde_json::from_value::<Operation>(request.payload);
    let mut admitted_operation = None;
    let (response, admission) = match operation {
        Ok(operation) => {
            if matches!(
                runtime.send(Command::CheckRequest {
                    request: request.id
                })?,
                Response::Ok
            ) {
                let outcome = manager.admit_with_effect(&request.caller, operation.clone());
                if outcome.effect == service_adapter::MutationEffect::Attempted {
                    adapter.observe_legacy_mutation(&request.caller, &operation);
                }
                admitted_operation = Some(operation);
                (outcome.response, outcome.admission)
            } else {
                (failure("request_ended"), None)
            }
        }
        Err(_) => (failure("invalid_request"), None),
    };
    let completion = runtime.send(Command::Complete {
        request: request.id,
        payload: response,
    });
    if !matches!(&completion, Ok(Response::Ok)) {
        if admission.is_some()
            && let Some(operation) = admitted_operation.as_ref()
        {
            adapter.observe_legacy_mutation(&request.caller, operation);
        }
        manager.completion_lost(admission);
    }
    match completion? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        _ => Err("virtual display completion failed".into()),
    }
}

fn handle_display_api<B: service_adapter::Backend>(
    runtime: &RuntimeClient,
    manager: &mut B,
    adapter: &mut service_adapter::Adapter,
    request: IncomingRequest<serde_json::Value>,
) -> ModuleResult<()> {
    let result = display::handle(runtime, &mut adapter.invocation(manager), &request);
    match result {
        Ok(()) => Ok(()),
        Err(ServiceError::Broker { code, .. })
            if code == "request_ended" || code == "not_found" || code == "cancelled" =>
        {
            Ok(())
        }
        Err(ServiceError::InvalidPayload(_) | ServiceError::WrongMethod) => runtime
            .complete_serialized(
                request.id,
                &display::ReplyEnvelope {
                    reply: display::Reply::InvalidRequest,
                },
            ),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod startup_tests;
