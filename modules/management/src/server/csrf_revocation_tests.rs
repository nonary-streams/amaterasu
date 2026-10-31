//! Revocation uses real loopback connections and records the runtime ABI calls.
use super::*;
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT,
    STATUS_OK, Status,
};
use std::ffi::c_void;
use std::net::TcpStream;
use std::sync::Mutex;

const TRUSTED: &str = "https://trusted.example:47990";

#[derive(Default)]
struct RuntimeRecorder {
    commands: Mutex<Vec<Command>>,
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
    let response = match command {
        Command::TakeResult { .. } => Response::Pending,
        Command::Cancel { .. } | Command::ReleaseResult { .. } => Response::Ok,
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
fn origin_revocation_cancels_and_releases_verifying_and_dispatched_requests() {
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

        server.set_csrf_policy(CsrfOrigins::default());
        server.tick(&runtime).unwrap();
        assert!(server.connections.is_empty());
        assert!(matches!(
            recorder.commands.lock().unwrap().as_slice(),
            [
                Command::Cancel { request: 41 },
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
