//! Application process-tree ownership for trusted administrative callers.
mod command_paths;
mod commands;
mod containment;
mod lifecycle;
mod output;
mod processes;
mod protocol;
mod proton_hook;
mod steam_tracking;
pub use processes::ProcessManager;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};

pub const SERVICE: &str = "amaterasu.application/1";

pub struct ApplicationModule {
    runtime: RuntimeClient,
    processes: ProcessManager,
}

impl ManagedModule for ApplicationModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            processes: ProcessManager::default(),
        })
    }

    fn prepare_shutdown(&mut self, remaining_ms: u64) -> ModuleResult<()> {
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(remaining_ms))
            .ok_or_else(|| {
                "application shutdown deadline exceeds monotonic clock bounds".to_owned()
            })?;
        self.processes.prepare_shutdown_until(deadline);
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.processes
            .poll_shutdown(std::time::Instant::now())
            .map_err(str::to_owned)
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.processes.reap_exited().map_err(str::to_owned)?;
        match self.runtime.send(Command::Poll {})? {
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => {
                let payload = self.processes.handle(&caller, payload);
                // Lifetime idempotency survives cancellation after a launch commits.
                match self.runtime.send(Command::Complete {
                    request: id,
                    payload,
                })? {
                    Response::Ok => Ok(()),
                    Response::Error { code, .. }
                        if code == "request_ended" || code == "not_found" =>
                    {
                        Ok(())
                    }
                    response => Err(format!(
                        "unexpected application completion response: {response:?}"
                    )),
                }
            }
            Response::Pending => Ok(()),
            response => Err(format!(
                "unexpected application poll response: {response:?}"
            )),
        }
    }
}

amaterasu_module_sdk::export_module!(ApplicationModule);

#[cfg(test)]
mod tests;
