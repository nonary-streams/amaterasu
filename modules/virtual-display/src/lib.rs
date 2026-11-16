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
mod retained_mode;
mod scale;
mod stream_issues;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
#[cfg(not(amaterasu_libvirtualdisplay))]
use backend::NativeDriver;
#[cfg(not(amaterasu_libvirtualdisplay))]
use journal::Journal;
#[cfg(not(amaterasu_libvirtualdisplay))]
use manager::Manager;
use model::{Operation, Settings, failure};

use amaterasu_module_sdk::virtual_display::SERVICE;

struct VirtualDisplayModule {
    runtime: RuntimeClient,
    #[cfg(not(amaterasu_libvirtualdisplay))]
    manager: Manager<NativeDriver>,
    #[cfg(amaterasu_libvirtualdisplay)]
    manager: linux_manager::Manager,
    stream_issues: stream_issues::StreamIssues,
}

impl ManagedModule for VirtualDisplayModule {
    #[cfg(amaterasu_libvirtualdisplay)]
    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.manager.quiesce()
    }

    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("virtual display settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid virtual display settings")?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        #[cfg(amaterasu_libvirtualdisplay)]
        let manager = linux_manager::Manager::start(settings)?;
        #[cfg(not(amaterasu_libvirtualdisplay))]
        let manager = if settings.enabled && cfg!(windows) {
            Manager::with_journal(settings, NativeDriver::new(), Journal::open_default()?)?
        } else {
            Manager::new(settings, NativeDriver::new())
        };
        let mut stream_issues = stream_issues::StreamIssues::default();
        stream_issues.enable();
        Ok(Self {
            runtime,
            manager,
            stream_issues,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.manager.tick();
        #[cfg(amaterasu_libvirtualdisplay)]
        self.stream_issues.adopt(self.manager.drain_stream_issues());
        let result = self.serve();
        let runtime = &self.runtime;
        self.stream_issues
            .publish(&mut |command| runtime.send(command));
        result
    }
}

impl VirtualDisplayModule {
    fn serve(&mut self) -> ModuleResult<()> {
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => {
                let operation = serde_json::from_value::<Operation>(payload);
                let stream = operation
                    .as_ref()
                    .ok()
                    .and_then(stream_issues::StreamOperation::of);
                let (response, admission) = match operation {
                    Ok(operation) => {
                        // Check the provider-owned request immediately before native effects.
                        if matches!(
                            self.runtime.send(Command::CheckRequest { request: id })?,
                            Response::Ok
                        ) {
                            self.manager.admit(&caller, operation)
                        } else {
                            (failure("request_ended"), None)
                        }
                    }
                    Err(_) => (failure("invalid_request"), None),
                };
                self.stream_issues.admitted(stream, &response);
                let completion = self.runtime.send(Command::Complete {
                    request: id,
                    payload: response,
                });
                if !matches!(&completion, Ok(Response::Ok)) {
                    self.manager.completion_lost(admission);
                }
                match completion? {
                    Response::Ok => Ok(()),
                    Response::Error { code, .. }
                        if code == "request_ended" || code == "not_found" =>
                    {
                        Ok(())
                    }
                    _ => Err("virtual display completion failed".into()),
                }
            }
            _ => Err("virtual display polling failed".into()),
        }
    }
}

// Driver and compositor operations belong to the service thread. The host's
// shared tick only wakes it; retirement joins it before the module can unload.
type VirtualDisplayService =
    amaterasu_module_sdk::service_worker::ServiceWorker<VirtualDisplayModule>;
amaterasu_module_sdk::export_module!(VirtualDisplayService);

#[cfg(test)]
mod worker_tests;
