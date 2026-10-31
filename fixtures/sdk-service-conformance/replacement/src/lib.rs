//! Independent in-memory implementation of the key/value subset only.
use amaterasu_module_sdk::{
    Command, ManagedModule, ModuleResult, RuntimeClient,
    state::{STATE_SERVICE, StateRequest},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
struct Namespace {
    revision: u64,
    values: BTreeMap<String, Value>,
}
struct Replacement {
    runtime: RuntimeClient,
    namespaces: BTreeMap<String, Namespace>,
}
impl ManagedModule for Replacement {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: STATE_SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            namespaces: BTreeMap::new(),
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        for _ in 0..16 {
            let Some(request) = self.runtime.poll_serialized()? else {
                break;
            };
            let result = match serde_json::from_value::<StateRequest>(request.payload) {
                Ok(operation) => execute(
                    self.namespaces.entry(request.caller).or_default(),
                    operation,
                ),
                Err(_) => {
                    json!({"ok":false,"error":{"code":"invalid_request","message":"invalid request"}})
                }
            };
            self.runtime.complete_serialized(request.id, &result)?;
        }
        Ok(())
    }
}
fn execute(namespace: &mut Namespace, operation: StateRequest) -> Value {
    match operation {
        StateRequest::Get { key } => {
            json!({"ok":true,"revision":namespace.revision,"found":namespace.values.contains_key(&key),"value":namespace.values.get(&key)})
        }
        StateRequest::Set {
            key,
            value,
            expected_revision,
        } => {
            if expected_revision.is_some_and(|revision| revision != namespace.revision) {
                return conflict(namespace.revision);
            }
            if !namespace.values.contains_key(&key) && namespace.values.len() >= 32 {
                return json!({"ok":false,"error":{"code":"resource_exhausted","message":"fixture capacity reached"}});
            }
            namespace.values.insert(key, value);
            namespace.revision += 1;
            json!({"ok":true,"revision":namespace.revision})
        }
        StateRequest::Delete {
            key,
            expected_revision,
        } => {
            if expected_revision.is_some_and(|revision| revision != namespace.revision) {
                return conflict(namespace.revision);
            }
            let deleted = namespace.values.remove(&key).is_some();
            namespace.revision += 1;
            json!({"ok":true,"revision":namespace.revision,"deleted":deleted})
        }
    }
}
fn conflict(revision: u64) -> Value {
    json!({"ok":false,"error":{"code":"revision_conflict","message":"revision differs","current_revision":revision}})
}
amaterasu_module_sdk::export_module!(Replacement);
