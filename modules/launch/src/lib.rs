//! Catalog-backed application launch and session ownership.
mod library;
mod pipeline;
mod protocol;
pub mod provider_evidence;
mod records;
mod responses;
mod workflow;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use pipeline::Pipeline;
use protocol::Settings;
use std::collections::BTreeMap;
pub const SERVICE: &str = "amaterasu.launch/1";
pub struct LaunchModule {
    runtime: RuntimeClient,
    pipeline: Pipeline,
    child_results: ChildResults,
}

/// Preserve a child result until its broker bookkeeping is definitely released.
/// A lost release reply must not hide the result of a delivered mutation.
#[derive(Default)]
struct ChildResults {
    observed: BTreeMap<u64, serde_json::Value>,
}

impl ChildResults {
    fn send(
        &mut self,
        command: Command,
        mut send: impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<Response> {
        if let Command::TakeResult { request } = command {
            if let std::collections::btree_map::Entry::Vacant(entry) = self.observed.entry(request)
            {
                match send(Command::TakeResult { request })? {
                    Response::Result { payload } => {
                        entry.insert(payload);
                    }
                    response => return Ok(response),
                }
            }
            return match send(Command::ReleaseResult { request }) {
                Ok(Response::Ok) => Ok(Response::Result {
                    payload: self.observed.remove(&request).expect("result retained"),
                }),
                Ok(Response::Error { code, .. }) if code == "not_found" => Ok(Response::Result {
                    payload: self.observed.remove(&request).expect("result retained"),
                }),
                _ => Ok(Response::Pending),
            };
        }
        if let Command::ReleaseResult { request } = command {
            let response = send(Command::ReleaseResult { request })?;
            if matches!(&response, Response::Ok)
                || matches!(&response, Response::Error { code, .. } if code == "not_found")
            {
                self.observed.remove(&request);
            }
            return Ok(response);
        }
        send(command)
    }
}
impl ManagedModule for LaunchModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("launch settings unavailable".into());
        };
        let mut settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid launch settings")?;
        if let Some(composition) = runtime.processing_composition()? {
            let lifecycle = composition
                .lifecycle
                .as_ref()
                .ok_or("launch lifecycle not selected")?;
            let service = amaterasu_module_sdk::processing_composition::require_contract(
                lifecycle,
                amaterasu_module_sdk::extension_api::SESSION_LIFECYCLE_SERVICE,
                1,
                0,
            )?;
            settings.prepared_context_service = Some(service.to_owned());
            settings
                .retirement_observers
                .insert(lifecycle.principal.clone());
        }
        settings.validate().map_err(str::to_owned)?;
        let enable_library_launch = !settings.library_routes.is_empty();
        let environment = settings
            .resolved_environment(|name| std::env::var(name).ok())
            .map_err(str::to_owned)?;
        let mut pipeline = Pipeline::new(environment).map_err(str::to_owned)?;
        pipeline.set_retirement_observers(settings.retirement_observers);
        pipeline.set_application_retirement_observers(settings.application_retirement_observers);
        pipeline.set_provider_routes(settings.provider_routes);
        pipeline.set_library_routes(settings.library_routes);
        pipeline.set_prepared_context_service(settings.prepared_context_service);
        pipeline.set_display_admission_principals(
            settings.display_admission_owner,
            settings.display_admission_verifier,
        );
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        if enable_library_launch {
            runtime.require_success(Command::Register {
                service: amaterasu_module_sdk::application_library::v2::LAUNCH_SERVICE.into(),
            })?;
        }
        Ok(Self {
            runtime,
            pipeline,
            child_results: ChildResults::default(),
        })
    }
    fn prepare_shutdown(&mut self, remaining_ms: u64) -> ModuleResult<()> {
        // Consumers drain first. Gate admission now, then retain the host's
        // deadline so our later provider cleanup cannot renew the host budget.
        self.pipeline.prepare_shutdown(remaining_ms);
        Ok(())
    }
    fn quiesce(&mut self) -> ModuleResult<bool> {
        // Direct SDK launches also own process and session cleanup. The host
        // keeps our provider dependencies ticking until this drain completes.
        Ok(self.pipeline.quiesce())
    }
    fn tick(&mut self) -> ModuleResult<()> {
        let runtime = &self.runtime;
        let results = &mut self.child_results;
        self.pipeline
            .tick(|command| results.send(command, |command| runtime.send(command)))
    }
}
amaterasu_module_sdk::export_module!(LaunchModule);

#[cfg(test)]
mod child_result_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lost_release_reply_does_not_discard_observed_child_result() {
        let mut results = ChildResults::default();
        let mut calls = Vec::new();
        let first = results
            .send(Command::TakeResult { request: 7 }, |command| {
                calls.push(command.clone());
                match command {
                    Command::TakeResult { .. } => Ok(Response::Result {
                        payload: json!({"process_id": 9}),
                    }),
                    Command::ReleaseResult { .. } => Err("lost release reply".into()),
                    _ => panic!("unexpected command"),
                }
            })
            .unwrap();
        assert!(matches!(first, Response::Pending));
        let second = results
            .send(Command::TakeResult { request: 7 }, |command| {
                calls.push(command.clone());
                match command {
                    Command::ReleaseResult { .. } => {
                        Ok(Response::error("not_found", "already released"))
                    }
                    _ => panic!("must not take the result again"),
                }
            })
            .unwrap();
        assert!(matches!(second, Response::Result { payload } if payload["process_id"] == 9));
        assert!(results.observed.is_empty());
        assert_eq!(calls.len(), 3);
    }
}
