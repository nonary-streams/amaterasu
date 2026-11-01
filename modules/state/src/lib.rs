//! Durable caller-scoped JSON state, served through the runtime request queue.
mod collection;
mod draft;
mod protocol;
mod snapshot;
mod store;

/// Minimal in-process State interface for cross-module integration tests.
/// The feature is disabled in production builds.
#[cfg(feature = "test-support")]
pub mod test_support {
    use serde_json::Value;
    use std::path::PathBuf;

    pub struct TestStateStore(crate::store::StateStore);

    impl TestStateStore {
        pub fn open(directory: PathBuf) -> Result<Self, String> {
            crate::store::StateStore::open(directory).map(Self)
        }

        pub fn handle(&self, caller: &str, payload: Value) -> Value {
            self.0.handle(caller, payload)
        }
    }
}

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

#[cfg(not(feature = "test-support"))]
amaterasu_module_sdk::export_module!(StateModule);

/// Provide the lifecycle table without a duplicate native entrypoint when
/// linking State into another module's in-process tests.
#[cfg(feature = "test-support")]
pub fn test_module_api() -> *const amaterasu_module_sdk::abi::ModuleApi {
    use amaterasu_module_sdk::{abi, exports};
    static API: abi::ModuleApi = abi::ModuleApi {
        header: abi::AbiHeader::of::<abi::ModuleApi>(),
        create: exports::create::<StateModule>,
        tick: exports::tick::<StateModule>,
        health: exports::health::<StateModule>,
        destroy: exports::destroy::<StateModule>,
    };
    &API
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "test-support")]
    #[test]
    fn test_support_opens_real_caller_scoped_store() {
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-state-test-support-{}",
            amaterasu_storage::random_identifier().unwrap()
        ));
        let store = test_support::TestStateStore::open(directory.clone()).unwrap();
        assert_eq!(
            store.handle(
                "first",
                serde_json::json!({"action":"set","key":"one","value":1})
            )["ok"],
            true
        );
        assert_eq!(
            store.handle("first", serde_json::json!({"action":"get","key":"one"}))["value"],
            1
        );
        assert_eq!(
            store.handle("second", serde_json::json!({"action":"get","key":"one"}))["found"],
            false
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn completion_discards_expired_requests_but_surfaces_other_errors() {
        assert!(complete_result(Response::error("request_ended", "expired")).is_ok());
        assert!(complete_result(Response::error("not_found", "removed")).is_ok());
        assert!(complete_result(Response::error("invalid_payload", "too large")).is_err());
    }
}
