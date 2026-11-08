use super::*;
use amaterasu_gamestream::{
    rtsp::Method,
    sdp::{Announce, parse_announce},
};
use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};
use std::{
    ffi::c_void,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, TcpStream, UdpSocket},
    sync::{
        Mutex,
        atomic::{AtomicU16, Ordering},
    },
};

pub(super) static NETWORK_TEST_LOCK: Mutex<()> = Mutex::new(());
static NEXT_TEST_BASE_PORT: AtomicU16 = AtomicU16::new(0);
const TEST_BASE_PORT: u16 = 47_989;

#[derive(Default)]
struct Endpoint {
    commands: Vec<Command>,
    completions: Vec<(u64, Value)>,
    active_request: bool,
    verifier_reply: Option<Value>,
    verifier_pending: bool,
    deny_launch: bool,
    deny_settlement: bool,
    deny_settlement_requests: BTreeSet<u64>,
    lifecycle_results: VecDeque<Option<Value>>,
    lifecycle_requests: BTreeMap<u64, Option<Value>>,
    recovery_results: VecDeque<Option<Value>>,
    recovery_requests: BTreeMap<u64, Option<Value>>,
    next_recovery_request: u64,
}

impl Endpoint {
    fn dispatch(&mut self, command: &Command) -> Response {
        match command {
            Command::Poll {} => Response::Pending,
            Command::CheckRequest { .. } if self.active_request => Response::Ok,
            Command::CheckRequest { .. } => Response::error("request_ended", "cancelled"),
            Command::Request { service, .. }
                if self.deny_launch && service == "amaterasu.launch/1" =>
            {
                Response::error("permission_denied", "no Launch grant")
            }
            Command::Request { service, .. }
                if service == "amaterasu.session-lifecycle/1"
                    && !self.lifecycle_results.is_empty() =>
            {
                self.next_recovery_request += 1;
                let id = 900 + self.next_recovery_request;
                self.lifecycle_requests
                    .insert(id, self.lifecycle_results.pop_front().expect("nonempty"));
                Response::Accepted { id }
            }
            Command::Request { service, .. }
                if service == "amaterasu.stream-session/1" && !self.recovery_results.is_empty() =>
            {
                self.next_recovery_request += 1;
                let id = 700 + self.next_recovery_request;
                self.recovery_requests
                    .insert(id, self.recovery_results.pop_front().expect("nonempty"));
                Response::Accepted { id }
            }
            Command::Request { .. } => Response::Accepted { id: 71 },
            Command::TakeResult { request }
                if let Some(Some(payload)) = self.lifecycle_requests.get(request) =>
            {
                Response::Result {
                    payload: payload.clone(),
                }
            }
            Command::TakeResult { request } if self.lifecycle_requests.contains_key(request) => {
                Response::Pending
            }
            Command::TakeResult { request }
                if let Some(Some(payload)) = self.recovery_requests.get(request) =>
            {
                Response::Result {
                    payload: payload.clone(),
                }
            }
            Command::TakeResult { request } if self.recovery_requests.contains_key(request) => {
                Response::Pending
            }
            Command::TakeResult { .. } if self.verifier_pending => Response::Pending,
            Command::TakeResult { .. } if self.verifier_reply.is_some() => Response::Result {
                payload: self.verifier_reply.clone().unwrap(),
            },
            Command::Complete { request, payload } => {
                self.completions.push((*request, payload.clone()));
                Response::Ok
            }
            Command::Cancel { request } | Command::ReleaseResult { request }
                if self.deny_settlement || self.deny_settlement_requests.contains(request) =>
            {
                Response::error("unavailable", "deliberate settlement failure")
            }
            _ => Response::Ok,
        }
    }
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> abi::Status {
    // SAFETY: the test keeps Endpoint exclusively alive for the client lifetime.
    let endpoint = unsafe { &mut *context.cast::<Endpoint>() };
    let Ok(bytes) = (unsafe { input.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    endpoint.commands.push(command.clone());
    let output = serde_json::to_vec(&endpoint.dispatch(&command)).expect("test response");
    // SAFETY: the synchronous SDK reply sink copies these bytes before return.
    unsafe { reply.send(&output) };
    abi::STATUS_OK
}

fn client(endpoint: &mut Endpoint) -> RuntimeClient {
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *mut Endpoint).cast(),
        invoke,
    };
    // SAFETY: caller retains endpoint for the synchronous test lifetime.
    unsafe { RuntimeClient::from_endpoint(api) }
}

fn open() -> Open {
    serde_json::from_value(json!({
        "operation_id":"caller-operation",
        "peer_ip":"127.0.0.1",
        "video_port":50000,
        "audio_port":50002,
        "key_hex":"00112233445566778899aabbccddeeff",
        "key_id":1,
        "connect_data":2,
        "capture":{
            "capture_provider":"dxgi",
            "output":"display",
            "encoder":1,
            "audio_endpoint":"default"
        }
    }))
    .expect("valid open")
}

fn startup_recovery_pending(controller: &mut Controller, deadline: u64) {
    controller.startup_recovery = StartupRecovery::new(0x1234, deadline);
}

fn listed_stream_sessions(sessions: Value, total: usize) -> Value {
    json!({"ok":true,"sessions":sessions,"total":total,"truncated":false,"startup_reconciled":true})
}

fn admit_pending_display_verifier(
    controller: &mut Controller,
    runtime: &RuntimeClient,
    deadline: u64,
) {
    let config = attested_open();
    let request_sha256 = config.display_request_sha256().unwrap();
    let mut admission = DisplayAdmission {
        request: 19,
        caller: "amaterasu.nvhttp".into(),
        config,
        deadline,
        request_sha256,
        verified: false,
        verification_started_at: 0,
        display_preflight_deadline: None,
        call: Call::for_service(
            "amaterasu.launch/1",
            json!({"action":"verify_display_admission"}),
            "rtsp-display-test".into(),
            Some(19),
        ),
    };
    assert!(matches!(admission.call.poll(runtime), Outcome::Waiting));
    controller.display_admission = Some(admission);
}

#[test]
fn retirement_observations_distinguish_runtime_fences_from_missing_tickets() {
    let settings = Settings {
        allowed_principals: vec!["caller".into()],
        retirement_status_principals: vec!["observer".into()],
        retirement_status_owners: BTreeMap::from([("observer".into(), "caller".into())]),
        ..Settings::default()
    };
    let mut controller = Controller::new(settings).unwrap();
    let epoch = controller.runtime_epoch.clone();
    let old_epoch = "0".repeat(32);
    assert_ne!(epoch, old_epoch);
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    controller.retirement_status(1, "observer", Some("persisted-op"), None, &runtime);
    controller.retirement_status(2, "observer", Some("persisted-op"), Some(&epoch), &runtime);
    controller.retirement_status(
        3,
        "observer",
        Some("persisted-op"),
        Some(&old_epoch),
        &runtime,
    );
    assert_eq!(endpoint.completions[0].1["state"], "unknown");
    assert_eq!(endpoint.completions[0].1["retired"], false);
    assert!(endpoint.completions[0].1["expected_runtime_epoch"].is_null());
    assert_eq!(endpoint.completions[1].1["retired"], false);
    assert_eq!(endpoint.completions[1].1["expected_runtime_epoch"], epoch);
    assert_eq!(endpoint.completions[2].1["state"], "epoch_fenced");
    assert_eq!(endpoint.completions[2].1["retired"], true);
    assert_eq!(
        endpoint.completions[2].1["expected_runtime_epoch"],
        old_epoch
    );
    assert_eq!(endpoint.completions[2].1["runtime_epoch"], epoch);
    let mut current = record(State::Ready);
    current.config.operation_id = "persisted-op".into();
    controller.records.insert(current.id, current);
    controller.retirement_status(
        4,
        "observer",
        Some("persisted-op"),
        Some(&old_epoch),
        &runtime,
    );
    assert_eq!(endpoint.completions[3].1["state"], "active");
    assert_eq!(endpoint.completions[3].1["retired"], false);
    let current = controller.records.get_mut(&5).unwrap();
    current.state = State::Closed;
    controller.retirement_status(5, "observer", Some("persisted-op"), Some(&epoch), &runtime);
    assert_eq!(endpoint.completions[4].1["retired"], false);
    controller.records.get_mut(&5).unwrap().cleanup_confirmed = true;
    controller.retirement_status(6, "observer", Some("persisted-op"), Some(&epoch), &runtime);
    assert_eq!(endpoint.completions[5].1["state"], "retired");
    assert_eq!(endpoint.completions[5].1["retired"], true);
    controller.retirement_status(7, "foreign", Some("persisted-op"), Some(&epoch), &runtime);
    assert_eq!(endpoint.completions[6].1["code"], "permission_denied");
    controller.reconcile_startup(8, "observer", 0, &runtime);
    assert_eq!(endpoint.completions[7].1["code"], "permission_denied");
}

#[test]
fn retirement_observers_are_pinned_to_one_owner_and_epoch_reads_ignore_current_occupancy() {
    let mut controller = Controller::new(Settings {
        allowed_principals: vec!["caller".into(), "foreign-owner".into()],
        retirement_status_owners: BTreeMap::from([
            ("observer".into(), "caller".into()),
            ("foreign-observer".into(), "foreign-owner".into()),
        ]),
        ..Settings::default()
    })
    .unwrap();
    let epoch = controller.runtime_epoch.clone();
    let mut owned = record(State::Closed);
    owned.config.operation_id = "colliding-operation".into();
    owned.cleanup_confirmed = true;
    let mut foreign = record(State::Playing);
    foreign.id = 6;
    foreign.owner = "foreign-owner".into();
    foreign.config.operation_id = "colliding-operation".into();
    controller.records.insert(owned.id, owned);
    controller.records.insert(foreign.id, foreign);
    controller.display_cleanup.push(DisplayCleanup {
        owner: "foreign-owner".into(),
        operation_id: "foreign-pending".into(),
        call: Call::new(json!({"action":"verify"}), "foreign-cleanup".into(), None),
    });
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    controller.retirement_status(
        1,
        "observer",
        Some("colliding-operation"),
        Some(&epoch),
        &runtime,
    );
    assert_eq!(endpoint.completions[0].1["retired"], true);
    assert_eq!(endpoint.completions[0].1["active_operations"], 0);
    assert_eq!(endpoint.completions[0].1["cleanup_pending"], false);
    controller.retirement_status(
        2,
        "foreign-observer",
        Some("colliding-operation"),
        Some(&epoch),
        &runtime,
    );
    assert_eq!(endpoint.completions[1].1["retired"], false);
    assert_eq!(endpoint.completions[1].1["active_operations"], 1);
    assert_eq!(endpoint.completions[1].1["cleanup_pending"], true);
    controller.retirement_status(3, "caller", None, None, &runtime);
    assert_eq!(endpoint.completions[2].1["active_operations"], 0);
    assert_eq!(endpoint.completions[2].1["cleanup_pending"], false);
    controller
        .retired_operations
        .insert(("foreign-owner".into(), "foreign-fence".into()));
    controller.retirement_status(4, "observer", Some("foreign-fence"), Some(&epoch), &runtime);
    assert_eq!(endpoint.completions[3].1["state"], "unknown");
    assert_eq!(endpoint.completions[3].1["retired"], false);
    controller.display_cleanup.push(DisplayCleanup {
        owner: "caller".into(),
        operation_id: "colliding-operation".into(),
        call: Call::new(json!({"action":"verify"}), "owned-cleanup".into(), None),
    });
    controller.retirement_status(5, "observer", None, None, &runtime);
    assert_eq!(endpoint.completions[4].1["startup_reconciled"], true);
    assert_eq!(endpoint.completions[4].1["state"], "ready");
    assert_eq!(endpoint.completions[4].1["cleanup_pending"], true);
    controller.retirement_status(
        6,
        "observer",
        Some("colliding-operation"),
        Some(&epoch),
        &runtime,
    );
    assert_eq!(endpoint.completions[5].1["retired"], false);
    assert_eq!(endpoint.completions[5].1["state"], "active");
    assert!(
        serde_json::from_value::<Operation>(
            json!({"action":"retirement_status", "owner":"foreign-owner"})
        )
        .is_err()
    );
}

#[test]
fn retirement_fences_wait_for_the_native_journal_reconciliation_proof() {
    let mut controller = Controller::new(Settings {
        allowed_principals: vec!["caller".into()],
        ..Settings::default()
    })
    .unwrap();
    startup_recovery_pending(&mut controller, 45_000);
    let mut pending = listed_stream_sessions(json!([]), 0);
    pending["startup_reconciled"] = json!(false);
    let mut endpoint = Endpoint {
        recovery_results: VecDeque::from([
            Some(pending),
            Some(listed_stream_sessions(json!([]), 0)),
        ]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    controller.advance_startup_recovery(&runtime, 0);
    controller.advance_startup_recovery(&runtime, 1);
    assert!(!controller.startup_recovery.ready());
    controller.retirement_status(
        1,
        "caller",
        Some("old-operation"),
        Some(&"0".repeat(32)),
        &runtime,
    );
    assert_eq!(endpoint.completions[0].1["retired"], false);
    assert_eq!(endpoint.completions[0].1["startup_reconciled"], false);
    controller.advance_startup_recovery(&runtime, 251);
    controller.advance_startup_recovery(&runtime, 252);
    assert!(controller.startup_recovery.ready());
    controller.retirement_status(
        2,
        "caller",
        Some("old-operation"),
        Some(&"0".repeat(32)),
        &runtime,
    );
    assert_eq!(endpoint.completions[1].1["retired"], true);
    let missing = json!({"ok":false,"code":"session_not_found"});
    assert!(recovery_stop_response(&missing, 23).is_err());
    assert!(recovery_status_response(&missing, 23).is_err());
}

#[test]
fn startup_reconciliation_retries_exact_failed_journal_owners_before_global_proof() {
    let mut controller = Controller::new(Settings {
        allowed_principals: vec!["amaterasu.nvhttp".into()],
        ..Settings::default()
    })
    .unwrap();
    startup_recovery_pending(&mut controller, 45_000);
    let mut blocked = listed_stream_sessions(
        json!([
            {"session":23,"state":"cleanup_failed","cleanup_error":"component_cleanup_failed"},
            {"session":24,"state":"stopping","cleanup_error":null}
        ]),
        2,
    );
    blocked["startup_reconciled"] = json!(false);
    let mut still_recovering = listed_stream_sessions(
        json!([
            {"session":23,"state":"stopped","cleanup_error":null},
            {"session":24,"state":"stopping","cleanup_error":null}
        ]),
        2,
    );
    still_recovering["startup_reconciled"] = json!(false);
    let mut endpoint = Endpoint {
        recovery_results: VecDeque::from([
            Some(blocked),
            Some(json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null})),
            Some(still_recovering),
        ]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);

    for now in 0..8 {
        controller.advance_startup_recovery(&runtime, now);
    }

    let stops: Vec<_> = endpoint
        .commands
        .iter()
        .filter_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.stream-session/1" && payload["action"] == "stop" => {
                Some(payload)
            }
            _ => None,
        })
        .collect();
    assert_eq!(stops.len(), 1, "retry only an exact failed journal owner");
    assert_eq!(stops[0]["session"], 23);
    assert!(stops[0].get("owner").is_none(), "broker supplies the owner");
    assert!(!controller.startup_recovery.ready());
    controller.retirement_status(
        1,
        "amaterasu.nvhttp",
        Some("old-operation"),
        Some(&"0".repeat(32)),
        &runtime,
    );
    assert_eq!(endpoint.completions.last().unwrap().1["retired"], false);
    controller.open(
        OpenRequest {
            request: 2,
            caller: "amaterasu.nvhttp".into(),
            config: open(),
            deadline: 5000,
            display_preflight_deadline: None,
        },
        8,
        &runtime,
        false,
    );
    assert_eq!(
        endpoint.completions.last().unwrap().1["code"],
        "startup_reconciliation_pending"
    );

    endpoint
        .recovery_results
        .push_back(Some(listed_stream_sessions(
            json!([
                {"session":23,"state":"stopped","cleanup_error":null},
                {"session":24,"state":"stopped","cleanup_error":null}
            ]),
            2,
        )));
    controller.advance_startup_recovery(&runtime, 257);
    controller.advance_startup_recovery(&runtime, 258);
    assert!(controller.startup_recovery.ready());
}

#[test]
fn startup_reconciliation_failed_owner_retry_stays_blocked() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    startup_recovery_pending(&mut controller, 45_000);
    let mut blocked = listed_stream_sessions(
        json!([
            {"session":23,"state":"cleanup_failed","cleanup_error":"component_cleanup_failed"}
        ]),
        1,
    );
    blocked["startup_reconciled"] = json!(false);
    let mut endpoint = Endpoint {
        recovery_results: VecDeque::from([
            Some(blocked),
            Some(
                json!({"ok":true,"session":23,"state":"cleanup_failed","cleanup_error":"component_cleanup_failed"}),
            ),
        ]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    for now in 0..8 {
        controller.advance_startup_recovery(&runtime, now);
    }
    assert!(controller.startup_recovery.blocked());
    assert_eq!(
        controller.startup_recovery.error,
        Some("startup_reconciliation_cleanup_failed")
    );
    assert!(!controller.startup_recovery.ready());
    assert_eq!(
        endpoint
            .commands
            .iter()
            .filter(|command| matches!(command,
        Command::Request { payload,.. } if payload["action"] == "stop"))
            .count(),
        1
    );
}

#[test]
fn startup_reconciliation_validates_the_complete_unreconciled_page_before_retry() {
    let malformed = [
        json!([
            {"session":23,"state":"cleanup_failed","cleanup_error":"cleanup_failed"},
            {"session":23,"state":"stopping","cleanup_error":null}
        ]),
        json!([
            {"session":23,"state":"cleanup_failed","cleanup_error":"cleanup_failed"},
            {"session":0,"state":"stopping","cleanup_error":null}
        ]),
        json!([
            {"session":23,"state":"cleanup_failed","cleanup_error":"cleanup_failed"},
            {"session":24,"state":"unknown","cleanup_error":null}
        ]),
    ];
    for sessions in malformed {
        let mut controller = Controller::new(Settings::default()).unwrap();
        startup_recovery_pending(&mut controller, 45_000);
        let mut page = listed_stream_sessions(sessions, 2);
        page["startup_reconciled"] = json!(false);
        let mut endpoint = Endpoint {
            recovery_results: VecDeque::from([Some(page)]),
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        controller.advance_startup_recovery(&runtime, 0);
        controller.advance_startup_recovery(&runtime, 1);
        assert!(controller.startup_recovery.blocked());
        assert!(!endpoint.commands.iter().any(|command| matches!(command,
            Command::Request {payload,..} if payload["action"] == "stop")));
    }
}

#[test]
fn startup_reconciliation_does_not_repeat_a_successful_stop_for_a_still_live_owner() {
    for reconciled in [false, true] {
        let mut controller = Controller::new(Settings {
            allowed_principals: vec!["amaterasu.nvhttp".into()],
            ..Settings::default()
        })
        .unwrap();
        startup_recovery_pending(&mut controller, 45_000);
        let mut page = listed_stream_sessions(
            json!([
                {"session":23,"state":"cleanup_failed","cleanup_error":"component_cleanup_failed"}
            ]),
            1,
        );
        page["startup_reconciled"] = json!(false);
        let mut contradictory = page.clone();
        contradictory["startup_reconciled"] = json!(reconciled);
        let mut endpoint = Endpoint {
            recovery_results: VecDeque::from([
                Some(page.clone()),
                Some(json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null})),
                Some(contradictory),
            ]),
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        for now in 0..10 {
            controller.advance_startup_recovery(&runtime, now);
        }
        assert!(controller.startup_recovery.blocked());
        assert_eq!(
            controller.startup_recovery.error,
            Some("startup_reconciliation_cleanup_unconfirmed")
        );
        assert_eq!(
            endpoint
                .commands
                .iter()
                .filter(|command| matches!(command,
            Command::Request {payload,..} if payload["action"] == "stop"))
                .count(),
            1
        );
        controller.reconcile_startup(1, "amaterasu.nvhttp", 100, &runtime);
        assert_eq!(controller.startup_recovery.phase, RecoveryPhase::List);
        assert!(controller.startup_recovery.attempted_sessions.is_empty());
        endpoint.recovery_results.extend([
            Some(page),
            Some(json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null})),
            Some(listed_stream_sessions(json!([]), 0)),
        ]);
        for now in 101..110 {
            controller.advance_startup_recovery(&runtime, now);
        }
        assert!(controller.startup_recovery.ready());
        assert_eq!(
            endpoint
                .commands
                .iter()
                .filter(|command| matches!(command,
            Command::Request {payload,..} if payload["action"] == "stop"))
                .count(),
            2
        );
    }
}

#[test]
fn startup_reconciliation_blocked_settlement_recovers_after_repeated_transport_failures() {
    let mut controller = Controller::new(Settings {
        allowed_principals: vec!["amaterasu.nvhttp".into()],
        retirement_status_principals: vec!["observer".into()],
        retirement_status_owners: BTreeMap::from([("observer".into(), "amaterasu.nvhttp".into())]),
        ..Settings::default()
    })
    .unwrap();
    startup_recovery_pending(&mut controller, 45_000);
    let mut endpoint = Endpoint {
        deny_settlement: true,
        recovery_results: VecDeque::from([Some(listed_stream_sessions(json!([]), 0))]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    for now in 0..12 {
        controller.advance_startup_recovery(&runtime, now);
    }
    assert!(controller.startup_recovery.blocked());
    assert!(controller.startup_recovery.call.is_some());
    endpoint.deny_settlement = false;
    for now in 12..17 {
        controller.advance_startup_recovery(&runtime, now);
    }
    assert!(
        controller.startup_recovery.call.is_none(),
        "settle the same request when transport recovers"
    );
    assert!(
        controller.startup_recovery.blocked(),
        "settlement alone never proves startup cleanup"
    );
    assert_eq!(
        endpoint
            .commands
            .iter()
            .filter(|command| matches!(command, Command::Request { .. }))
            .count(),
        1
    );
    assert!(
        endpoint
            .commands
            .iter()
            .any(|command| matches!(command, Command::ReleaseResult { request: 701 }))
    );
    controller.reconcile_startup(1, "observer", 17, &runtime);
    assert_eq!(
        endpoint.completions.last().unwrap().1["code"],
        "permission_denied"
    );
    assert!(controller.startup_recovery.blocked());
    controller.reconcile_startup(2, "amaterasu.nvhttp", 17, &runtime);
    assert_eq!(controller.startup_recovery.phase, RecoveryPhase::List);
}

#[test]
fn stale_epoch_admission_and_blocked_reconciliation_cannot_simulate_retirement() {
    let mut controller = Controller::new(Settings {
        allowed_principals: vec!["caller".into()],
        ..Settings::default()
    })
    .unwrap();
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let mut config = open();
    config.expected_runtime_epoch = Some("0".repeat(32));
    controller.open(
        OpenRequest {
            request: 1,
            caller: "caller".into(),
            config,
            deadline: 5000,
            display_preflight_deadline: None,
        },
        0,
        &runtime,
        false,
    );
    assert_eq!(endpoint.completions[0].1["code"], "runtime_epoch_changed");
    assert!(controller.records.is_empty());
    assert!(
        !endpoint
            .commands
            .iter()
            .any(|command| matches!(command, Command::Request { .. }))
    );
    controller.startup_recovery.phase = RecoveryPhase::Blocked;
    controller.startup_recovery.operation_sequence = 17;
    controller.retirement_status(2, "caller", Some("old-op"), Some(&"0".repeat(32)), &runtime);
    assert_eq!(endpoint.completions[1].1["retired"], false);
    controller.reconcile_startup(3, "caller", 1000, &runtime);
    assert_eq!(controller.startup_recovery.phase, RecoveryPhase::List);
    assert_eq!(controller.startup_recovery.operation_sequence, 17);
    assert_eq!(endpoint.completions[2].1["startup_reconciled"], false);
}

#[test]
fn exact_operation_retirement_fences_delayed_admission_and_waits_for_native_close() {
    let mut controller = Controller::new(Settings {
        allowed_principals: vec!["caller".into()],
        retirement_status_principals: vec!["observer".into()],
        retirement_status_owners: BTreeMap::from([("observer".into(), "caller".into())]),
        ..Settings::default()
    })
    .unwrap();
    let epoch = controller.runtime_epoch.clone();
    let mut endpoint = Endpoint {
        verifier_reply: Some(
            json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null}),
        ),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    controller.retire_operation(1, "caller", "never-admitted", &epoch, 0, &runtime);
    assert_eq!(endpoint.completions[0].1["state"], "operation_fenced");
    assert_eq!(endpoint.completions[0].1["retired"], true);
    let mut delayed = open();
    delayed.operation_id = "never-admitted".into();
    delayed.expected_runtime_epoch = Some(epoch.clone());
    controller.settings.enabled = true;
    controller.open(
        OpenRequest {
            request: 2,
            caller: "caller".into(),
            config: delayed,
            deadline: 5000,
            display_preflight_deadline: None,
        },
        0,
        &runtime,
        false,
    );
    assert_eq!(endpoint.completions[1].1["code"], "operation_retired");
    controller.records.insert(5, record(State::Playing));
    controller.retire_operation(3, "caller", "caller-operation", &epoch, 0, &runtime);
    assert_eq!(endpoint.completions[2].1["retired"], false);
    assert_eq!(endpoint.completions[2].1["active_operations"], 1);
    for now in 1..8 {
        controller.advance_job(&runtime, now);
    }
    assert!(controller.records[&5].cleanup_confirmed);
    assert!(endpoint.commands.iter().any(|command|matches!(command,Command::Request{service,payload,..} if service=="amaterasu.stream-session/1" && payload["action"]=="stop" && payload["session"]==23)));
    controller.retirement_status(
        4,
        "observer",
        Some("caller-operation"),
        Some(&epoch),
        &runtime,
    );
    assert_eq!(endpoint.completions.last().unwrap().1["retired"], true);
    assert_eq!(
        endpoint.completions.last().unwrap().1["active_operations"],
        0
    );
    controller.retire_operation(5, "observer", "foreign", &epoch, 0, &runtime);
    assert_eq!(
        endpoint.completions.last().unwrap().1["code"],
        "permission_denied"
    );
    for index in controller.retired_operations.len()..MAX_RECORDS {
        controller
            .retired_operations
            .insert(("caller".into(), format!("fenced-{index}")));
    }
    controller.retire_operation(6, "caller", "over-capacity", &epoch, 0, &runtime);
    assert_eq!(
        endpoint.completions.last().unwrap().1["code"],
        "retirement_fence_capacity_exhausted"
    );
}

#[test]
fn startup_reconciliation_stops_only_sessions_in_rtsp_scoped_status_and_waits_terminal() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    startup_recovery_pending(&mut controller, 45_000);
    let mut endpoint = Endpoint {
        recovery_results: VecDeque::from([
            Some(listed_stream_sessions(
                json!([{"session":23,"state":"active","cleanup_error":null}]),
                1,
            )),
            Some(json!({"ok":true,"session":23,"state":"stopping","cleanup_error":null})),
            Some(json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null})),
            Some(listed_stream_sessions(
                json!([
                    {"session":23,"state":"stopped","cleanup_error":null}
                ]),
                1,
            )),
        ]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);

    for now in [0, 1, 2, 3, 253, 254, 255, 256, 257] {
        controller.advance_startup_recovery(&runtime, now);
    }

    assert!(controller.startup_recovery.ready());
    let actions: Vec<_> = endpoint
        .commands
        .iter()
        .filter_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.stream-session/1" => Some(payload),
            _ => None,
        })
        .collect();
    assert_eq!(actions[0]["action"], "status");
    assert!(actions[0]["session"].is_null());
    assert_eq!(actions[1]["action"], "stop");
    assert_eq!(actions[1]["session"], 23);
    assert_eq!(actions[2]["action"], "status");
    assert_eq!(actions[2]["session"], 23);
    assert_eq!(actions[3]["action"], "status");
    assert!(actions[3]["session"].is_null());
    assert!(!actions.iter().any(|action| action["session"] == 24));
}

#[test]
fn startup_reconciliation_clean_status_admits_new_launch_without_stop() {
    let settings: Settings = serde_json::from_value(json!({
        "enabled":true,
        "bind":"127.0.0.1:0",
        "allowed_principals":["amaterasu.nvhttp"]
    }))
    .unwrap();
    let mut controller = Controller::new(settings).unwrap();
    startup_recovery_pending(&mut controller, 45_000);
    let mut endpoint = Endpoint {
        active_request: true,
        recovery_results: VecDeque::from([Some(listed_stream_sessions(json!([]), 0))]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);

    controller.advance_startup_recovery(&runtime, 0);
    controller.advance_startup_recovery(&runtime, 1);

    assert!(controller.startup_recovery.ready());
    assert!(endpoint.commands.iter().all(|command| !matches!(command,
        Command::Request { payload, .. } if payload["action"] == "stop")));
    controller.open(
        OpenRequest {
            request: 19,
            caller: "amaterasu.nvhttp".into(),
            config: open(),
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        2,
        &runtime,
        false,
    );
    assert!(controller.job.is_some());
    controller.advance_job(&runtime, 3);
    assert!(endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { service, payload, .. }
            if service == "amaterasu.stream-session/1" && payload["action"] == "prepare")));
}

#[test]
fn restart_reconciliation_stops_old_lease_before_fresh_resume_open() {
    let settings: Settings = serde_json::from_value(json!({
        "enabled":true,
        "bind":"127.0.0.1:0",
        "allowed_principals":["amaterasu.nvhttp"]
    }))
    .unwrap();
    let mut controller = Controller::new(settings).unwrap();
    // A new RTSP controller instance has no ticket records. The stream-session
    // broker still owns the previous in-memory Desktop lease.
    startup_recovery_pending(&mut controller, 45_000);
    let mut endpoint = Endpoint {
        active_request: true,
        recovery_results: VecDeque::from([
            Some(listed_stream_sessions(
                json!([{"session":23,"state":"active","cleanup_error":null}]),
                1,
            )),
            Some(json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null})),
            Some(listed_stream_sessions(
                json!([
                    {"session":23,"state":"stopped","cleanup_error":null}
                ]),
                1,
            )),
            Some(json!({
                "ok":true,
                "session":24,
                "state":"prepared",
                "audio_local":"127.0.0.1:51000",
                "video_local":"127.0.0.1:51002",
                "control_local":"127.0.0.1:51004",
                "history_activity_id":null,
                "rtsp_operation_id":null
            })),
        ]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);

    let mut resumed = open();
    resumed.operation_id = "nvhttp-resume-fresh".into();
    controller.open(
        OpenRequest {
            request: 19,
            caller: "amaterasu.nvhttp".into(),
            config: resumed.clone(),
            deadline: 10_000,
            display_preflight_deadline: None,
        },
        0,
        &runtime,
        false,
    );
    assert_eq!(
        endpoint.completions.last().unwrap().1["code"],
        "startup_reconciliation_pending"
    );
    assert!(!endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { payload, .. } if payload["action"] == "prepare")));

    for now in [1, 2, 3, 4, 5, 6, 7] {
        controller.advance_startup_recovery(&runtime, now);
    }
    assert!(controller.startup_recovery.ready());

    controller.open(
        OpenRequest {
            request: 20,
            caller: "amaterasu.nvhttp".into(),
            config: resumed,
            deadline: 10_000,
            display_preflight_deadline: None,
        },
        8,
        &runtime,
        false,
    );
    for now in 9..13 {
        controller.advance_job(&runtime, now);
    }

    assert_eq!(endpoint.completions.last().unwrap().0, 20);
    assert_eq!(endpoint.completions.last().unwrap().1["ok"], true);
    let requests: Vec<_> = endpoint
        .commands
        .iter()
        .filter_map(|command| match command {
            Command::Request {
                service,
                payload,
                operation_id,
                ..
            } if service == "amaterasu.stream-session/1" => Some((payload, operation_id.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].0["action"], "status");
    assert_eq!(requests[1].0["action"], "stop");
    assert_eq!(requests[1].0["session"], 23);
    assert_eq!(requests[2].0["action"], "status");
    assert!(requests[2].0["session"].is_null());
    assert_eq!(requests[3].0["action"], "prepare");
    let new_ticket = controller
        .records
        .values()
        .find(|record| record.config.operation_id == "nvhttp-resume-fresh")
        .expect("fresh resume ticket")
        .id;
    assert_eq!(
        requests[3].0["operation_id"],
        format!("rtsp-{new_ticket:016x}")
    );
    assert_ne!(requests[1].1, requests[3].1);
    assert!(controller.records.values().any(|record| {
        record.config.operation_id == "nvhttp-resume-fresh"
            && record.session == Some(24)
            && record.state == State::Ready
    }));
    assert!(
        !requests
            .iter()
            .any(|(payload, _)| payload["action"] == "stop" && payload["session"] == 24)
    );
}

#[test]
fn startup_reconciliation_timeout_after_partial_stop_keeps_launch_fail_closed() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    startup_recovery_pending(&mut controller, 300);
    let mut endpoint = Endpoint {
        recovery_results: VecDeque::from([
            Some(listed_stream_sessions(
                json!([{"session":23,"state":"active","cleanup_error":null}]),
                1,
            )),
            Some(json!({"ok":true,"session":23,"state":"stopping","cleanup_error":null})),
            None,
        ]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);

    for now in [0, 1, 2, 3, 253, 299, 300] {
        controller.advance_startup_recovery(&runtime, now);
    }

    assert!(controller.startup_recovery.blocked());
    assert!(!controller.startup_recovery.ready());
    let mut config = open();
    config.operation_id = "new-launch".into();
    controller.open(
        OpenRequest {
            request: 19,
            caller: "amaterasu.nvhttp".into(),
            config,
            deadline: 1_000,
            display_preflight_deadline: None,
        },
        301,
        &runtime,
        false,
    );
    assert!(
        endpoint
            .completions
            .iter()
            .any(|(_, reply)| { reply["code"] == "startup_reconciliation_timeout" })
    );
    assert!(!endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { payload, .. } if payload["action"] == "prepare")));
}

pub(super) fn record(state: State) -> Record {
    Record {
        id: 5,
        owner: "caller".into(),
        config: open(),
        state,
        session: Some(23),
        negotiation: None,
        negotiated_encryption: None,
        negotiated_slices_per_frame: None,
        negotiated_video_stream_format: None,
        client_feature_flags: None,
        negotiated_color: None,
        negotiated_video: None,
        url: "rtspenc://127.0.0.1:48010/launch-5".into(),
        session_token: "00112233445566778899aabbccddeeff".into(),
        udp_discovery: None,
        local: "127.0.0.1:48010".parse().expect("literal"),
        media_binds: None,
        expires: 5_000,
        next_status: 5_000,
        failure: None,
        cleanup_error: None,
        cleanup_retry_count: 0,
        cleanup_retry_at: None,
        uncertain: None,
        uncertain_phase: None,
        needs_lookup: false,
        stream_state: Some("stopping".into()),
        lifecycle_target: None,
        lifecycle_candidate: None,
        lifecycle_source_target: None,
        lifecycle_retained_source: None,
        lifecycle_prepare_outstanding: false,
        lifecycle_input_required: None,
        authenticated_disconnect_reason: None,
        display_applied: false,
        display_preflight_deadline: None,
        display_lease_ready_until: None,
        display_prepare_timeout_ms: None,
        revoke_after: None,
        cleanup_confirmed: false,
    }
}

#[test]
fn response_echoes_frozen_display_policy_for_authenticated_launch_readback() {
    let mut record = record(State::Ready);
    let snapshot = json!({
        "global_revision": 7,
        "application_overrides": {"dd_configuration_option":"verify_only"},
        "virtual_display_intent": null,
    });
    record.config.display_policy_snapshot = Some(snapshot.clone());

    assert_eq!(record.response()["display_policy_snapshot"], snapshot);
}

fn attested_open() -> Open {
    serde_json::from_value(json!({
        "operation_id":"rtsp-one","application_operation_id":"launch-one",
        "display_admission_token":"ab".repeat(32),
        "display_policy_snapshot":{"global_revision":1,"global_overrides":{},
            "catalog_revision":0,"certificate_id":"cd".repeat(32),
            "application_overrides":{},"client_overrides":{},"effective_overrides":{}},
        "display_request":{"expected_topology_revision":"aa".repeat(16),"configuration":{"displays":[
            {"id":"one","width":1920,"height":1080,"refresh":{"numerator":60,"denominator":1},"x":0,"y":0}
        ]},"mode_policy":{"display_id":"one","configuration_option":"ensure_only_display","remapping":{},"context":{
            "resolution_is_automatic":true,"refresh_rate_is_automatic":true,
            "client_display_mode_override":false,"optimize_game_settings":false}}},
        "peer_ip":"127.0.0.1","video_port":50000,"audio_port":50002,
        "key_hex":"00112233445566778899aabbccddeeff","key_id":1,"connect_data":2,
        "capture":{"capture_provider":"dxgi","output":"display","encoder":1,"audio_endpoint":"default"}
    })).unwrap()
}

#[test]
fn display_verifier_runs_before_ticket_or_stream_prepare() {
    let settings: Settings = serde_json::from_value(json!({
        "enabled":true,"bind":"127.0.0.1:0",
        "allowed_principals":["amaterasu.nvhttp","other"]
    }))
    .unwrap();
    let mut controller = Controller::new(settings).unwrap();
    let mut config = attested_open();
    config.ping_timeout_ms = 1_000;
    let retry_config = config.clone();
    let digest = config.display_request_sha256().unwrap();
    let snapshot = config.display_policy_snapshot.clone().unwrap();
    let mut endpoint = Endpoint {
        active_request: true,
        verifier_reply: Some(json!({"ok":true,"verified":true,
            "rtsp_operation_id":"rtsp-one","request_sha256":digest,
            "preflight_remaining_ms":10_000})),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    controller.open(
        OpenRequest {
            request: 19,
            caller: "amaterasu.nvhttp".into(),
            config,
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        0,
        &runtime,
        false,
    );
    assert!(controller.records.is_empty());
    assert!(controller.job.is_none());
    controller.advance_display_admission(&runtime, 1);
    assert!(controller.records.is_empty());
    controller.advance_display_admission(&runtime, 2);
    assert!(controller.records.is_empty());
    controller.advance_display_admission(&runtime, 3);
    assert_eq!(controller.records.len(), 1);
    let admitted = controller.records.values().next().unwrap();
    assert_eq!(admitted.display_preflight_deadline, Some(10_000));
    assert_eq!(admitted.display_lease_ready_until, Some(5_000));
    assert_eq!(admitted.expires, 5_000);
    controller.advance_job(&runtime, 4);
    let prepare = endpoint
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.stream-session/1" => Some(payload),
            _ => None,
        })
        .unwrap();
    assert_eq!(prepare["ping_timeout_ms"], 1_000);
    assert_eq!(prepare["display_prepare_timeout_ms"], 4_997);
    let verify = endpoint
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.launch/1" => Some(payload),
            _ => None,
        })
        .unwrap();
    assert_eq!(verify["action"], "verify_display_admission");
    assert_eq!(verify["display_policy_snapshot"], snapshot);
    assert_eq!(verify["request_sha256"], digest);
    assert_eq!(verify["catalog_revision"], 0);
    let verifier_operation = endpoint
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service,
                operation_id,
                ..
            } if service == "amaterasu.launch/1" => Some(operation_id),
            _ => None,
        })
        .unwrap();
    assert!(!verifier_operation.contains("abababababababab"));
    assert!(
        endpoint
            .commands
            .iter()
            .any(|command| matches!(command, Command::ReleaseResult { request: 71 }))
    );
    assert!(
        controller
            .records
            .values()
            .next()
            .unwrap()
            .response()
            .get("display_admission_token")
            .is_none()
    );
    assert!(
        controller.job.is_some(),
        "stream Prepare starts only after verification"
    );
    let frozen = controller.records.values().next().unwrap().response();
    assert_eq!(
        frozen["display_request"]["mode_policy"]["configuration_option"],
        "ensure_only_display"
    );
    controller.open(
        OpenRequest {
            request: 20,
            caller: "amaterasu.nvhttp".into(),
            config: retry_config.clone(),
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        4,
        &runtime,
        false,
    );
    assert!(
        endpoint
            .completions
            .iter()
            .any(|(request, response)| *request == 20 && *response == frozen)
    );
    let mut changed = retry_config;
    changed.display_request.as_mut().unwrap()["expected_topology_revision"] =
        json!("bb".repeat(16));
    controller.open(
        OpenRequest {
            request: 21,
            caller: "amaterasu.nvhttp".into(),
            config: changed,
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        4,
        &runtime,
        false,
    );
    assert!(
        endpoint
            .completions
            .iter()
            .any(|(request, response)| *request == 21 && response["code"] == "operation_conflict")
    );
    assert_eq!(controller.records.len(), 1);
}

#[test]
fn frozen_display_prepare_requires_owned_applied_readback_before_ticket_ready() {
    for failure in [
        "missing_lease",
        "wrong_request",
        "missing_timeout",
        "expired",
        "valid",
    ] {
        let settings: Settings = serde_json::from_value(json!({
            "enabled":true,"bind":"127.0.0.1:0","allowed_principals":["caller"]
        }))
        .unwrap();
        let mut controller = Controller::new(settings).unwrap();
        let mut record = record(State::Preparing);
        record.config = attested_open();
        record.display_preflight_deadline = Some(100);
        record.display_lease_ready_until = Some(50);
        record.display_prepare_timeout_ms = Some(40);
        let job = Job {
            ticket: record.id,
            phase: Phase::Prepare,
            call: Call::new(json!({"action":"prepare"}), "prepare".into(), Some(19)),
            caller: Some(19),
            network: None,
            deadline: 5_000,
        };
        let mut prepared = json!({
            "ok":true,"state":"prepared","session":23,
            "audio_local":"127.0.0.1:51000","video_local":"127.0.0.1:51002",
            "control_local":"127.0.0.1:51004",
            "display_request":record.config.display_request,
            "display_applied":true,"display_state":"applied",
            "display_prepare_timeout_ms":40,
        });
        match failure {
            "missing_lease" => prepared["display_applied"] = json!(false),
            "wrong_request" => {
                prepared["display_request"]["expected_topology_revision"] = json!("bb".repeat(16))
            }
            "missing_timeout" => prepared["display_prepare_timeout_ms"] = Value::Null,
            _ => {}
        }
        let now = if failure == "expired" { 100 } else { 10 };
        let mut endpoint = Endpoint {
            active_request: true,
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        controller.prepared(&mut record, job, &prepared, now, &runtime);
        if failure == "valid" {
            assert!(record.state == State::Ready);
            assert_eq!(record.response()["display_applied"], true);
            assert_eq!(record.expires, 50);
            let exact_retry = record.config.clone();
            finish_record(&mut record);
            assert_eq!(record.response()["display_applied"], false);
            controller.records.insert(record.id, record);
            controller.open(
                OpenRequest {
                    request: 20,
                    caller: "caller".into(),
                    config: exact_retry,
                    deadline: 5_000,
                    display_preflight_deadline: None,
                },
                20,
                &runtime,
                false,
            );
            assert!(endpoint.completions.iter().any(|(request, response)| {
                *request == 20
                    && response["state"] == "closed"
                    && response["display_applied"] == false
            }));
        } else {
            assert!(record.state == State::Closing, "{failure}");
            assert_eq!(record.response()["display_applied"], false);
            assert!(record.failure.is_some());
        }
    }
}

#[test]
fn nearly_expired_staged_admission_never_allocates_a_stream() {
    let settings: Settings = serde_json::from_value(json!({
        "enabled":true,"bind":"127.0.0.1:0","allowed_principals":["amaterasu.nvhttp"]
    }))
    .unwrap();
    let mut controller = Controller::new(settings).unwrap();
    let config = attested_open();
    let digest = config.display_request_sha256().unwrap();
    let mut endpoint = Endpoint {
        active_request: true,
        verifier_reply: Some(json!({
            "ok":true,"verified":true,"rtsp_operation_id":config.operation_id,
            "request_sha256":digest,"preflight_remaining_ms":5_000
        })),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    controller.open(
        OpenRequest {
            request: 19,
            caller: "amaterasu.nvhttp".into(),
            config,
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        0,
        &runtime,
        false,
    );
    for now in 1..=3 {
        controller.advance_display_admission(&runtime, now);
    }
    assert!(controller.records.is_empty());
    assert!(controller.job.is_none());
    assert!(
        endpoint
            .completions
            .iter()
            .any(|(_, response)| { response["code"] == "display_admission_expired" })
    );
}

#[test]
fn staged_ready_expiry_rejects_play_before_negotiation_and_arm_uses_ping_window() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut expired = record(State::Ready);
    expired.expires = 100;
    controller.records.insert(expired.id, expired);
    controller.handle_network(77, 5, request(Method::Play, "/", 7, true), 100);
    let expired = &controller.records[&5];
    assert!(expired.state == State::Closing);
    assert_eq!(expired.failure, Some("prepare_expired"));
    assert!(controller.job.is_none());

    let mut accepted = record(State::Ready);
    accepted.expires = 100;
    accepted.config.ping_timeout_ms = 10_000;
    let deadline = accepted.begin_arm(99);
    assert_eq!(deadline, 10_099);
    assert_eq!(accepted.expires, deadline);
    assert!(accepted.state == State::Arming);
}

#[test]
fn confirmed_stop_clears_display_readback_while_teardown_reply_is_pending() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut record = record(State::Closing);
    record.display_applied = true;
    record.cleanup_confirmed = true;
    record.revoke_after = Some(77);
    let job = Job {
        ticket: record.id,
        phase: Phase::Stop,
        call: Call::new(json!({"action":"stop"}), "stop".into(), None),
        caller: None,
        network: None,
        deadline: 5_000,
    };
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    controller.finish_cleanup_reply(&mut record, job, &runtime);
    assert!(record.state == State::Closing);
    assert_eq!(record.response()["display_applied"], false);
    assert!(record.cleanup_confirmed);
}

#[test]
fn malformed_topology_revision_never_reaches_verifier_or_stream_prepare() {
    for malformed in [
        serde_json::Value::Null,
        json!("AA".repeat(16)),
        json!("aa".repeat(15)),
    ] {
        let settings: Settings = serde_json::from_value(json!({
            "enabled":true,"bind":"127.0.0.1:0",
            "allowed_principals":["amaterasu.nvhttp"]
        }))
        .unwrap();
        let mut controller = Controller::new(settings).unwrap();
        let mut config = attested_open();
        config.display_request.as_mut().unwrap()["expected_topology_revision"] = malformed;
        let mut endpoint = Endpoint {
            active_request: true,
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        controller.open(
            OpenRequest {
                request: 19,
                caller: "amaterasu.nvhttp".into(),
                config,
                deadline: 5_000,
                display_preflight_deadline: None,
            },
            0,
            &runtime,
            false,
        );
        assert!(controller.records.is_empty());
        assert!(controller.job.is_none());
        assert!(controller.display_admission.is_none());
        assert!(
            !endpoint
                .commands
                .iter()
                .any(|command| matches!(command, Command::Request { .. }))
        );
        assert!(
            endpoint
                .completions
                .iter()
                .any(|(_, response)| response["code"] == "display_request_unattested")
        );
    }
}

#[test]
fn invalid_configuration_option_never_reaches_verifier_or_stream_prepare() {
    for invalid in [json!("ensure_everything"), json!("Ensure_Active"), json!(7)] {
        let settings: Settings = serde_json::from_value(json!({
            "enabled":true,"bind":"127.0.0.1:0",
            "allowed_principals":["amaterasu.nvhttp"]
        }))
        .unwrap();
        let mut controller = Controller::new(settings).unwrap();
        let mut config = attested_open();
        config.display_request.as_mut().unwrap()["mode_policy"]["configuration_option"] = invalid;
        let mut endpoint = Endpoint {
            active_request: true,
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        controller.open(
            OpenRequest {
                request: 19,
                caller: "amaterasu.nvhttp".into(),
                config,
                deadline: 5_000,
                display_preflight_deadline: None,
            },
            0,
            &runtime,
            false,
        );
        assert!(controller.records.is_empty());
        assert!(controller.job.is_none());
        assert!(controller.display_admission.is_none());
        assert!(
            !endpoint
                .commands
                .iter()
                .any(|command| matches!(command, Command::Request { .. }))
        );
        assert!(
            endpoint
                .completions
                .iter()
                .any(|(_, response)| response["code"] == "display_request_unattested")
        );
    }
}

#[test]
fn forged_or_unavailable_display_admission_never_creates_a_ticket() {
    for failure in [
        "wrong_caller",
        "wrong_hash",
        "wrong_revision_digest",
        "rejected",
        "missing_grant",
    ] {
        let settings: Settings = serde_json::from_value(json!({
            "enabled":true,"bind":"127.0.0.1:0",
            "allowed_principals":["amaterasu.nvhttp","other"]
        }))
        .unwrap();
        let mut controller = Controller::new(settings).unwrap();
        let mut config = attested_open();
        let digest = config.display_request_sha256().unwrap();
        if failure == "wrong_revision_digest" {
            config.display_request.as_mut().unwrap()["expected_topology_revision"] =
                json!("bb".repeat(16));
            assert_ne!(config.display_request_sha256().unwrap(), digest);
        }
        let mut endpoint = Endpoint {
            active_request: true,
            verifier_reply: Some(json!({"ok":true,"verified":true,
                "rtsp_operation_id":"rtsp-one","request_sha256":if failure == "wrong_hash" { "00".repeat(32) } else { digest } })),
            deny_launch: failure == "missing_grant",
            ..Endpoint::default()
        };
        if failure == "rejected" {
            endpoint.verifier_reply = Some(json!({"ok":false,"code":"display_admission_denied"}));
        }
        let runtime = client(&mut endpoint);
        controller.open(
            OpenRequest {
                request: 19,
                caller: if failure == "wrong_caller" {
                    "other"
                } else {
                    "amaterasu.nvhttp"
                }
                .into(),
                config,
                deadline: 5_000,
                display_preflight_deadline: None,
            },
            0,
            &runtime,
            false,
        );
        for now in 1..4 {
            controller.advance_display_admission(&runtime, now);
        }
        assert!(controller.records.is_empty(), "{failure}");
        assert!(controller.job.is_none(), "{failure}");
        assert!(
            !endpoint.commands.iter().any(|command| matches!(command,
            Command::Request { service, .. } if service == "amaterasu.stream-session/1")),
            "{failure}"
        );
        assert!(
            endpoint
                .completions
                .iter()
                .any(|(_, reply)| reply["ok"] == false),
            "{failure}"
        );
    }
}

#[test]
fn uncertain_verifier_settlement_keeps_child_identity_until_release() {
    let settings: Settings = serde_json::from_value(json!({
        "enabled":true,"bind":"127.0.0.1:0",
        "allowed_principals":["amaterasu.nvhttp"]
    }))
    .unwrap();
    let mut controller = Controller::new(settings).unwrap();
    let config = attested_open();
    let digest = config.display_request_sha256().unwrap();
    let mut endpoint = Endpoint {
        active_request: true,
        verifier_reply: Some(json!({"ok":true,"verified":true,
            "rtsp_operation_id":"rtsp-one","request_sha256":digest})),
        deny_settlement: true,
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    controller.open(
        OpenRequest {
            request: 19,
            caller: "amaterasu.nvhttp".into(),
            config,
            deadline: 5,
            display_preflight_deadline: None,
        },
        0,
        &runtime,
        false,
    );
    for now in 1..=5 {
        controller.advance_display_admission(&runtime, now);
    }
    assert!(controller.display_admission.is_none());
    assert_eq!(controller.display_cleanup.len(), 1);
    assert!(controller.records.is_empty());
    assert!(controller.job.is_none());
    endpoint.deny_settlement = false;
    controller.advance_display_cleanup(&runtime);
    assert!(controller.display_cleanup.is_empty());
    assert!(
        endpoint
            .commands
            .iter()
            .any(|command| matches!(command, Command::ReleaseResult { request: 71 }))
    );
    assert!(
        endpoint
            .completions
            .iter()
            .any(|(request, response)| *request == 19 && response["ok"] == false)
    );
}

#[test]
fn expired_pending_display_verification_is_cancelled_without_waiting_for_result() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut endpoint = Endpoint {
        active_request: true,
        verifier_pending: true,
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    admit_pending_display_verifier(&mut controller, &runtime, 5);

    controller.advance_display_admission(&runtime, 1);
    assert!(controller.display_admission.is_some());
    controller.advance_display_admission(&runtime, 5);

    assert!(controller.display_admission.is_none());
    assert!(controller.display_cleanup.is_empty());
    assert!(controller.records.is_empty());
    assert!(controller.job.is_none());
    assert!(
        endpoint
            .commands
            .iter()
            .any(|command| matches!(command, Command::Cancel { request: 71 }))
    );
    assert!(
        endpoint
            .commands
            .iter()
            .any(|command| matches!(command, Command::ReleaseResult { request: 71 }))
    );
    assert_eq!(
        endpoint
            .commands
            .iter()
            .filter(|command| matches!(command, Command::TakeResult { request: 71 }))
            .count(),
        1,
        "the pre-deadline poll observes Pending once; cancellation settles without another read"
    );
    assert!(!endpoint.commands.iter().any(|command| matches!(
        command,
        Command::Request { service, .. } if service == "amaterasu.stream-session/1"
    )));
}

#[test]
fn quiescence_cancels_pending_display_verification_without_waiting_for_result() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut endpoint = Endpoint {
        active_request: true,
        verifier_pending: true,
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    admit_pending_display_verifier(&mut controller, &runtime, 5_000);
    assert!(controller.display_admission.is_some());

    assert!(!controller.quiesce(&runtime, 2));
    controller.advance_display_cleanup(&runtime);

    assert!(controller.display_cleanup.is_empty());
    assert!(controller.records.is_empty());
    assert!(controller.job.is_none());
    assert!(
        endpoint
            .commands
            .iter()
            .any(|command| matches!(command, Command::Cancel { request: 71 }))
    );
    assert!(
        endpoint
            .commands
            .iter()
            .any(|command| matches!(command, Command::ReleaseResult { request: 71 }))
    );
    assert!(
        !endpoint
            .commands
            .iter()
            .any(|command| matches!(command, Command::TakeResult { request: 71 }))
    );
    assert!(!endpoint.commands.iter().any(|command| matches!(
        command,
        Command::Request { service, .. } if service == "amaterasu.stream-session/1"
    )));
}

#[test]
fn cancelled_parent_reaches_cleanup_failed_when_settlement_stays_unavailable() {
    let mut controller = Controller::new(Settings::default()).expect("disabled controller");
    let config = open();
    let record = Record {
        id: 5,
        owner: "caller".into(),
        config,
        state: State::Preparing,
        session: None,
        negotiation: None,
        negotiated_encryption: None,
        negotiated_slices_per_frame: None,
        negotiated_video_stream_format: None,
        client_feature_flags: None,
        negotiated_color: None,
        negotiated_video: None,
        url: "rtspenc://127.0.0.1:48010/launch-5".into(),
        session_token: "00112233445566778899aabbccddeeff".into(),
        udp_discovery: None,
        local: "127.0.0.1:48010".parse().expect("literal"),
        media_binds: None,
        expires: 5_000,
        next_status: 5_000,
        failure: None,
        cleanup_error: None,
        cleanup_retry_count: 0,
        cleanup_retry_at: None,
        uncertain: None,
        uncertain_phase: None,
        needs_lookup: true,
        stream_state: None,
        lifecycle_target: None,
        lifecycle_candidate: None,
        lifecycle_source_target: None,
        lifecycle_retained_source: None,
        lifecycle_prepare_outstanding: false,
        lifecycle_input_required: None,
        authenticated_disconnect_reason: None,
        display_applied: false,
        display_preflight_deadline: None,
        display_lease_ready_until: None,
        display_prepare_timeout_ms: None,
        revoke_after: None,
        cleanup_confirmed: false,
    };
    let payload = record.config.prepare(&record.operation(), None);
    controller.records.insert(record.id, record);
    controller.job = Some(Job {
        ticket: 5,
        phase: Phase::Prepare,
        call: Call::new(payload, "prepare-request".into(), Some(17)),
        caller: Some(17),
        network: None,
        deadline: 5_000,
    });
    let mut endpoint = Endpoint {
        active_request: true,
        deny_settlement: true,
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);

    controller.advance_job(&runtime, 0);
    endpoint.active_request = false;
    for now in 1..5 {
        controller.advance_job(&runtime, now);
    }

    let record = &controller.records[&5];
    assert!(record.state == State::CleanupFailed);
    assert_eq!(
        record.cleanup_error,
        Some("request_cancellation_unconfirmed")
    );
    assert!(record.uncertain.is_some());
    assert_eq!(
        endpoint
            .commands
            .iter()
            .filter(|command| matches!(command, Command::Request { .. }))
            .count(),
        1,
        "settlement retries must not resubmit an admitted mutation"
    );
}

#[test]
fn cleanup_status_polling_preserves_one_deadline() {
    let mut controller = Controller::new(Settings::default()).expect("disabled controller");
    let mut record = record(State::Closing);
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let stopping = json!({
        "ok":true,
        "session":23,
        "state":"stopping",
        "cleanup_error":null
    });
    let job = Job {
        ticket: 5,
        phase: Phase::Status,
        call: Call::new(json!({"action":"status"}), "status-1".into(), None),
        caller: None,
        network: None,
        deadline: 99,
    };

    controller.status_result(&mut record, job, &stopping, 10, &runtime);
    let job = controller.job.take().expect("next status poll");
    assert_eq!(job.deadline, 99);
    controller.status_result(&mut record, job, &stopping, 50, &runtime);
    let job = controller.job.take().expect("next status poll");
    assert_eq!(job.deadline, 99);

    controller.records.insert(record.id, record);
    controller.job = Some(job);
    controller.advance_job(&runtime, 99);
    let record = &controller.records[&5];
    assert!(record.state == State::CleanupFailed);
    assert_eq!(record.cleanup_error, Some("cleanup_deadline"));
    assert!(record.uncertain.is_some());
}

#[test]
fn configured_ipv6_listener_and_media_binds_use_the_selected_family() {
    let settings: Settings = serde_json::from_value(json!({
        "enabled": true,
        "address_family": "both",
        "bind_address": "::1",
        "bind": "[::1]:0"
    }))
    .unwrap();
    let controller = Controller::new(settings).expect("IPv6 RTSP listener");
    let local = controller.local.expect("bound RTSP listener");
    assert_eq!(local.ip(), "::1".parse::<IpAddr>().unwrap());
    assert!(TcpStream::connect(local).is_ok());

    let binds = controller
        .settings
        .media_binds(TEST_BASE_PORT)
        .expect("IPv6 media listeners");
    assert_eq!(binds.video.ip(), local.ip());
    assert_eq!(binds.control.ip(), local.ip());
    assert_eq!(binds.audio.ip(), local.ip());
}

#[test]
#[ignore = "opens a wildcard listener; run explicitly when testing remote binding"]
fn wildcard_listener_advertises_the_peer_route_and_uses_it_for_udp_binds() {
    let settings: Settings = serde_json::from_value(json!({
        "enabled": true,
        "allow_remote": true,
        "address_family": "ipv4",
        "bind_address": "",
        "bind": "127.0.0.1:0"
    }))
    .unwrap();
    let controller = Controller::new(settings).expect("wildcard RTSP listener");
    let listener = controller.local.expect("bound RTSP listener");
    assert!(listener.ip().is_unspecified());

    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let advertised = controller
        .advertised_local_for_peer(peer)
        .expect("route to loopback peer");
    assert_eq!(advertised.ip(), peer);
    assert_eq!(advertised.port(), listener.port());
    let binds = controller
        .settings
        .media_binds_on(TEST_BASE_PORT, advertised.ip())
        .expect("per-stream UDP binds");
    assert_eq!(binds.video.ip(), peer);
    assert_eq!(binds.control.ip(), peer);
    assert_eq!(binds.audio.ip(), peer);
    assert_eq!(
        controller.advertised_local_for_peer(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        Err("peer_family_mismatch")
    );
}

#[test]
fn loopback_family_listeners_accept_v4_and_v6_and_keep_each_stream_family_aligned() {
    assert_both_family_listeners_accept_v4_and_v6(false);
}

#[test]
#[ignore = "opens wildcard listeners; run explicitly when testing remote binding"]
fn wildcard_family_listeners_accept_v4_and_v6_and_keep_each_stream_family_aligned() {
    assert_both_family_listeners_accept_v4_and_v6(true);
}

fn assert_both_family_listeners_accept_v4_and_v6(allow_remote: bool) {
    let settings: Settings = serde_json::from_value(json!({
        "enabled": true,
        "allow_remote": allow_remote,
        "address_family": "both",
        "bind_address": "",
        "bind": "127.0.0.1:0"
    }))
    .unwrap();
    let controller = Controller::new(settings).expect("dual-family RTSP listeners");
    assert_eq!(controller.locals.len(), 2);
    assert_eq!(controller.locals[0].port(), controller.locals[1].port());

    for listener in &controller.locals {
        let peer_ip = if listener.is_ipv4() {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        } else {
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        };
        assert_eq!(listener.ip().is_loopback(), !allow_remote);
        let destination = SocketAddr::new(
            if listener.ip().is_unspecified() {
                peer_ip
            } else {
                listener.ip()
            },
            listener.port(),
        );
        assert!(TcpStream::connect(destination).is_ok());

        let advertised = controller
            .advertised_local_for_peer(peer_ip)
            .expect("family-matched local route");
        assert_eq!(advertised.ip(), peer_ip);
        assert_eq!(advertised.port(), listener.port());
        let media = controller
            .settings
            .media_binds_on(TEST_BASE_PORT, advertised.ip())
            .expect("peer-family UDP binds");
        assert_eq!(media.video.ip(), peer_ip);
        assert_eq!(media.control.ip(), peer_ip);
        assert_eq!(media.audio.ip(), peer_ip);
    }
}

#[test]
fn status_reports_all_bound_families_and_keeps_legacy_local_field() {
    let settings: Settings = serde_json::from_value(json!({
        "enabled": true,
        "address_family": "both",
        "bind_address": "",
        "bind": "127.0.0.1:0"
    }))
    .unwrap();
    let controller = Controller::new(settings).expect("dual-family RTSP listeners");
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);

    controller.status(99, "caller", None, &runtime);

    let (_, response) = endpoint.completions.pop().expect("status completion");
    assert_eq!(response["local"], json!(controller.local));
    assert_eq!(response["locals"], json!(controller.locals));
    assert_eq!(response["locals"].as_array().unwrap().len(), 2);
}

pub(super) fn free_base_port() -> u16 {
    // On Windows, an ephemeral TCP port can sit in an excluded UDP range.
    // Probe the actual RTSP and media ports before returning a base port.
    let start = u32::from(NEXT_TEST_BASE_PORT.fetch_add(37, Ordering::Relaxed))
        + std::process::id() % 18_000;
    for offset in 0..1_000_u32 {
        let base = 22_000 + ((start + offset * 37) % 18_000) as u16;
        let rtsp_port = base + 21;
        let Ok(_tcp_v4) = TcpListener::bind((Ipv4Addr::LOCALHOST, rtsp_port)) else {
            continue;
        };
        let Ok(_tcp_v6) = TcpListener::bind((Ipv6Addr::LOCALHOST, rtsp_port)) else {
            continue;
        };
        let udp_v4 = (9..=14)
            .map(|offset| UdpSocket::bind((Ipv4Addr::LOCALHOST, base + offset)))
            .collect::<std::io::Result<Vec<_>>>();
        let Ok(_udp_v4) = udp_v4 else {
            continue;
        };
        let udp_v6 = (9..=14)
            .map(|offset| UdpSocket::bind((Ipv6Addr::LOCALHOST, base + offset)))
            .collect::<std::io::Result<Vec<_>>>();
        if udp_v6.is_ok() {
            return base;
        }
    }
    panic!("no free RTSP test port range");
}

#[test]
fn network_port_updates_rebind_rtsp_and_defer_until_the_stream_is_idle() {
    let _guard = NETWORK_TEST_LOCK.lock().unwrap();
    let settings = Settings {
        enabled: true,
        bind: "127.0.0.1:0".parse().unwrap(),
        ..Settings::default()
    };
    let mut controller = Controller::new(settings).expect("enabled RTSP listener");
    let old_port = free_base_port();
    let mut port = free_base_port();
    while port == old_port {
        port = free_base_port();
    }
    let old_status = controller
        .configure_network_port(old_port, 1)
        .expect("initial base port applied");
    assert_eq!(old_status["active"], true);
    let old_local = controller.local.expect("bound listener");

    controller.records.insert(5, record(State::Playing));
    let pending = controller
        .configure_network_port(port, 10)
        .expect("configuration accepted");
    assert_eq!(pending["ok"], true);
    assert_eq!(pending["active"], false);
    assert_eq!(pending["pending"], true);
    assert_eq!(pending["udp_binds_preflighted"], false);
    assert_eq!(pending["active_base_port"], old_port);
    assert_eq!(controller.local, Some(old_local));
    let deferred_binds = controller
        .media_binds_for_open(IpAddr::V4(Ipv4Addr::LOCALHOST))
        .unwrap();
    assert_eq!(deferred_binds.video.port(), old_port + 9);
    assert_eq!(deferred_binds.control.port(), old_port + 10);
    assert_eq!(deferred_binds.audio.port(), old_port + 11);

    controller.records.get_mut(&5).unwrap().state = State::Closed;
    controller.reconcile_network_port(11);
    let active = controller.local.expect("rebound listener");
    assert_eq!(active.ip(), old_local.ip(), "preserve configured bind IP");
    assert_eq!(
        active.port(),
        port + 21,
        "port={port}, network_error={:?}",
        controller.network_error
    );
    assert_eq!(controller.active_network_base_port, Some(port));
    let applied_binds = controller
        .media_binds_for_open(IpAddr::V4(Ipv4Addr::LOCALHOST))
        .unwrap();
    assert_eq!(applied_binds.video.port(), port + 9);
    assert_eq!(applied_binds.control.port(), port + 10);
    assert_eq!(applied_binds.audio.port(), port + 11);

    let binds = controller.settings.media_binds(port).unwrap();
    assert_eq!(binds.video, SocketAddr::new(active.ip(), port + 9));
    assert_eq!(binds.control, SocketAddr::new(active.ip(), port + 10));
    assert_eq!(binds.audio, SocketAddr::new(active.ip(), port + 11));
}

#[test]
fn network_port_update_rebinds_both_family_listeners_and_udp_policy() {
    let _guard = NETWORK_TEST_LOCK.lock().unwrap();
    let settings: Settings = serde_json::from_value(json!({
        "enabled": true,
        "address_family": "both",
        "bind_address": "",
        "bind": "127.0.0.1:0"
    }))
    .unwrap();
    let mut controller = Controller::new(settings).expect("dual-family bootstrap listener");
    let base_port = free_base_port();

    let response = controller
        .configure_network_port(base_port, 1)
        .expect("dual-family network port applies");

    assert_eq!(response["active"], true);
    assert_eq!(controller.locals.len(), 2);
    assert_eq!(controller.locals[0].ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(controller.locals[1].ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_eq!(controller.locals[0].port(), base_port + 21);
    assert_eq!(controller.locals[1].port(), base_port + 21);
    for peer in [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ] {
        let binds = controller
            .media_binds_for_open(peer)
            .expect("family-matched UDP policy");
        assert_eq!(binds.video.ip(), peer);
        assert_eq!(binds.video.port(), base_port + 9);
        assert_eq!(binds.control.ip(), peer);
        assert_eq!(binds.control.port(), base_port + 10);
        assert_eq!(binds.audio.ip(), peer);
        assert_eq!(binds.audio.port(), base_port + 11);
    }
}

#[test]
fn network_policy_update_activates_requested_family_and_empty_bind_address() {
    let _guard = NETWORK_TEST_LOCK.lock().unwrap();
    let settings = Settings {
        enabled: true,
        bind: "127.0.0.1:0".parse().unwrap(),
        ..Settings::default()
    };
    let mut controller = Controller::new(settings).expect("initial loopback listener");
    let port = free_base_port();

    let status = controller
        .configure_network_policy(
            port,
            Some(crate::model::AddressFamily::Both),
            Some(String::new()),
            0,
        )
        .unwrap();

    assert_eq!(status["active"], true);
    assert_eq!(status["udp_binds_preflighted"], true);
    assert_eq!(status["active_address_family"], "both");
    assert_eq!(status["active_bind_address"], "");
    assert_eq!(controller.locals.len(), 2);
    assert!(controller.locals.iter().any(|address| address.is_ipv4()));
    assert!(controller.locals.iter().any(|address| address.is_ipv6()));
    assert_eq!(
        controller
            .media_binds_for_open(IpAddr::V6(Ipv6Addr::LOCALHOST))
            .unwrap()
            .video
            .ip(),
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    );
}

#[test]
fn network_policy_update_defers_family_and_bind_address_until_stream_is_terminal() {
    let _guard = NETWORK_TEST_LOCK.lock().unwrap();
    let port = free_base_port();
    let settings = Settings {
        enabled: true,
        bind: "127.0.0.1:0".parse().unwrap(),
        address_family: Some(crate::model::AddressFamily::Ipv4),
        bind_address: Some("127.0.0.1".into()),
        ..Settings::default()
    };
    let mut controller = Controller::new(settings).expect("IPv4 loopback listener");
    let initial = controller
        .configure_network_policy(
            port,
            Some(crate::model::AddressFamily::Ipv4),
            Some("127.0.0.1".into()),
            1,
        )
        .expect("initial IPv4 policy applies");
    assert_eq!(initial["active"], true);
    assert_eq!(initial["active_address_family"], "ipv4");
    assert_eq!(initial["active_bind_address"], "127.0.0.1");

    let old_listener = controller.local.expect("bound IPv4 listener");
    assert_eq!(old_listener.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(old_listener.port(), port + 21);
    assert!(TcpStream::connect(old_listener).is_ok());

    controller.records.insert(5, record(State::Playing));
    let pending = controller
        .configure_network_policy(
            port,
            Some(crate::model::AddressFamily::Both),
            Some(String::new()),
            2,
        )
        .expect("desired policy is accepted");

    assert_eq!(pending["ok"], true);
    assert_eq!(pending["active"], false);
    assert_eq!(pending["pending"], true);
    assert_eq!(pending["udp_binds_preflighted"], false);
    assert_eq!(pending["desired_port"], port);
    assert_eq!(pending["desired_address_family"], "both");
    assert_eq!(pending["desired_bind_address"], "");
    assert_eq!(pending["active_base_port"], port);
    assert_eq!(pending["active_address_family"], "ipv4");
    assert_eq!(pending["active_bind_address"], "127.0.0.1");
    assert_eq!(pending["rtsp_addresses"], json!([old_listener.to_string()]));
    assert_eq!(controller.local, Some(old_listener));
    assert_eq!(controller.locals, vec![old_listener]);
    assert!(TcpStream::connect(old_listener).is_ok());

    let old_media_binds = controller
        .media_binds_for_open(IpAddr::V4(Ipv4Addr::LOCALHOST))
        .expect("old IPv4 media binds remain active");
    assert_eq!(
        old_media_binds.video,
        SocketAddr::new(old_listener.ip(), port + 9)
    );
    assert_eq!(
        old_media_binds.control,
        SocketAddr::new(old_listener.ip(), port + 10)
    );
    assert_eq!(
        old_media_binds.audio,
        SocketAddr::new(old_listener.ip(), port + 11)
    );

    controller.records.get_mut(&5).unwrap().state = State::Closed;
    controller.reconcile_network_port(3);

    assert_eq!(controller.locals.len(), 2);
    let v4_listener = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port + 21);
    let v6_listener = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port + 21);
    assert!(controller.locals.contains(&v4_listener));
    assert!(controller.locals.contains(&v6_listener));
    assert!(TcpStream::connect(v4_listener).is_ok());
    assert!(TcpStream::connect(v6_listener).is_ok());
    assert_eq!(controller.active_network_base_port, Some(port));
    assert_eq!(
        controller.active_address_family,
        Some(crate::model::AddressFamily::Both)
    );
    assert_eq!(controller.active_bind_address.as_deref(), Some(""));

    for peer in [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ] {
        let media_binds = controller
            .media_binds_for_open(peer)
            .expect("family-matched media binds after reconciliation");
        assert_eq!(media_binds.video, SocketAddr::new(peer, port + 9));
        assert_eq!(media_binds.control, SocketAddr::new(peer, port + 10));
        assert_eq!(media_binds.audio, SocketAddr::new(peer, port + 11));
    }
}

#[test]
fn null_network_overrides_confirm_rtsp_startup_policy_without_fabricating_overrides() {
    let _guard = NETWORK_TEST_LOCK.lock().unwrap();
    let base = free_base_port();
    let settings = Settings {
        enabled: true,
        bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), base + 21),
        address_family: Some(crate::model::AddressFamily::Ipv4),
        bind_address: Some(String::new()),
        ..Settings::default()
    };
    let mut controller = Controller::new(settings).expect("startup policy listener");

    let status = controller
        .configure_network_policy(base, None, None, 0)
        .unwrap();

    assert_eq!(status["active"], true);
    assert_eq!(status["active_address_family"], Value::Null);
    assert_eq!(status["active_bind_address"], Value::Null);
    assert_eq!(status["rtsp_addresses"].as_array().unwrap().len(), 1);
    assert_eq!(controller.locals[0].ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
}

#[test]
fn failed_startup_rebind_keeps_media_binds_aligned_with_the_bootstrap_listener() {
    let _guard = NETWORK_TEST_LOCK.lock().unwrap();
    let old_port = free_base_port();
    let mut desired_port = free_base_port();
    while desired_port == old_port {
        desired_port = free_base_port();
    }
    let settings = Settings {
        enabled: true,
        bind: SocketAddr::from(([127, 0, 0, 1], old_port + 21)),
        ..Settings::default()
    };
    let mut controller = Controller::new(settings).expect("bootstrap listener binds");
    assert_eq!(controller.local.unwrap().port(), old_port + 21);
    assert_eq!(controller.active_network_base_port, None);

    let _occupied_video_port =
        UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], desired_port + 9)))
            .expect("reserve desired video UDP port");
    let pending = controller
        .configure_network_port(desired_port, 0)
        .expect("desired base port is persisted for reconciliation");
    assert_eq!(pending["active"], false);
    assert_eq!(pending["error"], "udp_bind_failed");
    assert_eq!(controller.local.unwrap().port(), old_port + 21);

    let binds = controller
        .media_binds_for_open(IpAddr::V4(Ipv4Addr::LOCALHOST))
        .expect("bootstrap listener has its own base port");
    assert_eq!(binds.video.port(), old_port + 9);
    assert_eq!(binds.control.port(), old_port + 10);
    assert_eq!(binds.audio.port(), old_port + 11);
    assert_ne!(binds.video.port(), desired_port + 9);
}

#[test]
fn network_configuration_is_restricted_to_nvhttp_and_reports_disabled_listener_state() {
    let _guard = NETWORK_TEST_LOCK.lock().unwrap();
    let settings = Settings {
        enabled: true,
        bind: "127.0.0.1:0".parse().unwrap(),
        ..Settings::default()
    };
    let mut controller = Controller::new(settings).expect("listener");
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let port = free_base_port();

    controller.receive(
        10,
        "amaterasu.management".into(),
        Operation::NetworkConfigure {
            port,
            address_family: None,
            bind_address: None,
        },
        0,
        5_000,
        &runtime,
    );
    assert_eq!(endpoint.completions[0].1["code"], "permission_denied");
    assert_eq!(controller.network_base_port, None);

    controller.receive(
        11,
        "amaterasu.nvhttp".into(),
        Operation::NetworkConfigure {
            port,
            address_family: None,
            bind_address: None,
        },
        0,
        5_000,
        &runtime,
    );
    assert_eq!(endpoint.completions[1].1["ok"], true);
    assert_eq!(endpoint.completions[1].1["active"], true);
    assert_eq!(endpoint.completions[1].1["active_base_port"], port);
}

#[test]
fn network_port_update_rejects_occupied_tcp_and_udp_targets_then_retries() {
    let _guard = NETWORK_TEST_LOCK.lock().unwrap();
    for occupied_udp_offset in [None, Some(9), Some(10), Some(11)] {
        let settings = Settings {
            enabled: true,
            bind: "127.0.0.1:0".parse().unwrap(),
            ..Settings::default()
        };
        let mut controller = Controller::new(settings).expect("enabled RTSP listener");
        let old_local = controller.local.expect("bound listener");
        let port = free_base_port();
        let tcp_reservation = if occupied_udp_offset.is_none() {
            Some(
                TcpListener::bind(SocketAddr::new(old_local.ip(), port + 21))
                    .expect("reserve target TCP port"),
            )
        } else {
            None
        };
        let udp_reservation = occupied_udp_offset
            .map(|offset| UdpSocket::bind(SocketAddr::new(old_local.ip(), port + offset)).unwrap());

        let pending = controller
            .configure_network_port(port, 0)
            .expect("desired port is accepted");
        assert_eq!(pending["active"], false);
        assert_eq!(
            pending["error"],
            if occupied_udp_offset.is_none() {
                "tcp_bind_failed"
            } else {
                "udp_bind_failed"
            }
        );
        assert_eq!(controller.local, Some(old_local));

        drop(tcp_reservation);
        drop(udp_reservation);
        controller.reconcile_network_port(1_000);
        assert_eq!(controller.local.unwrap().port(), port + 21);
        assert_eq!(controller.active_network_base_port, Some(port));
    }
}

#[test]
fn accepted_rtsp_connections_are_kept_on_the_old_listener_until_it_is_idle() {
    let _guard = NETWORK_TEST_LOCK.lock().unwrap();
    let settings = Settings {
        enabled: true,
        bind: "127.0.0.1:0".parse().unwrap(),
        ..Settings::default()
    };
    let mut controller = Controller::new(settings).expect("enabled RTSP listener");
    let old_local = controller.local.expect("bound listener");
    controller
        .transport
        .as_mut()
        .unwrap()
        .add_ticket(23, old_local.ip(), &[7; 16])
        .unwrap();
    let connection = TcpStream::connect(old_local).unwrap();
    controller.transport.as_mut().unwrap().poll(1);
    assert_eq!(controller.transport.as_ref().unwrap().connection_count(), 1);

    let port = free_base_port();
    let pending = controller
        .configure_network_port(port, 2)
        .expect("configuration accepted");
    assert_eq!(pending["active"], false);
    assert_eq!(controller.local, Some(old_local));

    drop(connection);
    controller.transport.as_mut().unwrap().poll(3);
    assert_eq!(controller.transport.as_ref().unwrap().connection_count(), 0);
    controller.reconcile_network_port(4);
    assert_eq!(controller.local.unwrap().port(), port + 21);
}

fn request(method: Method, target: &str, cseq: u32, session: bool) -> Request {
    let mut headers = Vec::new();
    if session {
        headers.push(("session".into(), "00112233445566778899aabbccddeeff".into()));
    }
    Request {
        method,
        target: target.into(),
        cseq,
        headers,
        body: Vec::new(),
    }
}

fn pending_play(record: &Record) -> (Negotiation, u64) {
    let mut negotiation = Negotiation::new(
        TrustedLaunch {
            epoch: record.id,
            rtsp_target: record.url.clone(),
            session_token: record.session_token.clone(),
            encrypted_rtsp: true,
            audio_port: 49_000,
            video_port: 49_002,
            control_port: 49_004,
            control_connect_data: record.config.connect_data,
            udp_discovery: None,
        },
        record.capabilities(),
    )
    .expect("valid negotiation");
    let target = record.url.as_str();
    negotiation
        .handle(record.id, &request(Method::Options, target, 1, false))
        .expect("options");
    let mut describe = request(Method::Describe, target, 2, false);
    describe
        .headers
        .push(("accept".into(), "application/sdp".into()));
    negotiation.handle(record.id, &describe).expect("describe");
    for (cseq, target) in [
        (3, "streamid=audio/0/0"),
        (4, "streamid=video/0/0"),
        (5, "streamid=control/13/0"),
    ] {
        let mut setup = request(Method::Setup, target, cseq, cseq > 3);
        setup.headers.push((
            "transport".into(),
            "unicast;X-GS-ClientPort=50000-50001".into(),
        ));
        negotiation.handle(record.id, &setup).expect("setup");
    }
    let mut announce = request(Method::Announce, "streamid=control/13/0", 6, true);
    announce
        .headers
        .push(("content-type".into(), "application/sdp".into()));
    announce.body =
        include_str!("../../../../crates/gamestream/src/sdp/fixtures/moonlight-h264.sdp")
            .replace(
                "x-ss-general.encryptionEnabled:0",
                "x-ss-general.encryptionEnabled:1",
            )
            .into_bytes();
    let NegotiationStep::Action { ticket, .. } =
        negotiation.handle(record.id, &announce).expect("announce")
    else {
        panic!("announce action");
    };
    negotiation
        .complete(record.id, ticket, true)
        .expect("announce accepted");
    let NegotiationStep::Action { ticket, .. } = negotiation
        .handle(record.id, &request(Method::Play, "/", 7, true))
        .expect("play")
    else {
        panic!("play action");
    };
    (negotiation, ticket)
}

fn announced_profile_record() -> (Record, Announce) {
    let announcement =
        include_str!("../../../../crates/gamestream/src/sdp/fixtures/moonlight-h264.sdp").replace(
            "x-ss-general.encryptionEnabled:0",
            "x-ss-general.encryptionEnabled:7",
        );
    let announce = parse_announce(announcement.as_bytes()).expect("valid encrypted ANNOUNCE");
    let mut record = record(State::Ready);
    let profile = &mut record.config.profile;
    profile.width = announce.video.width;
    profile.height = announce.video.height;
    profile.fps = announce.video.fps;
    profile.bitrate_kbps = announce.video.bitrate_kbps;
    profile.max_reference_frames = announce.video.max_reference_frames;
    profile.packet_size = announce.packet_size as usize;
    profile.packet_ms = announce.audio.packet_duration_ms;
    profile.min_parity_shards = announce.fec.minimum_packets as u8;
    profile.encrypt_video = announce.encryption.video;
    profile.encrypt_audio = announce.encryption.audio;
    if let Some(repair_percent) = announce.fec.repair_percent {
        profile.fec_percentage = repair_percent as u8;
    }
    (record, announce)
}

#[test]
fn accepted_announce_encryption_flows_into_authenticated_play_start() {
    let (mut record, announce) = announced_profile_record();
    assert!(record.accept_announce(&announce));

    let payload = record
        .authenticated_start_payload(23)
        .expect("accepted ANNOUNCE enables authenticated start");

    assert_eq!(payload["action"], "start_when_authenticated");
    assert_eq!(payload["session"], 23);
    assert_eq!(
        payload["configuration"]["encrypt_video"],
        announce.encryption.video
    );
    assert_eq!(
        payload["configuration"]["encrypt_audio"],
        announce.encryption.audio
    );
    assert_eq!(
        payload["configuration"]["client_feature_flags"],
        announce.client_feature_flags
    );
    assert_eq!(
        payload["configuration"]["configuration"]["slices_per_frame"],
        announce.video.slices_per_frame
    );
    assert!(
        record.config.arm(23)["configuration"]
            .get("encrypt_video")
            .is_none()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn accepted_color_survives_authenticated_start() {
    let (mut record, mut announce) = announced_profile_record();
    announce.video.color = amaterasu_gamestream::sdp::ColorMode::Bt601Full;
    assert!(record.accept_announce(&announce));
    let payload = record
        .authenticated_start_payload(23)
        .expect("accepted color");
    assert_eq!(
        payload["configuration"]["configuration"]["color_mode"],
        "bt601_full"
    );
    assert!(
        record.config.arm(23)["configuration"]["configuration"]
            .get("color_mode")
            .is_none()
    );
}

#[test]
fn negotiated_video_is_owned_by_its_rtsp_ticket() {
    let (mut low_ticket, mut low_announce) = announced_profile_record();
    low_ticket.id = 5;
    low_ticket.session = Some(23);
    low_ticket.config.profile.width = 1280;
    low_ticket.config.profile.height = 720;
    low_ticket.config.profile.fps = 30;
    low_ticket.config.max_bitrate_kbps = Some(8_000);
    low_ticket.config.negotiation_bounds = Some(crate::model::NegotiationBounds {
        max_width: 1280,
        max_height: 720,
        max_fps: 30,
        max_bitrate_kbps: 8_000,
        max_reference_frames: 1,
    });
    low_announce.video.width = 1280;
    low_announce.video.height = 720;
    low_announce.video.fps = 30;
    low_announce.video.bitrate_kbps = 8_000;
    low_announce.video.max_reference_frames = 0;

    let (mut high_ticket, high_announce) = announced_profile_record();
    high_ticket.id = 6;
    high_ticket.session = Some(24);
    high_ticket.config.negotiation_bounds = Some(crate::model::NegotiationBounds {
        max_width: 2560,
        max_height: 1440,
        max_fps: 120,
        max_bitrate_kbps: 20_000,
        max_reference_frames: 1,
    });

    assert!(low_ticket.accept_announce(&low_announce));
    assert!(high_ticket.accept_announce(&high_announce));
    let low_start = low_ticket.authenticated_start_payload(23).unwrap();
    let high_start = high_ticket.authenticated_start_payload(24).unwrap();
    let expected_video = json!({"width":1280,"height":720,"fps":30,"bitrate":8_000_000,"max_reference_frames":0,"color_mode":"bt709_limited", "slices_per_frame": 1, "video_stream_format": low_announce.video.format});
    for (field, expected) in expected_video.as_object().unwrap() {
        assert_eq!(
            &low_start["configuration"]["configuration"][field], expected,
            "{field}"
        );
    }
    assert_eq!(high_start["configuration"]["configuration"]["width"], 1920);
    assert_eq!(high_start["configuration"]["configuration"]["height"], 1080);
    assert_eq!(high_start["configuration"]["configuration"]["fps"], 60);

    assert!(!low_ticket.accept_announce(&high_announce));
    assert!(low_ticket.authenticated_start_payload(23).is_none());
    assert!(high_ticket.authenticated_start_payload(24).is_some());
}

#[test]
fn authenticated_start_keeps_the_exact_ticket_codec() {
    let (mut record, mut announce) = announced_profile_record();
    record.config.profile.codec = amaterasu_media::VideoCodec::Hevc;
    announce.video.format.codec = amaterasu_gamestream::sdp::VideoCodec::Hevc;
    announce.video.color = amaterasu_gamestream::sdp::ColorMode::Bt709Limited;
    assert!(record.accept_announce(&announce));
    let start = record.authenticated_start_payload(23).unwrap();
    assert_eq!(start["configuration"]["configuration"]["codec"], "hevc");
    assert_eq!(
        record.config.prepare("hevc-prepare", None)["video"]["codec"],
        "hevc"
    );
}

#[test]
fn codec_offer_drives_describe_markers_announce_and_start_codec() {
    let (mut record, mut announce) = announced_profile_record();
    let only_h264 = record.capabilities().transport;
    assert!(!only_h264.hevc && !only_h264.av1);

    record.config.profile.codecs = vec![
        amaterasu_media::VideoCodec::H264,
        amaterasu_media::VideoCodec::Av1,
    ];
    let offered = record.capabilities().transport;
    assert!(offered.av1 && !offered.hevc);
    let prepared = record.config.prepare("offer-prepare", None);
    assert_eq!(prepared["video"]["codecs"], json!(["h264", "av1"]));
    assert!(prepared["video"].get("codec").is_none());

    announce.video.color = amaterasu_gamestream::sdp::ColorMode::Bt709Limited;
    announce.video.format.codec = amaterasu_gamestream::sdp::VideoCodec::Hevc;
    assert!(!record.accept_announce(&announce), "HEVC was not offered");
    announce.video.format.codec = amaterasu_gamestream::sdp::VideoCodec::Av1;
    assert!(record.accept_announce(&announce));
    let start = record.authenticated_start_payload(23).unwrap();
    assert_eq!(start["configuration"]["configuration"]["codec"], "av1");

    announce.video.format.codec = amaterasu_gamestream::sdp::VideoCodec::H264;
    assert!(record.accept_announce(&announce));
    let start = record.authenticated_start_payload(24).unwrap();
    assert!(
        start["configuration"]["configuration"]
            .get("codec")
            .is_none()
    );
}

#[test]
fn codec_offers_must_name_each_codec_once_and_replace_the_single_codec() {
    let (mut record, _) = announced_profile_record();
    record.config.profile.codecs = vec![
        amaterasu_media::VideoCodec::Hevc,
        amaterasu_media::VideoCodec::Av1,
    ];
    assert!(record.config.validate(false).is_ok());
    record
        .config
        .profile
        .codecs
        .push(amaterasu_media::VideoCodec::Av1);
    assert_eq!(record.config.validate(false), Err("invalid_profile"));
    record.config.profile.codecs = vec![amaterasu_media::VideoCodec::Hevc];
    record.config.profile.codec = amaterasu_media::VideoCodec::Hevc;
    assert_eq!(record.config.validate(false), Err("invalid_profile"));
}

#[test]
fn accepted_announce_freezes_pcm_haptics_flag_into_authenticated_start() {
    let (mut record, mut announce) = announced_profile_record();
    announce.client_feature_flags |= amaterasu_gamestream::sdp::CLIENT_FEATURE_HAPTICS_PCM;
    assert!(record.accept_announce(&announce));
    let payload = record.authenticated_start_payload(23).unwrap();
    assert_eq!(payload["configuration"]["client_feature_flags"], 7);
    assert_eq!(
        payload["configuration"]["configuration"]["video_stream_format"],
        serde_json::to_value(announce.video.format).unwrap()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn ten_bit_depth_and_launch_dynamic_range_are_independent() {
    for hdr in [false, true] {
        let (mut record, mut announce) = announced_profile_record();
        record.config.profile.codecs = vec![amaterasu_media::VideoCodec::Hevc];
        record.config.profile.hdr = hdr;
        announce.video.format = amaterasu_module_sdk::VideoStreamFormat {
            codec: amaterasu_module_sdk::VideoCodec::Hevc,
            bit_depth: 10,
            dynamic_range: amaterasu_module_sdk::DynamicRange::Hdr,
        };
        assert!(record.accept_announce(&announce));
        let payload = record.authenticated_start_payload(23).unwrap();
        let format = &payload["configuration"]["configuration"]["video_stream_format"];
        assert_eq!(format["bit_depth"], 10);
        assert_eq!(format["dynamic_range"], if hdr { "hdr" } else { "sdr" });
        announce.video.format.bit_depth = 8;
        announce.video.format.dynamic_range = amaterasu_module_sdk::DynamicRange::Sdr;
        assert_eq!(record.accept_announce(&announce), !hdr);
    }
}

#[test]
fn unsupported_negotiated_codec_cannot_arm_h264_start_configuration() {
    let (mut record, mut announce) = announced_profile_record();
    announce.video.format = amaterasu_module_sdk::VideoStreamFormat {
        codec: amaterasu_module_sdk::VideoCodec::Hevc,
        bit_depth: 8,
        dynamic_range: amaterasu_module_sdk::DynamicRange::Sdr,
    };

    assert!(!record.accept_announce(&announce));
    assert!(record.negotiated_video_stream_format.is_none());
    assert!(record.authenticated_start_payload(23).is_none());
}

#[test]
fn rejected_announce_cannot_produce_an_authenticated_play_start() {
    let (mut record, announce) = announced_profile_record();
    record.config.profile.encrypt_audio = false;
    record.negotiated_encryption = Some(crate::model::EncryptionSnapshot {
        video: true,
        audio: true,
    });

    assert!(!record.accept_announce(&announce));
    assert!(record.negotiated_encryption.is_none());
    assert!(record.client_feature_flags.is_none());
    assert!(record.negotiated_color.is_none());
    assert!(record.negotiated_video.is_none());
    assert!(record.authenticated_start_payload(23).is_none());
}

#[test]
fn unavailable_play_response_never_marks_the_ticket_playing() {
    let mut controller = Controller::new(Settings::default()).expect("disabled controller");
    let mut record = record(State::Arming);
    record.config.profile.bitrate_kbps = 20_000;
    record.config.profile.packet_size = 1_392;
    record.config.profile.packet_ms = 5;
    record.config.profile.encrypt_video = false;
    record.config.profile.encrypt_audio = false;
    let (negotiation, action_ticket) = pending_play(&record);
    record.negotiation = Some(negotiation);
    let job = Job {
        ticket: record.id,
        phase: Phase::Arm,
        call: Call::new(
            json!({"action":"start_when_authenticated"}),
            "arm".into(),
            None,
        ),
        caller: None,
        network: Some((999, action_ticket)),
        deadline: 5_000,
    };

    controller.armed(
        &mut record,
        job,
        &json!({"ok":true,"session":23,"state":"armed","cleanup_error":null}),
        10,
    );

    assert!(record.state == State::Closing);
    assert_eq!(record.failure, Some("start_rejected"));
}

#[test]
fn late_prepare_and_arm_results_do_not_revive_a_closing_ticket() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut endpoint = Endpoint {
        active_request: true,
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    let mut record = record(State::Closing);
    record.session = None;
    record.needs_lookup = true;
    let prepare = Job {
        ticket: record.id,
        phase: Phase::Prepare,
        call: Call::new(json!({"action":"prepare"}), "prepare".into(), None),
        caller: Some(19),
        network: None,
        deadline: 5_000,
    };
    controller.prepared(
        &mut record,
        prepare,
        &json!({"ok":true,"state":"prepared","session":23}),
        10,
        &runtime,
    );
    assert!(record.state == State::Closing);
    assert_eq!(record.session, Some(23));
    assert!(record.negotiation.is_none());
    assert!(
        controller
            .job
            .as_ref()
            .is_some_and(|job| job.phase == Phase::Stop)
    );
    assert!(endpoint.completions.is_empty());

    controller.job = None;
    let arm = Job {
        ticket: record.id,
        phase: Phase::Arm,
        call: Call::new(
            json!({"action":"start_when_authenticated"}),
            "arm".into(),
            None,
        ),
        caller: None,
        network: Some((999, 1)),
        deadline: 5_000,
    };
    controller.armed(
        &mut record,
        arm,
        &json!({"ok":true,"state":"active","session":23}),
        11,
    );
    assert!(record.state == State::Closing);
    assert!(
        controller
            .job
            .as_ref()
            .is_some_and(|job| job.phase == Phase::Stop)
    );
}

#[test]
fn lifecycle_gate_waits_for_authority_and_passes_matching_candidate_to_stream_session() {
    let (mut record, announce) = announced_profile_record();
    assert!(record.accept_announce(&announce));
    record.config.lifecycle_session_target = Some(
        serde_json::from_value(
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1}),
        )
        .unwrap(),
    );
    record.config.lifecycle_settings_revision = Some("7".into());
    record.config.lifecycle_intent = Some("initial".into());
    record.config.lifecycle_client_context = Some(serde_json::from_value(json!({
        "client_ref":"client-a", "display_mode":{"width":1920,"height":1080,"refresh_millihz":59940}
    })).unwrap());
    record.lifecycle_input_required = Some(false);
    let mut controller = Controller::new(Settings::default()).unwrap();
    let start = record.authenticated_start_payload(23).unwrap();
    controller.schedule_arm(&mut record, start, 8, 9, 0);
    assert!(record.lifecycle_prepare_outstanding);
    controller.records.insert(record.id, record);
    let operation = "rtsp-0000000000000005-lifecycle-prepare";
    let mut endpoint = Endpoint {
        verifier_reply: Some(json!({"status":"pending","operation_id":operation})),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    controller.advance_job(&runtime, 1);
    controller.advance_job(&runtime, 2);
    assert!(endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { service, payload, .. }
        if service == "amaterasu.session-lifecycle/1"
            && payload["operation"] == "prepare_stream"
            && payload["client_context"]["client_ref"] == "client-a"
            && payload["client_context"]["display_mode"]["refresh_millihz"] == 59940
    )));
    assert!(
        controller
            .job
            .as_ref()
            .is_some_and(|job| job.phase == Phase::LifecycleLookup)
    );
    assert!(!endpoint.commands.iter().any(|command| matches!(command, Command::Request { service, payload, .. } if service == "amaterasu.stream-session/1" && payload["action"] == "start_when_authenticated")));
    endpoint.verifier_reply = Some(
        json!({"status":"succeeded","operation_id":operation,"transition_id":"transition",
        "target":{"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1},
        "candidate":{"capture_provider":"amaterasu.capture-pipewire/1","selector_schema":"amaterasu.kwin-wl-output/1","selector":{"output":"kwin:Virtual-12"},
            "resource":{"provider":"test.provider","resource_id":"output","incarnation":"one","revision":1,"execution_domain":"desktop","mutation_domain":null,"mutation_domains":["desktop"]},
            "mode":{"width":announce.video.width,"height":announce.video.height,"refresh_millihz":announce.video.fps * 1000,"hdr":false},"plan_revision":1,"geometry_revision":1,"generation":1,"fallback":"forbidden"}}),
    );
    controller.advance_job(&runtime, 3);
    controller.advance_job(&runtime, 4);
    assert!(
        controller
            .job
            .as_ref()
            .is_some_and(|job| job.phase == Phase::Arm)
    );
    controller.advance_job(&runtime, 5);
    let start = endpoint
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.stream-session/1"
                && payload["action"] == "start_when_authenticated" =>
            {
                Some(payload)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        start["configuration"]["lifecycle_target"]["stream_id"],
        "stream"
    );
    assert_eq!(
        start["configuration"]["lifecycle_candidate"]["plan_revision"],
        1
    );
    let record = &controller.records[&5];
    assert!(!record.lifecycle_prepare_outstanding);
    assert!(!clean_terminal(
        record,
        &json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null})
    ));
    assert!(clean_terminal(
        record,
        &json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null,
        "lifecycle_retired":true,"lifecycle_target":record.lifecycle_target})
    ));
}

#[test]
fn unknown_lifecycle_prepare_cannot_be_reported_clean() {
    let mut record = record(State::Closing);
    record.lifecycle_prepare_outstanding = true;
    assert!(!clean_terminal(
        &record,
        &json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null})
    ));
}

#[test]
fn joined_transport_preserves_its_intent_and_exact_lifecycle_session_target() {
    let (mut record, announce) = announced_profile_record();
    assert!(record.accept_announce(&announce));
    let target =
        json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
    record.config.lifecycle_session_target = serde_json::from_value(target.clone()).ok();
    record.config.lifecycle_settings_revision = Some("7".into());
    record.config.lifecycle_intent = Some("join".into());
    record.lifecycle_input_required = Some(false);
    assert_eq!(record.response()["lifecycle_intent"], "join");
    let mut controller = Controller::new(Settings::default()).unwrap();
    let start = record.authenticated_start_payload(23).unwrap();
    controller.schedule_arm(&mut record, start, 8, 9, 0);
    controller.records.insert(record.id, record);
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    controller.advance_job(&runtime, 1);
    assert!(endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { service, payload, .. }
            if service == "amaterasu.session-lifecycle/1"
            && payload["operation"] == "prepare_stream"
            && payload["intent"] == "join"
            && payload["target"] == target
    )));
}

#[test]
fn joined_transport_freezes_and_forwards_the_coordinator_source_before_start() {
    for source in [
        Some(
            json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"source","generation":3}),
        ),
        None,
    ] {
        let (mut record, announce) = announced_profile_record();
        assert!(record.accept_announce(&announce));
        record.config.lifecycle_session_target = serde_json::from_value(
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1}),
        )
        .ok();
        record.config.lifecycle_settings_revision = Some("7".into());
        record.config.lifecycle_intent = Some("join".into());
        record.lifecycle_input_required = Some(false);
        let mut controller = Controller::new(Settings::default()).unwrap();
        let start = record.authenticated_start_payload(23).unwrap();
        controller.schedule_arm(&mut record, start, 8, 9, 0);
        controller.records.insert(record.id, record);
        let mut reply = json!({"status":"succeeded","operation_id":"rtsp-0000000000000005-lifecycle-prepare",
            "target":{"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"subscriber","generation":1},
            "candidate":{"capture_provider":"amaterasu.capture-pipewire/1","selector_schema":"amaterasu.kwin-wl-output/1","selector":{"output":"kwin:Virtual-12"},
                "resource":{"provider":"test.provider","resource_id":"output","incarnation":"one","revision":1,"execution_domain":"desktop","mutation_domain":null,"mutation_domains":["desktop"]},
                "mode":{"width":announce.video.width * 2,"height":announce.video.height * 2,"refresh_millihz":announce.video.fps * 2000,"hdr":false},
                "plan_revision":1,"geometry_revision":1,"generation":1,"fallback":"forbidden"}});
        if let Some(source) = &source {
            reply["shared_source"] = source.clone();
            let mut source_binding = reply["candidate"].clone();
            source_binding["generation"] = source["generation"].clone();
            reply["join_context"] = json!({"source":source,"source_transition":"source-prepare",
                "binding_owner":"test.provider","binding":source_binding});
        }
        let mut endpoint = Endpoint {
            verifier_reply: Some(reply),
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        for tick in 1..=3 {
            controller.advance_job(&runtime, tick);
        }
        let start = endpoint.commands.iter().find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.stream-session/1"
                && payload["action"] == "start_when_authenticated" =>
            {
                Some(payload)
            }
            _ => None,
        });
        if let Some(source) = source {
            let start = start.unwrap();
            assert_eq!(start["configuration"]["lifecycle_shared_source"], source);
            assert_eq!(
                start["configuration"]["configuration"]["width"],
                announce.video.width
            );
            assert_eq!(
                start["configuration"]["configuration"]["height"],
                announce.video.height
            );
            assert_eq!(
                start["configuration"]["configuration"]["fps"],
                announce.video.fps
            );
            assert_eq!(
                start["configuration"]["lifecycle_candidate"]["mode"]["width"],
                announce.video.width * 2
            );
            assert_eq!(
                json!(controller.records[&5].lifecycle_source_target),
                source
            );
            assert_eq!(controller.records[&5].state, State::Arming);
        } else {
            assert!(start.is_none());
            assert!(controller.records[&5].lifecycle_source_target.is_none());
            assert!(controller.records[&5].lifecycle_prepare_outstanding);
        }
    }
}

#[test]
fn shared_retention_completes_departing_transport_without_releasing_source_status() {
    let mut record = record(State::Closing);
    let source = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"source","generation":3});
    record.config.lifecycle_session_target = serde_json::from_value(
        json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1}),
    )
    .ok();
    record.lifecycle_target = Some(
        json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"subscriber","generation":1}),
    );
    record.lifecycle_source_target = serde_json::from_value(source.clone()).ok();
    let receipt = json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null,
        "lifecycle_target":record.lifecycle_target,"lifecycle_retired":false,
        "lifecycle_consumer_retired":true,"lifecycle_resources_released":false,
        "lifecycle_shared_retained":true,"lifecycle_shared_source":source});
    let mut controller = Controller::new(Settings::default()).unwrap();
    let job = Job {
        ticket: record.id,
        phase: Phase::Stop,
        call: Call::new(json!({"action":"stop","session":23}), "stop".into(), None),
        caller: None,
        network: None,
        deadline: 5_000,
    };
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    controller.stopped(&mut record, job, &receipt, &runtime);
    assert_eq!(record.state, State::Closed);
    assert!(record.cleanup_confirmed);
    assert_eq!(
        record.response()["lifecycle_cleanup_state"],
        "shared_retained"
    );
    assert_eq!(record.response()["lifecycle_retired"], false);
    assert_eq!(record.response()["lifecycle_resources_released"], false);
    assert_eq!(record.response()["lifecycle_shared_source"], source);
}

#[test]
fn lifecycle_gate_rejects_foreign_stream_or_stale_binding_before_capture_start() {
    for mismatch in [
        "epoch",
        "generation",
        "geometry",
        "plan",
        "width",
        "height",
        "rate",
        "hdr",
    ] {
        let (mut record, announce) = announced_profile_record();
        assert!(record.accept_announce(&announce));
        record.config.lifecycle_session_target = Some(
        serde_json::from_value(
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1}),
        )
        .unwrap(),
    );
        record.config.lifecycle_settings_revision = Some("7".into());
        record.config.lifecycle_intent = Some("initial".into());
        record.lifecycle_input_required = Some(false);
        let mut controller = Controller::new(Settings::default()).unwrap();
        let start = record.authenticated_start_payload(23).unwrap();
        controller.schedule_arm(&mut record, start, 8, 9, 0);
        controller.records.insert(record.id, record);
        let mut endpoint = Endpoint {
            verifier_reply: Some(
                json!({"status":"succeeded","operation_id":"rtsp-0000000000000005-lifecycle-prepare",
        "target":{"kind":"stream","host_epoch":"foreign","session_id":"session","stream_id":"stream","generation":1},
        "candidate":{"capture_provider":"amaterasu.capture-pipewire/1","selector_schema":"amaterasu.kwin-wl-output/1","selector":{"output":"kwin:Virtual-12"},
            "resource":{"provider":"provider","resource_id":"output","incarnation":"one","revision":1,"execution_domain":"desktop","mutation_domain":null,"mutation_domains":["desktop"]},
            "mode":{"width":announce.video.width,"height":announce.video.height,"refresh_millihz":announce.video.fps * 1000,"hdr":false},"plan_revision":1,"geometry_revision":1,"generation":1,"fallback":"forbidden"}}),
            ),
            ..Endpoint::default()
        };
        let reply = endpoint.verifier_reply.as_mut().unwrap();
        if mismatch != "epoch" {
            reply["target"]["host_epoch"] = json!("epoch");
        }
        match mismatch {
            "generation" => reply["candidate"]["generation"] = json!(2),
            "geometry" => reply["candidate"]["geometry_revision"] = json!(0),
            "plan" => reply["candidate"]["plan_revision"] = json!(0),
            "width" => reply["candidate"]["mode"]["width"] = json!(announce.video.width + 1),
            "height" => reply["candidate"]["mode"]["height"] = json!(announce.video.height + 1),
            "rate" => {
                reply["candidate"]["mode"]["refresh_millihz"] = json!(announce.video.fps * 1000 + 1)
            }
            "hdr" => reply["candidate"]["mode"]["hdr"] = json!(true),
            _ => {}
        }
        let runtime = client(&mut endpoint);
        controller.advance_job(&runtime, 1);
        controller.advance_job(&runtime, 2);
        assert!(controller.job.is_none());
        assert_eq!(controller.records[&5].state, State::Closing);
        assert!(controller.records[&5].lifecycle_prepare_outstanding);
        assert!(!endpoint.commands.iter().any(|command| matches!(command, Command::Request {service,payload,..} if service == "amaterasu.stream-session/1" && payload["action"] == "start_when_authenticated")));
    }
}

#[test]
fn lifecycle_cleanup_status_distinguishes_unadmitted_unknown_and_retired_streams() {
    let mut record = record(State::Ready);
    assert!(record.response()["lifecycle_cleanup_state"].is_null());
    record.config.lifecycle_session_target = Some(
        serde_json::from_value(json!({
            "kind":"session","host_epoch":"epoch","session_id":"session","generation":1
        }))
        .unwrap(),
    );
    assert_eq!(record.response()["lifecycle_cleanup_state"], "not_admitted");
    record.lifecycle_prepare_outstanding = true;
    assert_eq!(
        record.response()["lifecycle_cleanup_state"],
        "prepare_unresolved"
    );
    record.lifecycle_prepare_outstanding = false;
    record.lifecycle_target = Some(
        json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1}),
    );
    assert_eq!(
        record.response()["lifecycle_cleanup_state"],
        "retirement_pending"
    );
    assert_eq!(record.response()["lifecycle_retired"], false);
    record.cleanup_confirmed = true;
    assert_eq!(record.response()["lifecycle_cleanup_state"], "retired");
    assert_eq!(record.response()["lifecycle_retired"], true);
}

#[test]
fn lifecycle_consumer_requirements_follow_frozen_stream_session_input_policy() {
    for policy in [Some(false), Some(true), None] {
        let (mut record, announce) = announced_profile_record();
        assert!(record.accept_announce(&announce));
        record.config.lifecycle_session_target = Some(serde_json::from_value(json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1})).unwrap());
        record.config.lifecycle_settings_revision = Some("7".into());
        record.config.lifecycle_intent = Some("initial".into());
        record.lifecycle_input_required = policy;
        let mut controller = Controller::new(Settings::default()).unwrap();
        let payload = record.authenticated_start_payload(23).unwrap();
        controller.schedule_arm(&mut record, payload, 8, 9, 0);
        if policy.is_none() {
            assert!(controller.job.is_none());
            assert_eq!(record.failure, Some("lifecycle_input_policy_unverified"));
            continue;
        }
        controller.records.insert(record.id, record);
        let mut endpoint = Endpoint::default();
        let runtime = client(&mut endpoint);
        controller.advance_job(&runtime, 1);
        let request = endpoint
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.session-lifecycle/1" => Some(payload),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            request["consumer_requirements"],
            if policy == Some(true) {
                json!(["capture", "input"])
            } else {
                json!(["capture"])
            }
        );
    }
}

#[test]
fn external_lifecycle_preparation_preserves_capture_and_requires_matching_mode() {
    for returned_mode in ["external", "managed"] {
        let (mut record, announce) = announced_profile_record();
        assert!(record.accept_announce(&announce));
        record.config.lifecycle_session_target = Some(
            serde_json::from_value(json!({
                "kind":"session","host_epoch":"epoch","session_id":"session","generation":1
            }))
            .unwrap(),
        );
        record.config.lifecycle_settings_revision = Some("7".into());
        record.config.lifecycle_intent = Some("initial".into());
        record.config.lifecycle_capture_mode =
            amaterasu_module_sdk::extension_api::CaptureBindingMode::External;
        record.lifecycle_input_required = Some(true);
        let mut controller = Controller::new(Settings::default()).unwrap();
        let payload = record.authenticated_start_payload(23).unwrap();
        controller.schedule_arm(&mut record, payload, 8, 9, 0);
        controller.records.insert(record.id, record);
        let mut endpoint = Endpoint {
            verifier_reply: Some(json!({"status":"succeeded",
                "operation_id":"rtsp-0000000000000005-lifecycle-prepare",
                "transition_id":"transition","capture_binding_mode":returned_mode,
                "target":{"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1},
                "candidate":null,"required_consumers":["capture"]})),
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        for tick in 1..=5 {
            controller.advance_job(&runtime, tick);
        }
        assert!(endpoint.commands.iter().any(|command| matches!(command,
            Command::Request { service, payload, .. }
            if service == "amaterasu.session-lifecycle/1"
                && payload["consumer_requirements"] == json!(["capture"])
        )));
        let start = endpoint.commands.iter().find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.stream-session/1"
                && payload["action"] == "start_when_authenticated" =>
            {
                Some(payload)
            }
            _ => None,
        });
        if returned_mode == "external" {
            let start =
                start.expect("external preparation forwards the native capture configuration");
            assert_eq!(start["configuration"]["lifecycle_capture_mode"], "external");
            assert!(start["configuration"]["lifecycle_candidate"].is_null());
        } else {
            assert!(
                start.is_none(),
                "a changed capture ownership mode must not start capture"
            );
        }
    }
}

#[test]
fn authenticated_disconnect_requires_exact_clean_lifecycle_retirement() {
    let mut record = record(State::Playing);
    let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
    record.lifecycle_target = Some(target.clone());
    let mut payload = json!({"ok":true,"session":23,"state":"failed","cleanup_error":null,
        "lifecycle_target":target,"lifecycle_retired":true,"authenticated_disconnect_reason":"peer_disconnected"});
    assert_eq!(
        authenticated_disconnect_reason(&record, &payload),
        Some("peer_disconnected")
    );
    payload["lifecycle_retired"] = json!(false);
    assert_eq!(authenticated_disconnect_reason(&record, &payload), None);
    payload["lifecycle_retired"] = json!(true);
    payload["session"] = json!(24);
    assert_eq!(authenticated_disconnect_reason(&record, &payload), None);
    payload["session"] = json!(23);
    payload["authenticated_disconnect_reason"] = json!("video_capture_failed");
    assert_eq!(authenticated_disconnect_reason(&record, &payload), None);
    payload["authenticated_disconnect_reason"] = json!("peer_disconnected");
    payload["state"] = json!("stopped");
    assert_eq!(authenticated_disconnect_reason(&record, &payload), None);
}

#[test]
fn auxiliary_disconnect_requires_an_exact_authenticated_control_retirement_witness() {
    let record = record(State::Playing);
    let mut payload = json!({"ok":true,"session":23,"state":"failed","cleanup_error":null,
        "authenticated_disconnect_reason":"peer_disconnected"});
    assert_eq!(authenticated_disconnect_reason(&record, &payload), None);
    payload["authenticated_disconnect"] = json!({"version":1,"session":23,
        "operation_id":record.operation(),"control_authenticated":true,"reason":"peer_disconnected"});
    assert_eq!(
        authenticated_disconnect_reason(&record, &payload),
        Some("peer_disconnected")
    );
    for (field, value) in [
        ("session", json!(24)),
        ("operation_id", json!("another-transport")),
        ("control_authenticated", json!(false)),
        ("reason", json!("peer_timeout")),
    ] {
        let mut forged = payload.clone();
        forged["authenticated_disconnect"][field] = value;
        assert_eq!(
            authenticated_disconnect_reason(&record, &forged),
            None,
            "{field}"
        );
    }
    payload["cleanup_error"] = json!("native_retirement_pending");
    assert_eq!(authenticated_disconnect_reason(&record, &payload), None);
}

#[test]
fn absent_stream_session_cannot_confirm_unresolved_lifecycle_retirement() {
    for issued_target in [false, true] {
        let mut controller = Controller::new(Settings::default()).unwrap();
        let mut record = record(State::Closing);
        record.session = None;
        record.needs_lookup = false;
        if issued_target {
            record.lifecycle_target = Some(
                json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1}),
            );
        } else {
            record.lifecycle_prepare_outstanding = true;
        }
        controller.schedule_cleanup(&mut record, None, 0);
        assert_eq!(record.state, State::CleanupFailed);
        assert!(!record.cleanup_confirmed);
        assert_eq!(
            record.cleanup_error,
            Some("lifecycle_retirement_unconfirmed")
        );
    }
}

fn lifecycle_cleanup_fixture(external: bool, joining: bool) -> (Record, Value) {
    let (mut record, announce) = announced_profile_record();
    assert!(record.accept_announce(&announce));
    record.config.lifecycle_session_target = serde_json::from_value(json!({
        "kind":"session","host_epoch":"epoch","session_id":"session","generation":1
    }))
    .ok();
    record.config.lifecycle_settings_revision = Some("7".into());
    record.config.lifecycle_intent = Some(if joining { "join" } else { "initial" }.into());
    record.lifecycle_input_required = Some(false);
    let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
    let candidate = json!({"capture_provider":"amaterasu.capture-pipewire/1","selector_schema":"amaterasu.kwin-wl-output/1","selector":{"output":"kwin:Virtual-12"},
        "resource":{"provider":"provider","resource_id":"output","incarnation":"one","revision":1,"execution_domain":"desktop","mutation_domain":null,"mutation_domains":["desktop"]},
        "mode":{"width":announce.video.width,"height":announce.video.height,"refresh_millihz":announce.video.fps * 1000,"hdr":false},
        "plan_revision":1,"geometry_revision":1,"generation":1,"fallback":"forbidden"});
    let mut reply = json!({"status":"succeeded",
        "operation_id":"rtsp-0000000000000005-lifecycle-prepare","target":target,"candidate":candidate});
    if external {
        record.config.lifecycle_capture_mode =
            amaterasu_extension_api::CaptureBindingMode::External;
        reply["capture_binding_mode"] = json!("external");
        reply["candidate"] = Value::Null;
        reply["required_consumers"] = json!(["capture"]);
    }
    if joining {
        let source = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"source","generation":3});
        let mut binding = reply["candidate"].clone();
        if !external {
            binding["generation"] = json!(3);
        }
        reply["shared_source"] = source.clone();
        reply["join_context"] = json!({"source":source,"source_transition":"source-prepare",
            "capture_binding_mode":record.config.lifecycle_capture_mode,
            "binding_owner":if external { Value::Null } else { json!("provider") },"binding":binding});
    }
    (record, reply)
}

#[test]
fn disconnect_during_lifecycle_prepare_resolves_admission_before_owned_cleanup() {
    for (external, joining) in [(false, false), (true, false), (false, true), (true, true)] {
        let (mut record, reply) = lifecycle_cleanup_fixture(external, joining);
        let mut controller = Controller::new(Settings::default()).unwrap();
        let start = record.authenticated_start_payload(23).unwrap();
        controller.schedule_arm(&mut record, start, 8, 9, 0);
        controller.records.insert(record.id, record);
        let mut endpoint = Endpoint::default();
        let runtime = client(&mut endpoint);

        controller.advance_job(&runtime, 1);
        controller.disconnected(8, Some(5), "peer_disconnected");
        controller.advance_job(&runtime, 2);
        controller.advance_job(&runtime, 3);
        assert!(
            !endpoint
                .commands
                .iter()
                .any(|command| matches!(command, Command::Cancel { .. }))
        );
        assert!(!endpoint.commands.iter().any(|command| matches!(command,
            Command::Request { payload, .. } if payload["action"] == "stop")));

        endpoint.verifier_reply = Some(reply.clone());
        controller.advance_job(&runtime, 4);
        controller.advance_job(&runtime, 5);
        let stop = endpoint
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.stream-session/1" && payload["action"] == "stop" => {
                    Some(payload)
                }
                _ => None,
            })
            .expect("owned stream-session stop after exact preparation resolution");
        assert_eq!(
            stop["lifecycle_configuration"]["lifecycle_target"],
            reply["target"]
        );
        assert_eq!(
            stop["lifecycle_configuration"]["lifecycle_candidate"],
            reply["candidate"]
        );
        assert_eq!(
            stop["lifecycle_configuration"]["lifecycle_shared_source"],
            reply["shared_source"]
        );
        assert_eq!(
            stop["lifecycle_configuration"]["lifecycle_capture_mode"],
            if external { "external" } else { "managed" }
        );
        assert!(!endpoint.commands.iter().any(|command| matches!(command,
            Command::Request { payload, .. } if payload["action"] == "start_when_authenticated")));

        let mut terminal = json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null,
            "lifecycle_target":reply["target"],"lifecycle_retired":!joining});
        if joining {
            terminal["lifecycle_consumer_retired"] = json!(true);
            terminal["lifecycle_resources_released"] = json!(false);
            terminal["lifecycle_shared_retained"] = json!(true);
            terminal["lifecycle_shared_source"] = reply["shared_source"].clone();
        }
        endpoint.verifier_reply = Some(terminal);
        controller.advance_job(&runtime, 6);
        let record = &controller.records[&5];
        assert!(record.state.terminal());
        assert!(record.cleanup_confirmed);
        assert!(!record.lifecycle_prepare_outstanding);
        assert_eq!(record.lifecycle_target.as_ref(), Some(&reply["target"]));
    }
}

#[test]
fn lifecycle_prepare_deadline_and_late_success_preserve_cleanup_only_admission() {
    for late_success in [false, true] {
        let (mut record, reply) = lifecycle_cleanup_fixture(false, false);
        let mut controller = Controller::new(Settings::default()).unwrap();
        let start = record.authenticated_start_payload(23).unwrap();
        controller.schedule_arm(&mut record, start, 8, 9, 0);
        let deadline = record.expires;
        controller.records.insert(record.id, record);
        let mut endpoint = Endpoint::default();
        let runtime = client(&mut endpoint);
        controller.advance_job(&runtime, 1);
        if late_success {
            // The coordinator reply may be available after the transport has
            // already committed to closing. It remains cleanup evidence.
            let mut record = controller.records.remove(&5).unwrap();
            record.close(Some("network_disconnected"));
            let mut job = controller.job.take().unwrap();
            job.network = None;
            controller.consume_result(&mut record, job, reply, 2, &runtime);
            controller.records.insert(5, record);
        } else {
            controller.advance_job(&runtime, deadline);
            controller.advance_job(&runtime, deadline + 1);
            assert!(
                controller
                    .job
                    .as_ref()
                    .is_some_and(|job| job.phase == Phase::LifecyclePrepare)
            );
            endpoint.verifier_reply = Some(reply);
            controller.advance_job(&runtime, deadline + 2);
        }
        controller.advance_job(&runtime, deadline + 3);
        assert!(endpoint.commands.iter().any(|command| matches!(command,
            Command::Request { service, payload, .. } if service == "amaterasu.stream-session/1"
                && payload["action"] == "stop" && payload["lifecycle_configuration"]["lifecycle_target"]["stream_id"] == "stream")));
        assert!(!endpoint.commands.iter().any(|command| matches!(command,
            Command::Request { payload, .. } if payload["action"] == "start_when_authenticated")));
    }
}

#[test]
fn missing_or_foreign_lifecycle_lookup_cannot_confirm_cleanup() {
    for reply in [
        json!({"status":"not_found"}),
        json!({"status":"rejected","operation_id":"foreign-operation"}),
    ] {
        let (mut record, _) = lifecycle_cleanup_fixture(false, false);
        record.state = State::Closing;
        record.lifecycle_prepare_outstanding = true;
        let mut controller = Controller::new(Settings::default()).unwrap();
        controller.schedule_cleanup(&mut record, None, 0);
        controller.records.insert(record.id, record);
        let mut endpoint = Endpoint {
            verifier_reply: Some(reply),
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        controller.advance_job(&runtime, 1);
        controller.advance_job(&runtime, 2);
        let record = &controller.records[&5];
        assert_eq!(record.state, State::CleanupFailed);
        assert!(record.lifecycle_prepare_outstanding);
        assert!(!record.cleanup_confirmed);
        assert!(!endpoint.commands.iter().any(|command| matches!(command,
            Command::Request { payload, .. } if payload["action"] == "stop")));
    }
}

#[test]
fn disconnect_during_lifecycle_lookup_retries_the_exact_preparation_before_cleanup() {
    let (mut record, reply) = lifecycle_cleanup_fixture(false, false);
    let mut controller = Controller::new(Settings::default()).unwrap();
    let start = record.authenticated_start_payload(23).unwrap();
    controller.schedule_arm(&mut record, start, 8, 9, 0);
    controller.records.insert(record.id, record);
    let mut endpoint = Endpoint {
        verifier_reply: Some(
            json!({"status":"pending","operation_id":"rtsp-0000000000000005-lifecycle-prepare"}),
        ),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    for now in 1..=3 {
        controller.advance_job(&runtime, now);
    }
    controller.disconnected(8, Some(5), "peer_disconnected");
    controller.advance_job(&runtime, 4);
    assert_eq!(controller.records[&5].state, State::Closing);
    endpoint.verifier_reply = Some(reply);
    for now in 256..=258 {
        controller.advance_job(&runtime, now);
    }
    assert!(!controller.records[&5].lifecycle_prepare_outstanding);
    assert!(endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { service, payload, .. } if service == "amaterasu.stream-session/1"
            && payload["action"] == "stop" && payload["lifecycle_configuration"]["lifecycle_target"]["stream_id"] == "stream")));
    assert!(!endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { payload, .. } if payload["action"] == "start_when_authenticated")));
}

#[test]
fn shutdown_during_lifecycle_prepare_resolves_admission_before_native_cleanup() {
    for (external, joining) in [(false, false), (true, false), (false, true), (true, true)] {
        for during_lookup in [false, true] {
            let (mut record, reply) = lifecycle_cleanup_fixture(external, joining);
            let mut controller = Controller::new(Settings::default()).unwrap();
            let start = record.authenticated_start_payload(23).unwrap();
            controller.schedule_arm(&mut record, start, 8, 9, 0);
            controller.records.insert(record.id, record);
            let lifecycle_results = if during_lookup {
                VecDeque::from([
                    Some(
                        json!({"status":"pending","operation_id":"rtsp-0000000000000005-lifecycle-prepare"}),
                    ),
                    None,
                    Some(
                        json!({"status":"pending","operation_id":"rtsp-0000000000000005-lifecycle-prepare"}),
                    ),
                    Some(reply.clone()),
                ])
            } else {
                VecDeque::from([None, Some(reply.clone())])
            };
            let mut terminal = json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null,
                "lifecycle_target":reply["target"],"lifecycle_retired":!joining});
            if joining {
                terminal["lifecycle_consumer_retired"] = json!(true);
                terminal["lifecycle_resources_released"] = json!(false);
                terminal["lifecycle_shared_retained"] = json!(true);
                terminal["lifecycle_shared_source"] = reply["shared_source"].clone();
            }
            let mut endpoint = Endpoint {
                lifecycle_results,
                recovery_results: VecDeque::from([Some(terminal)]),
                ..Endpoint::default()
            };
            let runtime = client(&mut endpoint);
            for now in 1..=if during_lookup { 3 } else { 1 } {
                controller.advance_job(&runtime, now);
            }
            controller.shutdown(&runtime);
            if !during_lookup {
                assert!(controller.records[&5].uncertain.is_some());
                assert!(!controller.records[&5].cleanup_confirmed);
                assert!(!endpoint.commands.iter().any(|command| matches!(
                    command,
                    Command::Cancel { .. } | Command::ReleaseResult { .. }
                )));
                assert!(!endpoint.commands.iter().any(|command| matches!(command,
                    Command::Request { payload, .. } if payload["action"] == "stop")));
                continue;
            }
            assert!(endpoint.commands.iter().any(|command| matches!(command,
                Command::Request { service, payload, .. } if service == "amaterasu.session-lifecycle/1"
                    && payload["operation"] == "lookup" && payload["operation_id"] == "rtsp-0000000000000005-lifecycle-prepare")));
            let stop = endpoint
                .commands
                .iter()
                .find_map(|command| match command {
                    Command::Request { payload, .. } if payload["action"] == "stop" => {
                        Some(payload)
                    }
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                stop["lifecycle_configuration"]["lifecycle_target"],
                reply["target"]
            );
            assert_eq!(
                stop["lifecycle_configuration"]["lifecycle_candidate"],
                reply["candidate"]
            );
            assert_eq!(
                stop["lifecycle_configuration"]["lifecycle_shared_source"],
                reply["shared_source"]
            );
            assert_eq!(
                stop["lifecycle_configuration"]["lifecycle_capture_mode"],
                if external { "external" } else { "managed" }
            );
            assert!(controller.records[&5].cleanup_confirmed);
            assert!(!controller.records[&5].lifecycle_prepare_outstanding);
            assert!(!endpoint.commands.iter().any(|command| matches!(command,
                Command::Request { payload, .. } if payload["action"] == "start_when_authenticated")));
        }
    }
}

#[test]
fn shutdown_before_lifecycle_prepare_submission_never_admits_a_coordinator_operation() {
    let (mut record, _) = lifecycle_cleanup_fixture(false, false);
    let mut controller = Controller::new(Settings::default()).unwrap();
    let start = record.authenticated_start_payload(23).unwrap();
    controller.schedule_arm(&mut record, start, 8, 9, 0);
    controller.records.insert(record.id, record);
    let mut endpoint = Endpoint {
        recovery_results: VecDeque::from([Some(
            json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null}),
        )]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    controller.shutdown(&runtime);
    assert!(!endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { service, .. } if service == "amaterasu.session-lifecycle/1")));
    assert!(controller.records[&5].cleanup_confirmed);
    assert!(!controller.records[&5].lifecycle_prepare_outstanding);
}

#[test]
fn cancelled_unsubmitted_lifecycle_prepare_stops_only_the_prepared_native_session() {
    let (mut record, _) = lifecycle_cleanup_fixture(false, false);
    let mut controller = Controller::new(Settings::default()).unwrap();
    let start = record.authenticated_start_payload(23).unwrap();
    controller.schedule_arm(&mut record, start, 8, 9, 0);
    controller.records.insert(record.id, record);
    let mut endpoint = Endpoint {
        recovery_results: VecDeque::from([Some(
            json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null}),
        )]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    controller.disconnected(8, Some(5), "peer_disconnected");
    for now in 1..=3 {
        controller.advance_job(&runtime, now);
    }
    assert!(!endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { service, .. } if service == "amaterasu.session-lifecycle/1")));
    assert!(controller.records[&5].cleanup_confirmed);
    assert!(!controller.records[&5].lifecycle_prepare_outstanding);
}

#[test]
fn disconnect_does_not_wait_for_an_inflight_background_status_before_stop() {
    for status in [
        None,
        Some(json!({"ok":true,"session":23,"state":"active","cleanup_error":null})),
    ] {
        let mut controller = Controller::new(Settings::default()).unwrap();
        let mut other = record(State::Playing);
        let record = record(State::Playing);
        controller.schedule_status(&record, None, None, 0);
        controller.records.insert(record.id, record);
        other.id = 6;
        other.session = Some(24);
        controller.records.insert(other.id, other);
        let mut endpoint = Endpoint {
            recovery_results: VecDeque::from([
                status,
                Some(json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null})),
            ]),
            deny_settlement_requests: BTreeSet::from([701]),
            ..Endpoint::default()
        };
        let runtime = client(&mut endpoint);
        controller.advance_job(&runtime, 1);
        assert!(endpoint.recovery_requests.contains_key(&701));
        controller.advance_job(&runtime, 2);
        controller.disconnected(8, Some(5), "peer_disconnected");
        for now in 3..=15 {
            controller.advance_shutdown_settlement(&runtime);
            controller.advance_job(&runtime, now);
            if controller.job.is_none() {
                controller.schedule_background(now);
            }
        }
        assert!(endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { payload, .. } if payload["action"] == "stop" && payload["session"] == 23)),
        "a pending read-only observation must not delay Stop");
        assert!(controller.records[&5].cleanup_confirmed);
        assert_eq!(controller.records[&6].state, State::Playing);
        assert_eq!(controller.shutdown_settlement.len(), 1);
        endpoint.deny_settlement_requests.clear();
        controller.advance_shutdown_settlement(&runtime);
        assert!(controller.shutdown_settlement.is_empty());
    }
}

#[test]
fn post_stop_status_polls_without_submitting_another_stop() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let record = record(State::Closing);
    controller.schedule_stop(&record, None, 0);
    controller.records.insert(record.id, record);
    let active = json!({"ok":true,"session":23,"state":"active","cleanup_error":null});
    let mut endpoint = Endpoint {
        recovery_results: VecDeque::from([
            Some(active.clone()),
            Some(active),
            Some(json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null})),
        ]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    for now in 1..=15 {
        controller.advance_job(&runtime, now);
    }
    let stops = endpoint
        .commands
        .iter()
        .filter(|command| {
            matches!(command,
        Command::Request { payload, .. } if payload["action"] == "stop" && payload["session"] == 23)
        })
        .count();
    assert_eq!(stops, 1);
    assert!(controller.records[&5].cleanup_confirmed);
}

#[test]
fn detached_observation_capacity_leaves_room_for_every_playing_record() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    for index in 0..MAX_RECORDS {
        let mut record = record(State::Playing);
        record.id = index as u64 + 1;
        record.session = Some(index as u64 + 1);
        record.next_status = 0;
        let ticket = record.id;
        controller.records.insert(ticket, record);
        endpoint.recovery_results.push_back(None);
        endpoint
            .recovery_results
            .push_back(Some(json!({"ok":true,"session":ticket,
            "state":"stopped","cleanup_error":null})));
        controller.schedule_background(1);
        assert!(
            controller
                .job
                .as_ref()
                .is_some_and(|job| job.phase == Phase::Observe)
        );
        controller.advance_job(&runtime, 2);
        let request = 701 + 2 * index as u64;
        endpoint.deny_settlement_requests.insert(request);
        controller.disconnected(8, Some(ticket), "peer_disconnected");
        controller.advance_job(&runtime, 3);
        assert_eq!(controller.shutdown_settlement.len(), index + 1);
        controller.schedule_background(4);
        controller.advance_job(&runtime, 5);
        controller.advance_job(&runtime, 6);
        assert!(controller.records[&ticket].cleanup_confirmed);
    }
    assert_eq!(controller.shutdown_settlement.len(), MAX_RECORDS);
    assert!(
        controller
            .records
            .values()
            .all(|record| record.state != State::Playing)
    );
}

#[test]
fn undispatched_lifecycle_lookup_cannot_clear_the_original_preparation() {
    let (mut record, reply) = lifecycle_cleanup_fixture(false, false);
    record.state = State::Closing;
    record.lifecycle_prepare_outstanding = true;
    let mut controller = Controller::new(Settings::default()).unwrap();
    controller.schedule_cleanup(&mut record, None, 0);
    controller.records.insert(record.id, record);
    let mut endpoint = Endpoint {
        lifecycle_results: VecDeque::from([Some(reply.clone())]),
        recovery_results: VecDeque::from([Some(
            json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null,
            "lifecycle_target":reply["target"],"lifecycle_retired":true}),
        )]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    controller.shutdown(&runtime);
    assert_eq!(
        controller.records[&5].lifecycle_target.as_ref(),
        Some(&reply["target"])
    );
    assert!(controller.records[&5].cleanup_confirmed);
}

#[test]
fn shutdown_with_no_remaining_budget_never_admits_new_work() {
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let response = shutdown_request(
        &runtime,
        "amaterasu.session-lifecycle/1",
        json!({"operation":"lookup","operation_id":"exact"}),
        "shutdown-expired".into(),
        Instant::now(),
    );
    assert!(response.is_err());
    assert!(endpoint.commands.is_empty());
}

#[test]
fn shutdown_status_bookkeeping_failure_does_not_block_native_stop() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let record = record(State::Playing);
    controller.job = Some(Job {
        ticket: record.id,
        phase: Phase::Status,
        call: Call::new(
            json!({"action":"status","session":23}),
            "read-only-status".into(),
            None,
        ),
        caller: None,
        network: None,
        deadline: 5_000,
    });
    controller.records.insert(record.id, record);
    let mut endpoint = Endpoint {
        deny_settlement_requests: BTreeSet::from([71]),
        recovery_results: VecDeque::from([Some(
            json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null}),
        )]),
        ..Endpoint::default()
    };
    // Admit only the read-only observation before assigning the stop result.
    let terminal = endpoint.recovery_results.pop_front().unwrap();
    let runtime = client(&mut endpoint);
    controller.advance_job(&runtime, 1);
    endpoint.recovery_results.push_back(terminal);
    controller.shutdown(&runtime);
    assert!(controller.records[&5].cleanup_confirmed);
    assert_eq!(controller.shutdown_settlement.len(), 1);
    assert!(endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { payload, .. } if payload["action"] == "stop" && payload["session"] == 23)));
}

#[test]
fn shutdown_preserves_a_known_lifecycle_result_when_only_result_release_fails() {
    let (mut record, reply) = lifecycle_cleanup_fixture(false, false);
    let mut controller = Controller::new(Settings::default()).unwrap();
    let start = record.authenticated_start_payload(23).unwrap();
    controller.schedule_arm(&mut record, start, 8, 9, 0);
    controller.records.insert(record.id, record);
    let mut endpoint = Endpoint {
        verifier_reply: Some(reply.clone()),
        deny_settlement_requests: BTreeSet::from([71]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    controller.advance_job(&runtime, 1);
    controller.advance_job(&runtime, 2);
    assert!(
        controller
            .job
            .as_ref()
            .unwrap()
            .call
            .observed_result()
            .is_some()
    );
    endpoint.recovery_results.push_back(Some(
        json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null,
        "lifecycle_target":reply["target"],"lifecycle_retired":true}),
    ));
    controller.shutdown(&runtime);
    assert!(controller.records[&5].cleanup_confirmed);
    assert!(!controller.records[&5].lifecycle_prepare_outstanding);
    assert_eq!(controller.shutdown_settlement.len(), 1);
    let stop = endpoint
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request { payload, .. } if payload["action"] == "stop" => Some(payload),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        stop["lifecycle_configuration"]["lifecycle_target"],
        reply["target"]
    );
}

#[test]
fn shutdown_records_share_the_original_aggregate_deadline() {
    let (mut pending, _) = lifecycle_cleanup_fixture(false, false);
    pending.state = State::Closing;
    pending.lifecycle_prepare_outstanding = true;
    let mut second = record(State::Playing);
    second.id = 6;
    second.session = Some(24);
    let mut controller = Controller::new(Settings::default()).unwrap();
    controller.records.insert(pending.id, pending);
    controller.records.insert(second.id, second);
    let mut endpoint = Endpoint {
        verifier_reply: Some(
            json!({"status":"pending","operation_id":"rtsp-0000000000000005-lifecycle-prepare"}),
        ),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    let started = Instant::now();
    controller.stop_owned_sessions_on_shutdown(&runtime, started + Duration::from_millis(15));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { payload, .. } if payload["action"] == "stop" && payload["session"] == 24)));
    assert!(!controller.records[&5].cleanup_confirmed);
    assert!(!controller.records[&6].cleanup_confirmed);
}

#[test]
fn shutdown_announcement_rejects_new_play_without_releasing_prepared_ownership() {
    let (mut record, announce) = announced_profile_record();
    assert!(record.accept_announce(&announce));
    let mut controller = Controller::new(Settings::default()).unwrap();
    controller.records.insert(record.id, record);
    controller.prepare_shutdown(10, 1_000);
    controller.handle_network(77, 5, request(Method::Play, "/", 7, true), 11);
    let record = &controller.records[&5];
    assert_eq!(record.state, State::Ready);
    assert!(record.failure.is_none());
    assert!(!record.lifecycle_prepare_outstanding);
    assert!(controller.job.is_none());
}

#[test]
fn owner_close_before_rtsp_quiescence_uses_the_announced_shutdown_deadline() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    controller.records.insert(5, record(State::Playing));
    let mut endpoint = Endpoint {
        active_request: true,
        recovery_results: VecDeque::from([None]),
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    controller.prepare_shutdown(100, 10);
    controller.close(9, "caller", 5, 109, &runtime);
    assert_eq!(controller.job.as_ref().unwrap().deadline, 110);
    controller.tick(&runtime, 109).unwrap();
    controller.tick(&runtime, 110).unwrap();
    for now in [400, 401, 1_500, 1_501] {
        controller.tick(&runtime, now).unwrap();
    }
    assert!(!controller.records[&5].cleanup_confirmed);
    assert_eq!(
        endpoint
            .commands
            .iter()
            .filter(|command| matches!(command, Command::Request { .. }))
            .count(),
        1,
        "owner cleanup must not admit more work after the original shutdown bound"
    );
}

#[test]
fn shutdown_announcement_does_not_replace_an_existing_forward_start_deadline() {
    let (mut record, _) = lifecycle_cleanup_fixture(false, false);
    let mut controller = Controller::new(Settings::default()).unwrap();
    let start = record.authenticated_start_payload(23).unwrap();
    controller.schedule_arm(&mut record, start, 8, 9, 0);
    let original_deadline = controller.job.as_ref().unwrap().deadline;
    controller.records.insert(record.id, record);
    controller.prepare_shutdown(100, 10);
    assert_eq!(controller.job.as_ref().unwrap().deadline, original_deadline);
    assert_eq!(controller.records[&5].state, State::Arming);
}

#[test]
fn exhausted_operation_sequence_does_not_reuse_an_identifier() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let record = record(State::Preparing);
    controller.sequence = u64::MAX - 1;
    let last = controller.operation_id(&record, "stop");
    assert!(!last.is_empty());
    assert!(controller.operation_id(&record, "stop").is_empty());
    assert!(controller.operation_id(&record, "status").is_empty());
}

#[test]
fn exhausted_stop_cannot_claim_session_cleanup() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let record = record(State::Closing);
    controller.sequence = u64::MAX;
    controller.schedule_stop(&record, None, 0);
    controller.records.insert(record.id, record);
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    controller.advance_job(&runtime, 1);
    controller.advance_job(&runtime, 2);
    let record = &controller.records[&5];
    assert_eq!(record.state, State::CleanupFailed);
    assert!(!record.cleanup_confirmed);
    assert!(
        endpoint
            .commands
            .iter()
            .all(|command| !matches!(command, Command::Request { .. }))
    );
}
