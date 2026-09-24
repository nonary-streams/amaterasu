use crate::protocol::{Outcome, State};
use crate::records::{CatalogSelection, Record};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub const CATALOG: &str = "amaterasu.catalog/1";
pub const SESSION: &str = "amaterasu.session/1";
pub const APPLICATION: &str = "amaterasu.application/1";
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Catalog,
    CreateSession,
    Acquire,
    Spawn,
    AwaitApplication,
    Activate,
    Monitor,
    FindSession,
    FindProcess,
    StopSession,
    Terminate,
    AwaitTermination,
    Finish,
}
#[derive(Clone)]
pub struct Workflow {
    pub launch_id: u64,
    pub request: Option<u64>,
    pub deadline: Instant,
    pub step: Step,
    pub pending: Option<u64>,
    pub cleanup: bool,
}
impl Workflow {
    pub fn launch(id: u64, request: u64, remaining_ms: u64) -> Self {
        Self {
            launch_id: id,
            request: Some(request),
            deadline: Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
            step: Step::Catalog,
            pending: None,
            cleanup: false,
        }
    }
    pub fn monitor(id: u64) -> Self {
        Self {
            launch_id: id,
            request: None,
            deadline: Instant::now() + Duration::from_secs(5),
            step: Step::Monitor,
            pending: None,
            cleanup: false,
        }
    }
    pub fn begin_cleanup(
        &mut self,
        record: &mut Record,
        outcome: Outcome,
        failure: Option<&'static str>,
    ) {
        record.state = State::Stopping;
        record.cleanup_outcome = outcome;
        if failure.is_some() {
            record.failure = failure;
        }
        record.cleanup_error = None;
        self.cleanup = true;
        self.pending = None;
        self.deadline = Instant::now() + Duration::from_secs(20);
        self.step = first_cleanup_step(record);
    }
    pub fn parent(&self) -> Option<u64> {
        if self.cleanup { None } else { self.request }
    }
    pub fn command(
        &self,
        record: &Record,
        environment: &std::collections::BTreeMap<String, String>,
    ) -> Result<(&'static str, Value), &'static str> {
        let session = record.session_id;
        Ok(match self.step {
            Step::Catalog => {
                let action = match record.catalog_selection {
                    CatalogSelection::Initiator => "resolve_launch",
                    CatalogSelection::Exported => "resolve_exported_launch",
                };
                (CATALOG, json!({"action":action,"id":record.app_id}))
            }
            Step::CreateSession => (SESSION, json!({"action":"create","key":record.session_key})),
            Step::Acquire => (
                SESSION,
                json!({"action":"acquire","session_id":session.ok_or("invalid_workflow")?,"resource":record.resource.as_deref().ok_or("invalid_workflow")?,"mode":"exclusive"}),
            ),
            Step::Spawn => {
                let executable = record.executable.as_ref().ok_or("invalid_workflow")?;
                (
                    APPLICATION,
                    json!({"action":"launch","operation_id":record.process_operation,"executable":executable.program,"args":executable.arguments,"cwd":executable.working_directory,"auto_detach":executable.auto_detach,"wait_all":executable.wait_all,"exit_timeout_seconds":executable.exit_timeout_seconds,"detached":executable.detached,"environment":environment,"preparation":executable.preparation}),
                )
            }
            Step::Activate => (
                SESSION,
                json!({"action":"activate","session_id":session.ok_or("invalid_workflow")?}),
            ),
            Step::AwaitApplication | Step::Monitor | Step::AwaitTermination => (
                APPLICATION,
                json!({"action":"status","process_id":record.process_id.ok_or("invalid_workflow")?}),
            ),
            Step::FindSession => (SESSION, json!({"action":"lookup","key":record.session_key})),
            Step::FindProcess => (
                APPLICATION,
                json!({"action":"lookup","operation_id":record.process_operation}),
            ),
            Step::StopSession => (
                SESSION,
                json!({"action":"stop","session_id":session.ok_or("invalid_workflow")?}),
            ),
            Step::Terminate => (
                APPLICATION,
                json!({"action":"terminate","process_id":record.process_id.ok_or("invalid_workflow")?}),
            ),
            Step::Finish => (
                SESSION,
                json!({"action":record.cleanup_outcome.session_action(),"session_id":session.ok_or("invalid_workflow")?}),
            ),
        })
    }
}
pub fn first_cleanup_step(record: &Record) -> Step {
    if record.session_requested && record.session_id.is_none() {
        Step::FindSession
    } else if record.process_requested && record.process_id.is_none() {
        Step::FindProcess
    } else if record.session_id.is_some() {
        Step::StopSession
    } else if record.process_id.is_some() {
        Step::Terminate
    } else {
        Step::Finish
    }
}
