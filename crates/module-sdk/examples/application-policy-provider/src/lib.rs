//! Synthetic separately built policy provider for replacement qualification.
use amaterasu_module_sdk::application_library::policy::{
    CommandSpec, Identity, PolicyAdapter, PolicyProvider, PreparationStep, Request, Snapshot,
};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use std::collections::{BTreeMap, BTreeSet};

struct Policy;
impl PolicyProvider for Policy {
    fn resolve(&mut self, _: &Identity<'_>, request: &Request) -> Result<Snapshot, &'static str> {
        let executable = std::env::current_exe()
            .map_err(|_| "unavailable")?
            .to_string_lossy()
            .into_owned();
        Ok(Snapshot {
            application: request.application.clone(),
            entry_revision: request.entry_revision.clone(),
            policy_revision: "artifact-policy-v1".into(),
            working_directory: None,
            exit_timeout_seconds: 10,
            preparation: vec![PreparationStep {
                run: CommandSpec {
                    executable,
                    args: vec![],
                    environment: BTreeMap::new(),
                    cwd: None,
                    timeout_ms: 10_000,
                },
                undo: None,
            }],
        })
    }
}

struct Module {
    runtime: RuntimeClient,
    adapter: PolicyAdapter<Policy>,
}
impl ManagedModule for Module {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let service = "fixture.policy/1";
        let callers =
            BTreeMap::from([("amaterasu.launch".into(), BTreeSet::from(["alice".into()]))]);
        let adapter = PolicyAdapter::new(Policy, service.into(), callers).map_err(str::to_owned)?;
        runtime.require_success(Command::Register {
            service: service.into(),
        })?;
        Ok(Self { runtime, adapter })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(request) = self.runtime.poll_serialized()? {
            let response = self.adapter.dispatch(&request);
            match self.runtime.send(Command::Complete {
                request: request.id,
                payload: response,
            })? {
                Response::Ok => (),
                other => return Err(format!("policy completion failed: {other:?}")),
            }
        }
        Ok(())
    }
}
amaterasu_module_sdk::export_module!(Module);
