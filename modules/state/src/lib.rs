//! Durable caller-scoped JSON state, served through the runtime request queue.
mod collection;
mod draft;
mod protocol;
mod snapshot;
mod store;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use store::StateStore;

const SERVICE: &str = "amaterasu.state/1";
const REQUESTS_PER_TICK: usize = 16;

struct StateModule {
    runtime: RuntimeClient,
    store: StateStore,
}

impl ManagedModule for StateModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let store = StateStore::open(runtime.runtime_state_root()?)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self { runtime, store })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        for _ in 0..REQUESTS_PER_TICK {
            match self.runtime.send(Command::Poll {})? {
                Response::Pending => break,
                Response::Request {
                    id,
                    caller,
                    service,
                    payload,
                    ..
                } => {
                    let result = if service == SERVICE {
                        self.store.handle(&caller, payload)
                    } else {
                        protocol::StateError::new("invalid_service", "unsupported state service")
                            .payload()
                    };
                    complete_result(self.runtime.send(Command::Complete {
                        request: id,
                        payload: result,
                    })?)?;
                }
                response => return Err(format!("unexpected state poll response: {response:?}")),
            }
        }
        Ok(())
    }
}

fn complete_result(response: Response) -> ModuleResult<()> {
    match response {
        Response::Ok => Ok(()),
        // Persistence may finish after a request expires or its caller cancels.
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        Response::Error { code, message } => Err(format!("{code}: {message}")),
        response => Err(format!(
            "unexpected state completion response: {response:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_discards_expired_requests_but_surfaces_other_errors() {
        assert!(complete_result(Response::error("request_ended", "expired")).is_ok());
        assert!(complete_result(Response::error("not_found", "removed")).is_ok());
        assert!(complete_result(Response::error("invalid_payload", "too large")).is_err());
    }
}

amaterasu_module_sdk::export_module!(StateModule);
