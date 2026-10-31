use amaterasu_contracts::{
    ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Command, Grants,
    RuntimeConfig,
};
use amaterasu_module_sdk::{
    RuntimeClient,
    abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK},
    service::{self, CallOptions, ServiceError, ServiceMethod},
};
use amaterasu_runtime::Broker;
use serde::{Deserialize, Serialize};
use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GreetRequest {
    name: String,
}
#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GreetResponse {
    message: String,
}
struct Greet;
impl ServiceMethod for Greet {
    type Request = GreetRequest;
    type Response = GreetResponse;
    fn method() -> ApiMethod {
        ApiMethod {
            service: "example.greetings/1".into(),
            method: "greet".into(),
            interface_major: 1,
            interface_minor: 0,
            request: ApiSchema {
                required: [("name".into(), ApiFieldKind::String)].into(),
                optional: Default::default(),
                allow_extra: false,
            },
            response: ApiSchema {
                required: [("message".into(), ApiFieldKind::String)].into(),
                optional: Default::default(),
                allow_extra: false,
            },
            privilege: ApiPrivilege::User,
        }
    }
}
struct Endpoint {
    broker: Arc<Mutex<Broker>>,
    principal: &'static str,
}
unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, reply: ReplySink) -> i32 {
    // SAFETY: endpoints and input outlive the synchronous call; the mutex serializes broker access.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    let bytes = unsafe { input.read().unwrap() };
    let command: Command = serde_json::from_slice(bytes).unwrap();
    let response = endpoint
        .broker
        .lock()
        .unwrap()
        .dispatch(endpoint.principal, command);
    let bytes = serde_json::to_vec(&response).unwrap();
    // SAFETY: reply is valid until the call returns and copies these bytes.
    unsafe { reply.send(&bytes) };
    STATUS_OK
}
fn client(endpoint: &Endpoint) -> RuntimeClient {
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *const Endpoint).cast_mut().cast(),
        invoke,
    };
    // SAFETY: each endpoint and its shared broker remain alive for every client call.
    unsafe { RuntimeClient::from_endpoint(api) }
}

fn hello(request: &GreetRequest) -> GreetResponse {
    GreetResponse {
        message: format!("Hello, {}!", request.name),
    }
}

fn welcome(request: &GreetRequest) -> GreetResponse {
    GreetResponse {
        message: ["Welcome, ", &request.name, "!"].concat(),
    }
}

fn run_provider(
    provider_name: &'static str,
    handler: fn(&GreetRequest) -> GreetResponse,
    expected: &str,
) {
    let method = Greet::method();
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        provider_name.into(),
        Grants {
            provide: [method.service.clone()].into(),
            ..Default::default()
        },
    );
    for principal in ["example.client", "example.outsider"] {
        config.principals.insert(
            principal.into(),
            Grants {
                call: [method.service.clone()].into(),
                ..Default::default()
            },
        );
    }
    config.api_methods.push(ApiMethodApproval {
        provider: provider_name.into(),
        callers: ["example.client".into()].into(),
        method,
    });
    let broker = Arc::new(Mutex::new(Broker::new(config).unwrap()));
    let provider_endpoint = Endpoint {
        broker: broker.clone(),
        principal: provider_name,
    };
    let client_endpoint = Endpoint {
        broker: broker.clone(),
        principal: "example.client",
    };
    let outsider_endpoint = Endpoint {
        broker,
        principal: "example.outsider",
    };
    let provider = client(&provider_endpoint);
    let consumer = client(&client_endpoint);
    let outsider = client(&outsider_endpoint);
    service::register::<Greet>(&provider).unwrap();
    let options = || CallOptions {
        timeout_ms: 5000,
        operation_id: "greeting.1",
        parent: None,
    };
    let input = GreetRequest { name: "Ada".into() };
    assert!(
        matches!(service::call::<Greet>(&outsider, options(), &input), Err(ServiceError::Broker { code, .. }) if code == "permission_denied")
    );
    let call = service::call::<Greet>(&consumer, options(), &input).unwrap();
    assert!(call.poll().unwrap().is_none());
    let incoming = provider.poll_serialized().unwrap().unwrap();
    let mut wrong = incoming.payload.clone();
    wrong["interface_minor"] = 1.into();
    let mismatched = amaterasu_module_sdk::requests::IncomingRequest {
        id: incoming.id,
        caller: incoming.caller.clone(),
        initiator: incoming.initiator.clone(),
        service: incoming.service.clone(),
        payload: wrong,
        remaining_ms: incoming.remaining_ms,
    };
    assert!(matches!(
        service::decode::<Greet>(&mismatched),
        Err(ServiceError::WrongMethod)
    ));
    let typed = service::decode::<Greet>(&incoming).unwrap();
    assert_eq!(
        (&*typed.request.caller, &*typed.request.initiator),
        ("example.client", "example.client")
    );
    assert_eq!(typed.request.payload, input);
    service::complete::<Greet>(&provider, &typed, &handler(&typed.request.payload)).unwrap();
    assert_eq!(call.poll().unwrap().unwrap().message, expected);
    assert!(call.release().is_ok());
    let cancelled = service::call::<Greet>(
        &consumer,
        CallOptions {
            timeout_ms: 5000,
            operation_id: "greeting.cancel",
            parent: None,
        },
        &input,
    )
    .unwrap();
    cancelled.cancel().unwrap();
    assert!(
        matches!(cancelled.poll(), Err(ServiceError::Broker { code, .. }) if code == "cancelled")
    );
    assert!(cancelled.release().is_ok());
}

#[test]
fn unchanged_consumer_uses_two_independent_approved_providers() {
    run_provider("example.first", hello, "Hello, Ada!");
    run_provider("example.second", welcome, "Welcome, Ada!");
}
