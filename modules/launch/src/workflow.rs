use crate::protocol::{LaunchTarget, Outcome, State};
use crate::records::{CatalogSelection, Record};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub const CATALOG: &str = "amaterasu.catalog/1";
pub const SESSION: &str = "amaterasu.session/1";
pub const APPLICATION: &str = "amaterasu.application/1";
pub const PLAYNITE: &str = "amaterasu.playnite/1";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Catalog,
    CreateSession,
    Acquire,
    Spawn,
    AwaitApplication,
    StartPlaynite,
    AwaitPlayniteStart,
    MonitorPlaynite,
    Activate,
    Monitor,
    FindSession,
    FindProcess,
    FindPlaynite,
    RecoverPlayniteStop,
    StopSession,
    Terminate,
    AwaitTermination,
    StopPlaynite,
    AwaitPlayniteStop,
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
    pub fn monitor(id: u64, playnite: bool) -> Self {
        Self {
            launch_id: id,
            request: None,
            deadline: Instant::now() + Duration::from_secs(5),
            step: if playnite {
                Step::MonitorPlaynite
            } else {
                Step::Monitor
            },
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
        let cleanup_seconds = match record.target.as_ref() {
            Some(LaunchTarget::Playnite {
                exit_timeout_seconds,
                ..
            // The provider may spend up to two 8-second authenticated pipe
            // connections before its bounded stop wait, plus one poll interval.
            }) => exit_timeout_seconds.saturating_add(21).min(321),
            _ => 20,
        };
        self.deadline = Instant::now() + Duration::from_secs(cleanup_seconds);
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
                let Some(LaunchTarget::Executable(executable)) = record.target.as_ref() else {
                    return Err("invalid_workflow");
                };
                (
                    APPLICATION,
                    json!({"action":"launch","operation_id":record.process_operation,"executable":executable.program,"args":executable.arguments,"cwd":executable.working_directory,"auto_detach":executable.auto_detach,"wait_all":executable.wait_all,"exit_timeout_seconds":executable.exit_timeout_seconds,"steam_id":executable.steam_id,"steam_install_dir":executable.steam_install_dir,"detached":executable.detached,"environment":environment,"preparation":executable.preparation}),
                )
            }
            Step::StartPlaynite => {
                let Some(LaunchTarget::Playnite { game_id, .. }) = record.target.as_ref() else {
                    return Err("invalid_workflow");
                };
                (
                    PLAYNITE,
                    json!({"action":"launch_start","operation_id":record.process_operation,"game_id":game_id}),
                )
            }
            Step::FindPlaynite => (
                PLAYNITE,
                json!({"action":"launch_lookup","operation_id":record.process_operation}),
            ),
            Step::RecoverPlayniteStop => {
                let Some(LaunchTarget::Playnite {
                    game_id,
                    exit_timeout_seconds,
                }) = record.target.as_ref()
                else {
                    return Err("invalid_workflow");
                };
                (
                    PLAYNITE,
                    json!({"action":"launch_recover_stop","operation_id":record.process_operation,"game_id":game_id,"exit_timeout_seconds":exit_timeout_seconds}),
                )
            }
            Step::StopPlaynite => {
                let (
                    Some(LaunchTarget::Playnite {
                        exit_timeout_seconds,
                        ..
                    }),
                    Some(ticket),
                ) = (record.target.as_ref(), record.provider_ticket.as_deref())
                else {
                    return Err("invalid_workflow");
                };
                (
                    PLAYNITE,
                    json!({"action":"launch_stop","ticket":ticket,"exit_timeout_seconds":exit_timeout_seconds}),
                )
            }
            Step::AwaitPlayniteStart | Step::MonitorPlaynite | Step::AwaitPlayniteStop => (
                PLAYNITE,
                json!({"action":"launch_status","ticket":record.provider_ticket.as_deref().ok_or("invalid_workflow")?}),
            ),
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
    } else if record.provider_launch_requested && record.provider_ticket.is_none() {
        Step::FindPlaynite
    } else if record.provider_ticket.is_some() && !record.provider_stopped {
        Step::StopPlaynite
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::Records;
    use std::collections::BTreeMap;

    #[test]
    fn ambiguous_playnite_start_looks_up_its_operation_and_stop_deadline_is_bounded() {
        let mut records = Records::new().unwrap();
        let (launch_id, _) = records
            .start(
                "alice",
                "alice",
                "operation".into(),
                "app-0000000000000001".into(),
                CatalogSelection::Initiator,
            )
            .unwrap();
        let record = records.values.get_mut(&launch_id).unwrap();
        record.target = Some(LaunchTarget::Playnite {
            game_id: "a1b2c3d4-1234-5678-9abc-0123456789ab".into(),
            exit_timeout_seconds: 300,
        });
        record.provider_launch_requested = true;
        let mut workflow = Workflow::launch(launch_id, 1, 30_000);
        workflow.begin_cleanup(record, Outcome::Failed, Some("unknown_start_result"));
        assert_eq!(workflow.step, Step::FindPlaynite);
        let deadline = workflow.deadline.saturating_duration_since(Instant::now());
        assert!(deadline <= Duration::from_secs(321));
        assert!(deadline >= Duration::from_secs(320));
    }

    #[test]
    fn steam_catalog_metadata_reaches_the_application_launch_request() {
        let mut records = Records::new().unwrap();
        let (launch_id, _) = records
            .start(
                "alice",
                "alice",
                "operation".into(),
                "app-0000000000000001".into(),
                CatalogSelection::Initiator,
            )
            .unwrap();
        let record = records.values.get_mut(&launch_id).unwrap();
        let program = std::env::current_exe().unwrap();
        record.target = Some(LaunchTarget::Executable(
            crate::protocol::Executable::from_catalog(&json!({
                "kind":"executable",
                "program":program,
                "arguments":["-applaunch","42"]
            }))
            .unwrap(),
        ));
        if let Some(LaunchTarget::Executable(executable)) = record.target.as_mut() {
            executable.steam_id = Some("42".into());
            executable.steam_install_dir = Some("/games/title".into());
        }
        record.process_operation = "steam-launch".into();
        let workflow = Workflow {
            launch_id,
            request: None,
            deadline: Instant::now() + Duration::from_secs(5),
            step: Step::Spawn,
            pending: None,
            cleanup: false,
        };

        let (_, payload) = workflow.command(record, &BTreeMap::new()).unwrap();
        assert_eq!(payload["steam_id"], "42");
        assert_eq!(payload["steam_install_dir"], "/games/title");
    }
}
