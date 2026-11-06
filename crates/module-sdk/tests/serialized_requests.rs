use amaterasu_module_sdk::{
    ApiFieldKind, ApiMethod, ApiPrivilege, ApiSchema, RuntimeClient,
    abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK},
    extension_api::{ActionTarget, EXTENSION_ACTIONS_SERVICE},
    requests::RequestOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{cell::RefCell, collections::VecDeque, ffi::c_void};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Payload {
    operation: String,
    target: ActionTarget,
}

struct Fixture {
    commands: RefCell<Vec<Value>>,
    replies: RefCell<VecDeque<Value>>,
}

#[test]
fn provider_can_declare_an_approved_method_through_the_sdk() {
    let method = ApiMethod {
        service: "example.provider/1".into(),
        method: "action_execute".into(),
        interface_major: 1,
        interface_minor: 0,
        request: ApiSchema {
            required: [("operation".into(), ApiFieldKind::String)].into(),
            optional: Default::default(),
            allow_extra: false,
        },
        response: ApiSchema {
            required: [("status".into(), ApiFieldKind::String)].into(),
            optional: Default::default(),
            allow_extra: false,
        },
        privilege: ApiPrivilege::User,
    };
    assert_eq!(method.interface_major, 1);
}

unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, reply: ReplySink) -> i32 {
    // SAFETY: the fixture and input remain alive throughout this synchronous call.
    let fixture = unsafe { &*context.cast::<Fixture>() };
    let command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    fixture.commands.borrow_mut().push(command);
    let bytes = serde_json::to_vec(&fixture.replies.borrow_mut().pop_front().unwrap()).unwrap();
    // SAFETY: the reply callback is valid until this invoke call returns.
    unsafe { reply.send(&bytes) };
    STATUS_OK
}

#[test]
fn serialized_requests_preserve_identity_pending_and_retry_state() {
    let fixture = Fixture {
        commands: RefCell::new(Vec::new()),
        replies: RefCell::new(VecDeque::from([
            json!({"kind":"accepted","id":7}),
            json!({"kind":"pending"}),
            json!({"kind":"result","payload":{"ok":true}}),
            json!({"kind":"request","id":9,"caller":"adapter","initiator":"owner",
                "service":"provider/1","payload":{"unexpected":true},"remaining_ms":400}),
            json!({"kind":"ok"}),
        ])),
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&fixture as *const Fixture).cast_mut().cast(),
        invoke,
    };
    // SAFETY: fixture lives longer than the client and all synchronous calls.
    let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
    let payload = Payload {
        operation: "invoke".into(),
        target: ActionTarget::Session {
            host_epoch: "epoch".into(),
            session_id: "session".into(),
            expected_generation: 2,
        },
    };
    let request = runtime
        .request_serialized(
            RequestOptions {
                service: EXTENSION_ACTIONS_SERVICE,
                timeout_ms: 500,
                operation_id: "operation-1",
                parent: Some(3),
            },
            &payload,
        )
        .unwrap();
    assert_eq!(request, 7);
    assert_eq!(runtime.take_serialized::<Value>(request).unwrap(), None);
    assert_eq!(
        runtime.take_serialized::<Value>(request).unwrap(),
        Some(json!({"ok":true}))
    );
    let incoming = runtime.poll_serialized().unwrap().unwrap();
    assert_eq!(
        (&*incoming.caller, &*incoming.initiator),
        ("adapter", "owner")
    );
    assert!(incoming.decode::<Payload>().is_err());
    runtime
        .complete_serialized(incoming.id, &json!({"error":"invalid_payload"}))
        .unwrap();
    let commands = fixture.commands.borrow();
    assert_eq!(commands[0]["operation_id"], "operation-1");
    assert_eq!(commands[0]["parent"], 3);
    assert_eq!(
        commands[0]["payload"],
        serde_json::to_value(payload).unwrap()
    );
    assert_eq!(commands[4]["request"], 9);
    assert!(
        !commands
            .iter()
            .any(|command| command["op"] == "release_result")
    );
}

#[test]
fn selected_operation_registration_preserves_old_policies_and_surfaces_failures() {
    use amaterasu_module_sdk::operation_api;
    let methods = [
        operation_api::method("example.selected/1", "read", &[], &[]),
        operation_api::method("example.selected/1", "write", &[], &[]),
    ];
    let fixture = Fixture {
        commands: RefCell::new(Vec::new()),
        replies: RefCell::new(VecDeque::from([
            json!({"kind":"error","code":"permission_denied","message":"not selected"}),
            json!({"kind":"ok"}),
            json!({"kind":"error","code":"invalid_request","message":"unavailable"}),
        ])),
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&fixture as *const Fixture).cast_mut().cast(),
        invoke,
    };
    // SAFETY: fixture outlives the client and these synchronous calls.
    let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
    assert_eq!(
        operation_api::register_selected(&runtime, &methods).unwrap(),
        1
    );
    assert!(operation_api::register_selected(&runtime, &methods[..1]).is_err());
    let commands = fixture.commands.borrow();
    assert_eq!(commands.len(), 3);
    assert!(
        commands
            .iter()
            .all(|command| command["op"] == "register_api")
    );
}
