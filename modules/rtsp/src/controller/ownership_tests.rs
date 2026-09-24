use super::*;
use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};
use std::{collections::BTreeSet, ffi::c_void};

const SESSION: u64 = 23;

struct CallerRequest {
    id: u64,
    caller: &'static str,
    payload: Value,
}

#[derive(Default)]
struct Endpoint {
    callers: VecDeque<CallerRequest>,
    active: BTreeSet<u64>,
    commands: Vec<Command>,
    completion_attempts: Vec<(u64, Value)>,
    downstream_actions: BTreeMap<u64, String>,
    next_downstream: u64,
    rejected_completions: BTreeSet<u64>,
    lose_prepare_result_once: bool,
    prepare_result_lost: bool,
    cleanup_fails: bool,
}

impl Endpoint {
    fn enqueue(&mut self, id: u64, caller: &'static str, payload: Value) {
        self.active.insert(id);
        self.callers.push_back(CallerRequest {
            id,
            caller,
            payload,
        });
    }

    fn enqueue_cancelled(&mut self, id: u64, caller: &'static str, payload: Value) {
        self.callers.push_back(CallerRequest {
            id,
            caller,
            payload,
        });
    }

    fn response(&mut self, command: &Command) -> Response {
        match command {
            Command::Poll {} => self
                .callers
                .pop_front()
                .map_or(Response::Pending, |request| Response::Request {
                    id: request.id,
                    caller: request.caller.into(),
                    initiator: request.caller.into(),
                    service: crate::SERVICE.into(),
                    payload: request.payload,
                    remaining_ms: 5_000,
                }),
            Command::CheckRequest { request } if self.active.contains(request) => Response::Ok,
            Command::CheckRequest { .. } => Response::error("request_ended", "cancelled"),
            Command::Complete { request, payload } => {
                self.completion_attempts.push((*request, payload.clone()));
                if self.rejected_completions.contains(request) {
                    Response::error("unavailable", "lost completion")
                } else {
                    self.active.remove(request);
                    Response::Ok
                }
            }
            Command::Request { payload, .. } => {
                self.next_downstream += 1;
                let request = 1_000 + self.next_downstream;
                self.downstream_actions.insert(
                    request,
                    payload["action"]
                        .as_str()
                        .expect("downstream action")
                        .into(),
                );
                Response::Accepted { id: request }
            }
            Command::TakeResult { request } => {
                let action = self
                    .downstream_actions
                    .get(request)
                    .map(String::as_str)
                    .expect("known downstream request");
                Response::Result {
                    payload: self.downstream_result(action),
                }
            }
            Command::Cancel { .. } | Command::ReleaseResult { .. } => Response::Ok,
            unexpected => panic!("unexpected command: {unexpected:?}"),
        }
    }

    fn downstream_result(&self, action: &str) -> Value {
        match action {
            "prepare" => json!({
                "ok": true,
                "session": SESSION,
                "state": "prepared",
                "audio_local": "127.0.0.1:51000",
                "video_local": "127.0.0.1:51002",
                "control_local": "127.0.0.1:51004"
            }),
            "lookup" => json!({"ok": true, "session": SESSION, "state": "prepared"}),
            "stop" if self.cleanup_fails => json!({
                "ok": true,
                "session": SESSION,
                "state": "cleanup_failed",
                "cleanup_error": "provider_cleanup_failed"
            }),
            "status" if self.cleanup_fails => json!({
                "ok": true,
                "session": SESSION,
                "state": "cleanup_failed",
                "cleanup_error": "provider_cleanup_failed"
            }),
            "stop" | "status" => json!({
                "ok": true,
                "session": SESSION,
                "state": "stopped",
                "cleanup_error": null
            }),
            action => panic!("unexpected downstream action: {action}"),
        }
    }

    fn should_lose_reply(&mut self, command: &Command) -> bool {
        let Command::TakeResult { request } = command else {
            return false;
        };
        if self.lose_prepare_result_once
            && !self.prepare_result_lost
            && self.downstream_actions.get(request).map(String::as_str) == Some("prepare")
        {
            self.prepare_result_lost = true;
            return true;
        }
        false
    }

    fn completed(&self, request: u64) -> &Value {
        &self
            .completion_attempts
            .iter()
            .find(|(completed, _)| *completed == request)
            .unwrap_or_else(|| panic!("request {request} was not completed"))
            .1
    }

    fn downstream_requests(&self) -> Vec<(&str, &str, Option<u64>)> {
        self.commands
            .iter()
            .filter_map(|command| match command {
                Command::Request {
                    payload,
                    operation_id,
                    parent,
                    ..
                } => Some((
                    payload["action"].as_str().expect("downstream action"),
                    operation_id.as_str(),
                    *parent,
                )),
                _ => None,
            })
            .collect()
    }
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> abi::Status {
    // SAFETY: every test keeps the endpoint alive and uses the client synchronously.
    let endpoint = unsafe { &mut *context.cast::<Endpoint>() };
    let Ok(bytes) = (unsafe { input.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    endpoint.commands.push(command.clone());
    if endpoint.should_lose_reply(&command) {
        return abi::STATUS_INTERNAL_ERROR;
    }
    let output = serde_json::to_vec(&endpoint.response(&command)).expect("test response");
    // SAFETY: the synchronous SDK reply sink copies these bytes before return.
    unsafe { reply.send(&output) };
    abi::STATUS_OK
}

fn runtime(endpoint: &mut Endpoint) -> RuntimeClient {
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *mut Endpoint).cast(),
        invoke,
    };
    // SAFETY: callers retain the endpoint and use the client synchronously.
    unsafe { RuntimeClient::from_endpoint(api) }
}

fn settings(enabled: bool, allowed_principals: &[&str]) -> Settings {
    Settings {
        enabled,
        allow_remote: false,
        bind: "127.0.0.1:0".parse().expect("literal"),
        allowed_principals: allowed_principals
            .iter()
            .map(|principal| (*principal).into())
            .collect(),
        ..Settings::default()
    }
}

fn open(operation_id: &str) -> Value {
    json!({
        "action": "open",
        "operation_id": operation_id,
        "peer_ip": "127.0.0.1",
        "video_port": 50000,
        "audio_port": 50002,
        "key_hex": "00112233445566778899aabbccddeeff",
        "key_id": 1,
        "connect_data": 2,
        "capture": {
            "capture_provider": "dxgi",
            "output": "display",
            "encoder": 1,
            "audio_endpoint": "default"
        }
    })
}

fn discovery_open(operation_id: &str) -> Value {
    let mut value = open(operation_id);
    value["video_port"] = json!(0);
    value["audio_port"] = json!(0);
    value["discover_peers"] = json!(true);
    value
}

fn tick(controller: &mut Controller, runtime: &RuntimeClient, now: u64) {
    controller.tick(runtime, now).expect("controller tick");
}

fn finish_prepare(controller: &mut Controller, runtime: &RuntimeClient) {
    for now in 0..3 {
        tick(controller, runtime, now);
    }
}

#[test]
fn disabled_unauthorized_and_cancelled_admissions_never_allocate() {
    let cases = [
        (settings(false, &["owner"]), true, "disabled"),
        (settings(true, &["someone-else"]), true, "permission_denied"),
        (settings(true, &["owner"]), false, "cancelled"),
    ];

    for (settings, active, expected) in cases {
        let mut endpoint = Endpoint::default();
        if active {
            endpoint.enqueue(1, "owner", open("admission"));
        } else {
            endpoint.enqueue_cancelled(1, "owner", open("admission"));
        }
        let runtime = runtime(&mut endpoint);
        let mut controller = Controller::new(settings).expect("controller");
        tick(&mut controller, &runtime, 0);

        assert!(controller.records.is_empty(), "{expected}");
        assert!(endpoint.downstream_requests().is_empty(), "{expected}");
        if active {
            assert_eq!(endpoint.completed(1)["code"], expected);
        } else {
            assert!(
                endpoint
                    .completion_attempts
                    .iter()
                    .all(|(request, _)| *request != 1)
            );
        }
    }
}

#[test]
fn unreachable_peer_address_family_never_allocates_a_ticket() {
    let mut endpoint = Endpoint::default();
    let mut payload = open("wrong-address-family");
    payload["peer_ip"] = json!("::1");
    endpoint.enqueue(1, "owner", payload);
    let runtime = runtime(&mut endpoint);
    let mut controller = Controller::new(settings(true, &["owner"])).unwrap();
    tick(&mut controller, &runtime, 0);
    assert_eq!(endpoint.completed(1)["code"], "peer_family_mismatch");
    assert!(controller.records.is_empty());
    assert!(endpoint.downstream_requests().is_empty());
}

#[test]
fn successful_prepare_and_exact_retry_share_one_owned_allocation() {
    let mut endpoint = Endpoint::default();
    endpoint.enqueue(1, "owner", open("same-operation"));
    endpoint.enqueue(2, "owner", open("same-operation"));
    let runtime = runtime(&mut endpoint);
    let mut controller = Controller::new(settings(true, &["owner"])).expect("controller");

    finish_prepare(&mut controller, &runtime);

    let first = endpoint.completed(1);
    let second = endpoint.completed(2);
    assert_eq!(first["ok"], true);
    assert_eq!(first["state"], "ready");
    assert_eq!(second, first);
    assert_eq!(controller.records.len(), 1);
    assert_eq!(endpoint.downstream_requests().len(), 1);
    assert_eq!(endpoint.downstream_requests()[0].0, "prepare");
    assert_eq!(endpoint.downstream_requests()[0].2, Some(1));
}

#[test]
fn ready_ticket_uses_configured_ping_timeout_after_preparation() {
    for timeout in [250, 10_000] {
        let mut endpoint = Endpoint::default();
        let mut request = open("timeout-operation");
        request["ping_timeout_ms"] = json!(timeout);
        endpoint.enqueue(1, "owner", request);
        let runtime = runtime(&mut endpoint);
        let mut controller = Controller::new(settings(true, &["owner"])).unwrap();

        finish_prepare(&mut controller, &runtime);

        let record = controller.records.values().next().expect("prepared ticket");
        assert!(record.state == State::Ready);
        assert!((timeout..=timeout + 2).contains(&record.expires));
    }
}

#[test]
fn discovery_tokens_are_fresh_per_ticket_stable_on_retry_and_never_public() {
    let mut endpoint = Endpoint::default();
    endpoint.enqueue(1, "owner", discovery_open("discovery-one"));
    endpoint.enqueue(2, "owner", discovery_open("discovery-one"));
    endpoint.enqueue(3, "owner", discovery_open("discovery-two"));
    let runtime = runtime(&mut endpoint);
    let mut controller = Controller::new(settings(true, &["owner"])).expect("controller");

    for now in 0..8 {
        tick(&mut controller, &runtime, now);
    }

    assert_eq!(controller.records.len(), 2);
    assert_eq!(endpoint.completed(1), endpoint.completed(2));
    let prepares: Vec<_> = endpoint
        .commands
        .iter()
        .filter_map(|command| match command {
            Command::Request { payload, .. } if payload["action"] == "prepare" => Some(payload),
            _ => None,
        })
        .collect();
    assert_eq!(prepares.len(), 2);

    let mut all_tokens = BTreeSet::new();
    for prepare in prepares {
        assert!(prepare.get("video_port").is_none());
        assert!(prepare.get("audio_port").is_none());
        for lane in ["audio_ping_payload", "video_ping_payload"] {
            let token = prepare["discovery"][lane].as_str().expect("token");
            assert_eq!(token.len(), 16);
            assert!(
                token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            );
            assert!(all_tokens.insert(token.to_owned()), "reused {lane} token");
        }
    }

    for request in 1..=3 {
        let public = endpoint.completed(request).to_string();
        assert!(all_tokens.iter().all(|token| !public.contains(token)));
        assert!(!public.contains("ping_payload"));
    }
}

#[test]
fn foreign_status_lookup_and_close_cannot_observe_or_stop_an_owned_record() {
    let mut endpoint = Endpoint::default();
    endpoint.enqueue(1, "owner", open("private-operation"));
    let runtime = runtime(&mut endpoint);
    let mut controller = Controller::new(settings(true, &["owner"])).expect("controller");
    finish_prepare(&mut controller, &runtime);

    let ticket = endpoint.completed(1)["ticket"].as_u64().unwrap();
    endpoint.enqueue(2, "intruder", json!({"action": "status", "ticket": ticket}));
    endpoint.enqueue(
        3,
        "intruder",
        json!({"action": "lookup", "operation_id": "private-operation"}),
    );
    endpoint.enqueue(4, "intruder", json!({"action": "close", "ticket": ticket}));
    endpoint.enqueue(5, "owner", json!({"action": "status", "ticket": ticket}));
    for now in 3..7 {
        tick(&mut controller, &runtime, now);
    }

    for request in 2..=4 {
        assert_eq!(endpoint.completed(request)["code"], "ticket_not_found");
    }
    assert_eq!(endpoint.completed(5)["ticket"], ticket);
    assert_eq!(endpoint.completed(5)["state"], "ready");
    assert_eq!(controller.records.len(), 1);
    assert_eq!(endpoint.downstream_requests().len(), 1);
}

fn lost_completion_result(cleanup_fails: bool) -> (Endpoint, Value, Vec<String>) {
    let mut endpoint = Endpoint {
        cleanup_fails,
        ..Endpoint::default()
    };
    endpoint.rejected_completions.insert(1);
    endpoint.enqueue(1, "owner", open("lost-completion"));
    let runtime = runtime(&mut endpoint);
    let mut controller = Controller::new(settings(true, &["owner"])).expect("controller");

    for now in 0..8 {
        tick(&mut controller, &runtime, now);
        if controller
            .records
            .values()
            .all(|record| matches!(record.state, State::Failed | State::CleanupFailed))
        {
            break;
        }
    }
    let record = controller.records.values().next().expect("retained record");
    let response = record.response();
    let actions = endpoint
        .downstream_requests()
        .iter()
        .map(|(action, _, _)| (*action).to_owned())
        .collect();
    drop(controller);
    (endpoint, response, actions)
}

#[test]
fn lost_prepare_completion_stops_only_its_session_and_retains_cleanup_outcome() {
    let (endpoint, terminal, actions) = lost_completion_result(false);
    assert_eq!(actions, ["prepare", "stop"]);
    assert_eq!(terminal["state"], "failed");
    assert_eq!(terminal["failure"], "completion_unavailable");
    assert!(terminal["cleanup_error"].is_null());
    assert_eq!(
        endpoint.downstream_requests()[1].2,
        None,
        "cleanup must not inherit authority from a lost caller"
    );

    let (_, cleanup_failed, actions) = lost_completion_result(true);
    assert_eq!(actions, ["prepare", "stop", "status"]);
    assert_eq!(cleanup_failed["state"], "cleanup_failed");
    assert_eq!(cleanup_failed["failure"], "completion_unavailable");
    assert_eq!(cleanup_failed["cleanup_error"], "stream_cleanup_failed");
}

#[test]
fn post_commit_lost_prepare_result_settles_before_lookup_and_owned_stop() {
    let mut endpoint = Endpoint {
        lose_prepare_result_once: true,
        ..Endpoint::default()
    };
    endpoint.enqueue(1, "owner", open("lost-result"));
    let runtime = runtime(&mut endpoint);
    let mut controller = Controller::new(settings(true, &["owner"])).expect("controller");

    for now in 0..9 {
        tick(&mut controller, &runtime, now);
    }

    let requests = endpoint.downstream_requests();
    assert_eq!(
        requests
            .iter()
            .map(|(action, _, _)| *action)
            .collect::<Vec<_>>(),
        ["prepare", "lookup", "stop"]
    );
    assert_eq!(requests[0].2, Some(1));
    assert_eq!(requests[1].2, None);
    assert_eq!(requests[2].2, None);

    let cancelled = endpoint
        .commands
        .iter()
        .position(|command| matches!(command, Command::Cancel { request: 1001 }))
        .expect("prepare cancellation");
    let released = endpoint
        .commands
        .iter()
        .rposition(|command| matches!(command, Command::ReleaseResult { request: 1001 }))
        .expect("prepare release");
    let lookup = endpoint
        .commands
        .iter()
        .position(|command| {
            matches!(command, Command::Request { payload, .. } if payload["action"] == "lookup")
        })
        .expect("lookup request");
    assert!(cancelled < lookup && released < lookup);

    let record = controller.records.values().next().expect("retained record");
    assert_eq!(record.response()["state"], "failed");
    assert_eq!(record.response()["failure"], "dependency_unavailable");
    assert!(record.response()["cleanup_error"].is_null());
}
