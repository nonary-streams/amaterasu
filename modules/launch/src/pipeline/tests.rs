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
    catalog_provenance: Option<Value>,
    lutris_status: Option<Value>,
    lutris_status_sequence: std::collections::VecDeque<Value>,
    lutris_transport_unavailable: bool,
    drop_spawn_result: bool,
    reject_activation_completion: bool,
    queue_spawn: bool,
    queued_spawn: Option<Value>,
    fail_cancel: bool,
    fail_release: bool,
    released: usize,
    catalog_preparation: Option<Value>,
    catalog_raw_preparation: Option<Value>,
    catalog_output: Option<Value>,
    catalog_exclude_global_prep_cmd: Option<bool>,
    catalog_config_overrides: Option<Value>,
    catalog_working_directory: Option<String>,
    catalog_auto_detach: Option<bool>,
    catalog_wait_all: Option<bool>,
    catalog_exit_timeout_seconds: Option<u64>,
    catalog_detached: Option<Value>,
    catalog_steam_id: Option<String>,
    catalog_steam_install_dir: Option<String>,
    catalog_fec_percentage: Option<u8>,
    catalog_max_bitrate: Option<i32>,
    catalog_minimum_fps_target: Option<f64>,
    catalog_video_max_batch_size_kb: Option<u8>,
    catalog_stream_audio: Option<bool>,
    catalog_audio_sink: Option<String>,
    catalog_audio_sink_capture_only: Option<bool>,
    catalog_back_button_timeout: Option<i32>,
    catalog_amd_av1_latency_mode: Option<Value>,
    catalog_nvenc_spatial_aq: Option<Value>,
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
            catalog_provenance: None,
            lutris_status: None,
            lutris_status_sequence: std::collections::VecDeque::new(),
            lutris_transport_unavailable: false,
            drop_spawn_result: false,
            reject_activation_completion: false,
            queue_spawn: false,
            queued_spawn: None,
            fail_cancel: false,
            fail_release: false,
            released: 0,
            catalog_preparation: None,
            catalog_raw_preparation: None,
            catalog_output: None,
            catalog_exclude_global_prep_cmd: None,
            catalog_config_overrides: None,
            catalog_working_directory: None,
            catalog_auto_detach: None,
            catalog_wait_all: None,
            catalog_exit_timeout_seconds: None,
            catalog_detached: None,
            catalog_steam_id: None,
            catalog_steam_install_dir: None,
            catalog_fec_percentage: None,
            catalog_max_bitrate: None,
            catalog_minimum_fps_target: None,
            catalog_video_max_batch_size_kb: None,
            catalog_stream_audio: None,
            catalog_audio_sink: None,
            catalog_audio_sink_capture_only: None,
            catalog_back_button_timeout: None,
            catalog_amd_av1_latency_mode: None,
            catalog_nvenc_spatial_aq: None,
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
                if service == crate::workflow::LUTRIS && self.lutris_transport_unavailable {
                    self.reply = Some(Response::error("service_unavailable", "Lutris unavailable"));
                    return Ok(Response::Accepted { id: 50 });
                }
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
                        let mut reply = json!({"ok":true,"revision":1,"id":payload["id"],"entry":{"name":"Test executable","launch":launch}});
                        let mut overrides = serde_json::Map::new();
                        if let Some(fec_percentage) = self.catalog_fec_percentage {
                            overrides.insert("fec_percentage".into(), json!(fec_percentage));
                        }
                        if let Some(max_bitrate) = self.catalog_max_bitrate {
                            overrides.insert("max_bitrate".into(), json!(max_bitrate));
                        }
                        if let Some(minimum_fps_target) = self.catalog_minimum_fps_target {
                            overrides
                                .insert("minimum_fps_target".into(), json!(minimum_fps_target));
                        }
                        if let Some(video_max_batch_size_kb) = self.catalog_video_max_batch_size_kb
                        {
                            overrides.insert(
                                "video_max_batch_size_kb".into(),
                                json!(video_max_batch_size_kb),
                            );
                        }
                        if let Some(stream_audio) = self.catalog_stream_audio {
                            overrides.insert("stream_audio".into(), json!(stream_audio));
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
                        if let Some(mode) = &self.catalog_amd_av1_latency_mode {
                            overrides.insert("amd_av1_latency_mode".into(), mode.clone());
                        }
                        if let Some(enabled) = &self.catalog_nvenc_spatial_aq {
                            overrides.insert("nvenc_spatial_aq".into(), enabled.clone());
                        }
                        if !overrides.is_empty() {
                            reply["entry"]["config_overrides"] = json!(overrides);
                        }
                        if let Some(config_overrides) = &self.catalog_config_overrides {
                            let entry = reply["entry"].as_object_mut().unwrap();
                            let overrides = entry
                                .entry("config_overrides")
                                .or_insert_with(|| json!({}))
                                .as_object_mut()
                                .unwrap();
                            for (key, value) in config_overrides.as_object().unwrap() {
                                overrides.insert(key.clone(), value.clone());
                            }
                        }
                        if let Some(preparation) = &self.catalog_preparation {
                            reply["entry"]["launch"]["preparation"] = preparation.clone();
                        }
                        if let Some(preparation) = &self.catalog_raw_preparation {
                            reply["entry"]["prep_cmd"] = preparation.clone();
                        }
                        if let Some(output) = &self.catalog_output {
                            reply["entry"]["output"] = output.clone();
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
                        if let Some(provenance) = &self.catalog_provenance {
                            reply["provider_provenance"] = provenance.clone();
                        }
                        reply
                    }
                    crate::workflow::SESSION => self.session(&payload),
                    crate::workflow::APPLICATION => self.application(&payload),
                    crate::workflow::PLAYNITE => self.playnite(&payload),
                    crate::workflow::LUTRIS => self.lutris_status_sequence.pop_front()
                        .or_else(|| self.lutris_status.clone()).unwrap_or_else(|| {
                            json!({"ok":true,"settings_ready":true,"enabled":true,"available":true})
                        }),
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
fn retained_lutris_launch_requires_live_enabled_available_status_before_session() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    harness.catalog_provenance = Some(json!({"provider":"amaterasu.lutris","source_id":"42"}));
    harness.lutris_status =
        Some(json!({"ok":true,"settings_ready":true,"enabled":false,"available":true}));
    let disabled = harness.request(&mut pipeline, "alice", launch("lutris-disabled"));
    assert_eq!(
        disabled["launch"]["failure"], "lutris_unavailable",
        "{disabled}"
    );
    assert!(harness.sessions.is_empty());
    assert!(harness.processes.is_empty());

    harness.lutris_status = None;
    let enabled = harness.request(&mut pipeline, "alice", launch("lutris-enabled"));
    assert_eq!(enabled["ok"], true, "{enabled}");
    assert_eq!(harness.sessions.len(), 1);
    assert_eq!(harness.processes.len(), 1);
    assert!(harness.commands.iter().any(|(service, request, _)| {
        service == crate::workflow::LUTRIS && request["action"] == "status"
    }));
}

#[test]
fn exported_lutris_preflight_rechecks_status_at_launch_admission() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    harness.catalog_provenance = Some(json!({"provider":"amaterasu.lutris","source_id":"42"}));
    let preflight = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"preflight_export","operation_id":"lutris-export","app_id":"app-0000000000000001"}),
    );
    assert_eq!(preflight["ok"], true, "{preflight}");
    assert!(
        harness
            .commands
            .iter()
            .all(|(service, _, _)| service != crate::workflow::LUTRIS)
    );
    harness.lutris_status =
        Some(json!({"ok":true,"settings_ready":true,"enabled":false,"available":true}));
    let mut launch = launch_export("lutris-export");
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();
    let denied = harness.request(&mut pipeline, "amaterasu.nvhttp", launch);
    assert_eq!(
        denied["launch"]["failure"], "lutris_unavailable",
        "{denied}"
    );
    assert!(harness.sessions.is_empty());
}

#[test]
fn lutris_admission_fails_closed_on_unavailable_or_malformed_status_and_provenance() {
    for status in [
        json!({"ok":false}),
        json!({"ok":true,"settings_ready":true,"enabled":true,"available":false}),
        json!({"ok":true,"settings_ready":true,"enabled":true}),
    ] {
        let mut harness = Harness::new();
        harness.catalog_provenance = Some(json!({"provider":"amaterasu.lutris","source_id":"42"}));
        harness.lutris_status = Some(status);
        let rejected = harness.request(&mut pipeline(), "alice", launch("status-reject"));
        assert_eq!(
            rejected["launch"]["failure"], "lutris_unavailable",
            "{rejected}"
        );
        assert!(harness.sessions.is_empty());
    }
    let mut harness = Harness::new();
    harness.catalog_provenance = Some(json!({"provider":"amaterasu.lutris","source_id":"042"}));
    let rejected = harness.request(&mut pipeline(), "alice", launch("bad-provenance"));
    assert_eq!(
        rejected["launch"]["failure"], "invalid_catalog_response",
        "{rejected}"
    );
    assert!(harness.sessions.is_empty());

    let mut harness = Harness::new();
    harness.catalog_provenance = Some(json!({"provider":"amaterasu.lutris","source_id":"42"}));
    harness.lutris_transport_unavailable = true;
    let unavailable = harness.request(&mut pipeline(), "alice", launch("unavailable-provider"));
    assert_eq!(unavailable["launch"]["state"], "failed", "{unavailable}");
    assert!(harness.sessions.is_empty());
    assert!(harness.processes.is_empty());
}

#[test]
fn lutris_disable_after_session_acquisition_prevents_process_spawn_and_cleans_up() {
    let mut harness = Harness::new();
    harness.catalog_provenance = Some(json!({"provider":"amaterasu.lutris","source_id":"42"}));
    harness
        .lutris_status_sequence
        .push_back(json!({"ok":true,"settings_ready":true,"enabled":true,"available":true}));
    harness
        .lutris_status_sequence
        .push_back(json!({"ok":true,"settings_ready":true,"enabled":false,"available":true}));
    let rejected = harness.request(&mut pipeline(), "alice", launch("disable-before-spawn"));
    assert_eq!(
        rejected["launch"]["failure"], "lutris_unavailable",
        "{rejected}"
    );
    assert!(harness.processes.is_empty());
    assert_eq!(harness.sessions.len(), 1);
    assert_eq!(harness.sessions[0].state, "stopped");
    assert_eq!(harness.sessions[0].lease, None);
    assert_eq!(
        harness
            .commands
            .iter()
            .filter(|(service, _, _)| service == crate::workflow::LUTRIS)
            .count(),
        2,
    );
    assert!(harness.commands.iter().all(|(service, request, _)| {
        service != crate::workflow::APPLICATION || request["action"] != "launch"
    }));
}

#[test]
fn launch_export_accepts_only_object_shaped_frame_limiter_compatibility_fields() {
    let candidate: Operation = serde_json::from_value(json!({
        "action":"launch_export",
        "operation_id":"compatibility-objects",
        "app_id":"app-0000000000000001",
        "frame_limiter_settings":{"enabled":true,"provider":"auto"},
        "client_frame_limiter_overrides":{"fps_limit":120},
    }))
    .unwrap();
    candidate.validate().unwrap();
    let Operation::LaunchExport {
        frame_limiter_settings,
        client_frame_limiter_overrides,
        ..
    } = candidate
    else {
        panic!("expected launch export operation");
    };
    assert_eq!(frame_limiter_settings.unwrap()["provider"], "auto");
    assert_eq!(client_frame_limiter_overrides.unwrap()["fps_limit"], 120);

    let legacy: Operation = serde_json::from_value(launch_export("legacy-omitted")).unwrap();
    let Operation::LaunchExport {
        frame_limiter_settings,
        client_frame_limiter_overrides,
        preflight_token,
        display_admission_token,
        virtual_display_intent,
        proton_dualsense_compatibility,
        return_on_admission,
        ..
    } = legacy
    else {
        panic!("expected launch export operation");
    };
    assert_eq!(frame_limiter_settings, None);
    assert_eq!(client_frame_limiter_overrides, None);
    assert_eq!(preflight_token, None);
    assert_eq!(display_admission_token, None);
    assert_eq!(virtual_display_intent, None);
    assert!(!proton_dualsense_compatibility);
    assert!(!return_on_admission);

    let null_compatibility: Operation = serde_json::from_value(json!({
        "action":"launch_export",
        "operation_id":"compatibility-null",
        "app_id":"app-0000000000000001",
        "frame_limiter_settings":null,
        "client_frame_limiter_overrides":null,
    }))
    .unwrap();
    let Operation::LaunchExport {
        frame_limiter_settings,
        client_frame_limiter_overrides,
        ..
    } = null_compatibility
    else {
        panic!("expected launch export operation");
    };
    assert_eq!(frame_limiter_settings, None);
    assert_eq!(client_frame_limiter_overrides, None);

    for field in ["frame_limiter_settings", "client_frame_limiter_overrides"] {
        for invalid in [json!("settings"), json!(1), json!(false), json!([])] {
            let mut request = launch_export("compatibility-invalid-type");
            request[field] = invalid.clone();
            assert!(
                serde_json::from_value::<Operation>(request).is_err(),
                "accepted non-object value {invalid} for {field}"
            );
        }
    }

    let mut unknown_field = launch_export("compatibility-unknown-field");
    unknown_field["unrelated_top_level_field"] = json!(true);
    assert!(serde_json::from_value::<Operation>(unknown_field).is_err());
}

fn frame_limiter_snapshot() -> Value {
    json!({
        "version":1,
        "settings":{
            "frame_limiter_enable":true,
            "frame_limiter_fps_limit_millihz":120000,
            "frame_limiter_provider":"auto",
            "frame_limiter_disable_vsync":false,
            "frame_limiter_auto_virtual_framegen":"enabled",
            "rtss_frame_limit_type":"async",
            "rtss_allow_virtual_display_override":false,
            "rtss_install_path":"",
            "mangohud_limiter_method":"late",
            "mangohud_preset":"custom",
            "mangohud_always_show_graph":false
        },
        "requested_stream_fps_millihz":60000,
        "client_display_mode_override_millihz":null,
        "lossless_scaling_limit_millihz":null,
        "virtual_display_active":null,
        "physical_framegen_capture":null,
        "game_frame_generation_mode":null,
        "rtss_frame_limit_type_explicit_override":null
    })
}

#[test]
fn client_frame_limiter_overrides_require_a_frozen_preflight_row() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    let mut launch = launch_export("client-policy-without-preflight");
    launch["client_frame_limiter_overrides"] = json!({"frame_limiter_enable":false});

    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch)["code"],
        "preflight_required"
    );
    assert!(harness.commands.is_empty());
    assert!(harness.sessions.is_empty());
    assert!(harness.processes.is_empty());

    let mut snapshot_without_preflight = launch_export("snapshot-without-preflight");
    snapshot_without_preflight["frame_limiter_settings"] = frame_limiter_snapshot();
    assert_eq!(
        harness.request(
            &mut pipeline,
            "amaterasu.nvhttp",
            snapshot_without_preflight
        )["code"],
        "preflight_required"
    );
    assert!(harness.commands.is_empty());

    let preflight = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"preflight_export","operation_id":"client-policy-without-preflight","app_id":"app-0000000000000001"}),
    );
    assert_eq!(preflight["ok"], true, "{preflight}");
    let mut launch = launch_export("client-policy-without-preflight");
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();
    launch["frame_limiter_settings"]["settings"]["frame_limiter_enable"] = json!(false);
    launch["client_frame_limiter_overrides"] = json!({"frame_limiter_enable":false});
    let started = harness.request(&mut pipeline, "amaterasu.nvhttp", launch);
    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert_eq!(
        started["launch"]["client_frame_limiter_overrides"],
        json!({"frame_limiter_enable":false})
    );
}

#[test]
fn preflight_freezes_catalog_row_and_frame_limiter_snapshot_for_all_readbacks() {
    let mut harness = Harness::new();
    harness.catalog_config_overrides = Some(json!({"frame_limiter_enable":true}));
    let mut pipeline = pipeline();
    let operation_id = "frozen-frame-policy";
    let preflight_request = json!({"action":"preflight_export","operation_id":operation_id,"app_id":"app-0000000000000001"});
    let preflight = harness.request(&mut pipeline, "amaterasu.nvhttp", preflight_request.clone());
    assert_eq!(preflight["catalog_revision"], 1);
    assert_eq!(
        preflight["config_overrides"],
        json!({"frame_limiter_enable":true})
    );
    assert_eq!(preflight["app_id"], "app-0000000000000001");
    assert!(preflight["executable_path"].is_string());
    assert!(harness.sessions.is_empty());
    assert!(harness.processes.is_empty());
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", preflight_request),
        preflight
    );

    let mut request = launch_export(operation_id);
    request["preflight_token"] = preflight["preflight_token"].clone();
    request["frame_limiter_settings"] = frame_limiter_snapshot();
    request["client_frame_limiter_overrides"] = json!({});
    harness.catalog_config_overrides = Some(json!({"frame_limiter_enable":false}));
    let started = harness.request(&mut pipeline, "amaterasu.nvhttp", request.clone());
    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert_eq!(
        started["launch"]["config_overrides"],
        preflight["config_overrides"]
    );
    assert_eq!(
        started["launch"]["executable_path"],
        preflight["executable_path"]
    );
    assert_eq!(
        started["launch"]["frame_limiter_settings"],
        frame_limiter_snapshot()
    );
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", request),
        started
    );
    let mut changed_retry = launch_export(operation_id);
    changed_retry["preflight_token"] = preflight["preflight_token"].clone();
    changed_retry["frame_limiter_settings"] = frame_limiter_snapshot();
    changed_retry["frame_limiter_settings"]["requested_stream_fps_millihz"] = json!(120000);
    changed_retry["client_frame_limiter_overrides"] = json!({});
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", changed_retry)["code"],
        "operation_conflict"
    );
    let launch_id = started["launch"]["launch_id"].clone();
    for query in [
        json!({"action":"status","launch_id":launch_id}),
        json!({"action":"lookup","operation_id":operation_id}),
    ] {
        let readback = harness.request(&mut pipeline, "amaterasu.nvhttp", query);
        assert_eq!(
            readback["launch"]["frame_limiter_settings"],
            frame_limiter_snapshot()
        );
        assert_eq!(
            readback["launch"]["config_overrides"],
            preflight["config_overrides"]
        );
    }
    assert_eq!(
        harness
            .commands
            .iter()
            .filter(|(service, _, _)| service == crate::workflow::CATALOG)
            .count(),
        1
    );
}

#[test]
fn preflight_keeps_target_and_sparse_limiter_policy_together_after_catalog_edit() {
    let mut harness = Harness::new();
    let mut original_policy = json!({"frame_limiter_enable":false});
    if cfg!(windows) {
        original_policy["rtss_frame_limit_type"] = json!("back edge sync");
    }
    harness.catalog_config_overrides = Some(original_policy.clone());
    let mut pipeline = pipeline();
    let operation_id = "frozen-target-and-policy";
    let preflight = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"preflight_export","operation_id":operation_id,"app_id":"app-0000000000000001"}),
    );
    assert_eq!(preflight["config_overrides"], original_policy);
    assert!(preflight["executable_path"].is_string());

    let mut launch = launch_export(operation_id);
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();
    launch["frame_limiter_settings"]["settings"]["frame_limiter_enable"] = json!(false);
    let mut client_overrides = json!({"frame_limiter_enable":null});
    if cfg!(windows) {
        launch["frame_limiter_settings"]["settings"]["rtss_frame_limit_type"] =
            json!("back edge sync");
        launch["frame_limiter_settings"]["rtss_frame_limit_type_explicit_override"] = json!(true);
        client_overrides["rtss_frame_limit_type"] = Value::Null;
    }
    launch["client_frame_limiter_overrides"] = client_overrides.clone();

    harness.catalog_playnite_game_id = Some("later-game".into());
    harness.catalog_config_overrides = Some(json!({"frame_limiter_enable":true}));
    let started = harness.request(&mut pipeline, "amaterasu.nvhttp", launch.clone());
    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert_eq!(started["launch"]["config_overrides"], original_policy);
    assert_eq!(
        started["launch"]["executable_path"],
        preflight["executable_path"]
    );
    assert_eq!(
        started["launch"]["frame_limiter_settings"],
        launch["frame_limiter_settings"]
    );
    assert_eq!(
        started["launch"]["client_frame_limiter_overrides"],
        client_overrides
    );
    assert!(harness.provider_launches.is_empty());
    assert_eq!(harness.processes.len(), 1);
    assert_eq!(
        harness
            .commands
            .iter()
            .filter(|(service, _, _)| service == crate::workflow::CATALOG)
            .count(),
        1
    );
    let launch_id = started["launch"]["launch_id"].clone();
    for readback in [
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch),
        harness.request(
            &mut pipeline,
            "amaterasu.nvhttp",
            json!({"action":"status","launch_id":launch_id}),
        ),
        harness.request(
            &mut pipeline,
            "amaterasu.nvhttp",
            json!({"action":"lookup","operation_id":operation_id}),
        ),
    ] {
        assert_eq!(readback, started);
    }
}

#[test]
fn rtsp_verifies_only_the_active_preflight_frame_limiter_snapshot() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    let operation_id = "verified-frame-limiter";
    let app_id = "app-0000000000000001";
    let preflight = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"preflight_export","operation_id":operation_id,"app_id":app_id}),
    );
    assert_eq!(preflight["ok"], true, "{preflight}");
    let mut launch = launch_export(operation_id);
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();
    let mut verify = json!({
        "action":"verify_frame_limiter_snapshot",
        "application_operation_id":operation_id,
        "app_id":app_id,
        "catalog_revision":preflight["catalog_revision"],
        "frame_limiter_settings":launch["frame_limiter_settings"],
        "frame_limiter_executable_path":preflight["executable_path"],
    });
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.rtsp", verify.clone())["code"],
        "frame_limiter_snapshot_denied"
    );

    let started = harness.request(&mut pipeline, "amaterasu.nvhttp", launch);
    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert_eq!(
        harness.request(&mut pipeline, "other-service", verify.clone())["code"],
        "frame_limiter_snapshot_denied"
    );
    let verified = harness.request(&mut pipeline, "amaterasu.rtsp", verify.clone());
    assert_eq!(
        verified,
        json!({
            "ok":true,
            "verified":true,
            "application_operation_id":operation_id,
            "app_id":app_id,
            "catalog_revision":preflight["catalog_revision"],
            "frame_limiter_settings":started["launch"]["frame_limiter_settings"],
            "frame_limiter_executable_path":preflight["executable_path"],
        })
    );
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.rtsp", verify.clone()),
        verified
    );

    for (field, value) in [
        ("application_operation_id", json!("other-operation")),
        ("app_id", json!("app-0000000000000002")),
        ("catalog_revision", json!(2)),
        ("frame_limiter_executable_path", json!("other-executable")),
    ] {
        let mut changed = verify.clone();
        changed[field] = value;
        assert_eq!(
            harness.request(&mut pipeline, "amaterasu.rtsp", changed)["code"],
            "frame_limiter_snapshot_denied",
            "{field}"
        );
    }
    verify["frame_limiter_settings"]["settings"]["frame_limiter_enable"] = json!(false);
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.rtsp", verify.clone())["code"],
        "frame_limiter_snapshot_denied"
    );
    verify["frame_limiter_settings"] = started["launch"]["frame_limiter_settings"].clone();
    assert_eq!(
        harness
            .commands
            .iter()
            .filter(|(service, _, _)| service == crate::workflow::CATALOG)
            .count(),
        1
    );
    let stopped = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"stop","launch_id":started["launch"]["launch_id"]}),
    );
    assert_ne!(stopped["launch"]["state"], "active", "{stopped}");
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.rtsp", verify)["code"],
        "frame_limiter_snapshot_denied"
    );
}

#[test]
fn display_export_is_bound_to_frozen_catalog_policy_and_retained_on_retry() {
    let mut harness = Harness::new();
    let display = json!({
        "dd_mode_remapping":{"mixed":[{"requested_resolution":"1920x1080","final_resolution":"2560x1440"}]},
        "dd_resolution_option":"manual",
        "dd_manual_resolution":"2560x1440",
        "dd_refresh_rate_option":"prefer_highest",
        "dd_manual_refresh_rate":"59.94",
        "dd_hdr_option":"auto",
        "dd_configuration_option":"ensure_primary",
        "dd_hdr_request_override":"force_off"
    });
    harness.catalog_config_overrides = Some(display.clone());
    let mut pipeline = pipeline();
    let preflight = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({
            "action":"preflight_export","operation_id":"frozen-display-policy",
            "app_id":"app-0000000000000001"
        }),
    );
    assert_eq!(preflight["catalog_revision"], 1);
    assert_eq!(preflight["application_display_overrides"], display);
    assert_eq!(preflight["config_overrides"], display);

    let mut launch = launch_export("frozen-display-policy");
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();
    launch["application_display_overrides"] = display.clone();
    let mut wrong_token = launch.clone();
    wrong_token["preflight_token"] = json!("wrong-token");
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", wrong_token)["code"],
        "preflight_mismatch"
    );
    assert_eq!(
        harness.request(&mut pipeline, "other-service", launch.clone())["code"],
        "preflight_not_found"
    );
    harness.catalog_config_overrides = Some(json!({"dd_resolution_option":"disabled"}));
    let mut mismatch = launch.clone();
    mismatch["application_display_overrides"]["dd_resolution_option"] = json!("disabled");
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", mismatch)["code"],
        "application_display_policy_mismatch"
    );
    assert!(harness.processes.is_empty());
    let mut malformed = launch.clone();
    malformed["application_display_overrides"]["dd_hdr_option"] = json!("unknown");
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", malformed)["code"],
        "invalid_application_display_overrides"
    );
    assert!(harness.processes.is_empty());

    let started = harness.request(&mut pipeline, "amaterasu.nvhttp", launch.clone());
    assert_eq!(started["launch"]["state"], "active", "{started}");
    assert_eq!(started["launch"]["application_display_overrides"], display);
    assert_eq!(started["launch"]["config_overrides"], display);
    assert_eq!(started["launch"]["catalog_revision"], 1);
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch),
        started
    );
    let status = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"status","launch_id":started["launch"]["launch_id"]}),
    );
    assert_eq!(status["launch"]["application_display_overrides"], display);
    assert_eq!(status["launch"]["config_overrides"], display);
}

#[test]
fn signed_principals_attest_frozen_display_policy_and_reject_changed_or_revoked_use() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    let application = json!({"dd_resolution_option":"manual","dd_manual_resolution":"2560x1440"});
    harness.catalog_config_overrides = Some(application.clone());
    let preflight = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({
            "action":"preflight_export","operation_id":"admitted-application",
            "app_id":"app-0000000000000001"
        }),
    );
    let certificate_id = "cd".repeat(32);
    let request_sha256 = "ab".repeat(32);
    let prepare = json!({
        "action":"prepare_display_admission",
        "application_operation_id":"admitted-application",
        "global_revision":7,
        "global_overrides":{"dd_hdr_option":"disabled","dd_resolution_option":"auto"},
        "catalog_revision":preflight["catalog_revision"],
        "application_display_overrides":application,
        "certificate_id":certificate_id,
        "client_display_overrides":{"dd_resolution_option":"disabled"},
        "rtsp_operation_id":"admitted-rtsp",
        "request_sha256":request_sha256,
    });
    let admission = harness.request(&mut pipeline, "amaterasu.nvhttp", prepare.clone());
    assert_eq!(admission["ok"], true, "{admission}");
    assert_eq!(
        harness.request(&mut pipeline, "nvhttp", prepare.clone())["code"],
        "display_admission_denied"
    );
    let token = admission["display_admission_token"].as_str().unwrap();
    let verify = json!({
        "action":"verify_display_admission",
        "token":token,
        "application_operation_id":"admitted-application",
        "rtsp_operation_id":"admitted-rtsp",
        "request_sha256":request_sha256,
        "display_policy_snapshot":admission["display_policy_snapshot"],
        "certificate_id":certificate_id,
        "catalog_revision":preflight["catalog_revision"],
    });
    assert_eq!(
        harness.request(&mut pipeline, "rtsp", verify.clone())["code"],
        "display_admission_denied"
    );
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.rtsp", verify.clone())["verified"],
        true
    );
    let mut launch = launch_export("admitted-application");
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();
    launch["application_display_overrides"] = application.clone();
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch.clone())["code"],
        "display_admission_denied"
    );
    assert!(harness.sessions.is_empty());
    assert!(harness.processes.is_empty());
    let mut forged = launch.clone();
    forged["display_admission_token"] = json!("00".repeat(32));
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", forged)["code"],
        "display_admission_denied"
    );
    assert!(harness.sessions.is_empty());
    assert!(harness.processes.is_empty());
    launch["display_admission_token"] = json!(token);
    let started = harness.request(&mut pipeline, "amaterasu.nvhttp", launch);
    assert_eq!(started["launch"]["state"], "active", "{started}");
    harness.catalog_config_overrides = Some(json!({"dd_hdr_option":"auto"}));
    let mut unversioned_global = prepare.clone();
    unversioned_global["global_revision"] = Value::Null;
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", unversioned_global)["code"],
        "display_admission_denied"
    );
    assert_eq!(
        harness.request(&mut pipeline, "other-service", prepare.clone())["code"],
        "display_admission_denied"
    );
    let retried_admission = harness.request(&mut pipeline, "amaterasu.nvhttp", prepare.clone());
    assert_eq!(retried_admission, admission);
    assert_eq!(token.len(), 64);
    assert_eq!(
        admission["display_policy_snapshot"]["catalog_revision"],
        preflight["catalog_revision"]
    );
    assert_eq!(admission["display_policy_snapshot"]["global_revision"], 7);
    assert_eq!(
        admission["display_policy_snapshot"]["effective_overrides"],
        json!({
            "dd_hdr_option":"disabled", "dd_resolution_option":"disabled", "dd_manual_resolution":"2560x1440"
        })
    );
    for (field, value) in [
        ("request_sha256", json!("ef".repeat(32))),
        ("certificate_id", json!("12".repeat(32))),
        ("rtsp_operation_id", json!("other-rtsp")),
        ("catalog_revision", json!(2)),
    ] {
        let mut changed = prepare.clone();
        changed[field] = value;
        assert_eq!(
            harness.request(&mut pipeline, "amaterasu.nvhttp", changed)["code"],
            "display_admission_denied"
        );
    }
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", verify.clone())["code"],
        "display_admission_denied"
    );
    let verified = harness.request(&mut pipeline, "amaterasu.rtsp", verify.clone());
    assert_eq!(verified["verified"], true, "{verified}");
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.rtsp", verify.clone()),
        verified
    );
    for (field, value) in [
        ("token", json!("00".repeat(32))),
        ("request_sha256", json!("ef".repeat(32))),
        ("certificate_id", json!("12".repeat(32))),
        ("rtsp_operation_id", json!("other-rtsp")),
        ("application_operation_id", json!("other-app")),
        ("catalog_revision", json!(2)),
    ] {
        let mut changed = verify.clone();
        changed[field] = value;
        assert_eq!(
            harness.request(&mut pipeline, "amaterasu.rtsp", changed)["code"],
            "display_admission_denied"
        );
    }
    let mut changed_snapshot = verify.clone();
    changed_snapshot["display_policy_snapshot"]["effective_overrides"]["dd_hdr_option"] =
        json!("auto");
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.rtsp", changed_snapshot)["code"],
        "display_admission_denied"
    );
    let status = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"status","launch_id":started["launch"]["launch_id"]}),
    );
    assert!(!status.to_string().contains(token));
    let lookup = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"lookup","operation_id":"admitted-application"}),
    );
    assert!(!lookup.to_string().contains(token));
    let _stopped = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"stop","launch_id":started["launch"]["launch_id"]}),
    );
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.rtsp", verify)["code"],
        "display_admission_denied"
    );
}

#[test]
fn staged_display_admission_expires_or_stops_before_launch() {
    let mut harness = Harness::new();
    let mut pipeline = Pipeline::new(BTreeMap::new()).unwrap();
    let preflight = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"preflight_export","operation_id":"expiring-app",
            "app_id":"app-0000000000000001"}),
    );
    assert_eq!(preflight["ok"], true);
    let prepare = json!({
        "action":"prepare_display_admission",
        "application_operation_id":"expiring-app",
        "global_revision":null,
        "global_overrides":{},
        "catalog_revision":preflight["catalog_revision"],
        "application_display_overrides":{},
        "certificate_id":"cd".repeat(32),
        "client_display_overrides":{},
        "rtsp_operation_id":"expiring-rtsp",
        "request_sha256":"ab".repeat(32),
    });
    let admission = harness.request(&mut pipeline, "amaterasu.nvhttp", prepare.clone());
    assert_eq!(admission["ok"], true);
    let verify = json!({
        "action":"verify_display_admission",
        "token":admission["display_admission_token"],
        "application_operation_id":"expiring-app",
        "rtsp_operation_id":"expiring-rtsp",
        "request_sha256":"ab".repeat(32),
        "display_policy_snapshot":admission["display_policy_snapshot"],
        "certificate_id":"cd".repeat(32),
        "catalog_revision":preflight["catalog_revision"],
    });
    let verified = harness.request(&mut pipeline, "amaterasu.rtsp", verify.clone());
    assert_eq!(verified["verified"], true);
    assert!(
        verified["preflight_remaining_ms"]
            .as_u64()
            .is_some_and(|remaining| (1..=120_000).contains(&remaining)),
        "{verified}"
    );
    let record = pipeline
        .records
        .values
        .values_mut()
        .find(|record| record.operation_id == "expiring-app")
        .unwrap();
    record.preflight_deadline = Some(Instant::now() - Duration::from_secs(1));
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", prepare.clone())["code"],
        "display_admission_denied"
    );
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.rtsp", verify.clone())["code"],
        "display_admission_denied"
    );
    let mut launch = launch_export("expiring-app");
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();
    launch["display_admission_token"] = admission["display_admission_token"].clone();
    assert_ne!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch)["ok"],
        true
    );
    assert!(harness.sessions.is_empty());
    assert!(harness.processes.is_empty());

    let mut harness = Harness::new();
    let mut pipeline = Pipeline::new(BTreeMap::new()).unwrap();
    let preflight = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"preflight_export","operation_id":"stopped-app",
            "app_id":"app-0000000000000001"}),
    );
    let mut stopped_prepare = prepare;
    stopped_prepare["application_operation_id"] = json!("stopped-app");
    stopped_prepare["catalog_revision"] = preflight["catalog_revision"].clone();
    let staged = harness.request(&mut pipeline, "amaterasu.nvhttp", stopped_prepare.clone());
    assert_eq!(staged["ok"], true);
    let launch_id = pipeline
        .records
        .values
        .values()
        .find(|record| record.operation_id == "stopped-app")
        .unwrap()
        .launch_id;
    let stopped = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"stop","launch_id":launch_id}),
    );
    assert_eq!(stopped["code"], "preflight_unavailable");
    assert!(
        pipeline.records.values[&launch_id]
            .display_admission
            .is_none()
    );
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", stopped_prepare)["code"],
        "display_admission_denied"
    );
    assert!(harness.sessions.is_empty());
    assert!(harness.processes.is_empty());
}

#[test]
fn already_launched_display_admission_keeps_legacy_launch_retry() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    let preflight = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"preflight_export","operation_id":"legacy-display-app",
            "app_id":"app-0000000000000001"}),
    );
    let mut launch = launch_export("legacy-display-app");
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();
    let started = harness.request(&mut pipeline, "amaterasu.nvhttp", launch.clone());
    assert_eq!(started["launch"]["state"], "active");
    let prepare = json!({
        "action":"prepare_display_admission",
        "application_operation_id":"legacy-display-app",
        "global_revision":null,
        "global_overrides":{},
        "catalog_revision":preflight["catalog_revision"],
        "application_display_overrides":{},
        "certificate_id":"cd".repeat(32),
        "client_display_overrides":{},
        "rtsp_operation_id":"legacy-display-rtsp",
        "request_sha256":"ab".repeat(32),
    });
    let admission = harness.request(&mut pipeline, "amaterasu.nvhttp", prepare);
    assert_eq!(admission["ok"], true);
    let verified = harness.request(
        &mut pipeline,
        "amaterasu.rtsp",
        json!({
            "action":"verify_display_admission",
            "token":admission["display_admission_token"],
            "application_operation_id":"legacy-display-app",
            "rtsp_operation_id":"legacy-display-rtsp",
            "request_sha256":"ab".repeat(32),
            "display_policy_snapshot":admission["display_policy_snapshot"],
            "certificate_id":"cd".repeat(32),
            "catalog_revision":preflight["catalog_revision"],
        }),
    );
    assert_eq!(verified["verified"], true);
    assert!(verified.get("preflight_remaining_ms").is_none());
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch),
        started
    );
    assert_eq!(harness.processes.len(), 1);
}

#[test]
fn display_admission_is_unavailable_to_legacy_launches() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    let started = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        launch_export("legacy-admission"),
    );
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let prepare = json!({
        "action":"prepare_display_admission",
        "application_operation_id":"legacy-admission",
        "global_revision":null,
        "global_overrides":{},
        "catalog_revision":started["launch"]["catalog_revision"],
        "application_display_overrides":{},
        "certificate_id":"cd".repeat(32),
        "client_display_overrides":{},
        "rtsp_operation_id":"legacy-rtsp",
        "request_sha256":"ab".repeat(32),
    });
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", prepare)["code"],
        "display_admission_denied"
    );

    let mut failed_harness = Harness::new();
    failed_harness.catalog_config_overrides = Some(json!({"dd_hdr_option":"invalid"}));
    let mut failed_pipeline = Pipeline::new(BTreeMap::new()).unwrap();
    let failed = failed_harness.request(
        &mut failed_pipeline,
        "amaterasu.nvhttp",
        json!({
            "action":"preflight_export", "operation_id":"failed-admission",
            "app_id":"app-0000000000000001"
        }),
    );
    assert_eq!(failed["code"], "invalid_catalog_response");
    assert!(failed.get("display_admission_token").is_none());
    let mut failed_prepare = json!({
        "action":"prepare_display_admission",
        "application_operation_id":"failed-admission",
        "global_revision":null,
        "global_overrides":{},
        "catalog_revision":1,
        "application_display_overrides":{},
        "certificate_id":"cd".repeat(32),
        "client_display_overrides":{},
        "rtsp_operation_id":"failed-rtsp",
        "request_sha256":"ab".repeat(32),
    });
    assert_eq!(
        failed_harness.request(
            &mut failed_pipeline,
            "amaterasu.nvhttp",
            failed_prepare.clone()
        )["code"],
        "display_admission_denied"
    );
    failed_prepare["application_operation_id"] = json!("missing-admission");
    assert_eq!(
        failed_harness.request(&mut failed_pipeline, "amaterasu.nvhttp", failed_prepare)["code"],
        "display_admission_denied"
    );
}

#[test]
fn display_preflight_treats_absent_and_null_as_sparse_and_rejects_bad_catalog_values() {
    for (case, overrides) in [
        ("absent", None),
        (
            "null",
            Some(json!({"dd_resolution_option":null,"dd_hdr_option":null})),
        ),
    ] {
        let mut harness = Harness::new();
        harness.catalog_config_overrides = overrides;
        let preflight = harness.request(
            &mut pipeline(),
            "amaterasu.nvhttp",
            json!({
                "action":"preflight_export","operation_id":format!("display-{case}"),
                "app_id":"app-0000000000000001"
            }),
        );
        assert_eq!(preflight["ok"], true, "{preflight}");
        assert_eq!(preflight["application_display_overrides"], json!({}));
    }
    for invalid in [
        json!(17),
        json!("unknown"),
        json!({"mixed":[{"requested_fps":22}]}),
        json!({"mixed":[{}]}),
        json!({"mixed":[{"requested_fps":"59.94","final_resolution":"1920x1080"}]}),
    ] {
        let mut harness = Harness::new();
        harness.catalog_config_overrides = Some(json!({"dd_mode_remapping":invalid}));
        let rejected = harness.request(
            &mut pipeline(),
            "amaterasu.nvhttp",
            json!({
                "action":"preflight_export","operation_id":"bad-display",
                "app_id":"app-0000000000000001"
            }),
        );
        assert_eq!(rejected["code"], "invalid_catalog_response", "{rejected}");
        assert!(harness.processes.is_empty());
    }
}

#[test]
fn preflight_token_is_bound_to_runtime_identity_application_and_operation() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    let preflight = harness.request_as(
        &mut pipeline,
        "amaterasu.nvhttp",
        "paired-client",
        json!({"action":"preflight_export","operation_id":"bound-token","app_id":"app-0000000000000001"}),
    );
    let mut launch = launch_export("bound-token");
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();

    for (caller, initiator, expected) in [
        ("other-service", "paired-client", "preflight_not_found"),
        ("amaterasu.nvhttp", "other-client", "operation_conflict"),
    ] {
        let rejected = harness.request_as(&mut pipeline, caller, initiator, launch.clone());
        assert_eq!(rejected["code"], expected, "{rejected}");
    }
    let mut wrong_operation = launch.clone();
    wrong_operation["operation_id"] = json!("other-operation");
    assert_eq!(
        harness.request_as(
            &mut pipeline,
            "amaterasu.nvhttp",
            "paired-client",
            wrong_operation
        )["code"],
        "preflight_not_found"
    );
    let mut wrong_application = launch.clone();
    wrong_application["app_id"] = json!("app-0000000000000002");
    assert_eq!(
        harness.request_as(
            &mut pipeline,
            "amaterasu.nvhttp",
            "paired-client",
            wrong_application
        )["code"],
        "operation_conflict"
    );
    let mut wrong_token = launch.clone();
    wrong_token["preflight_token"] = json!("wrong-token");
    assert_eq!(
        harness.request_as(
            &mut pipeline,
            "amaterasu.nvhttp",
            "paired-client",
            wrong_token
        )["code"],
        "preflight_mismatch"
    );
    assert!(harness.sessions.is_empty());
    assert!(harness.processes.is_empty());
    assert_eq!(
        harness.request_as(&mut pipeline, "amaterasu.nvhttp", "paired-client", launch)["launch"]["state"],
        "active"
    );
}

#[test]
fn preflight_rejects_invalid_catalog_policy_and_export_rejects_snapshot_mismatch() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    harness.catalog_config_overrides = Some(json!({"rtss_frame_limit_type":17}));
    let rejected = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"preflight_export","operation_id":"invalid-catalog","app_id":"app-0000000000000001"}),
    );
    assert_eq!(rejected["code"], "invalid_catalog_response");
    assert!(harness.sessions.is_empty());

    harness.catalog_config_overrides = Some(json!({"frame_limiter_enable":false}));
    let preflight = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"preflight_export","operation_id":"validate-snapshot","app_id":"app-0000000000000001"}),
    );
    let mut launch = launch_export("validate-snapshot");
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch.clone())["code"],
        "frame_limiter_policy_mismatch"
    );
    launch["frame_limiter_settings"]["settings"]["frame_limiter_enable"] = json!(false);
    launch["frame_limiter_settings"]["requested_stream_fps_millihz"] = json!("60hz");
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch.clone())["code"],
        "invalid_frame_limiter_snapshot"
    );
    launch["frame_limiter_settings"]["requested_stream_fps_millihz"] = json!(60000);
    launch["client_frame_limiter_overrides"] = json!({"unrecognized":null});
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch.clone())["code"],
        "invalid_client_frame_limiter_overrides"
    );
    launch["client_frame_limiter_overrides"] = json!({"frame_limiter_fps_limit":"120"});
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch.clone())["code"],
        "invalid_client_frame_limiter_overrides"
    );
    launch["client_frame_limiter_overrides"] = json!({});
    launch["frame_limiter_settings"]["rtss_frame_limit_type_explicit_override"] = json!("async");
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch.clone())["code"],
        "invalid_frame_limiter_snapshot"
    );
    launch["frame_limiter_settings"]["rtss_frame_limit_type_explicit_override"] = Value::Null;
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch)["launch"]["state"],
        "active"
    );
}

#[test]
fn rtss_limit_type_stays_sparse_across_preflight_launch_and_status() {
    let mut cases = vec![("absent", None, None), ("null", Some(Value::Null), None)];
    if cfg!(windows) {
        for value in [
            "async",
            "front edge sync",
            "back edge sync",
            "nvidia reflex",
        ] {
            cases.push((value, Some(json!(value)), Some(value)));
        }
    }
    for (index, (case, configured, expected)) in cases.into_iter().enumerate() {
        let mut harness = Harness::new();
        harness.catalog_config_overrides =
            configured.map(|value| json!({"rtss_frame_limit_type":value}));
        let mut pipeline = pipeline();
        let operation_id = format!("rtss-preflight-{index}");
        let preflight = harness.request(
            &mut pipeline,
            "amaterasu.nvhttp",
            json!({"action":"preflight_export","operation_id":operation_id,"app_id":"app-0000000000000001"}),
        );
        assert_eq!(
            preflight["config_overrides"]
                .get("rtss_frame_limit_type")
                .and_then(Value::as_str),
            expected,
            "{case}: {preflight}"
        );
        let mut launch = launch_export(&operation_id);
        launch["preflight_token"] = preflight["preflight_token"].clone();
        launch["frame_limiter_settings"] = frame_limiter_snapshot();
        launch["frame_limiter_settings"]["settings"]["rtss_frame_limit_type"] =
            json!(expected.unwrap_or("async"));
        if expected.is_some() {
            launch["frame_limiter_settings"]["rtss_frame_limit_type_explicit_override"] =
                json!(true);
        }
        let started = harness.request(&mut pipeline, "amaterasu.nvhttp", launch);
        assert_eq!(started["launch"]["state"], "active", "{case}: {started}");
        assert_eq!(
            started["launch"]["config_overrides"]
                .get("rtss_frame_limit_type")
                .and_then(Value::as_str),
            expected
        );
        let status = harness.request(
            &mut pipeline,
            "amaterasu.nvhttp",
            json!({"action":"status","launch_id":started["launch"]["launch_id"]}),
        );
        assert_eq!(
            status["launch"]["config_overrides"]
                .get("rtss_frame_limit_type")
                .and_then(Value::as_str),
            expected
        );
    }
}

#[test]
fn unresolved_preflight_keeps_its_operation_until_catalog_issues_the_token() {
    use crate::records::{CatalogSelection, GlobalPreparation, Records};

    let mut records = Records::new().unwrap();
    let (preflight_id, _) = records
        .start(
            "amaterasu.nvhttp",
            "paired-client",
            "slow-catalog".into(),
            "app-0000000000000001".into(),
            CatalogSelection::Exported,
            GlobalPreparation::default(),
        )
        .unwrap();
    records
        .values
        .get_mut(&preflight_id)
        .unwrap()
        .preflight_token = Some("token".into());
    assert!(records.values[&preflight_id].preflight_deadline.is_none());

    records
        .start(
            "amaterasu.nvhttp",
            "paired-client",
            "another-operation".into(),
            "app-0000000000000001".into(),
            CatalogSelection::Exported,
            GlobalPreparation::default(),
        )
        .unwrap();
    assert!(records.values.contains_key(&preflight_id));
    assert_eq!(
        records
            .start(
                "amaterasu.nvhttp",
                "paired-client",
                "slow-catalog".into(),
                "app-0000000000000001".into(),
                CatalogSelection::Exported,
                GlobalPreparation::default(),
            )
            .unwrap(),
        (preflight_id, false)
    );
}

#[test]
fn expired_preflight_token_cannot_launch_or_resurrect_after_catalog_change() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    let preflight_request = json!({"action":"preflight_export","operation_id":"expired-token","app_id":"app-0000000000000001"});
    let preflight = harness.request(&mut pipeline, "amaterasu.nvhttp", preflight_request.clone());
    pipeline
        .records
        .values
        .values_mut()
        .next()
        .unwrap()
        .preflight_deadline = Some(Instant::now() - Duration::from_secs(1));
    let mut launch = launch_export("expired-token");
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch.clone())["code"],
        "preflight_not_found"
    );
    assert!(harness.sessions.is_empty());
    harness.catalog_config_overrides = Some(json!({"frame_limiter_enable":false}));
    let replacement = harness.request(&mut pipeline, "amaterasu.nvhttp", preflight_request);
    assert_ne!(replacement["preflight_token"], preflight["preflight_token"]);
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch)["code"],
        "preflight_mismatch"
    );
    assert!(harness.sessions.is_empty());
    assert!(harness.processes.is_empty());
}

#[test]
fn launch_export_can_acknowledge_admission_while_owned_startup_remains_stoppable() {
    let mut harness = Harness::new();
    harness.application_preparing_statuses = 8;
    let mut pipeline = pipeline();
    let operation_id = "detached-startup";
    let preflight = harness.request_as(
        &mut pipeline,
        "amaterasu.nvhttp",
        "management",
        json!({"action":"preflight_export","operation_id":operation_id,
            "app_id":"app-0000000000000001"}),
    );
    let mut launch = launch_export(operation_id);
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();
    launch["proton_dualsense_compatibility"] = json!(true);
    launch["return_on_admission"] = json!(true);
    let admitted = harness.request_as(&mut pipeline, "amaterasu.nvhttp", "management", launch);
    assert_eq!(admitted["launch"]["state"], "starting", "{admitted}");
    assert!(
        pipeline.job.is_some(),
        "startup remains owned after acknowledgement"
    );
    for failure in ["poll", "complete"] {
        let retained_pending = pipeline.job.as_ref().unwrap().pending;
        let mut incoming = Some(Response::Request {
            id: 42,
            caller: "amaterasu.nvhttp".into(),
            initiator: "management".into(),
            service: SERVICE.into(),
            payload: json!({"action":"status","launch_id":admitted["launch"]["launch_id"]}),
            remaining_ms: 5_000,
        });
        let readback_error = pipeline.tick(|command| match command {
            Command::Poll {} if failure == "poll" => {
                Err("injected detached readback poll failure".into())
            }
            Command::Poll {} => Ok(incoming.take().unwrap_or(Response::Pending)),
            Command::Complete { .. } if failure == "complete" => {
                Err("injected detached readback reply failure".into())
            }
            other => panic!("failed detached readback must not advance dependencies: {other:?}"),
        });
        assert!(readback_error.is_err());
        assert_eq!(pipeline.job.as_ref().unwrap().pending, retained_pending);
        assert_eq!(pipeline.job.as_ref().unwrap().request, None);
    }
    let application_launch = harness
        .commands
        .iter()
        .find(|(service, request, _)| {
            service == crate::workflow::APPLICATION && request["action"] == "launch"
        })
        .unwrap();
    assert_eq!(application_launch.1["proton_dualsense_compatibility"], true);

    let status = harness.request_as(
        &mut pipeline,
        "amaterasu.nvhttp",
        "management",
        json!({"action":"status","launch_id":admitted["launch"]["launch_id"]}),
    );
    assert_eq!(status["launch"]["state"], "starting");
    let stopped = harness.request_as(
        &mut pipeline,
        "amaterasu.nvhttp",
        "management",
        json!({"action":"stop","launch_id":admitted["launch"]["launch_id"]}),
    );
    assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
    assert!(harness.sessions[0].lease.is_none());
    assert!(harness.processes[0].child.is_none());
    assert!(pipeline.job.is_none());
}

#[test]
fn frame_limiter_snapshot_rejects_invalid_runtime_context_and_host_policy() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    let preflight = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"preflight_export","operation_id":"invalid-runtime-context","app_id":"app-0000000000000001"}),
    );
    let mut launch = launch_export("invalid-runtime-context");
    launch["preflight_token"] = preflight["preflight_token"].clone();
    launch["frame_limiter_settings"] = frame_limiter_snapshot();
    let mut malformed = launch.clone();
    malformed["frame_limiter_settings"]["game_frame_generation_mode"] = json!("game_provided");
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", malformed)["code"],
        "invalid_frame_limiter_snapshot"
    );
    let mut malformed = launch.clone();
    malformed["frame_limiter_settings"]["rtss_frame_limit_type_explicit_override"] = json!(true);
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", malformed)["code"],
        "frame_limiter_policy_mismatch"
    );
    for (field, value) in [
        ("rtss_install_path", json!("bad\npath")),
        (
            "frame_limiter_provider",
            json!(if cfg!(windows) { "mangohud" } else { "rtss" }),
        ),
    ] {
        let mut malformed = launch.clone();
        malformed["frame_limiter_settings"]["settings"][field] = value;
        assert_eq!(
            harness.request(&mut pipeline, "amaterasu.nvhttp", malformed)["code"],
            "invalid_frame_limiter_snapshot"
        );
    }
    if !cfg!(windows) {
        let mut malformed = launch.clone();
        malformed["frame_limiter_settings"]["settings"]["rtss_install_path"] = json!("/opt/rtss");
        assert_eq!(
            harness.request(&mut pipeline, "amaterasu.nvhttp", malformed)["code"],
            "invalid_frame_limiter_snapshot"
        );
    } else {
        for path in ["C:relative", "\\\\server"] {
            let mut malformed = launch.clone();
            malformed["frame_limiter_settings"]["settings"]["rtss_install_path"] = json!(path);
            assert_eq!(
                harness.request(&mut pipeline, "amaterasu.nvhttp", malformed)["code"],
                "invalid_frame_limiter_snapshot"
            );
        }
    }
    assert!(harness.sessions.is_empty());
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
    assert_eq!(application[0]["exit_timeout_seconds"], 10);
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
fn output_path_is_snapshotted_at_preflight_and_forwarded_unchanged_on_replay() {
    let mut harness = Harness::new();
    harness.catalog_output = Some(json!("$(OUTPUT_ROOT)/application.log"));
    let original_root = std::env::temp_dir().to_string_lossy().into_owned();
    let mut pipeline = Pipeline::new(BTreeMap::from([(
        "OUTPUT_ROOT".into(),
        original_root.clone(),
    )]))
    .unwrap();
    let preflight_request = json!({"action":"preflight_export","operation_id":"output-snapshot","app_id":"app-0000000000000001"});
    let preflight = harness.request(&mut pipeline, "amaterasu.nvhttp", preflight_request.clone());
    assert_eq!(preflight["ok"], true, "{preflight}");
    assert!(harness.sessions.is_empty() && harness.processes.is_empty());
    let expected = std::path::Path::new(&original_root).join("application.log");
    pipeline
        .environment
        .insert("OUTPUT_ROOT".into(), "changed-after-preflight".into());
    harness.catalog_output = Some(json!("different-output.log"));
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", preflight_request)["preflight_token"],
        preflight["preflight_token"]
    );
    let mut request = launch_export("output-snapshot");
    request["preflight_token"] = preflight["preflight_token"].clone();
    request["frame_limiter_settings"] = frame_limiter_snapshot();
    let launched = harness.request(&mut pipeline, "amaterasu.nvhttp", request.clone());
    assert_eq!(launched["launch"]["state"], "active", "{launched}");
    let application: Vec<_> = harness
        .commands
        .iter()
        .filter(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "launch"
        })
        .map(|(_, payload, _)| payload)
        .collect();
    assert_eq!(application.len(), 1);
    assert_eq!(application[0]["output"], json!(expected));
    assert_eq!(
        harness.request(&mut pipeline, "amaterasu.nvhttp", request)["launch"]["state"],
        "active"
    );
    assert_eq!(
        harness
            .commands
            .iter()
            .filter(
                |(service, payload, _)| service == crate::workflow::APPLICATION
                    && payload["action"] == "launch"
            )
            .count(),
        1
    );
}

#[test]
fn provider_output_creates_a_preparation_lease_even_without_commands() {
    let mut harness = playnite_catalog("a1b2c3d4-1234-5678-9abc-0123456789ab", 10);
    let path = std::env::temp_dir().join("provider-command-output.log");
    harness.catalog_output = Some(json!(path));
    let mut pipeline = pipeline();
    let launched = harness.request(&mut pipeline, "alice", launch("provider-output-only"));
    assert_eq!(launched["launch"]["state"], "active", "{launched}");
    let prepare = harness
        .commands
        .iter()
        .find(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "prepare"
        })
        .unwrap();
    assert_eq!(prepare.1["output"], json!(path));
    assert_eq!(prepare.1["preparation"], json!([]));
    let stopped = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":1}),
    );
    assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
    assert!(harness.commands.iter().any(|(service, payload, _)| service
        == crate::workflow::APPLICATION
        && payload["action"] == "preparation_stop"));
}

#[test]
fn invalid_catalog_output_is_rejected_before_session_or_command_effects() {
    for output in [
        json!(42),
        json!("bad\npath"),
        json!("$(UNCLOSED"),
        json!("x".repeat(1_025)),
    ] {
        let mut harness = Harness::new();
        harness.catalog_output = Some(output);
        let mut pipeline = pipeline();
        let response = harness.request(&mut pipeline, "alice", launch("invalid-output"));
        assert_ne!(response["launch"]["state"], "active", "{response}");
        assert!(harness.sessions.is_empty() && harness.processes.is_empty());
        assert!(harness.preparations.is_empty());
    }
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
    request["legacy_global_preparation"] =
        json!([{"do":"global raw", "undo":"global undo", "elevated":true}]);
    harness.catalog_raw_preparation =
        Some(json!([{"do":"app raw", "undo":"app undo", "elevated":false}]));
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
        4
    );
    assert_eq!(
        application_launch["preparation"][0]["run"]["args"],
        json!(["global"])
    );
    assert_eq!(
        application_launch["preparation"][1],
        json!({"do":"global raw", "undo":"global undo", "elevated":true})
    );
    assert_eq!(
        application_launch["preparation"][2]["run"]["args"],
        json!(["app"])
    );
    assert_eq!(
        application_launch["preparation"][3],
        json!({"do":"app raw", "undo":"app undo", "elevated":false})
    );
    assert_eq!(
        pipeline.records.values[&1].global_preparation[0].run.args,
        ["global"]
    );

    // A retry with a changed global setting cannot reuse the old launch ID.
    let mut changed_structured = request.clone();
    changed_structured["global_preparation"][0]["run"]["args"] = json!(["changed"]);
    assert_eq!(
        harness.request(&mut pipeline, "alice", changed_structured)["code"],
        "operation_conflict"
    );
    request["legacy_global_preparation"][0]["do"] = json!("changed raw");
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
    harness.catalog_raw_preparation = Some(json!([{"do":"app raw", "undo":"app undo"}]));
    let mut request = launch("global-preparation-excluded");
    request["global_preparation"] = json!([{
        "run":{"executable":std::env::current_exe().unwrap(),"args":["global"]},
    }]);
    request["legacy_global_preparation"] = json!([{"do":"global raw", "undo":"global undo"}]);
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
        2
    );
    assert_eq!(
        application_launch["preparation"][0]["run"]["args"],
        json!(["app"])
    );
    assert_eq!(
        application_launch["preparation"][1],
        json!({"do":"app raw", "undo":"app undo", "elevated":false})
    );
}

#[test]
fn playnite_application_preparation_accepts_raw_steps_and_keeps_global_before_app() {
    let mut harness = Harness::new();
    harness.catalog_playnite_game_id = Some("a1b2c3d4-1234-5678-9abc-0123456789ab".into());
    harness.catalog_preparation = Some(json!([{
        "run":{"executable":std::env::current_exe().unwrap(),"args":["app"]},
    }]));
    harness.catalog_raw_preparation = Some(json!([{"do":"app raw", "undo":"app undo"}]));
    let command = std::env::current_exe().unwrap();
    let mut request = launch("playnite-raw-preparation");
    request["global_preparation"] = json!([{
        "run":{"executable":command,"args":["global"]},
    }]);
    request["legacy_global_preparation"] = json!([{"do":"global raw", "undo":"global undo"}]);
    let mut pipeline = pipeline();

    let started = harness.request(&mut pipeline, "alice", request);
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let application_launch = harness
        .commands
        .iter()
        .find(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "prepare"
        })
        .map(|(_, payload, _)| payload)
        .expect("Playnite preparation is sent before provider start");
    assert_eq!(
        application_launch["preparation"].as_array().unwrap().len(),
        4
    );
    assert_eq!(
        application_launch["preparation"][0]["run"]["args"],
        json!(["global"])
    );
    assert_eq!(application_launch["preparation"][1]["do"], "global raw");
    assert_eq!(
        application_launch["preparation"][2]["run"]["args"],
        json!(["app"])
    );
    assert_eq!(application_launch["preparation"][3]["do"], "app raw");
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
    let mut frame_limiter_overrides = json!({
        "frame_limiter_enable":false,
        "frame_limiter_fps_limit":0.0,
        "frame_limiter_provider":"auto",
        "frame_limiter_disable_vsync":false,
        "frame_limiter_auto_virtual_framegen":"vrr",
        "ds4_back_as_touchpad_click":false,
        "controller":false,
        "gamepad":"auto",
        "keyboard":false,
        "mouse":true,
    });
    if cfg!(windows) {
        frame_limiter_overrides["rtss_frame_limit_type"] = json!("nvidia reflex");
    }
    harness.catalog_config_overrides = Some(frame_limiter_overrides.clone());
    harness.catalog_fec_percentage = Some(37);
    harness.catalog_max_bitrate = Some(0);
    harness.catalog_minimum_fps_target = Some(59.94);
    harness.catalog_video_max_batch_size_kb = Some(0);
    harness.catalog_stream_audio = Some(false);
    harness.catalog_audio_sink = Some(String::new());
    harness.catalog_audio_sink_capture_only = Some(false);
    harness.catalog_back_button_timeout = Some(-1);
    let mut pipeline = pipeline();

    let operation_id = "fec-snapshot";
    let started = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        launch_export(operation_id),
    );
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let original_launch_id = started["launch"]["launch_id"].clone();
    let original_config_overrides = started["launch"]["config_overrides"].clone();
    assert!(
        !original_config_overrides["ds4_back_as_touchpad_click"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(started["launch"]["config_overrides"]["fec_percentage"], 37);
    assert_eq!(started["launch"]["config_overrides"]["max_bitrate"], 0);
    assert_eq!(
        started["launch"]["config_overrides"]["minimum_fps_target"],
        59.94
    );
    assert_eq!(
        started["launch"]["config_overrides"]["video_max_batch_size_kb"],
        0
    );
    assert_eq!(started["launch"]["config_overrides"]["stream_audio"], false);
    assert_eq!(started["launch"]["config_overrides"]["audio_sink"], "");
    assert_eq!(
        started["launch"]["config_overrides"]["audio_sink_capture_only"],
        false
    );
    assert_eq!(
        started["launch"]["config_overrides"]["back_button_timeout"],
        -1
    );
    assert_eq!(started["launch"]["config_overrides"]["controller"], false);
    assert_eq!(started["launch"]["config_overrides"]["gamepad"], "auto");
    assert_eq!(started["launch"]["config_overrides"]["keyboard"], false);
    assert_eq!(started["launch"]["config_overrides"]["mouse"], true);
    for (key, value) in frame_limiter_overrides.as_object().unwrap() {
        assert_eq!(&started["launch"]["config_overrides"][key.as_str()], value);
    }

    // Later catalog edits do not replace the policy captured by this launch.
    harness.catalog_fec_percentage = Some(81);
    harness.catalog_max_bitrate = Some(-100);
    harness.catalog_minimum_fps_target = Some(120.0);
    harness.catalog_video_max_batch_size_kb = Some(64);
    harness.catalog_stream_audio = Some(true);
    harness.catalog_audio_sink = Some("later-endpoint".into());
    harness.catalog_audio_sink_capture_only = Some(true);
    harness.catalog_back_button_timeout = Some(300);
    harness.catalog_config_overrides = Some(json!({
        "frame_limiter_enable":true,
        "frame_limiter_fps_limit":120.5,
        "frame_limiter_provider":"auto",
        "frame_limiter_disable_vsync":true,
        "frame_limiter_auto_virtual_framegen":"disabled",
        "ds4_back_as_touchpad_click":true,
        "controller":true,
        "gamepad":"auto",
        "keyboard":true,
        "mouse":false,
    }));
    let replayed = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        launch_export(operation_id),
    );
    assert_eq!(replayed["launch"]["launch_id"], original_launch_id);
    assert_eq!(
        replayed["launch"]["config_overrides"],
        original_config_overrides
    );
    let status = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        json!({"action":"status","launch_id":started["launch"]["launch_id"]}),
    );
    assert_eq!(status["launch"]["config_overrides"]["fec_percentage"], 37);
    assert_eq!(status["launch"]["config_overrides"]["max_bitrate"], 0);
    assert_eq!(
        status["launch"]["config_overrides"]["minimum_fps_target"],
        59.94
    );
    assert_eq!(
        status["launch"]["config_overrides"]["video_max_batch_size_kb"],
        0
    );
    assert_eq!(status["launch"]["config_overrides"]["stream_audio"], false);
    assert_eq!(status["launch"]["config_overrides"]["audio_sink"], "");
    assert_eq!(
        status["launch"]["config_overrides"]["audio_sink_capture_only"],
        false
    );
    assert_eq!(
        status["launch"]["config_overrides"]["back_button_timeout"],
        -1
    );
    assert_eq!(status["launch"]["config_overrides"]["controller"], false);
    assert_eq!(status["launch"]["config_overrides"]["gamepad"], "auto");
    assert_eq!(status["launch"]["config_overrides"]["keyboard"], false);
    assert_eq!(status["launch"]["config_overrides"]["mouse"], true);
    for (key, value) in frame_limiter_overrides.as_object().unwrap() {
        assert_eq!(&status["launch"]["config_overrides"][key.as_str()], value);
    }
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
fn amd_av1_latency_mode_is_retained_across_launch_retry_and_status() {
    for (index, mode) in ["auto", "none", "power_saving", "realtime", "lowest"]
        .into_iter()
        .enumerate()
    {
        let mut harness = Harness::new();
        harness.catalog_amd_av1_latency_mode = Some(json!(mode));
        let mut pipeline = pipeline();
        let operation_id = format!("av1-latency-{index}");

        let started = harness.request(&mut pipeline, "alice", launch(&operation_id));
        assert_eq!(started["launch"]["state"], "active", "{started}");
        let launch_id = started["launch"]["launch_id"].as_u64().unwrap();
        assert_eq!(
            started["launch"]["config_overrides"]["amd_av1_latency_mode"],
            mode
        );

        let changed_catalog_mode = if mode == "lowest" { "auto" } else { "lowest" };
        harness.catalog_amd_av1_latency_mode = Some(json!(changed_catalog_mode));
        let retry = harness.request(&mut pipeline, "alice", launch(&operation_id));
        assert_eq!(
            retry["launch"]["config_overrides"]["amd_av1_latency_mode"],
            mode
        );

        let status = harness.request(
            &mut pipeline,
            "alice",
            json!({"action":"status","launch_id":launch_id}),
        );
        assert_eq!(
            status["launch"]["config_overrides"]["amd_av1_latency_mode"],
            mode
        );
    }
}

#[test]
fn nvenc_spatial_aq_is_retained_across_launch_retry_and_status() {
    let cases = [
        ("absent", None, Some(json!(true)), json!({})),
        ("null", Some(json!(null)), Some(json!(false)), json!({})),
        (
            "true",
            Some(json!(true)),
            Some(json!(false)),
            json!({"nvenc_spatial_aq":true}),
        ),
        (
            "false",
            Some(json!(false)),
            Some(json!(true)),
            json!({"nvenc_spatial_aq":false}),
        ),
    ];

    for (case, initial_config_overrides, changed_config_overrides, expected_snapshot) in cases {
        let mut harness = Harness::new();
        harness.catalog_nvenc_spatial_aq = initial_config_overrides;
        let mut pipeline = pipeline();
        let operation_id = format!("nvenc-spatial-aq-{case}");

        let started = harness.request(&mut pipeline, "alice", launch(&operation_id));
        assert_eq!(started["launch"]["state"], "active", "{case}: {started}");
        assert_eq!(
            started["launch"]
                .get("config_overrides")
                .cloned()
                .unwrap_or_else(|| json!({})),
            expected_snapshot,
            "{case}: {started}"
        );
        let launch_id = started["launch"]["launch_id"].as_u64().unwrap();

        // Catalog edits do not replace the settings captured by the launch.
        harness.catalog_nvenc_spatial_aq = changed_config_overrides;
        let retry = harness.request(&mut pipeline, "alice", launch(&operation_id));
        assert_eq!(
            retry["launch"]
                .get("config_overrides")
                .cloned()
                .unwrap_or_else(|| json!({})),
            expected_snapshot,
            "{case}: {retry}"
        );

        let status = harness.request(
            &mut pipeline,
            "alice",
            json!({"action":"status","launch_id":launch_id}),
        );
        assert_eq!(
            status["launch"]
                .get("config_overrides")
                .cloned()
                .unwrap_or_else(|| json!({})),
            expected_snapshot,
            "{case}: {status}"
        );
    }
}

#[test]
fn native_pen_touch_is_retained_across_launch_response_retry_and_status() {
    let cases = [
        (
            "absent",
            None,
            Some(json!({"native_pen_touch":true})),
            json!({}),
        ),
        (
            "null",
            Some(json!({"native_pen_touch":null})),
            Some(json!({"native_pen_touch":false})),
            json!({}),
        ),
        (
            "true",
            Some(json!({"native_pen_touch":true})),
            Some(json!({"native_pen_touch":false})),
            json!({"native_pen_touch":true}),
        ),
        (
            "false",
            Some(json!({"native_pen_touch":false})),
            Some(json!({"native_pen_touch":true})),
            json!({"native_pen_touch":false}),
        ),
    ];

    for (case, initial_config_overrides, changed_config_overrides, expected_snapshot) in cases {
        let mut harness = Harness::new();
        harness.catalog_config_overrides = initial_config_overrides;
        let mut pipeline = pipeline();
        let operation_id = format!("native-pen-touch-{case}");

        let started = harness.request(
            &mut pipeline,
            "amaterasu.nvhttp",
            launch_export(&operation_id),
        );
        assert_eq!(started["launch"]["state"], "active", "{case}: {started}");
        assert_eq!(
            started["launch"]
                .get("config_overrides")
                .cloned()
                .unwrap_or_else(|| json!({})),
            expected_snapshot,
            "{case}: {started}"
        );
        let launch_id = started["launch"]["launch_id"].as_u64().unwrap();

        // Catalog edits do not replace the configuration captured by this launch.
        harness.catalog_config_overrides = changed_config_overrides;
        let retry = harness.request(
            &mut pipeline,
            "amaterasu.nvhttp",
            launch_export(&operation_id),
        );
        assert_eq!(
            retry["launch"]
                .get("config_overrides")
                .cloned()
                .unwrap_or_else(|| json!({})),
            expected_snapshot,
            "{case}: {retry}"
        );

        let status = harness.request(
            &mut pipeline,
            "amaterasu.nvhttp",
            json!({"action":"status","launch_id":launch_id}),
        );
        assert_eq!(
            status["launch"]
                .get("config_overrides")
                .cloned()
                .unwrap_or_else(|| json!({})),
            expected_snapshot,
            "{case}: {status}"
        );
    }
}

#[test]
fn invalid_native_pen_touch_fails_before_session_creation() {
    for (index, invalid) in [json!("false"), json!(0), json!([]), json!({})]
        .into_iter()
        .enumerate()
    {
        let mut harness = Harness::new();
        harness.catalog_config_overrides = Some(json!({"native_pen_touch":invalid}));
        let mut pipeline = pipeline();

        let failed = harness.request(
            &mut pipeline,
            "amaterasu.nvhttp",
            launch_export(&format!("invalid-native-pen-touch-{index}")),
        );
        assert_eq!(failed["launch"]["state"], "failed", "{failed}");
        assert_eq!(
            failed["launch"]["failure"], "invalid_catalog_response",
            "{failed}"
        );
        assert!(harness.sessions.is_empty());
        assert!(!harness.commands.iter().any(|(service, payload, _)| {
            service == crate::workflow::SESSION && payload["action"] == "create"
        }));
    }
}

#[test]
fn invalid_nvenc_spatial_aq_fails_before_session_creation() {
    for (index, invalid) in [json!("false"), json!(0), json!([]), json!({})]
        .into_iter()
        .enumerate()
    {
        let mut harness = Harness::new();
        harness.catalog_nvenc_spatial_aq = Some(invalid);
        let mut pipeline = pipeline();

        let failed = harness.request(
            &mut pipeline,
            "alice",
            launch(&format!("invalid-nvenc-spatial-aq-{index}")),
        );
        assert_eq!(failed["launch"]["state"], "failed", "{failed}");
        assert_eq!(
            failed["launch"]["failure"], "invalid_catalog_response",
            "{failed}"
        );
        assert!(harness.sessions.is_empty());
        assert!(!harness.commands.iter().any(|(service, payload, _)| {
            service == crate::workflow::SESSION && payload["action"] == "create"
        }));
    }
}

#[test]
fn invalid_amd_av1_latency_mode_fails_before_session_creation() {
    for (index, invalid) in [
        json!("AUTO"),
        json!("Power_saving"),
        json!("unknown"),
        json!(true),
        json!(42),
    ]
    .into_iter()
    .enumerate()
    {
        let mut harness = Harness::new();
        harness.catalog_amd_av1_latency_mode = Some(invalid);
        let mut pipeline = pipeline();

        let failed = harness.request(
            &mut pipeline,
            "alice",
            launch(&format!("invalid-av1-latency-{index}")),
        );
        assert_eq!(failed["launch"]["state"], "failed", "{failed}");
        assert_eq!(
            failed["launch"]["failure"], "invalid_catalog_response",
            "{failed}"
        );
        assert!(harness.sessions.is_empty());
        assert!(!harness.commands.iter().any(|(service, payload, _)| {
            service == crate::workflow::SESSION && payload["action"] == "create"
        }));
    }
}

#[test]
fn persisted_process_lifecycle_policy_reaches_application_launch() {
    let mut harness = Harness::new();
    harness.catalog_wait_all = Some(false);
    harness.catalog_exit_timeout_seconds = Some(0);
    harness.catalog_detached = Some(json!(["/opt/tools/overlay --launch"]));
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
fn typed_detached_policy_reaches_application_and_invalid_policy_never_spawns() {
    let output_directory = std::env::temp_dir().to_string_lossy().to_string();
    let typed = json!({
        "command":"overlay --launch",
        "environment":{"OVERLAY_MODE":"quiet"},
        "cwd":output_directory,
        "stdout":std::env::temp_dir().join("overlay.out").to_string_lossy(),
        "stderr":std::env::temp_dir().join("overlay.err").to_string_lossy()
    });
    let mut harness = Harness::new();
    harness.catalog_detached = Some(json!(["legacy --launch", typed]));
    let mut typed_pipeline = pipeline();
    let started = harness.request(&mut typed_pipeline, "alice", launch("typed-detached"));
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let application_launch = harness
        .commands
        .iter()
        .find(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "launch"
        })
        .map(|(_, payload, _)| payload)
        .unwrap();
    assert_eq!(
        application_launch["detached"],
        json!(["legacy --launch", typed])
    );

    for invalid in [
        json!({"command":"overlay","cwd":"relative"}),
        json!({"command":"overlay","stdout":"relative.out"}),
        json!({"command":"overlay","environment":{"A-B":"bad"}}),
        json!({"command":"overlay","unknown":true}),
    ] {
        let mut harness = Harness::new();
        harness.catalog_detached = Some(json!([invalid]));
        let mut pipeline = pipeline();
        let failed = harness.request(&mut pipeline, "alice", launch("invalid-typed-detached"));
        assert!(
            ["invalid_catalog_response", "unsupported_launch_spec"]
                .contains(&failed["launch"]["failure"].as_str().unwrap()),
            "{failed}"
        );
        assert!(!harness.commands.iter().any(|(service, payload, _)| {
            service == crate::workflow::APPLICATION && payload["action"] == "launch"
        }));
    }
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
    let exported = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        launch_export("public-one"),
    );
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
            "amaterasu.nvhttp",
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
        harness.request(&mut pipeline, "amaterasu.nvhttp", launch("public-one"))["code"],
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
    let result = harness.request(
        &mut pipeline,
        "amaterasu.nvhttp",
        launch_export("cancelled-export"),
    );
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
        harness.request(
            &mut pipeline,
            "amaterasu.nvhttp",
            launch_export("export-exit")
        )["launch"]["state"],
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
        "amaterasu.nvhttp",
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

#[test]
fn keybindings_preflight_freezes_whole_lists_without_changing_export_authority() {
    for (index, selected) in [json!([]), json!([0, 255, 65, 66, 65, 67])]
        .into_iter()
        .enumerate()
    {
        let mut harness = Harness::new();
        harness.catalog_config_overrides = Some(json!({"keybindings":selected}));
        let mut pipeline = pipeline();
        let operation_id = format!("keybindings-admitted-{index}");
        let preflight_request = json!({
            "action":"preflight_export","operation_id":operation_id,"app_id":"app-0000000000000001",
        });
        let preflight =
            harness.request(&mut pipeline, "amaterasu.nvhttp", preflight_request.clone());
        assert_eq!(preflight["ok"], true, "{preflight}");
        assert_eq!(preflight["config_overrides"]["keybindings"], selected);
        assert!(harness.sessions.is_empty());
        assert!(harness.processes.is_empty());
        let mut launch = launch_export(&operation_id);
        launch["preflight_token"] = preflight["preflight_token"].clone();
        launch["frame_limiter_settings"] = frame_limiter_snapshot();
        let denied = harness.request(&mut pipeline, "other-service", launch.clone());
        assert_eq!(denied["code"], "preflight_not_found");
        assert!(harness.sessions.is_empty());
        assert!(harness.processes.is_empty());
        harness.catalog_config_overrides = Some(json!({"keybindings":[90,91]}));
        let started = harness.request(&mut pipeline, "amaterasu.nvhttp", launch);
        assert_eq!(started["launch"]["state"], "active", "{started}");
        assert_eq!(
            started["launch"]["config_overrides"]["keybindings"],
            selected
        );
    }
}

#[test]
fn malformed_keybindings_catalog_policy_is_rejected_before_preflight_side_effects() {
    for invalid in [
        json!([65]),
        json!([0, 256]),
        json!([-1, 0]),
        json!([65.0, 66]),
        json!([true, 66]),
        json!(["65", 66]),
        json!({}),
        json!(vec![0_u8; 514]),
    ] {
        let mut harness = Harness::new();
        harness.catalog_config_overrides = Some(json!({"keybindings":invalid}));
        let rejected = harness.request(&mut pipeline(), "amaterasu.nvhttp", json!({
            "action":"preflight_export","operation_id":"invalid-keybindings","app_id":"app-0000000000000001",
        }));
        assert_eq!(
            rejected["code"], "invalid_catalog_response",
            "{invalid}: {rejected}"
        );
        assert!(harness.sessions.is_empty());
        assert!(harness.processes.is_empty());
    }
}

#[test]
fn cleanup_budget_is_snapshotted_at_preflight_and_survives_terminal_target_release() {
    let mut pipeline = Pipeline::new(BTreeMap::new()).unwrap();
    let mut harness = Harness::new();
    harness.catalog_playnite_game_id = Some("12345678-1234-1234-1234-123456789abc".into());
    harness.catalog_playnite_exit_timeout_seconds = 300;
    harness.catalog_raw_preparation = Some(json!(
        (0..8)
            .map(|index| json!({"do":format!("prepare{index}"), "undo":format!("undo{index}")}))
            .collect::<Vec<_>>()
    ));
    let preflight = harness.request_as(&mut pipeline, "amaterasu.nvhttp", "management", json!({"action":"preflight_export","operation_id":"budget-snapshot","app_id":"app-0000000000000001"}));
    assert_eq!(preflight["cleanup_budget_ms"], 2_745_000);
    let id = pipeline.records.values.keys().next().copied().unwrap();
    let record = pipeline.records.values.get_mut(&id).unwrap();
    assert_eq!(record.cleanup_budget_ms, 2_745_000);
    let mut cleanup = Workflow::launch(id, 42, 5_000);
    cleanup.begin_cleanup(record, Outcome::Completed, None);
    let deadline = cleanup.deadline;
    assert!(deadline.duration_since(Instant::now()) > Duration::from_secs(2_744));
    // Catalog mutation and terminal release cannot change the frozen scalar.
    harness.catalog_playnite_exit_timeout_seconds = 1;
    harness.catalog_raw_preparation = None;
    record.target = None;
    record.state = State::Stopped;
    assert_eq!(record.response()["launch"]["cleanup_budget_ms"], 2_745_000);
    assert_eq!(record.preflight_response()["cleanup_budget_ms"], 2_745_000);
    assert_eq!(cleanup.deadline, deadline);
}

#[test]
fn owned_readback_during_cleanup_preserves_deadline_and_rejects_other_mutations() {
    let mut pipeline = Pipeline::new(BTreeMap::new()).unwrap();
    let (id, _) = pipeline
        .records
        .start(
            "alice",
            "alice",
            "cleanup-owned".into(),
            "app-0000000000000001".into(),
            CatalogSelection::Initiator,
            GlobalPreparation::default(),
        )
        .unwrap();
    let record = pipeline.records.values.get_mut(&id).unwrap();
    record.cleanup_budget_ms = 2_745_000;
    record.process_requested = true;
    record.process_id = Some(3);
    let mut workflow = Workflow::launch(id, 7, 5_000);
    workflow.begin_cleanup(record, Outcome::Completed, None);
    workflow.pending = Some(50);
    let deadline = workflow.deadline;
    pipeline.job = Some(workflow);
    for (caller, request, expected) in [
        (
            "alice",
            json!({"action":"status","launch_id":id}),
            "stopping",
        ),
        (
            "alice",
            json!({"action":"lookup","operation_id":"cleanup-owned"}),
            "stopping",
        ),
        ("alice", json!({"action":"stop","launch_id":id}), "stopping"),
        (
            "bob",
            json!({"action":"status","launch_id":id}),
            "launch_not_found",
        ),
        (
            "bob",
            json!({"action":"lookup","operation_id":"cleanup-owned"}),
            "launch_not_found",
        ),
        (
            "bob",
            json!({"action":"stop","launch_id":id}),
            "launch_not_found",
        ),
        (
            "alice",
            json!({"action":"launch","operation_id":"another","app_id":"app-0000000000000001"}),
            "workflow_busy",
        ),
    ] {
        let mut incoming = Some(Response::Request {
            id: 42,
            caller: caller.into(),
            initiator: caller.into(),
            service: SERVICE.into(),
            payload: request,
            remaining_ms: 5_000,
        });
        let mut readback = None;
        pipeline
            .tick(|command| {
                Ok(match command {
                    Command::Poll {} => incoming.take().unwrap_or(Response::Pending),
                    Command::TakeResult { request: 50 } => Response::Pending,
                    Command::Complete {
                        request: 42,
                        payload,
                    } => {
                        readback = Some(payload);
                        Response::Ok
                    }
                    other => panic!("readback must not issue native effects: {other:?}"),
                })
            })
            .unwrap();
        let response = readback.unwrap();
        if expected == "stopping" {
            assert_eq!(response["launch"]["state"], expected);
            assert_eq!(response["launch"]["cleanup_budget_ms"], 2_745_000);
        } else {
            assert_eq!(response["code"], expected);
        }
        assert_eq!(pipeline.job.as_ref().unwrap().deadline, deadline);
        assert_eq!(pipeline.job.as_ref().unwrap().pending, Some(50));
        assert_eq!(pipeline.records.values.len(), 1);
    }
}

#[test]
fn cleanup_readback_transport_failure_retains_pending_work_then_settles() {
    for fail_poll in [false, true] {
        let mut pipeline = Pipeline::new(BTreeMap::new()).unwrap();
        let (id, _) = pipeline
            .records
            .start(
                "alice",
                "alice",
                "readback-failure".into(),
                "app-0000000000000001".into(),
                CatalogSelection::Initiator,
                GlobalPreparation::default(),
            )
            .unwrap();
        let record = pipeline.records.values.get_mut(&id).unwrap();
        record.process_requested = true;
        record.process_id = Some(3);
        record.cleanup_budget_ms = 2_745_000;
        let process_operation = record.process_operation.clone();
        let mut workflow = Workflow::launch(id, 7, 5_000);
        workflow.begin_cleanup(record, Outcome::Completed, None);
        workflow.step = Step::AwaitTermination;
        workflow.pending = Some(50);
        let deadline = workflow.deadline;
        pipeline.job = Some(workflow);
        let failure = pipeline.tick(|command| match command {
            Command::Poll {} if fail_poll => Err("injected ingress failure".into()),
            Command::Poll {} => Ok(Response::Request {
                id: 42,
                caller: "alice".into(),
                initiator: "alice".into(),
                service: SERVICE.into(),
                payload: json!({"action":"status","launch_id":id}),
                remaining_ms: 5_000,
            }),
            Command::Complete { request: 42, .. } => Ok(Response::Pending),
            other => panic!("failed ingress must retain dependency: {other:?}"),
        });
        assert!(failure.is_err());
        let retained = pipeline.job.as_ref().unwrap();
        assert_eq!(retained.launch_id, id);
        assert_eq!(retained.deadline, deadline);
        assert_eq!(retained.step, Step::AwaitTermination);
        assert_eq!(retained.pending, Some(50));
        assert!(pipeline.records.values[&id].state == State::Stopping);
        for _ in 0..2 {
            pipeline.tick(|command| {
                let response = match command {
                Command::Poll {} => Response::Pending,
                Command::TakeResult { request:50 } => Response::Result { payload:json!({"ok":true,"process":{"process_id":3,"operation_id":process_operation,"state":"exited"}}) },
                Command::Complete { request:7, payload } => { assert_eq!(payload["launch"]["state"], "stopped"); Response::Ok }
                other => panic!("retained cleanup must settle its original work: {other:?}"),
                };
                Ok(response)
            }).unwrap();
        }
        assert!(pipeline.job.is_none());
        assert!(pipeline.records.values[&id].state == State::Stopped);
    }
}

#[test]
fn cleanup_readback_list_paginates_by_bytes_and_preserves_owner() {
    let mut pipeline = Pipeline::new(BTreeMap::new()).unwrap();
    let mut expected = Vec::new();
    for owner in ["alice", "bob"] {
        for index in 0..16 {
            let (id, _) = pipeline
                .records
                .start(
                    owner,
                    owner,
                    format!("owned-{index}"),
                    "app-0000000000000001".into(),
                    CatalogSelection::Initiator,
                    GlobalPreparation::default(),
                )
                .unwrap();
            let record = pipeline.records.values.get_mut(&id).unwrap();
            record.state = State::Stopping;
            record.frame_limiter_settings = Some(BTreeMap::from([(
                "rtss_install_path".into(),
                json!(format!("/{}", "p".repeat(1_000))),
            )]));
            record.config_overrides = Some(
                serde_json::from_value(
                    json!({"audio_sink":"a".repeat(256),"sw_preset":"p".repeat(256)}),
                )
                .unwrap(),
            );
            record.cleanup_budget_ms = 2_745_000;
            if owner == "alice" {
                expected.push(id);
            }
        }
    }
    let mut offset = 0;
    let mut seen = Vec::new();
    loop {
        let mut page = None;
        let mut incoming = Some(Response::Request {
            id: 42,
            caller: "alice".into(),
            initiator: "alice".into(),
            service: SERVICE.into(),
            payload: json!({"action":"list","offset":offset,"limit":16}),
            remaining_ms: 5_000,
        });
        pipeline
            .poll_cleanup_readback(expected[0], &mut |command| {
                Ok(match command {
                    Command::Poll {} => incoming.take().unwrap_or(Response::Pending),
                    Command::Complete {
                        request: 42,
                        payload,
                    } => {
                        page = Some(payload);
                        Response::Ok
                    }
                    other => panic!("list must remain read-only: {other:?}"),
                })
            })
            .unwrap();
        let page = page.unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= 16_384);
        let records = page["launches"].as_array().unwrap();
        assert!(!records.is_empty());
        seen.extend(
            records
                .iter()
                .map(|record| record["launch_id"].as_u64().unwrap()),
        );
        let Some(next) = page["next_offset"].as_u64() else {
            break;
        };
        assert!(next > offset as u64);
        offset = next as usize;
    }
    assert_eq!(seen, expected);
}
