//! Actual launch module + runtime broker + SDK provider; catalog/session are fakes.
use super::*;
use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig};
use amaterasu_launch::LaunchModule;
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_OK,
};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use std::{cell::RefCell, ffi::c_void, rc::Rc};

const LAUNCH: &str = "amaterasu.launch";
const CATALOG: &str = "amaterasu.catalog/1";
const SESSION: &str = "amaterasu.session/1";
const LAUNCH_SERVICE: &str = "amaterasu.launch/1";

struct Shared {
    broker: Broker,
    events: Vec<(String, String, String)>,
    start_request: Option<u64>,
    lose_start_result: bool,
    cancelled_start: bool,
    released_start: bool,
    session_state: String,
    session_key: String,
    lease: Option<String>,
}
struct Endpoint {
    name: &'static str,
    shared: Rc<RefCell<Shared>>,
}

unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, sink: ReplySink) -> i32 {
    // SAFETY: test endpoints and input bytes stay live during synchronous calls.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let mut shared = endpoint.shared.borrow_mut();
    if endpoint.name == LAUNCH
        && let Command::TakeResult { request } = &command
        && shared.lose_start_result
        && shared.start_request == Some(*request)
    {
        shared.lose_start_result = false;
        return STATUS_INTERNAL_ERROR;
    }
    if endpoint.name == LAUNCH {
        match &command {
            Command::Cancel { request } if shared.start_request == Some(*request) => {
                shared.cancelled_start = true
            }
            Command::ReleaseResult { request } if shared.start_request == Some(*request) => {
                shared.released_start = true
            }
            _ => {}
        }
    }
    let is_start = endpoint.name == LAUNCH
        && matches!(&command, Command::Request { service, payload, .. } if service == SERVICE && payload["action"] == "launch");
    let response = shared.broker.dispatch(endpoint.name, command);
    if is_start && let Response::Accepted { id } = response {
        shared.start_request = Some(id);
    }
    if endpoint.name == "provider"
        && let Response::Request {
            caller,
            initiator,
            payload,
            ..
        } = &response
    {
        shared.events.push((
            payload["action"].as_str().unwrap().into(),
            caller.clone(),
            initiator.clone(),
        ));
    }
    let bytes = serde_json::to_vec(&response).unwrap();
    drop(shared);
    // SAFETY: reply sink belongs to the SDK call and remains live until return.
    unsafe { sink.send(&bytes) };
    STATUS_OK
}
fn endpoint(name: &'static str, shared: Rc<RefCell<Shared>>) -> (Box<Endpoint>, HostApi) {
    let mut endpoint = Box::new(Endpoint { name, shared });
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut *endpoint as *mut Endpoint).cast(),
        invoke,
    };
    (endpoint, api)
}
fn grants(calls: &[&str], provides: &[&str]) -> Grants {
    Grants {
        call: calls.iter().map(|v| (*v).into()).collect(),
        provide: provides.iter().map(|v| (*v).into()).collect(),
        ..Default::default()
    }
}
fn session_reply(shared: &Shared) -> Value {
    let mut leases = serde_json::Map::new();
    if let Some(lease) = &shared.lease {
        leases.insert(lease.clone(), json!("exclusive"));
    }
    json!({"ok":true,"revision":1,"sessions":[{"id":1,"key":shared.session_key,"state":shared.session_state,"leases":leases}]})
}
fn service_fake(shared: &mut Shared, principal: &str) {
    let Response::Request { id, payload, .. } = shared.broker.dispatch(principal, Command::Poll {})
    else {
        return;
    };
    let reply = if principal == "catalog" {
        json!({"ok":true,"revision":1,"id":payload["id"],"entry":{"name":"Fixture","launch":{"kind":"provider","provider":"example","application_id":"fixture-game"}},"provider_provenance":{"provider":"example.importer","source_id":"fixture-game"}})
    } else {
        match payload["action"].as_str().unwrap() {
            "lookup" => {}
            "create" => {
                shared.session_key = payload["key"].as_str().unwrap().into();
                shared.session_state = "preparing".into();
            }
            "acquire" => shared.lease = Some(payload["resource"].as_str().unwrap().into()),
            "activate" => shared.session_state = "active".into(),
            "stop" => shared.session_state = "stopping".into(),
            "finish_stop" | "cancel" | "fail" => {
                shared.session_state = "stopped".into();
                shared.lease = None;
            }
            other => panic!("unexpected session action {other}"),
        }
        session_reply(shared)
    };
    assert!(matches!(
        shared.broker.dispatch(
            principal,
            Command::Complete {
                request: id,
                payload: reply
            }
        ),
        Response::Ok
    ));
}
fn request(
    shared: &Rc<RefCell<Shared>>,
    launch: &mut LaunchModule,
    provider: &mut Module,
    caller: &str,
    op: &str,
    payload: Value,
) -> Value {
    let Response::Accepted { id } = shared.borrow_mut().broker.dispatch(
        caller,
        Command::Request {
            service: LAUNCH_SERVICE.into(),
            payload,
            timeout_ms: 30_000,
            operation_id: op.into(),
            parent: None,
        },
    ) else {
        panic!("client admission failed")
    };
    for _ in 0..10_000 {
        launch.tick().unwrap();
        provider.tick().unwrap();
        {
            let mut shared = shared.borrow_mut();
            service_fake(&mut shared, "catalog");
            service_fake(&mut shared, "session");
        }
        match shared
            .borrow_mut()
            .broker
            .dispatch(caller, Command::TakeResult { request: id })
        {
            Response::Pending => {}
            Response::Result { payload } => return payload,
            other => panic!("unexpected client result {other:?}"),
        }
    }
    panic!("launch workflow stalled")
}

#[test]
fn actual_launch_and_sdk_provider_reconcile_lost_start_through_broker() {
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "alice".into(),
        grants(&[LAUNCH_SERVICE, CATALOG, SESSION, SERVICE], &[]),
    );
    config.principals.insert(
        "mallory".into(),
        grants(&[LAUNCH_SERVICE, CATALOG, SESSION], &[]),
    );
    config.principals.insert(
        LAUNCH.into(),
        grants(&[CATALOG, SESSION, SERVICE], &[LAUNCH_SERVICE]),
    );
    config
        .principals
        .insert("provider".into(), grants(&[], &[SERVICE]));
    config
        .principals
        .insert("catalog".into(), grants(&[], &[CATALOG]));
    config
        .principals
        .insert("session".into(), grants(&[], &[SESSION]));
    config.settings.insert(
        LAUNCH.into(),
        json!({"provider_routes":{"example":{"importer":"example.importer","service":SERVICE}}}),
    );
    config.settings.insert(
        "provider".into(),
        json!({"callers":{"amaterasu.launch":{"initiators":["alice",LAUNCH],"lifecycle":true}}}),
    );
    let mut broker = Broker::new(config).unwrap();
    for (principal, service) in [("catalog", CATALOG), ("session", SESSION)] {
        assert!(matches!(
            broker.dispatch(
                principal,
                Command::Register {
                    service: service.into()
                }
            ),
            Response::Ok
        ));
    }
    let shared = Rc::new(RefCell::new(Shared {
        broker,
        events: vec![],
        start_request: None,
        lose_start_result: true,
        cancelled_start: false,
        released_start: false,
        session_state: String::new(),
        session_key: String::new(),
        lease: None,
    }));
    let (_launch_endpoint, launch_api) = endpoint(LAUNCH, shared.clone());
    let (_provider_endpoint, provider_api) = endpoint("provider", shared.clone());
    // SAFETY: endpoint allocations outlive both SDK clients and all synchronous calls.
    let mut launch =
        LaunchModule::start(unsafe { RuntimeClient::from_endpoint(launch_api) }).unwrap();
    let mut provider =
        Module::start(unsafe { RuntimeClient::from_endpoint(provider_api) }).unwrap();
    let result = request(
        &shared,
        &mut launch,
        &mut provider,
        "alice",
        "client-launch",
        json!({"action":"launch","operation_id":"test-op","app_id":"app-0000000000000001"}),
    );
    assert_eq!(result["launch"]["state"], "failed", "{result}");
    {
        let state = shared.borrow();
        assert_eq!(state.session_state, "stopped");
        assert!(state.lease.is_none());
        assert!(state.cancelled_start && state.released_start);
        assert!(
            state
                .events
                .contains(&("launch".into(), LAUNCH.into(), "alice".into())),
            "{:?}",
            state.events
        );
        assert!(
            state
                .events
                .contains(&("recover_stop".into(), LAUNCH.into(), LAUNCH.into())),
            "{:?}",
            state.events
        );
    }
    let started = request(
        &shared,
        &mut launch,
        &mut provider,
        "alice",
        "client-normal",
        json!({"action":"launch","operation_id":"normal-op","app_id":"app-0000000000000001"}),
    );
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let launch_id = started["launch"]["launch_id"].clone();
    for _ in 0..100 {
        launch.tick().unwrap();
        provider.tick().unwrap();
        {
            let mut state = shared.borrow_mut();
            service_fake(&mut state, "catalog");
            service_fake(&mut state, "session");
        }
        if shared
            .borrow()
            .events
            .iter()
            .any(|(action, caller, initiator)| {
                action == "lookup" && caller == LAUNCH && initiator == LAUNCH
            })
        {
            break;
        }
    }
    assert!(
        shared
            .borrow()
            .events
            .iter()
            .any(|(action, caller, initiator)| action == "lookup"
                && caller == LAUNCH
                && initiator == LAUNCH),
        "{:?}",
        shared.borrow().events
    );
    let stopped = request(
        &shared,
        &mut launch,
        &mut provider,
        "alice",
        "client-stop",
        json!({"action":"stop","launch_id":launch_id}),
    );
    assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
    assert!(shared.borrow().lease.is_none());
    let prior_provider_events = shared.borrow().events.len();
    let denied = request(
        &shared,
        &mut launch,
        &mut provider,
        "mallory",
        "client-denied",
        json!({"action":"launch","operation_id":"denied-op","app_id":"app-0000000000000001"}),
    );
    assert_eq!(denied["launch"]["state"], "failed", "{denied}");
    assert_eq!(shared.borrow().events.len(), prior_provider_events);
}
