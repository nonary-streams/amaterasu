use super::*;
use amaterasu_module_sdk::{
    ManagedModule,
    abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink},
};
use std::ffi::c_void;

#[derive(Default)]
struct Endpoint {
    commands: Vec<Command>,
    requests: BTreeMap<u64, (String, Value)>,
    next_request: u64,
    provider_progress: bool,
    blocked_settlement: Option<u64>,
    completions: Vec<(u64, Value)>,
}

impl Endpoint {
    fn dispatch(&mut self, command: Command) -> Response {
        self.commands.push(command.clone());
        match command {
            Command::Poll {} => Response::Pending,
            Command::CheckRequest { .. } => Response::Ok,
            Command::Request {
                service, payload, ..
            } => {
                self.next_request += 1;
                self.requests.insert(self.next_request, (service, payload));
                Response::Accepted {
                    id: self.next_request,
                }
            }
            Command::TakeResult { request } => {
                let Some((service, payload)) = self.requests.get(&request) else {
                    return Response::error("not_found", "unknown test request");
                };
                let response = if service == "amaterasu.session-lifecycle/1" {
                    json!({"operation_id":payload["operation_id"],
                        "status":if self.provider_progress { "rejected" } else { "pending" }})
                } else {
                    match payload["action"].as_str() {
                        Some("stop") if !self.provider_progress => return Response::Pending,
                        Some("stop") => json!({"ok":true,"session":payload["session"],
                            "state":"stopped","cleanup_error":null}),
                        Some("status") if payload["session"].is_null() => json!({"ok":true,
                            "sessions":[],"total":0,"truncated":false,"startup_reconciled":true}),
                        Some("status") => json!({"ok":true,"session":payload["session"],
                            "state":"active","cleanup_error":null}),
                        Some("lookup") => json!({"ok":true,"session":23}),
                        _ => return Response::Pending,
                    }
                };
                Response::Result { payload: response }
            }
            Command::Cancel { request } | Command::ReleaseResult { request }
                if self.blocked_settlement == Some(request) =>
            {
                Response::error("unavailable", "settlement deliberately blocked")
            }
            Command::Complete { request, payload } => {
                self.completions.push((request, payload));
                Response::Ok
            }
            _ => Response::Ok,
        }
    }

    fn stops(&self) -> usize {
        self.requests
            .values()
            .filter(|(_, payload)| payload["action"] == "stop")
            .count()
    }
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> abi::Status {
    // SAFETY: each test retains its exclusively accessed endpoint for every
    // synchronous SDK invocation, including module destruction.
    let endpoint = unsafe { &mut *context.cast::<Endpoint>() };
    let Ok(bytes) = (unsafe { input.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let bytes = serde_json::to_vec(&endpoint.dispatch(command)).expect("test response");
    // SAFETY: the SDK sink copies the bytes before the invocation returns.
    unsafe { reply.send(&bytes) };
    abi::STATUS_OK
}

fn client(endpoint: &mut Endpoint) -> RuntimeClient {
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *mut Endpoint).cast(),
        invoke,
    };
    // SAFETY: tests retain endpoint throughout every runtime/module use.
    unsafe { RuntimeClient::from_endpoint(api) }
}

fn module(endpoint: &mut Endpoint, controller: Controller) -> crate::RtspModule {
    crate::RtspModule {
        runtime: client(endpoint),
        controller,
        started: Instant::now(),
    }
}

#[test]
fn sdk_quiesce_keeps_active_stream_alive_until_provider_ticks_confirm_retirement() {
    let mut endpoint = Endpoint::default();
    let mut controller = Controller::new(Settings::default()).unwrap();
    controller.records.insert(5, tests::record(State::Playing));
    let mut module = module(&mut endpoint, controller);

    assert!(!module.quiesce().unwrap());
    assert_eq!(
        endpoint.stops(),
        0,
        "quiesce must not block polling providers"
    );
    for _ in 0..6 {
        module.tick().unwrap();
        assert!(!module.quiesce().unwrap());
    }
    assert_eq!(endpoint.stops(), 1);
    assert!(!module.controller.records[&5].cleanup_confirmed);

    endpoint.provider_progress = true;
    for _ in 0..6 {
        module.tick().unwrap();
        if module.quiesce().unwrap() {
            break;
        }
    }
    assert!(module.quiesce().unwrap());
    assert!(module.controller.records[&5].state.terminal());
    assert!(module.controller.records[&5].cleanup_confirmed);
    assert_eq!(endpoint.stops(), 1);
}

#[test]
fn sdk_quiesce_resolves_admitted_pending_preparation_before_stopping_stream() {
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut record = tests::record(State::Arming);
    record.config.lifecycle_session_target =
        Some(amaterasu_extension_api::LifecycleTarget::Session {
            host_epoch: "epoch".into(),
            session_id: "session".into(),
            generation: 1,
        });
    record.lifecycle_prepare_outstanding = true;
    let mut call = Call::for_service(
        "amaterasu.session-lifecycle/1",
        json!({"operation":"prepare"}),
        format!("{}-lifecycle-prepare", record.operation()),
        None,
    );
    assert!(matches!(call.poll(&runtime), Outcome::Waiting));
    controller.job = Some(Job {
        ticket: record.id,
        phase: Phase::LifecyclePrepare,
        call,
        caller: None,
        network: None,
        deadline: 5_000,
    });
    controller.records.insert(record.id, record);
    let mut module = module(&mut endpoint, controller);

    assert!(!module.quiesce().unwrap());
    for _ in 0..8 {
        module.tick().unwrap();
        assert!(!module.quiesce().unwrap());
    }
    assert!(module.controller.records[&5].lifecycle_prepare_outstanding);
    assert_eq!(
        endpoint.stops(),
        0,
        "pending admission is not retirement proof"
    );

    endpoint.provider_progress = true;
    for _ in 0..12 {
        module.tick().unwrap();
        if module.quiesce().unwrap() {
            break;
        }
    }
    assert!(module.quiesce().unwrap());
    assert!(!module.controller.records[&5].lifecycle_prepare_outstanding);
    assert!(module.controller.records[&5].cleanup_confirmed);
    assert_eq!(endpoint.stops(), 1);
    assert!(
        endpoint
            .requests
            .values()
            .all(|(_, payload)| payload["action"] != "arm")
    );
}

#[test]
fn quiesce_does_not_let_read_only_status_settlement_block_native_stop() {
    let mut endpoint = Endpoint {
        provider_progress: true,
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    let mut controller = Controller::new(Settings::default()).unwrap();
    let record = tests::record(State::Playing);
    controller.schedule_status(&record, None, None, 0);
    controller.records.insert(record.id, record);
    controller.advance_job(&runtime, 0);
    endpoint.blocked_settlement = Some(1);

    assert!(!controller.quiesce(&runtime, 1));
    for now in 2..10 {
        controller.tick(&runtime, now).unwrap();
    }
    assert_eq!(endpoint.stops(), 1);
    assert!(controller.records[&5].cleanup_confirmed);
    assert!(
        !controller.quiesce(&runtime, 10),
        "metadata settlement is still pending"
    );
    endpoint.blocked_settlement = None;
    for now in 11..15 {
        controller.tick(&runtime, now).unwrap();
    }
    assert!(controller.quiesce(&runtime, 15));
}

#[test]
fn quiesce_uses_one_deadline_and_does_not_retry_failed_cleanup() {
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let mut controller = Controller::new(Settings::default()).unwrap();
    controller.records.insert(5, tests::record(State::Playing));
    assert!(!controller.quiesce(&runtime, 100));
    for now in [101, 102, 14_999, 15_099] {
        controller.tick(&runtime, now).unwrap();
        assert!(!controller.quiesce(&runtime, now));
    }
    assert_eq!(controller.job.as_ref().unwrap().deadline, 15_100);
    controller.tick(&runtime, 15_100).unwrap();
    assert_eq!(controller.records[&5].state, State::CleanupFailed);
    assert!(controller.records[&5].uncertain.is_some());
    assert_eq!(
        controller.records[&5].cleanup_error,
        Some("quiescence_deadline")
    );
    let requests = endpoint.requests.len();
    for now in [15_350, 16_100, 30_100, 60_100] {
        controller.tick(&runtime, now).unwrap();
        assert!(!controller.quiesce(&runtime, now));
    }
    assert_eq!(endpoint.requests.len(), requests);
    assert_eq!(endpoint.stops(), 1);
    assert_eq!(controller.records[&5].cleanup_retry_count, 0);
}

#[test]
fn quiesce_enters_cleanup_budget_once_instead_of_reusing_prepare_expiry() {
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut record = tests::record(State::Preparing);
    record.session = None;
    record.needs_lookup = true;
    controller.job = Some(Job {
        ticket: record.id,
        phase: Phase::Prepare,
        call: Call::new(json!({"action":"prepare"}), "pending-prepare".into(), None),
        caller: None,
        network: None,
        deadline: 5_000,
    });
    controller.records.insert(record.id, record);

    assert!(!controller.quiesce(&runtime, 4_999));
    assert_eq!(controller.job.as_ref().unwrap().deadline, 19_999);
    assert!(!controller.quiesce(&runtime, 5_000));
    assert_eq!(controller.job.as_ref().unwrap().deadline, 19_999);
    controller.tick(&runtime, 5_001).unwrap();
    if let Some(job) = &controller.job {
        assert_eq!(job.deadline, 19_999);
    }
    assert!(
        endpoint
            .requests
            .values()
            .all(|(_, payload)| payload["action"] != "prepare")
    );
}

#[test]
fn quiesce_does_not_reopen_existing_cleanup_failure_or_accept_unresolved_terminal_proof() {
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut record = tests::record(State::CleanupFailed);
    record.cleanup_error = Some("native_release_failed");
    record.cleanup_retry_at = Some(1);
    controller.records.insert(5, record);
    assert!(!controller.quiesce(&runtime, 0));
    for now in [1, 250, 1_000] {
        controller.tick(&runtime, now).unwrap();
    }
    assert_eq!(endpoint.stops(), 0);
    assert_eq!(
        controller.records[&5].cleanup_error,
        Some("native_release_failed")
    );

    let record = controller.records.get_mut(&5).unwrap();
    record.state = State::Closed;
    record.cleanup_error = None;
    record.cleanup_confirmed = true;
    record.lifecycle_prepare_outstanding = true;
    assert!(!controller.quiesce(&runtime, 1_001));
}

#[test]
fn quiesce_cancels_pending_display_verification_without_new_admission() {
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let mut controller = Controller::new(Settings::default()).unwrap();
    controller.display_admission = Some(DisplayAdmission {
        request: 9,
        caller: "caller".into(),
        config: tests::record(State::Ready).config,
        deadline: 5_000,
        request_sha256: "a".repeat(64),
        verified: false,
        verification_started_at: 0,
        display_preflight_deadline: None,
        call: Call::for_service(
            "amaterasu.launch/1",
            json!({"operation":"verify_display_admission"}),
            "verification".into(),
            None,
        ),
    });
    assert!(!controller.quiesce(&runtime, 0));
    controller.tick(&runtime, 1).unwrap();
    assert!(controller.quiesce(&runtime, 2));
    assert!(endpoint.requests.is_empty());
    assert_eq!(endpoint.completions[0].1["code"], "module_quiescing");
}

#[test]
fn quiesce_rejects_open_and_retains_owner_status_and_close_operations() {
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut record = tests::record(State::Closed);
    record.cleanup_confirmed = true;
    let config = record.config.clone();
    controller.records.insert(5, record);
    assert!(controller.quiesce(&runtime, 0));
    controller.open(
        OpenRequest {
            request: 1,
            caller: "caller".into(),
            config,
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        1,
        &runtime,
        false,
    );
    controller.status(2, "caller", Some(5), &runtime);
    controller.close(3, "caller", 5, 1, &runtime);
    assert_eq!(endpoint.completions[0].1["code"], "module_quiescing");
    assert_eq!(endpoint.completions[1].1["ticket"], 5);
    assert_eq!(endpoint.completions[2].1["state"], "closed");
    assert!(controller.quiesce(&runtime, 2));
}

#[test]
fn sdk_shutdown_announcement_closes_open_without_retiring_active_provider_effects() {
    let mut endpoint = Endpoint::default();
    let mut controller = Controller::new(Settings {
        enabled: true,
        bind: "127.0.0.1:0".parse().unwrap(),
        allowed_principals: vec!["caller".into()],
        ..Settings::default()
    })
    .unwrap();
    let record = tests::record(State::Playing);
    let config = record.config.clone();
    controller.records.insert(record.id, record);
    let listener = controller.local;
    assert!(listener.is_some());
    let mut module = module(&mut endpoint, controller);

    module.prepare_shutdown(20_000).unwrap();
    assert!(module.controller.shutdown_deadline.is_some());
    assert!(module.controller.quiescence.is_none());
    module.controller.open(
        OpenRequest {
            request: 1,
            caller: "caller".into(),
            config,
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        0,
        &module.runtime,
        false,
    );
    for _ in 0..3 {
        module.tick().unwrap();
    }
    assert_eq!(endpoint.completions[0].1["code"], "module_quiescing");
    assert_eq!(module.controller.records[&5].state, State::Playing);
    assert!(!module.controller.records[&5].cleanup_confirmed);
    assert_eq!(endpoint.stops(), 0);
    assert_eq!(module.controller.local, listener);
    assert!(module.controller.transport.is_some());

    // Only the later DAG quiesce phase may retire this provider's effects.
    endpoint.provider_progress = true;
    assert!(!module.quiesce().unwrap());
    for _ in 0..6 {
        module.tick().unwrap();
        if module.quiesce().unwrap() {
            break;
        }
    }
    assert!(module.quiesce().unwrap());
}

#[test]
fn shutdown_announcement_budget_is_not_reset_by_later_quiescence_or_reannouncement() {
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let mut controller = Controller::new(Settings::default()).unwrap();
    controller.records.insert(5, tests::record(State::Playing));
    controller.prepare_shutdown(100, 20_000);
    controller.prepare_shutdown(500, 50_000);
    assert_eq!(controller.shutdown_deadline, Some(20_100));
    assert_eq!(controller.records[&5].state, State::Playing);
    assert!(endpoint.requests.is_empty());

    assert!(!controller.quiesce(&runtime, 19_000));
    controller.tick(&runtime, 19_001).unwrap();
    assert_eq!(controller.job.as_ref().unwrap().deadline, 20_100);
    assert!(!controller.quiesce(&runtime, 20_000));
    controller.prepare_shutdown(20_000, 50_000);
    assert_eq!(controller.job.as_ref().unwrap().deadline, 20_100);
    controller.tick(&runtime, 20_100).unwrap();
    assert_eq!(controller.records[&5].state, State::CleanupFailed);
    assert!(!controller.quiesce(&runtime, 20_101));
    assert_eq!(
        endpoint.stops(),
        0,
        "an undispatched stop cannot start after the announcement deadline"
    );
}
