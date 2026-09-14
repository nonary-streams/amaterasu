use super::*;
use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};
use std::collections::VecDeque;
use std::ffi::c_void;

#[derive(Default)]
struct TestRuntime {
    requests: VecDeque<Response>,
    completions: Vec<serde_json::Value>,
    events: Vec<serde_json::Value>,
    completion_error: Option<&'static str>,
}

impl TestRuntime {
    fn dispatch(&mut self, command: Command) -> Response {
        match command {
            Command::Register { .. } => Response::Ok,
            Command::Poll {} => self.requests.pop_front().unwrap_or(Response::Pending),
            Command::Publish { payload, .. } => {
                self.events.push(payload);
                Response::error("denied", "publication deliberately denied")
            }
            Command::Complete { payload, .. } => {
                self.completions.push(payload);
                match self.completion_error {
                    Some(code) => Response::error(code, "deliberate completion failure"),
                    None => Response::Ok,
                }
            }
            _ => Response::error("unexpected", "test endpoint command not supported"),
        }
    }
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> abi::Status {
    // SAFETY: the test retains exclusive ownership of TestRuntime until the module
    // is dropped, and the SDK provides input valid for this synchronous call.
    let runtime = unsafe { &mut *context.cast::<TestRuntime>() };
    let Ok(bytes) = (unsafe { input.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(output) = serde_json::to_vec(&runtime.dispatch(command)) else {
        return abi::STATUS_INTERNAL_ERROR;
    };
    // SAFETY: the SDK reply sink is alive during this call and copies output bytes.
    unsafe { reply.send(&output) };
    abi::STATUS_OK
}

#[test]
fn rejected_publication_keeps_state_and_binds_ownership_to_caller() {
    let mut runtime = TestRuntime::default();
    for (index, caller, payload) in [
        (0, "alice", json!({"action":"create","key":"secret-key"})),
        (1, "bob", json!({"action":"snapshot","session_id":1})),
        (2, "alice", json!({"action":"snapshot","session_id":1})),
        (3, "alice", json!({"action":"create","key":"secret-key"})),
    ] {
        runtime.requests.push_back(Response::Request {
            id: index,
            caller: caller.into(),
            initiator: "different-initiator".into(),
            service: SERVICE.into(),
            payload,
            remaining_ms: 1000,
        });
    }
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut runtime as *mut TestRuntime).cast(),
        invoke,
    };
    // SAFETY: runtime remains alive and is accessed only through the module until drop.
    let client = unsafe { RuntimeClient::from_endpoint(endpoint) };
    let mut module = SessionModule::start(client).unwrap();
    for _ in 0..5 {
        module.tick().unwrap();
    }
    drop(module);
    assert_eq!(runtime.events, vec![json!({"revision":1})]);
    assert_eq!(runtime.completions[0]["ok"], true);
    assert_eq!(runtime.completions[1]["code"], "session_not_found");
    assert_eq!(runtime.completions[0], runtime.completions[2]);
    assert_eq!(runtime.completions[0], runtime.completions[3]);
}

#[test]
fn ended_requests_keep_committed_state_but_unexpected_completion_errors_surface() {
    for code in ["request_ended", "not_found", "permission_denied"] {
        let mut runtime = TestRuntime {
            completion_error: Some(code),
            ..TestRuntime::default()
        };
        runtime.requests.push_back(Response::Request {
            id: 1,
            caller: "alice".into(),
            initiator: "alice".into(),
            service: SERVICE.into(),
            payload: json!({"action":"create","key":"retry"}),
            remaining_ms: 1,
        });
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut runtime as *mut TestRuntime).cast(),
            invoke,
        };
        // SAFETY: runtime remains alive and exclusively accessed by the module until drop.
        let client = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = SessionModule::start(client).unwrap();
        assert_eq!(module.tick().is_ok(), code != "permission_denied");
        assert_eq!(module.coordinator.revision(), 1);
        assert_eq!(
            module
                .coordinator
                .handle("alice", json!({"action":"snapshot","session_id":1}))["ok"],
            true
        );
    }
}
