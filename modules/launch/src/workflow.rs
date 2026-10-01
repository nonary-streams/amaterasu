use crate::protocol::{LaunchTarget, Outcome, PlannedPreparationStep, State};
use crate::records::{CatalogSelection, Record};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub const CATALOG: &str = "amaterasu.catalog/1";
pub const SESSION: &str = "amaterasu.session/1";
pub const APPLICATION: &str = "amaterasu.application/1";
pub const PLAYNITE: &str = "amaterasu.playnite/1";
pub const LUTRIS: &str = "amaterasu.lutris/1";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Catalog,
    LutrisStatus,
    LutrisPreSpawn,
    CreateSession,
    Acquire,
    Spawn,
    AwaitApplication,
    PrepareApplication,
    AwaitPreparation,
    StartPlaynite,
    AwaitPlayniteStart,
    MonitorPlaynite,
    Activate,
    Monitor,
    FindSession,
    FindProcess,
    FindPlaynite,
    StopPreparation,
    AwaitPreparationStop,
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
        self.deadline = Instant::now() + Duration::from_millis(record.cleanup_budget_ms);
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
            Step::LutrisStatus | Step::LutrisPreSpawn => (LUTRIS, json!({"action":"status"})),
            Step::CreateSession => (SESSION, json!({"action":"create","key":record.session_key})),
            Step::Acquire => (
                SESSION,
                json!({"action":"acquire","session_id":session.ok_or("invalid_workflow")?,"resource":record.resource.as_deref().ok_or("invalid_workflow")?,"mode":"exclusive"}),
            ),
            Step::Spawn => {
                let Some(LaunchTarget::Executable(executable)) = record.target.as_ref() else {
                    return Err("invalid_workflow");
                };
                let mut payload = json!({
                    "action":"launch",
                    "operation_id":record.process_operation,
                    "output":record.output,
                    "executable":executable.program,
                    "args":executable.arguments,
                    "cwd":executable.working_directory,
                    "preparation_cwd":executable.preparation_working_directory,
                    "detached_cwd":executable.preparation_working_directory,
                    "auto_detach":executable.auto_detach,
                    "wait_all":executable.wait_all,
                    "exit_timeout_seconds":executable.exit_timeout_seconds,
                    "steam_id":executable.steam_id,
                    "steam_install_dir":executable.steam_install_dir,
                    "elevated":executable.elevated,
                    "detached":executable.detached,
                    "environment":environment,
                    "preparation":executable.preparation,
                    "proton_dualsense_compatibility":record.proton_dualsense_compatibility,
                });
                if let Some(intent) = &record.virtual_display_intent {
                    payload["virtual_display_intent"] = json!(intent);
                }
                (APPLICATION, payload)
            }
            Step::StartPlaynite => match record.target.as_ref() {
                Some(LaunchTarget::Playnite { game_id, .. }) => (
                    PLAYNITE,
                    json!({"action":"launch_start","operation_id":record.process_operation,"game_id":game_id}),
                ),
                Some(LaunchTarget::PlayniteFullscreen { .. }) => (
                    PLAYNITE,
                    json!({"action":"fullscreen_start","operation_id":record.process_operation}),
                ),
                _ => return Err("invalid_workflow"),
            },
            Step::PrepareApplication => {
                let (preparation, working_directory, exit_timeout_seconds) =
                    match record.target.as_ref() {
                        Some(LaunchTarget::Playnite {
                            preparation,
                            working_directory,
                            exit_timeout_seconds,
                            ..
                        })
                        | Some(LaunchTarget::PlayniteFullscreen {
                            preparation,
                            working_directory,
                            exit_timeout_seconds,
                        })
                        | Some(LaunchTarget::Desktop {
                            preparation,
                            working_directory,
                            exit_timeout_seconds,
                        }) => (preparation, working_directory, exit_timeout_seconds),
                        _ => return Err("invalid_workflow"),
                    };
                (
                    APPLICATION,
                    json!({"action":"prepare","operation_id":record.preparation_operation,"output":record.output,"environment":environment,"cwd":working_directory,"exit_timeout_seconds":exit_timeout_seconds,"preparation":preparation}),
                )
            }
            Step::AwaitPreparation => (
                APPLICATION,
                json!({"action":"preparation_status","operation_id":record.preparation_operation}),
            ),
            Step::StopPreparation => (
                APPLICATION,
                json!({"action":"preparation_stop","operation_id":record.preparation_operation}),
            ),
            Step::AwaitPreparationStop => (
                APPLICATION,
                json!({"action":"preparation_status","operation_id":record.preparation_operation}),
            ),
            Step::FindPlaynite => match record.target.as_ref() {
                Some(LaunchTarget::Playnite { .. }) => (
                    PLAYNITE,
                    json!({"action":"launch_lookup","operation_id":record.process_operation}),
                ),
                Some(LaunchTarget::PlayniteFullscreen { .. }) => (
                    PLAYNITE,
                    json!({"action":"fullscreen_lookup","operation_id":record.process_operation}),
                ),
                _ => return Err("invalid_workflow"),
            },
            Step::RecoverPlayniteStop => match record.target.as_ref() {
                Some(LaunchTarget::Playnite {
                    game_id,
                    exit_timeout_seconds,
                    ..
                }) => (
                    PLAYNITE,
                    json!({"action":"launch_recover_stop","operation_id":record.process_operation,"game_id":game_id,"exit_timeout_seconds":exit_timeout_seconds}),
                ),
                Some(LaunchTarget::PlayniteFullscreen {
                    exit_timeout_seconds,
                    ..
                }) => (
                    PLAYNITE,
                    json!({"action":"fullscreen_recover_stop","operation_id":record.process_operation,"exit_timeout_seconds":exit_timeout_seconds}),
                ),
                _ => return Err("invalid_workflow"),
            },
            Step::StopPlaynite => {
                let Some(ticket) = record.provider_ticket.as_deref() else {
                    return Err("invalid_workflow");
                };
                match record.target.as_ref() {
                    Some(LaunchTarget::Playnite {
                        exit_timeout_seconds,
                        ..
                    }) => (
                        PLAYNITE,
                        json!({"action":"launch_stop","ticket":ticket,"exit_timeout_seconds":exit_timeout_seconds}),
                    ),
                    Some(LaunchTarget::PlayniteFullscreen {
                        exit_timeout_seconds,
                        ..
                    }) => (
                        PLAYNITE,
                        json!({"action":"fullscreen_stop","ticket":ticket,"exit_timeout_seconds":exit_timeout_seconds}),
                    ),
                    _ => return Err("invalid_workflow"),
                }
            }
            Step::AwaitPlayniteStart | Step::MonitorPlaynite | Step::AwaitPlayniteStop => {
                let ticket = record
                    .provider_ticket
                    .as_deref()
                    .ok_or("invalid_workflow")?;
                match record.target.as_ref() {
                    Some(LaunchTarget::Playnite { .. }) => {
                        (PLAYNITE, json!({"action":"launch_status","ticket":ticket}))
                    }
                    Some(LaunchTarget::PlayniteFullscreen { .. }) => (
                        PLAYNITE,
                        json!({"action":"fullscreen_status","ticket":ticket}),
                    ),
                    _ => return Err("invalid_workflow"),
                }
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

/// The workflow owns one deadline across provider stop and sequential undo.
/// Each RPC still has its own short request deadline; polling does not restart
/// this budget. The plan is a conservative upper bound because application
/// preparation alone knows which steps were started and therefore owe undo.
// The resolved plan admits at most eight steps, a 300-second exit timeout,
// and 30-second structured commands. The largest budget is provider stop plus
// eight raw undos: 321,000 + 8 * 303,000 milliseconds.
pub(crate) const MAX_CLEANUP_BUDGET_MS: u64 = 2_745_000;

/// Session stop, preparation polling, and the finish call, before any undo.
const DESKTOP_CLEANUP_BASE_MS: u64 = 20_000;

pub(crate) fn cleanup_budget(target: Option<&LaunchTarget>) -> Duration {
    let (primary_budget_ms, exit_timeout_seconds, preparation) = match target {
        Some(LaunchTarget::Executable(executable)) => {
            let timeout = executable.exit_timeout_seconds.min(300);
            (
                (timeout + 5).max(20) * 1_000,
                timeout,
                executable.preparation.as_slice(),
            )
        }
        Some(LaunchTarget::Playnite {
            exit_timeout_seconds,
            preparation,
            ..
        })
        | Some(LaunchTarget::PlayniteFullscreen {
            exit_timeout_seconds,
            preparation,
            ..
        }) => {
            let timeout = (*exit_timeout_seconds).min(300);
            // Two bounded authenticated pipe connections, stop, and polling.
            ((timeout + 21) * 1_000, timeout, preparation.as_slice())
        }
        Some(LaunchTarget::Desktop {
            exit_timeout_seconds,
            preparation,
            ..
        }) => {
            // No process or provider stop exists. Only the session stop and the
            // application provider's reverse undo remain.
            let timeout = (*exit_timeout_seconds).min(300);
            (DESKTOP_CLEANUP_BASE_MS, timeout, preparation.as_slice())
        }
        None => return Duration::from_secs(20),
    };
    let budget_ms = preparation
        .iter()
        .take(8)
        .filter_map(|step| match step {
            PlannedPreparationStep::Structured(step) => step
                .undo
                .as_ref()
                .map(|command| command.timeout_ms.min(30_000)),
            PlannedPreparationStep::Legacy(step) if !step.undo.is_empty() => {
                Some(exit_timeout_seconds.max(15) * 1_000)
            }
            PlannedPreparationStep::Legacy(_) => None,
        })
        .try_fold(primary_budget_ms, |budget, timeout_ms| {
            // Containment allows a 200ms stop grace and two-second reap tail.
            // Round up for asynchronous polling and calls between ordered steps.
            budget.checked_add(timeout_ms.checked_add(3_000)?)
        })
        .unwrap_or(MAX_CLEANUP_BUDGET_MS)
        .clamp(20_000, MAX_CLEANUP_BUDGET_MS);
    Duration::from_millis(budget_ms)
}
pub fn first_cleanup_step(record: &Record) -> Step {
    if record.session_requested && record.session_id.is_none() {
        Step::FindSession
    } else if record.provider_launch_requested && record.provider_ticket.is_none() {
        Step::FindPlaynite
    } else if record.provider_ticket.is_some() && !record.provider_stopped {
        Step::StopPlaynite
    } else if record.preparation_requested && !record.preparation_stopped {
        Step::StopPreparation
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
    use crate::records::{GlobalPreparation, Records};
    use std::collections::BTreeMap;

    #[test]
    fn executable_spawn_keeps_explicit_app_directory_separate_from_inferred_directory() {
        let mut records = Records::new().unwrap();
        let (launch_id, _) = records
            .start(
                "alice",
                "alice",
                "directory-policy".into(),
                "app-0000000000000001".into(),
                CatalogSelection::Initiator,
                GlobalPreparation::default(),
            )
            .unwrap();
        let program = std::env::current_exe().unwrap();
        let directory = std::env::current_dir().unwrap();
        for explicit_directory in [None, Some(directory.clone())] {
            let record = records.values.get_mut(&launch_id).unwrap();
            record.target = Some(
                LaunchTarget::from_catalog_entry_with_environment(
                    &json!({"launch":{
                        "kind":"executable","program":program,
                        "working_directory":explicit_directory,
                    }}),
                    &[],
                    &BTreeMap::new(),
                )
                .unwrap(),
            );
            let mut workflow = Workflow::launch(launch_id, 1, 30_000);
            workflow.step = Step::Spawn;
            let (service, payload) = workflow.command(record, &BTreeMap::new()).unwrap();
            assert_eq!(service, APPLICATION);
            assert_eq!(payload["detached_cwd"], json!(explicit_directory));
            let expected_main = explicit_directory
                .as_deref()
                .unwrap_or_else(|| program.parent().unwrap());
            assert_eq!(payload["cwd"], json!(expected_main));
        }
    }

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
                GlobalPreparation::default(),
            )
            .unwrap();
        let record = records.values.get_mut(&launch_id).unwrap();
        record.target = Some(LaunchTarget::Playnite {
            game_id: "a1b2c3d4-1234-5678-9abc-0123456789ab".into(),
            exit_timeout_seconds: 300,
            working_directory: None,
            preparation: Vec::new(),
        });
        record.cleanup_budget_ms =
            u64::try_from(cleanup_budget(record.target.as_ref()).as_millis()).unwrap();
        record.provider_launch_requested = true;
        let mut workflow = Workflow::launch(launch_id, 1, 30_000);
        workflow.begin_cleanup(record, Outcome::Failed, Some("unknown_start_result"));
        assert_eq!(workflow.step, Step::FindPlaynite);
        let deadline = workflow.deadline.saturating_duration_since(Instant::now());
        assert!(deadline <= Duration::from_secs(321));
        assert!(deadline >= Duration::from_secs(320));
    }

    #[test]
    fn playnite_preparation_forwards_the_explicit_catalog_directory_and_exit_policy() {
        let mut records = Records::new().unwrap();
        let (launch_id, _) = records
            .start(
                "alice",
                "alice",
                "operation".into(),
                "app-0000000000000001".into(),
                CatalogSelection::Initiator,
                GlobalPreparation::default(),
            )
            .unwrap();
        let directory = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let environment = BTreeMap::from([("GAME_PROFILE".into(), "profile-a".into())]);

        for target in [
            LaunchTarget::Playnite {
                game_id: "a1b2c3d4-1234-5678-9abc-0123456789ab".into(),
                exit_timeout_seconds: 300,
                working_directory: Some(directory.clone()),
                preparation: vec![crate::protocol::PlannedPreparationStep::Legacy(
                    crate::protocol::LegacyPreparationCommand {
                        do_command: "configure $(GAME_PROFILE)".into(),
                        undo: String::new(),
                        elevated: false,
                    },
                )],
            },
            LaunchTarget::PlayniteFullscreen {
                exit_timeout_seconds: 300,
                working_directory: Some(directory.clone()),
                preparation: vec![crate::protocol::PlannedPreparationStep::Legacy(
                    crate::protocol::LegacyPreparationCommand {
                        do_command: "configure $(GAME_PROFILE)".into(),
                        undo: String::new(),
                        elevated: false,
                    },
                )],
            },
        ] {
            let record = records.values.get_mut(&launch_id).unwrap();
            record.target = Some(target);
            let workflow = Workflow {
                launch_id,
                request: Some(1),
                deadline: Instant::now() + Duration::from_secs(5),
                step: Step::PrepareApplication,
                pending: None,
                cleanup: false,
            };
            let (service, payload) = workflow.command(record, &environment).unwrap();

            assert_eq!(service, APPLICATION);
            assert_eq!(payload["action"], "prepare");
            assert_eq!(payload["cwd"], directory);
            assert_eq!(payload["exit_timeout_seconds"], 300);
            assert_eq!(payload["environment"]["GAME_PROFILE"], "profile-a");
            assert_eq!(payload["preparation"][0]["do"], "configure $(GAME_PROFILE)");
        }
    }

    #[test]
    fn cleanup_deadline_covers_all_raw_undos_and_structured_explicit_timeouts() {
        let target = |timeout, steps| LaunchTarget::PlayniteFullscreen {
            exit_timeout_seconds: timeout,
            working_directory: None,
            preparation: steps,
        };
        let raw = PlannedPreparationStep::Legacy(crate::protocol::LegacyPreparationCommand {
            do_command: "prepare".into(),
            undo: "cleanup".into(),
            elevated: false,
        });
        assert_eq!(
            cleanup_budget(Some(&target(300, vec![raw.clone(); 8]))),
            Duration::from_secs(321 + 8 * 303)
        );
        assert_eq!(
            cleanup_budget(Some(&target(0, vec![raw]))),
            Duration::from_secs(21 + 18)
        );
        let command = crate::protocol::ConfiguredCommand {
            executable: std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cwd: None,
            timeout_ms: 25,
        };
        let structured = PlannedPreparationStep::Structured(crate::protocol::PreparationStep {
            run: command.clone(),
            undo: Some(command),
        });
        assert_eq!(
            cleanup_budget(Some(&target(300, vec![structured]))),
            Duration::from_millis(324_025)
        );
    }

    #[test]
    fn cleanup_budget_caps_invalid_internal_plans_without_overflow() {
        let raw = PlannedPreparationStep::Legacy(crate::protocol::LegacyPreparationCommand {
            do_command: "prepare".into(),
            undo: "undo".into(),
            elevated: false,
        });
        let target = LaunchTarget::PlayniteFullscreen {
            exit_timeout_seconds: u64::MAX,
            working_directory: None,
            preparation: vec![raw; 100],
        };
        assert_eq!(
            cleanup_budget(Some(&target)),
            Duration::from_millis(MAX_CLEANUP_BUDGET_MS)
        );
        assert_eq!(cleanup_budget(None), Duration::from_secs(20));
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
                GlobalPreparation::default(),
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

    #[test]
    fn executable_elevation_policy_reaches_application_launch_payload() {
        let mut records = Records::new().unwrap();
        let (launch_id, _) = records
            .start(
                "alice",
                "alice",
                "operation".into(),
                "app-0000000000000001".into(),
                CatalogSelection::Initiator,
                GlobalPreparation::default(),
            )
            .unwrap();
        let record = records.values.get_mut(&launch_id).unwrap();
        let mut executable = crate::protocol::Executable::from_catalog(&json!({
            "kind":"executable",
            "program":std::env::current_exe().unwrap(),
        }))
        .unwrap();
        executable.elevated = true;
        record.target = Some(LaunchTarget::Executable(executable));
        record.process_operation = "elevated-launch".into();
        let workflow = Workflow {
            launch_id,
            request: None,
            deadline: Instant::now() + Duration::from_secs(5),
            step: Step::Spawn,
            pending: None,
            cleanup: false,
        };

        let (_, payload) = workflow.command(record, &BTreeMap::new()).unwrap();
        assert_eq!(payload["elevated"], true);
    }
}
