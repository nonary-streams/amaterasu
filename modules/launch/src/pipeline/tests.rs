use super::*;
use serde_json::json;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::Duration;

struct TestSession {
    id: u64,
    key: String,
    state: &'static str,
    lease: Option<String>,
}
struct TestProcess {
    id: u64,
    operation: String,
    child: Option<Child>,
    state: &'static str,
    preparation_terminal: Option<&'static str>,
    pending_terminal: &'static str,
    preparing_statuses: usize,
    stopping_statuses: usize,
    fail_stopping_cleanup_once: bool,
}
struct TestPreparation {
    operation: String,
    plan: Value,
    state: &'static str,
    preparing_statuses: usize,
}
struct TestProviderLaunch {
    operation: String,
    game_id: String,
    ticket: String,
    state: &'static str,
}
#[derive(Default)]
struct Harness {
    incoming: Option<Response>,
    reply: Option<Response>,
    output: Option<Value>,
    sessions: Vec<TestSession>,
    processes: Vec<TestProcess>,
    preparations: Vec<TestPreparation>,
    root_active: bool,
    cancel_after_spawn: bool,
    cancel_after_session: bool,
    fail_activate: bool,
    fail_terminate: bool,
    commands: Vec<(String, Value, Option<u64>)>,
    catalog_exit: bool,
    drop_spawn_result: bool,
    reject_activation_completion: bool,
    queue_spawn: bool,
    queued_spawn: Option<Value>,
    fail_cancel: bool,
    fail_release: bool,
    released: usize,
    catalog_preparation: Option<Value>,
    catalog_exclude_global_prep_cmd: Option<bool>,
    catalog_working_directory: Option<String>,
    catalog_auto_detach: Option<bool>,
    catalog_wait_all: Option<bool>,
    catalog_exit_timeout_seconds: Option<u64>,
    catalog_detached: Option<Vec<String>>,
    catalog_steam_id: Option<String>,
    catalog_steam_install_dir: Option<String>,
    catalog_fec_percentage: Option<u8>,
    catalog_audio_sink: Option<String>,
    catalog_audio_sink_capture_only: Option<bool>,
    catalog_back_button_timeout: Option<i32>,
    catalog_playnite_game_id: Option<String>,
    catalog_playnite_exit_timeout_seconds: u64,
    provider_launches: Vec<TestProviderLaunch>,
    provider_start_rejection: Option<Value>,
    drop_provider_start_result: bool,
    provider_status_not_found_once: bool,
    provider_stop_failure_once: bool,
    provider_recovery_failure_once: bool,
    application_preparing_statuses: usize,
    preparation_terminal: Option<&'static str>,
    application_stopping_statuses: usize,
    fail_application_cleanup_once: bool,
    fail_automatic_cleanup_once: bool,
    global_preparation_statuses: usize,
    fail_global_preparation: bool,
    fail_global_preparation_cleanup_once: bool,
}
impl Harness {
    fn new() -> Self {
        Self {
            incoming: None,
            reply: None,
            output: None,
            sessions: Vec::new(),
            processes: Vec::new(),
            preparations: Vec::new(),
            root_active: true,
            cancel_after_spawn: false,
            cancel_after_session: false,
            fail_activate: false,
            fail_terminate: false,
            commands: Vec::new(),
            catalog_exit: false,
            drop_spawn_result: false,
            reject_activation_completion: false,
            queue_spawn: false,
            queued_spawn: None,
            fail_cancel: false,
            fail_release: false,
            released: 0,
            catalog_preparation: None,
            catalog_exclude_global_prep_cmd: None,
            catalog_working_directory: None,
            catalog_auto_detach: None,
            catalog_wait_all: None,
            catalog_exit_timeout_seconds: None,
            catalog_detached: None,
            catalog_steam_id: None,
            catalog_steam_install_dir: None,
            catalog_fec_percentage: None,
            catalog_audio_sink: None,
            catalog_audio_sink_capture_only: None,
            catalog_back_button_timeout: None,
            catalog_playnite_game_id: None,
            catalog_playnite_exit_timeout_seconds: 10,
            provider_launches: Vec::new(),
            provider_start_rejection: None,
            drop_provider_start_result: false,
            provider_status_not_found_once: false,
            provider_stop_failure_once: false,
            provider_recovery_failure_once: false,
            application_preparing_statuses: 0,
            preparation_terminal: None,
            application_stopping_statuses: 0,
            fail_application_cleanup_once: false,
            fail_automatic_cleanup_once: false,
            global_preparation_statuses: 0,
            fail_global_preparation: false,
            fail_global_preparation_cleanup_once: false,
        }
    }
    fn session_reply(session: &TestSession) -> Value {
        let mut leases = serde_json::Map::new();
        if let Some(resource) = &session.lease {
            leases.insert(resource.clone(), json!("exclusive"));
        }
        json!({"ok":true,"revision":1,"sessions":[{"id":session.id,"key":session.key,"state":session.state,"leases":leases}]})
    }
    fn session(&mut self, payload: &Value) -> Value {
        let action = payload["action"].as_str().unwrap();
        if action == "create" {
            if let Some(session) = self
                .sessions
                .iter()
                .find(|session| session.key == payload["key"])
            {
                return Self::session_reply(session);
            }
            self.sessions.push(TestSession {
                id: self.sessions.len() as u64 + 1,
                key: payload["key"].as_str().unwrap().into(),
                state: "preparing",
                lease: None,
            });
            return Self::session_reply(self.sessions.last().unwrap());
        }
        if action == "lookup" {
            return self
                .sessions
                .iter()
                .find(|session| session.key == payload["key"])
                .map(Self::session_reply)
                .unwrap_or(json!({"ok":false,"code":"session_not_found"}));
        }
        let index = self
            .sessions
            .iter()
            .position(|session| session.id == payload["session_id"])
            .unwrap();
        match action {
            "acquire" => {
                let resource = payload["resource"].as_str().unwrap();
                if self.sessions.iter().any(|session| {
                    session.id != self.sessions[index].id
                        && session.lease.as_deref() == Some(resource)
                }) {
                    return json!({"ok":false,"code":"resource_busy"});
                }
                self.sessions[index].lease = Some(resource.into());
            }
            "activate" => {
                if self.fail_activate {
                    return json!({"ok":false,"code":"unavailable"});
                }
                self.sessions[index].state = "active";
            }
            "stop" => self.sessions[index].state = "stopping",
            "finish_stop" | "cancel" | "fail" => {
                self.sessions[index].state = "stopped";
                self.sessions[index].lease = None;
            }
            _ => panic!("unexpected session action"),
        }
        Self::session_reply(&self.sessions[index])
    }
    fn process_reply(process: &TestProcess) -> Value {
        let mut reply = json!({"ok":true,"process":{"process_id":process.id,"operation_id":process.operation,"state":process.state}});
        if process.state == "cleanup_failed" {
            reply["process"]["outcome"] = json!(process.pending_terminal);
        }
        reply
    }
    fn application(&mut self, payload: &Value) -> Value {
        let action = payload["action"].as_str().unwrap();
        if matches!(
            action,
            "prepare" | "preparation_status" | "preparation_stop" | "preparation_retry"
        ) {
            return self.preparation(payload);
        }
        if action == "launch" {
            if let Some(process) = self
                .processes
                .iter()
                .find(|process| process.operation == payload["operation_id"])
            {
                return Self::process_reply(process);
            }
            let mut command = ProcessCommand::new(payload["executable"].as_str().unwrap());
            for argument in payload["args"].as_array().unwrap() {
                command.arg(argument.as_str().unwrap());
            }
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .env_clear();
            for (key, value) in payload["environment"].as_object().unwrap() {
                command.env(key, value.as_str().unwrap());
            }
            let child = command.spawn().unwrap();
            self.processes.push(TestProcess {
                id: self.processes.len() as u64 + 1,
                operation: payload["operation_id"].as_str().unwrap().into(),
                child: Some(child),
                state: if self.application_preparing_statuses > 0
                    || self.preparation_terminal.is_some()
                {
                    "preparing"
                } else {
                    "running"
                },
                preparation_terminal: self.preparation_terminal,
                pending_terminal: "exited",
                preparing_statuses: self.application_preparing_statuses,
                stopping_statuses: self.application_stopping_statuses,
                fail_stopping_cleanup_once: self.fail_automatic_cleanup_once,
            });
            return Self::process_reply(self.processes.last().unwrap());
        }
        if action == "lookup" {
            return self
                .processes
                .iter()
                .find(|process| process.operation == payload["operation_id"])
                .map(Self::process_reply)
                .unwrap_or(json!({"ok":false,"code":"process_not_found"}));
        }
        let stopping_statuses = self.application_stopping_statuses;
        let fail_cleanup = self.fail_application_cleanup_once;
        let process = self
            .processes
            .iter_mut()
            .find(|process| process.id == payload["process_id"])
            .unwrap();
        match action {
            "terminate" => {
                if self.fail_terminate {
                    return json!({"ok":false,"code":"process_kill_failed"});
                }
                if matches!(process.state, "exited" | "launch_failed" | "cancelled") {
                    return Self::process_reply(process);
                }
                if let Some(mut child) = process.child.take() {
                    let _ = child.kill();
                    child.wait().unwrap();
                }
                if process.state != "cleanup_failed" {
                    process.pending_terminal = "cancelled";
                }
                if fail_cleanup {
                    self.fail_application_cleanup_once = false;
                    process.state = "cleanup_failed";
                } else if stopping_statuses > 0 {
                    process.state = "stopping";
                    process.stopping_statuses = stopping_statuses;
                } else {
                    process.state = process.pending_terminal;
                }
            }
            "status" => {
                if process.state == "preparing" {
                    if process.preparing_statuses > 0 {
                        process.preparing_statuses -= 1;
                    } else if let Some(terminal) = process.preparation_terminal.take() {
                        process.pending_terminal = terminal;
                        process.state = "stopping";
                    } else {
                        process.state = "running";
                    }
                } else if process.state == "stopping" {
                    if process.stopping_statuses > 0 {
                        process.stopping_statuses -= 1;
                    } else if process.fail_stopping_cleanup_once {
                        process.fail_stopping_cleanup_once = false;
                        process.state = "cleanup_failed";
                    } else {
                        process.state = process.pending_terminal;
                    }
                } else if process.state == "running"
                    && let Some(child) = &mut process.child
                    && child.try_wait().unwrap().is_some()
                {
                    process.child = None;
                    process.state = "exited";
                }
            }
            _ => panic!("unexpected application action"),
        }
        Self::process_reply(process)
    }
    fn preparation(&mut self, payload: &Value) -> Value {
        let action = payload["action"].as_str().unwrap();
        let operation = payload["operation_id"].as_str().unwrap();
        if action == "prepare" {
            if let Some(existing) = self
                .preparations
                .iter()
                .find(|lease| lease.operation == operation)
            {
                if existing.plan != payload["preparation"] {
                    return json!({"ok":false,"code":"operation_conflict"});
                }
                return Self::preparation_reply(existing);
            }
            let state = if self.fail_global_preparation {
                "preparation_failed"
            } else if self.global_preparation_statuses > 0 {
                "preparing"
            } else {
                "ready"
            };
            self.preparations.push(TestPreparation {
                operation: operation.into(),
                plan: payload["preparation"].clone(),
                state,
                preparing_statuses: self.global_preparation_statuses,
            });
            return Self::preparation_reply(self.preparations.last().unwrap());
        }
        let Some(lease) = self
            .preparations
            .iter_mut()
            .find(|lease| lease.operation == operation)
        else {
            return json!({"ok":false,"code":"preparation_not_found"});
        };
        match action {
            "preparation_status" if lease.state == "preparing" => {
                if lease.preparing_statuses > 0 {
                    lease.preparing_statuses -= 1;
                } else {
                    lease.state = "ready";
                }
            }
            "preparation_stop" | "preparation_retry" => {
                if self.fail_global_preparation_cleanup_once {
                    self.fail_global_preparation_cleanup_once = false;
                    lease.state = "cleanup_failed";
                } else {
                    lease.state = if lease.state == "preparation_failed" {
                        "preparation_failed"
                    } else {
                        "complete"
                    };
                }
            }
            "preparation_status" => {}
            _ => panic!("unexpected preparation action"),
        }
        Self::preparation_reply(lease)
    }
    fn preparation_reply(lease: &TestPreparation) -> Value {
        json!({
            "ok":true,
            "preparation":{
                "operation_id":lease.operation,
                "state":lease.state,
                "preparation":{"completed":1},
                "cleanup":{"state":if lease.state == "complete" {"complete"} else {"pending"}},
            }
        })
    }
    fn playnite(&mut self, payload: &Value) -> Value {
        let action = payload["action"].as_str().unwrap();
        match action {
            "launch_start" => {
                if let Some(response) = self.provider_start_rejection.take() {
                    return response;
                }
                let operation = payload["operation_id"].as_str().unwrap();
                let game_id = payload["game_id"].as_str().unwrap();
                if let Some(existing) = self
                    .provider_launches
                    .iter()
                    .find(|launch| launch.operation == operation)
                {
                    return Self::provider_reply(existing);
                }
                let launch = TestProviderLaunch {
                    operation: operation.into(),
                    game_id: game_id.into(),
                    ticket: "cd".repeat(24),
                    state: "running",
                };
                let response = Self::provider_reply(&launch);
                self.provider_launches.push(launch);
                response
            }
            "launch_lookup" => self
                .provider_launches
                .iter()
                .find(|launch| launch.operation == payload["operation_id"])
                .map(Self::provider_reply)
                .unwrap_or(json!({"ok":false,"code":"launch_not_found"})),
            "launch_status" => {
                if self.provider_status_not_found_once {
                    self.provider_status_not_found_once = false;
                    self.provider_launches.clear();
                    return json!({"ok":false,"code":"launch_not_found"});
                }
                self.provider_launches
                    .iter()
                    .find(|launch| launch.ticket == payload["ticket"])
                    .map(Self::provider_reply)
                    .unwrap_or(json!({"ok":false,"code":"launch_not_found"}))
            }
            "launch_stop" => {
                let Some(launch) = self
                    .provider_launches
                    .iter_mut()
                    .find(|launch| launch.ticket == payload["ticket"])
                else {
                    return json!({"ok":false,"code":"launch_not_found"});
                };
                if self.provider_stop_failure_once {
                    self.provider_stop_failure_once = false;
                    launch.state = "cleanup_failed";
                } else {
                    launch.state = "stopped";
                }
                Self::provider_reply(launch)
            }
            "launch_recover_stop" => {
                let operation = payload["operation_id"].as_str().unwrap();
                let game_id = payload["game_id"].as_str().unwrap();
                if let Some(launch) = self
                    .provider_launches
                    .iter_mut()
                    .find(|launch| launch.operation == operation)
                {
                    launch.state = if self.provider_recovery_failure_once {
                        self.provider_recovery_failure_once = false;
                        "cleanup_failed"
                    } else {
                        "stopped"
                    };
                    return Self::provider_reply(launch);
                }
                let state = if self.provider_recovery_failure_once {
                    self.provider_recovery_failure_once = false;
                    "cleanup_failed"
                } else {
                    "stopped"
                };
                let launch = TestProviderLaunch {
                    operation: operation.into(),
                    game_id: game_id.into(),
                    ticket: "ef".repeat(24),
                    state,
                };
                let response = Self::provider_reply(&launch);
                self.provider_launches.push(launch);
                response
            }
            _ => panic!("unexpected Playnite action"),
        }
    }
    fn provider_reply(launch: &TestProviderLaunch) -> Value {
        json!({
            "ok":true,
            "ticket":launch.ticket,
            "game_id":launch.game_id,
            "state":launch.state
        })
    }
    fn send(&mut self, command: Command) -> Transport {
        Ok(match command {
            Command::Poll {} => self.incoming.take().unwrap_or(Response::Pending),
            Command::CheckRequest { .. } => {
                if self.root_active {
                    Response::Ok
                } else {
                    Response::error("request_ended", "cancelled")
                }
            }
            Command::Request {
                service,
                payload,
                parent,
                ..
            } => {
                let action = payload["action"].as_str().unwrap().to_owned();
                self.commands
                    .push((service.clone(), payload.clone(), parent));
                if action == "launch" && self.queue_spawn {
                    self.queued_spawn = Some(payload);
                    self.drop_spawn_result = true;
                    return Ok(Response::Accepted { id: 50 });
                }
                if action == "lookup" && self.queue_spawn {
                    assert!(
                        self.queued_spawn.is_none(),
                        "lookup must follow cancellation"
                    );
                    assert!(self.released > 0, "lookup must follow release");
                }
                let result = match service.as_str() {
                    crate::workflow::CATALOG => {
                        assert_eq!(parent, Some(7));
                        assert!(matches!(
                            action.as_str(),
                            "resolve_launch" | "resolve_exported_launch"
                        ));
                        let fixture = if self.catalog_exit {
                            "pipeline::tests::exiting_process"
                        } else {
                            "pipeline::tests::controlled_process"
                        };
                        let launch = self.catalog_playnite_game_id.as_ref().map_or_else(
                            || json!({"kind":"executable","program":std::env::current_exe().unwrap(),"arguments":["--exact",fixture,"--ignored"],"working_directory":null}),
                            |game_id| json!({"kind":"playnite","game_id":game_id,"exit_timeout_seconds":self.catalog_playnite_exit_timeout_seconds}),
                        );
                        let mut reply = json!({"ok":true,"id":payload["id"],"entry":{"name":"Test executable","launch":launch}});
                        let mut overrides = serde_json::Map::new();
                        if let Some(fec_percentage) = self.catalog_fec_percentage {
                            overrides.insert("fec_percentage".into(), json!(fec_percentage));
                        }
                        if let Some(audio_sink) = &self.catalog_audio_sink {
                            overrides.insert("audio_sink".into(), json!(audio_sink));
                        }
                        if let Some(capture_only) = self.catalog_audio_sink_capture_only {
                            overrides.insert("audio_sink_capture_only".into(), json!(capture_only));
                        }
                        if let Some(timeout) = self.catalog_back_button_timeout {
                            overrides.insert("back_button_timeout".into(), json!(timeout));
                        }
                        if !overrides.is_empty() {
                            reply["entry"]["config_overrides"] = json!(overrides);
                        }
                        if let Some(preparation) = &self.catalog_preparation {
                            reply["entry"]["launch"]["preparation"] = preparation.clone();
                        }
                        if let Some(exclude) = self.catalog_exclude_global_prep_cmd {
                            reply["entry"]["exclude_global_prep_cmd"] = json!(exclude);
                        }
                        if let Some(working_directory) = &self.catalog_working_directory {
                            reply["entry"]["launch"]["working_directory"] =
                                json!(working_directory);
                        }
                        if let Some(auto_detach) = self.catalog_auto_detach {
                            reply["entry"]["launch"]["auto_detach"] = json!(auto_detach);
                        }
                        if let Some(wait_all) = self.catalog_wait_all {
                            reply["entry"]["launch"]["wait_all"] = json!(wait_all);
                        }
                        if let Some(exit_timeout_seconds) = self.catalog_exit_timeout_seconds {
                            reply["entry"]["launch"]["exit_timeout_seconds"] =
                                json!(exit_timeout_seconds);
                        }
                        if let Some(detached) = &self.catalog_detached {
                            reply["entry"]["launch"]["detached"] = json!(detached);
                        }
                        if let Some(steam_id) = &self.catalog_steam_id {
                            reply["entry"]["steam_id"] = json!(steam_id);
                        }
                        if let Some(install_dir) = &self.catalog_steam_install_dir {
                            reply["entry"]["steam_install_dir"] = json!(install_dir);
                        }
                        if action == "resolve_exported_launch" {
                            reply["catalog_owner"] = json!("management");
                        }
                        reply
                    }
                    crate::workflow::SESSION => self.session(&payload),
                    crate::workflow::APPLICATION => self.application(&payload),
                    crate::workflow::PLAYNITE => self.playnite(&payload),
                    _ => panic!("unexpected dependency"),
                };
                if (action == "launch" && self.cancel_after_spawn)
                    || (action == "create" && self.cancel_after_session)
                {
                    self.root_active = false;
                    self.reply = Some(Response::error("cancelled", "mutation response hidden"));
                } else {
                    self.reply = Some(Response::Result { payload: result });
                }
                Response::Accepted { id: 50 }
            }
            Command::Cancel { request: 50 } => {
                if self.fail_cancel {
                    return Err("simulated cancellation transport failure".into());
                }
                self.queued_spawn = None;
                self.reply = Some(Response::error("cancelled", "cancelled"));
                Response::Ok
            }
            Command::ReleaseResult { request: 50 } => {
                if self.fail_release {
                    return Err("simulated release transport failure".into());
                }
                self.reply = None;
                self.released += 1;
                Response::Ok
            }
            Command::TakeResult { request: 50 } => {
                if self.drop_spawn_result
                    && self
                        .commands
                        .last()
                        .is_some_and(|(_, payload, _)| payload["action"] == "launch")
                {
                    self.drop_spawn_result = false;
                    self.reply = None;
                    return Err("simulated transport failure".into());
                }
                if self.drop_provider_start_result
                    && self
                        .commands
                        .last()
                        .is_some_and(|(_, payload, _)| payload["action"] == "launch_start")
                {
                    self.drop_provider_start_result = false;
                    self.reply = None;
                    return Err("simulated Playnite start acknowledgement loss".into());
                }
                self.reply.take().unwrap()
            }
            Command::Complete {
                request: 7,
                payload,
            } => {
                if self.reject_activation_completion && payload["launch"]["state"] == "active" {
                    self.root_active = false;
                    return Ok(Response::error("request_ended", "cancelled at completion"));
                }
                self.output = Some(payload);
                if self.root_active {
                    Response::Ok
                } else {
                    Response::error("request_ended", "cancelled")
                }
            }
            _ => panic!("unexpected runtime command"),
        })
    }
    fn request(&mut self, pipeline: &mut Pipeline, caller: &str, payload: Value) -> Value {
        self.request_as(pipeline, caller, caller, payload)
    }
    fn request_as(
        &mut self,
        pipeline: &mut Pipeline,
        caller: &str,
        initiator: &str,
        payload: Value,
    ) -> Value {
        self.output = None;
        self.incoming = Some(Response::Request {
            id: 7,
            caller: caller.into(),
            initiator: initiator.into(),
            service: SERVICE.into(),
            payload,
            remaining_ms: 30_000,
        });
        for _ in 0..100 {
            pipeline.tick(|command| self.send(command)).unwrap();
            if let Some(output) = self.output.take() {
                return output;
            }
        }
        panic!("launch workflow did not complete")
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        for process in &mut self.processes {
            if let Some(mut child) = process.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}
fn pipeline() -> Pipeline {
    Pipeline::new(BTreeMap::new()).unwrap()
}
fn launch(operation: &str) -> Value {
    json!({"action":"launch","operation_id":operation,"app_id":"app-0000000000000001"})
}
fn launch_export(operation: &str) -> Value {
    json!({"action":"launch_export","operation_id":operation,"app_id":"app-0000000000000001"})
}

#[test]
#[ignore = "test subprocess entrypoint"]
fn controlled_process() {
    std::thread::sleep(Duration::from_secs(60));
}
#[test]
#[ignore = "test subprocess entrypoint"]
fn exiting_process() {}

#[test]
fn canonical_catalog_preparation_is_forwarded_and_launch_waits_until_running() {
    let mut harness = Harness::new();
    let command = std::env::current_exe().unwrap();
    harness.catalog_preparation = Some(json!([{"run":{"executable":command}}]));
    harness.application_preparing_statuses = 2;
    let mut pipeline = pipeline();

    let started = harness.request(&mut pipeline, "alice", launch("prepared"));
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let application: Vec<_> = harness
        .commands
        .iter()
        .filter(|(service, _, _)| service == crate::workflow::APPLICATION)
        .map(|(_, payload, _)| payload)
        .collect();
    assert_eq!(application[0]["action"], "launch");
    assert_eq!(application[0]["cwd"], json!(command.parent().unwrap()));
    assert_eq!(application[0]["auto_detach"], true);
    assert_eq!(application[0]["wait_all"], true);
    assert_eq!(application[0]["exit_timeout_seconds"], 5);
    assert_eq!(application[0]["detached"], json!([]));
    assert_eq!(
        application[0]["preparation"],
        json!([{
            "run":{
                "executable":command,
                "args":[],
                "environment":{},
                "cwd":command.parent().unwrap(),
                "timeout_ms":10_000,
            },
            "undo":null,
        }])
    );
    assert_eq!(
        application
            .iter()
            .filter(|payload| payload["action"] == "status")
            .count(),
        3
    );
    let activate = harness
        .commands
        .iter()
        .position(|(_, payload, _)| payload["action"] == "activate")
        .unwrap();
    let last_status = harness
        .commands
        .iter()
        .rposition(|(_, payload, _)| payload["action"] == "status")
        .unwrap();
    assert!(activate > last_status);
}

#[test]
fn global_preparation_is_snapshotted_and_prepended_before_catalog_steps() {
    let mut harness = Harness::new();
    let command = std::env::current_exe().unwrap();
    let command_text = command.to_string_lossy().into_owned();
    harness.catalog_preparation = Some(json!([{
        "run":{"executable":command_text,"args":["app"]},
    }]));
    let global = json!([{
        "run":{"executable":command,"args":["global"]},
    }]);
    let mut request = launch("global-preparation");
    request["global_preparation"] = global.clone();
    let mut pipeline = pipeline();

    let started = harness.request(&mut pipeline, "alice", request.clone());
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let application_launch = harness
        .commands
        .iter()
        .find(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "launch"
        })
        .map(|(_, payload, _)| payload)
        .expect("launch workflow sends the merged preparation sequence");
    assert_eq!(
        application_launch["preparation"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        application_launch["preparation"][0]["run"]["args"],
        json!(["global"])
    );
    assert_eq!(
        application_launch["preparation"][1]["run"]["args"],
        json!(["app"])
    );
    assert_eq!(
        pipeline.records.values[&1].global_preparation[0].run.args,
        ["global"]
    );

    // A retry with a changed global setting cannot reuse the old launch ID.
    request["global_preparation"][0]["run"]["args"] = json!(["changed"]);
    assert_eq!(
        harness.request(&mut pipeline, "alice", request)["code"],
        "operation_conflict"
    );
}

#[test]
fn per_application_exclusion_keeps_app_preparation_and_skips_global_steps() {
    let mut harness = Harness::new();
    harness.catalog_exclude_global_prep_cmd = Some(true);
    harness.catalog_preparation = Some(json!([{
        "run":{"executable":std::env::current_exe().unwrap(),"args":["app"]},
    }]));
    let mut request = launch("global-preparation-excluded");
    request["global_preparation"] = json!([{
        "run":{"executable":std::env::current_exe().unwrap(),"args":["global"]},
    }]);
    let mut pipeline = pipeline();

    let started = harness.request(&mut pipeline, "alice", request);
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let application_launch = harness
        .commands
        .iter()
        .find(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "launch"
        })
        .map(|(_, payload, _)| payload)
        .expect("excluded app still launches");
    assert_eq!(
        application_launch["preparation"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        application_launch["preparation"][0]["run"]["args"],
        json!(["app"])
    );
}

#[test]
fn global_preparation_rejects_oversized_inputs_and_merged_sequences() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    let command = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let too_many = vec![json!({"run":{"executable":command}}); 9];
    let mut request = launch("global-preparation-input-limit");
    request["global_preparation"] = json!(too_many);
    assert_eq!(
        harness.request(&mut pipeline, "alice", request)["code"],
        "preparation_limit_exceeded"
    );
    assert!(pipeline.records.values.is_empty());

    let app_steps = (0..8)
        .map(|_| json!({"run":{"executable":command}}))
        .collect::<Vec<_>>();
    harness.catalog_preparation = Some(json!(app_steps));
    let mut request = launch("global-preparation-merged-limit");
    request["global_preparation"] = json!([{
        "run":{"executable":command},
    }]);
    let failed = harness.request(&mut pipeline, "alice", request);
    assert_eq!(failed["launch"]["state"], "failed", "{failed}");
    assert_eq!(failed["launch"]["failure"], "preparation_limit_exceeded");
    assert!(harness.sessions.is_empty());
}

#[test]
fn persisted_catalog_working_directory_reaches_application_launch_cwd() {
    let mut harness = Harness::new();
    let working_directory = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    harness.catalog_working_directory = Some(working_directory.clone());
    harness.catalog_auto_detach = Some(false);
    let mut pipeline = pipeline();

    let started = harness.request(&mut pipeline, "alice", launch("working-directory"));
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let application_launch = harness
        .commands
        .iter()
        .find(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "launch"
        })
        .map(|(_, payload, _)| payload)
        .expect("launch workflow sends an application launch request");
    assert_eq!(application_launch["cwd"], working_directory);
    assert_eq!(application_launch["auto_detach"], false);
}

#[test]
fn launch_result_retains_policy_from_the_catalog_revision_used_to_start_the_app() {
    let mut harness = Harness::new();
    harness.catalog_fec_percentage = Some(37);
    harness.catalog_audio_sink = Some(String::new());
    harness.catalog_audio_sink_capture_only = Some(false);
    harness.catalog_back_button_timeout = Some(-1);
    let mut pipeline = pipeline();

    let started = harness.request(&mut pipeline, "nvhttp", launch_export("fec-snapshot"));
    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert_eq!(started["launch"]["config_overrides"]["fec_percentage"], 37);
    assert_eq!(started["launch"]["config_overrides"]["audio_sink"], "");
    assert_eq!(
        started["launch"]["config_overrides"]["audio_sink_capture_only"],
        false
    );
    assert_eq!(
        started["launch"]["config_overrides"]["back_button_timeout"],
        -1
    );

    // Later catalog edits do not replace the policy captured by this launch.
    harness.catalog_fec_percentage = Some(81);
    harness.catalog_audio_sink = Some("later-endpoint".into());
    harness.catalog_audio_sink_capture_only = Some(true);
    harness.catalog_back_button_timeout = Some(300);
    let status = harness.request(
        &mut pipeline,
        "nvhttp",
        json!({"action":"status","launch_id":started["launch"]["launch_id"]}),
    );
    assert_eq!(status["launch"]["config_overrides"]["fec_percentage"], 37);
    assert_eq!(status["launch"]["config_overrides"]["audio_sink"], "");
    assert_eq!(
        status["launch"]["config_overrides"]["audio_sink_capture_only"],
        false
    );
    assert_eq!(
        status["launch"]["config_overrides"]["back_button_timeout"],
        -1
    );
    assert_eq!(
        harness
            .commands
            .iter()
            .filter(|(service, payload, _)| {
                service == crate::workflow::CATALOG
                    && payload["action"] == "resolve_exported_launch"
            })
            .count(),
        1
    );
}

#[test]
fn persisted_process_lifecycle_policy_reaches_application_launch() {
    let mut harness = Harness::new();
    harness.catalog_wait_all = Some(false);
    harness.catalog_exit_timeout_seconds = Some(0);
    harness.catalog_detached = Some(vec!["/opt/tools/overlay --launch".into()]);
    let mut pipeline = pipeline();

    let started = harness.request(&mut pipeline, "alice", launch("process-policy"));
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let application_launch = harness
        .commands
        .iter()
        .find(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "launch"
        })
        .map(|(_, payload, _)| payload)
        .expect("launch workflow sends an application launch request");
    assert_eq!(application_launch["wait_all"], false);
    assert_eq!(application_launch["exit_timeout_seconds"], 0);
    assert_eq!(
        application_launch["detached"],
        json!(["/opt/tools/overlay --launch"])
    );
}

#[test]
fn persisted_steam_identity_and_install_directory_reach_application_launch() {
    let mut harness = Harness::new();
    harness.catalog_steam_id = Some("42".into());
    harness.catalog_steam_install_dir = Some("/games/space game".into());
    let mut pipeline = pipeline();

    let started = harness.request(&mut pipeline, "alice", launch("steam-metadata"));
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let application_launch = harness
        .commands
        .iter()
        .find(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "launch"
        })
        .map(|(_, payload, _)| payload)
        .expect("launch workflow sends an application launch request");
    assert_eq!(application_launch["steam_id"], "42");
    assert_eq!(application_launch["steam_install_dir"], "/games/space game");
}

#[test]
fn persisted_catalog_working_directory_reaches_defaulted_preparation_and_undo_cwds() {
    let mut harness = Harness::new();
    let working_directory = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let explicit_directory = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let command = std::env::current_exe().unwrap();
    harness.catalog_working_directory = Some(working_directory.clone());
    harness.catalog_preparation = Some(json!([
        {
            "run":{"executable":command},
            "undo":{"executable":command},
        },
        {
            "run":{"executable":command,"cwd":explicit_directory},
            "undo":{"executable":command,"cwd":explicit_directory},
        }
    ]));
    let mut pipeline = pipeline();

    let started = harness.request(
        &mut pipeline,
        "alice",
        launch("working-directory-preparation"),
    );
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let application_launch = harness
        .commands
        .iter()
        .find(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "launch"
        })
        .map(|(_, payload, _)| payload)
        .expect("launch workflow sends an application launch request");
    assert_eq!(application_launch["cwd"], working_directory);
    assert_eq!(
        application_launch["preparation"][0]["run"]["cwd"],
        working_directory
    );
    assert_eq!(
        application_launch["preparation"][0]["undo"]["cwd"],
        working_directory
    );
    assert_eq!(
        application_launch["preparation"][1]["run"]["cwd"],
        explicit_directory
    );
    assert_eq!(
        application_launch["preparation"][1]["undo"]["cwd"],
        explicit_directory
    );
}

#[test]
fn preparation_failure_waits_through_stopping_before_releasing_the_lease() {
    let mut harness = Harness::new();
    harness.application_preparing_statuses = 1;
    harness.preparation_terminal = Some("launch_failed");
    harness.application_stopping_statuses = 2;
    let mut pipeline = pipeline();

    let failed = harness.request(&mut pipeline, "alice", launch("preparation-failed"));
    assert_eq!(failed["launch"]["state"], "failed", "{failed}");
    assert_eq!(failed["launch"]["failure"], "application_launch_failed");
    assert!(harness.sessions[0].lease.is_none());
    let finish = harness
        .commands
        .iter()
        .position(|(_, payload, _)| payload["action"] == "fail")
        .unwrap();
    let final_status = harness
        .commands
        .iter()
        .rposition(|(_, payload, _)| payload["action"] == "status")
        .unwrap();
    assert!(finish > final_status);
    assert!(
        !harness
            .commands
            .iter()
            .any(|(_, payload, _)| payload["action"] == "activate")
    );
}

#[test]
fn stop_waits_for_application_undo_before_finishing_the_session() {
    let mut harness = Harness::new();
    harness.application_stopping_statuses = 2;
    let mut pipeline = pipeline();
    assert_eq!(
        harness.request(&mut pipeline, "alice", launch("slow-undo"))["launch"]["state"],
        "active"
    );

    let stopped = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":1}),
    );
    assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
    let cleanup: Vec<_> = harness
        .commands
        .iter()
        .filter(|(_, _, parent)| parent.is_none())
        .map(|(_, payload, _)| payload["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        cleanup,
        vec![
            "stop",
            "terminate",
            "status",
            "status",
            "status",
            "finish_stop"
        ]
    );
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn application_cleanup_failure_holds_the_lease_until_explicit_stop_retry() {
    let mut harness = Harness::new();
    harness.fail_application_cleanup_once = true;
    let mut pipeline = pipeline();
    assert_eq!(
        harness.request(&mut pipeline, "alice", launch("undo-retry"))["launch"]["state"],
        "active"
    );

    let blocked = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":1}),
    );
    assert_eq!(blocked["launch"]["state"], "cleanup_failed", "{blocked}");
    assert_eq!(
        blocked["launch"]["cleanup_error"],
        "application_cleanup_failed"
    );
    assert!(harness.sessions[0].lease.is_some());
    assert!(
        !harness
            .commands
            .iter()
            .any(|(_, payload, _)| payload["action"] == "finish_stop")
    );

    let recovered = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":1}),
    );
    assert_eq!(recovered["launch"]["state"], "stopped", "{recovered}");
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn autonomous_cleanup_failure_preserves_application_outcome_for_retry() {
    let mut harness = Harness::new();
    harness.preparation_terminal = Some("launch_failed");
    harness.fail_automatic_cleanup_once = true;
    let mut pipeline = pipeline();

    let blocked = harness.request(&mut pipeline, "alice", launch("failed-undo"));
    assert_eq!(blocked["launch"]["state"], "cleanup_failed", "{blocked}");
    assert_eq!(blocked["launch"]["failure"], "application_launch_failed");
    assert_eq!(
        blocked["launch"]["cleanup_error"],
        "application_cleanup_failed"
    );
    assert!(harness.sessions[0].lease.is_some());

    let recovered = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":1}),
    );
    assert_eq!(recovered["launch"]["state"], "failed", "{recovered}");
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn real_process_launch_retry_owner_isolation_and_ordered_stop() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    let started = harness.request(&mut pipeline, "alice", launch("one"));
    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert_eq!(harness.processes.len(), 1);
    assert_eq!(harness.sessions[0].state, "active");
    assert_eq!(
        harness.request(&mut pipeline, "alice", launch("one")),
        started
    );
    assert_eq!(
        harness.request(&mut pipeline, "bob", json!({"action":"stop","launch_id":1}))["code"],
        "launch_not_found"
    );
    assert_eq!(
        harness.request(&mut pipeline, "bob", json!({"action":"list"}))["launches"],
        json!([])
    );
    let stopped = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":1}),
    );
    assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
    assert!(harness.processes[0].child.is_none());
    assert!(harness.sessions[0].lease.is_none());
    assert_eq!(
        harness.request(&mut pipeline, "alice", launch("one")),
        stopped
    );
    let cleanup: Vec<_> = harness
        .commands
        .iter()
        .filter(|(_, _, parent)| parent.is_none())
        .map(|(_, payload, _)| payload["action"].as_str().unwrap())
        .collect();
    assert_eq!(cleanup, vec!["stop", "terminate", "finish_stop"]);
}

#[test]
fn exported_launch_uses_canonical_owner_lease_and_supports_owned_lookup() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    let exported = harness.request(&mut pipeline, "nvhttp", launch_export("public-one"));
    assert_eq!(exported["launch"]["state"], "active", "{exported}");
    assert!(!exported.to_string().contains("management"));
    assert_eq!(
        pipeline.records.values[&1].catalog_principal.as_deref(),
        Some("management")
    );
    assert!(harness.commands.iter().any(|(_, payload, parent)| {
        payload["action"] == "resolve_exported_launch"
            && payload.get("owner").is_none()
            && *parent == Some(7)
    }));
    assert_eq!(
        harness.request(
            &mut pipeline,
            "nvhttp",
            json!({"action":"lookup","operation_id":"public-one"})
        ),
        exported
    );
    assert_eq!(
        harness.request(
            &mut pipeline,
            "foreign",
            json!({"action":"lookup","operation_id":"public-one"})
        )["code"],
        "launch_not_found"
    );

    let conflicting_owner_launch = harness.request(&mut pipeline, "management", launch("local"));
    assert_eq!(
        conflicting_owner_launch["launch"]["state"], "failed",
        "{conflicting_owner_launch}"
    );
    assert_eq!(
        pipeline.records.values[&1].resource,
        pipeline.records.values[&2].resource
    );
    assert_eq!(harness.processes.len(), 1);

    assert!(
        serde_json::from_value::<Operation>(json!({
            "action":"launch_export",
            "operation_id":"spoof",
            "app_id":"app-0000000000000001",
            "owner":"victim"
        }))
        .is_err()
    );
    assert_eq!(
        harness.request(&mut pipeline, "nvhttp", launch("public-one"))["code"],
        "operation_conflict"
    );
}

#[test]
fn exported_launch_retry_requires_the_original_runtime_initiator() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    let started = harness.request_as(
        &mut pipeline,
        "gateway",
        "consumer-a",
        launch_export("delegated"),
    );
    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert_eq!(pipeline.records.values[&1].catalog_initiator, "consumer-a");
    let conflict = harness.request_as(
        &mut pipeline,
        "gateway",
        "consumer-b",
        launch_export("delegated"),
    );
    assert_eq!(conflict["code"], "operation_conflict");
    assert_eq!(harness.processes.len(), 1);
}

#[test]
fn exported_launch_cancellation_reconciles_without_respawn() {
    let mut harness = Harness::new();
    harness.cancel_after_spawn = true;
    let mut pipeline = pipeline();
    let result = harness.request(&mut pipeline, "nvhttp", launch_export("cancelled-export"));
    assert_eq!(result["launch"]["state"], "cancelled", "{result}");
    assert_eq!(harness.processes.len(), 1);
    assert!(harness.processes[0].child.is_none());
    assert!(harness.sessions[0].lease.is_none());
    assert_eq!(
        pipeline.records.values[&1].catalog_principal.as_deref(),
        Some("management")
    );
}

#[test]
fn exported_launch_natural_exit_is_visible_through_lookup() {
    let mut harness = Harness::new();
    harness.catalog_exit = true;
    let mut pipeline = pipeline();
    assert_eq!(
        harness.request(&mut pipeline, "nvhttp", launch_export("export-exit"))["launch"]["state"],
        "active"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while pipeline.records.values[&1].state != State::Stopped {
        pipeline.tick(|command| harness.send(command)).unwrap();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let stopped = harness.request(
        &mut pipeline,
        "nvhttp",
        json!({"action":"lookup","operation_id":"export-exit"}),
    );
    assert_eq!(stopped["launch"]["state"], "stopped");
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn cancellation_after_committed_spawn_recovers_id_without_respawning() {
    let mut harness = Harness::new();
    harness.cancel_after_spawn = true;
    let mut pipeline = pipeline();
    let result = harness.request(&mut pipeline, "alice", launch("cancelled"));
    assert_eq!(result["launch"]["state"], "cancelled", "{result}");
    assert_eq!(harness.processes.len(), 1);
    assert!(harness.processes[0].child.is_none());
    assert!(harness.sessions[0].lease.is_none());
    assert!(
        harness
            .commands
            .iter()
            .any(
                |(service, payload, parent)| service == crate::workflow::APPLICATION
                    && payload["action"] == "lookup"
                    && parent.is_none()
            )
    );
}

#[test]
fn cancellation_after_session_create_recovers_owned_key_and_releases_session() {
    let mut harness = Harness::new();
    harness.cancel_after_session = true;
    let mut pipeline = pipeline();
    let result = harness.request(&mut pipeline, "alice", launch("cancelled-create"));
    assert_eq!(result["launch"]["state"], "cancelled", "{result}");
    assert!(harness.processes.is_empty());
    assert_eq!(harness.sessions[0].state, "stopped");
    assert!(
        harness
            .commands
            .iter()
            .any(
                |(service, payload, parent)| service == crate::workflow::SESSION
                    && payload["action"] == "lookup"
                    && parent.is_none()
            )
    );
}

#[test]
fn activation_failure_rolls_back_and_kill_failure_retains_lease_for_retry() {
    let mut harness = Harness::new();
    harness.fail_activate = true;
    harness.fail_terminate = true;
    let mut pipeline = pipeline();
    let failed = harness.request(&mut pipeline, "alice", launch("failure"));
    assert_eq!(failed["launch"]["state"], "cleanup_failed", "{failed}");
    assert!(harness.processes[0].child.is_some());
    assert!(harness.sessions[0].lease.is_some());
    assert!(!harness.commands.iter().any(|(_, payload, _)| matches!(
        payload["action"].as_str(),
        Some("fail" | "finish_stop" | "cancel")
    )));
    harness.fail_terminate = false;
    let recovered = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":1}),
    );
    assert_eq!(recovered["launch"]["state"], "failed", "{recovered}");
    assert!(harness.processes[0].child.is_none());
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn natural_process_exit_finishes_session_without_external_stop() {
    let mut harness = Harness::new();
    harness.catalog_exit = true;
    let mut pipeline = pipeline();
    assert_eq!(
        harness.request(&mut pipeline, "alice", launch("exit"))["launch"]["state"],
        "active"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        pipeline.tick(|command| harness.send(command)).unwrap();
        if pipeline.records.values[&1].state == State::Stopped {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(harness.sessions[0].lease.is_none());
    assert!(harness.processes[0].child.is_none());
}

#[test]
fn conflicting_operations_and_exclusive_catalog_lease_do_not_spawn_extra_process() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    harness.request(&mut pipeline, "alice", launch("one"));
    assert_eq!(
        harness.request(
            &mut pipeline,
            "alice",
            json!({"action":"launch","operation_id":"one","app_id":"app-0000000000000002"})
        )["code"],
        "operation_conflict"
    );
    let duplicate = harness.request(&mut pipeline, "alice", launch("two"));
    assert_eq!(duplicate["launch"]["state"], "failed", "{duplicate}");
    assert_eq!(harness.processes.len(), 1);
    assert!(harness.sessions[0].lease.is_some());
    assert!(harness.sessions[1].lease.is_none());
}

#[test]
fn sustained_status_traffic_cannot_starve_natural_exit_cleanup() {
    let mut harness = Harness::new();
    harness.catalog_exit = true;
    let mut pipeline = pipeline();
    harness.request(&mut pipeline, "alice", launch("exit-under-load"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut requests = 0;
    while pipeline.records.values[&1].state != State::Stopped {
        harness.request(
            &mut pipeline,
            "alice",
            json!({"action":"status","launch_id":1}),
        );
        requests += 1;
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(requests > 0);
    assert!(harness.sessions[0].lease.is_none());
    assert!(
        harness
            .commands
            .iter()
            .any(|(_, payload, parent)| payload["action"] == "status" && parent.is_none())
    );
}

#[test]
fn lost_transport_result_after_spawn_reconciles_and_cleans_up() {
    let mut harness = Harness::new();
    harness.drop_spawn_result = true;
    let mut pipeline = pipeline();
    let result = harness.request(&mut pipeline, "alice", launch("transport-loss"));
    assert_eq!(result["launch"]["state"], "failed", "{result}");
    assert_eq!(harness.processes.len(), 1);
    assert!(harness.processes[0].child.is_none());
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn cancellation_at_activation_response_rolls_back_running_process() {
    let mut harness = Harness::new();
    harness.reject_activation_completion = true;
    let mut pipeline = pipeline();
    let result = harness.request(&mut pipeline, "alice", launch("completion-cancel"));
    assert_eq!(result["launch"]["state"], "cancelled", "{result}");
    assert!(harness.processes[0].child.is_none());
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn lost_result_before_queued_spawn_cancels_and_releases_before_reconciliation() {
    let mut harness = Harness::new();
    harness.queue_spawn = true;
    let mut pipeline = pipeline();
    let result = harness.request(&mut pipeline, "alice", launch("queued"));
    assert_eq!(result["launch"]["state"], "failed");
    assert!(
        harness.queued_spawn.is_none(),
        "no queued mutation can run after lookup"
    );
    assert!(harness.processes.is_empty());
    assert_eq!(harness.released, 1);
    assert!(harness.sessions[0].lease.is_none());
    assert!(pipeline.records.values[&1].outstanding_request.is_none());
}

#[test]
fn unconfirmed_cancellation_or_release_preserves_request_and_lease_for_retry() {
    for fail_cancel in [true, false] {
        let mut harness = Harness::new();
        harness.queue_spawn = true;
        harness.fail_cancel = fail_cancel;
        harness.fail_release = !fail_cancel;
        let mut pipeline = pipeline();
        let result = harness.request(&mut pipeline, "alice", launch("queued"));
        assert_eq!(result["launch"]["state"], "cleanup_failed");
        assert_eq!(pipeline.records.values[&1].outstanding_request, Some(50));
        assert!(harness.sessions[0].lease.is_some());
        assert!(
            !harness
                .commands
                .iter()
                .any(|(_, payload, _)| payload["action"] == "lookup")
        );
        assert_eq!(harness.released, 0);
        harness.fail_cancel = false;
        harness.fail_release = false;
        let result = harness.request(
            &mut pipeline,
            "alice",
            json!({"action":"stop","launch_id":1}),
        );
        assert_eq!(result["launch"]["state"], "failed");
        assert!(harness.queued_spawn.is_none());
        assert!(harness.processes.is_empty());
        assert!(harness.sessions[0].lease.is_none());
        assert!(pipeline.records.values[&1].outstanding_request.is_none());
        assert_eq!(harness.released, 1);
    }
}

fn playnite_catalog(game_id: &str, exit_timeout_seconds: u64) -> Harness {
    let mut harness = Harness::new();
    harness.catalog_playnite_game_id = Some(game_id.into());
    harness.catalog_playnite_exit_timeout_seconds = exit_timeout_seconds;
    harness
}

#[test]
fn typed_playnite_launch_uses_provider_ticket_and_holds_lease_until_stop_confirmation() {
    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 300);
    let mut pipeline = pipeline();
    let started = harness.request(&mut pipeline, "alice", launch("playnite-start"));
    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert!(harness.sessions[0].lease.is_some());
    assert!(
        harness.processes.is_empty(),
        "Playnite GUIDs never become executable commands"
    );
    let start_request = harness
        .commands
        .iter()
        .find(|(service, payload, _)| {
            service == crate::workflow::PLAYNITE && payload["action"] == "launch_start"
        })
        .unwrap();
    assert_eq!(
        start_request.1["game_id"],
        "a1b2c3d4-1234-5678-9abc-0123456789ab"
    );
    assert_eq!(
        harness.request(
            &mut pipeline,
            "alice",
            json!({"action":"stop","launch_id":started["launch"]["launch_id"]}),
        )["launch"]["state"],
        "stopped"
    );
    assert!(harness.sessions[0].lease.is_none());
    let stop_request = harness
        .commands
        .iter()
        .find(|(service, payload, _)| {
            service == crate::workflow::PLAYNITE && payload["action"] == "launch_stop"
        })
        .unwrap();
    assert_eq!(stop_request.1["exit_timeout_seconds"], 300);
}

#[test]
fn global_preparation_gates_playnite_and_undo_follows_provider_stop() {
    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 10);
    harness.global_preparation_statuses = 2;
    let command = std::env::current_exe().unwrap();
    let mut request = launch("playnite-global-preparation");
    request["global_preparation"] = json!([{
        "run":{"executable":command},
        "undo":{"executable":command},
    }]);
    let mut pipeline = pipeline();
    let started = harness.request(&mut pipeline, "alice", request);
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let provider_start = harness
        .commands
        .iter()
        .position(|(service, payload, _)| {
            service == crate::workflow::PLAYNITE && payload["action"] == "launch_start"
        })
        .unwrap();
    let prep_start = harness
        .commands
        .iter()
        .position(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "prepare"
        })
        .unwrap();
    let prep_polls = harness
        .commands
        .iter()
        .filter(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "preparation_status"
        })
        .count();
    assert!(prep_start < provider_start);
    assert_eq!(
        prep_polls, 3,
        "Playnite started before global preparation was ready"
    );

    let stopped = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":started["launch"]["launch_id"]}),
    );
    assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
    let cleanup_actions: Vec<_> = harness
        .commands
        .iter()
        .filter(|(_, _, parent)| parent.is_none())
        .map(|(service, payload, _)| (service.as_str(), payload["action"].as_str().unwrap()))
        .collect();
    assert_eq!(
        cleanup_actions,
        vec![
            (crate::workflow::PLAYNITE, "launch_stop"),
            (crate::workflow::APPLICATION, "preparation_stop"),
            (crate::workflow::SESSION, "stop"),
            (crate::workflow::SESSION, "finish_stop"),
        ]
    );
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn playnite_global_preparation_failure_prevents_provider_start_and_releases_lease() {
    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 10);
    harness.fail_global_preparation = true;
    let command = std::env::current_exe().unwrap();
    let mut request = launch("playnite-global-preparation-failure");
    request["global_preparation"] = json!([{"run":{"executable":command}}]);
    let mut first_pipeline = pipeline();
    let failed = harness.request(&mut first_pipeline, "alice", request);
    assert_eq!(failed["launch"]["state"], "failed", "{failed}");
    assert_eq!(failed["launch"]["failure"], "global_preparation_failed");
    assert!(harness.sessions[0].lease.is_none());
    assert!(harness.provider_launches.is_empty());
    assert!(!harness.commands.iter().any(|(_, payload, _)| {
        matches!(
            payload["action"].as_str(),
            Some("launch_start" | "fullscreen_start")
        )
    }));
    let cleanup_actions: Vec<_> = harness
        .commands
        .iter()
        .filter(|(_, _, parent)| parent.is_none())
        .map(|(_, payload, _)| payload["action"].as_str().unwrap())
        .collect();
    assert_eq!(cleanup_actions, ["preparation_stop", "stop", "fail"]);
}

#[test]
fn playnite_exclusion_skips_global_preparation_but_still_starts_the_provider() {
    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 10);
    harness.catalog_exclude_global_prep_cmd = Some(true);
    let command = std::env::current_exe().unwrap();
    let mut request = launch("playnite-excludes-global-preparation");
    request["global_preparation"] = json!([{"run":{"executable":command}}]);
    let mut pipeline = pipeline();
    let started = harness.request(&mut pipeline, "alice", request);
    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert!(harness.preparations.is_empty());
    assert!(!harness.commands.iter().any(|(service, payload, _)| {
        service == crate::workflow::APPLICATION && payload["action"] == "prepare"
    }));
    assert!(harness.commands.iter().any(|(service, payload, _)| {
        service == crate::workflow::PLAYNITE && payload["action"] == "launch_start"
    }));
}

#[test]
fn failed_global_preparation_undo_holds_playnite_session_until_stop_retry() {
    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 10);
    harness.fail_global_preparation_cleanup_once = true;
    let command = std::env::current_exe().unwrap();
    let mut request = launch("playnite-global-preparation-undo-retry");
    request["global_preparation"] = json!([{"run":{"executable":command}}]);
    let mut pipeline = pipeline();
    let started = harness.request(&mut pipeline, "alice", request);
    let blocked = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":started["launch"]["launch_id"]}),
    );
    assert_eq!(blocked["launch"]["state"], "cleanup_failed", "{blocked}");
    assert_eq!(
        blocked["launch"]["cleanup_error"],
        "global_preparation_cleanup_failed"
    );
    assert!(harness.sessions[0].lease.is_some());
    assert!(
        !harness
            .commands
            .iter()
            .any(|(_, payload, _)| { payload["action"] == "finish_stop" })
    );

    let recovered = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":started["launch"]["launch_id"]}),
    );
    assert_eq!(recovered["launch"]["state"], "stopped", "{recovered}");
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn provider_start_and_stop_uncertainty_keep_global_preparation_owned_until_provider_stops() {
    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 10);
    harness.drop_provider_start_result = true;
    let command = std::env::current_exe().unwrap();
    let mut request = launch("playnite-preparation-provider-uncertainty");
    request["global_preparation"] = json!([{"run":{"executable":command}}]);
    let mut first_pipeline = pipeline();
    let failed = harness.request(&mut first_pipeline, "alice", request);
    assert_eq!(failed["launch"]["state"], "failed", "{failed}");
    let actions: Vec<_> = harness
        .commands
        .iter()
        .map(|(_, payload, _)| payload["action"].as_str().unwrap())
        .collect();
    assert!(
        actions
            .iter()
            .position(|action| *action == "launch_lookup")
            .unwrap()
            < actions
                .iter()
                .position(|action| *action == "preparation_stop")
                .unwrap()
    );
    assert!(
        actions
            .iter()
            .position(|action| *action == "launch_stop")
            .unwrap()
            < actions
                .iter()
                .position(|action| *action == "preparation_stop")
                .unwrap()
    );
    assert!(harness.sessions[0].lease.is_none());

    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 10);
    harness.provider_stop_failure_once = true;
    let command = std::env::current_exe().unwrap();
    let mut request = launch("playnite-preparation-stop-uncertainty");
    request["global_preparation"] = json!([{"run":{"executable":command}}]);
    let mut second_pipeline = pipeline();
    let started = harness.request(&mut second_pipeline, "alice", request);
    let blocked = harness.request(
        &mut second_pipeline,
        "alice",
        json!({"action":"stop","launch_id":started["launch"]["launch_id"]}),
    );
    assert_eq!(blocked["launch"]["state"], "cleanup_failed", "{blocked}");
    assert!(harness.sessions[0].lease.is_some());
    assert!(
        !harness
            .commands
            .iter()
            .any(|(_, payload, _)| { payload["action"] == "preparation_stop" })
    );

    let recovered = harness.request(
        &mut second_pipeline,
        "alice",
        json!({"action":"stop","launch_id":started["launch"]["launch_id"]}),
    );
    assert_eq!(recovered["launch"]["state"], "stopped", "{recovered}");
    let stop_index = harness
        .commands
        .iter()
        .rposition(|(_, payload, _)| payload["action"] == "launch_stop")
        .unwrap();
    let preparation_stop_index = harness
        .commands
        .iter()
        .position(|(_, payload, _)| payload["action"] == "preparation_stop")
        .unwrap();
    assert!(stop_index < preparation_stop_index);
    assert!(harness.sessions[0].lease.is_none());
}

#[test]
fn exact_not_started_marker_releases_only_after_a_preflight_rejection() {
    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 10);
    harness.provider_start_rejection =
        Some(json!({"ok":false,"code":"playnite_not_running","launch_state":"not_started"}));
    let mut pipeline = pipeline();
    let failed = harness.request(&mut pipeline, "alice", launch("known-no-start"));
    assert_eq!(failed["launch"]["state"], "failed", "{failed}");
    assert_eq!(failed["launch"]["failure"], "playnite_launch_rejected");
    assert!(harness.sessions[0].lease.is_none());
    assert!(harness.provider_launches.is_empty());
    assert!(
        !harness.commands.iter().any(|(_, payload, _)| matches!(
            payload["action"].as_str(),
            Some("launch_lookup" | "launch_recover_stop")
        )),
        "a strict preflight proof makes speculative recovery unnecessary"
    );
}

#[test]
fn lost_start_ack_is_reconciled_by_operation_lookup_and_stopped_without_respawn() {
    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 10);
    harness.drop_provider_start_result = true;
    let mut pipeline = pipeline();
    let failed = harness.request(&mut pipeline, "alice", launch("lost-playnite-ack"));
    assert_eq!(failed["launch"]["state"], "failed", "{failed}");
    assert_eq!(harness.provider_launches.len(), 1);
    assert_eq!(harness.provider_launches[0].state, "stopped");
    assert!(harness.sessions[0].lease.is_none());
    assert_eq!(
        harness
            .commands
            .iter()
            .filter(|(service, payload, _)| {
                service == crate::workflow::PLAYNITE && payload["action"] == "launch_start"
            })
            .count(),
        1,
        "the acknowledged start is resolved by lookup, never replayed"
    );
    assert!(harness.commands.iter().any(|(service, payload, parent)| {
        service == crate::workflow::PLAYNITE
            && payload["action"] == "launch_lookup"
            && parent.is_none()
    }));
}

#[test]
fn malformed_not_started_marker_and_missing_lookup_hold_lease_until_recovery_retry() {
    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 10);
    harness.provider_start_rejection = Some(json!({
        "ok":false,
        "code":"playnite_not_running",
        "launch_state":"not_started",
        "untrusted_extra":true
    }));
    harness.provider_recovery_failure_once = true;
    let mut pipeline = pipeline();
    let uncertain = harness.request(&mut pipeline, "alice", launch("uncertain-playnite-start"));
    assert_eq!(
        uncertain["launch"]["state"], "cleanup_failed",
        "{uncertain}"
    );
    assert!(harness.sessions[0].lease.is_some());
    assert!(
        harness
            .commands
            .iter()
            .any(|(_, payload, _)| { payload["action"] == "launch_lookup" })
    );
    assert!(
        harness
            .commands
            .iter()
            .any(|(_, payload, _)| { payload["action"] == "launch_recover_stop" })
    );

    let recovered = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":uncertain["launch"]["launch_id"]}),
    );
    assert_eq!(recovered["launch"]["state"], "failed", "{recovered}");
    assert!(harness.sessions[0].lease.is_none());
    assert_eq!(
        harness
            .commands
            .iter()
            .filter(|(_, payload, _)| payload["action"] == "launch_recover_stop")
            .count(),
        1,
        "the first recovery is unconfirmed; its returned ticket makes an explicit stop retry safe"
    );
    assert!(harness.commands.iter().any(|(service, payload, _)| {
        service == crate::workflow::PLAYNITE && payload["action"] == "launch_stop"
    }));
}

#[test]
fn missing_provider_status_after_module_restart_reacquires_extension_and_stops_before_release() {
    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 10);
    let mut pipeline = pipeline();
    let started = harness.request(&mut pipeline, "alice", launch("provider-restart"));
    assert_eq!(started["launch"]["state"], "active");
    harness.provider_status_not_found_once = true;

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while harness.sessions[0].lease.is_some()
        || (pipeline.records.values[&1].state != State::Failed
            && pipeline.records.values[&1].state != State::Stopped)
    {
        pipeline.tick(|command| harness.send(command)).unwrap();
        assert!(
            std::time::Instant::now() < deadline,
            "provider recovery stalled"
        );
    }
    assert_eq!(
        pipeline.records.values[&1].response()["launch"]["state"],
        "failed"
    );
    assert!(harness.commands.iter().any(|(service, payload, _)| {
        service == crate::workflow::PLAYNITE && payload["action"] == "launch_recover_stop"
    }));
    assert!(harness.sessions[0].lease.is_none());
}
