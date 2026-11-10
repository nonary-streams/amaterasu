//! Original session coordination service, independent of capture and protocol modules.
mod coordinator;
mod leases;
mod protocol;
pub use coordinator::Coordinator;

use amaterasu_module_sdk::events::topics::{SESSION_CHANGED, SessionChanged};
use amaterasu_module_sdk::{ManagedModule, ModuleResult, RuntimeClient};

pub const SERVICE: &str = "amaterasu.session/1";
pub const CHANGED_TOPIC: &str = SESSION_CHANGED.name();

pub struct SessionModule {
    runtime: RuntimeClient,
    coordinator: Coordinator,
}

impl ManagedModule for SessionModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.provide(SERVICE)?;
        Ok(Self {
            runtime,
            coordinator: Coordinator::default(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        let Some(request) = self.runtime.next_request()? else {
            return Ok(());
        };
        let previous_revision = self.coordinator.revision();
        let response = self.coordinator.handle(&request.caller, request.payload);
        if self.coordinator.revision() != previous_revision {
            // Notifications are advisory. State is committed even if publication fails;
            // clients recover through revisioned snapshots and caller-scoped lists.
            let change = SessionChanged {
                revision: self.coordinator.revision(),
            };
            let _ = self.runtime.publish(&SESSION_CHANGED, &change);
        }
        // A caller can cancel its request after dispatch. Committed state stands
        // and an idempotent retry recovers it when the reply is discarded.
        self.runtime.reply(request.id, &response)?;
        Ok(())
    }
}

amaterasu_module_sdk::export_module!(SessionModule);

#[cfg(test)]
mod tests;

#[cfg(test)]
mod module_tests;
