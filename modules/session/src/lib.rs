//! Original session coordination service, independent of capture and protocol modules.
mod coordinator;
mod leases;
mod protocol;
pub use coordinator::Coordinator;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde_json::json;

pub const SERVICE: &str = "amaterasu.session/1";
pub const CHANGED_TOPIC: &str = "amaterasu.session.changed/1";

pub struct SessionModule {
    runtime: RuntimeClient,
    coordinator: Coordinator,
}

impl ManagedModule for SessionModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            coordinator: Coordinator::default(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        match self.runtime.send(Command::Poll {})? {
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => {
                let previous_revision = self.coordinator.revision();
                let response = self.coordinator.handle(&caller, payload);
                if self.coordinator.revision() != previous_revision {
                    // Notifications are advisory. State is committed even if publication fails;
                    // clients recover through revisioned snapshots and caller-scoped lists.
                    let _ = self.runtime.send(Command::Publish {
                        topic: CHANGED_TOPIC.into(),
                        payload: json!({"revision": self.coordinator.revision()}),
                    });
                }
                // A caller can cancel its request after dispatch. Keep committed state and
                // let an idempotent retry recover it even when completion is rejected.
                match self.runtime.send(Command::Complete {
                    request: id,
                    payload: response,
                })? {
                    Response::Ok => Ok(()),
                    Response::Error { code, .. }
                        if code == "request_ended" || code == "not_found" =>
                    {
                        Ok(())
                    }
                    response => Err(format!(
                        "unexpected session completion response: {response:?}"
                    )),
                }
            }
            Response::Pending => Ok(()),
            response => Err(format!("unexpected session poll response: {response:?}")),
        }
    }
}

amaterasu_module_sdk::export_module!(SessionModule);

#[cfg(test)]
mod tests;

#[cfg(test)]
mod module_tests;
