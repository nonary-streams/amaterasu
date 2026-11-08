//! Authenticated extension discovery and scoped settings.
mod discovery;
mod persistence;
mod policy;
pub mod settings;
mod settings_service;

use amaterasu_extension_api::EXTENSION_REGISTRY_SERVICE;
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    List {
        after: Option<String>,
        limit: usize,
        revision: Option<u64>,
    },
    Describe {
        provider: String,
    },
    Capabilities {
        provider: String,
    },
    DescribeAction {
        action_id: String,
    },
}

struct Registry {
    runtime: RuntimeClient,
    policy: policy::Settings,
    settings: settings_service::SettingsService,
    quiescing: bool,
}

impl ManagedModule for Registry {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("extension registry settings unavailable".into());
        };
        let mut policy: policy::Settings =
            serde_json::from_value(payload).map_err(|e| e.to_string())?;
        let mut lifecycle_service =
            Some(amaterasu_extension_api::SESSION_LIFECYCLE_SERVICE.to_owned());
        let service = if let Some(composition) = runtime.processing_composition()? {
            let binding = composition
                .settings_registry
                .as_ref()
                .ok_or("registry processing composition lacks its selected binding")?;
            let service = amaterasu_module_sdk::processing_composition::require_contract(
                binding,
                EXTENSION_REGISTRY_SERVICE,
                1,
                0,
            )?
            .to_owned();
            lifecycle_service = composition
                .lifecycle
                .as_ref()
                .map(|binding| {
                    amaterasu_module_sdk::processing_composition::require_contract(
                        binding,
                        amaterasu_extension_api::SESSION_LIFECYCLE_SERVICE,
                        1,
                        0,
                    )
                    .map(str::to_owned)
                })
                .transpose()?;
            policy.add_processing_registrations(&composition.registrations)?;
            service
        } else {
            EXTENSION_REGISTRY_SERVICE.to_owned()
        };
        policy.validate()?;
        runtime.require_success(Command::Register { service })?;
        Ok(Self {
            runtime,
            policy,
            settings: settings_service::SettingsService::new(lifecycle_service),
            quiescing: false,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        for (id, payload) in self.settings.tick(&self.runtime)? {
            self.complete(id, payload)?;
        }
        if self.quiescing {
            return Ok(());
        }
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                initiator,
                payload,
                remaining_ms,
                ..
            } => {
                let name = payload
                    .get("operation")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if name == "settings_get" || name == "settings_update" || name == "settings_migrate"
                {
                    let result =
                        serde_json::from_value::<settings_service::SettingsOperation>(payload)
                            .map_err(|_| {
                                discovery::failure("invalid_request", "invalid settings operation")
                            })
                            .and_then(|operation| {
                                self.settings.accept(
                                    &self.runtime,
                                    &self.policy,
                                    id,
                                    (&caller, &initiator),
                                    remaining_ms,
                                    operation,
                                )
                            });
                    return match result {
                        Ok(()) => Ok(()),
                        Err(error) => self.complete(id, error),
                    };
                }
                let response = match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) => self.handle(id, &caller, &initiator, operation),
                    Err(_) => discovery::failure("invalid_request", "invalid registry operation"),
                };
                self.complete(id, response)
            }
            other => Err(format!("unexpected registry poll response: {other:?}")),
        }
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        self.quiescing = true;
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.quiescing = true;
        for (id, payload) in self.settings.tick(&self.runtime)? {
            self.complete(id, payload)?;
        }
        Ok(self.settings.is_idle())
    }
}

impl Registry {
    fn complete(&self, id: u64, payload: Value) -> ModuleResult<()> {
        let payload = if serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() <= 12_000) {
            payload
        } else {
            discovery::failure("resource_exhausted", "registry response exceeds size limit")
        };
        match self.runtime.send(Command::Complete {
            request: id,
            payload,
        })? {
            Response::Ok => Ok(()),
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(())
            }
            other => Err(format!("registry completion failed: {other:?}")),
        }
    }
    fn handle(&self, parent: u64, caller: &str, initiator: &str, operation: Operation) -> Value {
        let Ok((revision, approvals)) = discovery::approvals(&self.runtime, parent) else {
            return discovery::failure("unavailable", "broker approvals unavailable");
        };
        let capabilities_only = matches!(&operation, Operation::Capabilities { .. });
        match operation {
            Operation::List {
                after,
                limit,
                revision: expected,
            } => {
                if after.is_some() && expected != Some(revision) {
                    return discovery::failure("revision_conflict", "approval revision changed");
                }
                discovery::list(
                    &self.policy,
                    caller,
                    initiator,
                    revision,
                    &approvals,
                    after.as_deref(),
                    limit,
                )
            }
            Operation::Describe { provider } | Operation::Capabilities { provider } => {
                let capabilities = capabilities_only;
                let Some(descriptor) =
                    discovery::describe(&self.policy, caller, initiator, &provider, &approvals)
                else {
                    return discovery::failure("not_found", "descriptor unavailable");
                };
                if capabilities {
                    json!({"ok":true,"status":"ok","revision":revision,"capabilities":descriptor.capabilities})
                } else {
                    json!({"ok":true,"status":"ok","revision":revision,"descriptor":descriptor})
                }
            }
            Operation::DescribeAction { action_id } => {
                for item in &self.policy.descriptors {
                    if !item
                        .grants
                        .iter()
                        .any(|grant| grant.caller == caller && grant.read)
                        || !item
                            .grants
                            .iter()
                            .any(|grant| grant.caller == initiator && grant.read)
                    {
                        continue;
                    }
                    if let Some(action) = item
                        .descriptor
                        .actions
                        .iter()
                        .find(|action| action.id == action_id)
                    {
                        let approved =
                            policy::approved(item, &action.execute, &approvals)
                                && action.plan.as_ref().is_none_or(|method| {
                                    policy::approved(item, method, &approvals)
                                })
                                && action.status.as_ref().is_none_or(|method| {
                                    policy::approved(item, method, &approvals)
                                });
                        if approved {
                            return json!({"ok":true,"status":"ok","revision":revision,"provider":item.descriptor.provider,"action":action});
                        }
                    }
                }
                discovery::failure("not_found", "action unavailable")
            }
        }
    }
}

amaterasu_module_sdk::export_module!(Registry);
