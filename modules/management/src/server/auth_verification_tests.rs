//! Exercise cookie parsing, identity verification and route authorization together.
use super::*;
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT,
    STATUS_OK, Status,
};
use std::ffi::c_void;
use std::sync::Mutex;

struct VerificationRuntime {
    result: Response,
    commands: Mutex<Vec<Command>>,
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> Status {
    // SAFETY: each test retains this runtime and the input/reply allocations
    // throughout every synchronous RuntimeClient call.
    let runtime = unsafe { &*context.cast::<VerificationRuntime>() };
    let Ok(bytes) = (unsafe { input.read() }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
        return STATUS_INVALID_ARGUMENT;
    };
    let response = match &command {
        Command::Request { service, .. } if service == AUTH_SERVICE => {
            Response::Accepted { id: 41 }
        }
        Command::Request { .. } => Response::Accepted { id: 42 },
        Command::TakeResult { request: 41 } => runtime.result.clone(),
        Command::ReleaseResult { request: 41 } => Response::Ok,
        _ => return STATUS_INVALID_ARGUMENT,
    };
    let Ok(mut commands) = runtime.commands.lock() else {
        return STATUS_INTERNAL_ERROR;
    };
    commands.push(command);
    drop(commands);
    let Ok(bytes) = serde_json::to_vec(&response) else {
        return STATUS_INTERNAL_ERROR;
    };
    // SAFETY: the sink copies the reply before its allocation is dropped.
    unsafe { reply.send(&bytes) };
    STATUS_OK
}

fn verify_cookie(result: Response, cookie: char) -> (Stage, Vec<Command>) {
    let terminal = matches!(&result, Response::Result { .. });
    let recorder = VerificationRuntime {
        result,
        commands: Mutex::new(Vec::new()),
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: std::ptr::from_ref(&recorder).cast_mut().cast(),
        invoke,
    };
    // SAFETY: recorder outlives all synchronous client calls below.
    let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let authority = listener.local_addr().unwrap().to_string();
    let access = format!("ama_{}", cookie.to_string().repeat(64));
    let bytes = format!(
        "POST /v1/management_settings/get HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nOrigin: https://{authority}\r\nCookie: __Secure-amaterasu-access={access}\r\nX-Amaterasu-CSRF: 1\r\n\r\n{{}}"
    );
    let request = http::parse_policy_transport(
        bytes.as_bytes(),
        &authority,
        &[],
        http::Transport::Tls,
        &CsrfOrigins::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(request.bearer.as_deref(), Some(access.as_str()));
    let mut connection = Connection {
        stream,
        tls: None,
        tls_closing: false,
        deadline: Instant::now() + CONNECTION_TIMEOUT,
        origin: None,
        csrf_exception: None,
        stage: Stage::Closed,
        peer: "127.0.0.1".parse().unwrap(),
    };
    let mut operation = 1;
    let mut selection_owners = SelectionOwners::default();
    connection
        .authorize(
            &runtime,
            request,
            &mut operation,
            &mut None,
            &mut selection_owners,
        )
        .unwrap();
    let Stage::Verifying {
        request_id,
        request,
    } = std::mem::replace(&mut connection.stage, Stage::Closed)
    else {
        panic!("protected cookie request must verify identity")
    };
    assert_eq!(request_id, 41);
    connection
        .verify_result(
            &runtime,
            request_id,
            *request,
            &mut operation,
            &mut selection_owners,
            None,
        )
        .unwrap();
    let commands = recorder.commands.into_inner().unwrap();
    let Command::Request {
        service, payload, ..
    } = &commands[0]
    else {
        panic!("expected identity verification")
    };
    assert_eq!(service, AUTH_SERVICE);
    assert_eq!(
        payload,
        &json!({"action":"verify","bearer":access,"required_scopes":[]})
    );
    assert!(matches!(commands[1], Command::TakeResult { request: 41 }));
    if terminal {
        assert!(matches!(
            commands[2],
            Command::ReleaseResult { request: 41 }
        ));
    }
    (connection.stage, commands)
}

fn assert_denied(result: Response, cookie: char, status: u16, code: &str) {
    let expected_commands = if matches!(&result, Response::Result { .. }) {
        3
    } else {
        2
    };
    let (stage, commands) = verify_cookie(result, cookie);
    assert_eq!(
        commands.len(),
        expected_commands,
        "denied identity or scope must not dispatch"
    );
    let Stage::Writing { bytes, .. } = stage else {
        panic!("expected HTTP denial")
    };
    let reply = String::from_utf8(bytes).unwrap();
    assert!(reply.starts_with(&format!("HTTP/1.1 {status} ")));
    let (_, body) = reply.split_once("\r\n\r\n").unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(body).unwrap(),
        json!({"ok":false,"code":code})
    );
    assert!(
        !reply.contains("Set-Cookie:"),
        "a late denial must not erase a newer cookie pair"
    );
}

#[test]
fn expired_and_revoked_present_cookies_require_browser_identity_recovery() {
    // Authentication intentionally reports the same explicit denial for an
    // expired access session and a revoked session absent from its token table.
    for cookie in ['a', 'b'] {
        assert_denied(
            Response::Result {
                payload: json!({"ok":false,"code":"unauthorized"}),
            },
            cookie,
            401,
            "unauthorized",
        );
    }
}

#[test]
fn valid_cookie_with_insufficient_scope_is_forbidden_without_dispatch() {
    assert_denied(
        Response::Result {
            payload: json!({"ok":true,"kind":"administrator_session","subject":"admin","scopes":["catalog.read"],"expires_at":1_800_000_000}),
        },
        'c',
        403,
        "unauthorized",
    );
}

#[test]
fn valid_cookie_with_exact_route_scope_dispatches_only_the_resolved_action() {
    let (stage, commands) = verify_cookie(
        Response::Result {
            payload: json!({"ok":true,"kind":"administrator_session","role":"administrator","subject":"admin","scopes":["configuration.read"],"expires_at":1_800_000_000}),
        },
        'd',
    );
    assert!(matches!(stage, Stage::Dispatching { request_id: 42, .. }));
    assert_eq!(commands.len(), 4);
    let Command::Request {
        service, payload, ..
    } = &commands[3]
    else {
        panic!("expected product dispatch")
    };
    assert_eq!(service, crate::SERVICE);
    assert_eq!(payload, &json!({"action":"product_get"}));
}

#[test]
fn pipewire_selection_route_verifies_capture_scope_and_binds_the_handle_to_session() {
    const OPERATION_ID: &str = "a12f6f1e-04cb-4a97-bdf8-f53bb9cf2dd1";
    let auth_context_id = "a".repeat(64);
    let recorder = VerificationRuntime {
        result: Response::Result {
            payload: json!({
                "ok":true,
                "kind":"administrator_session",
                "role":"administrator",
                "subject":"admin",
                "auth_context_id":auth_context_id,
                "scopes":["capture.write"],
                "expires_at":1_800_000_000
            }),
        },
        commands: Mutex::new(Vec::new()),
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: std::ptr::from_ref(&recorder).cast_mut().cast(),
        invoke,
    };
    // SAFETY: recorder outlives all synchronous client calls below.
    let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let authority = listener.local_addr().unwrap().to_string();
    let access = format!("ama_{}", "d".repeat(64));
    let request_bytes = format!(
        "POST /v1/capture/pipewire_selection_begin HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: 79\r\nOrigin: https://{authority}\r\nCookie: __Secure-amaterasu-access={access}\r\nX-Amaterasu-CSRF: 1\r\n\r\n{{\"source_type\":\"monitor\",\"operation_id\":\"{OPERATION_ID}\"}}"
    );
    let request = http::parse_policy_transport(
        request_bytes.as_bytes(),
        &authority,
        &[],
        http::Transport::Tls,
        &CsrfOrigins::default(),
    )
    .unwrap()
    .unwrap();
    let mut connection = Connection {
        stream,
        tls: None,
        tls_closing: false,
        deadline: Instant::now() + CONNECTION_TIMEOUT,
        origin: None,
        csrf_exception: None,
        stage: Stage::Closed,
        peer: "127.0.0.1".parse().unwrap(),
    };
    let mut operation = 1;
    let mut selection_owners = SelectionOwners::default();
    connection
        .authorize(
            &runtime,
            request,
            &mut operation,
            &mut None,
            &mut selection_owners,
        )
        .unwrap();
    let Stage::Verifying {
        request_id,
        request,
    } = std::mem::replace(&mut connection.stage, Stage::Closed)
    else {
        panic!("capture route must verify the session before provider dispatch")
    };
    connection
        .verify_result(
            &runtime,
            request_id,
            *request,
            &mut operation,
            &mut selection_owners,
            None,
        )
        .unwrap();
    assert!(matches!(
        connection.stage,
        Stage::Dispatching {
            request_id: 42,
            capture: Some(CaptureDispatch {
                action: SelectionAction::Begin,
                ..
            }),
            ..
        }
    ));
    let identity = format!("administrator_session:admin:{auth_context_id}");
    assert!(selection_owners.permits(OPERATION_ID, &identity, Instant::now()));
    let commands = recorder.commands.into_inner().unwrap();
    let Command::Request {
        service: auth_service,
        payload: auth_payload,
        ..
    } = &commands[0]
    else {
        panic!("capture route must verify auth before dispatch")
    };
    assert_eq!(auth_service, AUTH_SERVICE);
    assert_eq!(auth_payload["action"], "verify");
    let Command::Request {
        service: capture_service,
        payload: capture_payload,
        parent,
        ..
    } = &commands[3]
    else {
        panic!("expected PipeWire selection call")
    };
    assert_eq!(capture_service, "amaterasu.capture-pipewire/1");
    assert_eq!(
        capture_payload,
        &json!({"action":"select_source","source_type":"monitor","operation_id":OPERATION_ID})
    );
    assert_eq!(*parent, None);
}

#[test]
fn rate_limits_and_unknown_or_malformed_verification_remain_closed() {
    assert_denied(
        Response::Result {
            payload: json!({"ok":false,"code":"rate_limited"}),
        },
        'e',
        429,
        "rate_limited",
    );
    for payload in [
        json!({"ok":false,"code":"service_unavailable"}),
        json!({"code":"unauthorized"}),
        json!({"ok":"false","code":"unauthorized"}),
        json!({"ok":true,"scopes":["*"]}),
        json!({"ok":true,"scopes":[null]}),
        json!({"ok":true,"scopes":["configuration.read",null]}),
        json!({"ok":true}),
        Value::Null,
    ] {
        assert_denied(Response::Result { payload }, 'f', 403, "unauthorized");
    }
    assert_denied(Response::Ok, 'a', 503, "authentication_unavailable");
}
