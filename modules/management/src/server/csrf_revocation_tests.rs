//! Revocation uses real loopback connections and records the runtime ABI calls.
use super::*;
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT,
    STATUS_OK, Status,
};
use std::ffi::c_void;
use std::net::TcpStream;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

const TRUSTED: &str = "https://trusted.example:47990";

#[test]
fn operation_ids_are_unique_across_server_instances() {
    let mut first = OperationSequence::new().unwrap();
    let mut second = OperationSequence::new().unwrap();
    let first_id = first.next_id().unwrap();
    assert_ne!(first_id, first.next_id().unwrap());
    assert_ne!(first_id, second.next_id().unwrap());
}

#[derive(Default)]
struct RuntimeRecorder {
    commands: Mutex<Vec<Command>>,
    fail_release: AtomicBool,
    lost_release: AtomicBool,
    released_requests: Mutex<std::collections::BTreeSet<u64>>,
    fail_take: AtomicBool,
    fail_submit: AtomicBool,
    complete_take: AtomicBool,
    take_payload: Mutex<Option<Value>>,
    expired_take: AtomicBool,
    delay_lookup: AtomicBool,
    lookup_result: Mutex<Option<Response>>,
    submit_result: Mutex<Option<Response>>,
}

unsafe extern "C" fn record_call(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> Status {
    // SAFETY: the test owns the recorder for the whole client lifetime, and
    // RuntimeClient supplies input bytes and a reply sink valid for this call.
    let recorder = unsafe { &*context.cast::<RuntimeRecorder>() };
    let Ok(bytes) = (unsafe { input.read() }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
        return STATUS_INVALID_ARGUMENT;
    };
    if matches!(command, Command::TakeResult { .. })
        && recorder.fail_take.swap(false, Ordering::AcqRel)
    {
        recorder.commands.lock().unwrap().push(command);
        return STATUS_INTERNAL_ERROR;
    }
    if matches!(command, Command::Request { .. } | Command::CallApi { .. })
        && recorder.fail_submit.swap(false, Ordering::AcqRel)
    {
        recorder.commands.lock().unwrap().push(command);
        return STATUS_INTERNAL_ERROR;
    }
    if let Command::ReleaseResult { request } = command
        && recorder.lost_release.swap(false, Ordering::AcqRel)
    {
        recorder.released_requests.lock().unwrap().insert(request);
        recorder.commands.lock().unwrap().push(command);
        return STATUS_INTERNAL_ERROR;
    }
    let response = match command {
        Command::Request { .. } | Command::CallApi { .. } => recorder
            .submit_result
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Response::Ok),
        Command::TakeResult { .. } if recorder.expired_take.load(Ordering::Acquire) => {
            Response::error("deadline_exceeded", "request expired")
        }
        Command::TakeResult { .. } if recorder.complete_take.load(Ordering::Acquire) => {
            Response::Result {
                payload: recorder
                    .take_payload
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| json!({"ok": true})),
            }
        }
        Command::TakeResult { .. } => Response::Pending,
        Command::LookupRequest { .. } | Command::LookupApiRequest { .. } => {
            if recorder.delay_lookup.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(600));
            }
            recorder
                .lookup_result
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| Response::error("invalid_request", "lookup unsupported"))
        }
        Command::Cancel { .. } => Response::Ok,
        Command::ReleaseResult { request }
            if recorder
                .released_requests
                .lock()
                .unwrap()
                .contains(&request) =>
        {
            Response::error("not_found", "already released")
        }
        Command::ReleaseResult { .. } if recorder.fail_release.swap(false, Ordering::AcqRel) => {
            Response::error("unavailable", "injected release failure")
        }
        Command::ReleaseResult { .. } => Response::Ok,
        _ => return STATUS_INVALID_ARGUMENT,
    };
    let Ok(mut commands) = recorder.commands.lock() else {
        return STATUS_INTERNAL_ERROR;
    };
    commands.push(command);
    drop(commands);
    let Ok(bytes) = serde_json::to_vec(&response) else {
        return STATUS_INTERNAL_ERROR;
    };
    // SAFETY: reply synchronously copies these bytes while their allocation is live.
    unsafe { reply.send(&bytes) };
    STATUS_OK
}

fn runtime(recorder: &RuntimeRecorder) -> RuntimeClient {
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: std::ptr::from_ref(recorder).cast_mut().cast(),
        invoke: record_call,
    };
    // SAFETY: callers retain the stack-owned recorder until after the last
    // synchronous server tick and then drop the RuntimeClient first.
    unsafe { RuntimeClient::from_endpoint(endpoint) }
}

fn tls_server() -> Server {
    let configuration = crate::tls::configuration(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/browser-cert.pem"
        ),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/browser-key.pem"
        ),
    )
    .unwrap();
    let mut server = Server::bind(
        "127.0.0.1:0".parse().unwrap(),
        vec![TRUSTED.to_owned()],
        None,
        Some("127.0.0.1:0".parse().unwrap()),
        Some(configuration),
    )
    .unwrap();
    server.set_peer_policy(Some(PeerTier::Pc));
    server.set_csrf_policy(serde_json::from_value(json!([TRUSTED])).unwrap());
    server
}

fn protected_request(authority: &str) -> http::Request {
    let access = format!("ama_{}", "a".repeat(64));
    let bytes = format!(
        "POST /v1/management_settings/get HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nOrigin: {TRUSTED}\r\nCookie: __Secure-amaterasu-access={access}\r\n\r\n{{}}"
    );
    http::parse_policy_transport(
        bytes.as_bytes(),
        authority,
        &[TRUSTED.to_owned()],
        http::Transport::Tls,
        &serde_json::from_value(json!([TRUSTED])).unwrap(),
    )
    .unwrap()
    .unwrap()
}

#[test]
fn origin_revocation_releases_completed_verifying_and_dispatched_requests() {
    for dispatched in [false, true] {
        let recorder = RuntimeRecorder::default();
        let runtime = runtime(&recorder);
        let mut server = tls_server();
        let client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
        server.accept().unwrap();
        assert_eq!(server.connections.len(), 1);
        let connection = &mut server.connections[0];
        let request = protected_request(&client.peer_addr().unwrap().to_string());
        assert!(request.csrf_exception);
        connection.origin = request.origin.clone();
        connection.csrf_exception = request.origin.clone();
        connection.stage = if dispatched {
            Stage::Dispatching {
                request_id: 41,
                login: false,
                operational_health: false,
                browser_auth: None,
                capture: None,
            }
        } else {
            Stage::Verifying {
                request_id: 41,
                request: Box::new(request),
            }
        };

        server.tick(&runtime).unwrap();
        assert_eq!(server.connections.len(), 1);
        assert!(matches!(
            recorder.commands.lock().unwrap().as_slice(),
            [Command::TakeResult { request: 41 }]
        ));
        recorder.commands.lock().unwrap().clear();

        recorder.complete_take.store(true, Ordering::Release);
        server.set_csrf_policy(CsrfOrigins::default());
        server.tick(&runtime).unwrap();
        assert!(server.connections.is_empty());
        assert!(matches!(
            recorder.commands.lock().unwrap().as_slice(),
            [
                Command::TakeResult { request: 41 },
                Command::ReleaseResult { request: 41 }
            ]
        ));
    }
}

#[test]
fn narrowing_exception_list_does_not_cancel_explicit_bearer_authorization() {
    let recorder = RuntimeRecorder::default();
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    let authority = client.peer_addr().unwrap().to_string();
    let bytes = format!(
        "POST /v1/management_settings/get HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nOrigin: {TRUSTED}\r\nAuthorization: Bearer explicit-token\r\n\r\n{{}}"
    );
    let request = http::parse_policy_transport(
        bytes.as_bytes(),
        &authority,
        &[TRUSTED.to_owned()],
        http::Transport::Tls,
        &serde_json::from_value(json!([TRUSTED])).unwrap(),
    )
    .unwrap()
    .unwrap();
    assert!(!request.csrf_exception);
    server.connections[0].origin = request.origin.clone();
    server.connections[0].stage = Stage::Verifying {
        request_id: 42,
        request: Box::new(request),
    };
    server.set_csrf_policy(CsrfOrigins::default());
    server.tick(&runtime).unwrap();
    assert_eq!(server.connections.len(), 1);
    assert!(matches!(
        recorder.commands.lock().unwrap().as_slice(),
        [Command::TakeResult { request: 42 }]
    ));
}

#[test]
fn shutdown_retries_connection_request_release_before_retiring() {
    let recorder = RuntimeRecorder::default();
    recorder.fail_release.store(true, Ordering::Release);
    recorder.complete_take.store(true, Ordering::Release);
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    server.connections[0].stage = Stage::Dispatching {
        request_id: 43,
        login: false,
        operational_health: false,
        browser_auth: None,
        capture: None,
    };

    server.prepare_shutdown();
    assert!(!server.quiesce(&runtime));
    assert!(matches!(
        server.connections[0].stage,
        Stage::Retiring {
            request_id: 43,
            settled: false,
            ..
        }
    ));
    assert!(server.quiesce(&runtime));
    assert!(matches!(
        recorder.commands.lock().unwrap().as_slice(),
        [
            Command::TakeResult { request: 43 },
            Command::ReleaseResult { request: 43 },
            Command::ReleaseResult { request: 43 }
        ]
    ));
}

#[test]
fn broker_deadline_settles_the_caller_only_after_result_release() {
    let recorder = RuntimeRecorder::default();
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    server.connections[0].stage = Stage::Dispatching {
        request_id: 47,
        login: false,
        operational_health: false,
        browser_auth: None,
        capture: None,
    };
    assert!(!server.quiesce(&runtime));
    recorder.expired_take.store(true, Ordering::Release);
    assert!(server.quiesce(&runtime));
    assert!(server.connections.is_empty());
    assert!(matches!(
        recorder.commands.lock().unwrap().as_slice(),
        [
            Command::TakeResult { request: 47 },
            Command::TakeResult { request: 47 },
            Command::ReleaseResult { request: 47 }
        ]
    ));
    // Caller settlement is not provider retirement. The real broker's
    // reliable_retirement_survives_cancel_release_and_result_expiry regression
    // proves that the accepted provider delivery remains fenced independently.
}

#[test]
fn revocation_cancels_authority_but_keeps_unconfirmed_work_fenced() {
    let recorder = RuntimeRecorder::default();
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    server.connections[0].csrf_exception = Some(TRUSTED.into());
    server.connections[0].stage = Stage::Dispatching {
        request_id: 48,
        login: false,
        operational_health: false,
        browser_auth: None,
        capture: None,
    };
    server.set_csrf_policy(CsrfOrigins::default());
    server.tick(&runtime).unwrap();
    assert!(!server.quiesce(&runtime));
    assert!(matches!(
        recorder.commands.lock().unwrap().as_slice(),
        [
            Command::TakeResult { request: 48 },
            Command::Cancel { request: 48 },
            Command::TakeResult { request: 48 }
        ]
    ));
}

#[test]
fn result_transport_failure_retains_request_for_shutdown() {
    let recorder = RuntimeRecorder::default();
    recorder.fail_take.store(true, Ordering::Release);
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    server.connections[0].stage = Stage::Dispatching {
        request_id: 45,
        login: false,
        operational_health: false,
        browser_auth: None,
        capture: None,
    };
    assert!(server.tick(&runtime).is_ok());
    assert!(matches!(
        server.connections[0].stage,
        Stage::Dispatching { request_id: 45, .. }
    ));
    recorder.complete_take.store(true, Ordering::Release);
    assert!(server.quiesce(&runtime));
    assert!(matches!(
        recorder.commands.lock().unwrap().as_slice(),
        [
            Command::TakeResult { request: 45 },
            Command::TakeResult { request: 45 },
            Command::ReleaseResult { request: 45 }
        ]
    ));
}

#[test]
fn lost_release_ack_preserves_result_across_all_request_stages() {
    for kind in 0..5 {
        let recorder = RuntimeRecorder::default();
        recorder.complete_take.store(true, Ordering::Release);
        recorder.lost_release.store(true, Ordering::Release);
        let runtime = runtime(&recorder);
        let mut server = tls_server();
        let client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
        server.accept().unwrap();
        let request = protected_request(&client.peer_addr().unwrap().to_string());
        server.connections[0].stage = match kind {
            0 => Stage::Verifying {
                request_id: 51,
                request: Box::new(request),
            },
            1 => Stage::Dispatching {
                request_id: 51,
                login: false,
                operational_health: false,
                browser_auth: None,
                capture: None,
            },
            2 => Stage::SessionExtensionDispatching {
                request_id: 51,
                action: "status",
            },
            3 => Stage::ExtensionDispatching { request_id: 51 },
            _ => Stage::Artwork {
                request_id: 51,
                service: "test.artwork/1".into(),
            },
        };
        server.tick(&runtime).unwrap();
        assert!(server.connections[0].terminal_result.is_some());
        server.tick(&runtime).unwrap();
        assert!(server.connections[0].terminal_result.is_none());
        assert!(matches!(
            recorder.commands.lock().unwrap().as_slice(),
            [
                Command::TakeResult { request: 51 },
                Command::ReleaseResult { request: 51 },
                Command::ReleaseResult { request: 51 }
            ]
        ));
    }
}

#[test]
fn lost_release_ack_can_retire_during_shutdown() {
    let recorder = RuntimeRecorder::default();
    recorder.complete_take.store(true, Ordering::Release);
    recorder.lost_release.store(true, Ordering::Release);
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    server.connections[0].stage = Stage::Dispatching {
        request_id: 52,
        login: false,
        operational_health: false,
        browser_auth: None,
        capture: None,
    };
    server.tick(&runtime).unwrap();
    assert!(server.connections[0].terminal_result.is_some());
    assert!(server.quiesce(&runtime));
    assert!(matches!(
        recorder.commands.lock().unwrap().as_slice(),
        [
            Command::TakeResult { request: 52 },
            Command::ReleaseResult { request: 52 },
            Command::ReleaseResult { request: 52 }
        ]
    ));
}

#[test]
fn deadline_result_is_settled_without_claiming_provider_retirement() {
    let recorder = RuntimeRecorder::default();
    recorder.expired_take.store(true, Ordering::Release);
    recorder.fail_release.store(true, Ordering::Release);
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    server.connections[0].stage = Stage::Dispatching {
        request_id: 53,
        login: false,
        operational_health: false,
        browser_auth: None,
        capture: None,
    };
    server.tick(&runtime).unwrap();
    // A failed release keeps the terminal result and exact active request.
    // Dispatch can keep its current stage while retrying result settlement.
    assert_eq!(server.connections[0].active_request(), Some(53));
    assert!(matches!(server.connections[0].terminal_result,
        Some((53, Response::Error { ref code, .. })) if code == "deadline_exceeded"));
    assert!(server.quiesce(&runtime));
    assert!(server.connections.is_empty());
    assert!(matches!(
        recorder.commands.lock().unwrap().as_slice(),
        [
            Command::TakeResult { request: 53 },
            Command::ReleaseResult { request: 53 },
            Command::ReleaseResult { request: 53 }
        ]
    ));
    // The broker regression `reliable_retirement_survives_cancel_release_and_result_expiry`
    // separately verifies caller result release preserves the provider's unretired receipt.
}

#[test]
fn downstream_dispatch_error_does_not_resurrect_released_verification() {
    let recorder = RuntimeRecorder::default();
    recorder.complete_take.store(true, Ordering::Release);
    *recorder.take_payload.lock().unwrap() = Some(json!({
        "ok": true, "kind": "administrator_session", "role": "administrator",
        "subject": "admin", "auth_context_id": "a".repeat(64),
        "scopes": ["capture.write"], "expires_at": 1_800_000_000_u64
    }));
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    let mut request = protected_request(&client.peer_addr().unwrap().to_string());
    request.target =
        RequestTarget::Api(crate::routes::resolve("/v1/capture/pipewire_selection_begin").unwrap());
    request.payload = json!({}); // Missing operation_id makes downstream dispatch fail.
    server.connections[0].stage = Stage::Verifying {
        request_id: 54,
        request: Box::new(request),
    };
    assert!(server.tick(&runtime).is_err());
    assert!(matches!(server.connections[0].stage, Stage::Closed));
    assert!(server.quiesce(&runtime));
    assert!(matches!(
        recorder.commands.lock().unwrap().as_slice(),
        [
            Command::TakeResult { request: 54 },
            Command::ReleaseResult { request: 54 }
        ]
    ));
}

#[test]
fn lost_admission_is_fenced_without_replaying_an_unparented_effect() {
    let recorder = RuntimeRecorder::default();
    recorder.fail_submit.store(true, Ordering::Release);
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    let mut sequence = OperationSequence::new().unwrap();
    assert!(
        server.connections[0]
            .submit(
                &runtime,
                "test.effect/1",
                json!({"action":"begin"}),
                &mut sequence
            )
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        &server.connections[0].stage,
        Stage::AdmissionUncertain {
            command: Command::Request { service, operation_id, parent: None, .. }, ..
        } if service == "test.effect/1" && operation_id.starts_with("management.")
    ));
    server.tick(&runtime).unwrap();
    server.prepare_shutdown();
    assert!(!server.quiesce(&runtime));
    assert!(!server.quiesce(&runtime));
    let commands = recorder.commands.lock().unwrap();
    let Command::Request { operation_id, .. } = &commands[0] else {
        panic!("expected initial request")
    };
    assert!(commands[1..].iter().all(|command| matches!(command, Command::LookupRequest { operation_id: queried, .. } if queried == operation_id)));
}

#[test]
fn lost_typed_api_admission_is_retained_without_replay() {
    let recorder = RuntimeRecorder::default();
    recorder.fail_submit.store(true, Ordering::Release);
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    let command = Command::CallApi {
        service: "extension.example/1".into(),
        method: "begin".into(),
        interface_major: 1,
        interface_minor: 0,
        payload: json!({"action":"begin"}),
        timeout_ms: 1000,
        operation_id: "management.7".into(),
        parent: None,
    };
    assert!(
        server.connections[0]
            .submit_command(&runtime, command)
            .is_none()
    );
    assert!(matches!(
        &server.connections[0].stage,
        Stage::AdmissionUncertain { command: Command::CallApi { operation_id, .. }, .. }
            if operation_id == "management.7"
    ));
    server.tick(&runtime).unwrap();
    assert!(!server.quiesce(&runtime));
    assert!(matches!(
        recorder.commands.lock().unwrap().as_slice(),
        [
            Command::CallApi { operation_id, .. },
            Command::LookupApiRequest { operation_id: first_lookup, .. },
            Command::LookupApiRequest { operation_id: second_lookup, .. }
        ] if operation_id == first_lookup && operation_id == second_lookup
    ));
}

#[test]
fn conflicting_or_unexpected_submit_response_is_not_assumed_absent() {
    for response in [Response::Ok, Response::error("conflict", "prior operation")] {
        let recorder = RuntimeRecorder::default();
        *recorder.submit_result.lock().unwrap() = Some(response);
        let runtime = runtime(&recorder);
        let mut server = tls_server();
        let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
        server.accept().unwrap();
        let mut sequence = OperationSequence::new().unwrap();
        assert!(
            server.connections[0]
                .submit(&runtime, "test.effect/1", json!({}), &mut sequence)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            server.connections[0].stage,
            Stage::AdmissionUncertain { .. }
        ));
        assert!(!server.quiesce(&runtime));
        let commands = recorder.commands.lock().unwrap();
        assert!(matches!(
            commands.as_slice(),
            [Command::Request { .. }, Command::LookupRequest { .. }]
        ));
    }
}

#[test]
fn lookup_reconciles_lost_request_without_submitting_it_again() {
    for found in [false, true] {
        let recorder = RuntimeRecorder::default();
        recorder.fail_submit.store(true, Ordering::Release);
        recorder.complete_take.store(true, Ordering::Release);
        *recorder.lookup_result.lock().unwrap() = Some(if found {
            Response::Accepted { id: 46 }
        } else {
            Response::error("not_found", "request absent")
        });
        let runtime = runtime(&recorder);
        let mut server = tls_server();
        let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
        server.accept().unwrap();
        let mut sequence = OperationSequence::new().unwrap();
        assert!(
            server.connections[0]
                .submit(
                    &runtime,
                    "test.effect/1",
                    json!({"action":"begin"}),
                    &mut sequence
                )
                .unwrap()
                .is_none()
        );
        assert!(server.quiesce(&runtime));
        let commands = recorder.commands.lock().unwrap();
        let Command::Request { operation_id, .. } = &commands[0] else {
            panic!("expected initial request")
        };
        assert!(
            matches!(&commands[1], Command::LookupRequest { operation_id: queried, .. } if queried == operation_id)
        );
        if found {
            assert!(matches!(
                &commands[2..],
                [
                    Command::TakeResult { request: 46 },
                    Command::ReleaseResult { request: 46 }
                ]
            ));
        } else {
            assert_eq!(commands.len(), 2);
        }
    }
}

#[test]
fn typed_lookup_uses_exact_contract_and_waits_for_provider_result() {
    for found in [false, true] {
        let recorder = RuntimeRecorder::default();
        recorder.fail_submit.store(true, Ordering::Release);
        recorder.complete_take.store(true, Ordering::Release);
        *recorder.lookup_result.lock().unwrap() = Some(if found {
            Response::Accepted { id: 49 }
        } else {
            Response::error("not_found", "request absent")
        });
        let runtime = runtime(&recorder);
        let mut server = tls_server();
        let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
        server.accept().unwrap();
        assert!(
            server.connections[0]
                .submit_command(
                    &runtime,
                    Command::CallApi {
                        service: "extension.example/1".into(),
                        method: "begin".into(),
                        interface_major: 1,
                        interface_minor: 2,
                        payload: json!({"value":3}),
                        timeout_ms: 1000,
                        operation_id: "management.typed".into(),
                        parent: None,
                    }
                )
                .is_none()
        );
        assert!(server.quiesce(&runtime));
        let commands = recorder.commands.lock().unwrap();
        assert!(matches!(
            &commands[1],
            Command::LookupApiRequest { service, method, interface_major: 1, interface_minor: 2, payload, operation_id, parent: None }
                if service == "extension.example/1" && method == "begin" && payload == &json!({"value":3}) && operation_id == "management.typed"
        ));
        if found {
            assert!(matches!(
                &commands[2..],
                [
                    Command::TakeResult { request: 49 },
                    Command::ReleaseResult { request: 49 }
                ]
            ));
        } else {
            assert_eq!(commands.len(), 2);
        }
    }
}

#[test]
fn expired_lookup_window_remains_fenced() {
    let recorder = RuntimeRecorder::default();
    recorder.fail_submit.store(true, Ordering::Release);
    *recorder.lookup_result.lock().unwrap() = Some(Response::error("not_found", "request absent"));
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    let mut sequence = OperationSequence::new().unwrap();
    server.connections[0]
        .submit(&runtime, "test.effect/1", json!({}), &mut sequence)
        .unwrap();
    if let Stage::AdmissionUncertain { first_sent, .. } = &mut server.connections[0].stage {
        *first_sent = Instant::now() - Duration::from_secs(300);
    } else {
        panic!("expected an uncertain admission");
    }
    assert!(!server.quiesce(&runtime));
    assert_eq!(recorder.commands.lock().unwrap().len(), 1);
}

#[test]
fn lookup_absence_after_retention_window_is_not_retirement_proof() {
    let recorder = RuntimeRecorder::default();
    recorder.fail_submit.store(true, Ordering::Release);
    recorder.delay_lookup.store(true, Ordering::Release);
    *recorder.lookup_result.lock().unwrap() = Some(Response::error("not_found", "request absent"));
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let _client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    let mut sequence = OperationSequence::new().unwrap();
    server.connections[0]
        .submit(&runtime, "test.effect/1", json!({}), &mut sequence)
        .unwrap();
    if let Stage::AdmissionUncertain { first_sent, .. } = &mut server.connections[0].stage {
        *first_sent = Instant::now() - Duration::from_millis(299_500);
    } else {
        panic!("expected an uncertain admission");
    }
    assert!(!server.quiesce(&runtime));
    assert!(matches!(
        recorder.commands.lock().unwrap().as_slice(),
        [Command::Request { .. }, Command::LookupRequest { .. }]
    ));
}

#[test]
fn shutdown_announcement_does_not_accept_or_dispatch_new_requests() {
    let recorder = RuntimeRecorder::default();
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let _first = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    assert_eq!(server.connections.len(), 1);
    server.prepare_shutdown();
    let _second = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.tick(&runtime).unwrap();
    assert_eq!(server.connections.len(), 1);
    assert!(recorder.commands.lock().unwrap().is_empty());
    assert!(server.quiesce(&runtime));
}

#[cfg(target_os = "linux")]
#[test]
fn tls_write_failure_retires_the_inflight_request() {
    use std::os::fd::AsRawFd;

    let recorder = RuntimeRecorder::default();
    recorder.complete_take.store(true, Ordering::Release);
    let runtime = runtime(&recorder);
    let mut server = tls_server();
    let client = TcpStream::connect(server.tls_address().unwrap()).unwrap();
    server.accept().unwrap();
    let connection = &mut server.connections[0];
    connection.stage = Stage::Dispatching {
        request_id: 44,
        login: false,
        operational_health: false,
        browser_auth: None,
        capture: None,
    };
    connection.tls.as_mut().unwrap().send_close_notify();
    let reset = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    // SAFETY: client is a live socket, and reset has the expected linger layout.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                client.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_LINGER,
                std::ptr::from_ref(&reset).cast(),
                std::mem::size_of_val(&reset) as libc::socklen_t,
            )
        },
        0
    );
    drop(client);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !server.connections.is_empty() {
        server.tick(&runtime).unwrap();
        assert!(
            Instant::now() < deadline,
            "TLS reset did not retire connection"
        );
        std::thread::yield_now();
    }
    assert!(matches!(
        recorder.commands.lock().unwrap().as_slice(),
        [
            Command::TakeResult { request: 44 },
            Command::ReleaseResult { request: 44 }
        ]
    ));
}
